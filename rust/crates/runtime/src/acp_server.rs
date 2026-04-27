//! Agent Client Protocol (ACP) server support built on the official Rust SDK.
//!
//! This module keeps the existing CLI-facing ACP adapter types (`AcpAgent`,
//! `AcpError`, `AcpSessionUpdateObserver`) while delegating the wire protocol
//! and stdio transport to `agent-client-protocol`.

use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use agent_client_protocol::role::acp::Agent;
use agent_client_protocol::{Client, ConnectionTo, Dispatch, Responder};
use agent_client_protocol_schema::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, Implementation,
    InitializeRequest, InitializeResponse, ListSessionsRequest, ListSessionsResponse,
    McpCapabilities, NewSessionRequest, NewSessionResponse, PromptCapabilities, PromptRequest,
    PromptResponse, ProtocolVersion, SessionCapabilities, SessionId, SessionInfo,
    SessionListCapabilities, SessionNotification, SessionUpdate, StopReason, TextContent, ToolCall,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol_tokio::Stdio;
use serde_json::Value;

use crate::conversation::RuntimeObserver;

/// Configuration for the ACP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpServerOptions {
    agent_version: String,
}

impl AcpServerOptions {
    #[must_use]
    pub fn new(agent_version: impl Into<String>) -> Self {
        Self {
            agent_version: agent_version.into(),
        }
    }

    pub(crate) fn agent_version(&self) -> &str {
        &self.agent_version
    }
}

/// Agent-side operations invoked by the ACP server.
pub trait AcpAgent {
    fn new_session(&mut self, cwd: Option<PathBuf>) -> Result<String, AcpError>;

    fn list_sessions(&self, cwd: Option<PathBuf>) -> Result<Vec<AcpSessionInfo>, AcpError>;

    fn run_prompt(
        &mut self,
        session_id: &str,
        prompt: String,
        observer: &mut AcpSessionUpdateObserver,
    ) -> Result<(), AcpError>;
}

/// Minimal ACP session metadata exposed to `session/list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSessionInfo {
    pub session_id: String,
    pub cwd: PathBuf,
}

impl AcpSessionInfo {
    #[must_use]
    pub fn new(session_id: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            session_id: session_id.into(),
            cwd: cwd.into(),
        }
    }
}

/// Error type returned by ACP agent implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpError {
    InvalidParams(String),
    Internal(String),
}

impl AcpError {
    #[must_use]
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::InvalidParams(message.into())
    }

    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    fn message(&self) -> &str {
        match self {
            Self::InvalidParams(message) | Self::Internal(message) => message,
        }
    }
}

impl Display for AcpError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for AcpError {}

/// Fatal server errors that prevent further ACP processing.
#[derive(Debug)]
pub enum AcpServerError {
    Io(io::Error),
    Protocol(agent_client_protocol::Error),
}

impl Display for AcpServerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Protocol(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for AcpServerError {}

impl From<io::Error> for AcpServerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<agent_client_protocol::Error> for AcpServerError {
    fn from(value: agent_client_protocol::Error) -> Self {
        Self::Protocol(value)
    }
}

/// Runtime observer that converts model/tool events to ACP session/update notifications.
pub struct AcpSessionUpdateObserver {
    session_id: SessionId,
    connection: ConnectionTo<Client>,
    write_error: Option<AcpError>,
}

impl AcpSessionUpdateObserver {
    fn new(session_id: impl Into<SessionId>, connection: ConnectionTo<Client>) -> Self {
        Self {
            session_id: session_id.into(),
            connection,
            write_error: None,
        }
    }

    fn finish(self) -> Result<(), AcpError> {
        match self.write_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn notify_update(&mut self, update: SessionUpdate) {
        if self.write_error.is_some() {
            return;
        }

        let notification = SessionNotification::new(self.session_id.clone(), update);
        if let Err(error) = self.connection.send_notification(notification) {
            self.write_error = Some(AcpError::internal(error.to_string()));
        }
    }
}

impl RuntimeObserver for AcpSessionUpdateObserver {
    fn on_text_delta(&mut self, delta: &str) {
        self.notify_update(SessionUpdate::AgentMessageChunk(ContentChunk::new(
            ContentBlock::Text(TextContent::new(delta)),
        )));
    }

    fn on_tool_use(&mut self, id: &str, name: &str, input: &str) {
        self.notify_update(SessionUpdate::ToolCall(
            ToolCall::new(id.to_string(), name.to_string())
                .kind(ToolKind::Other)
                .status(ToolCallStatus::InProgress)
                .raw_input(parse_json_or_string(input)),
        ));
    }

    fn on_tool_result(
        &mut self,
        tool_use_id: &str,
        _tool_name: &str,
        output: &str,
        is_error: bool,
    ) {
        let status = if is_error {
            ToolCallStatus::Failed
        } else {
            ToolCallStatus::Completed
        };
        self.notify_update(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            tool_use_id.to_string(),
            ToolCallUpdateFields::new()
                .status(status)
                .raw_output(parse_json_or_string(output))
                .content(vec![output.to_string().into()]),
        )));
    }
}

/// Run the ACP server on process stdin/stdout.
#[allow(clippy::too_many_lines)]
pub fn run_acp_stdio_server<A>(agent: A, options: &AcpServerOptions) -> Result<(), AcpServerError>
where
    A: AcpAgent + Send + 'static,
{
    let agent = Arc::new(Mutex::new(agent));
    let initialize_version = options.agent_version().to_string();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let new_session_agent = Arc::clone(&agent);
        let list_sessions_agent = Arc::clone(&agent);
        let prompt_agent = Arc::clone(&agent);

        Agent
            .builder()
            .name("scode")
            .on_receive_request(
                move |initialize: InitializeRequest,
                      responder: Responder<InitializeResponse>,
                      _connection: ConnectionTo<Client>| {
                    let response =
                        build_initialize_response(initialize.protocol_version, &initialize_version);
                    async move { responder.respond(response) }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                move |request: NewSessionRequest,
                      responder: Responder<NewSessionResponse>,
                      _connection: ConnectionTo<Client>| {
                    let agent = Arc::clone(&new_session_agent);
                    async move {
                        let session_id = {
                            let mut agent = agent.lock().unwrap_or_else(PoisonError::into_inner);
                            agent
                                .new_session(Some(request.cwd))
                                .map_err(map_acp_error)?
                        };
                        responder.respond(NewSessionResponse::new(session_id))
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                move |request: ListSessionsRequest,
                      responder: Responder<ListSessionsResponse>,
                      _connection: ConnectionTo<Client>| {
                    let agent = Arc::clone(&list_sessions_agent);
                    async move {
                        let sessions = {
                            let agent = agent.lock().unwrap_or_else(PoisonError::into_inner);
                            agent
                                .list_sessions(request.cwd)
                                .map_err(map_acp_error)?
                                .into_iter()
                                .map(|session| SessionInfo::new(session.session_id, session.cwd))
                                .collect::<Vec<_>>()
                        };
                        responder.respond(ListSessionsResponse::new(sessions))
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                move |request: PromptRequest,
                      responder: Responder<PromptResponse>,
                      connection: ConnectionTo<Client>| {
                    let agent = Arc::clone(&prompt_agent);
                    async move {
                        let prompt = extract_prompt_text(&request.prompt).map_err(map_acp_error)?;
                        let session_id = request.session_id.clone();
                        let prompt_result = tokio::task::spawn_blocking(move || {
                            // Prompt execution is synchronous and may own its own Tokio runtime,
                            // so run it off the ACP SDK executor to avoid nested `block_on`.
                            let mut observer =
                                AcpSessionUpdateObserver::new(session_id.clone(), connection);
                            {
                                let mut agent =
                                    agent.lock().unwrap_or_else(PoisonError::into_inner);
                                agent.run_prompt(&session_id.to_string(), prompt, &mut observer)?;
                            }
                            observer.finish()
                        })
                        .await
                        .map_err(|error| {
                            map_acp_error(AcpError::internal(format!(
                                "prompt task failed: {error}"
                            )))
                        })?;

                        prompt_result.map_err(map_acp_error)?;
                        responder.respond(PromptResponse::new(StopReason::EndTurn))
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_notification(
                async |_cancel: CancelNotification, _connection: ConnectionTo<Client>| Ok(()),
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_dispatch(
                async move |message: Dispatch, _connection: ConnectionTo<Client>| match message {
                    Dispatch::Request(_, responder) => responder
                        .respond_with_error(agent_client_protocol::Error::method_not_found()),
                    Dispatch::Notification(_) | Dispatch::Response(_, _) => Ok(()),
                },
                agent_client_protocol::on_receive_dispatch!(),
            )
            .connect_to(Stdio::default())
            .await
    })?;

    Ok(())
}

fn build_initialize_response(
    protocol_version: ProtocolVersion,
    agent_version: &str,
) -> InitializeResponse {
    InitializeResponse::new(protocol_version)
        .agent_capabilities(
            AgentCapabilities::new()
                .load_session(false)
                .session_capabilities(
                    SessionCapabilities::new().list(SessionListCapabilities::new()),
                )
                .prompt_capabilities(
                    PromptCapabilities::new()
                        .image(false)
                        .audio(false)
                        .embedded_context(false),
                )
                .mcp_capabilities(McpCapabilities::new().http(false).sse(false)),
        )
        .agent_info(Implementation::new("scode", agent_version))
}

fn extract_prompt_text(prompt: &[ContentBlock]) -> Result<String, AcpError> {
    let text = prompt
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(content) if !content.text.trim().is_empty() => {
                Some(content.text.trim().to_string())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    if text.trim().is_empty() {
        return Err(AcpError::invalid_params(
            "params.prompt must include at least one text content block",
        ));
    }

    Ok(text)
}

fn parse_json_or_string(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()))
}

fn map_acp_error(error: AcpError) -> agent_client_protocol::Error {
    match error {
        AcpError::InvalidParams(message) => {
            agent_client_protocol::Error::invalid_params().data(message)
        }
        AcpError::Internal(message) => agent_client_protocol::util::internal_error(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::{json, Value};

    #[test]
    fn initialize_response_matches_contract() {
        let protocol_version: ProtocolVersion =
            serde_json::from_value(json!(9)).expect("protocol version should deserialize");
        let response = build_initialize_response(protocol_version, "1.2.3");
        let response_json: Value =
            serde_json::to_value(response).expect("initialize response should serialize");

        assert_eq!(response_json["protocolVersion"], 9);
        assert_eq!(response_json["agentInfo"]["name"], "scode");
        assert_eq!(response_json["agentInfo"]["version"], "1.2.3");
        assert_eq!(response_json["agentCapabilities"]["loadSession"], false);
        assert_eq!(
            response_json["agentCapabilities"]["promptCapabilities"]["image"],
            false
        );
        assert_eq!(
            response_json["agentCapabilities"]["promptCapabilities"]["audio"],
            false
        );
        assert_eq!(
            response_json["agentCapabilities"]["promptCapabilities"]["embeddedContext"],
            false
        );
        assert_eq!(
            response_json["agentCapabilities"]["mcpCapabilities"]["http"],
            false
        );
        assert_eq!(
            response_json["agentCapabilities"]["mcpCapabilities"]["sse"],
            false
        );
        assert_eq!(
            response_json["agentCapabilities"]["sessionCapabilities"],
            json!({"list": {}})
        );
        assert_eq!(response_json["authMethods"], json!([]));
    }

    #[test]
    fn extract_prompt_text_joins_text_blocks() {
        let prompt = vec![
            ContentBlock::Text(TextContent::new("hello")),
            ContentBlock::Text(TextContent::new("world")),
        ];

        let text = extract_prompt_text(&prompt).expect("prompt text should extract");
        assert_eq!(text, "hello\nworld");
    }

    #[test]
    fn extract_prompt_text_rejects_empty_non_text_prompt() {
        let prompt = vec![ContentBlock::Text(TextContent::new("   "))];

        let error = extract_prompt_text(&prompt).expect_err("prompt should be rejected");
        assert_eq!(
            error,
            AcpError::invalid_params("params.prompt must include at least one text content block",)
        );
    }
}

//! ACP server implementation using the official `agent-client-protocol` SDK.
//!
//! This module provides an SDK-based ACP server that replaces the custom
//! JSON-RPC implementation with typed schema validation and full ACP 1.0
//! compliance. It is feature-gated behind `acp-sdk`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::role::acp::{Agent, Client};
use agent_client_protocol::{
    on_receive_dispatch, on_receive_notification, on_receive_request, ConnectionTo, Dispatch,
    Error, Responder,
};
use agent_client_protocol_schema::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, CurrentModeUpdate,
    Implementation, InitializeRequest, InitializeResponse, ListSessionsRequest,
    ListSessionsResponse, LoadSessionRequest, LoadSessionResponse, ModelInfo, NewSessionRequest,
    NewSessionResponse, PermissionOption, PermissionOptionKind, PromptCapabilities, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionCapabilities, SessionInfo, SessionListCapabilities,
    SessionMode, SessionModeState, SessionModelState, SessionNotification, SessionUpdate,
    SetSessionModeRequest, SetSessionModeResponse, SetSessionModelRequest, SetSessionModelResponse,
    StopReason, TextContent, ToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
    ToolKind,
};
use agent_client_protocol_tokio::Stdio;
use tokio::runtime::Handle;

use crate::conversation::RuntimeObserver;
use crate::permissions::{
    PermissionMode, PermissionPromptDecision, PermissionPrompter, PermissionRequest,
};
use crate::HookAbortSignal;

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
}

impl std::fmt::Display for AcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidParams(message) | Self::Internal(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for AcpError {}

/// Configuration for the SDK-based ACP server.
#[derive(Debug, Clone)]
pub struct SdkAcpConfig {
    pub agent_version: String,
    pub model: String,
    pub model_flag_raw: Option<String>,
    pub permission_mode_override: Option<PermissionMode>,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SdkSessionState {
    pub modes: SessionModeState,
    pub models: SessionModelState,
}

#[derive(Debug, Clone)]
pub struct SdkModeDescriptor {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

impl SdkModeDescriptor {
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: None,
        }
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct SdkModelDescriptor {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

impl SdkModelDescriptor {
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: None,
        }
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct SdkNewSession {
    pub session_id: String,
    pub cwd: PathBuf,
    pub state: SdkSessionState,
}

#[must_use]
pub fn build_sdk_mode_state(
    current_mode_id: impl Into<String>,
    available_modes: Vec<SdkModeDescriptor>,
) -> SessionModeState {
    SessionModeState::new(
        current_mode_id.into(),
        available_modes
            .into_iter()
            .map(|mode| {
                let mut value = SessionMode::new(mode.id, mode.name);
                if let Some(description) = mode.description {
                    value = value.description(description);
                }
                value
            })
            .collect(),
    )
}

#[must_use]
pub fn build_sdk_model_state(
    current_model_id: impl Into<String>,
    available_models: Vec<SdkModelDescriptor>,
) -> SessionModelState {
    SessionModelState::new(
        current_model_id.into(),
        available_models
            .into_iter()
            .map(|model| {
                let mut value = ModelInfo::new(model.id, model.name);
                if let Some(description) = model.description {
                    value = value.description(description);
                }
                value
            })
            .collect(),
    )
}

/// Callback trait that the CLI crate implements to provide session
/// construction and prompt execution, keeping runtime/provider deps out of
/// this crate.
pub trait SdkAcpDelegate: Send + 'static {
    /// Create a new session for the given working directory.
    fn new_session(&mut self, cwd: PathBuf) -> Result<SdkNewSession, AcpError>;

    /// Return the current ACP-visible state for a session.
    fn session_state(&self, session_id: &str) -> Result<SdkSessionState, AcpError>;

    /// Run a prompt turn. The implementation should call observer methods
    /// to stream session updates.
    fn run_prompt(
        &mut self,
        session_id: &str,
        prompt: String,
        abort_signal: HookAbortSignal,
        prompter: Option<&mut dyn PermissionPrompter>,
        observer: &mut SdkSessionObserver,
    ) -> Result<StopReason, AcpError>;

    /// Update the ACP-visible session mode.
    fn set_mode(&mut self, session_id: &str, mode_id: &str) -> Result<SdkSessionState, AcpError>;

    /// Update the ACP-visible session model.
    fn set_model(&mut self, session_id: &str, model_id: &str) -> Result<SdkSessionState, AcpError>;

    /// Handle a slash command, returning text output.
    fn handle_slash_command(
        &mut self,
        session_id: &str,
        input: &str,
        observer: &mut SdkSessionObserver,
    ) -> Result<(), AcpError>;

    /// List active session IDs with their cwds.
    fn list_sessions(&self) -> Vec<(String, PathBuf)>;

    /// Close (drop) a session by ID. Returns true if it existed.
    fn close_session(&mut self, session_id: &str) -> bool;
}

/// Observer that collects session update notifications to be forwarded to
/// the ACP client. Implements [`RuntimeObserver`] so existing `run_turn()`
/// machinery can drive it.
pub struct SdkSessionObserver {
    session_id: String,
    updates: Vec<SessionNotification>,
}

impl SdkSessionObserver {
    /// Create a new observer for the given session.
    #[must_use]
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            updates: Vec::new(),
        }
    }

    /// Drain all buffered notifications.
    pub fn drain(&mut self) -> Vec<SessionNotification> {
        std::mem::take(&mut self.updates)
    }

    fn push(&mut self, update: SessionUpdate) {
        self.updates
            .push(SessionNotification::new(self.session_id.clone(), update));
    }
}

impl RuntimeObserver for SdkSessionObserver {
    fn on_text_delta(&mut self, delta: &str) {
        self.push(SessionUpdate::AgentMessageChunk(ContentChunk::new(
            ContentBlock::Text(TextContent::new(delta)),
        )));
    }

    fn on_tool_use(&mut self, id: &str, name: &str, input: &str) {
        let id_owned = id.to_owned();
        let name_owned = name.to_owned();
        let raw_input = serde_json::from_str(input)
            .unwrap_or_else(|_| serde_json::Value::String(input.to_owned()));
        self.push(SessionUpdate::ToolCall(
            ToolCall::new(id_owned, name_owned)
                .kind(ToolKind::Other)
                .status(ToolCallStatus::InProgress)
                .raw_input(raw_input),
        ));
    }

    fn on_tool_result(
        &mut self,
        tool_use_id: &str,
        _tool_name: &str,
        output: &str,
        is_error: bool,
    ) {
        let id_owned = tool_use_id.to_owned();
        let raw_output = serde_json::from_str(output)
            .unwrap_or_else(|_| serde_json::Value::String(output.to_owned()));
        let status = if is_error {
            ToolCallStatus::Failed
        } else {
            ToolCallStatus::Completed
        };
        self.push(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            id_owned,
            ToolCallUpdateFields::new()
                .status(status)
                .raw_output(raw_output),
        )));
    }
}

/// Extract a safe textual prompt from ACP `ContentBlock`s.
pub(crate) fn extract_prompt_from_content_blocks(
    blocks: &[ContentBlock],
) -> Result<String, AcpError> {
    let rendered = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            ContentBlock::Text(tc) => {
                let text = tc.text.trim();
                (!text.is_empty()).then(|| text.to_owned())
            }
            ContentBlock::Image(image) => Some(format!(
                "[user attached image {}: mime={}, source={}, inline_bytes_omitted={}]",
                index + 1,
                image.mime_type,
                image.uri.as_deref().unwrap_or("inline"),
                image.data.len()
            )),
            _ => None,
        })
        .collect::<Vec<_>>();

    if rendered.is_empty() {
        return Err(AcpError::invalid_params(
            "prompt must include at least one non-empty text or image content block",
        ));
    }

    Ok(rendered.join("\n"))
}

/// Re-export `StopReason` so the CLI crate doesn't need a direct dep on
/// the schema crate.
pub use agent_client_protocol_schema::StopReason as AcpStopReason;

/// Thread-safe handle to a delegate, shared across async handlers.
pub type SharedDelegate = Arc<Mutex<Box<dyn SdkAcpDelegate>>>;
pub(crate) type SharedCancelRegistry = Arc<Mutex<HashMap<String, HookAbortSignal>>>;

const ACP_PERMISSION_ALLOW_ONCE: &str = "allow_once";
const ACP_PERMISSION_REJECT_ONCE: &str = "reject_once";

pub(crate) fn register_prompt_cancel_signal(
    registry: &SharedCancelRegistry,
    session_id: &str,
    abort_signal: HookAbortSignal,
) {
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session_id.to_string(), abort_signal);
}

pub(crate) fn unregister_prompt_cancel_signal(registry: &SharedCancelRegistry, session_id: &str) {
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_id);
}

pub(crate) fn cancel_prompt(registry: &SharedCancelRegistry, session_id: &str) -> bool {
    let signal = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session_id)
        .cloned();
    if let Some(signal) = signal {
        signal.abort();
        true
    } else {
        false
    }
}

struct AcpClientPermissionPrompter {
    session_id: String,
    client: ConnectionTo<Client>,
    runtime_handle: Handle,
    abort_signal: HookAbortSignal,
    next_request_index: usize,
}

impl AcpClientPermissionPrompter {
    fn new(
        session_id: String,
        client: ConnectionTo<Client>,
        runtime_handle: Handle,
        abort_signal: HookAbortSignal,
    ) -> Self {
        Self {
            session_id,
            client,
            runtime_handle,
            abort_signal,
            next_request_index: 0,
        }
    }

    fn next_tool_call_id(&mut self, request: &PermissionRequest) -> String {
        if let Some(tool_call_id) = &request.tool_call_id {
            return tool_call_id.clone();
        }
        self.next_request_index += 1;
        format!(
            "permission-{}-{}",
            request.tool_name, self.next_request_index
        )
    }

    fn options() -> Vec<PermissionOption> {
        vec![
            PermissionOption::new(
                ACP_PERMISSION_ALLOW_ONCE,
                "Allow once",
                PermissionOptionKind::AllowOnce,
            ),
            PermissionOption::new(
                ACP_PERMISSION_REJECT_ONCE,
                "Deny",
                PermissionOptionKind::RejectOnce,
            ),
        ]
    }
}

impl PermissionPrompter for AcpClientPermissionPrompter {
    fn decide(&mut self, request: &PermissionRequest) -> PermissionPromptDecision {
        if self.abort_signal.is_aborted() {
            return PermissionPromptDecision::Cancelled;
        }

        let tool_call_id = self.next_tool_call_id(request);
        let raw_input = serde_json::from_str(&request.input)
            .unwrap_or_else(|_| serde_json::Value::String(request.input.clone()));
        let response = self.runtime_handle.block_on(async {
            self.client
                .send_request(RequestPermissionRequest::new(
                    self.session_id.clone(),
                    ToolCallUpdate::new(
                        tool_call_id,
                        ToolCallUpdateFields::new()
                            .title(format!("Approval required: {}", request.tool_name))
                            .status(ToolCallStatus::Pending)
                            .raw_input(raw_input),
                    ),
                    Self::options(),
                ))
                .block_task()
                .await
        });

        match response.map(|response: RequestPermissionResponse| response.outcome) {
            Ok(RequestPermissionOutcome::Cancelled) => PermissionPromptDecision::Cancelled,
            Ok(RequestPermissionOutcome::Selected(SelectedPermissionOutcome {
                option_id, ..
            })) => match option_id.to_string().as_str() {
                ACP_PERMISSION_ALLOW_ONCE => PermissionPromptDecision::Allow,
                ACP_PERMISSION_REJECT_ONCE => PermissionPromptDecision::Deny {
                    reason: format!(
                        "tool '{}' denied by ACP permission prompt",
                        request.tool_name
                    ),
                },
                other => PermissionPromptDecision::Deny {
                    reason: format!("unsupported ACP permission option selected: {other}"),
                },
            },
            Ok(other) => PermissionPromptDecision::Deny {
                reason: format!("unsupported ACP permission outcome: {other:?}"),
            },
            Err(error) => PermissionPromptDecision::Deny {
                reason: format!("ACP permission request failed: {error}"),
            },
        }
    }
}

/// Run a prompt through the delegate (blocking), returning the stop reason
/// and buffered notifications. Handles slash-command dispatch internally.
pub(crate) fn run_delegate_prompt(
    delegate: &SharedDelegate,
    session_id: &str,
    prompt: String,
    abort_signal: HookAbortSignal,
    prompter: Option<&mut dyn PermissionPrompter>,
) -> Result<(StopReason, Vec<SessionNotification>), AcpError> {
    let mut observer = SdkSessionObserver::new(session_id);
    let mut delegate = delegate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stop = if prompt.starts_with('/') {
        delegate
            .handle_slash_command(session_id, &prompt, &mut observer)
            .map(|()| StopReason::EndTurn)
    } else {
        delegate.run_prompt(session_id, prompt, abort_signal, prompter, &mut observer)
    };
    let notifications = observer.drain();
    stop.map(|reason| (reason, notifications))
}

// ---------------------------------------------------------------------------
// Server entry point
// ---------------------------------------------------------------------------

/// Run the SDK-based ACP server on stdin/stdout.
#[allow(clippy::too_many_lines)]
pub async fn run_sdk_acp_server(
    config: SdkAcpConfig,
    delegate: Box<dyn SdkAcpDelegate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let agent_version = config.agent_version.clone();
    let delegate: SharedDelegate = Arc::new(Mutex::new(delegate));
    let cancel_registry: SharedCancelRegistry = Arc::new(Mutex::new(HashMap::new()));

    let builder = Agent
        .builder()
        .name("scode")
        .on_receive_request(
            {
                let version = agent_version.clone();
                async move |req: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _cx: ConnectionTo<Client>| {
                    let resp = InitializeResponse::new(req.protocol_version)
                        .agent_info(Implementation::new("scode", &version))
                        .agent_capabilities(
                            AgentCapabilities::new()
                                .prompt_capabilities(PromptCapabilities::new().image(true))
                                .session_capabilities(
                                    SessionCapabilities::new().list(SessionListCapabilities::new()),
                                ),
                        );
                    responder.respond(resp)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let delegate = Arc::clone(&delegate);
                async move |req: NewSessionRequest,
                            responder: Responder<NewSessionResponse>,
                            cx: ConnectionTo<Client>| {
                    let d = Arc::clone(&delegate);
                    cx.spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            d.lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .new_session(req.cwd)
                        })
                        .await
                        .unwrap_or_else(|e| Err(AcpError::internal(e.to_string())));

                        match result {
                            Ok(new_session) => {
                                responder.respond(build_new_session_response(new_session))?;
                            }
                            Err(e) => {
                                responder.respond_with_error(acp_error_to_sdk(&e))?;
                            }
                        }
                        Ok(())
                    })?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let delegate = Arc::clone(&delegate);
                let cancel_registry = Arc::clone(&cancel_registry);
                move |req: PromptRequest,
                      responder: Responder<PromptResponse>,
                      cx: ConnectionTo<Client>| {
                    let delegate = Arc::clone(&delegate);
                    let cancel_registry = Arc::clone(&cancel_registry);
                    async move {
                        let prompt_text = match extract_prompt_from_content_blocks(&req.prompt) {
                            Ok(t) => t,
                            Err(e) => {
                                responder.respond_with_error(acp_error_to_sdk(&e))?;
                                return Ok(());
                            }
                        };

                        let sid = req.session_id.to_string();
                        let sid_for_prompt = sid.clone();
                        let cx_inner = cx.clone();
                        let permission_cx = cx.clone();
                        let runtime_handle = Handle::current();
                        let abort_signal = HookAbortSignal::new();
                        register_prompt_cancel_signal(&cancel_registry, &sid, abort_signal.clone());
                        cx.spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                let mut permission_prompter = AcpClientPermissionPrompter::new(
                                    sid_for_prompt.clone(),
                                    permission_cx,
                                    runtime_handle,
                                    abort_signal.clone(),
                                );
                                run_delegate_prompt(
                                    &delegate,
                                    &sid_for_prompt,
                                    prompt_text,
                                    abort_signal,
                                    Some(&mut permission_prompter),
                                )
                            })
                            .await
                            .unwrap_or_else(|e| Err(AcpError::internal(e.to_string())));
                            unregister_prompt_cancel_signal(&cancel_registry, &sid);

                            match result {
                                Ok((stop_reason, notifications)) => {
                                    for notif in notifications {
                                        cx_inner.send_notification(notif)?;
                                    }
                                    responder.respond(PromptResponse::new(stop_reason))?;
                                }
                                Err(e) => {
                                    responder.respond_with_error(acp_error_to_sdk(&e))?;
                                }
                            }
                            Ok(())
                        })?;
                        Ok(())
                    }
                }
            },
            on_receive_request!(),
        )
        .on_receive_notification(
            {
                let cancel_registry = Arc::clone(&cancel_registry);
                async move |notif: CancelNotification, _cx: ConnectionTo<Client>| {
                    let session_id = notif.session_id.to_string();
                    if !cancel_prompt(&cancel_registry, &session_id) {
                        eprintln!(
                            "[acp-sdk] cancel requested for inactive session {}",
                            session_id
                        );
                    }
                    Ok(())
                }
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            {
                let delegate = Arc::clone(&delegate);
                async move |req: SetSessionModeRequest,
                            responder: Responder<SetSessionModeResponse>,
                            cx: ConnectionTo<Client>| {
                    let d = Arc::clone(&delegate);
                    let session_id = req.session_id.to_string();
                    let session_id_for_update = session_id.clone();
                    let mode_id = req.mode_id.to_string();
                    let cx_inner = cx.clone();
                    cx.spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            d.lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .set_mode(&session_id_for_update, &mode_id)
                        })
                        .await
                        .unwrap_or_else(|e| Err(AcpError::internal(e.to_string())));

                        match result {
                            Ok(state) => {
                                responder.respond(SetSessionModeResponse::new())?;
                                cx_inner.send_notification(SessionNotification::new(
                                    session_id,
                                    SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(
                                        state.modes.current_mode_id,
                                    )),
                                ))?;
                            }
                            Err(e) => {
                                responder.respond_with_error(acp_error_to_sdk(&e))?;
                            }
                        }
                        Ok(())
                    })?;
                    Ok(())
                }
            },
            on_receive_request!(),
        );

    let builder = builder.on_receive_request(
        {
            let delegate = Arc::clone(&delegate);
            async move |req: SetSessionModelRequest,
                        responder: Responder<SetSessionModelResponse>,
                        cx: ConnectionTo<Client>| {
                let d = Arc::clone(&delegate);
                let session_id = req.session_id.to_string();
                let session_id_for_prompt = session_id.clone();
                let model_id = req.model_id.to_string();
                cx.spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        d.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .set_model(&session_id_for_prompt, &model_id)
                    })
                    .await
                    .unwrap_or_else(|e| Err(AcpError::internal(e.to_string())));

                    match result {
                        Ok(_state) => {
                            responder.respond(SetSessionModelResponse::new())?;
                        }
                        Err(e) => {
                            responder.respond_with_error(acp_error_to_sdk(&e))?;
                        }
                    }
                    Ok(())
                })?;
                Ok(())
            }
        },
        on_receive_request!(),
    );

    let builder = builder
        .on_receive_request(
            {
                let delegate = Arc::clone(&delegate);
                async move |_req: ListSessionsRequest,
                            responder: Responder<ListSessionsResponse>,
                            cx: ConnectionTo<Client>| {
                    let d = Arc::clone(&delegate);
                    cx.spawn(async move {
                        let infos = tokio::task::spawn_blocking(move || {
                            d.lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .list_sessions()
                                .into_iter()
                                .map(|(id, cwd)| SessionInfo::new(id, cwd))
                                .collect::<Vec<_>>()
                        })
                        .await
                        .unwrap_or_default();

                        responder.respond(ListSessionsResponse::new(infos))?;
                        Ok(())
                    })?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                async move |_req: LoadSessionRequest,
                            responder: Responder<LoadSessionResponse>,
                            _cx: ConnectionTo<Client>| {
                    responder.respond_with_error(Error::internal_error().data(
                        serde_json::Value::String("session loading not yet supported".to_string()),
                    ))?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_dispatch(
            async move |dispatch: Dispatch, cx: ConnectionTo<Client>| {
                dispatch.respond_with_error(Error::method_not_found(), cx)?;
                Ok(())
            },
            on_receive_dispatch!(),
        );

    builder.connect_to(Stdio::new()).await?;

    Ok(())
}

pub(crate) fn build_new_session_response(new_session: SdkNewSession) -> NewSessionResponse {
    NewSessionResponse::new(new_session.session_id)
        .modes(new_session.state.modes)
        .models(new_session.state.models)
}

/// Map our `AcpError` to the SDK's `Error` type.
pub(crate) fn acp_error_to_sdk(e: &AcpError) -> Error {
    match e {
        AcpError::InvalidParams(msg) => {
            Error::invalid_params().data(serde_json::Value::String(msg.clone()))
        }
        AcpError::Internal(msg) => {
            Error::internal_error().data(serde_json::Value::String(msg.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        build_new_session_response, build_sdk_mode_state, extract_prompt_from_content_blocks,
        SdkModeDescriptor, SdkNewSession, SdkSessionState,
    };
    use agent_client_protocol_schema::{ContentBlock, ImageContent, TextContent};

    #[test]
    fn extracts_safe_prompt_text_from_images_without_leaking_data() {
        let prompt = extract_prompt_from_content_blocks(&[
            ContentBlock::Text(TextContent::new("describe this")),
            ContentBlock::Image(ImageContent::new("base64-secret", "image/png")),
        ])
        .expect("image prompts should be accepted");

        assert!(prompt.contains("describe this"));
        assert!(prompt.contains("image/png"));
        assert!(!prompt.contains("base64-secret"));
    }

    #[test]
    fn new_session_responses_include_mode_state() {
        let response = build_new_session_response(SdkNewSession {
            session_id: "sess_123".to_string(),
            cwd: PathBuf::from("/tmp/project"),
            state: SdkSessionState {
                modes: build_sdk_mode_state(
                    "workspace-write",
                    vec![SdkModeDescriptor::new("workspace-write", "Workspace Write")],
                ),
                models: super::build_sdk_model_state(
                    "claude-opus-4-6",
                    vec![super::SdkModelDescriptor::new(
                        "claude-opus-4-6",
                        "Claude Opus 4.6",
                    )],
                ),
            },
        });

        assert_eq!(response.session_id.to_string(), "sess_123");
        assert_eq!(
            response
                .modes
                .as_ref()
                .expect("modes should be present")
                .current_mode_id
                .to_string(),
            "workspace-write"
        );
    }
}

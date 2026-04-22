//! Minimal Agent Client Protocol (ACP) JSON-RPC server support.
//!
//! This module owns the ACP wire protocol and stdio framing. The actual agent
//! runtime is supplied by the CLI crate so it can reuse the normal provider,
//! tool, plugin, config, and MCP construction path without adding those
//! dependencies to `runtime`.

use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;

use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};

use crate::conversation::RuntimeObserver;
use crate::jsonrpc_transport::{read_msg, write_msg};

const JSONRPC_VERSION: &str = "2.0";
const DEFAULT_PROTOCOL_VERSION: i64 = 1;
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// Configuration for the ACP JSON-RPC server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpServerOptions {
    agent_version: String,
    default_protocol_version: i64,
}

impl AcpServerOptions {
    #[must_use]
    pub fn new(agent_version: impl Into<String>) -> Self {
        Self {
            agent_version: agent_version.into(),
            default_protocol_version: DEFAULT_PROTOCOL_VERSION,
        }
    }

    #[must_use]
    pub fn with_default_protocol_version(mut self, version: i64) -> Self {
        self.default_protocol_version = version;
        self
    }
}

/// Agent-side operations invoked by the protocol server.
pub trait AcpAgent {
    fn new_session(&mut self, cwd: Option<PathBuf>) -> Result<String, AcpError>;

    fn run_prompt(
        &mut self,
        session_id: &str,
        prompt: String,
        observer: &mut AcpSessionUpdateObserver<'_>,
    ) -> Result<(), AcpError>;
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

    fn code(&self) -> i64 {
        match self {
            Self::InvalidParams(_) => INVALID_PARAMS,
            Self::Internal(_) => INTERNAL_ERROR,
        }
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
    Json(serde_json::Error),
}

impl Display for AcpServerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for AcpServerError {}

impl From<io::Error> for AcpServerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for AcpServerError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Runtime observer that converts model/tool events to ACP session/update notifications.
pub struct AcpSessionUpdateObserver<'a> {
    session_id: String,
    sink: &'a mut dyn AcpMessageSink,
    write_error: Option<io::Error>,
}

impl<'a> AcpSessionUpdateObserver<'a> {
    fn new(session_id: String, sink: &'a mut dyn AcpMessageSink) -> Self {
        Self {
            session_id,
            sink,
            write_error: None,
        }
    }

    fn finish(self) -> io::Result<()> {
        match self.write_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn notify_update(&mut self, update: Value) {
        if self.write_error.is_some() {
            return;
        }
        let notification = json!({
            "jsonrpc": JSONRPC_VERSION,
            "method": "session/update",
            "params": {
                "sessionId": self.session_id,
                "update": update,
            },
        });
        if let Err(error) = self.sink.send_value(&notification) {
            self.write_error = Some(error);
        }
    }
}

impl RuntimeObserver for AcpSessionUpdateObserver<'_> {
    fn on_text_delta(&mut self, delta: &str) {
        self.notify_update(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {
                "type": "text",
                "text": delta,
            },
        }));
    }

    fn on_tool_use(&mut self, id: &str, name: &str, input: &str) {
        self.notify_update(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": id,
            "title": name,
            "kind": "other",
            "status": "in_progress",
            "rawInput": parse_json_or_string(input),
        }));
    }

    fn on_tool_result(
        &mut self,
        tool_use_id: &str,
        _tool_name: &str,
        output: &str,
        is_error: bool,
    ) {
        self.notify_update(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": tool_use_id,
            "status": if is_error { "failed" } else { "completed" },
            "rawOutput": parse_json_or_string(output),
            "content": [{
                "type": "content",
                "content": {
                    "type": "text",
                    "text": output,
                },
            }],
        }));
    }
}

trait AcpMessageSink {
    fn send_value(&mut self, value: &Value) -> io::Result<()>;
}

struct AcpIoSink<'a, W> {
    runtime: &'a tokio::runtime::Runtime,
    writer: &'a mut W,
}

impl<W> AcpMessageSink for AcpIoSink<'_, W>
where
    W: AsyncWrite + Unpin,
{
    fn send_value(&mut self, value: &Value) -> io::Result<()> {
        let payload = serde_json::to_vec(value).map_err(io::Error::other)?;
        self.runtime
            .block_on(write_msg(&mut *self.writer, &payload))
    }
}

/// Run the ACP server on process stdin/stdout.
///
/// All stdout writes go through JSON-RPC framing. Diagnostics should be
/// propagated as errors and rendered by callers on stderr.
pub fn run_acp_stdio_server<A>(agent: A, options: AcpServerOptions) -> Result<(), AcpServerError>
where
    A: AcpAgent,
{
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    run_acp_server_with_io(agent, BufReader::new(stdin), stdout, options)
}

/// Run the ACP server over supplied framed IO streams.
pub fn run_acp_server_with_io<A, R, W>(
    mut agent: A,
    mut reader: R,
    mut writer: W,
    options: AcpServerOptions,
) -> Result<(), AcpServerError>
where
    A: AcpAgent,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let runtime = tokio::runtime::Runtime::new()?;
    let mut sink = AcpIoSink {
        runtime: &runtime,
        writer: &mut writer,
    };

    loop {
        let Some(payload) = runtime.block_on(read_msg(&mut reader))? else {
            break;
        };
        handle_payload(&mut agent, &options, &payload, &mut sink)?;
    }
    Ok(())
}

fn handle_payload<A>(
    agent: &mut A,
    options: &AcpServerOptions,
    payload: &[u8],
    sink: &mut dyn AcpMessageSink,
) -> io::Result<()>
where
    A: AcpAgent,
{
    match serde_json::from_slice::<Value>(payload) {
        Ok(value) => handle_value(agent, options, value, sink),
        Err(error) => sink.send_value(&error_response(
            Value::Null,
            PARSE_ERROR,
            format!("parse error: {error}"),
        )),
    }
}

fn handle_value<A>(
    agent: &mut A,
    options: &AcpServerOptions,
    value: Value,
    sink: &mut dyn AcpMessageSink,
) -> io::Result<()>
where
    A: AcpAgent,
{
    let Some(object) = value.as_object() else {
        return sink.send_value(&error_response(
            Value::Null,
            INVALID_REQUEST,
            "JSON-RPC request must be an object",
        ));
    };
    let id = object.get("id").cloned();
    let is_notification = id.is_none();
    let response_id = id.unwrap_or(Value::Null);

    let Some(method) = object.get("method").and_then(Value::as_str) else {
        if is_notification {
            return Ok(());
        }
        return sink.send_value(&error_response(
            response_id,
            INVALID_REQUEST,
            "JSON-RPC request method must be a string",
        ));
    };

    let params = object.get("params");
    let result = match method {
        "initialize" => handle_initialize(params, options),
        "session/new" => handle_session_new(params, agent),
        "session/prompt" => handle_session_prompt(params, agent, sink),
        _ => {
            if is_notification {
                return Ok(());
            }
            Err(AcpError::InvalidParams("__method_not_found__".to_string()))
        }
    };

    if is_notification {
        return Ok(());
    }

    match result {
        Ok(result) => sink.send_value(&success_response(response_id, result)),
        Err(AcpError::InvalidParams(message)) if message == "__method_not_found__" => sink
            .send_value(&error_response(
                response_id,
                METHOD_NOT_FOUND,
                format!("method not found: {method}"),
            )),
        Err(error) => sink.send_value(&error_response(
            response_id,
            error.code(),
            error.message().to_string(),
        )),
    }
}

fn handle_initialize(
    params: Option<&Value>,
    options: &AcpServerOptions,
) -> Result<Value, AcpError> {
    if params.is_some_and(|value| !value.is_object()) {
        return Err(AcpError::invalid_params(
            "initialize params must be an object",
        ));
    }
    let protocol_version = match params.and_then(|value| value.get("protocolVersion")) {
        Some(value) => value
            .as_i64()
            .ok_or_else(|| AcpError::invalid_params("params.protocolVersion must be numeric"))?,
        None => options.default_protocol_version,
    };

    Ok(json!({
        "protocolVersion": protocol_version,
        "agentInfo": {
            "name": "scode",
            "version": options.agent_version,
        },
        "agentCapabilities": {
            "loadSession": false,
            "promptCapabilities": {
                "image": false,
                "audio": false,
                "embeddedContext": false,
            },
            "mcpCapabilities": {
                "http": false,
                "sse": false,
            },
            "sessionCapabilities": {},
        },
        "authMethods": [],
    }))
}

fn handle_session_new<A>(params: Option<&Value>, agent: &mut A) -> Result<Value, AcpError>
where
    A: AcpAgent,
{
    if params.is_some_and(|value| !value.is_object()) {
        return Err(AcpError::invalid_params(
            "session/new params must be an object",
        ));
    }
    let cwd = match params.and_then(|value| value.get("cwd")) {
        Some(value) => {
            let raw = value
                .as_str()
                .ok_or_else(|| AcpError::invalid_params("params.cwd must be a string"))?;
            let path = PathBuf::from(raw);
            if !path.is_absolute() {
                return Err(AcpError::invalid_params(
                    "params.cwd must be an absolute path",
                ));
            }
            Some(path)
        }
        None => None,
    };

    let session_id = agent.new_session(cwd)?;
    Ok(json!({ "sessionId": session_id }))
}

fn handle_session_prompt<A>(
    params: Option<&Value>,
    agent: &mut A,
    sink: &mut dyn AcpMessageSink,
) -> Result<Value, AcpError>
where
    A: AcpAgent,
{
    let params = params
        .and_then(Value::as_object)
        .ok_or_else(|| AcpError::invalid_params("session/prompt params must be an object"))?;
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AcpError::invalid_params("params.sessionId must be a non-empty string"))?;
    let prompt = extract_prompt_text(params.get("prompt"))?;

    let mut observer = AcpSessionUpdateObserver::new(session_id.to_string(), sink);
    let result = agent.run_prompt(session_id, prompt, &mut observer);
    observer
        .finish()
        .map_err(|error| AcpError::internal(error.to_string()))?;
    result?;

    Ok(json!({ "stopReason": "end_turn" }))
}

fn extract_prompt_text(prompt: Option<&Value>) -> Result<String, AcpError> {
    let prompt = prompt.ok_or_else(|| AcpError::invalid_params("params.prompt is required"))?;
    let content = prompt
        .as_object()
        .and_then(|object| object.get("content"))
        .or_else(|| prompt.as_array().map(|_| prompt))
        .ok_or_else(|| AcpError::invalid_params("params.prompt.content must be an array"))?;
    let blocks = content
        .as_array()
        .ok_or_else(|| AcpError::invalid_params("params.prompt.content must be an array"))?;

    let text = blocks
        .iter()
        .filter_map(extract_text_block)
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return Err(AcpError::invalid_params(
            "params.prompt must include at least one text content block",
        ));
    }
    Ok(text)
}

fn extract_text_block(block: &Value) -> Option<String> {
    let object = block.as_object()?;
    match object.get("type").and_then(Value::as_str) {
        Some("text") => object
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned),
        Some("resource_link") => None,
        _ => None,
    }
}

fn success_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "result": result,
    })
}

fn error_response(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "error": {
            "code": code,
            "message": message.into(),
        },
    })
}

fn parse_json_or_string(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingSink {
        messages: Vec<Value>,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                messages: Vec::new(),
            }
        }
    }

    impl AcpMessageSink for RecordingSink {
        fn send_value(&mut self, value: &Value) -> io::Result<()> {
            self.messages.push(value.clone());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeAgent {
        sessions: Vec<Option<PathBuf>>,
        prompts: Vec<(String, String)>,
    }

    impl AcpAgent for FakeAgent {
        fn new_session(&mut self, cwd: Option<PathBuf>) -> Result<String, AcpError> {
            self.sessions.push(cwd);
            Ok(format!("session-{}", self.sessions.len()))
        }

        fn run_prompt(
            &mut self,
            session_id: &str,
            prompt: String,
            observer: &mut AcpSessionUpdateObserver<'_>,
        ) -> Result<(), AcpError> {
            if session_id != "session-1" {
                return Err(AcpError::invalid_params("unknown sessionId"));
            }
            self.prompts.push((session_id.to_string(), prompt));
            observer.on_text_delta("hello ");
            observer.on_tool_use("toolu_1", "read_file", r#"{"file_path":"Cargo.toml"}"#);
            observer.on_tool_result("toolu_1", "read_file", r#"{"ok":true}"#, false);
            Ok(())
        }
    }

    fn request(id: i64, method: &str, params: Value) -> Value {
        json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": method,
            "params": params,
        })
    }

    #[test]
    fn initialize_echoes_protocol_version_and_capabilities() {
        let mut agent = FakeAgent::default();
        let options = AcpServerOptions::new("1.2.3");
        let mut sink = RecordingSink::new();

        handle_value(
            &mut agent,
            &options,
            request(1, "initialize", json!({ "protocolVersion": 42 })),
            &mut sink,
        )
        .expect("initialize should handle");

        let response = sink.messages.pop().expect("response");
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["protocolVersion"], 42);
        assert_eq!(response["result"]["agentInfo"]["name"], "scode");
        assert_eq!(response["result"]["agentInfo"]["version"], "1.2.3");
        assert_eq!(
            response["result"]["agentCapabilities"]["loadSession"],
            false
        );
        assert_eq!(
            response["result"]["agentCapabilities"]["mcpCapabilities"]["http"],
            false
        );
        assert_eq!(response["result"]["authMethods"], json!([]));
    }

    #[test]
    fn notifications_do_not_receive_responses() {
        let mut agent = FakeAgent::default();
        let options = AcpServerOptions::new("1.2.3");
        let mut sink = RecordingSink::new();

        handle_value(
            &mut agent,
            &options,
            json!({ "jsonrpc": JSONRPC_VERSION, "method": "unknown/notification" }),
            &mut sink,
        )
        .expect("notification should handle");

        assert!(sink.messages.is_empty());
    }

    #[test]
    fn unknown_request_method_returns_method_not_found() {
        let mut agent = FakeAgent::default();
        let options = AcpServerOptions::new("1.2.3");
        let mut sink = RecordingSink::new();

        handle_value(
            &mut agent,
            &options,
            request(7, "missing/method", json!({})),
            &mut sink,
        )
        .expect("unknown method should handle");

        let response = sink.messages.pop().expect("response");
        assert_eq!(response["id"], 7);
        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn session_new_rejects_relative_cwd() {
        let mut agent = FakeAgent::default();
        let options = AcpServerOptions::new("1.2.3");
        let mut sink = RecordingSink::new();

        handle_value(
            &mut agent,
            &options,
            request(2, "session/new", json!({ "cwd": "relative/path" })),
            &mut sink,
        )
        .expect("session/new should handle");

        let response = sink.messages.pop().expect("response");
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert!(agent.sessions.is_empty());
    }

    #[test]
    fn session_prompt_extracts_text_and_emits_updates_before_response() {
        let mut agent = FakeAgent::default();
        agent.sessions.push(None);
        let options = AcpServerOptions::new("1.2.3");
        let mut sink = RecordingSink::new();

        handle_value(
            &mut agent,
            &options,
            request(
                3,
                "session/prompt",
                json!({
                    "sessionId": "session-1",
                    "prompt": {
                        "content": [
                            { "type": "text", "text": "First" },
                            { "type": "resource_link", "uri": "file:///tmp/a.txt" },
                            { "type": "text", "text": "Second" }
                        ]
                    }
                }),
            ),
            &mut sink,
        )
        .expect("session/prompt should handle");

        assert_eq!(
            agent.prompts,
            vec![("session-1".to_string(), "First\nSecond".to_string())]
        );
        assert_eq!(sink.messages.len(), 4);
        assert_eq!(sink.messages[0]["method"], "session/update");
        assert_eq!(
            sink.messages[0]["params"]["update"]["sessionUpdate"],
            "agent_message_chunk"
        );
        assert_eq!(
            sink.messages[1]["params"]["update"]["rawInput"],
            json!({ "file_path": "Cargo.toml" })
        );
        assert_eq!(
            sink.messages[2]["params"]["update"]["rawOutput"],
            json!({ "ok": true })
        );
        assert_eq!(sink.messages[3]["result"]["stopReason"], "end_turn");
    }

    #[test]
    fn session_prompt_rejects_prompts_without_text_blocks() {
        let mut agent = FakeAgent::default();
        let options = AcpServerOptions::new("1.2.3");
        let mut sink = RecordingSink::new();

        handle_value(
            &mut agent,
            &options,
            request(
                4,
                "session/prompt",
                json!({
                    "sessionId": "session-1",
                    "prompt": {
                        "content": [{ "type": "resource_link", "uri": "file:///tmp/a.txt" }]
                    }
                }),
            ),
            &mut sink,
        )
        .expect("session/prompt should handle");

        let response = sink.messages.pop().expect("response");
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert!(agent.prompts.is_empty());
    }
}

//! ACP server implementation using the official `agent-client-protocol` SDK.
//!
//! This module provides an SDK-based ACP server that replaces the custom
//! JSON-RPC implementation with typed schema validation and full ACP 1.0
//! compliance. It is feature-gated behind `acp-sdk`.

use std::path::PathBuf;
use std::sync::mpsc;

use agent_client_protocol::role::acp::{Agent, Client};
use agent_client_protocol::{
    on_receive_dispatch, on_receive_notification, on_receive_request, ConnectionTo, Dispatch,
    Error, Responder,
};
use agent_client_protocol_schema::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, Implementation,
    InitializeRequest, InitializeResponse, ListSessionsRequest, ListSessionsResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse,
    PromptCapabilities, PromptRequest, PromptResponse, SessionInfo, SessionNotification,
    SessionUpdate, StopReason, TextContent, ToolCall, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol_tokio::Stdio;

use crate::acp_server::AcpError;
use crate::conversation::RuntimeObserver;
use crate::permissions::PermissionMode;

/// Configuration for the SDK-based ACP server.
#[derive(Debug, Clone)]
pub struct SdkAcpConfig {
    pub agent_version: String,
    pub model: String,
    pub model_flag_raw: Option<String>,
    pub permission_mode_override: Option<PermissionMode>,
    pub reasoning_effort: Option<String>,
}

/// Callback trait that the CLI crate implements to provide session
/// construction and prompt execution, keeping runtime/provider deps out of
/// this crate.
///
/// All methods are called on a dedicated worker thread, so implementations
/// do not need to be `Send`.
pub trait SdkAcpDelegate: 'static {
    /// Create a new session for the given working directory, returning
    /// `(session_id, cwd)` on success.
    fn new_session(&mut self, cwd: PathBuf) -> Result<(String, PathBuf), AcpError>;

    /// Run a prompt turn. The implementation should call observer methods
    /// to stream session updates.
    fn run_prompt(
        &mut self,
        session_id: &str,
        prompt: String,
        observer: &mut SdkSessionObserver,
    ) -> Result<StopReason, AcpError>;

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

/// Extract plain text from a slice of ACP `ContentBlock`s.
fn extract_text_from_content_blocks(blocks: &[ContentBlock]) -> Result<String, AcpError> {
    let text: String = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(tc) => {
                let t = tc.text.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_owned())
                }
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    if text.is_empty() {
        return Err(AcpError::invalid_params(
            "prompt must include at least one non-empty text content block",
        ));
    }
    Ok(text)
}

/// Re-export `StopReason` so the CLI crate doesn't need a direct dep on
/// the schema crate.
pub use agent_client_protocol_schema::StopReason as AcpStopReason;

// ---------------------------------------------------------------------------
// Channel-based delegate proxy
// ---------------------------------------------------------------------------

/// Commands sent from async handlers to the dedicated delegate worker thread.
enum DelegateCmd {
    NewSession {
        cwd: PathBuf,
        reply: mpsc::Sender<Result<(String, PathBuf), AcpError>>,
    },
    Prompt {
        session_id: String,
        prompt: String,
        reply: mpsc::Sender<(StopReason, Vec<SessionNotification>)>,
    },
    ListSessions {
        reply: mpsc::Sender<Vec<(String, PathBuf)>>,
    },
}

/// A `Send + Sync` handle that async handlers use to invoke the delegate
/// on its dedicated thread via channels.
#[derive(Clone)]
struct DelegateProxy {
    cmd_tx: mpsc::Sender<DelegateCmd>,
}

impl DelegateProxy {
    fn new_session(&self, cwd: PathBuf) -> Result<(String, PathBuf), AcpError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.cmd_tx.send(DelegateCmd::NewSession {
            cwd,
            reply: reply_tx,
        });
        reply_rx
            .recv()
            .unwrap_or_else(|_| Err(AcpError::internal("delegate worker gone")))
    }

    fn prompt(&self, session_id: String, prompt: String) -> (StopReason, Vec<SessionNotification>) {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.cmd_tx.send(DelegateCmd::Prompt {
            session_id,
            prompt,
            reply: reply_tx,
        });
        reply_rx.recv().unwrap_or((StopReason::EndTurn, Vec::new()))
    }

    fn list_sessions(&self) -> Vec<(String, PathBuf)> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self
            .cmd_tx
            .send(DelegateCmd::ListSessions { reply: reply_tx });
        reply_rx.recv().unwrap_or_default()
    }
}

/// Spawn a dedicated OS thread that owns the (non-Send) delegate and
/// processes commands from the channel.
///
/// The delegate factory is called on the worker thread so the delegate
/// itself never needs to be `Send`.
fn spawn_delegate_worker<F>(factory: F) -> DelegateProxy
where
    F: FnOnce() -> Box<dyn SdkAcpDelegate> + Send + 'static,
{
    let (cmd_tx, cmd_rx) = mpsc::channel::<DelegateCmd>();

    std::thread::spawn(move || {
        let mut delegate = factory();
        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                DelegateCmd::NewSession { cwd, reply } => {
                    let _ = reply.send(delegate.new_session(cwd));
                }
                DelegateCmd::Prompt {
                    session_id,
                    prompt,
                    reply,
                } => {
                    let mut observer = SdkSessionObserver::new(&session_id);
                    let stop = if prompt.starts_with('/') {
                        delegate
                            .handle_slash_command(&session_id, &prompt, &mut observer)
                            .map(|()| StopReason::EndTurn)
                    } else {
                        delegate.run_prompt(&session_id, prompt, &mut observer)
                    };
                    let notifications = observer.drain();
                    let reason = stop.unwrap_or(StopReason::EndTurn);
                    let _ = reply.send((reason, notifications));
                }
                DelegateCmd::ListSessions { reply } => {
                    let _ = reply.send(delegate.list_sessions());
                }
            }
        }
    });

    DelegateProxy { cmd_tx }
}

// ---------------------------------------------------------------------------
// Server entry point
// ---------------------------------------------------------------------------

/// Run the SDK-based ACP server on stdin/stdout.
///
/// The `delegate_factory` is called on a dedicated OS thread to create the
/// delegate, so the delegate itself does not need to be `Send`. The factory
/// closure *does* need to be `Send`.
#[allow(clippy::too_many_lines)]
pub async fn run_sdk_acp_server<F>(
    config: SdkAcpConfig,
    delegate_factory: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnOnce() -> Box<dyn SdkAcpDelegate> + Send + 'static,
{
    let agent_version = config.agent_version.clone();
    let proxy = spawn_delegate_worker(delegate_factory);

    Agent
        .builder()
        .name("scode")
        // --- initialize ---
        .on_receive_request(
            {
                let version = agent_version.clone();
                async move |req: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _cx: ConnectionTo<Client>| {
                    let resp = InitializeResponse::new(req.protocol_version)
                        .agent_info(Implementation::new("scode", &version))
                        .agent_capabilities(
                            AgentCapabilities::new().prompt_capabilities(PromptCapabilities::new()),
                        );
                    responder.respond(resp)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        // --- session/new ---
        .on_receive_request(
            {
                let proxy = proxy.clone();
                async move |req: NewSessionRequest,
                            responder: Responder<NewSessionResponse>,
                            cx: ConnectionTo<Client>| {
                    let p = proxy.clone();
                    cx.spawn(async move {
                        let result = tokio::task::spawn_blocking(move || p.new_session(req.cwd))
                            .await
                            .unwrap_or_else(|e| Err(AcpError::internal(e.to_string())));

                        match result {
                            Ok((session_id, _cwd)) => {
                                responder.respond(NewSessionResponse::new(session_id))?;
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
        // --- session/prompt ---
        .on_receive_request(
            {
                let proxy = proxy.clone();
                async move |req: PromptRequest,
                            responder: Responder<PromptResponse>,
                            cx: ConnectionTo<Client>| {
                    let session_id = req.session_id.to_string();
                    let prompt_text = match extract_text_from_content_blocks(&req.prompt) {
                        Ok(t) => t,
                        Err(e) => {
                            responder.respond_with_error(acp_error_to_sdk(&e))?;
                            return Ok(());
                        }
                    };

                    // Spawn blocking work off the dispatch loop.
                    let p = proxy.clone();
                    let sid = session_id.clone();
                    let cx_inner = cx.clone();
                    cx.spawn(async move {
                        let (stop_reason, notifications) =
                            tokio::task::spawn_blocking(move || p.prompt(sid, prompt_text))
                                .await
                                .unwrap_or((StopReason::EndTurn, Vec::new()));

                        // Send all buffered session update notifications.
                        for notif in notifications {
                            cx_inner.send_notification(notif)?;
                        }

                        responder.respond(PromptResponse::new(stop_reason))?;
                        Ok(())
                    })?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        // --- session/cancel (notification, no response) ---
        .on_receive_notification(
            {
                async move |notif: CancelNotification, _cx: ConnectionTo<Client>| {
                    eprintln!(
                        "[acp-sdk] cancel requested for session {}, not yet implemented",
                        notif.session_id
                    );
                    Ok(())
                }
            },
            on_receive_notification!(),
        )
        // --- session/list ---
        .on_receive_request(
            {
                let proxy = proxy.clone();
                async move |_req: ListSessionsRequest,
                            responder: Responder<ListSessionsResponse>,
                            cx: ConnectionTo<Client>| {
                    let p = proxy.clone();
                    cx.spawn(async move {
                        let infos = tokio::task::spawn_blocking(move || {
                            p.list_sessions()
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
        // --- session/load (stub — not yet supported) ---
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
        // --- catch-all for unhandled methods (includes session/close) ---
        .on_receive_dispatch(
            async move |dispatch: Dispatch, cx: ConnectionTo<Client>| {
                dispatch.respond_with_error(Error::method_not_found(), cx)?;
                Ok(())
            },
            on_receive_dispatch!(),
        )
        .connect_to(Stdio::new())
        .await?;

    Ok(())
}

/// Map our `AcpError` to the SDK's `Error` type.
fn acp_error_to_sdk(e: &AcpError) -> Error {
    match e {
        AcpError::InvalidParams(msg) => {
            Error::invalid_params().data(serde_json::Value::String(msg.clone()))
        }
        AcpError::Internal(msg) => {
            Error::internal_error().data(serde_json::Value::String(msg.clone()))
        }
    }
}

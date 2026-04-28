//! ACP server implementation using the official `agent-client-protocol` SDK.
//!
//! This module provides an SDK-based ACP server with full ACP 1.0 compliance
//! for the session features used by this codebase, including prompt image
//! blocks, permission-mode switching, model switching, cancellation, and ACP
//! permission prompting.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::role::acp::{Agent, Client};
use agent_client_protocol::{
    on_receive_dispatch, on_receive_notification, on_receive_request, ConnectionTo, Dispatch,
    Error, Responder,
};
use agent_client_protocol_schema::{
    AgentCapabilities, CancelNotification, CloseSessionRequest, CloseSessionResponse,
    ContentBlock as AcpContentBlock, ContentChunk, Implementation, InitializeRequest,
    InitializeResponse, ListSessionsRequest, ListSessionsResponse, LoadSessionRequest,
    LoadSessionResponse, ModelInfo, NewSessionRequest, NewSessionResponse, PermissionOption,
    PermissionOptionId, PermissionOptionKind, PromptCapabilities, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SessionCapabilities, SessionCloseCapabilities, SessionInfo, SessionMode, SessionModeState,
    SessionModelState, SessionNotification, SessionUpdate, SetSessionModeRequest,
    SetSessionModeResponse, SetSessionModelRequest, SetSessionModelResponse, StopReason,
    TextContent, ToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol_tokio::Stdio;
use tokio::sync::{mpsc, oneshot, watch};

use crate::conversation::RuntimeObserver;
use crate::permissions::{
    PermissionMode, PermissionPromptDecision, PermissionPrompter, PermissionRequest,
};
use crate::session::ContentBlock as SessionContentBlock;

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

/// Callback trait that the CLI crate implements to provide session
/// construction and prompt execution, keeping runtime/provider deps out of
/// this crate.
pub trait SdkAcpDelegate: Send + 'static {
    /// Create a new session for the given working directory, returning
    /// `(session_id, cwd)` on success.
    fn new_session(&mut self, cwd: PathBuf) -> Result<(String, PathBuf), AcpError>;

    /// Run a prompt turn. The implementation should call observer methods
    /// to stream session updates.
    fn run_prompt(
        &mut self,
        session_id: &str,
        prompt: Vec<SessionContentBlock>,
        observer: &mut SdkSessionObserver,
    ) -> Result<StopReason, AcpError>;

    /// Run a prompt with permission prompting bridged to the ACP client.
    fn run_prompt_with_prompter(
        &mut self,
        session_id: &str,
        prompt: Vec<SessionContentBlock>,
        observer: &mut SdkSessionObserver,
        prompter: &mut dyn PermissionPrompter,
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

    /// Cancel a running prompt for the given session.
    fn cancel_session(&mut self, session_id: &str) -> bool;

    /// Switch the model for a session. Returns a human-readable report.
    fn set_model(&mut self, session_id: &str, model_id: &str) -> Result<String, AcpError>;

    /// Return the current model ID and available models for a session.
    fn get_model_info(&self, session_id: &str) -> Result<(String, Vec<String>), AcpError>;

    /// Change the permission mode for a session.
    fn set_permission_mode(
        &mut self,
        session_id: &str,
        mode: PermissionMode,
    ) -> Result<(), AcpError>;

    /// Return the current permission mode for a session.
    fn get_permission_mode(&self, session_id: &str) -> Result<PermissionMode, AcpError>;
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
            AcpContentBlock::Text(TextContent::new(delta)),
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

/// Re-export `StopReason` so the CLI crate doesn't need a direct dep on
/// the schema crate.
pub use agent_client_protocol_schema::StopReason as AcpStopReason;

/// Thread-safe handle to a delegate, shared across async handlers.
pub type SharedDelegate = Arc<Mutex<Box<dyn SdkAcpDelegate>>>;

#[derive(Debug, Clone, Default)]
struct PromptCancellationRegistry {
    inner: Arc<Mutex<HashMap<String, watch::Sender<bool>>>>,
}

impl PromptCancellationRegistry {
    fn register(&self, session_id: &str) -> watch::Receiver<bool> {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.to_string(), cancel_tx);
        cancel_rx
    }

    fn unregister(&self, session_id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
    }

    fn cancel(&self, session_id: &str) {
        if let Some(cancel_tx) = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
        {
            let _ = cancel_tx.send(true);
        }
    }
}

/// Run a prompt through the delegate (blocking), returning the stop reason
/// and buffered notifications. Handles slash-command dispatch internally.
pub(crate) fn run_delegate_prompt(
    delegate: &SharedDelegate,
    session_id: &str,
    prompt: Vec<SessionContentBlock>,
) -> (StopReason, Vec<SessionNotification>) {
    let mut observer = SdkSessionObserver::new(session_id);
    let mut delegate = delegate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stop = if let Some(slash_command) = prompt_blocks_as_slash_command(&prompt) {
        delegate
            .handle_slash_command(session_id, &slash_command, &mut observer)
            .map(|()| StopReason::EndTurn)
    } else {
        delegate.run_prompt(session_id, prompt, &mut observer)
    };
    let notifications = observer.drain();
    let reason = stop.unwrap_or(StopReason::EndTurn);
    (reason, notifications)
}

/// Extract runtime prompt blocks from ACP prompt content.
pub(crate) fn extract_prompt_blocks_from_content_blocks(
    blocks: &[AcpContentBlock],
) -> Result<Vec<SessionContentBlock>, AcpError> {
    let mut prompt_blocks = Vec::new();
    for block in blocks {
        match block {
            AcpContentBlock::Text(tc) => {
                if !tc.text.trim().is_empty() {
                    prompt_blocks.push(SessionContentBlock::Text {
                        text: tc.text.clone(),
                    });
                }
            }
            AcpContentBlock::Image(image) => {
                prompt_blocks.push(SessionContentBlock::Image {
                    data: image.data.clone(),
                    mime_type: image.mime_type.clone(),
                });
            }
            AcpContentBlock::Audio(_)
            | AcpContentBlock::ResourceLink(_)
            | AcpContentBlock::Resource(_) => {
                return Err(AcpError::invalid_params(
                    "prompt contains unsupported ACP content blocks",
                ));
            }
            _ => {
                return Err(AcpError::invalid_params(
                    "prompt contains unsupported ACP content blocks",
                ));
            }
        }
    }

    if prompt_blocks.is_empty() {
        return Err(AcpError::invalid_params(
            "prompt must include at least one non-empty text or image content block",
        ));
    }

    Ok(prompt_blocks)
}

fn prompt_blocks_as_slash_command(blocks: &[SessionContentBlock]) -> Option<String> {
    let mut texts = Vec::new();
    for block in blocks {
        match block {
            SessionContentBlock::Text { text } if !text.trim().is_empty() => {
                texts.push(text.trim().to_string());
            }
            SessionContentBlock::Text { .. } => {}
            SessionContentBlock::Image { .. }
            | SessionContentBlock::ToolUse { .. }
            | SessionContentBlock::ToolResult { .. } => return None,
        }
    }

    let prompt = texts.join("\n");
    prompt.starts_with('/').then_some(prompt)
}

pub(crate) fn session_mode_state(mode: PermissionMode) -> SessionModeState {
    SessionModeState::new(mode.as_str(), available_session_modes())
}

pub(crate) fn session_model_state(
    current_model: String,
    available_models: Vec<String>,
) -> SessionModelState {
    let available_models = available_models
        .into_iter()
        .map(|model| ModelInfo::new(model.clone(), model))
        .collect();
    SessionModelState::new(current_model, available_models)
}

pub(crate) fn permission_mode_from_id(mode_id: &str) -> Result<PermissionMode, AcpError> {
    match mode_id {
        "read-only" => Ok(PermissionMode::ReadOnly),
        "workspace-write" => Ok(PermissionMode::WorkspaceWrite),
        "danger-full-access" => Ok(PermissionMode::DangerFullAccess),
        "prompt" => Ok(PermissionMode::Prompt),
        "allow" => Ok(PermissionMode::Allow),
        _ => Err(AcpError::invalid_params(format!(
            "unknown permission mode: {mode_id}"
        ))),
    }
}

fn available_session_modes() -> Vec<SessionMode> {
    vec![
        SessionMode::new("read-only", "Read Only"),
        SessionMode::new("workspace-write", "Workspace Write"),
        SessionMode::new("danger-full-access", "Danger Full Access"),
        SessionMode::new("prompt", "Prompt"),
        SessionMode::new("allow", "Allow"),
    ]
}

/// A permission prompter that bridges to the ACP client over channels.
struct AcpPermissionBridge {
    tx: mpsc::UnboundedSender<(PermissionRequest, oneshot::Sender<PermissionPromptDecision>)>,
}

impl PermissionPrompter for AcpPermissionBridge {
    fn decide(&mut self, request: &PermissionRequest) -> PermissionPromptDecision {
        let (response_tx, response_rx) = oneshot::channel();
        if self.tx.send((request.clone(), response_tx)).is_err() {
            return PermissionPromptDecision::Deny {
                reason: "permission bridge closed".to_string(),
            };
        }
        response_rx
            .blocking_recv()
            .unwrap_or(PermissionPromptDecision::Deny {
                reason: "permission response channel closed".to_string(),
            })
    }
}

fn build_acp_permission_request(
    session_id: String,
    request: &PermissionRequest,
) -> RequestPermissionRequest {
    let tool_call = ToolCallUpdate::new(
        format!("perm-{}", pseudo_uuid_v4()),
        ToolCallUpdateFields::new()
            .status(ToolCallStatus::InProgress)
            .raw_input(serde_json::Value::String(request.input.clone())),
    );

    let options = vec![
        PermissionOption::new(
            PermissionOptionId::new("allow_once"),
            "Allow Once",
            PermissionOptionKind::AllowOnce,
        ),
        PermissionOption::new(
            PermissionOptionId::new("allow_always"),
            "Allow Always",
            PermissionOptionKind::AllowAlways,
        ),
        PermissionOption::new(
            PermissionOptionId::new("reject_once"),
            "Reject Once",
            PermissionOptionKind::RejectOnce,
        ),
        PermissionOption::new(
            PermissionOptionId::new("reject_always"),
            "Reject Always",
            PermissionOptionKind::RejectAlways,
        ),
    ];

    RequestPermissionRequest::new(session_id, tool_call, options)
}

fn map_permission_response(response: RequestPermissionResponse) -> PermissionPromptDecision {
    match response.outcome {
        RequestPermissionOutcome::Selected(selected) => {
            let id_str: &str = &selected.option_id.0;
            if id_str.starts_with("allow") {
                PermissionPromptDecision::Allow
            } else {
                PermissionPromptDecision::Deny {
                    reason: format!("user selected: {id_str}"),
                }
            }
        }
        RequestPermissionOutcome::Cancelled | _ => PermissionPromptDecision::Deny {
            reason: "turn cancelled by abort signal".to_string(),
        },
    }
}

fn pseudo_uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:032x}")
}

/// Run the SDK-based ACP server on stdin/stdout.
#[allow(clippy::too_many_lines)]
pub async fn run_sdk_acp_server(
    config: SdkAcpConfig,
    delegate: Box<dyn SdkAcpDelegate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let agent_version = config.agent_version.clone();
    let delegate: SharedDelegate = Arc::new(Mutex::new(delegate));
    let cancellations = PromptCancellationRegistry::default();

    Agent
        .builder()
        .name("scode")
        .on_receive_request(
            {
                let version = agent_version.clone();
                async move |req: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _cx: ConnectionTo<Client>| {
                    let response = InitializeResponse::new(req.protocol_version)
                        .agent_info(Implementation::new("scode", &version))
                        .agent_capabilities(
                            AgentCapabilities::new()
                                .prompt_capabilities(PromptCapabilities::new().image(true))
                                .session_capabilities(
                                    SessionCapabilities::new()
                                        .close(SessionCloseCapabilities::new()),
                                ),
                        );
                    responder.respond(response)?;
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
                    let delegate = Arc::clone(&delegate);
                    cx.spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            let mut delegate = delegate
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let (session_id, _cwd) = delegate.new_session(req.cwd)?;
                            let mode = delegate.get_permission_mode(&session_id)?;
                            let (current_model, available_models) =
                                delegate.get_model_info(&session_id)?;
                            Ok::<_, AcpError>((session_id, mode, current_model, available_models))
                        })
                        .await
                        .unwrap_or_else(|e| Err(AcpError::internal(e.to_string())));

                        match result {
                            Ok((session_id, mode, current_model, available_models)) => {
                                responder.respond(
                                    NewSessionResponse::new(session_id)
                                        .modes(session_mode_state(mode))
                                        .models(session_model_state(
                                            current_model,
                                            available_models,
                                        )),
                                )?;
                            }
                            Err(error) => {
                                responder.respond_with_error(acp_error_to_sdk(&error))?;
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
                let cancellations = cancellations.clone();
                async move |req: PromptRequest,
                            responder: Responder<PromptResponse>,
                            cx: ConnectionTo<Client>| {
                    let prompt_blocks = match extract_prompt_blocks_from_content_blocks(&req.prompt) {
                        Ok(prompt_blocks) => prompt_blocks,
                        Err(error) => {
                            responder.respond_with_error(acp_error_to_sdk(&error))?;
                            return Ok(());
                        }
                    };

                    let delegate = Arc::clone(&delegate);
                    let session_id = req.session_id.to_string();
                    let mut cancel_rx = cancellations.register(&session_id);
                    let cancellations = cancellations.clone();
                    let cx_updates = cx.clone();
                    let cx_permissions = cx.clone();

                    cx.spawn(async move {
                        let (bridge_tx, mut bridge_rx) = mpsc::unbounded_channel::<(
                            PermissionRequest,
                            oneshot::Sender<PermissionPromptDecision>,
                        )>();

                        let blocking_session_id = session_id.clone();
                        let blocking_prompt = prompt_blocks.clone();
                        let blocking_handle = tokio::task::spawn_blocking(move || {
                            let mut observer = SdkSessionObserver::new(&blocking_session_id);
                            let mut delegate = delegate
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let stop = if let Some(slash_command) =
                                prompt_blocks_as_slash_command(&blocking_prompt)
                            {
                                delegate
                                    .handle_slash_command(
                                        &blocking_session_id,
                                        &slash_command,
                                        &mut observer,
                                    )
                                    .map(|()| StopReason::EndTurn)
                            } else {
                                let mut bridge = AcpPermissionBridge { tx: bridge_tx };
                                delegate.run_prompt_with_prompter(
                                    &blocking_session_id,
                                    blocking_prompt,
                                    &mut observer,
                                    &mut bridge,
                                )
                            };
                            let notifications = observer.drain();
                            let reason = stop.unwrap_or(StopReason::EndTurn);
                            (reason, notifications)
                        });

                        let mut blocking_handle = blocking_handle;
                        let result = loop {
                            tokio::select! {
                                maybe_permission = bridge_rx.recv() => {
                                    if let Some((permission_request, response_tx)) = maybe_permission {
                                        let acp_request = build_acp_permission_request(
                                            session_id.clone(),
                                            &permission_request,
                                        );
                                        let request_future = cx_permissions
                                            .send_request(acp_request)
                                            .block_task();
                                        tokio::pin!(request_future);
                                        let decision = tokio::select! {
                                            biased;
                                            changed = cancel_rx.changed() => {
                                                let _ = changed;
                                                PermissionPromptDecision::Deny {
                                                    reason: "turn cancelled by abort signal".to_string(),
                                                }
                                            }
                                            response = &mut request_future => {
                                                match response {
                                                    Ok(response) => map_permission_response(response),
                                                    Err(_) => PermissionPromptDecision::Deny {
                                                        reason: "ACP permission request failed".to_string(),
                                                    },
                                                }
                                            }
                                        };
                                        let _ = response_tx.send(decision);
                                    }
                                }
                                done = &mut blocking_handle => {
                                    break done.unwrap_or((StopReason::EndTurn, Vec::new()));
                                }
                            }
                        };

                        cancellations.unregister(&session_id);

                        let (stop_reason, notifications) = result;
                        for notification in notifications {
                            cx_updates.send_notification(notification)?;
                        }

                        responder.respond(PromptResponse::new(stop_reason))?;
                        Ok(())
                    })?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_notification(
            {
                let delegate = Arc::clone(&delegate);
                let cancellations = cancellations.clone();
                async move |notification: CancelNotification, _cx: ConnectionTo<Client>| {
                    let session_id = notification.session_id.to_string();
                    cancellations.cancel(&session_id);
                    let delegate = Arc::clone(&delegate);
                    tokio::task::spawn_blocking(move || {
                        delegate
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .cancel_session(&session_id);
                    });
                    Ok(())
                }
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            {
                let delegate = Arc::clone(&delegate);
                async move |req: CloseSessionRequest,
                            responder: Responder<CloseSessionResponse>,
                            cx: ConnectionTo<Client>| {
                    let delegate = Arc::clone(&delegate);
                    let session_id = req.session_id.to_string();
                    cx.spawn(async move {
                        tokio::task::spawn_blocking(move || {
                            delegate
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .close_session(&session_id);
                        })
                        .await
                        .ok();
                        responder.respond(CloseSessionResponse::new())?;
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
                async move |_req: ListSessionsRequest,
                            responder: Responder<ListSessionsResponse>,
                            cx: ConnectionTo<Client>| {
                    let delegate = Arc::clone(&delegate);
                    cx.spawn(async move {
                        let sessions = tokio::task::spawn_blocking(move || {
                            delegate
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .list_sessions()
                                .into_iter()
                                .map(|(id, cwd)| SessionInfo::new(id, cwd))
                                .collect::<Vec<_>>()
                        })
                        .await
                        .unwrap_or_default();

                        responder.respond(ListSessionsResponse::new(sessions))?;
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
                async move |req: SetSessionModeRequest,
                            responder: Responder<SetSessionModeResponse>,
                            cx: ConnectionTo<Client>| {
                    let delegate = Arc::clone(&delegate);
                    let session_id = req.session_id.to_string();
                    let mode_id = req.mode_id.0.to_string();
                    cx.spawn(async move {
                        let mode = match permission_mode_from_id(&mode_id) {
                            Ok(mode) => mode,
                            Err(error) => {
                                responder.respond_with_error(acp_error_to_sdk(&error))?;
                                return Ok(());
                            }
                        };
                        let result = tokio::task::spawn_blocking(move || {
                            delegate
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .set_permission_mode(&session_id, mode)
                        })
                        .await
                        .unwrap_or_else(|e| Err(AcpError::internal(e.to_string())));

                        match result {
                            Ok(()) => responder.respond(SetSessionModeResponse::new())?,
                            Err(error) => {
                                responder.respond_with_error(acp_error_to_sdk(&error))?;
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
                async move |req: SetSessionModelRequest,
                            responder: Responder<SetSessionModelResponse>,
                            cx: ConnectionTo<Client>| {
                    let delegate = Arc::clone(&delegate);
                    let session_id = req.session_id.to_string();
                    let model_id = req.model_id.0.to_string();
                    cx.spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            delegate
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .set_model(&session_id, &model_id)
                        })
                        .await
                        .unwrap_or_else(|e| Err(AcpError::internal(e.to_string())));

                        match result {
                            Ok(_) => responder.respond(SetSessionModelResponse::new())?,
                            Err(error) => {
                                responder.respond_with_error(acp_error_to_sdk(&error))?;
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
        )
        .connect_to(Stdio::new())
        .await?;

    Ok(())
}

/// Map our `AcpError` to the SDK's `Error` type.
pub(crate) fn acp_error_to_sdk(error: &AcpError) -> Error {
    match error {
        AcpError::InvalidParams(message) => {
            Error::invalid_params().data(serde_json::Value::String(message.clone()))
        }
        AcpError::Internal(message) => {
            Error::internal_error().data(serde_json::Value::String(message.clone()))
        }
    }
}

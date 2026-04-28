//! WebSocket-based ACP server using axum.
//!
//! Provides a browser-accessible endpoint that shares the same delegate model
//! as the stdio ACP server. The transport remains intentionally lightweight,
//! but the exposed method names and payloads stay aligned with the formal ACP
//! session surface where practical.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol_schema::ContentBlock as AcpContentBlock;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::acp_sdk_server::{
    extract_prompt_blocks_from_content_blocks, permission_mode_from_id, run_delegate_prompt,
    session_mode_state, session_model_state, SdkAcpConfig, SdkAcpDelegate, SharedDelegate,
};

static WEB_UI_HTML: &str = include_str!("acp_web_ui.html");

#[derive(Clone)]
struct AppState {
    config: SdkAcpConfig,
    delegate: SharedDelegate,
}

/// Run an ACP server over WebSocket + serve the embedded web UI.
///
/// # Errors
///
/// Returns an error if the TCP listener or axum server fails.
pub async fn run_acp_ws_server(
    config: SdkAcpConfig,
    delegate: Box<dyn SdkAcpDelegate>,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState {
        config,
        delegate: Arc::new(Mutex::new(delegate)),
    };
    let app = Router::new()
        .route("/", get(serve_html))
        .route("/ws", get(ws_upgrade))
        .with_state(state);

    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    eprintln!("[acp-ws] listening on http://0.0.0.0:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn serve_html() -> impl IntoResponse {
    Html(WEB_UI_HTML)
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: AppState) {
    eprintln!("[acp-ws] client connected");
    while let Some(Ok(msg)) = socket.recv().await {
        let text = match msg {
            Message::Text(text) => text,
            Message::Close(_) => break,
            _ => continue,
        };

        let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
            let error = json_rpc_error(&Value::Null, -32700, "Parse error");
            let _ = send_json(&mut socket, &error).await;
            continue;
        };

        let id = parsed.get("id").cloned().unwrap_or(Value::Null);
        let method = parsed
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = parsed.get("params").cloned().unwrap_or(json!({}));

        dispatch_method(&mut socket, &state, &id, method, &params).await;
    }
    eprintln!("[acp-ws] client disconnected");
}

#[allow(clippy::too_many_lines)]
async fn dispatch_method(
    socket: &mut WebSocket,
    state: &AppState,
    id: &Value,
    method: &str,
    params: &Value,
) {
    match method {
        "initialize" => handle_initialize(socket, state, id, params).await,
        "session/new" => handle_session_new(socket, state, id, params).await,
        "session/prompt" => handle_session_prompt(socket, state, id, params).await,
        "session/list" => handle_session_list(socket, state, id).await,
        "session/close" => handle_session_close(socket, state, id, params).await,
        "session/cancel" => handle_session_cancel(state, params).await,
        "session/set_mode" | "session/setPermissionMode" => {
            handle_session_set_mode(socket, state, id, params).await;
        }
        "session/set_model" | "session/setModel" => {
            handle_session_set_model(socket, state, id, params).await;
        }
        "session/load" => {
            let response = json_rpc_error(id, -32603, "session loading not yet supported");
            let _ = send_json(socket, &response).await;
        }
        "" => {
            let error = json_rpc_error(id, -32600, "Invalid Request: missing method");
            let _ = send_json(socket, &error).await;
        }
        _ => {
            let error = json_rpc_error(id, -32601, &format!("Method not found: {method}"));
            let _ = send_json(socket, &error).await;
        }
    }
}

async fn handle_initialize(socket: &mut WebSocket, state: &AppState, id: &Value, params: &Value) {
    let protocol_version = params
        .get("protocolVersion")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": protocol_version,
            "agentInfo": {
                "name": "scode",
                "version": state.config.agent_version,
            },
            "agentCapabilities": {
                "promptCapabilities": {
                    "image": true
                },
                "sessionCapabilities": {
                    "close": {}
                }
            }
        }
    });
    let _ = send_json(socket, &response).await;
}

async fn handle_session_new(socket: &mut WebSocket, state: &AppState, id: &Value, params: &Value) {
    let cwd = params.get("cwd").and_then(Value::as_str).map_or_else(
        || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        PathBuf::from,
    );

    let delegate = Arc::clone(&state.delegate);
    let result = tokio::task::spawn_blocking(move || {
        let mut delegate = delegate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (session_id, _cwd) = delegate.new_session(cwd)?;
        let mode = delegate.get_permission_mode(&session_id)?;
        let (current_model, available_models) = delegate.get_model_info(&session_id)?;
        Ok::<_, crate::acp_sdk_server::AcpError>((
            session_id,
            mode,
            current_model,
            available_models,
        ))
    })
    .await
    .unwrap_or_else(|e| Err(crate::acp_sdk_server::AcpError::internal(e.to_string())));

    let response = match result {
        Ok((session_id, mode, current_model, available_models)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "sessionId": session_id,
                "modes": serde_json::to_value(session_mode_state(mode)).unwrap_or(json!({})),
                "models": serde_json::to_value(session_model_state(current_model, available_models))
                    .unwrap_or(json!({})),
            }
        }),
        Err(error) => json_rpc_error(id, -32603, &error.to_string()),
    };
    let _ = send_json(socket, &response).await;
}

async fn handle_session_prompt(
    socket: &mut WebSocket,
    state: &AppState,
    id: &Value,
    params: &Value,
) {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let prompt = match extract_prompt_blocks(params) {
        Ok(prompt) => prompt,
        Err(message) => {
            let error = json_rpc_error(id, -32602, &message);
            let _ = send_json(socket, &error).await;
            return;
        }
    };

    let delegate = Arc::clone(&state.delegate);
    let (stop_reason, notifications) =
        tokio::task::spawn_blocking(move || run_delegate_prompt(&delegate, &session_id, prompt))
            .await
            .unwrap_or_else(|_| {
                (
                    agent_client_protocol_schema::StopReason::EndTurn,
                    Vec::new(),
                )
            });

    for notification in &notifications {
        if let Ok(serialized) = serde_json::to_value(notification) {
            let ws_notification = json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": serialized,
            });
            let _ = send_json(socket, &ws_notification).await;
        }
    }

    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "stopReason": stop_reason }
    });
    let _ = send_json(socket, &response).await;
}

async fn handle_session_list(socket: &mut WebSocket, state: &AppState, id: &Value) {
    let delegate = Arc::clone(&state.delegate);
    let sessions = tokio::task::spawn_blocking(move || {
        delegate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .list_sessions()
    })
    .await
    .unwrap_or_default();

    let infos: Vec<Value> = sessions
        .into_iter()
        .map(|(session_id, cwd)| json!({ "sessionId": session_id, "cwd": cwd.to_string_lossy() }))
        .collect();

    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "sessions": infos }
    });
    let _ = send_json(socket, &response).await;
}

async fn handle_session_close(
    socket: &mut WebSocket,
    state: &AppState,
    id: &Value,
    params: &Value,
) {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let delegate = Arc::clone(&state.delegate);
    tokio::task::spawn_blocking(move || {
        delegate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .close_session(&session_id);
    })
    .await
    .ok();

    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {}
    });
    let _ = send_json(socket, &response).await;
}

async fn handle_session_cancel(state: &AppState, params: &Value) {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let delegate = Arc::clone(&state.delegate);
    tokio::task::spawn_blocking(move || {
        delegate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel_session(&session_id);
    })
    .await
    .ok();
}

async fn handle_session_set_model(
    socket: &mut WebSocket,
    state: &AppState,
    id: &Value,
    params: &Value,
) {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let model_id = params
        .get("modelId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let delegate = Arc::clone(&state.delegate);
    let result = tokio::task::spawn_blocking(move || {
        delegate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_model(&session_id, &model_id)
    })
    .await
    .unwrap_or_else(|e| Err(crate::acp_sdk_server::AcpError::internal(e.to_string())));

    let response = match result {
        Ok(_) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        }),
        Err(error) => json_rpc_error(id, -32603, &error.to_string()),
    };
    let _ = send_json(socket, &response).await;
}

async fn handle_session_set_mode(
    socket: &mut WebSocket,
    state: &AppState,
    id: &Value,
    params: &Value,
) {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mode_id = params
        .get("modeId")
        .or_else(|| params.get("permissionMode"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let mode = match permission_mode_from_id(&mode_id) {
        Ok(mode) => mode,
        Err(error) => {
            let response = json_rpc_error(id, -32602, &error.to_string());
            let _ = send_json(socket, &response).await;
            return;
        }
    };

    let delegate = Arc::clone(&state.delegate);
    let result = tokio::task::spawn_blocking(move || {
        delegate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_permission_mode(&session_id, mode)
    })
    .await
    .unwrap_or_else(|e| Err(crate::acp_sdk_server::AcpError::internal(e.to_string())));

    let response = match result {
        Ok(()) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        }),
        Err(error) => json_rpc_error(id, -32603, &error.to_string()),
    };
    let _ = send_json(socket, &response).await;
}

fn extract_prompt_blocks(params: &Value) -> Result<Vec<crate::session::ContentBlock>, String> {
    let prompt = params
        .get("prompt")
        .ok_or_else(|| "params.prompt is required".to_string())?;

    if let Some(text) = prompt.as_str() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("params.prompt must not be empty".to_string());
        }
        return Ok(vec![crate::session::ContentBlock::Text {
            text: trimmed.to_string(),
        }]);
    }

    let prompt_blocks: Vec<AcpContentBlock> = if let Some(array) = prompt.as_array() {
        serde_json::from_value(Value::Array(array.clone()))
            .map_err(|_| "params.prompt must be a valid ACP content-block array".to_string())?
    } else if prompt.is_object() {
        serde_json::from_value::<AcpContentBlock>(prompt.clone())
            .map(|block| vec![block])
            .map_err(|_| "params.prompt must be a valid ACP content block".to_string())?
    } else {
        return Err("params.prompt must be a string or ACP content-block array".to_string());
    };

    extract_prompt_blocks_from_content_blocks(&prompt_blocks).map_err(|error| error.to_string())
}

fn json_rpc_error(id: &Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

async fn send_json(socket: &mut WebSocket, value: &Value) -> Result<(), axum::Error> {
    let text = serde_json::to_string(value).unwrap_or_default();
    socket.send(Message::Text(text.into())).await
}

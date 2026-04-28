//! WebSocket-based ACP server using axum.
//!
//! Provides a browser-accessible endpoint that shares the same
//! `SharedDelegate` pattern used by the stdio server.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::acp_sdk_server::{run_delegate_prompt, SdkAcpConfig, SdkAcpDelegate, SharedDelegate};

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
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };

        let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
            let err = json_rpc_error(&Value::Null, -32700, "Parse error");
            let _ = send_json(&mut socket, &err).await;
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
        "session/load" => {
            let resp = json_rpc_error(id, -32603, "session loading not yet supported");
            let _ = send_json(socket, &resp).await;
        }
        "" => {
            let err = json_rpc_error(id, -32600, "Invalid Request: missing method");
            let _ = send_json(socket, &err).await;
        }
        _ => {
            let err = json_rpc_error(id, -32601, &format!("Method not found: {method}"));
            let _ = send_json(socket, &err).await;
        }
    }
}

async fn handle_initialize(socket: &mut WebSocket, state: &AppState, id: &Value, params: &Value) {
    let protocol_version = params
        .get("protocolVersion")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    let resp = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": protocol_version,
            "agentInfo": {
                "name": "scode",
                "version": state.config.agent_version,
            },
            "agentCapabilities": {
                "promptCapabilities": {}
            }
        }
    });
    let _ = send_json(socket, &resp).await;
}

async fn handle_session_new(socket: &mut WebSocket, state: &AppState, id: &Value, params: &Value) {
    let cwd = params.get("cwd").and_then(Value::as_str).map_or_else(
        || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        PathBuf::from,
    );

    let delegate = Arc::clone(&state.delegate);
    let result = tokio::task::spawn_blocking(move || {
        delegate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .new_session(cwd)
    })
    .await
    .unwrap_or_else(|e| Err(crate::acp_sdk_server::AcpError::internal(e.to_string())));

    let resp = match result {
        Ok((session_id, _cwd)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "sessionId": session_id }
        }),
        Err(e) => json_rpc_error(id, -32603, &e.to_string()),
    };
    let _ = send_json(socket, &resp).await;
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

    let Some(prompt_text) = extract_prompt_text(params) else {
        let err = json_rpc_error(
            id,
            -32602,
            "params.prompt must contain at least one non-empty text content block",
        );
        let _ = send_json(socket, &err).await;
        return;
    };

    let delegate = Arc::clone(&state.delegate);
    let (stop_reason, notifications) = tokio::task::spawn_blocking(move || {
        run_delegate_prompt(&delegate, &session_id, prompt_text)
    })
    .await
    .unwrap_or_else(|_| {
        (
            agent_client_protocol_schema::StopReason::EndTurn,
            Vec::new(),
        )
    });

    // Send each notification as a separate WS message.
    for notif in &notifications {
        if let Ok(serialized) = serde_json::to_value(notif) {
            let ws_notif = json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": serialized
            });
            let _ = send_json(socket, &ws_notif).await;
        }
    }

    // Send the final response.
    let resp = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "stopReason": stop_reason }
    });
    let _ = send_json(socket, &resp).await;
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
        .map(|(id, cwd)| json!({ "sessionId": id, "cwd": cwd.to_string_lossy() }))
        .collect();

    let resp = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "sessions": infos }
    });
    let _ = send_json(socket, &resp).await;
}

/// Extract prompt text from JSON-RPC params.
///
/// Supports both:
/// - `{"prompt": [{"type": "text", "text": "hello"}]}` (ACP content blocks)
/// - `{"prompt": "hello"}` (plain string shorthand)
fn extract_prompt_text(params: &Value) -> Option<String> {
    let prompt = params.get("prompt")?;
    if let Some(s) = prompt.as_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        return Some(trimmed.to_owned());
    }
    if let Some(arr) = prompt.as_array() {
        let texts: Vec<String> = arr
            .iter()
            .filter_map(|block| {
                if block.get("type")?.as_str()? == "text" {
                    let t = block.get("text")?.as_str()?.trim();
                    if t.is_empty() {
                        None
                    } else {
                        Some(t.to_owned())
                    }
                } else {
                    None
                }
            })
            .collect();
        if texts.is_empty() {
            return None;
        }
        return Some(texts.join("\n"));
    }
    None
}

fn json_rpc_error(id: &Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

async fn send_json(socket: &mut WebSocket, value: &Value) -> Result<(), axum::Error> {
    let text = serde_json::to_string(value).unwrap_or_default();
    socket.send(Message::Text(text.into())).await
}

//! Basic ACP (Agent Communication Protocol) server.
//!
//! Implements a JSON-RPC server over stdin/stdout that will handle ACP
//! messages from editors like Zed. This is the Phase 3 foundation: the
//! server reads LSP-framed JSON-RPC requests from stdin, dispatches them,
//! and writes responses to stdout.
//!
//! The framing reuses the same `Content-Length` header convention used by
//! the MCP server in [`crate::mcp_server`], so existing MCP test
//! infrastructure can exercise this server as well.

use std::io;

use serde_json::{json, Value as JsonValue};
use tokio::io::{
    stdin, stdout, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Stdin, Stdout,
};

use crate::mcp_stdio::{JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse};

/// Protocol version the ACP server advertises during `initialize`.
pub const ACP_PROTOCOL_VERSION: &str = "2025-03-26";

/// Configuration for an [`AcpServer`] instance.
pub struct AcpServerSpec {
    /// Name advertised in the `serverInfo` field of the `initialize` response.
    pub server_name: String,
    /// Version advertised in the `serverInfo` field of the `initialize` response.
    pub server_version: String,
}

/// Basic ACP stdio server.
///
/// The server runs a read/dispatch/write loop over the current process's
/// stdin/stdout, terminating cleanly when the peer closes the stream.
/// In this initial phase the server only handles `initialize` and responds
/// to unknown methods with a `method not found` error.
pub struct AcpServer {
    spec: AcpServerSpec,
    stdin: BufReader<Stdin>,
    stdout: Stdout,
}

impl AcpServer {
    #[must_use]
    pub fn new(spec: AcpServerSpec) -> Self {
        Self {
            spec,
            stdin: BufReader::new(stdin()),
            stdout: stdout(),
        }
    }

    /// Runs the server until the client closes stdin.
    ///
    /// Returns `Ok(())` on clean EOF; any other I/O error is propagated so
    /// callers can log and exit non-zero.
    pub async fn run(&mut self) -> io::Result<()> {
        loop {
            let Some(payload) = read_frame(&mut self.stdin).await? else {
                return Ok(());
            };

            let message: JsonValue = match serde_json::from_slice(&payload) {
                Ok(value) => value,
                Err(error) => {
                    let response = JsonRpcResponse::<JsonValue> {
                        jsonrpc: "2.0".to_string(),
                        id: JsonRpcId::Null,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32700,
                            message: format!("parse error: {error}"),
                            data: None,
                        }),
                    };
                    write_response(&mut self.stdout, &response).await?;
                    continue;
                }
            };

            if message.get("id").is_none() {
                // Notification: no reply required.
                continue;
            }

            let request: JsonRpcRequest<JsonValue> = match serde_json::from_value(message) {
                Ok(request) => request,
                Err(error) => {
                    let response = JsonRpcResponse::<JsonValue> {
                        jsonrpc: "2.0".to_string(),
                        id: JsonRpcId::Null,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32600,
                            message: format!("invalid request: {error}"),
                            data: None,
                        }),
                    };
                    write_response(&mut self.stdout, &response).await?;
                    continue;
                }
            };

            let response = self.dispatch(request);
            write_response(&mut self.stdout, &response).await?;
        }
    }

    fn dispatch(&self, request: JsonRpcRequest<JsonValue>) -> JsonRpcResponse<JsonValue> {
        let method = request.method;
        let id = request.id;
        match method.as_str() {
            "initialize" => self.handle_initialize(id),
            other => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("method not found: {other}"),
                    data: None,
                }),
            },
        }
    }

    fn handle_initialize(&self, id: JsonRpcId) -> JsonRpcResponse<JsonValue> {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(json!({
                "protocolVersion": ACP_PROTOCOL_VERSION,
                "capabilities": {},
                "serverInfo": {
                    "name": self.spec.server_name,
                    "version": self.spec.server_version,
                }
            })),
            error: None,
        }
    }
}

/// Reads a single LSP-framed JSON-RPC payload from `reader`.
///
/// Returns `Ok(None)` on clean EOF before any header bytes have been read.
async fn read_frame(reader: &mut BufReader<Stdin>) -> io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    let mut first_header = true;
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            if first_header {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "ACP stdio stream closed while reading headers",
            ));
        }
        first_header = false;
        if line == "\r\n" || line == "\n" {
            break;
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                let parsed = value
                    .trim()
                    .parse::<usize>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                content_length = Some(parsed);
            }
        }
    }

    let content_length = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;
    let mut payload = vec![0_u8; content_length];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

async fn write_response(
    stdout: &mut Stdout,
    response: &JsonRpcResponse<JsonValue>,
) -> io::Result<()> {
    let body = serde_json::to_vec(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdout.write_all(header.as_bytes()).await?;
    stdout.write_all(&body).await?;
    stdout.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_initialize_returns_server_info() {
        let server = AcpServer {
            spec: AcpServerSpec {
                server_name: "test-acp".to_string(),
                server_version: "0.1.0".to_string(),
            },
            stdin: BufReader::new(stdin()),
            stdout: stdout(),
        };
        let request = JsonRpcRequest::<JsonValue> {
            jsonrpc: "2.0".to_string(),
            id: JsonRpcId::Number(1),
            method: "initialize".to_string(),
            params: None,
        };
        let response = server.dispatch(request);
        assert_eq!(response.id, JsonRpcId::Number(1));
        assert!(response.error.is_none());
        let result = response.result.expect("initialize result");
        assert_eq!(result["protocolVersion"], ACP_PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], "test-acp");
        assert_eq!(result["serverInfo"]["version"], "0.1.0");
    }

    #[test]
    fn dispatch_unknown_method_returns_method_not_found() {
        let server = AcpServer {
            spec: AcpServerSpec {
                server_name: "test-acp".to_string(),
                server_version: "0.1.0".to_string(),
            },
            stdin: BufReader::new(stdin()),
            stdout: stdout(),
        };
        let request = JsonRpcRequest::<JsonValue> {
            jsonrpc: "2.0".to_string(),
            id: JsonRpcId::Number(2),
            method: "nonexistent".to_string(),
            params: None,
        };
        let response = server.dispatch(request);
        let error = response.error.expect("error payload");
        assert_eq!(error.code, -32601);
    }
}

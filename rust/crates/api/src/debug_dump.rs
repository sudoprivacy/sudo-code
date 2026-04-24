//! Optional debug dump of outgoing API request bodies.
//!
//! When the `SCODE_DEBUG_API_REQUEST` environment variable is set to a file
//! path, each provider writes two files **before** sending the HTTP request:
//!
//! 1. `<path>` — the raw JSON body (machine-readable).
//! 2. `<path>.txt` — a human-readable rendering of the full prompt including
//!    the system prompt, every message, and a tool summary.
//!
//! The dump is best-effort: failures to write are silently ignored so they
//! never disrupt normal operation.

use std::fs::{self, OpenOptions};
use std::io::Write;

use serde_json::Value;

/// Environment variable that activates request body dumping.
pub const DEBUG_API_REQUEST_ENV: &str = "SCODE_DEBUG_API_REQUEST";

/// If `SCODE_DEBUG_API_REQUEST` is set to a file path, write the JSON body and
/// a human-readable `.txt` companion. Does nothing when the variable is unset
/// or empty.
pub fn maybe_dump_request_body(body: &Value) {
    let Ok(path) = std::env::var(DEBUG_API_REQUEST_ENV) else {
        return;
    };
    if path.is_empty() {
        return;
    }

    // Ensure parent directories exist.
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = fs::create_dir_all(parent);
    }

    // 1. JSON dump (append).
    if let Ok(json) = serde_json::to_string_pretty(body) {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "--- request at {} ---", timestamp());
            let _ = writeln!(file, "{json}");
            let _ = file.flush();
        }
    }

    // 2. Human-readable dump (overwrite — always shows latest request).
    let txt_path = format!("{path}.txt");
    let rendered = render_readable(body);
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&txt_path)
    {
        let _ = write!(file, "{rendered}");
        let _ = file.flush();
    }
}

/// Render the full API request body as human-readable text.
fn render_readable(body: &Value) -> String {
    let mut out = String::new();

    // Header
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let max_tokens = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    out.push_str(&format!(
        "═══ Full LLM Request ═══  (model: {model}, max_tokens: {max_tokens}, stream: {stream})\n\n"
    ));

    // System prompt
    if let Some(system) = body.get("system").and_then(Value::as_str) {
        out.push_str("━━━ SYSTEM PROMPT ━━━\n\n");
        out.push_str(system);
        out.push_str("\n\n");
    }

    // Messages
    render_messages(&mut out, body);

    // Tools
    render_tools(&mut out, body);

    out
}

fn render_messages(out: &mut String, body: &Value) {
    let messages = match body.get("messages").and_then(Value::as_array) {
        Some(m) => m,
        None => return,
    };

    for (i, msg) in messages.iter().enumerate() {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("unknown");
        out.push_str(&format!(
            "━━━ MESSAGE {} ({}) ━━━\n\n",
            i + 1,
            role.to_uppercase()
        ));

        // Anthropic format: content is an array of blocks.
        if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
            for block in blocks {
                render_content_block(out, block);
            }
        }
        // OpenAI format: content may be a plain string.
        else if let Some(text) = msg.get("content").and_then(Value::as_str) {
            out.push_str(text);
            out.push_str("\n\n");
        }

        // OpenAI tool_calls on assistant messages.
        if let Some(tool_calls) = msg.get("tool_calls").and_then(Value::as_array) {
            for tc in tool_calls {
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                let args = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                out.push_str(&format!("[tool_call] {name}({args})\n"));
            }
            out.push('\n');
        }
    }
}

fn render_content_block(out: &mut String, block: &Value) {
    let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
    match block_type {
        "text" => {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                out.push_str(text);
                out.push_str("\n\n");
            }
        }
        "tool_use" => {
            let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
            let input = block.get("input").unwrap_or(&Value::Null);
            let input_str = serde_json::to_string_pretty(input).unwrap_or_default();
            out.push_str(&format!("[tool_use: {name}]\n{input_str}\n\n"));
        }
        "tool_result" => {
            let tool_id = block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let is_error = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let status = if is_error { "ERROR" } else { "ok" };
            out.push_str(&format!("[tool_result: {tool_id} ({status})]\n"));
            if let Some(content) = block.get("content").and_then(Value::as_array) {
                for item in content {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        out.push_str(text);
                        out.push('\n');
                    }
                }
            } else if let Some(content) = block.get("content").and_then(Value::as_str) {
                out.push_str(content);
                out.push('\n');
            }
            out.push('\n');
        }
        "thinking" => {
            let thinking = block.get("thinking").and_then(Value::as_str).unwrap_or("");
            if !thinking.is_empty() {
                out.push_str(&format!("[thinking]\n{thinking}\n\n"));
            }
        }
        _ => {
            // Unknown block type — dump as compact JSON.
            if let Ok(s) = serde_json::to_string(block) {
                out.push_str(&format!("[{block_type}] {s}\n\n"));
            }
        }
    }
}

fn render_tools(out: &mut String, body: &Value) {
    let tools = match body.get("tools").and_then(Value::as_array) {
        Some(t) if !t.is_empty() => t,
        _ => return,
    };

    out.push_str(&format!("━━━ TOOLS ({} defined) ━━━\n\n", tools.len()));

    for tool in tools {
        // Anthropic format
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            // OpenAI format: tools[].function.name
            .or_else(|| {
                tool.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("?");
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| {
                tool.get("function")
                    .and_then(|f| f.get("description"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("");

        // Truncate long descriptions to first line.
        let short_desc = description
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(100)
            .collect::<String>();

        out.push_str(&format!("  - {name}: {short_desc}\n"));
    }
    out.push('\n');
}

fn timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(
            |_| "unknown".to_string(),
            |d| format!("{}ms", d.as_millis()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maybe_dump_writes_pretty_json_and_readable_txt() {
        let dir = std::env::temp_dir().join(format!(
            "scode-debug-dump-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let json_path = dir.join("dump.json");
        let txt_path = dir.join("dump.json.txt");

        let body = serde_json::json!({
            "model": "claude-opus-4-6",
            "max_tokens": 16384,
            "system": "You are a helpful assistant.\n\nBe concise.",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "What is 2+2?"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "Let me check."},
                    {"type": "tool_use", "id": "t1", "name": "bash", "input": {"command": "echo 4"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "4"}], "is_error": false}
                ]}
            ],
            "tools": [
                {"name": "bash", "description": "Execute a shell command.", "input_schema": {}},
                {"name": "read_file", "description": "Read a text file from the workspace.", "input_schema": {}}
            ],
            "stream": true
        });

        let key = DEBUG_API_REQUEST_ENV;
        let previous = std::env::var_os(key);
        std::env::set_var(key, json_path.to_str().unwrap());

        maybe_dump_request_body(&body);

        match previous {
            Some(val) => std::env::set_var(key, val),
            None => std::env::remove_var(key),
        }

        // JSON file
        let json_contents = std::fs::read_to_string(&json_path).expect("json dump should exist");
        assert!(json_contents.contains("\"model\": \"claude-opus-4-6\""));

        // Readable text file
        let txt_contents = std::fs::read_to_string(&txt_path).expect("txt dump should exist");
        assert!(txt_contents.contains("SYSTEM PROMPT"));
        assert!(txt_contents.contains("You are a helpful assistant."));
        assert!(txt_contents.contains("Be concise."));
        assert!(txt_contents.contains("MESSAGE 1 (USER)"));
        assert!(txt_contents.contains("What is 2+2?"));
        assert!(txt_contents.contains("MESSAGE 2 (ASSISTANT)"));
        assert!(txt_contents.contains("[tool_use: bash]"));
        assert!(txt_contents.contains("MESSAGE 3 (USER)"));
        assert!(txt_contents.contains("[tool_result: t1 (ok)]"));
        assert!(txt_contents.contains("TOOLS (2 defined)"));
        assert!(txt_contents.contains("- bash: Execute a shell command."));
        assert!(txt_contents.contains("- read_file: Read a text file"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn maybe_dump_is_noop_when_env_is_unset() {
        let key = DEBUG_API_REQUEST_ENV;
        let previous = std::env::var_os(key);
        std::env::remove_var(key);

        maybe_dump_request_body(&serde_json::json!({"test": true}));

        if let Some(val) = previous {
            std::env::set_var(key, val);
        }
    }
}

//! Optional debug dump of outgoing API request bodies.
//!
//! When the `SCODE_DEBUG_API_REQUEST` environment variable is set to a file
//! path, each provider writes the fully-assembled JSON body to that file
//! **before** sending the HTTP request. This is useful for inspecting the
//! exact prompt, tool definitions, and messages that the CLI sends to the
//! upstream model API.
//!
//! The dump is best-effort: failures to write are silently ignored so they
//! never disrupt normal operation.

use std::fs::{self, OpenOptions};
use std::io::Write;

use serde_json::Value;

/// Environment variable that activates request body dumping.
pub const DEBUG_API_REQUEST_ENV: &str = "SCODE_DEBUG_API_REQUEST";

/// If `SCODE_DEBUG_API_REQUEST` is set to a file path, pretty-print `body` as
/// JSON and **append** it to that file (separated by a newline boundary). Does
/// nothing when the variable is unset or empty.
pub fn maybe_dump_request_body(body: &Value) {
    let Ok(path) = std::env::var(DEBUG_API_REQUEST_ENV) else {
        return;
    };
    if path.is_empty() {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(body) else {
        return;
    };

    // Ensure parent directories exist.
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = fs::create_dir_all(parent);
    }

    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(file, "--- request at {} ---", timestamp());
    let _ = writeln!(file, "{json}");
    let _ = file.flush();
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
    fn maybe_dump_writes_pretty_json_when_env_is_set() {
        let dir = std::env::temp_dir().join(format!(
            "scode-debug-dump-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("dump.json");

        // Serialize a small representative body.
        let body = serde_json::json!({
            "model": "claude-opus-4-6",
            "max_tokens": 16384,
            "system": "You are a helpful assistant.",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hello"}]}
            ],
            "tools": [
                {"name": "Read", "description": "Read a file", "input_schema": {}}
            ],
            "stream": true
        });

        // Guard: set the env var for the duration of this test only.
        let key = DEBUG_API_REQUEST_ENV;
        let previous = std::env::var_os(key);
        std::env::set_var(key, path.to_str().unwrap());

        maybe_dump_request_body(&body);

        // Restore original env state.
        match previous {
            Some(val) => std::env::set_var(key, val),
            None => std::env::remove_var(key),
        }

        let contents = std::fs::read_to_string(&path).expect("dump file should exist");
        assert!(contents.contains("\"model\": \"claude-opus-4-6\""));
        assert!(contents.contains("\"system\": \"You are a helpful assistant.\""));
        assert!(contents.contains("--- request at"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn maybe_dump_is_noop_when_env_is_unset() {
        let key = DEBUG_API_REQUEST_ENV;
        let previous = std::env::var_os(key);
        std::env::remove_var(key);

        // Should not panic or write anything.
        maybe_dump_request_body(&serde_json::json!({"test": true}));

        if let Some(val) = previous {
            std::env::set_var(key, val);
        }
    }
}

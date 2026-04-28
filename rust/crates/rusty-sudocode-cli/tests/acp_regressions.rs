use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mock_anthropic_service::{MockAnthropicService, SCENARIO_PREFIX};
use serde_json::{json, Value};

const ACP_TIMEOUT: Duration = Duration::from_secs(5);

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn initialize_advertises_image_prompt_capability() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let workspace = HarnessWorkspace::new(unique_temp_dir("acp-init-image"));
    workspace.create().expect("workspace should exist");
    workspace.write_sudocode_json(&server.base_url());

    let mut harness = AcpHarness::spawn(&workspace, "danger-full-access", None);

    let initialize_id = harness.request("initialize", json!({ "protocolVersion": 1 }));
    let response = harness.wait_for_response(initialize_id, ACP_TIMEOUT);
    let result = expect_result(&response, "initialize");

    assert_eq!(result["protocolVersion"], 1);
    assert_eq!(
        result["agentCapabilities"]["promptCapabilities"]["image"],
        Value::Bool(true)
    );
}

#[test]
fn session_set_model_updates_subsequent_model_report() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let workspace = HarnessWorkspace::new(unique_temp_dir("acp-set-model"));
    workspace.create().expect("workspace should exist");
    workspace.write_sudocode_json(&server.base_url());

    let mut harness = AcpHarness::spawn(&workspace, "danger-full-access", None);
    initialize(&mut harness);
    let session_id = new_session(&mut harness, &workspace.root);

    let set_model_id = harness.request(
        "session/set_model",
        json!({
            "sessionId": session_id,
            "modelId": "haiku",
        }),
    );
    let set_model_response = harness.wait_for_response(set_model_id, ACP_TIMEOUT);
    expect_result(&set_model_response, "session/set_model");

    let prompt_id = harness.request(
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": "/model" }],
        }),
    );
    let _update = harness
        .wait_for_message_containing("Current model    claude-haiku-4-5-20251213", ACP_TIMEOUT);
    let prompt_response = harness.wait_for_response(prompt_id, ACP_TIMEOUT);
    let result = expect_result(&prompt_response, "session/prompt");

    assert_eq!(result["stopReason"], "end_turn");
}

#[test]
fn session_set_mode_to_prompt_triggers_permission_request_for_bash() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let workspace = HarnessWorkspace::new(unique_temp_dir("acp-set-mode"));
    workspace.create().expect("workspace should exist");
    workspace.write_sudocode_json(&server.base_url());

    let mut harness = AcpHarness::spawn(&workspace, "danger-full-access", Some("bash"));
    initialize(&mut harness);
    let session_id = new_session(&mut harness, &workspace.root);

    let set_mode_id = harness.request(
        "session/set_mode",
        json!({
            "sessionId": session_id,
            "modeId": "prompt",
        }),
    );
    let response = harness.wait_for_response(set_mode_id, ACP_TIMEOUT);
    expect_result(&response, "session/set_mode");

    let prompt_id = harness.request(
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{
                "type": "text",
                "text": format!("{SCENARIO_PREFIX}bash_permission_prompt_denied"),
            }],
        }),
    );
    let permission_request = harness.wait_for_request_before_response(
        prompt_id,
        "session/request_permission",
        ACP_TIMEOUT,
    );
    harness.respond(
        permission_request["id"].clone(),
        json!({
            "outcome": {
                "outcome": "selected",
                "optionId": "reject_once",
            }
        }),
    );
    let prompt_response = harness.wait_for_response(prompt_id, ACP_TIMEOUT);
    let result = expect_result(&prompt_response, "session/prompt");

    assert_eq!(result["stopReason"], "end_turn");
}

#[test]
fn permission_request_flow_allows_tool_after_client_approval() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let workspace = HarnessWorkspace::new(unique_temp_dir("acp-permission-allow"));
    workspace.create().expect("workspace should exist");
    workspace.write_sudocode_json(&server.base_url());

    let mut harness = AcpHarness::spawn(&workspace, "workspace-write", Some("bash"));
    initialize(&mut harness);
    let session_id = new_session(&mut harness, &workspace.root);

    let prompt_id = harness.request(
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{
                "type": "text",
                "text": format!("{SCENARIO_PREFIX}bash_permission_prompt_approved"),
            }],
        }),
    );
    let permission_request = harness.wait_for_request_before_response(
        prompt_id,
        "session/request_permission",
        ACP_TIMEOUT,
    );

    assert_eq!(
        permission_request["params"]["sessionId"],
        Value::String(session_id.clone())
    );
    let options = permission_request["params"]["options"]
        .as_array()
        .expect("permission request options should be an array");
    assert!(
        options
            .iter()
            .any(|option| option["optionId"] == "allow_once"),
        "expected allow_once option in permission request: {permission_request:#}"
    );
    assert!(
        options
            .iter()
            .any(|option| option["optionId"] == "reject_once"),
        "expected reject_once option in permission request: {permission_request:#}"
    );
    assert!(
        permission_request
            .to_string()
            .contains("approved via prompt"),
        "permission request should include the pending bash command: {permission_request:#}"
    );

    harness.respond(
        permission_request["id"].clone(),
        json!({
            "outcome": {
                "outcome": "selected",
                "optionId": "allow_once",
            }
        }),
    );

    let prompt_response = harness.wait_for_response(prompt_id, ACP_TIMEOUT);
    let result = expect_result(&prompt_response, "session/prompt");
    assert_eq!(result["stopReason"], "end_turn");

    let transcript = harness.backlog_text();
    assert!(
        transcript.contains("bash approved and executed: approved via prompt"),
        "expected approved final text in ACP updates.\nTranscript:\n{transcript}"
    );
}

#[test]
fn permission_request_flow_denies_tool_after_client_rejection() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let workspace = HarnessWorkspace::new(unique_temp_dir("acp-permission-deny"));
    workspace.create().expect("workspace should exist");
    workspace.write_sudocode_json(&server.base_url());

    let mut harness = AcpHarness::spawn(&workspace, "workspace-write", Some("bash"));
    initialize(&mut harness);
    let session_id = new_session(&mut harness, &workspace.root);

    let prompt_id = harness.request(
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{
                "type": "text",
                "text": format!("{SCENARIO_PREFIX}bash_permission_prompt_denied"),
            }],
        }),
    );
    let permission_request = harness.wait_for_request_before_response(
        prompt_id,
        "session/request_permission",
        ACP_TIMEOUT,
    );

    harness.respond(
        permission_request["id"].clone(),
        json!({
            "outcome": {
                "outcome": "selected",
                "optionId": "reject_once",
            }
        }),
    );

    let prompt_response = harness.wait_for_response(prompt_id, ACP_TIMEOUT);
    let result = expect_result(&prompt_response, "session/prompt");
    assert_eq!(result["stopReason"], "end_turn");

    let transcript = harness.backlog_text();
    assert!(
        transcript.contains("bash denied as expected:"),
        "expected denied final text in ACP updates.\nTranscript:\n{transcript}"
    );
}

#[test]
fn session_cancel_during_permission_request_returns_cancelled_stop_reason() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let workspace = HarnessWorkspace::new(unique_temp_dir("acp-cancel"));
    workspace.create().expect("workspace should exist");
    workspace.write_sudocode_json(&server.base_url());

    let mut harness = AcpHarness::spawn(&workspace, "workspace-write", Some("bash"));
    initialize(&mut harness);
    let session_id = new_session(&mut harness, &workspace.root);

    let prompt_id = harness.request(
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{
                "type": "text",
                "text": format!("{SCENARIO_PREFIX}bash_permission_prompt_approved"),
            }],
        }),
    );
    let permission_request = harness.wait_for_request_before_response(
        prompt_id,
        "session/request_permission",
        ACP_TIMEOUT,
    );

    harness.notify("session/cancel", json!({ "sessionId": session_id }));
    harness.respond(
        permission_request["id"].clone(),
        json!({
            "outcome": {
                "outcome": "cancelled",
            }
        }),
    );

    let prompt_response = harness.wait_for_response(prompt_id, ACP_TIMEOUT);
    let result = expect_result(&prompt_response, "session/prompt");

    assert_eq!(result["stopReason"], "cancelled");
}

struct HarnessWorkspace {
    root: PathBuf,
    config_home: PathBuf,
    home: PathBuf,
}

impl HarnessWorkspace {
    fn new(root: PathBuf) -> Self {
        Self {
            config_home: root.join("config-home"),
            home: root.join("home"),
            root,
        }
    }

    fn create(&self) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        fs::create_dir_all(&self.config_home)?;
        fs::create_dir_all(&self.home)?;
        Ok(())
    }

    fn write_sudocode_json(&self, base_url: &str) {
        let sample = runtime::SAMPLE_SUDOCODE_JSON
            .replace("https://api.anthropic.com", base_url)
            .replace("<YOUR_ANTHROPIC_API_KEY>", "test-acp-key");
        fs::write(self.config_home.join("sudocode.json"), sample)
            .expect("test sudocode.json should be written");
    }
}

struct AcpHarness {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<Value>,
    backlog: VecDeque<Value>,
    next_id: u64,
    stderr: Arc<Mutex<String>>,
    stdout_reader_error: Arc<Mutex<Option<String>>>,
}

impl AcpHarness {
    fn spawn(
        workspace: &HarnessWorkspace,
        permission_mode: &str,
        allowed_tools: Option<&str>,
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_scode"));
        command
            .current_dir(&workspace.root)
            .env_clear()
            .env("SUDO_CODE_CONFIG_HOME", &workspace.config_home)
            .env("HOME", &workspace.home)
            .env("NO_COLOR", "1")
            .env("PATH", "/usr/bin:/bin")
            .args([
                "--auth",
                "api-key",
                "--model",
                "sonnet",
                "--permission-mode",
                permission_mode,
            ]);

        if let Some(allowed_tools) = allowed_tools {
            command.args(["--allowedTools", allowed_tools]);
        }

        command
            .arg("acp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().expect("scode ACP server should launch");
        let stdin = child.stdin.take().expect("child stdin should be piped");
        let stdout = child.stdout.take().expect("child stdout should be piped");
        let stderr = child.stderr.take().expect("child stderr should be piped");

        let (tx, rx) = mpsc::channel();
        let stderr_buffer = Arc::new(Mutex::new(String::new()));
        let stdout_reader_error = Arc::new(Mutex::new(None));
        spawn_stdout_reader(stdout, tx, Arc::clone(&stdout_reader_error));
        spawn_stderr_collector(stderr, Arc::clone(&stderr_buffer));

        Self {
            child,
            stdin,
            rx,
            backlog: VecDeque::new(),
            next_id: 1,
            stderr: stderr_buffer,
            stdout_reader_error,
        }
    }

    fn request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        id
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    fn respond(&mut self, id: Value, result: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }));
    }

    fn wait_for_response(&mut self, id: u64, timeout: Duration) -> Value {
        self.wait_for_matching(&format!("response id {id}"), timeout, |message| {
            message.get("id").and_then(Value::as_u64) == Some(id)
        })
    }

    fn wait_for_message_containing(&mut self, needle: &str, timeout: Duration) -> Value {
        self.wait_for_matching(
            &format!("message containing {needle:?}"),
            timeout,
            |message| message.to_string().contains(needle),
        )
    }

    fn wait_for_request_before_response(
        &mut self,
        response_id: u64,
        method: &str,
        timeout: Duration,
    ) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(position) = self
                .backlog
                .iter()
                .position(|message| message["method"] == method)
            {
                return self
                    .backlog
                    .remove(position)
                    .expect("matching backlog message should exist");
            }
            if let Some(response) = self
                .backlog
                .iter()
                .find(|message| message.get("id").and_then(Value::as_u64) == Some(response_id))
            {
                panic!(
                    "expected `{method}` before response id {response_id}, got response first:\n{}\n{}",
                    pretty_json(response),
                    self.debug_context()
                );
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!(
                    "timed out waiting for `{method}` before response id {response_id}\n{}",
                    self.debug_context()
                );
            }

            match self.rx.recv_timeout(remaining) {
                Ok(message) => {
                    if message["method"] == method {
                        return message;
                    }
                    if message.get("id").and_then(Value::as_u64) == Some(response_id) {
                        panic!(
                            "expected `{method}` before response id {response_id}, got response first:\n{}\n{}",
                            pretty_json(&message),
                            self.debug_context()
                        );
                    }
                    self.backlog.push_back(message);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!(
                        "timed out waiting for `{method}` before response id {response_id}\n{}",
                        self.debug_context()
                    );
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!(
                        "ACP server stdout closed while waiting for `{method}` before response id {response_id}\n{}",
                        self.debug_context()
                    );
                }
            }
        }
    }

    fn backlog_text(&self) -> String {
        self.backlog
            .iter()
            .filter_map(extract_agent_text_chunk)
            .collect::<Vec<_>>()
            .join("")
    }

    fn send(&mut self, message: Value) {
        let payload = serde_json::to_string(&message).expect("json-rpc message should serialize");
        writeln!(self.stdin, "{payload}").expect("json-rpc line should write");
        self.stdin.flush().expect("json-rpc line should flush");
    }

    fn wait_for_matching<F>(&mut self, label: &str, timeout: Duration, matches: F) -> Value
    where
        F: Fn(&Value) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(position) = self.backlog.iter().position(&matches) {
                return self
                    .backlog
                    .remove(position)
                    .expect("matching backlog message should exist");
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!("timed out waiting for {label}\n{}", self.debug_context());
            }

            match self.rx.recv_timeout(remaining) {
                Ok(message) => {
                    if matches(&message) {
                        return message;
                    }
                    self.backlog.push_back(message);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("timed out waiting for {label}\n{}", self.debug_context());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!(
                        "ACP server stdout closed while waiting for {label}\n{}",
                        self.debug_context()
                    );
                }
            }
        }
    }

    fn debug_context(&self) -> String {
        let backlog = if self.backlog.is_empty() {
            String::from("  <empty>")
        } else {
            self.backlog
                .iter()
                .map(pretty_json)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let stderr = self.stderr.lock().expect("stderr lock").clone();
        let stdout_reader_error = self
            .stdout_reader_error
            .lock()
            .expect("stdout reader error lock")
            .clone()
            .unwrap_or_else(|| String::from("<none>"));
        format!(
            "Backlog:\n{backlog}\n\nstderr:\n{stderr}\n\nstdout reader error:\n{stdout_reader_error}"
        )
    }
}

impl Drop for AcpHarness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn initialize(harness: &mut AcpHarness) {
    let initialize_id = harness.request("initialize", json!({ "protocolVersion": 1 }));
    let response = harness.wait_for_response(initialize_id, ACP_TIMEOUT);
    let result = expect_result(&response, "initialize");
    assert_eq!(result["protocolVersion"], 1);
}

fn new_session(harness: &mut AcpHarness, cwd: &Path) -> String {
    let session_id = harness.request(
        "session/new",
        json!({
            "cwd": cwd,
            "mcpServers": [],
        }),
    );
    let response = harness.wait_for_response(session_id, ACP_TIMEOUT);
    let result = expect_result(&response, "session/new");
    result["sessionId"]
        .as_str()
        .expect("session/new should return sessionId")
        .to_string()
}

fn expect_result<'a>(response: &'a Value, method: &str) -> &'a Value {
    if let Some(error) = response.get("error") {
        panic!(
            "{method} returned ACP error:\n{}\nFull response:\n{}",
            pretty_json(error),
            pretty_json(response)
        );
    }
    response
        .get("result")
        .expect("ACP response should include result when no error is present")
}

fn extract_agent_text_chunk(message: &Value) -> Option<&str> {
    let update = message.get("params")?.get("update")?;
    if update.get("sessionUpdate")?.as_str()? != "agent_message_chunk" {
        return None;
    }
    update.get("content")?.get("text")?.as_str()
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("json should pretty-print")
}

fn spawn_stdout_reader(
    stdout: ChildStdout,
    tx: mpsc::Sender<Value>,
    reader_error: Arc<Mutex<Option<String>>>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_jsonrpc_line(&mut reader) {
                Ok(Some(message)) => {
                    if tx.send(message).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    *reader_error.lock().expect("reader error lock") = Some(error.to_string());
                    break;
                }
            }
        }
    });
}

fn spawn_stderr_collector(mut stderr: ChildStderr, buffer: Arc<Mutex<String>>) {
    thread::spawn(move || {
        let mut output = String::new();
        let _ = stderr.read_to_string(&mut output);
        *buffer.lock().expect("stderr buffer lock") = output;
    });
}

fn read_jsonrpc_line(reader: &mut BufReader<ChildStdout>) -> io::Result<Option<Value>> {
    let mut line = String::new();
    let bytes_read = reader.read_line(&mut line)?;
    if bytes_read == 0 {
        return Ok(None);
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return read_jsonrpc_line(reader);
    }
    serde_json::from_str(trimmed).map(Some).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid JSON-RPC line: {error}"),
        )
    })
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_millis();
    std::env::temp_dir().join(format!("sudocode-{prefix}-{timestamp}-{counter}"))
}

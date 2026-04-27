use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use runtime::Session;
use serde_json::{json, Value};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn help_emits_json_when_requested() {
    let root = unique_temp_dir("help-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let parsed = assert_json_command(&root, &["--output-format", "json", "help"]);
    assert_eq!(parsed["kind"], "help");
    assert!(parsed["message"]
        .as_str()
        .expect("help text")
        .contains("Usage:"));
}

#[test]
fn version_emits_json_when_requested() {
    let root = unique_temp_dir("version-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let parsed = assert_json_command(&root, &["--output-format", "json", "version"]);
    assert_eq!(parsed["kind"], "version");
    assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn status_and_sandbox_emit_json_when_requested() {
    let root = unique_temp_dir("status-sandbox-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let status = assert_json_command(&root, &["--output-format", "json", "status"]);
    assert_eq!(status["kind"], "status");
    assert!(status["workspace"]["cwd"].as_str().is_some());

    let sandbox = assert_json_command(&root, &["--output-format", "json", "sandbox"]);
    assert_eq!(sandbox["kind"], "sandbox");
    assert!(sandbox["filesystem_mode"].as_str().is_some());
}

#[test]
fn acp_server_responds_to_line_delimited_initialize() {
    let root = unique_temp_dir("acp-jsonrpc");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let acp = assert_line_delimited_acp_command(
        &root,
        &["--output-format", "json", "acp"],
        &[],
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": 9 },
        }),
    );
    assert_eq!(acp["id"], 1);
    assert_eq!(acp["result"]["protocolVersion"], 9);
    assert_eq!(acp["result"]["agentInfo"]["name"], "scode");
    assert_eq!(
        acp["result"]["agentInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(acp["result"]["agentCapabilities"]["loadSession"], false);
    assert_eq!(
        acp["result"]["agentCapabilities"]["mcpCapabilities"]["http"],
        false
    );
    assert_eq!(acp["result"]["authMethods"], json!([]));
}

#[test]
fn acp_server_handles_session_prompt_for_local_slash_command() {
    let root = unique_temp_dir("acp-slash-prompt");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let (initialize, new_session, update, prompt_response) = assert_line_delimited_acp_slash_prompt(
        &root,
        &["--output-format", "json", "acp"],
        &[],
        "/help",
    );

    assert_eq!(initialize["id"], 1);
    assert_eq!(new_session["id"], 2);
    assert!(new_session["result"]["sessionId"].as_str().is_some());
    assert_eq!(update["method"], "session/update");
    assert_eq!(
        update["params"]["update"]["sessionUpdate"],
        "agent_message_chunk"
    );
    assert!(update["params"]["update"]["content"]["text"]
        .as_str()
        .expect("slash prompt text")
        .contains("Slash commands"));
    assert_eq!(prompt_response["id"], 3);
    assert_eq!(prompt_response["result"]["stopReason"], "end_turn");
}

#[test]
fn acp_server_lists_created_sessions() {
    let root = unique_temp_dir("acp-session-list");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let (first_session_id, second_session_id, session_list) =
        assert_line_delimited_acp_session_list(&root, &["--output-format", "json", "acp"], &[]);

    let listed_ids = session_list["result"]["sessions"]
        .as_array()
        .expect("sessions should be an array")
        .iter()
        .filter_map(|session| session["sessionId"].as_str())
        .collect::<Vec<_>>();

    assert!(listed_ids.contains(&first_session_id.as_str()));
    assert!(listed_ids.contains(&second_session_id.as_str()));
}

#[test]
fn acp_server_reports_unknown_slash_commands_as_not_supported() {
    let root = unique_temp_dir("acp-unknown-slash");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let (_initialize, _new_session, update, prompt_response) =
        assert_line_delimited_acp_slash_prompt(
            &root,
            &["--output-format", "json", "acp"],
            &[],
            "/nonexistent",
        );

    assert_eq!(prompt_response["result"]["stopReason"], "end_turn");
    assert!(update["params"]["update"]["content"]["text"]
        .as_str()
        .expect("unknown slash text")
        .contains("not supported"));
}

#[test]
fn inventory_commands_emit_structured_json_when_requested() {
    let root = unique_temp_dir("inventory-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let isolated_home = root.join("home");
    let isolated_config = root.join("config-home");
    let isolated_codex = root.join("codex-home");
    fs::create_dir_all(&isolated_home).expect("isolated home should exist");

    let agents = assert_json_command_with_env(
        &root,
        &["--output-format", "json", "agents"],
        &[
            ("HOME", isolated_home.to_str().expect("utf8 home")),
            (
                "SUDO_CODE_CONFIG_HOME",
                isolated_config.to_str().expect("utf8 config home"),
            ),
            (
                "CODEX_HOME",
                isolated_codex.to_str().expect("utf8 codex home"),
            ),
        ],
    );
    assert_eq!(agents["kind"], "agents");
    assert_eq!(agents["action"], "list");
    assert_eq!(agents["count"], 0);
    assert_eq!(agents["summary"]["active"], 0);
    assert!(agents["agents"]
        .as_array()
        .expect("agents array")
        .is_empty());

    let mcp = assert_json_command(&root, &["--output-format", "json", "mcp"]);
    assert_eq!(mcp["kind"], "mcp");
    assert_eq!(mcp["action"], "list");

    let skills = assert_json_command(&root, &["--output-format", "json", "skills"]);
    assert_eq!(skills["kind"], "skills");
    assert_eq!(skills["action"], "list");
}

#[test]
fn agents_command_emits_structured_agent_entries_when_requested() {
    let root = unique_temp_dir("agents-json-populated");
    let workspace = root.join("workspace");
    let project_agents = workspace.join(".codex").join("agents");
    let home = root.join("home");
    let user_agents = home.join(".codex").join("agents");
    let isolated_config = root.join("config-home");
    let isolated_codex = root.join("codex-home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    write_agent(
        &project_agents,
        "planner",
        "Project planner",
        "gpt-5.4",
        "medium",
    );
    write_agent(
        &project_agents,
        "verifier",
        "Verification agent",
        "gpt-5.4-mini",
        "high",
    );
    write_agent(
        &user_agents,
        "planner",
        "User planner",
        "gpt-5.4-mini",
        "high",
    );

    let parsed = assert_json_command_with_env(
        &workspace,
        &["--output-format", "json", "agents"],
        &[
            ("HOME", home.to_str().expect("utf8 home")),
            (
                "SUDO_CODE_CONFIG_HOME",
                isolated_config.to_str().expect("utf8 config home"),
            ),
            (
                "CODEX_HOME",
                isolated_codex.to_str().expect("utf8 codex home"),
            ),
        ],
    );

    assert_eq!(parsed["kind"], "agents");
    assert_eq!(parsed["action"], "list");
    assert_eq!(parsed["count"], 3);
    assert_eq!(parsed["summary"]["active"], 2);
    assert_eq!(parsed["summary"]["shadowed"], 1);
    assert_eq!(parsed["agents"][0]["name"], "planner");
    assert_eq!(parsed["agents"][0]["source"]["id"], "project_scode");
    assert_eq!(parsed["agents"][0]["active"], true);
    assert_eq!(parsed["agents"][1]["name"], "verifier");
    assert_eq!(parsed["agents"][2]["name"], "planner");
    assert_eq!(parsed["agents"][2]["active"], false);
    assert_eq!(parsed["agents"][2]["shadowed_by"]["id"], "project_scode");
}

#[test]
fn bootstrap_and_system_prompt_emit_json_when_requested() {
    let root = unique_temp_dir("bootstrap-system-prompt-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let plan = assert_json_command(&root, &["--output-format", "json", "bootstrap-plan"]);
    assert_eq!(plan["kind"], "bootstrap-plan");
    assert!(plan["phases"].as_array().expect("phases").len() > 1);

    let prompt = assert_json_command(&root, &["--output-format", "json", "system-prompt"]);
    assert_eq!(prompt["kind"], "system-prompt");
    assert!(prompt["message"]
        .as_str()
        .expect("prompt text")
        .contains("interactive agent"));
}

#[test]
fn dump_manifests_and_init_emit_json_when_requested() {
    let root = unique_temp_dir("manifest-init-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let upstream = write_upstream_fixture(&root);
    let manifests = assert_json_command(
        &root,
        &[
            "--output-format",
            "json",
            "dump-manifests",
            "--manifests-dir",
            upstream.to_str().expect("utf8 upstream"),
        ],
    );
    assert_eq!(manifests["kind"], "dump-manifests");
    assert_eq!(manifests["commands"], 1);
    assert_eq!(manifests["tools"], 1);

    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let init = assert_json_command(&workspace, &["--output-format", "json", "init"]);
    assert_eq!(init["kind"], "init");
    assert!(workspace.join("CLAUDE.md").exists());
}

#[test]
fn doctor_and_resume_status_emit_json_when_requested() {
    let root = unique_temp_dir("doctor-resume-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let doctor = assert_json_command(&root, &["--output-format", "json", "doctor"]);
    assert_eq!(doctor["kind"], "doctor");
    assert!(doctor["message"].is_string());
    let summary = doctor["summary"].as_object().expect("doctor summary");
    assert!(summary["ok"].as_u64().is_some());
    assert!(summary["warnings"].as_u64().is_some());
    assert!(summary["failures"].as_u64().is_some());

    let checks = doctor["checks"].as_array().expect("doctor checks");
    assert_eq!(checks.len(), 6);
    let check_names = checks
        .iter()
        .map(|check| {
            assert!(check["status"].as_str().is_some());
            assert!(check["summary"].as_str().is_some());
            assert!(check["details"].is_array());
            check["name"].as_str().expect("doctor check name")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        check_names,
        vec![
            "auth",
            "config",
            "install source",
            "workspace",
            "sandbox",
            "system"
        ]
    );

    let install_source = checks
        .iter()
        .find(|check| check["name"] == "install source")
        .expect("install source check");
    assert_eq!(
        install_source["official_repo"],
        "https://github.com/ultraworkers/sudocode"
    );
    assert_eq!(
        install_source["deprecated_install"],
        "cargo install sudocode"
    );

    let workspace = checks
        .iter()
        .find(|check| check["name"] == "workspace")
        .expect("workspace check");
    assert!(workspace["cwd"].as_str().is_some());
    assert!(workspace["in_git_repo"].is_boolean());

    let sandbox = checks
        .iter()
        .find(|check| check["name"] == "sandbox")
        .expect("sandbox check");
    assert!(sandbox["filesystem_mode"].as_str().is_some());
    assert!(sandbox["enabled"].is_boolean());
    assert!(sandbox["fallback_reason"].is_null() || sandbox["fallback_reason"].is_string());

    let session_path = write_session_fixture(&root, "resume-json", Some("hello"));
    let resumed = assert_json_command(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/status",
        ],
    );
    assert_eq!(resumed["kind"], "status");
    // model is null in resume mode (not known without --model flag)
    assert!(resumed["model"].is_null());
    assert_eq!(resumed["usage"]["messages"], 1);
    assert!(resumed["workspace"]["cwd"].as_str().is_some());
    assert!(resumed["sandbox"]["filesystem_mode"].as_str().is_some());
}

#[test]
fn resumed_inventory_commands_emit_structured_json_when_requested() {
    let root = unique_temp_dir("resume-inventory-json");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    let session_path = write_session_fixture(&root, "resume-inventory-json", Some("inventory"));

    let mcp = assert_json_command_with_env(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/mcp",
        ],
        &[
            (
                "SUDO_CODE_CONFIG_HOME",
                config_home.to_str().expect("utf8 config home"),
            ),
            ("HOME", home.to_str().expect("utf8 home")),
        ],
    );
    assert_eq!(mcp["kind"], "mcp");
    assert_eq!(mcp["action"], "list");
    assert!(mcp["servers"].is_array());

    let skills = assert_json_command_with_env(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/skills",
        ],
        &[
            (
                "SUDO_CODE_CONFIG_HOME",
                config_home.to_str().expect("utf8 config home"),
            ),
            ("HOME", home.to_str().expect("utf8 home")),
        ],
    );
    assert_eq!(skills["kind"], "skills");
    assert_eq!(skills["action"], "list");
    assert!(skills["summary"]["total"].is_number());
    assert!(skills["skills"].is_array());
}

#[test]
fn resumed_version_and_init_emit_structured_json_when_requested() {
    let root = unique_temp_dir("resume-version-init-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let session_path = write_session_fixture(&root, "resume-version-init-json", None);

    let version = assert_json_command(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/version",
        ],
    );
    assert_eq!(version["kind"], "version");
    assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));

    let init = assert_json_command(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/init",
        ],
    );
    assert_eq!(init["kind"], "init");
    assert!(root.join("CLAUDE.md").exists());
}

fn assert_json_command(current_dir: &Path, args: &[&str]) -> Value {
    assert_json_command_with_env(current_dir, args, &[])
}

fn assert_json_command_with_env(current_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Value {
    let output = run_scode(current_dir, args, envs);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout should be valid json")
}

fn run_scode(current_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_scode"));
    command.current_dir(current_dir).args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("scode should launch")
}

fn assert_line_delimited_acp_command(
    current_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    request: &Value,
) -> Value {
    let request = serde_json::to_string(request).expect("request should serialize");
    let mut child = Command::new(env!("CARGO_BIN_EXE_scode"))
        .current_dir(current_dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(envs.iter().copied())
        .spawn()
        .expect("scode should launch");
    {
        let stdin = child.stdin.as_mut().expect("stdin should be piped");
        stdin
            .write_all(request.as_bytes())
            .expect("request line should write");
        stdin.write_all(b"\n").expect("newline should write");
    }
    drop(child.stdin.take());
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("stdout should be piped"));
    let mut stderr = child.stderr.take().expect("stderr should be piped");
    let mut response_line = String::new();
    stdout
        .read_line(&mut response_line)
        .expect("response line should read");
    if response_line.trim().is_empty() {
        let _ = child.kill();
        let _ = child.wait();
        let mut stderr_output = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut stderr_output);
        panic!("stdout did not contain a JSON-RPC response line\n\nstderr:\n{stderr_output}");
    }

    let _ = child.kill();
    let _ = child.wait();

    serde_json::from_str(&response_line).expect("response line should be valid json")
}

fn assert_line_delimited_acp_slash_prompt(
    current_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    prompt: &str,
) -> (Value, Value, Value, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_scode"))
        .current_dir(current_dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(envs.iter().copied())
        .spawn()
        .expect("scode should launch");
    let mut stdin = child.stdin.take().expect("stdin should be piped");
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("stdout should be piped"));
    let mut stderr = child.stderr.take().expect("stderr should be piped");

    let read_json_line = |stdout: &mut std::io::BufReader<_>,
                          stderr: &mut _,
                          child: &mut std::process::Child|
     -> Value {
        let mut line = String::new();
        stdout
            .read_line(&mut line)
            .expect("response line should read");
        if line.trim().is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            let mut stderr_output = String::new();
            let _ = std::io::Read::read_to_string(stderr, &mut stderr_output);
            panic!("stdout did not contain a JSON-RPC response line\n\nstderr:\n{stderr_output}");
        }
        serde_json::from_str(&line).expect("response line should be valid json")
    };

    for request in [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": 9 },
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {
                "cwd": current_dir,
                "mcpServers": [],
            },
        }),
    ] {
        let request = serde_json::to_string(&request).expect("request should serialize");
        stdin
            .write_all(request.as_bytes())
            .expect("request line should write");
        stdin.write_all(b"\n").expect("newline should write");
    }

    let initialize: Value = read_json_line(&mut stdout, &mut stderr, &mut child);
    let new_session: Value = read_json_line(&mut stdout, &mut stderr, &mut child);
    let session_id = new_session["result"]["sessionId"]
        .as_str()
        .expect("session/new should return sessionId");
    let prompt_request = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": prompt }],
        },
    }))
    .expect("prompt request should serialize");
    stdin
        .write_all(prompt_request.as_bytes())
        .expect("prompt request should write");
    stdin.write_all(b"\n").expect("newline should write");

    let update: Value = read_json_line(&mut stdout, &mut stderr, &mut child);
    let prompt_response: Value = read_json_line(&mut stdout, &mut stderr, &mut child);

    let _ = child.kill();
    let _ = child.wait();

    (initialize, new_session, update, prompt_response)
}

fn assert_line_delimited_acp_session_list(
    current_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> (String, String, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_scode"))
        .current_dir(current_dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(envs.iter().copied())
        .spawn()
        .expect("scode should launch");
    let mut stdin = child.stdin.take().expect("stdin should be piped");
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("stdout should be piped"));
    let mut stderr = child.stderr.take().expect("stderr should be piped");

    let read_json_line = |stdout: &mut std::io::BufReader<_>,
                          stderr: &mut _,
                          child: &mut std::process::Child|
     -> Value {
        let mut line = String::new();
        stdout
            .read_line(&mut line)
            .expect("response line should read");
        if line.trim().is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            let mut stderr_output = String::new();
            let _ = std::io::Read::read_to_string(stderr, &mut stderr_output);
            panic!("stdout did not contain a JSON-RPC response line\n\nstderr:\n{stderr_output}");
        }
        serde_json::from_str(&line).expect("response line should be valid json")
    };

    for request in [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": 9 },
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {
                "cwd": current_dir,
                "mcpServers": [],
            },
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/new",
            "params": {
                "cwd": current_dir,
                "mcpServers": [],
            },
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/list",
            "params": {},
        }),
    ] {
        let request = serde_json::to_string(&request).expect("request should serialize");
        stdin
            .write_all(request.as_bytes())
            .expect("request line should write");
        stdin.write_all(b"\n").expect("newline should write");
    }

    let _initialize: Value = read_json_line(&mut stdout, &mut stderr, &mut child);
    let first_session: Value = read_json_line(&mut stdout, &mut stderr, &mut child);
    let second_session: Value = read_json_line(&mut stdout, &mut stderr, &mut child);
    let session_list: Value = read_json_line(&mut stdout, &mut stderr, &mut child);

    let _ = child.kill();
    let _ = child.wait();

    (
        first_session["result"]["sessionId"]
            .as_str()
            .expect("first session id should exist")
            .to_string(),
        second_session["result"]["sessionId"]
            .as_str()
            .expect("second session id should exist")
            .to_string(),
        session_list,
    )
}

fn write_upstream_fixture(root: &Path) -> PathBuf {
    let upstream = root.join("sudocode");
    let src = upstream.join("src");
    let entrypoints = src.join("entrypoints");
    fs::create_dir_all(&entrypoints).expect("upstream entrypoints dir should exist");
    fs::write(
        src.join("commands.ts"),
        "import FooCommand from './commands/foo'\n",
    )
    .expect("commands fixture should write");
    fs::write(
        src.join("tools.ts"),
        "import ReadTool from './tools/read'\n",
    )
    .expect("tools fixture should write");
    fs::write(
        entrypoints.join("cli.tsx"),
        "if (args[0] === '--version') {}\nstartupProfiler()\n",
    )
    .expect("cli fixture should write");
    upstream
}

fn write_session_fixture(root: &Path, session_id: &str, user_text: Option<&str>) -> PathBuf {
    let session_path = root.join("session.jsonl");
    let mut session = Session::new()
        .with_workspace_root(root.to_path_buf())
        .with_persistence_path(session_path.clone());
    session.session_id = session_id.to_string();
    if let Some(text) = user_text {
        session
            .push_user_text(text)
            .expect("session fixture message should persist");
    } else {
        session
            .save_to_path(&session_path)
            .expect("session fixture should persist");
    }
    session_path
}

fn write_agent(root: &Path, name: &str, description: &str, model: &str, reasoning: &str) {
    fs::create_dir_all(root).expect("agent root should exist");
    fs::write(
        root.join(format!("{name}.toml")),
        format!(
            "name = \"{name}\"\ndescription = \"{description}\"\nmodel = \"{model}\"\nmodel_reasoning_effort = \"{reasoning}\"\n"
        ),
    )
    .expect("agent fixture should write");
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_millis();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "scode-output-format-{label}-{}-{millis}-{counter}",
        std::process::id()
    ))
}

#![allow(clippy::doc_markdown)]
//! Sandbox escape tests — exercise sandbox boundaries through the execute_bash API.
//!
//! These tests verify that the sandbox properly:
//! 1. Redirects HOME and TMPDIR when filesystem isolation is active
//! 2. Reports correct sandbox status for each configuration
//! 3. On macOS, relies on env-var redirection (no kernel enforcement)
//! 4. On Linux, uses namespace isolation via unshare(1)

use runtime::sandbox::{
    build_linux_sandbox_command, resolve_sandbox_status_for_request, FilesystemIsolationMode,
    SandboxConfig, SandboxRequest, SandboxStatus,
};
use runtime::{execute_bash, BashCommandInput};
use std::path::Path;

fn make_input(command: &str, disable_sandbox: bool) -> BashCommandInput {
    BashCommandInput {
        command: command.to_string(),
        timeout: Some(5_000),
        description: Some("sandbox escape test".to_string()),
        run_in_background: Some(false),
        dangerously_disable_sandbox: Some(disable_sandbox),
        namespace_restrictions: Some(false),
        isolate_network: Some(false),
        filesystem_mode: Some(FilesystemIsolationMode::WorkspaceOnly),
        allowed_mounts: None,
    }
}

// ── Environment redirection tests ───────────────────────────────

#[test]
fn sandbox_enabled_redirects_home_to_sandbox_home() {
    let output =
        execute_bash(make_input("printf '%s' \"$HOME\"", false)).expect("command should execute");

    let home = output.stdout.trim();
    assert!(
        home.ends_with(".sandbox-home"),
        "HOME should end with .sandbox-home when sandbox is active, got: {home}"
    );
}

#[test]
fn sandbox_enabled_redirects_tmpdir_to_sandbox_tmp() {
    let output =
        execute_bash(make_input("printf '%s' \"$TMPDIR\"", false)).expect("command should execute");

    let tmpdir = output.stdout.trim();
    assert!(
        tmpdir.ends_with(".sandbox-tmp"),
        "TMPDIR should end with .sandbox-tmp when sandbox is active, got: {tmpdir}"
    );
}

#[test]
fn sandbox_disabled_preserves_real_home() {
    let output =
        execute_bash(make_input("printf '%s' \"$HOME\"", true)).expect("command should execute");

    let home = output.stdout.trim();
    assert!(
        !home.ends_with(".sandbox-home"),
        "HOME should NOT be redirected when sandbox is disabled, got: {home}"
    );
}

#[test]
fn sandbox_disabled_preserves_real_tmpdir() {
    let output = execute_bash(make_input("printf '%s' \"${TMPDIR:-/tmp}\"", true))
        .expect("command should execute");

    let tmpdir = output.stdout.trim();
    assert!(
        !tmpdir.ends_with(".sandbox-tmp"),
        "TMPDIR should NOT be redirected when sandbox is disabled, got: {tmpdir}"
    );
}

// ── Filesystem mode tests ───────────────────────────────────────

#[test]
fn filesystem_off_mode_does_not_redirect_env() {
    let input = BashCommandInput {
        command: "printf '%s' \"$HOME\"".to_string(),
        timeout: Some(5_000),
        description: None,
        run_in_background: Some(false),
        dangerously_disable_sandbox: Some(false),
        namespace_restrictions: Some(false),
        isolate_network: Some(false),
        filesystem_mode: Some(FilesystemIsolationMode::Off),
        allowed_mounts: None,
    };

    let output = execute_bash(input).expect("command should execute");
    let home = output.stdout.trim();
    assert!(
        !home.ends_with(".sandbox-home"),
        "HOME should NOT be redirected in filesystem Off mode, got: {home}"
    );
}

// ── Status resolution tests ─────────────────────────────────────

#[test]
fn status_reports_filesystem_active_for_workspace_only_mode() {
    let request = SandboxRequest {
        enabled: true,
        namespace_restrictions: false,
        network_isolation: false,
        filesystem_mode: FilesystemIsolationMode::WorkspaceOnly,
        allowed_mounts: vec![],
    };

    let status = resolve_sandbox_status_for_request(&request, Path::new("/tmp"));
    assert!(status.filesystem_active, "filesystem should be active");
    assert!(status.enabled, "sandbox should be marked enabled");
}

#[test]
fn status_reports_filesystem_inactive_for_off_mode() {
    let request = SandboxRequest {
        enabled: true,
        namespace_restrictions: false,
        network_isolation: false,
        filesystem_mode: FilesystemIsolationMode::Off,
        allowed_mounts: vec![],
    };

    let status = resolve_sandbox_status_for_request(&request, Path::new("/tmp"));
    assert!(
        !status.filesystem_active,
        "filesystem should be inactive in Off mode"
    );
}

#[test]
fn status_includes_fallback_reason_when_namespace_unsupported() {
    let request = SandboxRequest {
        enabled: true,
        namespace_restrictions: true,
        network_isolation: false,
        filesystem_mode: FilesystemIsolationMode::WorkspaceOnly,
        allowed_mounts: vec![],
    };

    let status = resolve_sandbox_status_for_request(&request, Path::new("/tmp"));

    if !cfg!(target_os = "linux") {
        assert!(
            status.fallback_reason.is_some(),
            "non-Linux should have a fallback reason for namespace restrictions"
        );
        assert!(
            status
                .fallback_reason
                .as_deref()
                .unwrap_or("")
                .contains("namespace isolation unavailable"),
            "fallback reason should mention namespace unavailability"
        );
    }
}

#[test]
fn status_includes_fallback_for_empty_allow_list() {
    let request = SandboxRequest {
        enabled: true,
        namespace_restrictions: false,
        network_isolation: false,
        filesystem_mode: FilesystemIsolationMode::AllowList,
        allowed_mounts: vec![],
    };

    let status = resolve_sandbox_status_for_request(&request, Path::new("/tmp"));
    assert!(
        status
            .fallback_reason
            .as_deref()
            .unwrap_or("")
            .contains("allow-list requested without configured mounts"),
        "empty allow-list should produce fallback reason"
    );
}

// ── Config resolution tests ─────────────────────────────────────

#[test]
fn config_defaults_enable_sandbox_with_workspace_only() {
    let config = SandboxConfig::default();
    let request = config.resolve_request(None, None, None, None, None);

    assert!(request.enabled, "default should enable sandbox");
    assert!(
        request.namespace_restrictions,
        "default should enable namespace restrictions"
    );
    assert!(
        !request.network_isolation,
        "default should NOT enable network isolation"
    );
    assert_eq!(
        request.filesystem_mode,
        FilesystemIsolationMode::WorkspaceOnly,
        "default filesystem mode should be WorkspaceOnly"
    );
}

#[test]
fn overrides_take_precedence_over_config() {
    let config = SandboxConfig {
        enabled: Some(true),
        namespace_restrictions: Some(true),
        network_isolation: Some(false),
        filesystem_mode: Some(FilesystemIsolationMode::WorkspaceOnly),
        allowed_mounts: vec![],
    };

    let request = config.resolve_request(
        Some(false),                              // disable sandbox
        Some(false),                              // disable namespace
        Some(true),                               // enable network
        Some(FilesystemIsolationMode::AllowList), // switch fs mode
        Some(vec!["logs".to_string()]),           // add mount
    );

    assert!(!request.enabled);
    assert!(!request.namespace_restrictions);
    assert!(request.network_isolation);
    assert_eq!(request.filesystem_mode, FilesystemIsolationMode::AllowList);
    assert_eq!(request.allowed_mounts, vec!["logs"]);
}

// ── Linux sandbox command tests ─────────────────────────────────

#[test]
fn linux_sandbox_command_includes_net_only_when_requested() {
    let with_net = SandboxStatus {
        enabled: true,
        namespace_supported: true,
        namespace_active: true,
        network_supported: true,
        network_active: true,
        filesystem_mode: FilesystemIsolationMode::WorkspaceOnly,
        filesystem_active: true,
        ..SandboxStatus::default()
    };

    let without_net = SandboxStatus {
        enabled: true,
        namespace_supported: true,
        namespace_active: true,
        network_supported: true,
        network_active: false,
        filesystem_mode: FilesystemIsolationMode::WorkspaceOnly,
        filesystem_active: true,
        ..SandboxStatus::default()
    };

    if cfg!(target_os = "linux") {
        let cmd_with = build_linux_sandbox_command("echo hi", Path::new("/ws"), &with_net);
        assert!(cmd_with.is_some());
        assert!(cmd_with.unwrap().args.contains(&"--net".to_string()));

        let cmd_without = build_linux_sandbox_command("echo hi", Path::new("/ws"), &without_net);
        assert!(cmd_without.is_some());
        assert!(!cmd_without.unwrap().args.contains(&"--net".to_string()));
    } else {
        // On non-Linux, build_linux_sandbox_command always returns None
        assert!(build_linux_sandbox_command("echo hi", Path::new("/ws"), &with_net).is_none());
    }
}

// ── macOS-specific: env-var only enforcement ────────────────────

#[test]
fn macos_sandbox_is_env_var_only_no_namespace_support() {
    if cfg!(target_os = "macos") {
        let request = SandboxRequest {
            enabled: true,
            namespace_restrictions: true,
            network_isolation: true,
            filesystem_mode: FilesystemIsolationMode::WorkspaceOnly,
            allowed_mounts: vec![],
        };

        let status = resolve_sandbox_status_for_request(&request, Path::new("/tmp"));

        assert!(
            !status.namespace_supported,
            "macOS should not support namespaces"
        );
        assert!(
            !status.namespace_active,
            "macOS should not have active namespaces"
        );
        assert!(
            !status.network_supported,
            "macOS should not support network isolation"
        );
        assert!(
            !status.network_active,
            "macOS should not have active network isolation"
        );
        assert!(
            status.filesystem_active,
            "macOS should still have filesystem env-var redirection"
        );
        assert!(
            status.fallback_reason.is_some(),
            "macOS should report fallback reason for namespace/network"
        );
    }
}

// ── Practical escape: macOS env-var sandbox does NOT prevent filesystem access ──

#[test]
fn macos_sandbox_cannot_prevent_reading_etc_passwd() {
    // On macOS, the sandbox only redirects HOME/TMPDIR via env vars.
    // It cannot actually prevent reading arbitrary files.
    if cfg!(target_os = "macos") {
        let output = execute_bash(make_input("cat /etc/passwd | head -1", false))
            .expect("command should execute");

        // This WILL succeed on macOS — demonstrating the env-var-only limitation
        assert!(
            !output.stdout.is_empty(),
            "macOS sandbox should NOT prevent reading /etc/passwd (env-var only)"
        );
    }
}

#[test]
fn macos_sandbox_cannot_prevent_writing_tmp() {
    if cfg!(target_os = "macos") {
        let output = execute_bash(make_input(
            "echo probe > /tmp/.sandbox-escape-test && rm /tmp/.sandbox-escape-test && echo ok",
            false,
        ))
        .expect("command should execute");

        // This WILL succeed on macOS — the sandbox has no kernel enforcement
        assert_eq!(
            output.stdout.trim(),
            "ok",
            "macOS sandbox should NOT prevent writing to /tmp (env-var only)"
        );
    }
}

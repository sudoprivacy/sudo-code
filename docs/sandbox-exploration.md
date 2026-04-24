# Sandbox Feature: Usage, Limits, and Best Practices

> Exploration report — April 2026

## 1. Overview

The `scode` sandbox restricts commands executed by the AI assistant's Bash tool.
It provides three layers of protection:

| Layer | Linux | macOS |
|-------|-------|-------|
| **Namespace isolation** | `unshare(1)` — user, mount, IPC, PID, UTS namespaces | Not available |
| **Network isolation** | `unshare --net` (new network namespace) | Not available |
| **Filesystem env-var redirection** | `HOME`/`TMPDIR` → `.sandbox-home`/`.sandbox-tmp` | Same env-var redirection |

## 2. Sandbox Status Report

Running `scode sandbox` on macOS produces:

```
Sandbox
  Enabled           true
  Active            false          ← "active" requires all requested layers to work
  Supported         false          ← namespace support requires Linux + unshare
  In container      false
  Requested ns      true
  Active ns         false
  Requested net     false
  Active net        false
  Filesystem mode   workspace-only
  Filesystem active true           ← env-var redirection IS active
  Allowed mounts    <none>
  Markers           <none>
  Fallback reason   namespace isolation unavailable (requires Linux with `unshare`)
```

Key observations:
- **`Enabled: true`** — sandbox is enabled by default.
- **`Active: false`** — on macOS, because namespace isolation is unavailable.
- **`Filesystem active: true`** — the env-var layer still works on both platforms.
- **`Fallback reason`** — explains why full isolation isn't active.

## 3. Escape Test Results

### 3.1 Bash Script Probes (macOS, no kernel sandbox)

| Probe | Without Sandbox | With Sandbox (macOS) |
|-------|----------------|----------------------|
| Write to `/tmp` | Escaped | **Escaped** — no kernel enforcement |
| Write to real `$HOME` | Escaped | **Escaped** — no kernel enforcement |
| Read `~/.ssh/id_rsa` | Blocked (file absent) | Blocked (file absent) |
| Read `/etc/passwd` | Escaped | **Escaped** — no kernel enforcement |
| Write inside workspace | Escaped | Escaped (intended) |
| Write to `/usr/local` | Blocked (permissions) | Blocked (permissions) |
| DNS resolution | Escaped | **Escaped** — no network namespace |
| HTTP fetch | Escaped | **Escaped** — no network namespace |
| TCP connect | Escaped | **Escaped** — no network namespace |
| List host processes | Escaped | **Escaped** — no PID namespace |
| `HOME` redirected | No | **Yes** — `.sandbox-home` |
| `TMPDIR` redirected | No | **Yes** — `.sandbox-tmp` |

### 3.2 Rust Integration Tests (15 tests, all passing)

| Test | Result | What It Proves |
|------|--------|----------------|
| `sandbox_enabled_redirects_home_to_sandbox_home` | PASS | HOME → .sandbox-home |
| `sandbox_enabled_redirects_tmpdir_to_sandbox_tmp` | PASS | TMPDIR → .sandbox-tmp |
| `sandbox_disabled_preserves_real_home` | PASS | disabling sandbox restores real HOME |
| `sandbox_disabled_preserves_real_tmpdir` | PASS | disabling sandbox restores real TMPDIR |
| `filesystem_off_mode_does_not_redirect_env` | PASS | `FilesystemIsolationMode::Off` skips redirect |
| `status_reports_filesystem_active_for_workspace_only_mode` | PASS | correct status reporting |
| `status_reports_filesystem_inactive_for_off_mode` | PASS | Off mode is inactive |
| `status_includes_fallback_reason_when_namespace_unsupported` | PASS | fallback reason present on macOS |
| `status_includes_fallback_for_empty_allow_list` | PASS | empty allow-list detected |
| `config_defaults_enable_sandbox_with_workspace_only` | PASS | defaults are safe |
| `overrides_take_precedence_over_config` | PASS | per-call overrides work |
| `linux_sandbox_command_includes_net_only_when_requested` | PASS | `--net` flag conditional |
| `macos_sandbox_is_env_var_only_no_namespace_support` | PASS | macOS limitations documented |
| `macos_sandbox_cannot_prevent_reading_etc_passwd` | PASS | env-var sandbox doesn't block reads |
| `macos_sandbox_cannot_prevent_writing_tmp` | PASS | env-var sandbox doesn't block writes |

## 4. Permission Modes and Sandbox Interaction

The permission system (`PermissionMode`) and the sandbox are **independent layers**:

| Permission Mode | What It Controls | Sandbox Interaction |
|----------------|-----------------|---------------------|
| `ReadOnly` | Denies write tools at the tool-dispatch layer | Sandbox still active for any allowed reads |
| `WorkspaceWrite` | Allows writes within workspace; prompts for danger tools | Sandbox redirection active for Bash commands |
| `DangerFullAccess` | Allows all tools without prompting | Sandbox still active unless `dangerouslyDisableSandbox` |
| `Prompt` | Always prompts the user | Sandbox active |
| `Allow` | Allows everything (rule-based) | Sandbox active |

Key finding: **`DangerFullAccess` does NOT disable the sandbox**. The sandbox can only be
disabled per-command via `dangerouslyDisableSandbox: true` in the `BashCommandInput`. The
permission mode controls *whether the tool is invoked at all*, while the sandbox controls
*what the tool can access during execution*.

### How `dangerouslyDisableSandbox` Works

```
BashCommandInput.dangerously_disable_sandbox = Some(true)
  → SandboxConfig.resolve_request(enabled_override = Some(false), ...)
  → SandboxRequest.enabled = false
  → SandboxStatus.enabled = false, filesystem_active = false
  → Command runs with real HOME, TMPDIR, no namespace wrapping
```

## 5. Linux vs macOS: Enforcement Differences

### Linux (Namespace-Based)

On Linux with `unshare(1)` available, the sandbox provides **kernel-enforced isolation**:

```
unshare --user --map-root-user --mount --ipc --pid --uts --fork [--net] sh -lc "command"
```

| Namespace | Effect |
|-----------|--------|
| `--user --map-root-user` | Process runs as UID 0 inside namespace, mapped to calling user outside |
| `--mount` | Separate mount table — can hide host filesystem |
| `--ipc` | Separate IPC namespace (shared memory, semaphores) |
| `--pid` | Separate PID namespace — cannot see host processes |
| `--uts` | Separate hostname namespace |
| `--net` | Separate network namespace — **no network access** (only if `networkIsolation: true`) |

Environment variables are also set:
- `HOME` → `<workspace>/.sandbox-home`
- `TMPDIR` → `<workspace>/.sandbox-tmp`
- `SUDOCODE_SANDBOX_FILESYSTEM_MODE` → `"workspace-only"` or `"allow-list"`
- `SUDOCODE_SANDBOX_ALLOWED_MOUNTS` → colon-separated paths

**Linux enforcement is real**: processes inside the namespace genuinely cannot access
the host filesystem (except the workspace bind-mount), cannot see host processes,
and cannot reach the network when `--net` is used.

#### Linux Caveats

- **GitHub Actions / CI**: `unshare --user` often fails due to kernel restrictions
  (`kernel.unprivileged_userns_clone=0`). The sandbox detects this via
  `unshare_user_namespace_works()` and falls back gracefully.
- **Container environments**: When already inside Docker/Podman/Kubernetes, nested
  namespaces may not work. The sandbox detects containers via `/. dockerenv`,
  `/run/.containerenv`, cgroup hints, and env vars.

### macOS (Env-Var Only)

macOS **does not have Linux namespaces**. The sandbox falls back to:

1. **Redirect `HOME`** → `<workspace>/.sandbox-home`
2. **Redirect `TMPDIR`** → `<workspace>/.sandbox-tmp`
3. **Set `SUDOCODE_SANDBOX_FILESYSTEM_MODE`** env var

**What this protects against:**
- Well-behaved tools that use `$HOME` for config/cache will write to the sandbox
- Tools that use `$TMPDIR` for temp files will write inside workspace
- Provides a signal to sandbox-aware code about the active isolation mode

**What this does NOT protect against:**
- Direct filesystem access (e.g., `cat /etc/passwd`, `echo > /tmp/file`)
- Network access (curl, wget, nc, etc.)
- Process visibility (ps, kill, etc.)
- Any code that ignores HOME/TMPDIR env vars and uses absolute paths

### Comparison Matrix

| Capability | Linux (namespaces) | macOS (env-var) |
|-----------|-------------------|-----------------|
| Filesystem isolation | Kernel-enforced | Convention-only |
| Network isolation | Kernel-enforced (`--net`) | None |
| Process isolation | Kernel-enforced (`--pid`) | None |
| IPC isolation | Kernel-enforced (`--ipc`) | None |
| HOME redirection | Env-var + mount namespace | Env-var only |
| TMPDIR redirection | Env-var + mount namespace | Env-var only |
| Container detection | Yes | Yes (reports no sandbox support) |
| Graceful fallback | Yes (reports fallback reason) | Yes (reports fallback reason) |

## 6. Configuration Reference

### `.scode.json` / `.nexus/sudocode/settings.json`

```json
{
  "sandbox": {
    "enabled": true,
    "namespaceRestrictions": true,
    "networkIsolation": false,
    "filesystemMode": "workspace-only",
    "allowedMounts": []
  }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Master switch for the entire sandbox |
| `namespaceRestrictions` | `bool` | `true` | Request Linux namespace isolation |
| `networkIsolation` | `bool` | `false` | Block all network access (Linux only) |
| `filesystemMode` | `string` | `"workspace-only"` | `"off"`, `"workspace-only"`, or `"allow-list"` |
| `allowedMounts` | `string[]` | `[]` | Extra paths visible in allow-list mode |

### Filesystem Modes

| Mode | Behavior |
|------|----------|
| `off` | No filesystem isolation; real HOME and TMPDIR |
| `workspace-only` | HOME/TMPDIR redirected to workspace subdirs |
| `allow-list` | Only workspace + explicit `allowedMounts` paths accessible |

## 7. Best Practices for Tool Developers

### DO

1. **Always use `$HOME` and `$TMPDIR`** — never hardcode `/home/<user>` or `/tmp`.
   The sandbox redirects these env vars, so tools that honor them automatically
   write to safe locations.

2. **Check `SUDOCODE_SANDBOX_FILESYSTEM_MODE`** — if your tool needs to know
   whether it's sandboxed, read this env var. Values: `"off"`, `"workspace-only"`,
   `"allow-list"`.

3. **Keep artifacts inside the workspace** — write output files, caches, and logs
   relative to the current directory or `$HOME`. This works on both Linux and macOS.

4. **Test with `scode sandbox`** — run `scode sandbox --output-format json` in your
   project directory to verify what isolation is actually active before relying on it.

5. **Handle network absence gracefully** — if `networkIsolation` is enabled, all
   outbound connections will fail. Tools should check for this and provide clear
   error messages rather than hanging on connection timeouts.

6. **Use `allowedMounts` for shared caches** — if your tool needs access to a
   shared cache directory (e.g., `~/.cargo/registry`), configure it in
   `allowedMounts` rather than disabling the sandbox entirely.

### DON'T

7. **Don't rely on macOS sandbox for security** — the macOS sandbox is a
   convention layer, not a security boundary. On macOS, any subprocess CAN
   access the full filesystem and network. The env-var redirection is a guide
   for well-behaved tools, not an enforcement mechanism.

8. **Don't use `dangerouslyDisableSandbox` without cause** — this flag exists
   for commands that genuinely need full system access (e.g., Docker builds,
   system package managers). Using it for convenience defeats the purpose.

9. **Don't assume namespace support** — always check `SandboxStatus.supported`
   or `SandboxStatus.namespace_active`. Even on Linux, user namespaces may be
   disabled by the kernel or restricted in CI environments.

10. **Don't ignore the fallback reason** — when `enabled: true` but `active: false`,
    the `fallback_reason` field explains why. Surface this to users so they
    understand their actual security posture.

11. **Don't embed secrets in tool code** — even with sandbox isolation, secrets
    in environment variables or hardcoded paths may leak through `/proc` or
    other side channels. Use proper secret management.

### Architecture Guidance

12. **Treat sandbox as defense-in-depth** — the sandbox is one layer in a
    multi-layer security model (permissions + sandbox + user approval). Don't
    rely on any single layer alone.

13. **Design for the weakest platform** — if your tool must work on both Linux
    and macOS, assume macOS-level isolation (env-var only). If you need stronger
    guarantees, document the Linux requirement.

14. **Test escape probes in CI** — include the test script from
    `tests/sandbox_escape_test.sh` in your CI pipeline to verify sandbox
    behavior on your target platform.

## 8. Files Added

| File | Purpose |
|------|---------|
| `tests/sandbox_escape_test.sh` | Bash script that probes sandbox boundaries |
| `rust/crates/runtime/tests/sandbox_escape_tests.rs` | 15 Rust integration tests for sandbox behavior |
| `docs/sandbox-exploration.md` | This document |

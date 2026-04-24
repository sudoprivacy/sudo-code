#!/usr/bin/env bash
# sandbox_escape_test.sh — Probes sandbox boundaries to document enforcement limits.
#
# Run this script directly (outside the sandbox) to see what the OS allows,
# then run it via `scode` bash execution to see what the sandbox blocks.
#
# Exit codes:
#   0 — all probes completed (check per-probe PASS/FAIL in output)
#   1 — script itself failed to start

set -euo pipefail

WORKSPACE="${SUDOCODE_SANDBOX_WORKSPACE:-$(pwd)}"
RESULTS=()

probe() {
    local name="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        RESULTS+=("FAIL (escaped)  $name")
    else
        RESULTS+=("PASS (blocked)  $name")
    fi
}

echo "=== Sandbox Escape Probes ==="
echo "Date:      $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "Platform:  $(uname -s) $(uname -m)"
echo "Workspace: $WORKSPACE"
echo "HOME:      ${HOME:-<unset>}"
echo "TMPDIR:    ${TMPDIR:-<unset>}"
echo "FS mode:   ${SUDOCODE_SANDBOX_FILESYSTEM_MODE:-<unset>}"
echo "Mounts:    ${SUDOCODE_SANDBOX_ALLOWED_MOUNTS:-<unset>}"
echo ""

# ── Filesystem probes ────────────────────────────────────────────

# 1. Write outside workspace — to /tmp
probe "write /tmp/sandbox-probe" \
    sh -c 'echo probe > /tmp/sandbox-probe && rm -f /tmp/sandbox-probe'

# 2. Write to real home directory
REAL_HOME="${ORIGINAL_HOME:-/Users/$(whoami)}"
probe "write \$REAL_HOME/.sandbox-probe" \
    sh -c "echo probe > '$REAL_HOME/.sandbox-probe' && rm -f '$REAL_HOME/.sandbox-probe'"

# 3. Read sensitive file from real home
probe "read \$REAL_HOME/.ssh/id_rsa" \
    test -r "$REAL_HOME/.ssh/id_rsa"

probe "read \$REAL_HOME/.ssh/id_ed25519" \
    test -r "$REAL_HOME/.ssh/id_ed25519"

# 4. Read /etc/passwd (always present on Unix)
probe "read /etc/passwd" \
    test -r /etc/passwd

# 5. Write inside workspace (should succeed even in sandbox)
probe "write workspace/sandbox-probe-internal" \
    sh -c "echo probe > '$WORKSPACE/.sandbox-probe-internal' && rm -f '$WORKSPACE/.sandbox-probe-internal'"

# 6. Write to system directories
probe "write /usr/local/sandbox-probe" \
    sh -c 'echo probe > /usr/local/sandbox-probe'

# 7. Read .git/config from workspace parent
probe "read ../../../.git/config" \
    test -r "$WORKSPACE/../../../.git/config"

# ── Network probes ───────────────────────────────────────────────

# 8. DNS resolution
probe "DNS resolution (dns.google)" \
    sh -c 'getent hosts dns.google 2>/dev/null || host dns.google 2>/dev/null || nslookup dns.google 2>/dev/null'

# 9. HTTP fetch
probe "HTTP fetch (example.com)" \
    sh -c 'curl -s --connect-timeout 3 -o /dev/null https://example.com'

# 10. Raw TCP connection
probe "TCP connect (1.1.1.1:53)" \
    sh -c 'echo | nc -w 2 1.1.1.1 53 2>/dev/null'

# ── Process / namespace probes ───────────────────────────────────

# 11. See host processes
probe "list host processes (ps aux)" \
    sh -c 'ps aux | grep -q "launchd\|systemd"'

# 12. Access /proc (Linux)
if [ "$(uname -s)" = "Linux" ]; then
    probe "read /proc/1/cmdline" \
        test -r /proc/1/cmdline

    probe "read /proc/self/ns/net" \
        readlink /proc/self/ns/net
fi

# ── Environment probes ───────────────────────────────────────────

# 13. Env var leak — PATH should still exist
probe "PATH env var present" \
    sh -c 'test -n "$PATH"'

# This one is inverted — HOME *should* be redirected
if [[ "${HOME:-}" == *".sandbox-home"* ]]; then
    RESULTS+=("PASS (sandboxed) HOME points to .sandbox-home")
else
    RESULTS+=("INFO (raw HOME)  HOME=$HOME (not redirected)")
fi

if [[ "${TMPDIR:-}" == *".sandbox-tmp"* ]]; then
    RESULTS+=("PASS (sandboxed) TMPDIR points to .sandbox-tmp")
else
    RESULTS+=("INFO (raw TMPDIR) TMPDIR=${TMPDIR:-<unset>} (not redirected)")
fi

# ── Summary ──────────────────────────────────────────────────────
echo ""
echo "=== Results ==="
for r in "${RESULTS[@]}"; do
    echo "  $r"
done

blocked=0
escaped=0
for r in "${RESULTS[@]}"; do
    case "$r" in
        "PASS"*) ((blocked++)) ;;
        "FAIL"*) ((escaped++)) ;;
    esac
done

echo ""
echo "Blocked: $blocked   Escaped: $escaped"
echo "=== Done ==="

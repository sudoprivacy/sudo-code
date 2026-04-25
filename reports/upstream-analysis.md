# Upstream Analysis Report: claw-code vs sudo-code

**Date:** 2026-04-25
**Upstream:** `ultraworkers/claw-code` (remote: `upstream`)
**Fork point:** Commit `a389f8d` (2026-04-22, "session_store missing list_sessions")
**Last synced:** 2026-04-23 (per README)

---

## Executive Summary

Upstream `main` has **zero new commits** since our fork point. However, there is **significant in-flight work across 30+ feature branches** that has not yet been merged to `upstream/main`. The most active development period is 2026-04-22 to 2026-04-25, with a systematic "Jobdori" issue-tracking methodology driving CLI hardening.

Our fork (`sudo-code`) has **14 merge commits** ahead of the fork point, focused on branding, ACP transport, multi-provider auth, and config-driven CLI.

**No new Rust crates have been added upstream.** The workspace structure (9 crates) is identical across all upstream branches.

---

## Upstream Branch Activity (Since Fork)

### Tier 1 — High-Impact Branches (Recommend Adoption)

| Branch | Date | Summary |
|--------|------|---------|
| `feat/134-135-session-identity` | Apr 21 | **Major refactor:** Extracts `session_identity` module, adds boot-scoped session IDs to lane events, removes ~1200 lines from `main.rs` (code extraction), simplifies MCP/plugin error handling |
| `feat/provider-routing-parity` | Apr 8 | **OAuth + Provider routing:** Adds OAuth flow helpers (`generate_pkce_pair`, `parse_oauth_callback`), `oauth_token_is_expired` check, cleans up provider imports. Removes ~90 lines of ModelProvenance/ModelSource that we should also drop |
| `feat/jobdori-251-session-dispatch` | Apr 23 | **Session CLI commands:** Adds `ListSessions`, `LoadSession`, `DeleteSession`, `FlushTranscript` as `CliAction` variants dispatched before credential check |

### Tier 2 — CLI Hardening (Nice to Have)

| Branch | Date | Summary |
|--------|------|---------|
| `feat/jobdori-168c-emission-routing` | Apr 25 | **Latest tip.** Error envelopes route to stdout (not stderr) under `--output-format json`. Adds ROADMAP entries #200-#204 for token usage gaps |
| `feat/jobdori-247-classify-prompt-errors` | Apr 22 | Expands `classify_error_kind()` with 8+ new patterns: `cli_parse` for empty prompts, unsupported values, missing values, invalid flags, slash-command-requires-repl |
| `feat/jobdori-130b-filesystem-context` | Apr 23 | `contextualize_io_error()` wraps `io::Error` with operation + path context |
| `feat/jobdori-130c/d/e-*-help` | Apr 23 | Help flag (`--help`/`-h`) routed correctly for `diff`, `config`, `plugins`, `prompt`, `help`, `submit`, `resume` commands |
| `feat/jobdori-152-*-suffix-guard` | Apr 23 | `claw init` and `claw bootstrap-plan` reject trailing arguments |
| `feat/jobdori-122-doctor-stale-base` | Apr 23 | `claw doctor` now checks stale-base condition |
| `feat/jobdori-122b-doctor-broad-cwd` | Apr 23 | `claw doctor` warns when cwd is `/` or `$HOME` |
| `feat/jobdori-249-resumed-slash-kind` | Apr 23 | Adds `kind` + `hint` to resumed-session slash error JSON envelopes |

### Tier 3 — Infrastructure/Docs

| Branch | Date | Summary |
|--------|------|---------|
| `claw-code-issue-188k-brand-redesign` | Apr 23 | README celebrates 188K stars, adds star-history chart |
| `dev/rust` | Apr 8 | Provider/auth support matrix docs, improved `MissingApiKey` error copy. Also large cleanup: removes `telemetry` crate contents, `pdf_extract`, `lane_completion`, mock parity scripts |
| `feat/batch3-all` | Apr 7 | Documents phantom completion root cause, adds `workspace_root` to session |

---

## Architectural Changes Worth Adopting

### 1. Session Identity Module (Priority: HIGH)
**Branch:** `feat/134-135-session-identity`

New module: `rust/crates/runtime/src/session_identity.rs`
- Boot-scoped session ID generation (`boot-{hash}`)
- `begin_session()` / `end_session()` / `is_active_session()` lifecycle
- `CLAW_SESSION_ID` env var override support
- Session ID threaded into `LaneEvent` via `with_session_id()` builder method

**Impact on sudo-code:** We should adopt this for ACP session correlation. Rename env var to `SCODE_SESSION_ID`.

### 2. CLI main.rs Extraction (Priority: HIGH)
**Branch:** `feat/134-135-session-identity`

The branch removes ~1200 lines from `main.rs` by:
- Dropping `ModelProvenance` / `ModelSource` structs (we already handle this differently via config)
- Simplifying MCP error handling (removing degraded-mode fallbacks in commands crate)
- Removing `json_tag()`, `artifacts_with_status()`, `artifact_json_entries()` from init.rs

**Impact on sudo-code:** Our `main.rs` likely has these same patterns. We should evaluate whether our config-driven approach already supersedes these.

### 3. Session Management CLI Actions (Priority: MEDIUM)
**Branch:** `feat/jobdori-251-session-dispatch`

Adds direct dispatch for session-management verbs (`list-sessions`, `load-session`, `delete-session`, `flush-transcript`) at parser level, bypassing credential checks since they're pure-local operations.

**Impact on sudo-code:** We have `session_control.rs` with `list_managed_sessions_for` and `load_managed_session_for` — upstream is wiring these into CLI dispatch, which we should too.

### 4. Error Classification Expansion (Priority: MEDIUM)
**Branch:** `feat/jobdori-247-classify-prompt-errors` + stacked branches

`classify_error_kind()` grows from ~10 patterns to ~20+ patterns covering:
- `cli_parse` for empty prompts, unsupported/missing/invalid flag values
- `slash_command_requires_repl` for interactive-only commands invoked outside REPL
- `filesystem_io_error` for enriched I/O errors
- JSON hint synthesis when text-mode has hints but JSON mode doesn't

**Impact on sudo-code:** Our error handling should adopt these patterns for headless/ACP operation where machine-readable errors matter.

### 5. Build.rs Worktree Fix (Priority: LOW)
**Branch:** `feat/jobdori-168c-emission-routing`

`build.rs` now resolves `.git/HEAD` correctly in worktree environments by parsing the `.git` pointer file.

**Impact on sudo-code:** We use Hydra worktrees extensively — this fix prevents unnecessary rebuilds.

### 6. OAuth Infrastructure (Priority: MEDIUM)
**Branch:** `feat/provider-routing-parity`

New runtime exports: `OAuthConfig`, `OAuthAuthorizationRequest`, `OAuthTokenExchangeRequest`, `generate_pkce_pair`, `generate_state`, `parse_oauth_callback_request_target`, `oauth_token_is_expired`, `clear_oauth_credentials`, `save_oauth_credentials`.

**Impact on sudo-code:** We already have `scode login` but should check if upstream's OAuth primitives are better factored.

---

## New Files/Modules Added Upstream (Across All Branches)

### Rust
| File | Branch | Purpose |
|------|--------|---------|
| `rust/crates/runtime/src/session_identity.rs` | feat/134-135 | Boot-scoped session identity |
| `rust/crates/rusty-claude-cli/tests/output_format_contract.rs` | Multiple | JSON output contract tests (+484 lines on most advanced branch) |

### Documentation
| File | Branch | Purpose |
|------|--------|---------|
| `ROADMAP.md` | Multiple | 12,463-line detailed roadmap with issue tracking |
| `SCHEMAS.md` | jobdori-168c | JSON envelope schema contract (v1.0 -> v2.0 target) |
| `ERROR_HANDLING.md` | jobdori-168c | Error classification and handling guide |
| `FIX_LOCUS_164.md` | jobdori-168c | Migration plan for JSON envelope v2.0 |
| `MERGE_CHECKLIST.md` | jobdori-168c | Branch merge verification checklist |
| `OPT_OUT_AUDIT.md` | jobdori-168c | Opt-out surface audit |

---

## Crate Structure Comparison

**No new crates added.** Both upstream and our fork have identical workspace members:

```
rust/crates/api
rust/crates/commands
rust/crates/compat-harness
rust/crates/mock-anthropic-service
rust/crates/plugins
rust/crates/runtime
rust/crates/rusty-claude-cli
rust/crates/telemetry
rust/crates/tools
```

Note: `dev/rust` branch removes `telemetry` and `mock-anthropic-service` crate contents and deletes `pdf_extract.rs` + `lane_completion.rs` from tools — a major cleanup that hasn't been merged to upstream/main yet.

---

## Our Divergence (14 Commits Ahead)

| PR | Summary |
|----|---------|
| #1 feat/branding-sudo-code | Initial rebrand from claw-code to sudo-code |
| #5 feat/acp-transport | ACP transport layer |
| #7 feat/acp-observer | ACP observer pattern |
| #8 feat/acp-cli-server-v3 | ACP CLI server implementation |
| #9 feat/sudocode-rebrand-final | Final rebrand cleanup |
| #10 chore/surgical-cleanup | Code cleanup |
| #11 debug/session-paths | Session path debugging |
| #12 refactor/drop-legacy-session-compat | Remove legacy session compat |
| #13 fix/tui-newline-trimming | TUI newline fix |
| #14 chore/cleanup-documentation | Doc cleanup |
| #16 feat/scode-login | `scode login` command |
| #21 refactor/runtime-config-struct | Runtime config refactoring |
| #23 feat/config-driven-providers | Config-driven multi-provider support |
| #25 fix/acp-server-lints | ACP server lint fixes |

---

## Recommendations

### Immediate (Cherry-pick or Adapt)
1. **Build.rs worktree fix** — Single-file change, no conflicts expected
2. **Session identity module** — Clean module, rename `CLAW_SESSION_ID` -> `SCODE_SESSION_ID`
3. **Error classification expansion** — Merge new `classify_error_kind()` patterns

### Short-term (Next Sync)
4. **Session management CLI dispatch** — Wire `list-sessions`/`load-session` into our CLI
5. **JSON emission routing** — Error envelopes to stdout under `--output-format json`
6. **SCHEMAS.md adoption** — Adapt their JSON contract spec for our CLI surface

### Evaluate
7. **OAuth primitives** — Compare with our existing `scode login` implementation
8. **`dev/rust` cleanup** — Removing telemetry crate contents, pdf_extract, lane_completion. Check if we still use any of these.
9. **ROADMAP.md** — Their 12K-line roadmap is comprehensive; extract relevant items for our own backlog

### Skip
- Brand redesign (we have our own branding)
- Help flag routing fixes (low-priority UX polish, import if/when needed)
- Opt-out audit docs (claw-code specific compliance)

# Refactoring Analysis: `main.rs` (13,529 lines)

**File:** `rust/crates/rusty-claude-cli/src/main.rs`
**Date:** 2026-04-24
**Branch:** `feat/cli-refactor-analysis`

---

## 1. Major Logical Blocks

| # | Block | Lines | Span | Description |
|---|-------|-------|------|-------------|
| A | **Types & Bootstrapping** | ~210 | 1–210 | Module declarations, imports, `ModelSource`, `ModelProvenance`, `main()` entry point |
| B | **Error Helpers** | ~80 | 261–340 | `classify_error_kind`, `split_error_hint`, `read_piped_stdin`, `merge_prompt_with_stdin` |
| C | **`run()` & CLI Dispatch** | ~155 | 344–496 | Top-level `run()` orchestrator that matches `CliAction` variants |
| D | **Argument Parsing** | ~860 | 497–1503 | `CliAction` enum (25 variants, 108 lines), `parse_args()` (437 lines), 30+ helper functions for parsing/validation/suggestions |
| E | **Model & Permission Resolution** | ~165 | 1504–1670 | Model alias resolution, permission mode parsing, config lookups |
| F | **Subcommand Execution** | ~465 | 1670–2110 | `format_connected_line`, `parse_system_prompt_args`, diagnostics (`Doctor`), `run_mcp_serve`, `run_worker_state` |
| G | **Health Checks** | ~365 | 2179–2582 | `check_auth_health` (120 lines), `check_config_health` (93 lines), `check_workspace_health` (72 lines), `check_sandbox_health` (63 lines), `check_system_health` |
| H | **Session Resume & Manifests** | ~400 | 2583–2990 | `dump_manifests`, `resume_session`, `run_resume_command` (345 lines) |
| I | **Formatting & Reports** | ~780 | 2990–3816 | 40+ `format_*` / `render_*` functions for model, permissions, auth, cost, resume, compact, git, status, sandbox, commit reports |
| J | **Core Structs & REPL Setup** | ~320 | 3816–4163 | `LiveCli`, `RuntimeConfig`, `BuiltRuntime`, `RuntimeMcpState`, `AcpCliAgent`, `AcpCliSession`, `ScopedCurrentDir`, `run_repl()` |
| K | **MCP Integration** | ~200 | 4163–4460 | `RuntimeMcpState` impl (190 lines), MCP tool definition builders, `build_runtime_mcp_state` |
| L | **`LiveCli` Implementation** | ~1,043 | 4516–5559 | 45 methods across session mgmt, prompt execution, REPL command dispatch, state reporting, config changes, tool integration |
| M | **Session Management** | ~200 | 5559–5755 | 15 standalone functions: `sessions_dir`, `new_cli_session`, `list_managed_sessions`, `render_session_list`, etc. |
| N | **Status, Help & Config Rendering** | ~475 | 5755–6233 | `render_repl_help`, `print_status_snapshot`, `format_status_report` (95 lines), `render_help_topic` (86 lines) |
| O | **Config, Memory, Init, Diff, Export** | ~600 | 6233–6900 | `render_config_report`, `render_memory_report`, `run_init`, `render_diff_report`, `render_teleport_report`, history formatting |
| P | **Export & Session Markdown** | ~300 | 6900–7260 | `render_session_markdown` (90 lines), `run_export`, `default_export_filename`, `summarize_tool_payload_for_markdown` |
| Q | **Progress Reporting** | ~260 | 7260–7530 | `InternalPromptProgressReporter`, `InternalPromptProgressRun`, `format_internal_prompt_progress_line`, `describe_tool_progress` |
| R | **Runtime Building** | ~260 | 7530–7810 | `build_runtime`, `build_runtime_for_cwd`, `build_runtime_with_plugin_state`, `build_runtime_plugin_state` |
| S | **API Client** | ~230 | 7738–8040 | `AnthropicRuntimeClient` struct (10 fields), `ApiClient` impl, auth source resolution |
| T | **Stream Processing & Output** | ~490 | 8040–8560 | `format_tool_call_start`, `format_bash_result`, `format_read_result`, `format_edit_result`, `format_grep_result`, `format_glob_result`, 20+ tool output formatters |
| U | **Tool Executor** | ~220 | 8560–8900 | `CliToolExecutor` struct, `ToolExecutor` impl, `push_output_block`, `response_to_events` |
| V | **Message Conversion & Help** | ~230 | 8900–9318 | `convert_messages`, `permission_policy`, `print_help_to` (160 lines), `print_help` |
| W | **Test Module (main)** | ~3,976 | 9320–13,394 | `mod tests` — 100+ imported functions, comprehensive unit tests |
| X | **Test Module (sandbox)** | ~55 | 13,396–13,450 | `mod sandbox_report_tests` — 3 tests |
| Y | **Test Module (manifests)** | ~79 | 13,452–13,529 | `mod dump_manifests_tests` — 2 tests |

**Summary:**
- Production code: ~9,318 lines (69%)
- Test code: ~4,211 lines (31%)

---

## 2. Recommended Module Extraction

### Tier 1 — High Impact, Low Risk (pure functions, no shared state)

| Proposed Module | Source Lines | Functions to Move | Rationale |
|-----------------|-------------|-------------------|-----------|
| **`cli/args.rs`** | 497–1503 (~1,007 lines) | `CliAction` enum, `parse_args()`, `parse_local_help_action`, `parse_acp_args`, `parse_export_args`, `parse_resume_args`, `parse_dump_manifests_args`, `parse_system_prompt_args`, `parse_direct_slash_cli_action`, `parse_single_word_command_alias`, `join_optional_args`, `is_help_flag`, all `format_unknown_*` helpers, `suggest_*` functions, `levenshtein_distance`, `ranked_suggestions` | Pure argument parsing with zero runtime dependencies. Self-contained. Currently the largest single concern. |
| **`cli/format.rs`** | 2990–3190 + 6043–6110 + 8411–8800 (~950 lines) | All `format_*` functions: model/auth/permissions reports, tool call/result formatting, `format_bash_result`, `format_read_result`, `format_edit_result`, `format_grep_result`, `format_glob_result`, `format_generic_tool_result`, `truncate_*` helpers | Pure formatting — takes data in, returns `String`. No side effects. Easiest to extract. |
| **`cli/session.rs`** | 5559–5755 (~200 lines) | `sessions_dir`, `current_session_store`, `new_cli_session`, `create_managed_session_handle`, `resolve_session_reference`, `list_managed_sessions`, `latest_managed_session`, `load_session_reference`, `delete_managed_session`, `render_session_list`, `format_session_modified_age`, `write_session_clear_backup` | Standalone session CRUD. Only depends on `runtime::SessionStore`. |
| **`cli/doctor.rs`** | 1881–2582 (~700 lines) | `DiagnosticLevel`, `DiagnosticCheck`, `DoctorReport`, all `check_*_health` functions, `render_diagnostic_check`, `render_doctor_report`, `run_doctor` | Self-contained diagnostic subsystem with its own types. |

### Tier 2 — Medium Impact, Moderate Coupling

| Proposed Module | Source Lines | What to Move | Coupling Notes |
|-----------------|-------------|--------------|----------------|
| **`cli/status.rs`** | 5778–6043 + 2920–2990 (~370 lines) | `StatusContext`, `StatusUsage`, `print_status_snapshot`, `status_json_value`, `status_context`, `format_status_report` | Depends on `GitWorkspaceSummary`, `RuntimeConfig`. Need to define trait or pass context struct. |
| **`cli/export.rs`** | 6900–7170 (~270 lines) | `render_session_markdown`, `run_export`, `default_export_filename`, `resolve_export_path`, `summarize_tool_payload_for_markdown`, `short_tool_id` | Depends on `Session` type only. Clean boundary. |
| **`cli/git.rs`** | 3188–3650 (~460 lines) | `parse_git_*` functions, `resolve_git_branch_for`, `run_git_capture_in`, `find_git_root_in`, `GitWorkspaceSummary` | Used by status, commit, and resume. Shared type. |
| **`cli/help.rs`** | 5755–5778 + 6147–6233 + 9143–9318 (~310 lines) | `render_repl_help`, `render_help_topic`, `print_help_to`, `print_help`, `LocalHelpTopic` | Mostly static text generation. `print_help_to` is 160 lines of help text. |
| **`cli/mcp.rs`** | 4139–4460 + part of 4163 impl (~320 lines) | `ToolSearchRequest`, `McpToolRequest`, `RuntimeMcpState` impl, `build_runtime_mcp_state`, `mcp_runtime_tool_definition`, `mcp_wrapper_tool_definitions`, `permission_mode_for_mcp_tool` | Tightly coupled to `McpServerManager` but forms a coherent subsystem. |

### Tier 3 — High Impact, High Risk (stateful objects)

| Proposed Module | Source Lines | What to Move | Challenge |
|-----------------|-------------|--------------|-----------|
| **`cli/live.rs`** | 4516–5559 (~1,043 lines) | `LiveCli` impl block | God Object — 45 methods across 8 concerns. Should be decomposed first (see Section 4). |
| **`cli/api_client.rs`** | 7738–8040 (~300 lines) | `AnthropicRuntimeClient`, `ApiClient` impl, auth resolution | References `RuntimeConfig`, `AllowedToolSet`, progress reporter. |
| **`cli/tool_executor.rs`** | 8560–8900 (~340 lines) | `CliToolExecutor`, `ToolExecutor` impl | Depends on `RuntimeMcpState`, `GlobalToolRegistry`, formatting functions. |

---

## 3. Test Migration Strategy

### Current State
- **Main test module** (`mod tests`): lines 9320–13,394 = **4,074 lines**, 100+ imported symbols
- **`sandbox_report_tests`**: lines 13,396–13,450 = 55 lines
- **`dump_manifests_tests`**: lines 13,452–13,529 = 78 lines
- Total test code: **~4,207 lines** (31% of file)

### Recommended Approach: Phased Migration

**Phase 1: Co-locate tests with extracted modules**

As modules are extracted (Tier 1 first), move their corresponding tests into `#[cfg(test)] mod tests` within each new module file. This is the idiomatic Rust approach and eliminates the need for `use super::*` imports spanning the entire file.

Example mapping:
| Test Group | Target Module | Approx Lines |
|------------|---------------|--------------|
| `parse_args` tests | `cli/args.rs` | ~800 |
| `format_*` tests | `cli/format.rs` | ~600 |
| Session tests | `cli/session.rs` | ~200 |
| Doctor/health tests | `cli/doctor.rs` | ~150 |
| Status tests | `cli/status.rs` | ~100 |
| Export/markdown tests | `cli/export.rs` | ~150 |
| Git parsing tests | `cli/git.rs` | ~200 |

**Phase 2: Integration tests to `tests/` directory**

Tests that exercise cross-module interactions (e.g., `build_runtime_with_plugin_state` tests, MCP server fixture tests, full `run_resume_command` tests) should move to `tests/cli_integration.rs`:

```
rust/crates/rusty-claude-cli/
├── src/
│   ├── main.rs          (entry point + run() + residual ~2,000 lines)
│   ├── cli/
│   │   ├── mod.rs
│   │   ├── args.rs
│   │   ├── format.rs
│   │   ├── session.rs
│   │   ├── doctor.rs
│   │   ├── status.rs
│   │   ├── export.rs
│   │   ├── git.rs
│   │   ├── help.rs
│   │   ├── mcp.rs
│   │   ├── live.rs
│   │   ├── api_client.rs
│   │   └── tool_executor.rs
│   ├── init.rs           (existing)
│   ├── input.rs          (existing)
│   └── render.rs         (existing)
└── tests/
    └── cli_integration.rs  (~800 lines of cross-module tests)
```

**Phase 3: Remaining `main.rs` tests**

After Tiers 1–3, the main `mod tests` should shrink to <500 lines covering only the residual `run()` dispatch logic.

### Visibility Considerations

Currently all functions are `fn` (private to the crate). To support `tests/` directory tests, key functions need `pub(crate)` visibility. The module extraction naturally handles this — each module exports its public interface.

---

## 4. God Object Analysis & Tight Coupling

### Primary God Object: `LiveCli` (45 methods, 8 concerns)

**Definition:** lines 3832–3837 (4 fields)
**Implementation:** lines 4516–5559 (1,043 lines)

```rust
struct LiveCli {
    config: RuntimeConfig,
    runtime: BuiltRuntime,
    session: SessionHandle,
    prompt_history: Vec<PromptHistoryEntry>,
}
```

**Responsibility decomposition:**

| Concern | Methods | Lines | Proposed Extraction |
|---------|---------|-------|---------------------|
| Prompt execution | `run_turn`, `run_turn_with_output`, `run_prompt_compact`, `run_prompt_compact_json`, `run_prompt_json` | ~100 | Keep in `LiveCli` — core responsibility |
| Session lifecycle | `clear_session`, `resume_session`, `persist_session`, `record_prompt_history`, `print_prompt_history` | ~80 | Extract to standalone functions taking `&mut Session` |
| REPL command dispatch | `handle_repl_command` (167 lines!), `handle_session_command` (111 lines!), `handle_plugins_command` | ~300 | Extract to `cli/repl_commands.rs` — largest single method |
| State reporting | `print_status`, `print_cost`, `print_config`, `print_memory`, `print_agents`, `print_mcp`, `print_skills`, `print_plugins`, `startup_banner` | ~200 | Delegate to `cli/status.rs` and `cli/format.rs` |
| Configuration | `set_model`, `set_permissions`, `set_auth` | ~50 | Keep in `LiveCli` — thin wrappers |
| Tool integration | `run_commit`, `run_pr`, `run_issue`, `run_bughunter` | ~60 | Extract to `cli/commands.rs` |
| Feature ops | `reload_runtime_features`, `compact`, `run_debug_tool_call` | ~80 | Keep — runtime lifecycle |
| Specialized ops | `run_internal_prompt_text*`, `run_teleport`, `run_ultraplan`, `export_session`, `set_reasoning_effort`, `repl_completion_candidates` | ~170 | Extract internal prompt helpers |

**Key coupling issue:** `handle_repl_command` is a 167-line match statement that dispatches to 25+ slash commands. Each branch directly calls methods on `self`, making it impossible to test individual commands in isolation.

**Recommended refactoring:** Convert to a dispatch table or trait-based command pattern:
```rust
// Before: monolithic match in LiveCli
fn handle_repl_command(&mut self, cmd: &str) -> Result<...> {
    match cmd {
        "/model" => { ... 15 lines ... }
        "/status" => { ... 10 lines ... }
        // ... 25 more arms
    }
}

// After: dispatch to focused handlers
fn handle_repl_command(&mut self, cmd: &str) -> Result<...> {
    let (name, args) = split_command(cmd);
    repl_commands::dispatch(self, name, args)
}
```

### Secondary Concern: `parse_args()` (437 lines)

A single function handling all CLI subcommand parsing. While not a struct-level God Object, it has God Function characteristics:
- 25+ code paths (one per `CliAction` variant)
- Complex flag accumulation with multiple mutable variables
- Difficult to test individual subcommand parsing in isolation

**Recommendation:** Split into per-subcommand parsers called from a thin dispatcher.

### Tertiary Concern: `AnthropicRuntimeClient` (10 fields)

Not a true God Object but has mixed responsibilities:
- API client configuration (model, auth, tools)
- Async runtime management (`tokio::runtime::Runtime`)
- Stream consumption with retry logic
- Progress reporting

**Recommendation:** The async runtime ownership is the main design smell. Consider injecting a shared `tokio::Runtime` instead of each client owning one.

### Cross-Cutting Coupling Points

| Pattern | Instances | Impact |
|---------|-----------|--------|
| Direct `env::current_dir()` calls | ~15 | Makes functions untestable without `chdir`. Should inject `cwd: &Path`. |
| `eprintln!` / `println!` in business logic | ~40 | Side effects mixed with logic. Should return values, let caller decide output. |
| `std::process::exit()` calls | ~5 | Prevents graceful error propagation. Should return `Result`. |
| Global `ConfigLoader::load()` calls | ~8 | Hidden dependency on filesystem. Should inject config. |

---

## 5. Recommended Execution Order

### Sprint 1: Risk-free extractions (est. ~3,000 lines moved)
1. Extract `cli/format.rs` — ~950 lines of pure functions
2. Extract `cli/args.rs` — ~1,007 lines of parsing logic
3. Extract `cli/session.rs` — ~200 lines of session CRUD
4. Extract `cli/doctor.rs` — ~700 lines of diagnostics
5. Move corresponding tests with each module

### Sprint 2: Stateful module extractions (~1,700 lines moved)
6. Extract `cli/git.rs` — ~460 lines
7. Extract `cli/status.rs` — ~370 lines
8. Extract `cli/export.rs` — ~270 lines
9. Extract `cli/help.rs` — ~310 lines
10. Extract `cli/mcp.rs` — ~320 lines

### Sprint 3: God Object decomposition (~1,700 lines moved)
11. Extract `handle_repl_command` dispatch to `cli/repl_commands.rs`
12. Extract `cli/api_client.rs` — `AnthropicRuntimeClient`
13. Extract `cli/tool_executor.rs` — `CliToolExecutor`
14. Slim `LiveCli` to core lifecycle methods only
15. Move integration tests to `tests/cli_integration.rs`

### Post-refactor target

| File | Lines (est.) |
|------|--------------|
| `main.rs` | ~1,500 (entry point, `run()`, `LiveCli` core, `run_repl`) |
| `cli/args.rs` | ~1,000 |
| `cli/format.rs` | ~950 |
| `cli/doctor.rs` | ~700 |
| `cli/git.rs` | ~460 |
| `cli/status.rs` | ~370 |
| `cli/mcp.rs` | ~320 |
| `cli/help.rs` | ~310 |
| `cli/api_client.rs` | ~300 |
| `cli/repl_commands.rs` | ~300 |
| `cli/tool_executor.rs` | ~340 |
| `cli/export.rs` | ~270 |
| `cli/session.rs` | ~200 |
| `tests/cli_integration.rs` | ~800 |
| **Total** | **~7,820** (production) + **~5,709** (tests co-located + integration) |

No line is duplicated — the total remains ~13,529 but distributed across 14 files averaging ~960 lines each instead of one 13,529-line monolith.

---

## 6. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Breaking `use super::*` imports in test module | Move tests alongside their code. Compile after each extraction. |
| Circular dependencies between new modules | `cli/format.rs` has zero deps on other cli modules. Extract it first as a proof of concept. |
| Visibility changes (`fn` → `pub(crate) fn`) | Keep all new modules under `cli/` submodule to maintain crate-private visibility. |
| Large merge conflicts with concurrent work | Extract one module per PR. Each PR should be independently reviewable and mergeable. |
| Runtime behavior regression | The existing 4,207 lines of tests serve as a regression safety net. Run `cargo test --workspace` after each extraction. |

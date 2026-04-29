# SudoCode Continuous Health Monitoring (CHM)

A lightweight, speed-first health scanning system for the SudoCode Rust workspace.
It produces a JSON snapshot of codebase health and renders an actionable Markdown dashboard.

## Quick Start

```bash
cd rust/

# 1. Run the scan (produces target/chm/snapshot.json)
./scripts/chm_scan.sh

# 2. Render the dashboard (produces target/chm/dashboard.md)
./scripts/chm_report.sh
```

## What Gets Collected

### Lint Collection (`cargo clippy`)

Runs `cargo clippy --workspace --all-targets --message-format=json` and counts:

| Field | Description |
|-------|-------------|
| `lints.warnings` | Total clippy warning count |
| `lints.errors` | Total clippy error count |
| `lints.top_codes` | Most frequent lint codes with occurrence counts |

**Why it matters:** Lint warnings accumulate silently. Tracking them over time reveals whether the codebase is getting cleaner or dirtier.

### Volume Metrics (`tokei` / `wc -l`)

Uses [tokei](https://github.com/XAMPPRocky/tokei) for fast, accurate line counting (falls back to `wc -l` if tokei is not installed).

| Field | Description |
|-------|-------------|
| `volume.rust_lines` | Lines of Rust code (excluding comments/blanks) |
| `volume.total_files` | Number of source files |
| `volume.total_lines` | Total lines across all languages |

**Why it matters:** Rapid code growth without proportional test growth signals risk.

### Monolith Watch (large file detection)

Scans all `.rs` files and flags any exceeding the threshold (default: 500 LOC).

| Field | Description |
|-------|-------------|
| `monolith_watch.threshold_loc` | Current threshold (configurable via `CHM_MONOLITH_THRESHOLD`) |
| `monolith_watch.count` | Number of files above threshold |
| `monolith_watch.files` | Array of `{lines, file}` objects |

**Why it matters:** Large files are harder to review, test, and compile. They are the top refactor candidates.

### Speed Watch (compile-time)

Measures `cargo check --workspace` wall-clock time.

| Field | Description |
|-------|-------------|
| `speed_watch.incremental_check_ms` | Incremental check time in milliseconds |

| Range | Assessment |
|-------|------------|
| < 5 s | Excellent |
| 5–15 s | Acceptable |
| > 15 s | Investigate with `cargo build --timings` |

**Why it matters:** Compile time is the tightest feedback loop in Rust development. Degradation directly impacts velocity.

### Test Inventory

Counts `#[test]` annotations across the workspace.

| Field | Description |
|-------|-------------|
| `tests.test_count` | Total number of `#[test]` functions |
| `tests.test_file_count` | Number of files containing tests |

### Coverage (`cargo-tarpaulin`, optional)

If [cargo-tarpaulin](https://github.com/xd009642/tarpaulin) is installed, collects line coverage.

| Field | Description |
|-------|-------------|
| `coverage.line_pct` | Overall line coverage percentage (null if tarpaulin unavailable) |
| `coverage.uncovered_files` | Files with 0% coverage — top candidates for new tests |

Install tarpaulin: `cargo install cargo-tarpaulin`

## Configuration

All settings are via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `CHM_OUT_DIR` | `target/chm` | Output directory for snapshot and reports |
| `CHM_MONOLITH_THRESHOLD` | `500` | LOC threshold for monolith detection |
| `CHM_REPORT_OUT` | `target/chm/dashboard.md` | Output path for rendered dashboard |

## Output Files

```
target/chm/
├── snapshot.json       # Machine-readable health snapshot
├── dashboard.md        # Human-readable Markdown report
├── clippy_raw.txt      # Raw clippy JSON output
├── clippy.json         # Parsed clippy summary (if applicable)
└── volume.json         # Raw tokei/volume output
```

## JSON Snapshot Schema

```json
{
  "timestamp": "2026-04-29T12:00:00Z",
  "workspace": "/path/to/rust",
  "lints": {
    "warnings": 12,
    "errors": 0,
    "top_codes": [{"code": "clippy::needless_pass_by_value", "count": 3}]
  },
  "volume": {
    "rust_lines": 8500,
    "total_files": 95,
    "total_lines": 10200
  },
  "monolith_watch": {
    "threshold_loc": 500,
    "count": 2,
    "files": [{"lines": 680, "file": "crates/runtime/src/lib.rs"}]
  },
  "speed_watch": {
    "incremental_check_ms": 3200
  },
  "tests": {
    "test_count": 45,
    "test_file_count": 12
  },
  "coverage": {
    "line_pct": 62.5,
    "uncovered_files": ["crates/tools/src/pdf_extract.rs"]
  }
}
```

## Integration Ideas

- **CI gate:** Fail the build if `lints.errors > 0` or `monolith_watch.count` increases.
- **Trend tracking:** Commit `snapshot.json` to a dedicated branch and plot metrics over time.
- **PR checks:** Compare snapshot before/after a PR to catch regressions.
- **Maintenance worker:** Feed the snapshot to an LLM with the prompt template below to generate a prioritised action plan.

## Maintenance Worker (LLM Prompt Template)

The file `scripts/chm_maintenance_prompt.md` contains a prompt template that accepts
the JSON snapshot and produces a prioritised, human-readable maintenance report.

Usage with any LLM:

```
Paste the contents of chm_maintenance_prompt.md, then append:

SNAPSHOT:
<paste target/chm/snapshot.json>
```

## Dependencies

| Tool | Required | Install |
|------|----------|---------|
| `cargo` + `clippy` | Yes | Included with rustup |
| `jq` | Yes | `brew install jq` / `apt install jq` |
| `tokei` | Recommended | `cargo install tokei` |
| `cargo-tarpaulin` | Optional | `cargo install cargo-tarpaulin` |

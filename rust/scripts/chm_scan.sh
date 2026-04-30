#!/usr/bin/env bash
# SudoCode Continuous Health Monitoring (CHM) — scan script
# Collects lints, volume metrics, and optional coverage into a JSON snapshot.
# Optimised for SPEED: each collector runs with minimal overhead.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_DIR="${CHM_OUT_DIR:-$WORKSPACE_ROOT/target/chm}"
TIMESTAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
SNAPSHOT="$OUT_DIR/snapshot.json"

mkdir -p "$OUT_DIR"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
info()  { printf '\033[1;34m[CHM]\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33m[CHM]\033[0m %s\n' "$*" >&2; }

# Cross-platform millisecond timer (macOS date lacks %N)
now_ms() {
  python3 -c 'import time; print(int(time.time()*1000))' 2>/dev/null \
    || perl -MTime::HiRes=time -e 'printf "%d\n", time()*1000' 2>/dev/null \
    || echo $(( $(date +%s) * 1000 ))
}

# ---------------------------------------------------------------------------
# 1. Lint collection  (cargo clippy)
# ---------------------------------------------------------------------------
info "Collecting clippy lints..."
CLIPPY_RAW="$OUT_DIR/clippy_raw.txt"
CLIPPY_JSON="$OUT_DIR/clippy.json"

# Run clippy in message-format=json for machine-readable output.
# We intentionally allow clippy to "fail" (warnings count as exit-code 0 in
# message-format mode) and capture everything.
(cd "$WORKSPACE_ROOT" && cargo clippy --workspace --all-targets --message-format=json 2>&1) \
  > "$CLIPPY_RAW" || true

# Parse: count warnings and errors, extract top-level diagnostics
LINT_WARNINGS=0
LINT_ERRORS=0
LINT_DETAILS="[]"

if [ -s "$CLIPPY_RAW" ]; then
  LINT_WARNINGS=$(grep -c '"level":"warning"' "$CLIPPY_RAW" || true)
  LINT_ERRORS=$(grep -c '"level":"error"' "$CLIPPY_RAW" || true)
  # Ensure we got a number, not empty
  LINT_WARNINGS="${LINT_WARNINGS:-0}"
  LINT_ERRORS="${LINT_ERRORS:-0}"

  # Build a small array of the first 50 unique lint codes (allow pipeline failures)
  LINT_DETAILS=$(set +o pipefail; grep '"reason":"compiler-message"' "$CLIPPY_RAW" \
    | jq -r 'select(.message.code != null) | .message.code.code' 2>/dev/null \
    | sort | uniq -c | sort -rn | head -50 \
    | awk '{printf "{\"code\":\"%s\",\"count\":%d},", $2, $1}' \
    | sed 's/,$//' | awk '{printf "[%s]", $0}') || true
  [ -z "$LINT_DETAILS" ] && LINT_DETAILS="[]"
fi

info "  warnings=$LINT_WARNINGS  errors=$LINT_ERRORS"

# ---------------------------------------------------------------------------
# 2. Volume metrics  (tokei or fallback wc -l)
# ---------------------------------------------------------------------------
info "Collecting volume metrics..."
VOLUME_JSON="$OUT_DIR/volume.json"

if command -v tokei &>/dev/null; then
  (cd "$WORKSPACE_ROOT" && tokei --output json .) > "$VOLUME_JSON" 2>/dev/null
  TOTAL_LINES=$(jq '.Total.code // 0' "$VOLUME_JSON" 2>/dev/null || echo 0)
  TOTAL_FILES=$(jq '[.[] | select(type=="object") | .reports // [] | length] | add // 0' "$VOLUME_JSON" 2>/dev/null || echo 0)
  RUST_LINES=$(jq '.Rust.code // 0' "$VOLUME_JSON" 2>/dev/null || echo 0)
else
  warn "tokei not found — falling back to wc -l"
  TOTAL_FILES=$(find "$WORKSPACE_ROOT/crates" -name '*.rs' | wc -l | tr -d ' ')
  TOTAL_LINES=$(find "$WORKSPACE_ROOT/crates" -name '*.rs' -exec cat {} + | wc -l | tr -d ' ')
  RUST_LINES="$TOTAL_LINES"
  echo "{\"fallback\":true,\"rust_lines\":$RUST_LINES,\"total_files\":$TOTAL_FILES}" > "$VOLUME_JSON"
fi

info "  rust_lines=$RUST_LINES  total_files=$TOTAL_FILES"

# ---------------------------------------------------------------------------
# 3. Monolith watch — large files (>500 LOC)
# ---------------------------------------------------------------------------
info "Scanning for monolith files (>500 LOC)..."
MONOLITH_THRESHOLD="${CHM_MONOLITH_THRESHOLD:-500}"
MONOLITHS="[]"

MONOLITHS=$(find "$WORKSPACE_ROOT/crates" -name '*.rs' -exec wc -l {} + 2>/dev/null \
  | grep -v ' total$' \
  | awk -v thresh="$MONOLITH_THRESHOLD" '$1 > thresh {printf "{\"lines\":%d,\"file\":\"%s\"},", $1, $2}' \
  | sed 's/,$//' | awk '{printf "[%s]", $0}')
[ -z "$MONOLITHS" ] && MONOLITHS="[]"

MONOLITH_COUNT=$(echo "$MONOLITHS" | jq 'length' 2>/dev/null || echo 0)
info "  monolith_files=$MONOLITH_COUNT (threshold=${MONOLITH_THRESHOLD} LOC)"

# ---------------------------------------------------------------------------
# 4. Compile-time measurement (Speed Watch)
# ---------------------------------------------------------------------------
info "Measuring incremental compile time..."
COMPILE_START=$(now_ms)
(cd "$WORKSPACE_ROOT" && cargo check --workspace 2>/dev/null) || true
COMPILE_END=$(now_ms)
COMPILE_MS=$(( COMPILE_END - COMPILE_START ))
info "  incremental_check_ms=$COMPILE_MS"

# ---------------------------------------------------------------------------
# 5. Coverage (cargo tarpaulin — optional)
# ---------------------------------------------------------------------------
COVERAGE_PCT="null"
UNCOVERED_FILES="[]"

if command -v cargo-tarpaulin &>/dev/null; then
  info "Collecting coverage via cargo-tarpaulin..."
  TARPAULIN_JSON="$OUT_DIR/tarpaulin.json"
  (cd "$WORKSPACE_ROOT" && cargo tarpaulin --workspace --out Json --output-dir "$OUT_DIR" 2>/dev/null) || true

  if [ -f "$OUT_DIR/tarpaulin-report.json" ]; then
    mv "$OUT_DIR/tarpaulin-report.json" "$TARPAULIN_JSON" 2>/dev/null || true
  fi

  if [ -f "$TARPAULIN_JSON" ]; then
    COVERAGE_PCT=$(jq '.coverage // null' "$TARPAULIN_JSON" 2>/dev/null || echo null)
    # Files with 0% coverage — top refactor candidates
    UNCOVERED_FILES=$(jq '[.files // [] | .[] | select(.coverage == 0) | .path] | .[0:20]' \
      "$TARPAULIN_JSON" 2>/dev/null || echo "[]")
  fi
  info "  coverage=$COVERAGE_PCT%"
else
  warn "cargo-tarpaulin not found — skipping coverage"
fi

# ---------------------------------------------------------------------------
# 6. Test count
# ---------------------------------------------------------------------------
info "Counting tests..."
TEST_COUNT=$(grep -r '#\[test\]' "$WORKSPACE_ROOT/crates" --include='*.rs' 2>/dev/null | wc -l | tr -d ' ')
TEST_FILE_COUNT=$(grep -rl '#\[test\]' "$WORKSPACE_ROOT/crates" --include='*.rs' 2>/dev/null | wc -l | tr -d ' ')
info "  tests=$TEST_COUNT in $TEST_FILE_COUNT files"

# ---------------------------------------------------------------------------
# 7. Assemble JSON snapshot
# ---------------------------------------------------------------------------
info "Writing snapshot → $SNAPSHOT"

cat > "$SNAPSHOT" <<SNAPSHOT_EOF
{
  "timestamp": "$TIMESTAMP",
  "workspace": "$WORKSPACE_ROOT",
  "lints": {
    "warnings": $LINT_WARNINGS,
    "errors": $LINT_ERRORS,
    "top_codes": $LINT_DETAILS
  },
  "volume": {
    "rust_lines": $RUST_LINES,
    "total_files": $TOTAL_FILES,
    "total_lines": ${TOTAL_LINES:-0}
  },
  "monolith_watch": {
    "threshold_loc": $MONOLITH_THRESHOLD,
    "count": $MONOLITH_COUNT,
    "files": $MONOLITHS
  },
  "speed_watch": {
    "incremental_check_ms": $COMPILE_MS
  },
  "tests": {
    "test_count": $TEST_COUNT,
    "test_file_count": $TEST_FILE_COUNT
  },
  "coverage": {
    "line_pct": $COVERAGE_PCT,
    "uncovered_files": $UNCOVERED_FILES
  }
}
SNAPSHOT_EOF

# Pretty-print if jq is available
if command -v jq &>/dev/null; then
  jq '.' "$SNAPSHOT" > "$SNAPSHOT.tmp" && mv "$SNAPSHOT.tmp" "$SNAPSHOT"
fi

info "Done. Snapshot written to $SNAPSHOT"
info "Run: cat $SNAPSHOT | jq ."

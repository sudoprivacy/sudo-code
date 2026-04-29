#!/usr/bin/env bash
# Tests for the CHM (Continuous Health Monitoring) system.
# Validates that chm_scan.sh produces valid JSON with the expected schema
# and that chm_report.sh renders a valid Markdown dashboard.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TEST_OUT_DIR="$(mktemp -d)"
PASS=0
FAIL=0

cleanup() { rm -rf "$TEST_OUT_DIR"; }
trap cleanup EXIT

pass() { PASS=$((PASS + 1)); printf '  \033[1;32mPASS\033[0m %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); printf '  \033[1;31mFAIL\033[0m %s\n' "$1"; }

assert_eq() {
  local desc="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then pass "$desc"; else fail "$desc (expected=$expected, got=$actual)"; fi
}

assert_nonzero() {
  local desc="$1" val="$2"
  if [ "$val" != "0" ] && [ "$val" != "null" ] && [ -n "$val" ]; then pass "$desc"; else fail "$desc (got=$val)"; fi
}

assert_file_exists() {
  local desc="$1" path="$2"
  if [ -f "$path" ]; then pass "$desc"; else fail "$desc (file not found: $path)"; fi
}

assert_contains() {
  local desc="$1" file="$2" pattern="$3"
  if grep -q "$pattern" "$file" 2>/dev/null; then pass "$desc"; else fail "$desc (pattern '$pattern' not found)"; fi
}

# =========================================================================
echo "=== CHM Test Suite ==="
echo ""

# -------------------------------------------------------------------------
echo "--- Test 1: chm_scan.sh produces output ---"
# -------------------------------------------------------------------------
CHM_OUT_DIR="$TEST_OUT_DIR" "$SCRIPT_DIR/chm_scan.sh" > /dev/null 2>&1
SNAPSHOT="$TEST_OUT_DIR/snapshot.json"

assert_file_exists "snapshot.json created" "$SNAPSHOT"

# -------------------------------------------------------------------------
echo "--- Test 2: snapshot is valid JSON ---"
# -------------------------------------------------------------------------
if jq empty "$SNAPSHOT" 2>/dev/null; then
  pass "snapshot is valid JSON"
else
  fail "snapshot is not valid JSON"
fi

# -------------------------------------------------------------------------
echo "--- Test 3: snapshot has required top-level keys ---"
# -------------------------------------------------------------------------
for key in timestamp workspace lints volume monolith_watch speed_watch tests coverage; do
  if jq -e ".$key" "$SNAPSHOT" > /dev/null 2>&1; then
    pass "key '$key' present"
  else
    fail "key '$key' missing"
  fi
done

# -------------------------------------------------------------------------
echo "--- Test 4: lint fields are numeric ---"
# -------------------------------------------------------------------------
LINT_W=$(jq '.lints.warnings' "$SNAPSHOT")
LINT_E=$(jq '.lints.errors' "$SNAPSHOT")
if [[ "$LINT_W" =~ ^[0-9]+$ ]]; then pass "lints.warnings is numeric ($LINT_W)"; else fail "lints.warnings not numeric ($LINT_W)"; fi
if [[ "$LINT_E" =~ ^[0-9]+$ ]]; then pass "lints.errors is numeric ($LINT_E)"; else fail "lints.errors not numeric ($LINT_E)"; fi

# -------------------------------------------------------------------------
echo "--- Test 5: volume metrics are sensible ---"
# -------------------------------------------------------------------------
RUST_LINES=$(jq '.volume.rust_lines' "$SNAPSHOT")
TOTAL_FILES=$(jq '.volume.total_files' "$SNAPSHOT")
assert_nonzero "rust_lines > 0" "$RUST_LINES"
assert_nonzero "total_files > 0" "$TOTAL_FILES"

# -------------------------------------------------------------------------
echo "--- Test 6: monolith_watch structure ---"
# -------------------------------------------------------------------------
MONOLITH_COUNT=$(jq '.monolith_watch.count' "$SNAPSHOT")
MONOLITH_THRESH=$(jq '.monolith_watch.threshold_loc' "$SNAPSHOT")
assert_eq "default threshold is 500" "500" "$MONOLITH_THRESH"
if [[ "$MONOLITH_COUNT" =~ ^[0-9]+$ ]]; then pass "monolith count is numeric ($MONOLITH_COUNT)"; else fail "monolith count not numeric"; fi

# Verify all listed files actually exceed the threshold
BAD_MONOLITHS=$(jq "[.monolith_watch.files[] | select(.lines <= .threshold_loc)] | length" "$SNAPSHOT" 2>/dev/null || echo 0)
# Since threshold_loc is at snapshot level, check differently:
BAD_MONOLITHS=$(jq --argjson t "$MONOLITH_THRESH" '[.monolith_watch.files[] | select(.lines <= $t)] | length' "$SNAPSHOT")
assert_eq "all monolith files exceed threshold" "0" "$BAD_MONOLITHS"

# -------------------------------------------------------------------------
echo "--- Test 7: speed_watch has compile time ---"
# -------------------------------------------------------------------------
CHECK_MS=$(jq '.speed_watch.incremental_check_ms' "$SNAPSHOT")
if [[ "$CHECK_MS" =~ ^[0-9]+$ ]]; then pass "incremental_check_ms is numeric ($CHECK_MS)"; else fail "incremental_check_ms not numeric ($CHECK_MS)"; fi

# -------------------------------------------------------------------------
echo "--- Test 8: test counts are sensible ---"
# -------------------------------------------------------------------------
TEST_COUNT=$(jq '.tests.test_count' "$SNAPSHOT")
TEST_FILES=$(jq '.tests.test_file_count' "$SNAPSHOT")
assert_nonzero "test_count > 0" "$TEST_COUNT"
assert_nonzero "test_file_count > 0" "$TEST_FILES"

# test_file_count <= test_count
if [ "$TEST_FILES" -le "$TEST_COUNT" ] 2>/dev/null; then
  pass "test_file_count <= test_count"
else
  fail "test_file_count ($TEST_FILES) > test_count ($TEST_COUNT)"
fi

# -------------------------------------------------------------------------
echo "--- Test 9: coverage fields exist ---"
# -------------------------------------------------------------------------
COV_PCT=$(jq '.coverage.line_pct' "$SNAPSHOT")
UNCOV_TYPE=$(jq -r '.coverage.uncovered_files | type' "$SNAPSHOT")
# line_pct can be null (tarpaulin not installed) or a number — both valid
if [ "$COV_PCT" = "null" ] || [[ "$COV_PCT" =~ ^[0-9] ]]; then
  pass "coverage.line_pct is null or numeric"
else
  fail "coverage.line_pct unexpected value: $COV_PCT"
fi
assert_eq "uncovered_files is an array" "array" "$UNCOV_TYPE"

# -------------------------------------------------------------------------
echo "--- Test 10: chm_report.sh produces dashboard ---"
# -------------------------------------------------------------------------
CHM_REPORT_OUT="$TEST_OUT_DIR/dashboard.md" "$SCRIPT_DIR/chm_report.sh" "$SNAPSHOT" > /dev/null 2>&1
DASHBOARD="$TEST_OUT_DIR/dashboard.md"

assert_file_exists "dashboard.md created" "$DASHBOARD"
assert_contains "dashboard has title" "$DASHBOARD" "Health Dashboard"
assert_contains "dashboard has Monolith Watch" "$DASHBOARD" "Monolith Watch"
assert_contains "dashboard has Lint Summary" "$DASHBOARD" "Lint Summary"
assert_contains "dashboard has Unchecked Complexity" "$DASHBOARD" "Unchecked Complexity"
assert_contains "dashboard has Speed Watch" "$DASHBOARD" "Speed Watch"
assert_contains "dashboard has Volume Overview" "$DASHBOARD" "Volume Overview"

# -------------------------------------------------------------------------
echo "--- Test 11: custom threshold ---"
# -------------------------------------------------------------------------
CUSTOM_OUT="$(mktemp -d)"
CHM_OUT_DIR="$CUSTOM_OUT" CHM_MONOLITH_THRESHOLD=2000 "$SCRIPT_DIR/chm_scan.sh" > /dev/null 2>&1
CUSTOM_THRESH=$(jq '.monolith_watch.threshold_loc' "$CUSTOM_OUT/snapshot.json")
CUSTOM_COUNT=$(jq '.monolith_watch.count' "$CUSTOM_OUT/snapshot.json")
assert_eq "custom threshold respected" "2000" "$CUSTOM_THRESH"
# With a higher threshold, we should have fewer (or equal) monoliths
if [ "$CUSTOM_COUNT" -le "$MONOLITH_COUNT" ] 2>/dev/null; then
  pass "higher threshold reduces monolith count ($CUSTOM_COUNT <= $MONOLITH_COUNT)"
else
  fail "higher threshold did not reduce count ($CUSTOM_COUNT > $MONOLITH_COUNT)"
fi
rm -rf "$CUSTOM_OUT"

# =========================================================================
echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi

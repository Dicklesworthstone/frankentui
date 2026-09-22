#!/bin/bash
# Focus Navigation E2E Tests
# bd-1n5t.6: Validates Tab/Shift+Tab, arrow key spatial navigation, wrapping
#
# Tests exercised:
# - Tab through ordered fields (focus_next/focus_prev)
# - Arrow key spatial navigation with quadrant-based search
# - Focus wrapping at boundaries
# - Explicit edge priority over spatial fallback
#
# Usage:
#   ./scripts/e2e/focus_management/test_focus_nav.sh
#   E2E_LOG_DIR=/tmp/focus_nav ./scripts/e2e/focus_management/test_focus_nav.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui-focus-nav-$(date +%Y%m%d_%H%M%S)}"

mkdir -p "$LOG_DIR"
cd "$PROJECT_ROOT"

PASS=0
FAIL=0

# Milliseconds from the monotonic clock. Not `date +%s%3N`: BSD date prints
# "...3N", and the duration arithmetic then aborted the script on macOS.
now_ms() {
    python3 -c 'import time; print(time.monotonic_ns() // 1_000_000)'
}

run_test() {
    local name="$1"
    local pattern="$2"
    local test_type="${3:-lib}"

    local start_ms
    start_ms="$(now_ms)"

    local exit_code=0
    if [[ "$test_type" == "integration" ]]; then
        cargo test -p ftui-widgets --test focus_integration -- "$pattern" > "$LOG_DIR/${name}.log" 2>&1 || exit_code=$?
    else
        cargo test -p ftui-widgets --lib -- "$pattern" > "$LOG_DIR/${name}.log" 2>&1 || exit_code=$?
    fi

    local end_ms
    end_ms="$(now_ms)"
    local dur=$((end_ms - start_ms))

    if [[ $exit_code -eq 0 ]]; then
        echo -e "\033[32m  PASS\033[0m $name (${dur}ms)"
        PASS=$((PASS + 1))
    else
        echo -e "\033[31m  FAIL\033[0m $name (${dur}ms) -> $LOG_DIR/${name}.log"
        FAIL=$((FAIL + 1))
    fi
}

echo "Focus Navigation E2E Tests"
echo "=========================="
echo ""

echo "Unit tests (manager):"
run_test "focus_next"            "focus::manager::tests::focus_next"
run_test "focus_prev"            "focus::manager::tests::focus_prev"
run_test "focus_first"           "focus::manager::tests::focus_first"
run_test "focus_last"            "focus::manager::tests::focus_last"
run_test "navigate_spatial"      "focus::manager::tests::navigate_spatial"

echo ""
echo "Unit tests (graph):"
run_test "tab_order"             "focus::graph::tests::tab_order"
run_test "build_tab_chain"       "focus::graph::tests::build_tab_chain"
run_test "find_cycle"            "focus::graph::tests::find_cycle"
run_test "navigate_edges"        "focus::graph::tests::navigate"

echo ""
echo "Unit tests (spatial algorithm):"
run_test "spatial_basic"         "focus::spatial::tests"

echo ""
echo "Integration tests:"
run_test "tab_traversal"         "tab_traversal"              "integration"
run_test "spatial_integration"   "spatial"                     "integration"
run_test "form_layout"           "form_layout"                "integration"
run_test "explicit_override"     "explicit_edge_overrides"    "integration"

echo ""
echo "Results: $PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]

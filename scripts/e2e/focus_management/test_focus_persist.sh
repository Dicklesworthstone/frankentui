#!/bin/bash
# Focus Persistence E2E Tests
# bd-1n5t.6: Validates focus survives re-render, moves on widget removal
#
# Tests exercised:
# - Focus history (back navigation)
# - Focus survives graph modifications
# - Focus moves to valid target when focused widget removed
# - Blur/restore cycle
#
# Usage:
#   ./scripts/e2e/focus_management/test_focus_persist.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui-focus-persist-$(date +%Y%m%d_%H%M%S)}"

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

echo "Focus Persistence E2E Tests"
echo "==========================="
echo ""

echo "Unit tests (manager):"
run_test "focus_back"            "focus::manager::tests::focus_back"
run_test "blur_clear"            "focus::manager::tests::blur"
run_test "clear_history"         "focus::manager::tests::clear_history"

echo ""
echo "Unit tests (graph):"
run_test "remove_node"           "focus::graph::tests::remove"
run_test "graph_clear"           "focus::graph::tests::clear"

echo ""
echo "Integration tests:"
run_test "focus_survives"        "focus_survives"             "integration"
run_test "widget_removal"        "removal"                    "integration"
run_test "dynamic_graph"         "dynamic"                    "integration"

echo ""
echo "Results: $PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]

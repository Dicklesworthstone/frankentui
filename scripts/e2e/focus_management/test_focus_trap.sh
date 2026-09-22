#!/bin/bash
# Focus Trapping E2E Tests
# bd-1n5t.6: Validates modal Tab confinement, nested traps, Escape release
#
# Tests exercised:
# - Push trap confines Tab to group members
# - Nested modals with proper trap stack ordering
# - Pop trap restores previous focus correctly
# - Focus returns to trigger element on trap release
#
# Usage:
#   ./scripts/e2e/focus_management/test_focus_trap.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui-focus-trap-$(date +%Y%m%d_%H%M%S)}"

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

echo "Focus Trapping E2E Tests"
echo "========================"
echo ""

echo "Unit tests (manager):"
run_test "push_trap"             "focus::manager::tests::push_trap"
run_test "pop_trap"              "focus::manager::tests::pop_trap"
run_test "is_trapped"            "focus::manager::tests::is_trapped"
run_test "trap_blocks_spatial"   "focus::manager::tests::navigate_spatial_respects_trap"

echo ""
echo "Integration tests:"
run_test "modal_trap_confinement" "modal_trap"                "integration"
run_test "nested_traps"           "nested_trap"               "integration"
run_test "trap_focus_restore"     "trap.*restore"             "integration"

echo ""
echo "Results: $PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]

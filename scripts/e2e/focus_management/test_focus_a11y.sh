#!/bin/bash
# Focus Accessibility E2E Tests
# bd-1n5t.6: Validates screen reader announcements, focus event emission
#
# Tests exercised:
# - FocusEvent::Focused/Blurred emission on focus changes
# - Focus event contains correct widget IDs
# - Events emitted during trap transitions
# - Event queue consumed correctly (take_focus_event)
#
# Usage:
#   ./scripts/e2e/focus_management/test_focus_a11y.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui-focus-a11y-$(date +%Y%m%d_%H%M%S)}"

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

echo "Focus Accessibility E2E Tests"
echo "============================="
echo ""

echo "Unit tests (focus events):"
run_test "focus_event_on_focus"  "focus::manager::tests::focus_event"
run_test "event_take"            "focus::manager::tests::take_focus"

echo ""
echo "Integration tests:"
run_test "focus_event_emission"  "focus_event"                "integration"
run_test "event_sequence"        "event.*sequence"            "integration"

echo ""
echo "Results: $PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]

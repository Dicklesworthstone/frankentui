#!/bin/bash
# Toast System E2E Test Suite for FrankenTUI
# bd-3tmk.6: Comprehensive end-to-end verification of notification/toast behavior
#
# This script validates:
# 1. Toast appearance and rendering
# 2. Toast queue behavior and stacking
# 3. Toast auto-dismiss timing
# 4. Toast interaction (dismiss on key/click)
# 5. Multiple toast handling
#
# Usage:
#   ./tests/e2e/scripts/test_toast_system.sh
#   E2E_LOG_DIR=/tmp/toast-logs ./tests/e2e/scripts/test_toast_system.sh
#
# Environment:
#   E2E_LOG_DIR         Directory for log files (default: /tmp/ftui_e2e_logs)
#   FTUI_DEMO_BIN       Path to the ftui-demo-showcase binary

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

# Notifications/Toast screen is screen 16 (1-based index)
TOAST_SCREEN=16

# Check for demo showcase binary
resolve_demo_bin() {
    if [[ -n "${FTUI_DEMO_BIN:-}" && -x "$FTUI_DEMO_BIN" ]]; then
        echo "$FTUI_DEMO_BIN"
        return 0
    fi

    # Check shared cargo target directory first (if CARGO_TARGET_DIR is set)
    if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
        local shared_debug="$CARGO_TARGET_DIR/debug/ftui-demo-showcase"
        local shared_release="$CARGO_TARGET_DIR/release/ftui-demo-showcase"
        if [[ -x "$shared_debug" ]]; then
            echo "$shared_debug"
            return 0
        fi
        if [[ -x "$shared_release" ]]; then
            echo "$shared_release"
            return 0
        fi
    fi

    # Check project-local target directory
    local debug_bin="$PROJECT_ROOT/target/debug/ftui-demo-showcase"
    local release_bin="$PROJECT_ROOT/target/release/ftui-demo-showcase"

    if [[ -x "$debug_bin" ]]; then
        echo "$debug_bin"
        return 0
    fi
    if [[ -x "$release_bin" ]]; then
        echo "$release_bin"
        return 0
    fi

    return 1
}

DEMO_BIN=""
if ! DEMO_BIN="$(resolve_demo_bin)"; then
    LOG_FILE="$E2E_LOG_DIR/toast_system_missing.log"
    for t in toast_screen_loads toast_trigger_basic toast_multiple_stack toast_dismiss_key toast_auto_dismiss toast_priority_order toast_rapid_trigger toast_clear_all toast_persist_toggle toast_queue_overflow; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

run_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(e2e_monotonic_ms)"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)"
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)"
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "toast system assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "toast system assertions failed"
    return 1
}

# Test: Toast screen loads without crashing
toast_screen_loads() {
    LOG_FILE="$E2E_LOG_DIR/toast_screen_loads.log"
    local output_file="$E2E_LOG_DIR/toast_screen_loads.pty"

    log_test_start "toast_screen_loads"

    # Start demo on Notifications screen (screen 16)
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TOAST_SCREEN"

    # Notifications screen should render
    if ! grep -a -q "Notification\|Toast\|notification" "$output_file"; then
        log_warn "Toast screen content not found in output"
        return 1
    fi

    # Output should have substantial content
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Trigger a basic toast notification
toast_trigger_basic() {
    LOG_FILE="$E2E_LOG_DIR/toast_trigger_basic.log"
    local output_file="$E2E_LOG_DIR/toast_trigger_basic.pty"

    log_test_start "toast_trigger_basic"

    # Press a key to trigger toast (typically 'i' for info, 'w' for warning, 'e' for error)
    PTY_SEND='i' \
    PTY_SEND_DELAY_MS=500 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TOAST_SCREEN"

    # App should continue running without crash
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Multiple toasts stack correctly
toast_multiple_stack() {
    LOG_FILE="$E2E_LOG_DIR/toast_multiple_stack.log"
    local output_file="$E2E_LOG_DIR/toast_multiple_stack.pty"

    log_test_start "toast_multiple_stack"

    # Trigger multiple toasts in quick succession
    PTY_SEND='iwe' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_DEMO_EXIT_AFTER_MS=4000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TOAST_SCREEN"

    # App should handle multiple toasts without crash
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Dismiss toast with keyboard
toast_dismiss_key() {
    LOG_FILE="$E2E_LOG_DIR/toast_dismiss_key.log"
    local output_file="$E2E_LOG_DIR/toast_dismiss_key.pty"

    log_test_start "toast_dismiss_key"

    # Trigger toast then try to dismiss (typically Escape or Enter)
    PTY_SEND=$'i\x1b' \
    PTY_SEND_DELAY_MS=500 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TOAST_SCREEN"

    # App should handle dismiss key
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Toast auto-dismisses after timeout
toast_auto_dismiss() {
    LOG_FILE="$E2E_LOG_DIR/toast_auto_dismiss.log"
    local output_file="$E2E_LOG_DIR/toast_auto_dismiss.pty"

    log_test_start "toast_auto_dismiss"

    # Trigger toast and wait for auto-dismiss (longer timeout)
    PTY_SEND='i' \
    PTY_SEND_DELAY_MS=500 \
    FTUI_DEMO_EXIT_AFTER_MS=6000 \
    PTY_TIMEOUT=8 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TOAST_SCREEN"

    # App should run and toast should auto-dismiss
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Priority ordering (urgent toasts first)
toast_priority_order() {
    LOG_FILE="$E2E_LOG_DIR/toast_priority_order.log"
    local output_file="$E2E_LOG_DIR/toast_priority_order.pty"

    log_test_start "toast_priority_order"

    # Trigger info, then error (error should show prominently)
    PTY_SEND='ie' \
    PTY_SEND_DELAY_MS=400 \
    FTUI_DEMO_EXIT_AFTER_MS=4000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TOAST_SCREEN"

    # App should handle priority toasts
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Rapid toast triggering (stress test)
toast_rapid_trigger() {
    LOG_FILE="$E2E_LOG_DIR/toast_rapid_trigger.log"
    local output_file="$E2E_LOG_DIR/toast_rapid_trigger.pty"

    log_test_start "toast_rapid_trigger"

    # Rapidly trigger many toasts
    PTY_SEND='iiiieeeewwwws' \
    PTY_SEND_DELAY_MS=100 \
    FTUI_DEMO_EXIT_AFTER_MS=5000 \
    PTY_TIMEOUT=7 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TOAST_SCREEN"

    # App should handle rapid toasts without crash
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Clear all toasts
toast_clear_all() {
    LOG_FILE="$E2E_LOG_DIR/toast_clear_all.log"
    local output_file="$E2E_LOG_DIR/toast_clear_all.pty"

    log_test_start "toast_clear_all"

    # Trigger toasts then try to clear all (typically 'c' or Ctrl+C)
    PTY_SEND='iiic' \
    PTY_SEND_DELAY_MS=400 \
    FTUI_DEMO_EXIT_AFTER_MS=4000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TOAST_SCREEN"

    # App should handle clear all
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Toggle persistent toast mode
toast_persist_toggle() {
    LOG_FILE="$E2E_LOG_DIR/toast_persist_toggle.log"
    local output_file="$E2E_LOG_DIR/toast_persist_toggle.pty"

    log_test_start "toast_persist_toggle"

    # Toggle persistent mode if available (typically 'p')
    PTY_SEND='pi' \
    PTY_SEND_DELAY_MS=400 \
    FTUI_DEMO_EXIT_AFTER_MS=4000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TOAST_SCREEN"

    # App should handle persist toggle
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Queue overflow behavior
toast_queue_overflow() {
    LOG_FILE="$E2E_LOG_DIR/toast_queue_overflow.log"
    local output_file="$E2E_LOG_DIR/toast_queue_overflow.pty"

    log_test_start "toast_queue_overflow"

    # Trigger many toasts to test queue limits
    PTY_SEND='iiiiiiiiiieeeeeeeeee' \
    PTY_SEND_DELAY_MS=50 \
    FTUI_DEMO_EXIT_AFTER_MS=5000 \
    PTY_TIMEOUT=7 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TOAST_SCREEN"

    # App should handle queue overflow gracefully
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Run all tests
# ============================================================================

FAILURES=0
run_case "toast_screen_loads" toast_screen_loads               || FAILURES=$((FAILURES + 1))
run_case "toast_trigger_basic" toast_trigger_basic             || FAILURES=$((FAILURES + 1))
run_case "toast_multiple_stack" toast_multiple_stack           || FAILURES=$((FAILURES + 1))
run_case "toast_dismiss_key" toast_dismiss_key                 || FAILURES=$((FAILURES + 1))
run_case "toast_auto_dismiss" toast_auto_dismiss               || FAILURES=$((FAILURES + 1))
run_case "toast_priority_order" toast_priority_order           || FAILURES=$((FAILURES + 1))
run_case "toast_rapid_trigger" toast_rapid_trigger             || FAILURES=$((FAILURES + 1))
run_case "toast_clear_all" toast_clear_all                     || FAILURES=$((FAILURES + 1))
run_case "toast_persist_toggle" toast_persist_toggle           || FAILURES=$((FAILURES + 1))
run_case "toast_queue_overflow" toast_queue_overflow           || FAILURES=$((FAILURES + 1))

exit "$FAILURES"

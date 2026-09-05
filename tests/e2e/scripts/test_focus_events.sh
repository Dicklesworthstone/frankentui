#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

E2E_SUITE_SCRIPT="$SCRIPT_DIR/test_focus_events.sh"
export E2E_SUITE_SCRIPT
ONLY_CASE="${E2E_ONLY_CASE:-}"

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/focus_missing.log"
    for t in focus_in_event focus_out_event; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

run_case() {
    local name="$1"
    shift
    if [[ -n "$ONLY_CASE" && "$ONLY_CASE" != "$name" ]]; then
        LOG_FILE="$E2E_LOG_DIR/${name}.log"
        log_test_skip "$name" "filtered (E2E_ONLY_CASE=$ONLY_CASE)"
        record_result "$name" "skipped" 0 "$LOG_FILE" "filtered"
        return 0
    fi
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)" || return 2
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "focus assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "focus assertions failed"
    return 1
}

focus_in_event() {
    LOG_FILE="$E2E_LOG_DIR/focus_in_event.log"
    local output_file="$E2E_LOG_DIR/focus_in_event.pty"

    log_test_start "focus_in_event"

    PTY_SEND=$'\x1b[I' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_FOCUS=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q "Focus: gained" "$output_file" || return 1
}

focus_out_event() {
    LOG_FILE="$E2E_LOG_DIR/focus_out_event.log"
    local output_file="$E2E_LOG_DIR/focus_out_event.pty"

    log_test_start "focus_out_event"

    PTY_SEND=$'\x1b[O' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_FOCUS=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q "Focus: lost" "$output_file" || return 1
}

FAILURES=0
run_case "focus_in_event" focus_in_event   || FAILURES=$((FAILURES + 1))
run_case "focus_out_event" focus_out_event || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

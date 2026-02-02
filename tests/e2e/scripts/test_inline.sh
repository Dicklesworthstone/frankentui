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

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/inline_missing.log"
    log_test_skip "inline_basic" "ftui-harness binary missing"
    record_result "inline_basic" "skipped" 0 "$LOG_FILE" "binary missing"
    log_test_skip "inline_log_scroll" "ftui-harness binary missing"
    record_result "inline_log_scroll" "skipped" 0 "$LOG_FILE" "binary missing"
    exit 0
fi

run_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(date +%s%3N)"

    if "$@"; then
        local end_ms
        end_ms="$(date +%s%3N)"
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    fi

    local end_ms
    end_ms="$(date +%s%3N)"
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    return 1
}

inline_basic() {
    LOG_FILE="$E2E_LOG_DIR/inline_basic.log"
    local output_file="$E2E_LOG_DIR/inline_basic.pty"

    log_test_start "inline_basic"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=0 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    rg -a -q "Welcome to the Agent Harness" "$output_file"
    rg -a -q "Type a command and press Enter" "$output_file"
}

inline_log_scroll() {
    LOG_FILE="$E2E_LOG_DIR/inline_log_scroll.log"
    local output_file="$E2E_LOG_DIR/inline_log_scroll.pty"

    log_test_start "inline_log_scroll"

    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_LOG_LINES=50 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    rg -a -q "Log line" "$output_file"
    rg -a -q "Log line [0-9][0-9]" "$output_file"

    local count
    count=$(rg -a -o "Log line [0-9]+" "$output_file" | wc -l | tr -d ' ')
    [[ "$count" -ge 4 ]]
}

run_case "inline_basic" inline_basic
run_case "inline_log_scroll" inline_log_scroll

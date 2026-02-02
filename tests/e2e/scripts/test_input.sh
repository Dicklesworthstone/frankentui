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
    LOG_FILE="$E2E_LOG_DIR/input_missing.log"
    log_test_skip "input_help_command" "ftui-harness binary missing"
    record_result "input_help_command" "skipped" 0 "$LOG_FILE" "binary missing"
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
    log_test_fail "$name" "input assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "input assertions failed"
    return 1
}

input_help_command() {
    LOG_FILE="$E2E_LOG_DIR/input_help_command.log"
    local output_file="$E2E_LOG_DIR/input_help_command.pty"

    log_test_start "input_help_command"

    PTY_SEND=$'help\r' \
    PTY_SEND_DELAY_MS=200 \
    FTUI_HARNESS_EXIT_AFTER_MS=1400 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q "Available commands:" "$output_file"
    grep -a -q "help      - Show this help" "$output_file"
}

run_case "input_help_command" input_help_command

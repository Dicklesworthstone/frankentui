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
    LOG_FILE="$E2E_LOG_DIR/altscreen_missing.log"
    log_test_skip "altscreen_enter_exit" "ftui-harness binary missing"
    record_result "altscreen_enter_exit" "skipped" 0 "$LOG_FILE" "binary missing"
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
    log_test_fail "$name" "alt-screen assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "alt-screen assertions failed"
    return 1
}

altscreen_enter_exit() {
    LOG_FILE="$E2E_LOG_DIR/altscreen_enter_exit.log"
    local output_file="$E2E_LOG_DIR/altscreen_enter_exit.pty"

    log_test_start "altscreen_enter_exit"

    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=0 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q $'\x1b[?1049h' "$output_file"
    grep -a -q $'\x1b[?1049l' "$output_file"
    grep -a -q $'\x1b[?25h' "$output_file"
}

run_case "altscreen_enter_exit" altscreen_enter_exit

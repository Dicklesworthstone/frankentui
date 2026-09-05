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

E2E_SUITE_SCRIPT="$SCRIPT_DIR/test_kitty_keyboard.sh"
export E2E_SUITE_SCRIPT
ONLY_CASE="${E2E_ONLY_CASE:-}"

ALL_CASES=(
    kitty_basic_char
    kitty_ctrl_repeat
    kitty_function_key
    kitty_tab_key
)

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/kitty_missing.log"
    for t in "${ALL_CASES[@]}"; do
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
    log_test_fail "$name" "kitty keyboard assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "kitty keyboard assertions failed"
    return 1
}

kitty_basic_char() {
    LOG_FILE="$E2E_LOG_DIR/kitty_basic_char.log"
    local output_file="$E2E_LOG_DIR/kitty_basic_char.pty"

    log_test_start "kitty_basic_char"

    PTY_SEND=$'\x1b[97u' \
    PTY_SEND_DELAY_MS=200 \
    FTUI_HARNESS_INPUT_MODE=parser \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q "Key: code=Char('a') kind=Press mods=none" "$output_file" || return 1
}

kitty_ctrl_repeat() {
    LOG_FILE="$E2E_LOG_DIR/kitty_ctrl_repeat.log"
    local output_file="$E2E_LOG_DIR/kitty_ctrl_repeat.pty"

    log_test_start "kitty_ctrl_repeat"

    PTY_SEND=$'\x1b[97;5:2u' \
    PTY_SEND_DELAY_MS=200 \
    FTUI_HARNESS_INPUT_MODE=parser \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q "Key: code=Char('a') kind=Repeat mods=ctrl" "$output_file" || return 1
}

kitty_function_key() {
    LOG_FILE="$E2E_LOG_DIR/kitty_function_key.log"
    local output_file="$E2E_LOG_DIR/kitty_function_key.pty"

    log_test_start "kitty_function_key"

    PTY_SEND=$'\x1b[57364u' \
    PTY_SEND_DELAY_MS=200 \
    FTUI_HARNESS_INPUT_MODE=parser \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q "Key: code=F(1) kind=Press mods=none" "$output_file" || return 1
}

kitty_tab_key() {
    LOG_FILE="$E2E_LOG_DIR/kitty_tab_key.log"
    local output_file="$E2E_LOG_DIR/kitty_tab_key.pty"

    log_test_start "kitty_tab_key"

    PTY_SEND=$'\x1b[57346u' \
    PTY_SEND_DELAY_MS=200 \
    FTUI_HARNESS_INPUT_MODE=parser \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q "Key: code=Tab kind=Press mods=none" "$output_file" || return 1
}

FAILURES=0
run_case "kitty_basic_char" kitty_basic_char         || FAILURES=$((FAILURES + 1))
run_case "kitty_ctrl_repeat" kitty_ctrl_repeat       || FAILURES=$((FAILURES + 1))
run_case "kitty_function_key" kitty_function_key     || FAILURES=$((FAILURES + 1))
run_case "kitty_tab_key" kitty_tab_key               || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

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
    LOG_FILE="$E2E_LOG_DIR/cleanup_missing.log"
    log_test_skip "cleanup_normal" "ftui-harness binary missing"
    record_result "cleanup_normal" "skipped" 0 "$LOG_FILE" "binary missing"
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
    log_test_fail "$name" "cleanup assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "cleanup assertions failed"
    return 1
}

cleanup_normal() {
    LOG_FILE="$E2E_LOG_DIR/cleanup_normal.log"
    local output_file="$E2E_LOG_DIR/cleanup_normal.pty"

    log_test_start "cleanup_normal"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=0 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q $'\x1b[?25h' "$output_file"
}

run_case "cleanup_normal" cleanup_normal

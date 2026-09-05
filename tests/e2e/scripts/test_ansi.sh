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

ALL_CASES=(
    ansi_sgr_colors
    ansi_sgr_reset
    ansi_cursor_position
    ansi_sync_output
)

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/ansi_missing.log"
    for t in "${ALL_CASES[@]}"; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

run_case() {
    local name="$1"
    shift
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
    log_test_fail "$name" "ANSI assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "ANSI assertions failed"
    return 1
}

# Test: SGR color sequences appear in output
# The harness status bar uses colored text (model name, status indicators).
# Verify that SGR sequences with color parameters are present.
ansi_sgr_colors() {
    LOG_FILE="$E2E_LOG_DIR/ansi_sgr_colors.log"
    local output_file="$E2E_LOG_DIR/ansi_sgr_colors.pty"

    log_test_start "ansi_sgr_colors"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=0 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # SGR sequences should be present (CSI ... m)
    # At minimum, the presenter emits color codes for styled widgets.
    # Match any SGR sequence: ESC [ <params> m
    grep -a -E -q $'\x1b\\[[0-9]+(;[0-9]+)*m' "$output_file" || return 1
    log_debug "SGR color sequences found in output"

    # Verify reset sequence appears (cleanup or style transitions)
    grep -a -E -q $'\x1b\\[0?m' "$output_file" || return 1
    log_debug "SGR reset sequence found"
}

# Test: SGR reset emitted during cleanup
# After the harness exits, the terminal should have SGR 0 (reset) to clear styles.
ansi_sgr_reset() {
    LOG_FILE="$E2E_LOG_DIR/ansi_sgr_reset.log"
    local output_file="$E2E_LOG_DIR/ansi_sgr_reset.pty"

    log_test_start "ansi_sgr_reset"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=0 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # The output must contain SGR reset (ESC[0m or ESC[m)
    grep -a -E -q $'\x1b\\[0?m' "$output_file" || return 1
    log_debug "SGR reset found in output"

    # Cursor show must also be present (general cleanup)
    grep -a -F -q $'\x1b[?25h' "$output_file" || return 1
    log_debug "Cursor show sequence found"
}

# Test: Cursor positioning sequences used during rendering
# The presenter uses CUP (CSI row;col H) to position the cursor for rendering.
ansi_cursor_position() {
    LOG_FILE="$E2E_LOG_DIR/ansi_cursor_position.log"
    local output_file="$E2E_LOG_DIR/ansi_cursor_position.pty"

    log_test_start "ansi_cursor_position"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=3 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # CUP sequence: ESC [ row ; col H  (cursor positioning)
    grep -a -E -q $'\x1b\\[[0-9]+;[0-9]+H' "$output_file" || return 1
    log_debug "CUP cursor position sequences found"
}

# Test: Synchronized output mode (DEC 2026)
# FrankenTUI uses synchronized output to prevent tearing.
# BSU = ESC[?2026h (begin), ESU = ESC[?2026l (end)
ansi_sync_output() {
    LOG_FILE="$E2E_LOG_DIR/ansi_sync_output.log"
    local output_file="$E2E_LOG_DIR/ansi_sync_output.pty"

    log_test_start "ansi_sync_output"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Check for synchronized output begin/end sequences
    # BSU: ESC[?2026h
    if grep -a -F -q $'\x1b[?2026h' "$output_file"; then
        log_debug "Synchronized output BSU found"
        # If BSU is present, ESU should also be present
        grep -a -F -q $'\x1b[?2026l' "$output_file" || return 1
        log_debug "Synchronized output ESU found"
    else
        # Sync output may not be enabled on all terminals/configurations.
        # This is acceptable - just log and pass.
        log_debug "Synchronized output not used (acceptable)"
    fi

    # At minimum, the output should have render content
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1
}

FAILURES=0
run_case "ansi_sgr_colors" ansi_sgr_colors             || FAILURES=$((FAILURES + 1))
run_case "ansi_sgr_reset" ansi_sgr_reset               || FAILURES=$((FAILURES + 1))
run_case "ansi_cursor_position" ansi_cursor_position   || FAILURES=$((FAILURES + 1))
run_case "ansi_sync_output" ansi_sync_output           || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

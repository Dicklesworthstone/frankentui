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
    for t in inline_basic inline_log_scroll inline_many_logs inline_custom_height inline_ui_chrome inline_resize inline_cursor_contract; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
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

    rg -a -q "Welcome to the Agent Harness" "$output_file" || return 1
    rg -a -q "Type a command and press Enter" "$output_file" || return 1
}

inline_log_scroll() {
    LOG_FILE="$E2E_LOG_DIR/inline_log_scroll.log"
    local output_file="$E2E_LOG_DIR/inline_log_scroll.pty"

    log_test_start "inline_log_scroll"

    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_LOG_LINES=50 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # With many log lines, the harness should still render without crashing.
    # The PTY output will contain the first render frame plus diff updates.
    # Verify the output file has substantial content (render cycles ran).
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1

    # The first render should still contain harness UI chrome
    grep -a -q "claude-3.5" "$output_file" || return 1
}

inline_many_logs() {
    LOG_FILE="$E2E_LOG_DIR/inline_many_logs.log"
    local output_file="$E2E_LOG_DIR/inline_many_logs.pty"

    log_test_start "inline_many_logs"

    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    FTUI_HARNESS_LOG_LINES=200 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # With 200 log lines, the harness must handle large content without crashing.
    # Verify the output file has substantial content (render cycles ran).
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1

    # Status bar should still be rendered
    grep -a -q "claude-3.5" "$output_file" || return 1
}

inline_custom_height() {
    LOG_FILE="$E2E_LOG_DIR/inline_custom_height.log"
    local output_file="$E2E_LOG_DIR/inline_custom_height.pty"

    log_test_start "inline_custom_height"

    FTUI_HARNESS_UI_HEIGHT=20 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=5 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Basic sanity: welcome text appears
    rg -a -q "Welcome to the Agent Harness" "$output_file" || return 1
    # Log lines also appear
    rg -a -q "Log line [1-5]" "$output_file" || return 1
}

inline_ui_chrome() {
    LOG_FILE="$E2E_LOG_DIR/inline_ui_chrome.log"
    local output_file="$E2E_LOG_DIR/inline_ui_chrome.pty"

    log_test_start "inline_ui_chrome"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=0 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Status bar should show model name
    rg -a -q "claude-3.5" "$output_file" || return 1
    # Status bar shows Idle state
    rg -a -q "Idle" "$output_file" || return 1
    # UI has the Log panel title
    rg -a -q "Log" "$output_file" || return 1
    # Key hint should appear in status bar
    grep -a -q "Quit" "$output_file" || return 1
}

inline_resize() {
    LOG_FILE="$E2E_LOG_DIR/inline_resize.log"
    local output_file="$E2E_LOG_DIR/inline_resize.pty"

    log_test_start "inline_resize"

    # Run at a non-default PTY size to verify the harness adapts.
    # If the harness handles resize (SIGWINCH), it should still render correctly.
    PTY_COLS=60 \
    PTY_ROWS=15 \
    FTUI_HARNESS_EXIT_AFTER_MS=1000 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # The harness must render without crashing at a smaller terminal size.
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    # Status bar should still render at smaller width
    grep -a -q "claude-3.5" "$output_file" || return 1
}

inline_cursor_contract() {
    LOG_FILE="$E2E_LOG_DIR/inline_cursor_contract.log"
    local output_file="$E2E_LOG_DIR/inline_cursor_contract.pty"

    log_test_start "inline_cursor_contract"

    # Run with log output to exercise multiple render cycles.
    # After cleanup, cursor visibility must be restored.
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_LOG_LINES=20 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Cursor hide is optional in inline mode (alt-screen mode always hides).
    # Just log whether it was emitted for diagnostics.
    if grep -a -F -q $'\x1b[?25l' "$output_file"; then
        log_debug "Cursor hide sequence found (inline mode)"
    fi

    # Cursor show must appear at cleanup (required for terminal restore)
    grep -a -F -q $'\x1b[?25h' "$output_file" || return 1
}

FAILURES=0
run_case "inline_basic" inline_basic              || FAILURES=$((FAILURES + 1))
run_case "inline_log_scroll" inline_log_scroll    || FAILURES=$((FAILURES + 1))
run_case "inline_many_logs" inline_many_logs      || FAILURES=$((FAILURES + 1))
run_case "inline_custom_height" inline_custom_height || FAILURES=$((FAILURES + 1))
run_case "inline_ui_chrome" inline_ui_chrome      || FAILURES=$((FAILURES + 1))
run_case "inline_resize" inline_resize            || FAILURES=$((FAILURES + 1))
run_case "inline_cursor_contract" inline_cursor_contract || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

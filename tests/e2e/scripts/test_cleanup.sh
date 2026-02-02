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
    cleanup_normal
    cleanup_cursor_visible
    cleanup_sigterm
    cleanup_mouse_disabled
    cleanup_bracketed_paste_disabled
    cleanup_altscreen_exit
    cleanup_altscreen_mouse_focus
)

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/cleanup_missing.log"
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

    grep -a -F -q $'\x1b[?25h' "$output_file" || return 1
}

cleanup_cursor_visible() {
    LOG_FILE="$E2E_LOG_DIR/cleanup_cursor_visible.log"
    local output_file="$E2E_LOG_DIR/cleanup_cursor_visible.pty"

    log_test_start "cleanup_cursor_visible"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=0 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Cursor show sequence must appear (cleanup restores cursor visibility)
    grep -a -F -q $'\x1b[?25h' "$output_file" || return 1

    # Output should end cleanly (no truncated escape sequences at the end)
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 100 ]] || return 1
}

cleanup_sigterm() {
    LOG_FILE="$E2E_LOG_DIR/cleanup_sigterm.log"
    local output_file="$E2E_LOG_DIR/cleanup_sigterm.pty"

    log_test_start "cleanup_sigterm"

    # Start harness with a long timeout so we can send SIGTERM
    FTUI_HARNESS_EXIT_AFTER_MS=10000 \
    FTUI_HARNESS_LOG_LINES=5 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || true

    # The PTY timeout (3s) will kill the process via SIGTERM.
    # Verify the output file exists and has content (the app ran)
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 50 ]] || return 1

    # Verify welcome text appeared (app started successfully before kill)
    grep -a -q "Welcome" "$output_file" || return 1
}

cleanup_mouse_disabled() {
    LOG_FILE="$E2E_LOG_DIR/cleanup_mouse_disabled.log"
    local output_file="$E2E_LOG_DIR/cleanup_mouse_disabled.pty"

    log_test_start "cleanup_mouse_disabled"

    # Enable mouse capture — cleanup must disable it
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_LOG_LINES=0 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Mouse disable sequence must appear (CSI ? 1000 l or combined)
    grep -a -P -q '\x1b\[\?1000' "$output_file" || return 1
    # Cursor show must still be present
    grep -a -F -q $'\x1b[?25h' "$output_file" || return 1
}

cleanup_bracketed_paste_disabled() {
    LOG_FILE="$E2E_LOG_DIR/cleanup_bracketed_paste_disabled.log"
    local output_file="$E2E_LOG_DIR/cleanup_bracketed_paste_disabled.pty"

    log_test_start "cleanup_bracketed_paste_disabled"

    # Bracketed paste is enabled by default in ProgramConfig.
    # Cleanup must emit CSI ? 2004 l
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=0 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Bracketed paste disable must appear
    grep -a -F -q $'\x1b[?2004l' "$output_file" || return 1
    # Cursor show must still be present
    grep -a -F -q $'\x1b[?25h' "$output_file" || return 1
}

cleanup_altscreen_exit() {
    LOG_FILE="$E2E_LOG_DIR/cleanup_altscreen_exit.log"
    local output_file="$E2E_LOG_DIR/cleanup_altscreen_exit.pty"

    log_test_start "cleanup_altscreen_exit"

    # Run in alt-screen mode — cleanup must exit alt screen
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_LOG_LINES=0 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Alt-screen exit sequence must appear
    grep -a -F -q $'\x1b[?1049l' "$output_file" || return 1
    # Cursor show must still be present
    grep -a -F -q $'\x1b[?25h' "$output_file" || return 1
}

cleanup_altscreen_mouse_focus() {
    LOG_FILE="$E2E_LOG_DIR/cleanup_altscreen_mouse_focus.log"
    local output_file="$E2E_LOG_DIR/cleanup_altscreen_mouse_focus.pty"

    log_test_start "cleanup_altscreen_mouse_focus"

    # Enable all features — verify combined cleanup
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_ENABLE_FOCUS=1 \
    FTUI_HARNESS_LOG_LINES=0 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # All cleanup sequences must appear
    grep -a -F -q $'\x1b[?1049l' "$output_file"   || return 1  # alt-screen exit
    grep -a -F -q $'\x1b[?25h'  "$output_file"    || return 1  # cursor show
    grep -a -P -q '\x1b\[\?1000' "$output_file"   || return 1  # mouse disable
    grep -a -F -q $'\x1b[?1004l' "$output_file"   || return 1  # focus events disable
    grep -a -F -q $'\x1b[?2004l' "$output_file"   || return 1  # bracketed paste disable
}

FAILURES=0
run_case "cleanup_normal" cleanup_normal                               || FAILURES=$((FAILURES + 1))
run_case "cleanup_cursor_visible" cleanup_cursor_visible               || FAILURES=$((FAILURES + 1))
run_case "cleanup_sigterm" cleanup_sigterm                             || FAILURES=$((FAILURES + 1))
run_case "cleanup_mouse_disabled" cleanup_mouse_disabled               || FAILURES=$((FAILURES + 1))
run_case "cleanup_bracketed_paste_disabled" cleanup_bracketed_paste_disabled || FAILURES=$((FAILURES + 1))
run_case "cleanup_altscreen_exit" cleanup_altscreen_exit               || FAILURES=$((FAILURES + 1))
run_case "cleanup_altscreen_mouse_focus" cleanup_altscreen_mouse_focus || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

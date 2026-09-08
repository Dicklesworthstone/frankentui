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
    log_test_fail "$name" "cleanup assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "cleanup assertions failed"
    return 1
}

# These cases exercise supported modes on a simulated bare modern terminal.
# Keep the profile inside the child invocation so other suites retain theirs.
cleanup_pty_run() (
    unset NO_COLOR FTUI_TEST_PROFILE TMUX TMUX_PANE STY ZELLIJ \
        WEZTERM_UNIX_SOCKET WEZTERM_PANE WEZTERM_EXECUTABLE KITTY_WINDOW_ID \
        WT_SESSION TERM_PROGRAM_VERSION LC_TERMINAL LC_TERMINAL_VERSION
    export TERM=xterm-256color COLORTERM=truecolor TERM_PROGRAM=Alacritty
    log_info "PTY profile: TERM=$TERM COLORTERM=$COLORTERM TERM_PROGRAM=$TERM_PROGRAM; inherited capability/mux markers cleared"
    log_info "Requested modes: screen=${FTUI_HARNESS_SCREEN_MODE:-inline} mouse=${FTUI_HARNESS_ENABLE_MOUSE:-0} focus=${FTUI_HARNESS_ENABLE_FOCUS:-0} paste=1 (ProgramConfig default)"
    pty_run "$@"
)

assert_cleanup_modes() {
    "$E2E_PYTHON" - "$@" <<'PY'
import pathlib
import re
import sys


def check_modes(data, disabled_modes):
    transitions = {}
    for parameters, state in re.findall(rb"\x1b\[\?([0-9]+(?:;[0-9]+)*)([hl])", data):
        for parameter in parameters.split(b";"):
            transitions.setdefault(int(parameter), []).append(state)
    expected = [(25, b"l", b"h")]
    expected.extend((mode, b"h", b"l") for mode in disabled_modes)
    errors = []
    for mode, active, restored in expected:
        states = transitions.get(mode, [])
        if active not in states or states[-1] != restored:
            errors.append(f"mode {mode}: expected {active!r} then final {restored!r}, got {states!r}")
    return errors


errors = check_modes(pathlib.Path(sys.argv[1]).read_bytes(), [int(mode) for mode in sys.argv[2:]])
for error in errors:
    print(error, file=sys.stderr)
sys.exit(bool(errors))
PY
}

cleanup_normal() {
    LOG_FILE="$E2E_LOG_DIR/cleanup_normal.log"
    local output_file="$E2E_LOG_DIR/cleanup_normal.pty"

    log_test_start "cleanup_normal"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=0 \
    PTY_TIMEOUT=3 \
        cleanup_pty_run "$output_file" "$E2E_HARNESS_BIN" || return 1

    assert_cleanup_modes "$output_file"
}

cleanup_cursor_visible() {
    LOG_FILE="$E2E_LOG_DIR/cleanup_cursor_visible.log"
    local output_file="$E2E_LOG_DIR/cleanup_cursor_visible.pty"

    log_test_start "cleanup_cursor_visible"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=0 \
    PTY_TIMEOUT=3 \
        cleanup_pty_run "$output_file" "$E2E_HARNESS_BIN" || return 1

    # Cursor must be shown after its final hide.
    assert_cleanup_modes "$output_file" || return 1

    # Require a rendered application, not just an empty cleanup sequence.
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
        cleanup_pty_run "$output_file" "$E2E_HARNESS_BIN" || true

    # The PTY timeout (3s) will kill the process via SIGTERM.
    # Verify the output file exists and has content (the app ran)
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 50 ]] || return 1

    # The five fixture logs scroll the welcome text out of the inline viewport.
    # Verify a visible fixture line, then the actual cleanup state.
    grep -a -F -q "Log line 5" "$output_file" || return 1
    assert_cleanup_modes "$output_file" 2004
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
        cleanup_pty_run "$output_file" "$E2E_HARNESS_BIN" || return 1

    # Both split and grouped DEC parameters must finish disabled after enabling.
    assert_cleanup_modes "$output_file" 1000 1002 1006
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
        cleanup_pty_run "$output_file" "$E2E_HARNESS_BIN" || return 1

    assert_cleanup_modes "$output_file" 2004
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
        cleanup_pty_run "$output_file" "$E2E_HARNESS_BIN" || return 1

    assert_cleanup_modes "$output_file" 1049
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
        cleanup_pty_run "$output_file" "$E2E_HARNESS_BIN" || return 1

    assert_cleanup_modes "$output_file" 1049 1000 1002 1006 1004 2004
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

#!/bin/bash
set -euo pipefail

# ─────────────────────────────────────────────────────────────────────────────
# E2E Tests: Mouse SGR Protocol
#
# KNOWN LIMITATION: Crossterm reads from /dev/tty directly on Unix, bypassing
# PTY input for complex escape sequences. These tests verify the SGR mouse
# sequences would be correctly handled IF delivered to the event system.
#
# For comprehensive input parser coverage, see unit tests in:
#   crates/ftui-core/src/input_parser.rs (mouse_sgr_* tests)
#
# These E2E tests may fail in PTY environments due to crossterm's /dev/tty
# reading behavior. They serve as documentation of expected behavior and will
# pass when running with a real TTY or when the test infrastructure is improved
# to support stdin-based event reading.
# ─────────────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

E2E_SUITE_SCRIPT="$SCRIPT_DIR/test_mouse_sgr.sh"
export E2E_SUITE_SCRIPT
export PTY_CANONICALIZE=1
ONLY_CASE="${E2E_ONLY_CASE:-}"

# All test cases for skip reporting
ALL_CASES=(
    mouse_left_click_release
    mouse_middle_click
    mouse_right_click
    mouse_move_event
    mouse_drag_left
    mouse_scroll_events
    mouse_coordinates
    mouse_large_coords
    mouse_shift_click
    mouse_ctrl_click
    mouse_alt_click
)

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/mouse_missing.log"
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
    log_test_fail "$name" "mouse SGR assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "mouse SGR assertions failed"
    return 1
}

# ─────────────────────────────────────────────────────────────────────────────
# SGR Mouse Protocol Reference:
#   Format: ESC [ < Cb ; Cx ; Cy M (press/motion) or ESC [ < Cb ; Cx ; Cy m (release)
#   Button bits 0-1: 0=Left, 1=Middle, 2=Right
#   Modifier bits: 4=Shift, 8=Alt/Meta, 16=Ctrl
#   Motion bit: 32 (bit 5) indicates motion event
#   Scroll: 64=Up, 65=Down, 66=Left, 67=Right
# ─────────────────────────────────────────────────────────────────────────────

mouse_left_click_release() {
    LOG_FILE="$E2E_LOG_DIR/mouse_left_click_release.log"
    local output_file="$E2E_LOG_DIR/mouse_left_click_release.pty"

    log_test_start "mouse_left_click_release"
    PTY_TEST_NAME="mouse_left_click_release"

    # Button code 0 = Left, M=press, m=release
    PTY_SEND=$'\x1b[<0;10;5M\x1b[<0;10;5m' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Verify button type is explicitly Left
    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Mouse: Down(Left)" "$canonical_file" || return 1
    grep -a -q "Mouse: Up(Left)" "$canonical_file" || return 1
}

mouse_middle_click() {
    LOG_FILE="$E2E_LOG_DIR/mouse_middle_click.log"
    local output_file="$E2E_LOG_DIR/mouse_middle_click.pty"

    log_test_start "mouse_middle_click"
    PTY_TEST_NAME="mouse_middle_click"

    # Button code 1 = Middle
    PTY_SEND=$'\x1b[<1;20;10M\x1b[<1;20;10m' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Mouse: Down(Middle)" "$canonical_file" || return 1
    grep -a -q "Mouse: Up(Middle)" "$canonical_file" || return 1
}

mouse_right_click() {
    LOG_FILE="$E2E_LOG_DIR/mouse_right_click.log"
    local output_file="$E2E_LOG_DIR/mouse_right_click.pty"

    log_test_start "mouse_right_click"
    PTY_TEST_NAME="mouse_right_click"

    # Button code 2 = Right
    PTY_SEND=$'\x1b[<2;15;8M\x1b[<2;15;8m' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Mouse: Down(Right)" "$canonical_file" || return 1
    grep -a -q "Mouse: Up(Right)" "$canonical_file" || return 1
}

mouse_move_event() {
    LOG_FILE="$E2E_LOG_DIR/mouse_move_event.log"
    local output_file="$E2E_LOG_DIR/mouse_move_event.pty"

    log_test_start "mouse_move_event"
    PTY_TEST_NAME="mouse_move_event"

    # Button code 32 = motion bit set (no button held)
    PTY_SEND=$'\x1b[<32;12;6M' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Mouse: Moved" "$canonical_file" || return 1
}

mouse_drag_left() {
    LOG_FILE="$E2E_LOG_DIR/mouse_drag_left.log"
    local output_file="$E2E_LOG_DIR/mouse_drag_left.pty"

    log_test_start "mouse_drag_left"
    PTY_TEST_NAME="mouse_drag_left"

    # Button code 32 = motion with left button (0+32=32)
    # Note: Drag vs Move depends on whether a button was pressed first
    # Send click, then drag motion
    PTY_SEND=$'\x1b[<0;10;10M\x1b[<32;15;10M\x1b[<32;20;10M\x1b[<0;20;10m' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should see Down, motion events, then Up
    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Mouse: Down(Left)" "$canonical_file" || return 1
    grep -a -q "Mouse: Moved" "$canonical_file" || return 1
    grep -a -q "Mouse: Up(Left)" "$canonical_file" || return 1
}

mouse_scroll_events() {
    LOG_FILE="$E2E_LOG_DIR/mouse_scroll_events.log"
    local output_file="$E2E_LOG_DIR/mouse_scroll_events.pty"

    log_test_start "mouse_scroll_events"
    PTY_TEST_NAME="mouse_scroll_events"

    # Button codes 64=ScrollUp, 65=ScrollDown
    PTY_SEND=$'\x1b[<64;15;7M\x1b[<65;15;7M' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Mouse: ScrollUp" "$canonical_file" || return 1
    grep -a -q "Mouse: ScrollDown" "$canonical_file" || return 1
}

mouse_coordinates() {
    LOG_FILE="$E2E_LOG_DIR/mouse_coordinates.log"
    local output_file="$E2E_LOG_DIR/mouse_coordinates.pty"

    log_test_start "mouse_coordinates"
    PTY_TEST_NAME="mouse_coordinates"

    # SGR uses 1-based coords, harness displays 0-based (x-1, y-1)
    # Send click at (25, 12) in 1-based → expect (24, 11) in 0-based
    PTY_SEND=$'\x1b[<0;25;12M' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Verify coordinates are parsed correctly (0-indexed: 24, 11)
    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Mouse: Down(Left) @ 24,11" "$canonical_file" || return 1
}

mouse_large_coords() {
    LOG_FILE="$E2E_LOG_DIR/mouse_large_coords.log"
    local output_file="$E2E_LOG_DIR/mouse_large_coords.pty"

    log_test_start "mouse_large_coords"
    PTY_TEST_NAME="mouse_large_coords"

    # SGR supports coords > 223 (unlike legacy X10 protocol)
    # Send click at (300, 150) in 1-based → expect (299, 149) in 0-based
    PTY_SEND=$'\x1b[<0;300;150M' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Verify large coordinates are handled correctly
    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Mouse: Down(Left) @ 299,149" "$canonical_file" || return 1
}

mouse_shift_click() {
    LOG_FILE="$E2E_LOG_DIR/mouse_shift_click.log"
    local output_file="$E2E_LOG_DIR/mouse_shift_click.pty"

    log_test_start "mouse_shift_click"
    PTY_TEST_NAME="mouse_shift_click"

    # Button code with Shift modifier: 0 (left) + 4 (shift) = 4
    PTY_SEND=$'\x1b[<4;10;10M' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should still be a Left button Down
    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Mouse: Down(Left)" "$canonical_file" || return 1
}

mouse_ctrl_click() {
    LOG_FILE="$E2E_LOG_DIR/mouse_ctrl_click.log"
    local output_file="$E2E_LOG_DIR/mouse_ctrl_click.pty"

    log_test_start "mouse_ctrl_click"
    PTY_TEST_NAME="mouse_ctrl_click"

    # Button code with Ctrl modifier: 0 (left) + 16 (ctrl) = 16
    PTY_SEND=$'\x1b[<16;10;10M' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Mouse: Down(Left)" "$canonical_file" || return 1
}

mouse_alt_click() {
    LOG_FILE="$E2E_LOG_DIR/mouse_alt_click.log"
    local output_file="$E2E_LOG_DIR/mouse_alt_click.pty"

    log_test_start "mouse_alt_click"
    PTY_TEST_NAME="mouse_alt_click"

    # Button code with Alt/Meta modifier: 0 (left) + 8 (alt) = 8
    PTY_SEND=$'\x1b[<8;10;10M' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_ENABLE_MOUSE=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Mouse: Down(Left)" "$canonical_file" || return 1
}

FAILURES=0

# Basic button tests
run_case "mouse_left_click_release" mouse_left_click_release || FAILURES=$((FAILURES + 1))
run_case "mouse_middle_click" mouse_middle_click             || FAILURES=$((FAILURES + 1))
run_case "mouse_right_click" mouse_right_click               || FAILURES=$((FAILURES + 1))

# Motion and drag
run_case "mouse_move_event" mouse_move_event                 || FAILURES=$((FAILURES + 1))
run_case "mouse_drag_left" mouse_drag_left                   || FAILURES=$((FAILURES + 1))

# Scroll
run_case "mouse_scroll_events" mouse_scroll_events           || FAILURES=$((FAILURES + 1))

# Coordinate verification
run_case "mouse_coordinates" mouse_coordinates               || FAILURES=$((FAILURES + 1))
run_case "mouse_large_coords" mouse_large_coords             || FAILURES=$((FAILURES + 1))

# Modifier combinations
run_case "mouse_shift_click" mouse_shift_click               || FAILURES=$((FAILURES + 1))
run_case "mouse_ctrl_click" mouse_ctrl_click                 || FAILURES=$((FAILURES + 1))
run_case "mouse_alt_click" mouse_alt_click                   || FAILURES=$((FAILURES + 1))

exit "$FAILURES"

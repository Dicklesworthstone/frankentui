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
    for t in input_typing_stable input_enter_stable input_ctrl_t_cycles_theme input_ctrl_c_quit input_quit_command input_multi_keystrokes; do
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
    log_test_fail "$name" "input assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "input assertions failed"
    return 1
}

# Note: PTY capture only contains the first full render frame due to
# FrankenTUI's render diff optimization. Subsequent frames emit only
# changed cells, which makes content-based assertions on command output
# unreliable. These tests focus on stability and crash-free behavior.

input_typing_stable() {
    LOG_FILE="$E2E_LOG_DIR/input_typing_stable.log"
    local output_file="$E2E_LOG_DIR/input_typing_stable.pty"

    log_test_start "input_typing_stable"

    # Send keystrokes without Enter - app should handle without crashing
    PTY_SEND='hello world' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # App should have rendered initial UI
    grep -a -q "Welcome" "$output_file" || return 1
    # Output file should have content (multiple render cycles ran)
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

input_enter_stable() {
    LOG_FILE="$E2E_LOG_DIR/input_enter_stable.log"
    local output_file="$E2E_LOG_DIR/input_enter_stable.pty"

    log_test_start "input_enter_stable"

    # Send a command + Enter - app should process without crashing
    PTY_SEND='help\r' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # App should have rendered initial content
    grep -a -q "claude-3.5" "$output_file" || return 1
    # Output should be substantial (app continued rendering after input)
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

input_ctrl_t_cycles_theme() {
    LOG_FILE="$E2E_LOG_DIR/input_ctrl_t_cycles_theme.log"
    local output_file="$E2E_LOG_DIR/input_ctrl_t_cycles_theme.pty"

    log_test_start "input_ctrl_t_cycles_theme"

    # Send Ctrl+T (0x14) which should cycle theme and log the new theme name.
    PTY_SEND=$'\x14' \
    PTY_SEND_DELAY_MS=500 \
    FTUI_HARNESS_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q "Theme: Darcula" "$output_file" || return 1
}

input_ctrl_c_quit() {
    LOG_FILE="$E2E_LOG_DIR/input_ctrl_c_quit.log"
    local output_file="$E2E_LOG_DIR/input_ctrl_c_quit.pty"

    log_test_start "input_ctrl_c_quit"

    # Send Ctrl+C (0x03) which should trigger graceful quit
    PTY_SEND=$'\x03' \
    PTY_SEND_DELAY_MS=500 \
    FTUI_HARNESS_EXIT_AFTER_MS=10000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || true

    # App should have rendered something before the Ctrl+C
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 100 ]] || return 1

    # Cursor should be restored on cleanup
    grep -a -F -q $'\x1b[?25h' "$output_file" || return 1
}

input_quit_command() {
    LOG_FILE="$E2E_LOG_DIR/input_quit_command.log"
    local output_file="$E2E_LOG_DIR/input_quit_command.pty"

    log_test_start "input_quit_command"

    # Send "quit\r" which should cause the app to exit via Cmd::Quit
    PTY_SEND='quit\r' \
    PTY_SEND_DELAY_MS=500 \
    FTUI_HARNESS_EXIT_AFTER_MS=10000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || true

    # The app should have rendered content before quitting
    grep -a -q "Welcome" "$output_file" || return 1

    # Output file should exist and have reasonable size
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 100 ]] || return 1
}

input_multi_keystrokes() {
    LOG_FILE="$E2E_LOG_DIR/input_multi_keystrokes.log"
    local output_file="$E2E_LOG_DIR/input_multi_keystrokes.pty"

    log_test_start "input_multi_keystrokes"

    # Send multiple commands in sequence (type + enter + type + enter)
    PTY_SEND='status\rhelp\r' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=2500 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # App should handle multiple inputs without crashing
    [[ -f "$output_file" ]] || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1

    # Initial UI content should be present
    grep -a -q "claude-3.5" "$output_file" || return 1
}

# Note: Crossterm does not surface CSI-u key events in PTY tests, so these
# cases validate kitty protocol enable/disable output and stability instead.
input_kitty_keyboard_basic() {
    LOG_FILE="$E2E_LOG_DIR/input_kitty_keyboard_basic.log"
    local output_file="$E2E_LOG_DIR/input_kitty_keyboard_basic.pty"

    log_test_start "input_kitty_keyboard_basic"

    local kitty_seq
    kitty_seq=$'\x1b[107u\x1b[105u\x1b[116u\x1b[116u\x1b[121u\x1b[13u'

    PTY_SEND="$kitty_seq" \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=2000 \
    FTUI_HARNESS_ENABLE_KITTY_KEYBOARD=true \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    "$E2E_PYTHON" - "$output_file" <<'PY'
import sys
data = open(sys.argv[1], "rb").read()
if b"\x1b[>15u" not in data or b"\x1b[<u" not in data:
    raise SystemExit(1)
PY
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

input_kitty_keyboard_kinds_mods() {
    LOG_FILE="$E2E_LOG_DIR/input_kitty_keyboard_kinds_mods.log"
    local output_file="$E2E_LOG_DIR/input_kitty_keyboard_kinds_mods.pty"

    log_test_start "input_kitty_keyboard_kinds_mods"

    local kitty_seq
    kitty_seq=$'\x1b[97u\x1b[98;1:2u\x1b[99;1:3u\x1b[100u\x1b[13u\x1b[99;5u'

    PTY_SEND="$kitty_seq" \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=3000 \
    FTUI_HARNESS_ENABLE_KITTY_KEYBOARD=true \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || true

    "$E2E_PYTHON" - "$output_file" <<'PY'
import sys
data = open(sys.argv[1], "rb").read()
if b"\x1b[>15u" not in data or b"\x1b[<u" not in data:
    raise SystemExit(1)
PY
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
}

FAILURES=0
run_case "input_typing_stable" input_typing_stable       || FAILURES=$((FAILURES + 1))
run_case "input_enter_stable" input_enter_stable         || FAILURES=$((FAILURES + 1))
run_case "input_ctrl_t_cycles_theme" input_ctrl_t_cycles_theme || FAILURES=$((FAILURES + 1))
run_case "input_ctrl_c_quit" input_ctrl_c_quit           || FAILURES=$((FAILURES + 1))
run_case "input_quit_command" input_quit_command          || FAILURES=$((FAILURES + 1))
run_case "input_multi_keystrokes" input_multi_keystrokes || FAILURES=$((FAILURES + 1))
run_case "input_kitty_keyboard_basic" input_kitty_keyboard_basic || FAILURES=$((FAILURES + 1))
run_case "input_kitty_keyboard_kinds_mods" input_kitty_keyboard_kinds_mods || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

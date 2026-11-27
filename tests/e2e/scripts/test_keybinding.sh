#!/bin/bash
# Keybinding E2E PTY Tests
#
# Tests for bd-2vne.8: Pi-style keybinding behaviors
# - Ctrl+C clears input, cancels task, or quits
# - Esc clears input, cancels task, or closes overlay
# - Esc Esc toggles tree view overlay
#
# Environment:
# - FTUI_HARNESS_LOG_KEYS=1 enables key event logging
# - FTUI_HARNESS_SUPPRESS_WELCOME=1 hides welcome text

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

# JSONL logging with verbose schema per bd-2vne.8 requirements
# Schema: run_id, case, env, seed, timings, checksums, capabilities, outcome
RUN_ID="$("${E2E_PYTHON:-python3}" -c 'import time; print(time.time_ns())')" || exit 2
RUN_ID="keybind-${RUN_ID}-$$"
JSONL_LOG="${E2E_LOG_DIR:-/tmp/ftui_e2e_logs}/keybinding_e2e.jsonl"
mkdir -p "$(dirname "$JSONL_LOG")"

# Collect environment info for logging
get_env_info() {
    local term_type="${TERM:-unknown}"
    local term_program="${TERM_PROGRAM:-unknown}"
    local shell_type="${SHELL:-unknown}"
    local os_type
    os_type="$(uname -s)"
    printf '{"TERM":"%s","TERM_PROGRAM":"%s","SHELL":"%s","OS":"%s"}' "$term_type" "$term_program" "$shell_type" "$os_type"
}

ESC_WINDOW_MS=250
CASE_FAIL_REASON=""
CASE_SEND_DELAY_MS=0

# Compute checksum of PTY output file
compute_checksum() {
    local file="$1"
    if [[ -f "$file" ]]; then
        sha256sum "$file" 2>/dev/null | cut -d' ' -f1 || shasum -a 256 "$file" 2>/dev/null | cut -d' ' -f1 || echo "no-checksum"
    else
        echo "no-file"
    fi
}

# Assert a substring is present in PTY capture, recording specific failure reason.
assert_capture_contains() {
    local output_file="$1"
    local needle="$2"
    local label="$3"
    if ! grep -a -F -q -- "$needle" "$output_file"; then
        CASE_FAIL_REASON="missing:${label}"
        return 1
    fi
    return 0
}

# Assert a substring is absent from PTY capture, recording specific failure reason.
assert_capture_absent() {
    local output_file="$1"
    local needle="$2"
    local label="$3"
    if grep -a -F -q -- "$needle" "$output_file"; then
        CASE_FAIL_REASON="unexpected:${label}"
        return 1
    fi
    return 0
}

# Log a test result in JSONL format
# Args: case_name, outcome (passed|failed|skipped), start_ms, end_ms, output_file, error_msg, send_delay_ms
log_jsonl() {
    local case_name="$1"
    local outcome="$2"
    local start_ms="$3"
    local end_ms="$4"
    local output_file="${5:-}"
    local error_msg="${6:-}"
    local send_delay_ms="${7:-0}"

    local duration_ms=$((end_ms - start_ms))
    local checksum
    checksum="$(compute_checksum "$output_file")"
    local env_json
    env_json="$(get_env_info)"
    local ts
    ts="$("${E2E_PYTHON:-python3}" -c 'from datetime import datetime; print(datetime.now().astimezone().isoformat(timespec="seconds"))')" || return 2

    # Escape error message for JSON
    local safe_error
    safe_error="$(printf '%s' "$error_msg" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr '\n' ' ')"

    # Write JSONL entry (preserving existing fields and adding bd-g00-root-epic-ewths.6.17.2 diagnostics)
    printf '{"run_id":"%s","case":"%s","timestamp":"%s","env":%s,"identity":%s,"seed":0,"send_delay_ms":%s,"esc_window_ms":%s,"timings":{"start_ms":%s,"end_ms":%s,"duration_ms":%s},"duration_ms":%s,"hash":"%s","checksums":{"output":"%s"},"capabilities":{"pty":true,"inline_mode":true},"outcome":"%s","error":"%s"}\n' \
        "$RUN_ID" "$case_name" "$ts" "$env_json" "$env_json" "$send_delay_ms" "$ESC_WINDOW_MS" "$start_ms" "$end_ms" "$duration_ms" "$duration_ms" "$checksum" "$checksum" "$outcome" "$safe_error" \
        >> "$JSONL_LOG"
}

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/keybinding_missing.log"
    skip_ts="$(e2e_monotonic_ms)" || exit 2
    for t in \
        keybind_ctrl_c_clears_input \
        keybind_ctrl_c_cancels_task \
        keybind_esc_clears_input \
        keybind_esc_cancels_task \
        keybind_esc_esc_toggles_tree \
        keybind_esc_closes_tree \
        keybind_ctrl_d_soft_quit \
        keybind_ctrl_q_hard_quit \
    ; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        log_jsonl "$t" "skipped" "$skip_ts" "$skip_ts" "" "ftui-harness binary missing" 0
    done
    exit 0
fi

run_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2
    local output_file="$E2E_LOG_DIR/${name}.pty"
    CASE_FAIL_REASON=""
    CASE_SEND_DELAY_MS=0

    local rc=0
    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)" || return 2
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        log_jsonl "$name" "passed" "$start_ms" "$end_ms" "$output_file" "" "$CASE_SEND_DELAY_MS"
        return 0
    else
        rc=$?
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))
    local reason="${CASE_FAIL_REASON:-pty_run_failed:exit_${rc}}"
    log_test_fail "$name" "$reason"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "$reason"
    log_jsonl "$name" "failed" "$start_ms" "$end_ms" "$output_file" "$reason" "$CASE_SEND_DELAY_MS"
    return 1
}

# Wait until ftui-harness enters raw mode and renders its first frame (" Command "
# input box header) before sending keystrokes so tcsetattr(TCSAFLUSH) cannot
# discard early typeahead on slower/debug macOS builds.
HARNESS_READY_MARKER=' Command '

# Test: Ctrl+C clears input when text is present
# Per keybinding-policy.md section 6.1 priority 3
keybind_ctrl_c_clears_input() {
    LOG_FILE="$E2E_LOG_DIR/keybind_ctrl_c_clears_input.log"
    local output_file="$E2E_LOG_DIR/keybind_ctrl_c_clears_input.pty"
    CASE_SEND_DELAY_MS=100

    log_test_start "keybind_ctrl_c_clears_input"

    # Type text, then Ctrl+C (0x03) - should clear input, not quit
    # Then type more and Enter to show the app is still running
    local rc=0
    PTY_SEND_AFTER_OUTPUT="$HARNESS_READY_MARKER" \
    PTY_SEND='hello\x03status\r' \
    PTY_SEND_DELAY_MS="$CASE_SEND_DELAY_MS" \
    FTUI_HARNESS_EXIT_AFTER_MS=3000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        CASE_FAIL_REASON="pty_run_exit_${rc}"
        return "$rc"
    fi

    # Should see "(Input cleared)" in output indicating Ctrl+C cleared input
    assert_capture_contains "$output_file" "(Input cleared)" "input_cleared" || return 1
    # App should still be running (processed status command)
    assert_capture_contains "$output_file" "Model: claude-3.5" "status_command_output" || return 1
}

# Test: Ctrl+C cancels running task (when input is empty)
# Per keybinding-policy.md section 6.1 priority 4
keybind_ctrl_c_cancels_task() {
    LOG_FILE="$E2E_LOG_DIR/keybind_ctrl_c_cancels_task.log"
    local output_file="$E2E_LOG_DIR/keybind_ctrl_c_cancels_task.pty"
    CASE_SEND_DELAY_MS=100

    log_test_start "keybind_ctrl_c_cancels_task"

    # Start a task (search command), wait for it to start, then Ctrl+C
    local rc=0
    PTY_SEND_AFTER_OUTPUT="$HARNESS_READY_MARKER" \
    PTY_SEND='search\r\x03status\r' \
    PTY_SEND_DELAY_MS="$CASE_SEND_DELAY_MS" \
    FTUI_HARNESS_EXIT_AFTER_MS=4000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=7 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        CASE_FAIL_REASON="pty_run_exit_${rc}"
        return "$rc"
    fi

    # Should see task cancellation message
    assert_capture_contains "$output_file" "(Task cancelled)" "task_cancelled" || return 1
    # Should NOT see "Search complete" (task was cancelled)
    assert_capture_absent "$output_file" "Search complete" "search_complete" || return 1
}

# Test: Esc clears input when text is present
# Per keybinding-policy.md section 6.1 priority 7
keybind_esc_clears_input() {
    LOG_FILE="$E2E_LOG_DIR/keybind_esc_clears_input.log"
    local output_file="$E2E_LOG_DIR/keybind_esc_clears_input.pty"
    CASE_SEND_DELAY_MS=100

    log_test_start "keybind_esc_clears_input"

    # Type text, then Esc (0x1B), then wait past the 50ms parser grace and 250ms
    # double-Esc window so bare Esc cannot coalesce with 's' into Alt+s even under load.
    local rc=0
    PTY_SEND_AFTER_OUTPUT="$HARNESS_READY_MARKER" \
    PTY_SEND_SEQUENCE='[{"delay_ms":100,"text":"hello"},{"delay_ms":250,"text":"\u001b"},{"delay_ms":600,"text":"status\r"}]' \
    FTUI_HARNESS_EXIT_AFTER_MS=3000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        CASE_FAIL_REASON="pty_run_exit_${rc}"
        return "$rc"
    fi

    # Should see input cleared message
    assert_capture_contains "$output_file" "(Input cleared)" "input_cleared" || return 1
    assert_capture_contains "$output_file" "Model: claude-3.5" "status_command_output" || return 1
}

# Test: Esc cancels running task (when input is empty)
# Per keybinding-policy.md section 6.1 priority 8
keybind_esc_cancels_task() {
    LOG_FILE="$E2E_LOG_DIR/keybind_esc_cancels_task.log"
    local output_file="$E2E_LOG_DIR/keybind_esc_cancels_task.pty"
    CASE_SEND_DELAY_MS=100

    log_test_start "keybind_esc_cancels_task"

    # Start a task, wait for it to start, then Esc
    local rc=0
    PTY_SEND_AFTER_OUTPUT="$HARNESS_READY_MARKER" \
    PTY_SEND_SEQUENCE='[{"delay_ms":100,"text":"search\r"},{"delay_ms":250,"text":"\u001b"},{"delay_ms":600,"text":"status\r"}]' \
    FTUI_HARNESS_EXIT_AFTER_MS=4000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=7 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        CASE_FAIL_REASON="pty_run_exit_${rc}"
        return "$rc"
    fi

    # Should see task cancellation message
    assert_capture_contains "$output_file" "(Task cancelled)" "task_cancelled" || return 1
}

# Test: Esc Esc (double Esc within timeout) toggles tree view
# Per keybinding-policy.md section 6.1 priority 9
keybind_esc_esc_toggles_tree() {
    LOG_FILE="$E2E_LOG_DIR/keybind_esc_esc_toggles_tree.log"
    local output_file="$E2E_LOG_DIR/keybind_esc_esc_toggles_tree.pty"
    CASE_SEND_DELAY_MS=150

    log_test_start "keybind_esc_esc_toggles_tree"

    # Separate Escape presses beyond the 50ms parser grace but within the 250ms
    # double-tap window, sent only after first frame readiness.
    local rc=0
    PTY_SEND_AFTER_OUTPUT="$HARNESS_READY_MARKER" \
    PTY_SEND_SEQUENCE='[{"delay_ms":150,"text":"\u001b"},{"delay_ms":300,"text":"\u001b"}]' \
    FTUI_HARNESS_EXIT_AFTER_MS=2000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        CASE_FAIL_REASON="pty_run_exit_${rc}"
        return "$rc"
    fi

    # The open overlay covers the log message; require its visible content.
    assert_capture_contains "$output_file" "Tree View Overlay" "tree_view_overlay_title" || return 1
    assert_capture_contains "$output_file" "name: claude-3.5" "tree_view_overlay_body" || return 1
}

# Test: Esc closes tree view when open
# Per keybinding-policy.md section 6.1 priority 6
keybind_esc_closes_tree() {
    LOG_FILE="$E2E_LOG_DIR/keybind_esc_closes_tree.log"
    local output_file="$E2E_LOG_DIR/keybind_esc_closes_tree.pty"
    CASE_SEND_DELAY_MS=100

    log_test_start "keybind_esc_closes_tree"

    # Open tree via 'tree' command, then close with single Esc after frame renders
    local rc=0
    PTY_SEND_AFTER_OUTPUT="$HARNESS_READY_MARKER" \
    PTY_SEND_SEQUENCE='[{"delay_ms":100,"text":"tree\r"},{"delay_ms":300,"text":"\u001b"}]' \
    FTUI_HARNESS_EXIT_AFTER_MS=2500 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        CASE_FAIL_REASON="pty_run_exit_${rc}"
        return "$rc"
    fi

    # Should see tree opened and then closed
    assert_capture_contains "$output_file" "Tree view opened" "tree_view_opened" || return 1
    assert_capture_contains "$output_file" "(Tree view closed)" "tree_view_closed" || return 1
}

# Test: Ctrl+D soft quit (cancels task if running, else quits)
# Per keybinding-policy.md section 6.1 priority 10
keybind_ctrl_d_soft_quit() {
    LOG_FILE="$E2E_LOG_DIR/keybind_ctrl_d_soft_quit.log"
    local output_file="$E2E_LOG_DIR/keybind_ctrl_d_soft_quit.pty"
    CASE_SEND_DELAY_MS=100

    log_test_start "keybind_ctrl_d_soft_quit"

    # Start a task, then Ctrl+D should cancel it
    local rc=0
    PTY_SEND_AFTER_OUTPUT="$HARNESS_READY_MARKER" \
    PTY_SEND='search\r\x04status\r' \
    PTY_SEND_DELAY_MS="$CASE_SEND_DELAY_MS" \
    FTUI_HARNESS_EXIT_AFTER_MS=4000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=7 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        CASE_FAIL_REASON="pty_run_exit_${rc}"
        return "$rc"
    fi

    # Should see soft quit cancellation message
    assert_capture_contains "$output_file" "(Task cancelled via Ctrl+D)" "task_cancelled_ctrl_d" || return 1
}

# Test: Ctrl+Q hard quit (immediate exit)
# Per keybinding-policy.md section 6.1 priority 11
keybind_ctrl_q_hard_quit() {
    LOG_FILE="$E2E_LOG_DIR/keybind_ctrl_q_hard_quit.log"
    local output_file="$E2E_LOG_DIR/keybind_ctrl_q_hard_quit.pty"
    CASE_SEND_DELAY_MS=100

    log_test_start "keybind_ctrl_q_hard_quit"

    # Ctrl+Q (0x11) should quit immediately
    local rc=0
    PTY_SEND_AFTER_OUTPUT="$HARNESS_READY_MARKER" \
    PTY_SEND='\x11' \
    PTY_SEND_DELAY_MS="$CASE_SEND_DELAY_MS" \
    FTUI_HARNESS_EXIT_AFTER_MS=10000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        CASE_FAIL_REASON="pty_run_exit_${rc}"
        return "$rc"
    fi

    # App should have exited - cursor should be restored
    assert_capture_contains "$output_file" $'\x1b[?25h' "cursor_restore_escape" || return 1
}

# Run all keybinding tests
log_info "Starting keybinding E2E tests (run_id: $RUN_ID)"
log_info "JSONL log: $JSONL_LOG"

FAILURES=0
run_case "keybind_ctrl_c_clears_input" keybind_ctrl_c_clears_input || FAILURES=$((FAILURES + 1))
run_case "keybind_ctrl_c_cancels_task" keybind_ctrl_c_cancels_task || FAILURES=$((FAILURES + 1))
run_case "keybind_esc_clears_input" keybind_esc_clears_input       || FAILURES=$((FAILURES + 1))
run_case "keybind_esc_cancels_task" keybind_esc_cancels_task       || FAILURES=$((FAILURES + 1))
run_case "keybind_esc_esc_toggles_tree" keybind_esc_esc_toggles_tree || FAILURES=$((FAILURES + 1))
run_case "keybind_esc_closes_tree" keybind_esc_closes_tree         || FAILURES=$((FAILURES + 1))
run_case "keybind_ctrl_d_soft_quit" keybind_ctrl_d_soft_quit       || FAILURES=$((FAILURES + 1))
run_case "keybind_ctrl_q_hard_quit" keybind_ctrl_q_hard_quit       || FAILURES=$((FAILURES + 1))

# Summary
PASSED=$((8 - FAILURES))
log_info "Keybinding E2E tests complete: $PASSED passed, $FAILURES failed"
log_info "JSONL log written to: $JSONL_LOG"

exit "$FAILURES"

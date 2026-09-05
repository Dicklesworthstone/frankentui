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
    local shell_type="${SHELL:-unknown}"
    local os_type
    os_type="$(uname -s)"
    printf '{"TERM":"%s","SHELL":"%s","OS":"%s"}' "$term_type" "$shell_type" "$os_type"
}

# Compute checksum of PTY output file
compute_checksum() {
    local file="$1"
    if [[ -f "$file" ]]; then
        sha256sum "$file" 2>/dev/null | cut -d' ' -f1 || echo "no-checksum"
    else
        echo "no-file"
    fi
}

# Log a test result in JSONL format
# Args: case_name, outcome (passed|failed|skipped), start_ms, end_ms, output_file, error_msg
log_jsonl() {
    local case_name="$1"
    local outcome="$2"
    local start_ms="$3"
    local end_ms="$4"
    local output_file="${5:-}"
    local error_msg="${6:-}"

    local duration_ms=$((end_ms - start_ms))
    local checksum
    checksum="$(compute_checksum "$output_file")"
    local env_json
    env_json="$(get_env_info)"
    local ts
    ts="$("${E2E_PYTHON:-python3}" -c 'from datetime import datetime; print(datetime.now().astimezone().isoformat(timespec="seconds"))')" || return 2

    # Escape error message for JSON
    local safe_error
    safe_error="$(printf '%s' "$error_msg" | sed 's/"/\\"/g' | tr '\n' ' ')"

    # Write JSONL entry
    printf '{"run_id":"%s","case":"%s","timestamp":"%s","env":%s,"seed":0,"timings":{"start_ms":%s,"end_ms":%s,"duration_ms":%s},"checksums":{"output":"%s"},"capabilities":{"pty":true,"inline_mode":true},"outcome":"%s","error":"%s"}\n' \
        "$RUN_ID" "$case_name" "$ts" "$env_json" "$start_ms" "$end_ms" "$duration_ms" "$checksum" "$outcome" "$safe_error" \
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
        log_jsonl "$t" "skipped" "$skip_ts" "$skip_ts" "" "ftui-harness binary missing"
    done
    exit 0
fi

run_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2
    local output_file="$E2E_LOG_DIR/${name}.pty"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)" || return 2
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        log_jsonl "$name" "passed" "$start_ms" "$end_ms" "$output_file" ""
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "keybinding assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "keybinding assertion failed"
    log_jsonl "$name" "failed" "$start_ms" "$end_ms" "$output_file" "keybinding assertion failed"
    return 1
}

# Test: Ctrl+C clears input when text is present
# Per keybinding-policy.md section 6.1 priority 3
keybind_ctrl_c_clears_input() {
    LOG_FILE="$E2E_LOG_DIR/keybind_ctrl_c_clears_input.log"
    local output_file="$E2E_LOG_DIR/keybind_ctrl_c_clears_input.pty"

    log_test_start "keybind_ctrl_c_clears_input"

    # Type text, then Ctrl+C (0x03) - should clear input, not quit
    # Then type more and Enter to show the app is still running
    PTY_SEND='hello\x03status\r' \
    PTY_SEND_DELAY_MS=500 \
    FTUI_HARNESS_EXIT_AFTER_MS=3000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should see "(Input cleared)" in output indicating Ctrl+C cleared input
    grep -a -q "(Input cleared)" "$output_file" || return 1
    # App should still be running (processed status command)
    grep -a -q "claude-3.5" "$output_file" || return 1
}

# Test: Ctrl+C cancels running task (when input is empty)
# Per keybinding-policy.md section 6.1 priority 4
keybind_ctrl_c_cancels_task() {
    LOG_FILE="$E2E_LOG_DIR/keybind_ctrl_c_cancels_task.log"
    local output_file="$E2E_LOG_DIR/keybind_ctrl_c_cancels_task.pty"

    log_test_start "keybind_ctrl_c_cancels_task"

    # Start a task (search command), wait for it to start, then Ctrl+C
    PTY_SEND='search\r\x03status\r' \
    PTY_SEND_DELAY_MS=600 \
    FTUI_HARNESS_EXIT_AFTER_MS=4000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=7 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should see task cancellation message
    grep -a -q "(Task cancelled)" "$output_file" || return 1
    # Should NOT see "Search complete" (task was cancelled)
    if grep -a -q "Search complete" "$output_file"; then
        return 1
    fi
}

# Test: Esc clears input when text is present
# Per keybinding-policy.md section 6.1 priority 7
keybind_esc_clears_input() {
    LOG_FILE="$E2E_LOG_DIR/keybind_esc_clears_input.log"
    local output_file="$E2E_LOG_DIR/keybind_esc_clears_input.pty"

    log_test_start "keybind_esc_clears_input"

    # Type text, then Esc (0x1B), then type more to show app is still running
    PTY_SEND='hello\x1bstatus\r' \
    PTY_SEND_DELAY_MS=500 \
    FTUI_HARNESS_EXIT_AFTER_MS=3000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should see input cleared message
    grep -a -q "(Input cleared)" "$output_file" || return 1
}

# Test: Esc cancels running task (when input is empty)
# Per keybinding-policy.md section 6.1 priority 8
keybind_esc_cancels_task() {
    LOG_FILE="$E2E_LOG_DIR/keybind_esc_cancels_task.log"
    local output_file="$E2E_LOG_DIR/keybind_esc_cancels_task.pty"

    log_test_start "keybind_esc_cancels_task"

    # Start a task, wait for it to start, then Esc
    PTY_SEND='search\r\x1bstatus\r' \
    PTY_SEND_DELAY_MS=600 \
    FTUI_HARNESS_EXIT_AFTER_MS=4000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=7 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should see task cancellation message
    grep -a -q "(Task cancelled)" "$output_file" || return 1
}

# Test: Esc Esc (double Esc within timeout) toggles tree view
# Per keybinding-policy.md section 6.1 priority 9
keybind_esc_esc_toggles_tree() {
    LOG_FILE="$E2E_LOG_DIR/keybind_esc_esc_toggles_tree.log"
    local output_file="$E2E_LOG_DIR/keybind_esc_esc_toggles_tree.pty"

    log_test_start "keybind_esc_esc_toggles_tree"

    # Send Esc Esc rapidly (within 250ms timeout)
    # The ActionMapper should detect this as a double-Esc sequence
    PTY_SEND='\x1b\x1b' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=2000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should see tree view toggle message
    grep -a -q "(Tree view opened)" "$output_file" || return 1
}

# Test: Esc closes tree view when open
# Per keybinding-policy.md section 6.1 priority 6
keybind_esc_closes_tree() {
    LOG_FILE="$E2E_LOG_DIR/keybind_esc_closes_tree.log"
    local output_file="$E2E_LOG_DIR/keybind_esc_closes_tree.pty"

    log_test_start "keybind_esc_closes_tree"

    # Open tree with Esc Esc, then close with single Esc
    # Need to use 'tree' command as alternative since double-Esc timing is tricky
    PTY_SEND='tree\r\x1b' \
    PTY_SEND_DELAY_MS=400 \
    FTUI_HARNESS_EXIT_AFTER_MS=2500 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should see tree opened and then closed
    grep -a -q "Tree view opened" "$output_file" || return 1
    grep -a -q "(Tree view closed)" "$output_file" || return 1
}

# Test: Ctrl+D soft quit (cancels task if running, else quits)
# Per keybinding-policy.md section 6.1 priority 10
keybind_ctrl_d_soft_quit() {
    LOG_FILE="$E2E_LOG_DIR/keybind_ctrl_d_soft_quit.log"
    local output_file="$E2E_LOG_DIR/keybind_ctrl_d_soft_quit.pty"

    log_test_start "keybind_ctrl_d_soft_quit"

    # Start a task, then Ctrl+D should cancel it
    PTY_SEND='search\r\x04status\r' \
    PTY_SEND_DELAY_MS=500 \
    FTUI_HARNESS_EXIT_AFTER_MS=4000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=7 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || true

    # Should see soft quit cancellation message
    grep -a -q "(Task cancelled via Ctrl+D)" "$output_file" || return 1
}

# Test: Ctrl+Q hard quit (immediate exit)
# Per keybinding-policy.md section 6.1 priority 11
keybind_ctrl_q_hard_quit() {
    LOG_FILE="$E2E_LOG_DIR/keybind_ctrl_q_hard_quit.log"
    local output_file="$E2E_LOG_DIR/keybind_ctrl_q_hard_quit.pty"

    log_test_start "keybind_ctrl_q_hard_quit"

    # Ctrl+Q (0x11) should quit immediately
    PTY_SEND='\x11' \
    PTY_SEND_DELAY_MS=500 \
    FTUI_HARNESS_EXIT_AFTER_MS=10000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || true

    # App should have exited - cursor should be restored
    grep -a -F -q $'\x1b[?25h' "$output_file" || return 1
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

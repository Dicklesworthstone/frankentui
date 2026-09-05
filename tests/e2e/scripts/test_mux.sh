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

E2E_SUITE_SCRIPT="$SCRIPT_DIR/test_mux.sh"
export E2E_SUITE_SCRIPT
ONLY_CASE="${E2E_ONLY_CASE:-}"

ALL_CASES=(
    mux_baseline_scroll_region
    mux_tmux_disables_scroll_region
    mux_screen_disables_scroll_region
    mux_zellij_disables_scroll_region
)

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/mux_missing.log"
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
    log_test_fail "$name" "mux assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "mux assertions failed"
    return 1
}

assert_has_scroll_region() {
    local output_file="$1"
    grep -a -F -q $'\x1b[1;18r' "$output_file"
}

assert_no_scroll_region() {
    local output_file="$1"
    if grep -a -o -E $'\x1b\\[[0-9]+;[0-9]+r' "$output_file" >/dev/null 2>&1; then
        return 1
    fi
    return 0
}

mux_baseline_scroll_region() {
    LOG_FILE="$E2E_LOG_DIR/mux_baseline_scroll_region.log"
    local output_file="$E2E_LOG_DIR/mux_baseline_scroll_region.pty"

    log_test_start "mux_baseline_scroll_region"

    TERM="xterm-256color" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=inline \
    FTUI_HARNESS_UI_HEIGHT=6 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1
    grep -a -q "claude-3.5" "$output_file" || return 1

    assert_has_scroll_region "$output_file" || return 1
}

mux_tmux_disables_scroll_region() {
    LOG_FILE="$E2E_LOG_DIR/mux_tmux_disables_scroll_region.log"
    local output_file="$E2E_LOG_DIR/mux_tmux_disables_scroll_region.pty"

    log_test_start "mux_tmux_disables_scroll_region"

    TMUX="/tmp/tmux-test" \
    TERM="screen-256color" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=inline \
    FTUI_HARNESS_UI_HEIGHT=6 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1
    grep -a -q "claude-3.5" "$output_file" || return 1

    assert_no_scroll_region "$output_file" || return 1
}

mux_screen_disables_scroll_region() {
    LOG_FILE="$E2E_LOG_DIR/mux_screen_disables_scroll_region.log"
    local output_file="$E2E_LOG_DIR/mux_screen_disables_scroll_region.pty"

    log_test_start "mux_screen_disables_scroll_region"

    STY="screen" \
    TERM="screen" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=inline \
    FTUI_HARNESS_UI_HEIGHT=6 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1
    grep -a -q "claude-3.5" "$output_file" || return 1

    assert_no_scroll_region "$output_file" || return 1
}

mux_zellij_disables_scroll_region() {
    LOG_FILE="$E2E_LOG_DIR/mux_zellij_disables_scroll_region.log"
    local output_file="$E2E_LOG_DIR/mux_zellij_disables_scroll_region.pty"

    log_test_start "mux_zellij_disables_scroll_region"

    ZELLIJ="1" \
    TERM="xterm-256color" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=inline \
    FTUI_HARNESS_UI_HEIGHT=6 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1
    grep -a -q "claude-3.5" "$output_file" || return 1

    assert_no_scroll_region "$output_file" || return 1
}

FAILURES=0
run_case "mux_baseline_scroll_region" mux_baseline_scroll_region || FAILURES=$((FAILURES + 1))
run_case "mux_tmux_disables_scroll_region" mux_tmux_disables_scroll_region || FAILURES=$((FAILURES + 1))
run_case "mux_screen_disables_scroll_region" mux_screen_disables_scroll_region || FAILURES=$((FAILURES + 1))
run_case "mux_zellij_disables_scroll_region" mux_zellij_disables_scroll_region || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

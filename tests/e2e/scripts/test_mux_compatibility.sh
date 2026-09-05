#!/bin/bash
# =============================================================================
# test_mux_compatibility.sh - Mux Compatibility Matrix + Fallback Tests
# =============================================================================
#
# E2E test suite for bd-1rz0.19: Mux Compatibility Matrix + Fallback Tests
#
# Tests tmux/screen/zellij fallbacks and verifies safe overlay strategy
# with detailed JSONL logging including:
# - Environment variables
# - Terminal capabilities detected
# - Strategy selected
# - ANSI sequences emitted
# - Timing information
# - Checksums for reproducibility
#
# Usage:
#   ./test_mux_compatibility.sh                    # Run all tests
#   E2E_ONLY_CASE=sync_output_baseline ./test_mux_compatibility.sh  # Run one test
#
# Output:
#   - JSONL logs in $E2E_LOG_DIR/mux_compat_*.jsonl
#   - Summary in $E2E_RESULTS_DIR/mux_compat_summary.json
#
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

E2E_SUITE_SCRIPT="$SCRIPT_DIR/test_mux_compatibility.sh"
export E2E_SUITE_SCRIPT
ONLY_CASE="${E2E_ONLY_CASE:-}"

# =============================================================================
# JSONL Logging Functions
# =============================================================================

# Unique run ID for this invocation
RUN_ID="${RUN_ID:-$(date +%s)-$$}"
export RUN_ID

# JSONL output file
JSONL_LOG="$E2E_LOG_DIR/mux_compat_${RUN_ID}.jsonl"
mkdir -p "$(dirname "$JSONL_LOG")"

emit_jsonl() {
    local event="$1"
    shift
    local ts
    ts="$("${E2E_PYTHON:-python3}" -c 'from datetime import datetime, timezone; print(datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z"))')" || return 2

    # Build JSON with jq if available, otherwise use printf
    if command -v jq >/dev/null 2>&1; then
        local json
        json=$(jq -n \
            --arg run_id "$RUN_ID" \
            --arg timestamp "$ts" \
            --arg event "$event" \
            --argjson data "$1" \
            '{run_id:$run_id,timestamp:$timestamp,event:$event,data:$data}')
        echo "$json" >> "$JSONL_LOG"
    else
        printf '{"run_id":"%s","timestamp":"%s","event":"%s","data":%s}\n' \
            "$RUN_ID" "$ts" "$event" "$1" >> "$JSONL_LOG"
    fi
}

emit_env() {
    local env_json
    env_json=$(jq -n \
        --arg term "${TERM:-}" \
        --arg term_program "${TERM_PROGRAM:-}" \
        --arg colorterm "${COLORTERM:-}" \
        --arg tmux "${TMUX:-}" \
        --arg sty "${STY:-}" \
        --arg zellij "${ZELLIJ:-}" \
        --arg no_color "${NO_COLOR:-}" \
        --arg pty_cols "${PTY_COLS:-80}" \
        --arg pty_rows "${PTY_ROWS:-24}" \
        '{TERM:$term,TERM_PROGRAM:$term_program,COLORTERM:$colorterm,TMUX:$tmux,STY:$sty,ZELLIJ:$zellij,NO_COLOR:$no_color,PTY_COLS:$pty_cols,PTY_ROWS:$pty_rows}')
    emit_jsonl "environment" "$env_json"
}

emit_case_start() {
    local case_name="$1"
    emit_jsonl "case_start" "{\"name\":\"$case_name\"}"
}

emit_case_end() {
    local case_name="$1"
    local status="$2"
    local duration_ms="$3"
    local assertions="${4:-[]}"
    emit_jsonl "case_end" "{\"name\":\"$case_name\",\"status\":\"$status\",\"duration_ms\":$duration_ms,\"assertions\":$assertions}"
}

emit_assertion() {
    local assertion="$1"
    local result="$2"
    local details="${3:-null}"
    emit_jsonl "assertion" "{\"assertion\":\"$assertion\",\"result\":\"$result\",\"details\":$details}"
}

# =============================================================================
# Test Cases
# =============================================================================

ALL_CASES=(
    sync_output_baseline
    sync_output_disabled_in_tmux
    sync_output_disabled_in_screen
    sync_output_disabled_in_zellij
    scroll_region_baseline
    scroll_region_disabled_in_tmux
    scroll_region_disabled_in_screen
    scroll_region_disabled_in_zellij
    overlay_strategy_in_tmux
    overlay_strategy_in_screen
    overlay_strategy_in_zellij
    passthrough_wrap_tmux
    passthrough_wrap_screen
    passthrough_wrap_zellij_not_needed
)

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/mux_compat_missing.log"
    emit_jsonl "suite_skip" "{\"reason\":\"ftui-harness binary missing\"}"
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
    emit_case_start "$name"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)" || return 2
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        emit_case_end "$name" "passed" "$duration_ms"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "assertions failed"
    emit_case_end "$name" "failed" "$duration_ms"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertions failed"
    return 1
}

# =============================================================================
# Assertion Helpers
# =============================================================================

# Check for sync output sequences (CSI ? 2026 h/l)
assert_has_sync_output() {
    local output_file="$1"
    if grep -a -o -F $'\x1b[?2026h' "$output_file" >/dev/null 2>&1; then
        emit_assertion "has_sync_output" "pass" "null"
        return 0
    fi
    emit_assertion "has_sync_output" "fail" "{\"reason\":\"CSI ?2026h not found\"}"
    return 1
}

assert_no_sync_output() {
    local output_file="$1"
    if grep -a -o -F $'\x1b[?2026h' "$output_file" >/dev/null 2>&1; then
        emit_assertion "no_sync_output" "fail" "{\"reason\":\"CSI ?2026h found but should not be present\"}"
        return 1
    fi
    emit_assertion "no_sync_output" "pass" "null"
    return 0
}

# Check for scroll region sequences (CSI n;m r)
assert_has_scroll_region() {
    local output_file="$1"
    if grep -a -o -E $'\x1b\\[[0-9]+;[0-9]+r' "$output_file" >/dev/null 2>&1; then
        emit_assertion "has_scroll_region" "pass" "null"
        return 0
    fi
    emit_assertion "has_scroll_region" "fail" "{\"reason\":\"DECSTBM sequence not found\"}"
    return 1
}

assert_no_scroll_region() {
    local output_file="$1"
    if grep -a -o -E $'\x1b\\[[0-9]+;[0-9]+r' "$output_file" >/dev/null 2>&1; then
        emit_assertion "no_scroll_region" "fail" "{\"reason\":\"DECSTBM found but should not be present in mux\"}"
        return 1
    fi
    emit_assertion "no_scroll_region" "pass" "null"
    return 0
}

# Check for tmux passthrough wrapping (ESC P tmux; ... ESC \)
assert_no_passthrough_wrap() {
    local output_file="$1"
    # tmux passthrough: ESC P tmux; ... ESC \
    if grep -a -o -F $'\x1bPtmux;' "$output_file" >/dev/null 2>&1; then
        emit_assertion "no_passthrough_wrap" "fail" "{\"reason\":\"tmux passthrough found but not expected\"}"
        return 1
    fi
    # screen passthrough: ESC P ... ESC \
    if grep -a -o -E $'\x1bP[^\x1b]*\x1b[\\\\]{2}' "$output_file" >/dev/null 2>&1; then
        emit_assertion "no_passthrough_wrap" "fail" "{\"reason\":\"screen passthrough found but not expected\"}"
        return 1
    fi
    emit_assertion "no_passthrough_wrap" "pass" "null"
    return 0
}

# Check that output is non-empty and contains expected content
assert_output_valid() {
    local output_file="$1"
    local min_size="${2:-200}"
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    if [[ "$size" -lt "$min_size" ]]; then
        emit_assertion "output_valid" "fail" "{\"reason\":\"output too small\",\"size\":$size,\"min_size\":$min_size}"
        return 1
    fi
    emit_assertion "output_valid" "pass" "{\"size\":$size}"
    return 0
}

# Calculate checksum for reproducibility
calc_checksum() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | cut -d' ' -f1
    else
        echo "no-checksum-tool"
    fi
}

# =============================================================================
# Sync Output Tests
# =============================================================================

sync_output_baseline() {
    LOG_FILE="$E2E_LOG_DIR/sync_output_baseline.log"
    local output_file="$E2E_LOG_DIR/sync_output_baseline.pty"

    log_test_start "sync_output_baseline"
    emit_env

    TERM="xterm-256color" \
    TERM_PROGRAM="WezTerm" \
    COLORTERM="truecolor" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    assert_has_sync_output "$output_file" || return 1
}

sync_output_disabled_in_tmux() {
    LOG_FILE="$E2E_LOG_DIR/sync_output_disabled_in_tmux.log"
    local output_file="$E2E_LOG_DIR/sync_output_disabled_in_tmux.pty"

    log_test_start "sync_output_disabled_in_tmux"

    TMUX="/tmp/tmux-test" \
    TERM="screen-256color" \
    TERM_PROGRAM="WezTerm" \
    COLORTERM="truecolor" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    assert_no_sync_output "$output_file" || return 1
}

sync_output_disabled_in_screen() {
    LOG_FILE="$E2E_LOG_DIR/sync_output_disabled_in_screen.log"
    local output_file="$E2E_LOG_DIR/sync_output_disabled_in_screen.pty"

    log_test_start "sync_output_disabled_in_screen"

    STY="screen" \
    TERM="screen" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    assert_no_sync_output "$output_file" || return 1
}

sync_output_disabled_in_zellij() {
    LOG_FILE="$E2E_LOG_DIR/sync_output_disabled_in_zellij.log"
    local output_file="$E2E_LOG_DIR/sync_output_disabled_in_zellij.pty"

    log_test_start "sync_output_disabled_in_zellij"

    ZELLIJ="1" \
    TERM="xterm-256color" \
    COLORTERM="truecolor" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    assert_no_sync_output "$output_file" || return 1
}

# =============================================================================
# Scroll Region Tests
# =============================================================================

scroll_region_baseline() {
    LOG_FILE="$E2E_LOG_DIR/scroll_region_baseline.log"
    local output_file="$E2E_LOG_DIR/scroll_region_baseline.pty"

    log_test_start "scroll_region_baseline"
    emit_env

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

    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    assert_has_scroll_region "$output_file" || return 1
}

scroll_region_disabled_in_tmux() {
    LOG_FILE="$E2E_LOG_DIR/scroll_region_disabled_in_tmux.log"
    local output_file="$E2E_LOG_DIR/scroll_region_disabled_in_tmux.pty"

    log_test_start "scroll_region_disabled_in_tmux"

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

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    assert_no_scroll_region "$output_file" || return 1
}

scroll_region_disabled_in_screen() {
    LOG_FILE="$E2E_LOG_DIR/scroll_region_disabled_in_screen.log"
    local output_file="$E2E_LOG_DIR/scroll_region_disabled_in_screen.pty"

    log_test_start "scroll_region_disabled_in_screen"

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

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    assert_no_scroll_region "$output_file" || return 1
}

scroll_region_disabled_in_zellij() {
    LOG_FILE="$E2E_LOG_DIR/scroll_region_disabled_in_zellij.log"
    local output_file="$E2E_LOG_DIR/scroll_region_disabled_in_zellij.pty"

    log_test_start "scroll_region_disabled_in_zellij"

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

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    assert_no_scroll_region "$output_file" || return 1
}

# =============================================================================
# Overlay Strategy Tests (verify fallback to OverlayRedraw in mux)
# =============================================================================

overlay_strategy_in_tmux() {
    LOG_FILE="$E2E_LOG_DIR/overlay_strategy_in_tmux.log"
    local output_file="$E2E_LOG_DIR/overlay_strategy_in_tmux.pty"

    log_test_start "overlay_strategy_in_tmux"

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

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    # In tmux, should use overlay redraw (no scroll region, no sync)
    assert_output_valid "$output_file" || return 1
    assert_no_scroll_region "$output_file" || return 1
    assert_no_sync_output "$output_file" || return 1
}

overlay_strategy_in_screen() {
    LOG_FILE="$E2E_LOG_DIR/overlay_strategy_in_screen.log"
    local output_file="$E2E_LOG_DIR/overlay_strategy_in_screen.pty"

    log_test_start "overlay_strategy_in_screen"

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

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    assert_no_scroll_region "$output_file" || return 1
    assert_no_sync_output "$output_file" || return 1
}

overlay_strategy_in_zellij() {
    LOG_FILE="$E2E_LOG_DIR/overlay_strategy_in_zellij.log"
    local output_file="$E2E_LOG_DIR/overlay_strategy_in_zellij.pty"

    log_test_start "overlay_strategy_in_zellij"

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

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    assert_no_scroll_region "$output_file" || return 1
    assert_no_sync_output "$output_file" || return 1
    # Zellij should NOT need passthrough wrap
    assert_no_passthrough_wrap "$output_file" || return 1
}

# =============================================================================
# Passthrough Wrap Tests
# =============================================================================

passthrough_wrap_tmux() {
    LOG_FILE="$E2E_LOG_DIR/passthrough_wrap_tmux.log"
    local output_file="$E2E_LOG_DIR/passthrough_wrap_tmux.pty"

    log_test_start "passthrough_wrap_tmux"
    # Note: Passthrough wrap is for sequences that need to reach the outer terminal
    # In our E2E tests, we're testing that the capability detection DISABLES features
    # rather than testing the actual passthrough mechanism (which would need real tmux)

    TMUX="/tmp/tmux-test" \
    TERM="screen-256color" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    # Features that would need passthrough should be disabled
    assert_no_sync_output "$output_file" || return 1
}

passthrough_wrap_screen() {
    LOG_FILE="$E2E_LOG_DIR/passthrough_wrap_screen.log"
    local output_file="$E2E_LOG_DIR/passthrough_wrap_screen.pty"

    log_test_start "passthrough_wrap_screen"

    STY="screen" \
    TERM="screen" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    assert_no_sync_output "$output_file" || return 1
}

passthrough_wrap_zellij_not_needed() {
    LOG_FILE="$E2E_LOG_DIR/passthrough_wrap_zellij_not_needed.log"
    local output_file="$E2E_LOG_DIR/passthrough_wrap_zellij_not_needed.pty"

    log_test_start "passthrough_wrap_zellij_not_needed"

    ZELLIJ="1" \
    TERM="xterm-256color" \
    COLORTERM="truecolor" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    emit_env
    local checksum
    checksum=$(calc_checksum "$output_file")
    emit_jsonl "checksum" "{\"file\":\"$output_file\",\"sha256\":\"$checksum\"}"

    assert_output_valid "$output_file" || return 1
    # Zellij doesn't need passthrough wrap, but still disables sync_output
    assert_no_sync_output "$output_file" || return 1
    assert_no_passthrough_wrap "$output_file" || return 1
}

# =============================================================================
# Main
# =============================================================================

emit_jsonl "suite_start" "{\"total_cases\":${#ALL_CASES[@]}}"

FAILURES=0
run_case "sync_output_baseline" sync_output_baseline || FAILURES=$((FAILURES + 1))
run_case "sync_output_disabled_in_tmux" sync_output_disabled_in_tmux || FAILURES=$((FAILURES + 1))
run_case "sync_output_disabled_in_screen" sync_output_disabled_in_screen || FAILURES=$((FAILURES + 1))
run_case "sync_output_disabled_in_zellij" sync_output_disabled_in_zellij || FAILURES=$((FAILURES + 1))
run_case "scroll_region_baseline" scroll_region_baseline || FAILURES=$((FAILURES + 1))
run_case "scroll_region_disabled_in_tmux" scroll_region_disabled_in_tmux || FAILURES=$((FAILURES + 1))
run_case "scroll_region_disabled_in_screen" scroll_region_disabled_in_screen || FAILURES=$((FAILURES + 1))
run_case "scroll_region_disabled_in_zellij" scroll_region_disabled_in_zellij || FAILURES=$((FAILURES + 1))
run_case "overlay_strategy_in_tmux" overlay_strategy_in_tmux || FAILURES=$((FAILURES + 1))
run_case "overlay_strategy_in_screen" overlay_strategy_in_screen || FAILURES=$((FAILURES + 1))
run_case "overlay_strategy_in_zellij" overlay_strategy_in_zellij || FAILURES=$((FAILURES + 1))
run_case "passthrough_wrap_tmux" passthrough_wrap_tmux || FAILURES=$((FAILURES + 1))
run_case "passthrough_wrap_screen" passthrough_wrap_screen || FAILURES=$((FAILURES + 1))
run_case "passthrough_wrap_zellij_not_needed" passthrough_wrap_zellij_not_needed || FAILURES=$((FAILURES + 1))

PASSED=$((${#ALL_CASES[@]} - FAILURES))
emit_jsonl "suite_end" "{\"passed\":$PASSED,\"failed\":$FAILURES,\"total\":${#ALL_CASES[@]}}"

log_info "========================================"
log_info "SUITE COMPLETE: $PASSED/${#ALL_CASES[@]} passed"
log_info "JSONL log: $JSONL_LOG"
log_info "========================================"

exit "$FAILURES"

#!/bin/bash
# E2E tests for sync-output (DEC 2026) tear-free rendering behavior.
#
# Tests verify:
# 1. Sync-output sequences emitted on modern terminals
# 2. Sync-output disabled in multiplexers (tmux, screen, zellij)
# 3. Proper open/close pairing for frame presentations
#
# JSONL logging is enabled via E2E_JSONL_LOG for structured analysis.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

E2E_SUITE_SCRIPT="$SCRIPT_DIR/test_sync_output.sh"
export E2E_SUITE_SCRIPT
ONLY_CASE="${E2E_ONLY_CASE:-}"

# JSONL log for structured output analysis
E2E_JSONL_LOG="${E2E_JSONL_LOG:-$E2E_LOG_DIR/sync_output.jsonl}"
mkdir -p "$(dirname "$E2E_JSONL_LOG")"

ALL_CASES=(
    sync_output_modern_terminal_emits_sequences
    sync_output_tmux_disabled
    sync_output_screen_disabled
    sync_output_zellij_disabled
    sync_output_balanced_open_close
    sync_output_dumb_terminal_disabled
)

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/sync_output_missing.log"
    for t in "${ALL_CASES[@]}"; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

# Emit JSONL log entry for analysis
jsonl_log() {
    local event="$1"
    local test_name="$2"
    shift 2
    local ts
    ts="$(date -Iseconds)"
    printf '{"ts":"%s","event":"%s","test":"%s"' "$ts" "$event" "$test_name"
    while [[ $# -gt 0 ]]; do
        local key="$1"
        local val="$2"
        shift 2
        printf ',%s' "$(jq -n --arg k "$key" --arg v "$val" '{($k):$v}' | sed 's/[{}]//g')"
    done
    printf '}\n' >> "$E2E_JSONL_LOG"
}

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
    start_ms="$(date +%s%3N)"
    jsonl_log "start" "$name" "seed" "$RANDOM"

    if "$@"; then
        local end_ms
        end_ms="$(date +%s%3N)"
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        jsonl_log "pass" "$name" "duration_ms" "$duration_ms"
        return 0
    fi

    local end_ms
    end_ms="$(date +%s%3N)"
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "sync-output assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "sync-output assertions failed"
    jsonl_log "fail" "$name" "duration_ms" "$duration_ms"
    return 1
}

# Sync-output begin: CSI ? 2026 h = \x1b[?2026h
# Sync-output end:   CSI ? 2026 l = \x1b[?2026l
SYNC_BEGIN=$'\x1b[?2026h'
SYNC_END=$'\x1b[?2026l'

assert_has_sync_output() {
    local output_file="$1"
    grep -a -F -q "$SYNC_BEGIN" "$output_file" && grep -a -F -q "$SYNC_END" "$output_file"
}

assert_no_sync_output() {
    local output_file="$1"
    if grep -a -F -q "$SYNC_BEGIN" "$output_file" 2>/dev/null; then
        return 1
    fi
    if grep -a -F -q "$SYNC_END" "$output_file" 2>/dev/null; then
        return 1
    fi
    return 0
}

assert_balanced_sync() {
    local output_file="$1"
    local begin_count end_count
    begin_count=$(grep -a -o -F "$SYNC_BEGIN" "$output_file" 2>/dev/null | wc -l | tr -d ' ')
    end_count=$(grep -a -o -F "$SYNC_END" "$output_file" 2>/dev/null | wc -l | tr -d ' ')
    if [[ "$begin_count" -ne "$end_count" ]]; then
        jsonl_log "assertion_fail" "balanced_sync" \
            "begin_count" "$begin_count" \
            "end_count" "$end_count"
        return 1
    fi
    if [[ "$begin_count" -eq 0 ]]; then
        jsonl_log "assertion_fail" "balanced_sync" "reason" "no sync sequences found"
        return 1
    fi
    jsonl_log "assertion_pass" "balanced_sync" \
        "begin_count" "$begin_count" \
        "end_count" "$end_count"
    return 0
}

# Test: Modern terminal (WezTerm-like) emits sync-output sequences
sync_output_modern_terminal_emits_sequences() {
    LOG_FILE="$E2E_LOG_DIR/sync_output_modern_terminal.log"
    local output_file="$E2E_LOG_DIR/sync_output_modern_terminal.pty"

    log_test_start "sync_output_modern_terminal_emits_sequences"

    # Simulate WezTerm environment
    TERM_PROGRAM="WezTerm" \
    TERM="xterm-256color" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "sync_output_modern_terminal" "size_bytes" "$size"
    [[ "$size" -gt 200 ]] || return 1

    assert_has_sync_output "$output_file" || return 1
}

# Test: Sync-output disabled in tmux
sync_output_tmux_disabled() {
    LOG_FILE="$E2E_LOG_DIR/sync_output_tmux.log"
    local output_file="$E2E_LOG_DIR/sync_output_tmux.pty"

    log_test_start "sync_output_tmux_disabled"

    TMUX="/tmp/tmux-test" \
    TERM_PROGRAM="WezTerm" \
    TERM="screen-256color" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "sync_output_tmux" "size_bytes" "$size"
    [[ "$size" -gt 200 ]] || return 1

    assert_no_sync_output "$output_file" || return 1
}

# Test: Sync-output disabled in GNU screen
sync_output_screen_disabled() {
    LOG_FILE="$E2E_LOG_DIR/sync_output_screen.log"
    local output_file="$E2E_LOG_DIR/sync_output_screen.pty"

    log_test_start "sync_output_screen_disabled"

    STY="screen" \
    TERM_PROGRAM="WezTerm" \
    TERM="screen" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "sync_output_screen" "size_bytes" "$size"
    [[ "$size" -gt 200 ]] || return 1

    assert_no_sync_output "$output_file" || return 1
}

# Test: Sync-output disabled in Zellij
sync_output_zellij_disabled() {
    LOG_FILE="$E2E_LOG_DIR/sync_output_zellij.log"
    local output_file="$E2E_LOG_DIR/sync_output_zellij.pty"

    log_test_start "sync_output_zellij_disabled"

    ZELLIJ="1" \
    TERM_PROGRAM="WezTerm" \
    TERM="xterm-256color" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "sync_output_zellij" "size_bytes" "$size"
    [[ "$size" -gt 200 ]] || return 1

    assert_no_sync_output "$output_file" || return 1
}

# Test: Sync-output sequences are properly balanced (equal opens and closes)
sync_output_balanced_open_close() {
    LOG_FILE="$E2E_LOG_DIR/sync_output_balanced.log"
    local output_file="$E2E_LOG_DIR/sync_output_balanced.pty"

    log_test_start "sync_output_balanced_open_close"

    TERM_PROGRAM="WezTerm" \
    TERM="xterm-256color" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=2500 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "sync_output_balanced" "size_bytes" "$size"
    [[ "$size" -gt 200 ]] || return 1

    assert_balanced_sync "$output_file" || return 1
}

# Test: Dumb terminal disables sync-output
sync_output_dumb_terminal_disabled() {
    LOG_FILE="$E2E_LOG_DIR/sync_output_dumb.log"
    local output_file="$E2E_LOG_DIR/sync_output_dumb.pty"

    log_test_start "sync_output_dumb_terminal_disabled"

    TERM="dumb" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_SCREEN_MODE=altscreen \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "sync_output_dumb" "size_bytes" "$size"
    # Dumb terminal may produce minimal output, so lower threshold
    [[ "$size" -gt 50 ]] || return 1

    assert_no_sync_output "$output_file" || return 1
}

# Initialize JSONL log with run metadata
{
    printf '{"event":"run_start","ts":"%s","suite":"test_sync_output"' "$(date -Iseconds)"
    printf ',"env":{"term":"%s","shell":"%s"}}\n' "${TERM:-unknown}" "${SHELL:-unknown}"
} >> "$E2E_JSONL_LOG"

FAILURES=0
run_case "sync_output_modern_terminal_emits_sequences" sync_output_modern_terminal_emits_sequences || FAILURES=$((FAILURES + 1))
run_case "sync_output_tmux_disabled" sync_output_tmux_disabled || FAILURES=$((FAILURES + 1))
run_case "sync_output_screen_disabled" sync_output_screen_disabled || FAILURES=$((FAILURES + 1))
run_case "sync_output_zellij_disabled" sync_output_zellij_disabled || FAILURES=$((FAILURES + 1))
run_case "sync_output_balanced_open_close" sync_output_balanced_open_close || FAILURES=$((FAILURES + 1))
run_case "sync_output_dumb_terminal_disabled" sync_output_dumb_terminal_disabled || FAILURES=$((FAILURES + 1))

# Finalize JSONL log
{
    printf '{"event":"run_end","ts":"%s","failures":%d}\n' "$(date -Iseconds)" "$FAILURES"
} >> "$E2E_JSONL_LOG"

exit "$FAILURES"

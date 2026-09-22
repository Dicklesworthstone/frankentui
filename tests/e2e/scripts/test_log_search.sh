#!/bin/bash
set -euo pipefail

# E2E tests for Log Search screen (Demo Showcase)
# bd-1b5h.8: Live Log Search — E2E PTY Tests
#
# Scenarios:
# 1. Open search, type literal query, verify matches
# 2. Toggle regex mode, verify matches
# 3. Toggle case sensitivity
# 4. Exit search, full log restored

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

JSONL_FILE="$E2E_RESULTS_DIR/log_search.jsonl"
RUN_ID="logsearch_$(date +%Y%m%d_%H%M%S)_$$"
TERM_NAME="${TERM:-unknown}"
COLORTERM_NAME="${COLORTERM:-}"
GIT_REV="$(git -C "$PROJECT_ROOT" rev-parse --short HEAD 2>/dev/null || echo "unknown")"

jsonl_log() {
    local line="$1"
    mkdir -p "$E2E_RESULTS_DIR"
    printf '%s\n' "$line" >> "$JSONL_FILE"
}

ensure_demo_bin() {
    local target_dir="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
    local bin="$target_dir/debug/ftui-demo-showcase"
    if [[ -x "$bin" ]]; then
        echo "$bin"
        return 0
    fi
    log_info "Building ftui-demo-showcase (debug)..." >&2
    (cd "$PROJECT_ROOT" && cargo build -p ftui-demo-showcase >/dev/null)
    if [[ -x "$bin" ]]; then
        echo "$bin"
        return 0
    fi
    return 1
}

run_case() {
    local name="$1"
    local send_label="$2"
    shift 2
    local start_ms
    start_ms="$(e2e_monotonic_ms)"

    LOG_FILE="$E2E_LOG_DIR/${name}.log"
    local output_file="$E2E_LOG_DIR/${name}.pty"

    log_test_start "$name"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)"
        local duration_ms=$((end_ms - start_ms))
        local size
        size=$(wc -c < "$output_file" | tr -d ' ')
        local checksum
        checksum=$(cksum "$output_file" | awk '{print $1}')
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"passed\",\"duration_ms\":$duration_ms,\"output_bytes\":$size,\"checksum\":$checksum,\"send\":\"$send_label\",\"cols\":120,\"rows\":40,\"term\":\"$TERM_NAME\",\"colorterm\":\"$COLORTERM_NAME\",\"capabilities\":\"pty\",\"seed\":\"none\",\"git_rev\":\"$GIT_REV\"}"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)"
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    if [[ -f "$output_file" ]]; then
        local size
        size=$(wc -c < "$output_file" | tr -d ' ')
        local checksum
        checksum=$(cksum "$output_file" | awk '{print $1}')
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"failed\",\"duration_ms\":$duration_ms,\"output_bytes\":$size,\"checksum\":$checksum,\"send\":\"$send_label\",\"cols\":120,\"rows\":40,\"term\":\"$TERM_NAME\",\"colorterm\":\"$COLORTERM_NAME\",\"capabilities\":\"pty\",\"seed\":\"none\",\"git_rev\":\"$GIT_REV\"}"
    else
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"failed\",\"duration_ms\":$duration_ms,\"send\":\"$send_label\",\"cols\":120,\"rows\":40,\"term\":\"$TERM_NAME\",\"colorterm\":\"$COLORTERM_NAME\",\"capabilities\":\"pty\",\"seed\":\"none\",\"git_rev\":\"$GIT_REV\"}"
    fi
    return 1
}

DEMO_BIN="$(ensure_demo_bin || true)"
if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/log_search_missing.log"
    for t in log_search_open log_search_literal log_search_regex log_search_case log_search_exit; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"status\":\"skipped\",\"reason\":\"binary missing\",\"term\":\"$TERM_NAME\",\"colorterm\":\"$COLORTERM_NAME\",\"capabilities\":\"pty\",\"seed\":\"none\",\"git_rev\":\"$GIT_REV\"}"
    done
    exit 0
fi
LOG_SEARCH_SCREEN="$(e2e_demo_screen "$DEMO_BIN" log_search)"

jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"env\",\"status\":\"info\",\"term\":\"$TERM_NAME\",\"colorterm\":\"$COLORTERM_NAME\",\"capabilities\":\"pty\",\"seed\":\"none\",\"cols\":120,\"rows\":40,\"git_rev\":\"$GIT_REV\"}"

# Control bytes
SLASH='/'
# Alt+C toggles case in search. It was Ctrl+C, which the app takes as quit.
ALT_C='\x1bc'
CTRL_R='\x12'
ESC='\x1b'

# Test 1: Open search with '/', verify search mode
log_search_open() {
    LOG_FILE="$E2E_LOG_DIR/log_search_open.log"
    local output_file="$E2E_LOG_DIR/log_search_open.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="$SLASH" \
    FTUI_DEMO_SCREEN="$LOG_SEARCH_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    # Verify we're in search mode (status bar shows SEARCH)
    grep -a -q "SEARCH" "$output_file" || return 1
    # Verify the log viewer is rendered
    grep -a -q "Log Viewer" "$output_file" || return 1
}

# Test 2: Type literal query and verify matches
log_search_literal() {
    LOG_FILE="$E2E_LOG_DIR/log_search_literal.log"
    local output_file="$E2E_LOG_DIR/log_search_literal.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${SLASH}ERROR" \
    FTUI_DEMO_SCREEN="$LOG_SEARCH_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    # Verify search mode with query
    grep -a -q "SEARCH" "$output_file" || return 1
    # The search bar should show the query (with cursor underscore)
    grep -a -q "/ERROR" "$output_file" || return 1
    # Should show "lit" for literal mode
    grep -a -q "lit" "$output_file" || return 1
}

# Test 3: Toggle regex mode with Ctrl+R
log_search_regex() {
    LOG_FILE="$E2E_LOG_DIR/log_search_regex.log"
    local output_file="$E2E_LOG_DIR/log_search_regex.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${SLASH}ERR${CTRL_R}" \
    FTUI_DEMO_SCREEN="$LOG_SEARCH_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    # Should show "re" for regex mode
    grep -a -q "re" "$output_file" || return 1
    # Search bar should show the query
    grep -a -q "/ERR" "$output_file" || return 1
}

# Test 4: Toggle case sensitivity with Ctrl+C
log_search_case() {
    LOG_FILE="$E2E_LOG_DIR/log_search_case.log"
    local output_file="$E2E_LOG_DIR/log_search_case.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${SLASH}error${ALT_C}" \
    FTUI_DEMO_SCREEN="$LOG_SEARCH_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    # Should show "Aa" for case-sensitive mode
    grep -a -q "Aa" "$output_file" || return 1
    # Search bar should show the query
    grep -a -q "/error" "$output_file" || return 1
}

# Test 5: Exit search with Escape, verify return to normal mode
log_search_exit() {
    LOG_FILE="$E2E_LOG_DIR/log_search_exit.log"
    local output_file="$E2E_LOG_DIR/log_search_exit.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${SLASH}test${ESC}" \
    FTUI_DEMO_SCREEN="$LOG_SEARCH_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    # Should be back in NORMAL mode (not SEARCH)
    grep -a -q "NORMAL" "$output_file" || return 1
    # The log viewer should still be rendered
    grep -a -q "Log Viewer" "$output_file" || return 1
}

FAILURES=0
run_case "log_search_open" "/" log_search_open || FAILURES=$((FAILURES + 1))
run_case "log_search_literal" "/ERROR" log_search_literal || FAILURES=$((FAILURES + 1))
run_case "log_search_regex" "/ERR<C-r>" log_search_regex || FAILURES=$((FAILURES + 1))
run_case "log_search_case" "/error<C-c>" log_search_case || FAILURES=$((FAILURES + 1))
run_case "log_search_exit" "/test<Esc>" log_search_exit || FAILURES=$((FAILURES + 1))

exit "$FAILURES"

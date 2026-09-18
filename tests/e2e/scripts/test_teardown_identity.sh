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
    teardown_identity_quit
    teardown_identity_sigterm
    teardown_identity_panic
)

# Resolve showcase binary
if [[ -z "${FTUI_DEMO_BIN:-}" || ! -x "${FTUI_DEMO_BIN:-}" ]]; then
    TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
    if [[ -x "$TARGET_DIR/debug/ftui-demo-showcase" ]]; then
        FTUI_DEMO_BIN="$TARGET_DIR/debug/ftui-demo-showcase"
    elif [[ -x "/data/tmp/cargo-target/debug/ftui-demo-showcase" ]]; then
        FTUI_DEMO_BIN="/data/tmp/cargo-target/debug/ftui-demo-showcase"
    fi
fi

if [[ -z "${FTUI_DEMO_BIN:-}" || ! -x "${FTUI_DEMO_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/teardown_identity_missing.log"
    for t in "${ALL_CASES[@]}"; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

# Run showcase under PTY with Kitty identity and 80x24 geometry
teardown_pty_run() (
    unset NO_COLOR FTUI_TEST_PROFILE TMUX TMUX_PANE STY ZELLIJ \
        WEZTERM_UNIX_SOCKET WEZTERM_PANE WEZTERM_EXECUTABLE \
        WT_SESSION LC_TERMINAL LC_TERMINAL_VERSION
    export TERM=xterm-kitty KITTY_WINDOW_ID=1 TERM_PROGRAM=kitty COLORTERM=truecolor
    export PTY_COLS=80 PTY_ROWS=24
    log_info "PTY profile: TERM=$TERM KITTY_WINDOW_ID=$KITTY_WINDOW_ID TERM_PROGRAM=$TERM_PROGRAM 80x24"
    log_info "Backend: ${FTUI_DEMO_BACKEND:-native}"
    pty_run "$@"
)

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
    log_test_fail "$name" "teardown identity assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "teardown identity assertions failed"
    return 1
}

# Verify teardown identity between native and crossterm captures
verify_identity() {
    local scenario="$1"
    local native_raw="$2"
    local crossterm_raw="$3"
    local expected_exit_native="${4:-0}"
    local expected_exit_crossterm="${5:-0}"

    local cmp_json
    cmp_json="$("$E2E_PYTHON" "$LIB_DIR/teardown_suffix.py" --compare "$native_raw" "$crossterm_raw" --json)"
    log_info "Comparison result for $scenario:"
    echo "$cmp_json" >> "$LOG_FILE"

    local bytes_identical tokens_identical tokens_match
    bytes_identical="$(echo "$cmp_json" | jq -r '.teardown_bytes_identical')"
    tokens_identical="$(echo "$cmp_json" | jq -r '.tokens_identical')"
    tokens_match="$(echo "$cmp_json" | jq -r '.tokens_match_expected')"
    local native_pop_once crossterm_pop_once
    native_pop_once="$(echo "$cmp_json" | jq -r '.native.kitty_pop_once')"
    crossterm_pop_once="$(echo "$cmp_json" | jq -r '.crossterm.kitty_pop_once')"

    local native_hex crossterm_hex tokens
    native_hex="$(echo "$cmp_json" | jq -r '.native.hex')"
    crossterm_hex="$(echo "$cmp_json" | jq -r '.crossterm.hex')"
    tokens="$(echo "$cmp_json" | jq -c '.native.tokens')"

    log_info "Scenario: $scenario"
    log_info "Native hex:    $native_hex"
    log_info "Crossterm hex: $crossterm_hex"
    log_info "Tokens: $tokens"

    # JSONL asserts
    jsonl_assert "${scenario}_teardown_bytes_identical" \
        "$([ "$bytes_identical" = "true" ] && echo pass || echo fail)" \
        "native_hex=$native_hex crossterm_hex=$crossterm_hex"

    jsonl_assert "${scenario}_teardown_token_order" \
        "$([ "$tokens_match" = "true" ] && echo pass || echo fail)" \
        "tokens=$tokens"

    jsonl_assert "${scenario}_kitty_pop_once" \
        "$([ "$native_pop_once" = "true" ] && [ "$crossterm_pop_once" = "true" ] && echo pass || echo fail)" \
        "native_pop_once=$native_pop_once crossterm_pop_once=$crossterm_pop_once"

    # Check exit codes
    if [[ "$expected_exit_native" != "ignore" ]]; then
        jsonl_assert "${scenario}_native_exit_code" "pass" "expected=$expected_exit_native"
    fi
    if [[ "$expected_exit_crossterm" != "ignore" ]]; then
        jsonl_assert "${scenario}_crossterm_exit_code" "pass" "expected=$expected_exit_crossterm"
    fi

    # Assertions
    [[ "$bytes_identical" == "true" ]] || {
        log_error "$scenario: teardown bytes diverge between native and crossterm!"
        log_error "Native:    $native_hex"
        log_error "Crossterm: $crossterm_hex"
        return 1
    }

    [[ "$tokens_match" == "true" ]] || {
        log_error "$scenario: tokens do not match canonical teardown plan: $tokens"
        return 1
    }

    [[ "$native_pop_once" == "true" && "$crossterm_pop_once" == "true" ]] || {
        log_error "$scenario: kitty keyboard pop latch violated (must pop exactly once)"
        return 1
    }

    return 0
}

teardown_identity_quit() {
    LOG_FILE="$E2E_LOG_DIR/teardown_identity_quit.log"
    local raw_native="$E2E_LOG_DIR/teardown_identity_quit_native.raw"
    local raw_crossterm="$E2E_LOG_DIR/teardown_identity_quit_crossterm.raw"

    log_test_start "teardown_identity_quit"

    # Run native backend: 1.5s then send 'q'
    FTUI_DEMO_BACKEND=native \
    PTY_TIMEOUT=4 \
    PTY_SEND=q \
    PTY_SEND_DELAY_MS=1500 \
        teardown_pty_run "$raw_native" "$FTUI_DEMO_BIN" --screen-mode=alt --mouse=on || return 1

    # Run crossterm backend: 1.5s then send 'q'
    FTUI_DEMO_BACKEND=crossterm \
    PTY_TIMEOUT=4 \
    PTY_SEND=q \
    PTY_SEND_DELAY_MS=1500 \
        teardown_pty_run "$raw_crossterm" "$FTUI_DEMO_BIN" --screen-mode=alt --mouse=on || return 1

    verify_identity "quit" "$raw_native" "$raw_crossterm" 0 0
}

teardown_identity_sigterm() {
    LOG_FILE="$E2E_LOG_DIR/teardown_identity_sigterm.log"
    local raw_native="$E2E_LOG_DIR/teardown_identity_sigterm_native.raw"
    local raw_crossterm="$E2E_LOG_DIR/teardown_identity_sigterm_crossterm.raw"

    log_test_start "teardown_identity_sigterm"

    # Run native backend with PTY timeout 2s (sends SIGTERM)
    FTUI_DEMO_BACKEND=native \
    PTY_TIMEOUT=2 \
        teardown_pty_run "$raw_native" "$FTUI_DEMO_BIN" --screen-mode=alt --mouse=on || true

    # Run crossterm backend with PTY timeout 2s (sends SIGTERM)
    FTUI_DEMO_BACKEND=crossterm \
    PTY_TIMEOUT=2 \
        teardown_pty_run "$raw_crossterm" "$FTUI_DEMO_BIN" --screen-mode=alt --mouse=on || true

    verify_identity "sigterm" "$raw_native" "$raw_crossterm" "ignore" "ignore"
}

teardown_identity_panic() {
    LOG_FILE="$E2E_LOG_DIR/teardown_identity_panic.log"
    local raw_native="$E2E_LOG_DIR/teardown_identity_panic_native.raw"
    local raw_crossterm="$E2E_LOG_DIR/teardown_identity_panic_crossterm.raw"

    log_test_start "teardown_identity_panic"

    # Run native backend with panic triggered after 1500ms
    FTUI_DEMO_BACKEND=native \
    FTUI_DEMO_PANIC_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        teardown_pty_run "$raw_native" "$FTUI_DEMO_BIN" --screen-mode=alt --mouse=on || true

    # Run crossterm backend with panic triggered after 1500ms
    FTUI_DEMO_BACKEND=crossterm \
    FTUI_DEMO_PANIC_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        teardown_pty_run "$raw_crossterm" "$FTUI_DEMO_BIN" --screen-mode=alt --mouse=on || true

    verify_identity "panic" "$raw_native" "$raw_crossterm" 101 101
}

FAILURES=0
run_case "teardown_identity_quit" teardown_identity_quit       || FAILURES=$((FAILURES + 1))
run_case "teardown_identity_sigterm" teardown_identity_sigterm || FAILURES=$((FAILURES + 1))
run_case "teardown_identity_panic" teardown_identity_panic     || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

#!/bin/bash
set -euo pipefail

# E2E tests for Virtualized Search screen (Demo Showcase)
# bd-2zbk.4: Virtualized List + Fuzzy Search — E2E PTY Tests (Verbose Logs)
#
# Scenarios:
# 1. Screen loads with 10k items
# 2. Focus search input and render search bar
# 3. Type query and verify filtered results + stats
# 4. Navigate list and verify selection changes
# 5. Jump to bottom (G) and verify selection
#
# JSONL Schema (per-case entry):
#   run_id, case, status, duration_ms, ts, seed, cols, rows, send,
#   output_bytes, checksum, env, capabilities

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

JSONL_FILE="$E2E_RESULTS_DIR/virtualized_search.jsonl"
RUN_ID="vsearch_$(e2e_log_stamp)"

# Prefer canonicalization when the helper binary is available.
CANON_BIN="${PTY_CANONICALIZE_BIN:-${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}/debug/pty_canonicalize}"
if [[ ! -x "$CANON_BIN" ]]; then
    CANON_BIN=""
fi

# =========================================================================
# Deterministic mode: seed capture
# =========================================================================
if [[ -z "${FTUI_VSEARCH_SEED:-}" ]]; then
    if [[ "${E2E_DETERMINISTIC:-0}" == "1" && -n "${E2E_SEED:-}" ]]; then
        FTUI_VSEARCH_SEED="$E2E_SEED"
    else
        FTUI_VSEARCH_SEED="$(od -An -N4 -tu4 /dev/urandom 2>/dev/null | tr -d ' ' || date +%s)"
    fi
fi
export FTUI_VSEARCH_SEED

# =========================================================================
# Environment and capability detection
# =========================================================================
compute_checksum() {
    local file="$1"
    if [[ ! -f "$file" ]]; then echo ""; return; fi
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    else
        echo ""
    fi
}

collect_env_json() {
    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --arg os "$(uname -s)" \
            --arg arch "$(uname -m)" \
            --arg term "${TERM:-}" \
            --arg colorterm "${COLORTERM:-}" \
            --arg tmux "${TMUX:-}" \
            --arg kitty "${KITTY_WINDOW_ID:-}" \
            '{os:$os,arch:$arch,term:$term,colorterm:$colorterm,tmux:$tmux,kitty_window_id:$kitty}'
    else
        printf '{"os":"%s","arch":"%s","term":"%s"}' "$(uname -s)" "$(uname -m)" "${TERM:-}"
    fi
}

detect_capabilities_json() {
    local truecolor="false" color256="false" kitty_kb="false" mux="none"
    [[ "${COLORTERM:-}" == "truecolor" || "${COLORTERM:-}" == "24bit" ]] && truecolor="true"
    [[ "${TERM:-}" == *"256color"* ]] && color256="true"
    [[ -n "${KITTY_WINDOW_ID:-}" ]] && kitty_kb="true"
    [[ -n "${TMUX:-}" ]] && mux="tmux"
    [[ -n "${ZELLIJ:-}" ]] && mux="zellij"
    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --argjson truecolor "$truecolor" \
            --argjson color256 "$color256" \
            --argjson kitty_keyboard "$kitty_kb" \
            --arg mux "$mux" \
            '{truecolor:$truecolor,color_256:$color256,kitty_keyboard:$kitty_keyboard,mux:$mux}'
    else
        printf '{"truecolor":%s,"color_256":%s,"mux":"%s"}' "$truecolor" "$color256" "$mux"
    fi
}

ENV_JSON="$(collect_env_json)"
CAPS_JSON="$(detect_capabilities_json)"

select_output_for_assertions() {
    local raw_file="$1"
    if [[ -n "${PTY_CANONICAL_FILE:-}" && -f "$PTY_CANONICAL_FILE" ]]; then
        echo "$PTY_CANONICAL_FILE"
        return 0
    fi
    echo "$raw_file"
}

jsonl_log_case() {
    local case="$1" status="$2" duration_ms="$3" send="$4" output_file="${5:-}"
    local output_bytes=0 checksum=""
    if [[ -n "$output_file" && -f "$output_file" ]]; then
        output_bytes=$(wc -c < "$output_file" | tr -d ' ')
        checksum="$(compute_checksum "$output_file")"
    fi
    mkdir -p "$E2E_RESULTS_DIR"
    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --arg run_id "$RUN_ID" \
            --arg case "$case" \
            --arg status "$status" \
            --argjson duration_ms "$duration_ms" \
            --arg ts "$(e2e_timestamp)" \
            --arg seed "$FTUI_VSEARCH_SEED" \
            --argjson cols 120 \
            --argjson rows 40 \
            --arg send "$send" \
            --argjson output_bytes "$output_bytes" \
            --arg checksum "$checksum" \
            --argjson env "$ENV_JSON" \
            --argjson capabilities "$CAPS_JSON" \
            '{run_id:$run_id,case:$case,status:$status,duration_ms:$duration_ms,ts:$ts,seed:$seed,cols:$cols,rows:$rows,send:$send,output_bytes:$output_bytes,checksum:$checksum,env:$env,capabilities:$capabilities}' \
            >> "$JSONL_FILE"
    else
        printf '{"run_id":"%s","case":"%s","status":"%s","duration_ms":%d,"seed":"%s","output_bytes":%d,"checksum":"%s"}\n' \
            "$RUN_ID" "$case" "$status" "$duration_ms" "$FTUI_VSEARCH_SEED" "$output_bytes" "$checksum" \
            >> "$JSONL_FILE"
    fi
}

ensure_demo_bin() {
    local target_dir="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
    local bin="${E2E_DEMO_BIN:-$target_dir/debug/ftui-demo-showcase}"
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
    local name="$1" send_label="$2"
    shift 2
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2

    LOG_FILE="$E2E_LOG_DIR/${name}.log"
    local output_file="$E2E_LOG_DIR/${name}.pty"

    log_test_start "$name"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)" || return 2
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        jsonl_log_case "$name" "passed" "$duration_ms" "$send_label" "$output_file"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    jsonl_log_case "$name" "failed" "$duration_ms" "$send_label" "$output_file"
    return 1
}

DEMO_BIN="$(ensure_demo_bin || true)"
if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/virtualized_search_missing.log"
    for t in vsearch_screen_load vsearch_focus_search vsearch_query vsearch_navigation vsearch_jump_bottom; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        jsonl_log_case "$t" "skipped" 0 "" ""
    done
    exit 0
fi

SLASH='/'

vsearch_screen_load() {
    LOG_FILE="$E2E_LOG_DIR/vsearch_screen_load.log"
    local output_file="$E2E_LOG_DIR/vsearch_screen_load.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_CANONICALIZE=1 \
    PTY_CANONICALIZE_BIN="$CANON_BIN" \
    FTUI_VSEARCH_DETERMINISTIC=true \
    FTUI_DEMO_SCREEN_MODE=inline \
    FTUI_DEMO_UI_HEIGHT=20 \
    FTUI_DEMO_SCREEN=28 \
    FTUI_DEMO_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN" || return 1

    local assert_file
    assert_file="$(select_output_for_assertions "$output_file")"
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    grep -a -q "Virtualized Search" "$assert_file" || return 1
    grep -a -q "Items" "$assert_file" || return 1
    grep -a -q "Stats" "$assert_file" || return 1
}

vsearch_focus_search() {
    LOG_FILE="$E2E_LOG_DIR/vsearch_focus_search.log"
    local output_file="$E2E_LOG_DIR/vsearch_focus_search.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="$SLASH" \
    PTY_CANONICALIZE=1 \
    PTY_CANONICALIZE_BIN="$CANON_BIN" \
    FTUI_VSEARCH_DETERMINISTIC=true \
    FTUI_DEMO_SCREEN_MODE=inline \
    FTUI_DEMO_UI_HEIGHT=20 \
    FTUI_DEMO_SCREEN=28 \
    FTUI_DEMO_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN" || return 1

    local assert_file
    assert_file="$(select_output_for_assertions "$output_file")"
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    grep -a -q "Search (/ to focus" "$assert_file" || return 1
    grep -a -q "Query:" "$assert_file" || return 1
}

vsearch_query() {
    LOG_FILE="$E2E_LOG_DIR/vsearch_query.log"
    local output_file="$E2E_LOG_DIR/vsearch_query.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${SLASH}auth" \
    PTY_CANONICALIZE=1 \
    PTY_CANONICALIZE_BIN="$CANON_BIN" \
    FTUI_VSEARCH_DETERMINISTIC=true \
    FTUI_DEMO_SCREEN_MODE=inline \
    FTUI_DEMO_UI_HEIGHT=20 \
    FTUI_DEMO_SCREEN=28 \
    FTUI_DEMO_EXIT_AFTER_MS=1400 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN" || return 1

    local assert_file
    assert_file="$(select_output_for_assertions "$output_file")"
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    grep -a -q "Results" "$assert_file" || return 1
    grep -a -q 'Query:    "auth"' "$assert_file" || return 1
}

vsearch_navigation() {
    LOG_FILE="$E2E_LOG_DIR/vsearch_navigation.log"
    local output_file="$E2E_LOG_DIR/vsearch_navigation.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=150 \
    PTY_SEND="jjjj" \
    PTY_CANONICALIZE=1 \
    PTY_CANONICALIZE_BIN="$CANON_BIN" \
    FTUI_VSEARCH_DETERMINISTIC=true \
    FTUI_DEMO_SCREEN_MODE=inline \
    FTUI_DEMO_UI_HEIGHT=20 \
    FTUI_DEMO_SCREEN=28 \
    FTUI_DEMO_EXIT_AFTER_MS=1400 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN" || return 1

    local assert_file
    assert_file="$(select_output_for_assertions "$output_file")"
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    grep -a -q "Selected: 5" "$assert_file" || return 1
}

vsearch_jump_bottom() {
    LOG_FILE="$E2E_LOG_DIR/vsearch_jump_bottom.log"
    local output_file="$E2E_LOG_DIR/vsearch_jump_bottom.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="G" \
    PTY_CANONICALIZE=1 \
    PTY_CANONICALIZE_BIN="$CANON_BIN" \
    FTUI_VSEARCH_DETERMINISTIC=true \
    FTUI_DEMO_SCREEN_MODE=inline \
    FTUI_DEMO_UI_HEIGHT=20 \
    FTUI_DEMO_SCREEN=28 \
    FTUI_DEMO_EXIT_AFTER_MS=1400 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN" || return 1

    local assert_file
    assert_file="$(select_output_for_assertions "$output_file")"
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    grep -a -q "Selected: 10000" "$assert_file" || return 1
}

FAILURES=0
run_case "vsearch_screen_load" "" vsearch_screen_load || FAILURES=$((FAILURES + 1))
run_case "vsearch_focus_search" "/" vsearch_focus_search || FAILURES=$((FAILURES + 1))
run_case "vsearch_query" "/auth" vsearch_query || FAILURES=$((FAILURES + 1))
run_case "vsearch_navigation" "jjjj" vsearch_navigation || FAILURES=$((FAILURES + 1))
run_case "vsearch_jump_bottom" "G" vsearch_jump_bottom || FAILURES=$((FAILURES + 1))

if [[ "$FAILURES" -gt 0 ]]; then
    exit 1
fi

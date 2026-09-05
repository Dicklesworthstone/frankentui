#!/bin/bash
set -euo pipefail

# E2E tests for Theme Studio screen (Demo Showcase)
# bd-vu0o.4: Theme Studio — E2E PTY Tests (Verbose Logs)
#
# Scenarios:
# 1. Smoke test: render Theme Studio screen
# 2. Theme cycle: Ctrl+T cycles theme, status bar reflects change

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

JSONL_FILE="$E2E_RESULTS_DIR/theme_studio.jsonl"
RUN_ID="theme_studio_$(date +%Y%m%d_%H%M%S)_$$"
SEED="${FTUI_DEMO_SEED:-0}"

jsonl_log() {
    local line="$1"
    mkdir -p "$E2E_RESULTS_DIR"
    printf '%s\n' "$line" >> "$JSONL_FILE"
}

sha256_file() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1 && [[ -f "$file" ]]; then
        sha256sum "$file" | awk '{print $1}'
        return 0
    fi
    echo ""
    return 0
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

detect_theme_studio_screen() {
    local bin="$1"
    local help
    help="$($bin --help 2>/dev/null || true)"
    if [[ -z "$help" ]]; then
        return 1
    fi
    local line
    line=$(printf '%s\n' "$help" | grep -Ei "Theme Studio" | head -n 1 || true)
    if [[ -z "$line" ]]; then
        return 1
    fi
    local screen
    screen=$(printf '%s' "$line" | awk '{print $1}')
    if [[ ! "$screen" =~ ^[0-9]+$ ]]; then
        return 1
    fi
    printf '%s' "$screen"
    return 0
}

run_case() {
    local name="$1"
    local send_label="$2"
    shift 2
    local start_ms
    # BSD date lacks %N; elapsed timing uses the real monotonic clock.
    start_ms="$(e2e_monotonic_ms)" || return 2

    LOG_FILE="$E2E_LOG_DIR/${name}.log"
    local output_file="$E2E_LOG_DIR/${name}.pty"

    log_test_start "$name"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)" || return 2
        local duration_ms=$((end_ms - start_ms))
        local size
        size=$(wc -c < "$output_file" | tr -d ' ')
        local output_sha
        output_sha="$(sha256_file "$output_file")"
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"passed\",\"duration_ms\":$duration_ms,\"output_bytes\":$size,\"output_sha256\":\"$output_sha\",\"send\":\"$send_label\",\"cols\":120,\"rows\":40,\"seed\":\"$SEED\",\"screen\":\"${THEME_STUDIO_SCREEN:-}\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\"}"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))
    local output_sha
    output_sha="$(sha256_file "$output_file")"
    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"failed\",\"duration_ms\":$duration_ms,\"output_sha256\":\"$output_sha\",\"send\":\"$send_label\",\"cols\":120,\"rows\":40,\"seed\":\"$SEED\",\"screen\":\"${THEME_STUDIO_SCREEN:-}\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\"}"
    return 1
}

DEMO_BIN="$(ensure_demo_bin || true)"
if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/theme_studio_missing.log"
    for t in theme_studio_smoke theme_studio_cycle; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"status\":\"skipped\",\"reason\":\"binary missing\",\"seed\":\"$SEED\",\"screen\":\"${THEME_STUDIO_SCREEN:-}\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\"}"
    done
    exit 0
fi

THEME_STUDIO_SCREEN="$(detect_theme_studio_screen "$DEMO_BIN" || true)"
if [[ -z "$THEME_STUDIO_SCREEN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/theme_studio_missing.log"
    for t in theme_studio_smoke theme_studio_cycle; do
        log_test_skip "$t" "Theme Studio screen not registered in --help"
        record_result "$t" "skipped" 0 "$LOG_FILE" "screen missing"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"status\":\"skipped\",\"reason\":\"screen missing\",\"seed\":\"$SEED\",\"screen\":\"${THEME_STUDIO_SCREEN:-}\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\"}"
    done
    exit 0
fi

# Control bytes
CTRL_T='\x14'

# Test 1: Smoke test (render Theme Studio screen)
theme_studio_smoke() {
    LOG_FILE="$E2E_LOG_DIR/theme_studio_smoke.log"
    local output_file="$E2E_LOG_DIR/theme_studio_smoke.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="" \
    FTUI_DEMO_SCREEN="$THEME_STUDIO_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    # Expect the screen title to appear in the output
    grep -a -qi "Theme Studio" "$output_file" || return 1
}

# Test 2: Cycle theme via Ctrl+T
# Expect at least two distinct theme names to appear in the PTY capture.
theme_studio_cycle() {
    LOG_FILE="$E2E_LOG_DIR/theme_studio_cycle.log"
    local output_file="$E2E_LOG_DIR/theme_studio_cycle.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="$CTRL_T" \
    FTUI_DEMO_SCREEN="$THEME_STUDIO_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    local count=0
    local theme_names=("Cyberpunk" "Darcula" "Lumen" "Nordic" "High Contrast")
    local theme_name
    for theme_name in "${theme_names[@]}"; do
        if grep -a -q "$theme_name" "$output_file"; then
            count=$((count + 1))
        fi
    done
    [[ "$count" -ge 2 ]]
}

FAILURES=0
run_case "theme_studio_smoke" "" theme_studio_smoke || FAILURES=$((FAILURES + 1))
run_case "theme_studio_cycle" "CTRL_T" theme_studio_cycle || FAILURES=$((FAILURES + 1))

if [[ "$FAILURES" -gt 0 ]]; then
    exit 1
fi

exit 0

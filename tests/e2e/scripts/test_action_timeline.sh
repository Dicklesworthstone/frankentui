#!/bin/bash
set -euo pipefail

# E2E tests for Action Timeline / Event Stream Viewer (bd-11ck.4)
# PTY E2E tests with verbose JSONL logging (env, capabilities, timings, seed, checksums).
#
# Environment Variables:
#   FTUI_SEED       - Seed for deterministic mode (auto-generated if unset)
#   E2E_BENCHMARK   - Set to "1" to run hyperfine benchmarks after tests
#   E2E_VERBOSE     - Set to "1" for verbose output
#
# JSONL Schema (per-case entry):
#   run_id, case, status, duration_ms, ts, seed, cols, rows, send,
#   output_bytes, checksum, env, capabilities, timings

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

JSONL_FILE="$E2E_RESULTS_DIR/action_timeline.jsonl"
RUN_ID="timeline_$(date +%Y%m%d_%H%M%S)_$$"

# =========================================================================
# Deterministic mode: seed generation/capture (bd-11ck.4)
# =========================================================================
if [[ -z "${FTUI_SEED:-}" ]]; then
    FTUI_SEED="$(od -An -N4 -tu4 /dev/urandom 2>/dev/null | tr -d ' ' || date +%s)"
fi
export FTUI_SEED

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

jsonl_log() {
    local line="$1"
    mkdir -p "$E2E_RESULTS_DIR"
    printf '%s\n' "$line" >> "$JSONL_FILE"
}

jsonl_log_case() {
    local case="$1" status="$2" duration_ms="$3" send="$4" output_file="${5:-}"
    local output_bytes=0 checksum=""
    if [[ -n "$output_file" && -f "$output_file" ]]; then
        output_bytes=$(wc -c < "$output_file" | tr -d ' ')
        checksum="$(compute_checksum "$output_file")"
    fi
    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --arg run_id "$RUN_ID" \
            --arg case "$case" \
            --arg status "$status" \
            --argjson duration_ms "$duration_ms" \
            --arg ts "$(date -Iseconds)" \
            --arg seed "$FTUI_SEED" \
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
            "$RUN_ID" "$case" "$status" "$duration_ms" "$FTUI_SEED" "$output_bytes" "$checksum" >> "$JSONL_FILE"
    fi
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
    # BSD date lacks %N; elapsed timing uses the real monotonic clock.
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
    LOG_FILE="$E2E_LOG_DIR/action_timeline_missing.log"
    for t in timeline_initial timeline_navigate_down timeline_filter_toggle timeline_vim_nav timeline_details_toggle timeline_follow_toggle; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"status\":\"skipped\",\"reason\":\"binary missing\"}"
    done
    exit 0
fi

# Control bytes
TAB='\t'
ARROW_DOWN='\x1b[B'
ARROW_UP='\x1b[A'
PAGE_DOWN='\x1b[6~'
PAGE_UP='\x1b[5~'
HOME='\x1b[H'
END='\x1b[F'
ENTER='\r'

# Navigate to Action Timeline screen (index 16, so 16 tabs from Dashboard)
NAV_TO_TIMELINE=""
for _ in {1..16}; do
    NAV_TO_TIMELINE="${NAV_TO_TIMELINE}${TAB}"
done

# Test: Initial render of Action Timeline screen
timeline_initial() {
    LOG_FILE="$E2E_LOG_DIR/timeline_initial.log"
    local output_file="$E2E_LOG_DIR/timeline_initial.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="$NAV_TO_TIMELINE" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
    # Should show Timeline tab label or timeline content
    grep -a -q -E "(Timeline|Action|Event)" "$output_file" || return 1
}

# Test: Navigate down through events
timeline_navigate_down() {
    LOG_FILE="$E2E_LOG_DIR/timeline_navigate_down.log"
    local output_file="$E2E_LOG_DIR/timeline_navigate_down.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${NAV_TO_TIMELINE}${ARROW_DOWN}${ARROW_DOWN}${ARROW_DOWN}" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Toggle filter with 'f' key
timeline_filter_toggle() {
    LOG_FILE="$E2E_LOG_DIR/timeline_filter_toggle.log"
    local output_file="$E2E_LOG_DIR/timeline_filter_toggle.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${NAV_TO_TIMELINE}f" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Vim-style navigation with j/k
timeline_vim_nav() {
    LOG_FILE="$E2E_LOG_DIR/timeline_vim_nav.log"
    local output_file="$E2E_LOG_DIR/timeline_vim_nav.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${NAV_TO_TIMELINE}jjjkkk" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Toggle details with Enter
timeline_details_toggle() {
    LOG_FILE="$E2E_LOG_DIR/timeline_details_toggle.log"
    local output_file="$E2E_LOG_DIR/timeline_details_toggle.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${NAV_TO_TIMELINE}${ENTER}${ENTER}" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Toggle follow mode with 'f'
timeline_follow_toggle() {
    LOG_FILE="$E2E_LOG_DIR/timeline_follow_toggle.log"
    local output_file="$E2E_LOG_DIR/timeline_follow_toggle.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${NAV_TO_TIMELINE}ff" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Page navigation
timeline_page_nav() {
    LOG_FILE="$E2E_LOG_DIR/timeline_page_nav.log"
    local output_file="$E2E_LOG_DIR/timeline_page_nav.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${NAV_TO_TIMELINE}${PAGE_DOWN}${PAGE_UP}" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Home/End navigation
timeline_home_end() {
    LOG_FILE="$E2E_LOG_DIR/timeline_home_end.log"
    local output_file="$E2E_LOG_DIR/timeline_home_end.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${NAV_TO_TIMELINE}${END}${HOME}" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Cycle component filter with 'c'
timeline_filter_component() {
    LOG_FILE="$E2E_LOG_DIR/timeline_filter_component.log"
    local output_file="$E2E_LOG_DIR/timeline_filter_component.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${NAV_TO_TIMELINE}ccc" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Cycle severity filter with 's'
timeline_filter_severity() {
    LOG_FILE="$E2E_LOG_DIR/timeline_filter_severity.log"
    local output_file="$E2E_LOG_DIR/timeline_filter_severity.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${NAV_TO_TIMELINE}sss" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Clear filters with 'x'
timeline_clear_filters() {
    LOG_FILE="$E2E_LOG_DIR/timeline_clear_filters.log"
    local output_file="$E2E_LOG_DIR/timeline_clear_filters.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${NAV_TO_TIMELINE}csx" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Log run metadata with full schema (bd-11ck.4)
if command -v jq >/dev/null 2>&1; then
    jq -nc \
        --arg run_id "$RUN_ID" \
        --arg event "run_start" \
        --arg ts "$(date -Iseconds)" \
        --arg seed "$FTUI_SEED" \
        --arg binary "$DEMO_BIN" \
        --arg script "test_action_timeline.sh" \
        --argjson env "$ENV_JSON" \
        --argjson capabilities "$CAPS_JSON" \
        '{run_id:$run_id,event:$event,ts:$ts,seed:$seed,binary:$binary,script:$script,env:$env,capabilities:$capabilities}' \
        >> "$JSONL_FILE"
else
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"event\":\"run_start\",\"ts\":\"$(date -Iseconds)\",\"seed\":\"$FTUI_SEED\",\"binary\":\"$DEMO_BIN\"}"
fi

FAILURES=0
run_case "timeline_initial" "<16xTab>" timeline_initial || FAILURES=$((FAILURES + 1))
run_case "timeline_navigate_down" "<16xTab><Down><Down><Down>" timeline_navigate_down || FAILURES=$((FAILURES + 1))
run_case "timeline_filter_toggle" "<16xTab>f" timeline_filter_toggle || FAILURES=$((FAILURES + 1))
run_case "timeline_vim_nav" "<16xTab>jjjkkk" timeline_vim_nav || FAILURES=$((FAILURES + 1))
run_case "timeline_details_toggle" "<16xTab><Enter><Enter>" timeline_details_toggle || FAILURES=$((FAILURES + 1))
run_case "timeline_follow_toggle" "<16xTab>ff" timeline_follow_toggle || FAILURES=$((FAILURES + 1))
run_case "timeline_page_nav" "<16xTab><PgDn><PgUp>" timeline_page_nav || FAILURES=$((FAILURES + 1))
run_case "timeline_home_end" "<16xTab><End><Home>" timeline_home_end || FAILURES=$((FAILURES + 1))
run_case "timeline_filter_component" "<16xTab>ccc" timeline_filter_component || FAILURES=$((FAILURES + 1))
run_case "timeline_filter_severity" "<16xTab>sss" timeline_filter_severity || FAILURES=$((FAILURES + 1))
run_case "timeline_clear_filters" "<16xTab>csx" timeline_clear_filters || FAILURES=$((FAILURES + 1))

# Log run summary with full schema (bd-11ck.4)
TOTAL_TESTS=11
PASSED=$((TOTAL_TESTS - FAILURES))
if command -v jq >/dev/null 2>&1; then
    jq -nc \
        --arg run_id "$RUN_ID" \
        --arg event "run_end" \
        --arg ts "$(date -Iseconds)" \
        --arg seed "$FTUI_SEED" \
        --argjson total_tests "$TOTAL_TESTS" \
        --argjson passed "$PASSED" \
        --argjson failed "$FAILURES" \
        '{run_id:$run_id,event:$event,ts:$ts,seed:$seed,total_tests:$total_tests,passed:$passed,failed:$failed}' \
        >> "$JSONL_FILE"
else
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"event\":\"run_end\",\"ts\":\"$(date -Iseconds)\",\"seed\":\"$FTUI_SEED\",\"total_tests\":$TOTAL_TESTS,\"passed\":$PASSED,\"failed\":$FAILURES}"
fi

if [[ "$FAILURES" -gt 0 ]]; then
    log_error "Action Timeline E2E tests: $FAILURES failures"
else
    log_info "Action Timeline E2E tests: all passed"
fi

# =========================================================================
# Optional: Hyperfine performance benchmarks (bd-11ck.4)
# Run with: E2E_BENCHMARK=1 ./tests/e2e/scripts/test_action_timeline.sh
# =========================================================================
if [[ "${E2E_BENCHMARK:-}" == "1" ]]; then
    BENCH_RESULTS="$E2E_RESULTS_DIR/action_timeline_bench.json"
    if command -v hyperfine >/dev/null 2>&1; then
        log_info "Running hyperfine benchmarks for action timeline startup..."

        # Benchmark startup render time (p50/p95/p99)
        hyperfine \
            --warmup 2 \
            --runs 10 \
            --export-json "$BENCH_RESULTS" \
            --export-markdown "$E2E_RESULTS_DIR/action_timeline_bench.md" \
            "FTUI_HARNESS_VIEW=action_timeline FTUI_DEMO_EXIT_AFTER_MS=200 $DEMO_BIN" \
            2>&1 | tee "$E2E_LOG_DIR/hyperfine.log" || true

        # Log benchmark results to JSONL
        if [[ -f "$BENCH_RESULTS" ]] && command -v jq >/dev/null 2>&1; then
            mean_ms=$(jq -r '.results[0].mean * 1000 | floor' "$BENCH_RESULTS" 2>/dev/null || echo 0)
            median_ms=$(jq -r '.results[0].median * 1000 | floor' "$BENCH_RESULTS" 2>/dev/null || echo 0)
            min_ms=$(jq -r '.results[0].min * 1000 | floor' "$BENCH_RESULTS" 2>/dev/null || echo 0)
            max_ms=$(jq -r '.results[0].max * 1000 | floor' "$BENCH_RESULTS" 2>/dev/null || echo 0)
            stddev_ms=$(jq -r '.results[0].stddev * 1000 | floor' "$BENCH_RESULTS" 2>/dev/null || echo 0)

            jq -nc \
                --arg run_id "$RUN_ID" \
                --arg event "benchmark" \
                --arg ts "$(date -Iseconds)" \
                --arg seed "$FTUI_SEED" \
                --arg benchmark "startup" \
                --argjson mean_ms "$mean_ms" \
                --argjson median_ms "$median_ms" \
                --argjson min_ms "$min_ms" \
                --argjson max_ms "$max_ms" \
                --argjson stddev_ms "$stddev_ms" \
                '{run_id:$run_id,event:$event,ts:$ts,seed:$seed,benchmark:$benchmark,mean_ms:$mean_ms,median_ms:$median_ms,min_ms:$min_ms,max_ms:$max_ms,stddev_ms:$stddev_ms}' \
                >> "$JSONL_FILE"

            log_info "Benchmark results: mean=${mean_ms}ms, median=${median_ms}ms, min=${min_ms}ms, max=${max_ms}ms"
        fi
    else
        log_warn "hyperfine not found, skipping benchmarks (install with: cargo install hyperfine)"
    fi
fi

# Print seed for reproducibility
log_info "Run completed with seed: $FTUI_SEED (use FTUI_SEED=$FTUI_SEED to reproduce)"

exit "$FAILURES"

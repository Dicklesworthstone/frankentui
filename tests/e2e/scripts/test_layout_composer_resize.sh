#!/bin/bash
set -euo pipefail

# E2E tests for Layout Laboratory resize regressions (Demo Showcase)
# bd-32my.2: Layout Composer — Resize Regression Tests
#
# Scenarios (basic profile):
# 1. Resize down: 120x40 -> 80x24
# 2. Resize up: 80x24 -> 200x50
# 3. Resize tiny: 120x40 -> 40x10
#
# Scenarios (thrash profile, bd-1pys5.2):
# 4. Rapid oscillation: 120x40 <-> 80x24 every 16ms
# 5. Progressive shrink/grow round-trip: 120x40 -> 10x5 -> 120x40
# 6. Progressive grow/shrink round-trip: 10x5 -> 300x100 -> 10x5
# 7. Random resize stream (10s) with deterministic seed and round-trip
# 8. Extreme sizes: 1x1 -> 1000x1000 -> 10x10000 -> back to origin
#
# Logging: JSONL with env/capabilities, seed, timings, checksums.
# Optional benchmarks: set E2E_BENCHMARK=1 to run hyperfine baseline.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

JSONL_FILE="$E2E_RESULTS_DIR/layout_composer_resize.jsonl"
RUN_ID="layout_resize_$(e2e_log_stamp)"
SEED="${LAYOUT_RESIZE_SEED:-0}"
PROFILE="${LAYOUT_RESIZE_PROFILE:-basic}" # basic | thrash | all
THRASH_INTERVAL_MS="${LAYOUT_THRASH_INTERVAL_MS:-16}"
THRASH_RANDOM_DURATION_MS="${LAYOUT_THRASH_RANDOM_DURATION_MS:-10000}"
THRASH_EXIT_AFTER_MS="${LAYOUT_THRASH_EXIT_AFTER_MS:-12000}"
THRASH_TIMEOUT_SECONDS="${LAYOUT_THRASH_TIMEOUT_SECONDS:-20}"
ROUNDTRIP_TAIL_BYTES="${LAYOUT_ROUNDTRIP_TAIL_BYTES:-4096}"

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
    if command -v shasum >/dev/null 2>&1 && [[ -f "$file" ]]; then
        shasum -a 256 "$file" | awk '{print $1}'
        return 0
    fi
    echo ""
    return 0
}

sha256_text() {
    local text="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        printf '%s' "$text" | sha256sum | awk '{print $1}'
        return 0
    fi
    if command -v shasum >/dev/null 2>&1; then
        printf '%s' "$text" | shasum -a 256 | awk '{print $1}'
        return 0
    fi
    echo ""
    return 0
}

final_view_sha256() {
    local file="$1"
    local tail_bytes="$2"
    if [[ ! -f "$file" ]]; then
        echo ""
        return 0
    fi
    if [[ -n "${E2E_PYTHON:-}" ]]; then
        "$E2E_PYTHON" - "$file" "$tail_bytes" <<'PY'
import hashlib
import re
import sys

path = sys.argv[1]
tail = int(sys.argv[2])

with open(path, "rb") as handle:
    text = handle.read().decode("utf-8", errors="replace")

text = re.sub(r"\x1b\[[0-9;?]*[ -/]*[@-~]", "", text).replace("\r", "\n")
marker = text.rfind("Layout Lab [")
if marker >= 0:
    start = max(0, marker - tail)
    segment = text[start:marker]
else:
    segment = text[-tail:]

print(hashlib.sha256(segment.encode("utf-8")).hexdigest())
PY
        return 0
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        if command -v perl >/dev/null 2>&1; then
            perl -pe 's/\e\[[0-9;?]*[ -\/]*[@-~]//g; s/\r/\n/g' "$file" | tail -c "$tail_bytes" | sha256sum | awk '{print $1}'
            return 0
        fi
        tail -c "$tail_bytes" "$file" | sha256sum | awk '{print $1}'
        return 0
    fi
    if command -v shasum >/dev/null 2>&1; then
        if command -v perl >/dev/null 2>&1; then
            perl -pe 's/\e\[[0-9;?]*[ -\/]*[@-~]//g; s/\r/\n/g' "$file" | tail -c "$tail_bytes" | shasum -a 256 | awk '{print $1}'
            return 0
        fi
        tail -c "$tail_bytes" "$file" | shasum -a 256 | awk '{print $1}'
        return 0
    fi
    echo ""
    return 0
}

count_resize_events() {
    local sequence="$1"
    if [[ -z "$sequence" ]]; then
        echo 0
        return 0
    fi
    awk -F';' '{print NF}' <<< "$sequence"
}

build_oscillation_sequence() {
    local start_cols="$1" start_rows="$2" alt_cols="$3" alt_rows="$4" interval_ms="$5" steps="$6"
    local delay=0
    local sequence=""
    local i
    for ((i = 0; i < steps; i++)); do
        if (( i % 2 == 0 )); then
            sequence+="${delay}:${alt_cols}x${alt_rows};"
        else
            sequence+="${delay}:${start_cols}x${start_rows};"
        fi
        delay=$((delay + interval_ms))
    done
    if (( steps % 2 != 0 )); then
        sequence+="${delay}:${start_cols}x${start_rows};"
    fi
    printf '%s' "${sequence%;}"
}

build_progressive_roundtrip_sequence() {
    local start_cols="$1" start_rows="$2" end_cols="$3" end_rows="$4" interval_ms="$5"
    local current_cols="$start_cols"
    local current_rows="$start_rows"
    local delay=0
    local sequence=""

    while (( current_cols != end_cols || current_rows != end_rows )); do
        if (( current_cols < end_cols )); then
            current_cols=$((current_cols + 1))
        elif (( current_cols > end_cols )); then
            current_cols=$((current_cols - 1))
        fi

        if (( current_rows < end_rows )); then
            current_rows=$((current_rows + 1))
        elif (( current_rows > end_rows )); then
            current_rows=$((current_rows - 1))
        fi

        sequence+="${delay}:${current_cols}x${current_rows};"
        delay=$((delay + interval_ms))
    done

    while (( current_cols != start_cols || current_rows != start_rows )); do
        if (( current_cols < start_cols )); then
            current_cols=$((current_cols + 1))
        elif (( current_cols > start_cols )); then
            current_cols=$((current_cols - 1))
        fi

        if (( current_rows < start_rows )); then
            current_rows=$((current_rows + 1))
        elif (( current_rows > start_rows )); then
            current_rows=$((current_rows - 1))
        fi

        sequence+="${delay}:${current_cols}x${current_rows};"
        delay=$((delay + interval_ms))
    done

    printf '%s' "${sequence%;}"
}

build_random_roundtrip_sequence() {
    local start_cols="$1" start_rows="$2" interval_ms="$3" duration_ms="$4"
    local min_cols=20 max_cols=220
    local min_rows=8 max_rows=80
    local state="$SEED"
    local steps=$((duration_ms / interval_ms))
    local delay=0
    local sequence=""
    local i

    if [[ "$state" -eq 0 ]]; then
        state=1
    fi

    for ((i = 0; i < steps; i++)); do
        state=$(( (1664525 * state + 1013904223) & 0xffffffff ))
        local next_cols=$((min_cols + (state % (max_cols - min_cols + 1))))
        state=$(( (1664525 * state + 1013904223) & 0xffffffff ))
        local next_rows=$((min_rows + (state % (max_rows - min_rows + 1))))
        sequence+="${delay}:${next_cols}x${next_rows};"
        delay=$((delay + interval_ms))
    done
    sequence+="${delay}:${start_cols}x${start_rows};"
    printf '%s' "${sequence%;}"
}

build_extreme_roundtrip_sequence() {
    local start_cols="$1" start_rows="$2" interval_ms="$3"
    printf '0:1x1;%s:1000x1000;%s:10x10000;%s:%sx%s' \
        "$interval_ms" \
        "$((interval_ms * 2))" \
        "$((interval_ms * 3))" \
        "$start_cols" \
        "$start_rows"
}

collect_env_json() {
    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --arg os "$(uname -s)" \
            --arg arch "$(uname -m)" \
            --arg term "${TERM:-}" \
            --arg colorterm "${COLORTERM:-}" \
            --arg tmux "${TMUX:-}" \
            --arg zellij "${ZELLIJ:-}" \
            '{os:$os,arch:$arch,term:$term,colorterm:$colorterm,tmux:$tmux,zellij:$zellij}'
    else
        printf '{"os":"%s","arch":"%s","term":"%s"}' "$(uname -s)" "$(uname -m)" "${TERM:-}"
    fi
}

detect_capabilities_json() {
    local truecolor="false" color256="false" mux="none"
    [[ "${COLORTERM:-}" == "truecolor" || "${COLORTERM:-}" == "24bit" ]] && truecolor="true"
    [[ "${TERM:-}" == *"256color"* ]] && color256="true"
    [[ -n "${TMUX:-}" ]] && mux="tmux"
    [[ -n "${ZELLIJ:-}" ]] && mux="zellij"
    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --argjson truecolor "$truecolor" \
            --argjson color256 "$color256" \
            --arg mux "$mux" \
            '{truecolor:$truecolor,color_256:$color256,mux:$mux}'
    else
        printf '{"truecolor":%s,"color_256":%s,"mux":"%s"}' "$truecolor" "$color256" "$mux"
    fi
}

ENV_JSON="$(collect_env_json)"
CAPS_JSON="$(detect_capabilities_json)"

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

detect_layout_screen() {
    local bin="$1"
    local help
    help="$($bin --help 2>/dev/null || true)"
    if [[ -z "$help" ]]; then
        return 1
    fi
    local line
    line=$(printf '%s\n' "$help" | command grep -E "Layout Lab|Layout Laboratory" | head -n 1 || true)
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
    local name="$1" send_label="$2" start_cols="$3" start_rows="$4" resize_cols="$5" resize_rows="$6"
    shift 6
    local start_ms
    start_ms="$(e2e_now_ms)"

    LOG_FILE="$E2E_LOG_DIR/${name}.log"
    local output_file="$E2E_LOG_DIR/${name}.pty"

    log_test_start "$name"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_now_ms)"
        local duration_ms=$((end_ms - start_ms))
        local size
        size=$(wc -c < "$output_file" | tr -d ' ')
        local output_sha
        output_sha="$(sha256_file "$output_file")"
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"passed\",\"duration_ms\":$duration_ms,\"output_bytes\":$size,\"output_sha256\":\"$output_sha\",\"send\":\"$send_label\",\"cols\":$start_cols,\"rows\":$start_rows,\"resize_cols\":$resize_cols,\"resize_rows\":$resize_rows,\"seed\":\"$SEED\",\"env\":$ENV_JSON,\"capabilities\":$CAPS_JSON}"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_now_ms)"
    local duration_ms=$((end_ms - start_ms))
    local output_sha
    output_sha="$(sha256_file "$output_file")"
    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"failed\",\"duration_ms\":$duration_ms,\"output_sha256\":\"$output_sha\",\"send\":\"$send_label\",\"cols\":$start_cols,\"rows\":$start_rows,\"resize_cols\":$resize_cols,\"resize_rows\":$resize_rows,\"seed\":\"$SEED\",\"env\":$ENV_JSON,\"capabilities\":$CAPS_JSON}"
    return 1
}

declare -A BASELINE_VIEW_HASHES=()

baseline_view_hash() {
    local cols="$1" rows="$2" exit_after_ms="$3"
    local key="${cols}x${rows}@${exit_after_ms}"
    if [[ -n "${BASELINE_VIEW_HASHES[$key]:-}" ]]; then
        printf '%s' "${BASELINE_VIEW_HASHES[$key]}"
        return 0
    fi

    local output_file="$E2E_LOG_DIR/layout_resize_baseline_${cols}x${rows}_${exit_after_ms}.pty"
    local timeout="$(( (exit_after_ms / 1000) + 8 ))"
    LOG_FILE="$E2E_LOG_DIR/layout_resize_baseline_${cols}x${rows}_${exit_after_ms}.log"

    PTY_COLS="$cols" \
    PTY_ROWS="$rows" \
    FTUI_DEMO_EXIT_AFTER_MS="$exit_after_ms" \
    PTY_TIMEOUT="$timeout" \
        pty_run "$output_file" "$DEMO_BIN" >/dev/null 2>&1 || return 1

    local baseline_hash
    baseline_hash="$(final_view_sha256 "$output_file" "$ROUNDTRIP_TAIL_BYTES")"
    BASELINE_VIEW_HASHES[$key]="$baseline_hash"
    printf '%s' "$baseline_hash"
}

run_thrash_case() {
    local name="$1" start_cols="$2" start_rows="$3" resize_sequence="$4" duration_bound_ms="$5" exit_after_ms="$6"
    local start_ms
    start_ms="$(e2e_now_ms)"

    LOG_FILE="$E2E_LOG_DIR/${name}.log"
    local output_file="$E2E_LOG_DIR/${name}.pty"
    local resize_events
    resize_events="$(count_resize_events "$resize_sequence")"
    local sequence_sha
    sequence_sha="$(sha256_text "$resize_sequence")"

    log_test_start "$name"

    local timeout="$THRASH_TIMEOUT_SECONDS"
    if (( timeout < (exit_after_ms / 1000 + 4) )); then
        timeout=$(( (exit_after_ms / 1000) + 4 ))
    fi

    if PTY_COLS="$start_cols" \
        PTY_ROWS="$start_rows" \
        PTY_RESIZE_SEQUENCE="$resize_sequence" \
        FTUI_DEMO_EXIT_AFTER_MS="$exit_after_ms" \
        PTY_TIMEOUT="$timeout" \
            pty_run "$output_file" "$DEMO_BIN"; then
        local end_ms
        end_ms="$(e2e_now_ms)"
        local duration_ms=$((end_ms - start_ms))
        local output_sha
        output_sha="$(sha256_file "$output_file")"
        local final_view_hash
        final_view_hash="$(final_view_sha256 "$output_file" "$ROUNDTRIP_TAIL_BYTES")"
        local baseline_hash
        baseline_hash="$(baseline_view_hash "$start_cols" "$start_rows" "$exit_after_ms" || true)"

        local roundtrip_ok=true
        if [[ -z "$baseline_hash" || -z "$final_view_hash" || "$baseline_hash" != "$final_view_hash" ]]; then
            roundtrip_ok=false
        fi

        local duration_bound_ok=true
        if (( duration_ms > duration_bound_ms )); then
            duration_bound_ok=false
        fi

        local ansi_integrity_ok=true
        if command grep -a -q $'\x1b\\[[0-9;?]*$' "$output_file"; then
            ansi_integrity_ok=false
        fi

        local content_ok=true
        command grep -a -qi "Layout Laboratory\|Preset" "$output_file" || content_ok=false

        local status="passed"
        local failure_reason=""
        if [[ "$roundtrip_ok" != "true" || "$duration_bound_ok" != "true" || "$ansi_integrity_ok" != "true" || "$content_ok" != "true" ]]; then
            status="failed"
            failure_reason="roundtrip=$roundtrip_ok duration_bound=$duration_bound_ok ansi_integrity=$ansi_integrity_ok content=$content_ok"
        fi

        local output_bytes
        output_bytes=$(wc -c < "$output_file" | tr -d ' ')
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"scenario\":\"thrash\",\"status\":\"$status\",\"duration_ms\":$duration_ms,\"output_bytes\":$output_bytes,\"output_sha256\":\"$output_sha\",\"final_view_sha256\":\"$final_view_hash\",\"baseline_view_sha256\":\"$baseline_hash\",\"resize_events\":$resize_events,\"sequence_sha256\":\"$sequence_sha\",\"duration_bound_ms\":$duration_bound_ms,\"duration_bound_ok\":$duration_bound_ok,\"roundtrip_ok\":$roundtrip_ok,\"ansi_integrity_ok\":$ansi_integrity_ok,\"seed\":\"$SEED\",\"env\":$ENV_JSON,\"capabilities\":$CAPS_JSON}"

        if [[ "$status" == "passed" ]]; then
            log_test_pass "$name"
            record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
            return 0
        fi

        log_test_fail "$name" "$failure_reason"
        record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "$failure_reason"
        return 1
    fi

    local end_ms
    end_ms="$(e2e_now_ms)"
    local duration_ms=$((end_ms - start_ms))
    local output_sha
    output_sha="$(sha256_file "$output_file")"
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"scenario\":\"thrash\",\"status\":\"failed\",\"duration_ms\":$duration_ms,\"output_sha256\":\"$output_sha\",\"resize_events\":$resize_events,\"sequence_sha256\":\"$sequence_sha\",\"duration_bound_ms\":$duration_bound_ms,\"duration_bound_ok\":false,\"roundtrip_ok\":false,\"ansi_integrity_ok\":false,\"seed\":\"$SEED\",\"env\":$ENV_JSON,\"capabilities\":$CAPS_JSON}"
    log_test_fail "$name" "command failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "command failed"
    return 1
}

BASIC_CASES=(layout_resize_down layout_resize_up layout_resize_tiny)
THRASH_CASES=(
    layout_resize_thrash_oscillation
    layout_resize_thrash_progressive_shrink_roundtrip
    layout_resize_thrash_progressive_grow_roundtrip
    layout_resize_thrash_random_roundtrip
    layout_resize_thrash_extreme_roundtrip
)
ALL_CASES=("${BASIC_CASES[@]}")
if [[ "$PROFILE" == "thrash" || "$PROFILE" == "all" ]]; then
    ALL_CASES+=("${THRASH_CASES[@]}")
fi

DEMO_BIN="$(ensure_demo_bin || true)"
if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/layout_resize_missing.log"
    for t in "${ALL_CASES[@]}"; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"status\":\"skipped\",\"reason\":\"binary missing\",\"seed\":\"$SEED\"}"
    done
    exit 0
fi

LAYOUT_SCREEN="$(detect_layout_screen "$DEMO_BIN" || true)"
if [[ -z "$LAYOUT_SCREEN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/layout_resize_missing.log"
    for t in "${ALL_CASES[@]}"; do
        log_test_skip "$t" "Layout Laboratory screen not registered in --help"
        record_result "$t" "skipped" 0 "$LOG_FILE" "screen missing"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"status\":\"skipped\",\"reason\":\"screen missing\",\"seed\":\"$SEED\"}"
    done
    exit 0
fi

export FTUI_DEMO_SCREEN="$LAYOUT_SCREEN"
export FTUI_DEMO_SEED="$SEED"

layout_resize_down() {
    LOG_FILE="$E2E_LOG_DIR/layout_resize_down.log"
    local output_file="$E2E_LOG_DIR/layout_resize_down.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_RESIZE_DELAY_MS=300 \
    PTY_RESIZE_COLS=80 \
    PTY_RESIZE_ROWS=24 \
    FTUI_DEMO_EXIT_AFTER_MS=1600 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    command grep -a -qi "Layout Laboratory\|Preset" "$output_file" || return 1
}

layout_resize_up() {
    LOG_FILE="$E2E_LOG_DIR/layout_resize_up.log"
    local output_file="$E2E_LOG_DIR/layout_resize_up.pty"

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_RESIZE_DELAY_MS=300 \
    PTY_RESIZE_COLS=200 \
    PTY_RESIZE_ROWS=50 \
    FTUI_DEMO_EXIT_AFTER_MS=1600 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1
    command grep -a -qi "Layout Laboratory\|Preset" "$output_file" || return 1
}

layout_resize_tiny() {
    LOG_FILE="$E2E_LOG_DIR/layout_resize_tiny.log"
    local output_file="$E2E_LOG_DIR/layout_resize_tiny.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_RESIZE_DELAY_MS=300 \
    PTY_RESIZE_COLS=40 \
    PTY_RESIZE_ROWS=10 \
    FTUI_DEMO_EXIT_AFTER_MS=1600 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1
    command grep -a -qi "Layout Laboratory\|Preset" "$output_file" || return 1
}

FAILURES=0
TOTAL_TESTS=0

TOTAL_TESTS=$((TOTAL_TESTS + 1))
run_case "layout_resize_down" "" 120 40 80 24 layout_resize_down || FAILURES=$((FAILURES + 1))

TOTAL_TESTS=$((TOTAL_TESTS + 1))
run_case "layout_resize_up" "" 80 24 200 50 layout_resize_up || FAILURES=$((FAILURES + 1))

TOTAL_TESTS=$((TOTAL_TESTS + 1))
run_case "layout_resize_tiny" "" 120 40 40 10 layout_resize_tiny || FAILURES=$((FAILURES + 1))

if [[ "$PROFILE" == "thrash" || "$PROFILE" == "all" ]]; then
    oscillation_sequence="$(build_oscillation_sequence 120 40 80 24 "$THRASH_INTERVAL_MS" 120)"
    shrink_roundtrip_sequence="$(build_progressive_roundtrip_sequence 120 40 10 5 "$THRASH_INTERVAL_MS")"
    grow_roundtrip_sequence="$(build_progressive_roundtrip_sequence 10 5 300 100 "$THRASH_INTERVAL_MS")"
    random_roundtrip_sequence="$(build_random_roundtrip_sequence 120 40 "$THRASH_INTERVAL_MS" "$THRASH_RANDOM_DURATION_MS")"
    extreme_roundtrip_sequence="$(build_extreme_roundtrip_sequence 120 40 "$THRASH_INTERVAL_MS")"

    random_exit_after_ms="$THRASH_EXIT_AFTER_MS"
    if (( random_exit_after_ms < THRASH_RANDOM_DURATION_MS + 1500 )); then
        random_exit_after_ms=$((THRASH_RANDOM_DURATION_MS + 1500))
    fi

    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    run_thrash_case \
        "layout_resize_thrash_oscillation" \
        120 \
        40 \
        "$oscillation_sequence" \
        7000 \
        3500 || FAILURES=$((FAILURES + 1))

    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    run_thrash_case \
        "layout_resize_thrash_progressive_shrink_roundtrip" \
        120 \
        40 \
        "$shrink_roundtrip_sequence" \
        9000 \
        6000 || FAILURES=$((FAILURES + 1))

    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    run_thrash_case \
        "layout_resize_thrash_progressive_grow_roundtrip" \
        10 \
        5 \
        "$grow_roundtrip_sequence" \
        20000 \
        11000 || FAILURES=$((FAILURES + 1))

    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    run_thrash_case \
        "layout_resize_thrash_random_roundtrip" \
        120 \
        40 \
        "$random_roundtrip_sequence" \
        "$((THRASH_RANDOM_DURATION_MS + 8000))" \
        "$random_exit_after_ms" || FAILURES=$((FAILURES + 1))

    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    run_thrash_case \
        "layout_resize_thrash_extreme_roundtrip" \
        120 \
        40 \
        "$extreme_roundtrip_sequence" \
        9000 \
        4000 || FAILURES=$((FAILURES + 1))
fi

PASSED=$((TOTAL_TESTS - FAILURES))
if command -v jq >/dev/null 2>&1; then
    jq -nc \
        --arg run_id "$RUN_ID" \
        --arg event "run_end" \
        --arg ts "$(e2e_timestamp)" \
        --arg seed "$SEED" \
        --argjson total_tests "$TOTAL_TESTS" \
        --argjson passed "$PASSED" \
        --argjson failed "$FAILURES" \
        '{run_id:$run_id,event:$event,ts:$ts,seed:$seed,total_tests:$total_tests,passed:$passed,failed:$failed}' \
        >> "$JSONL_FILE"
else
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"event\":\"run_end\",\"ts\":\"$(e2e_timestamp)\",\"seed\":\"$SEED\",\"total_tests\":$TOTAL_TESTS,\"passed\":$PASSED,\"failed\":$FAILURES}"
fi

# Optional: Hyperfine baseline (p50/p95/p99) for startup+render.
if [[ "${E2E_BENCHMARK:-}" == "1" ]]; then
    BENCH_RESULTS="$E2E_RESULTS_DIR/layout_resize_bench.json"
    if command -v hyperfine >/dev/null 2>&1; then
        log_info "Running hyperfine benchmarks for layout lab startup..."
        hyperfine \
            --warmup 2 \
            --runs 10 \
            --export-json "$BENCH_RESULTS" \
            --export-markdown "$E2E_RESULTS_DIR/layout_resize_bench.md" \
            "FTUI_DEMO_SCREEN=$LAYOUT_SCREEN FTUI_DEMO_EXIT_AFTER_MS=200 $DEMO_BIN" \
            2>&1 | tee "$E2E_LOG_DIR/hyperfine.log" || true

        if [[ -f "$BENCH_RESULTS" ]] && command -v jq >/dev/null 2>&1; then
            stats=$(jq -r '
                def pct(p):
                    . as $t
                    | ($t | length) as $n
                    | ( ($n - 1) * p | floor ) as $i
                    | $t[$i];
                .results[0].times
                | sort
                | {p50: pct(0.5), p95: pct(0.95), p99: pct(0.99)}
            ' "$BENCH_RESULTS" 2>/dev/null || echo "")
            p50_ms=$(printf '%s' "$stats" | jq -r '.p50 * 1000 | floor' 2>/dev/null || echo 0)
            p95_ms=$(printf '%s' "$stats" | jq -r '.p95 * 1000 | floor' 2>/dev/null || echo 0)
            p99_ms=$(printf '%s' "$stats" | jq -r '.p99 * 1000 | floor' 2>/dev/null || echo 0)

            jq -nc \
                --arg run_id "$RUN_ID" \
                --arg event "benchmark" \
                --arg ts "$(e2e_timestamp)" \
                --arg seed "$SEED" \
                --arg benchmark "startup" \
                --argjson p50_ms "$p50_ms" \
                --argjson p95_ms "$p95_ms" \
                --argjson p99_ms "$p99_ms" \
                '{run_id:$run_id,event:$event,ts:$ts,seed:$seed,benchmark:$benchmark,p50_ms:$p50_ms,p95_ms:$p95_ms,p99_ms:$p99_ms}' \
                >> "$JSONL_FILE"

            log_info "Benchmark percentiles: p50=${p50_ms}ms, p95=${p95_ms}ms, p99=${p99_ms}ms"
        fi
    else
        log_warn "hyperfine not found, skipping benchmarks (install with: cargo install hyperfine)"
    fi
fi

# Print seed for reproducibility
log_info "Run completed with seed: $SEED (use LAYOUT_RESIZE_SEED=$SEED to reproduce)"

exit "$FAILURES"

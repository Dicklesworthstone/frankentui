#!/bin/bash
set -euo pipefail

# E2E tests for Terminal Capability Explorer (Demo Showcase)
# bd-3b13l: Terminal capabilities + inline/alt verification
#
# Scenarios:
# - Run terminal capabilities flow in alt + inline at 80x24 and 120x40.
# - Export JSONL capability report and log raw values + derived metrics.
# - Emit step start/end events with duration and stable hashes.
#
# Keybindings used:
# - j: Select capability
# - E: Export JSONL capability report

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

E2E_JSONL_FILE="${E2E_RESULTS_DIR:-/tmp/ftui_e2e_results}/terminal_capabilities.jsonl"
E2E_RUN_CMD="${E2E_RUN_CMD:-tests/e2e/scripts/test_terminal_capabilities.sh}"

# Initialize deterministic fixtures + JSONL baseline
E2E_DETERMINISTIC="${E2E_DETERMINISTIC:-1}"
e2e_fixture_init "terminal_caps"
jsonl_init

RUN_ID="${E2E_RUN_ID}"
SEED="${E2E_SEED}"

CAPS_SEND_SEQUENCE="jE"
INLINE_UI_HEIGHT="${FTUI_DEMO_UI_HEIGHT:-12}"

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

detect_caps_screen() {
    local bin="$1"
    local help
    help="$($bin --help 2>/dev/null || true)"
    if [[ -z "$help" ]]; then
        return 1
    fi
    local line
    line=$(printf '%s\n' "$help" | command grep "Terminal Caps" | head -n 1 || true)
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

caps_report_load() {
    local report_file="$1"
    CAPS_REPORT_LINE=""
    CAPS_CAPABILITIES_JSON="null"
    CAPS_METRICS_JSON="null"
    CAPS_DETECTED_PROFILE=""
    CAPS_SIMULATED_PROFILE=""
    CAPS_SIMULATION_ACTIVE="false"

    if [[ -f "$report_file" ]]; then
        CAPS_REPORT_LINE="$(tail -n 1 "$report_file" 2>/dev/null || true)"
    fi

    if [[ -n "$CAPS_REPORT_LINE" && $(command -v jq >/dev/null 2>&1; echo $?) -eq 0 ]]; then
        CAPS_CAPABILITIES_JSON="$(jq -c '.capabilities // []' <<<"$CAPS_REPORT_LINE" 2>/dev/null || echo 'null')"
        CAPS_METRICS_JSON="$(jq -c '{total:(.capabilities|length),enabled:(.capabilities|map(select(.effective==true))|length),disabled:(.capabilities|map(select(.effective==false))|length),fallbacks:(.capabilities|map(select(.fallback != null and .fallback != ""))|length)}' <<<"$CAPS_REPORT_LINE" 2>/dev/null || echo 'null')"
        CAPS_DETECTED_PROFILE="$(jq -r '.detected_profile // ""' <<<"$CAPS_REPORT_LINE" 2>/dev/null || echo "")"
        CAPS_SIMULATED_PROFILE="$(jq -r '.simulated_profile // ""' <<<"$CAPS_REPORT_LINE" 2>/dev/null || echo "")"
        CAPS_SIMULATION_ACTIVE="$(jq -r '.simulation_active // false' <<<"$CAPS_REPORT_LINE" 2>/dev/null || echo "false")"
    fi
}

emit_caps_case_end() {
    local name="$1"
    local status="$2"
    local duration_ms="$3"
    local mode="$4"
    local cols="$5"
    local rows="$6"
    local output_file="$7"
    local report_file="$8"
    local canonical_file="${9:-}"

    local ts
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi
    local hash_key
    hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "${E2E_SEED:-0}")"

    local output_sha=""
    local output_bytes=0
    local stable_output_sha=""
    local stable_output_file="$output_file"
    if [[ -n "$canonical_file" && -f "$canonical_file" ]]; then
        stable_output_file="$canonical_file"
    fi
    if output_sha=$(sha256_file "$output_file" 2>/dev/null); then
        output_bytes=$(wc -c < "$output_file" 2>/dev/null | tr -d ' ')
    fi
    if stable_output_sha=$(sha256_file "$stable_output_file" 2>/dev/null); then
        :
    else
        stable_output_sha="$output_sha"
    fi

    local report_sha=""
    local report_bytes=0
    if report_sha=$(sha256_file "$report_file" 2>/dev/null); then
        report_bytes=$(wc -c < "$report_file" 2>/dev/null | tr -d ' ')
        # Treat exported report hash as the deterministic checksum source.
        stable_output_sha="$report_sha"
        stable_output_file="$report_file"
    fi

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "case_step_end" \
            --arg timestamp "$ts" \
            --arg run_id "$RUN_ID" \
            --arg case "$name" \
            --arg step "terminal_caps_flow" \
            --arg status "$status" \
            --argjson duration_ms "$duration_ms" \
            --arg action "pty_run" \
            --arg details "screen=$CAPS_SCREEN" \
            --arg mode "$mode" \
            --arg hash_key "$hash_key" \
            --argjson cols "$cols" \
            --argjson rows "$rows" \
            --argjson seed "$seed_json" \
            --arg output_file "$output_file" \
            --arg output_sha256 "$output_sha" \
            --arg stable_output_file "$stable_output_file" \
            --arg stable_output_sha256 "$stable_output_sha" \
            --argjson output_bytes "${output_bytes:-0}" \
            --arg report_file "$report_file" \
            --arg report_sha256 "$report_sha" \
            --argjson report_bytes "${report_bytes:-0}" \
            --arg detected_profile "$CAPS_DETECTED_PROFILE" \
            --arg simulated_profile "$CAPS_SIMULATED_PROFILE" \
            --argjson simulation_active "$CAPS_SIMULATION_ACTIVE" \
            --argjson capabilities "$CAPS_CAPABILITIES_JSON" \
            --argjson metrics "$CAPS_METRICS_JSON" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,case:$case,step:$step,status:$status,duration_ms:$duration_ms,action:$action,details:$details,mode:$mode,hash_key:$hash_key,cols:$cols,rows:$rows,output_file:$output_file,output_sha256:$output_sha256,stable_output_file:$stable_output_file,stable_output_sha256:$stable_output_sha256,output_bytes:$output_bytes,report_file:$report_file,report_sha256:$report_sha256,report_bytes:$report_bytes,detected_profile:$detected_profile,simulated_profile:$simulated_profile,simulation_active:$simulation_active,capabilities:$capabilities,metrics:$metrics}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"case_step_end\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$RUN_ID")\",\"seed\":${seed_json},\"case\":\"$(json_escape "$name")\",\"step\":\"terminal_caps_flow\",\"status\":\"$(json_escape "$status")\",\"duration_ms\":${duration_ms},\"action\":\"pty_run\",\"details\":\"screen=$CAPS_SCREEN\",\"mode\":\"$(json_escape "$mode")\",\"hash_key\":\"$(json_escape "$hash_key")\",\"cols\":${cols},\"rows\":${rows},\"output_file\":\"$(json_escape "$output_file")\",\"report_file\":\"$(json_escape "$report_file")\"}"
    fi
}

run_caps_case() {
    local name="$1"
    local mode="$2"
    local cols="$3"
    local rows="$4"

    LOG_FILE="$E2E_LOG_DIR/${name}_${RUN_ID}.log"
    local output_file="$E2E_LOG_DIR/${name}_${RUN_ID}.pty"
    local report_file="$E2E_LOG_DIR/${name}_${RUN_ID}_report.jsonl"

    export E2E_CONTEXT_MODE="$mode"
    export E2E_CONTEXT_COLS="$cols"
    export E2E_CONTEXT_ROWS="$rows"
    export E2E_CONTEXT_SEED="${E2E_SEED:-0}"

    log_test_start "$name"
    jsonl_case_step_start "$name" "terminal_caps_flow" "pty_run" "screen=$CAPS_SCREEN"

    local start_ms end_ms duration_ms
    start_ms="$(e2e_now_ms)"

    local exit_code=0
    export FTUI_DEMO_SCREEN_MODE="$mode"
    if [[ "$mode" == "inline" ]]; then
        export FTUI_DEMO_UI_HEIGHT="$INLINE_UI_HEIGHT"
    else
        unset FTUI_DEMO_UI_HEIGHT
    fi

    if PTY_COLS="$cols" \
        PTY_ROWS="$rows" \
        PTY_SEND_DELAY_MS=400 \
        PTY_SEND="$CAPS_SEND_SEQUENCE" \
        PTY_TIMEOUT=6 \
        PTY_CANONICALIZE=1 \
        FTUI_DEMO_EXIT_AFTER_MS=2000 \
        FTUI_TERMCAPS_DIAGNOSTICS=true \
        FTUI_TERMCAPS_DETERMINISTIC=true \
        FTUI_TERMCAPS_REPORT_PATH="$report_file" \
        pty_run "$output_file" "$DEMO_BIN"; then
        exit_code=0
    else
        exit_code=$?
    fi

    end_ms="$(e2e_now_ms)"
    duration_ms=$((end_ms - start_ms))

    local status="passed"
    if [[ "$exit_code" -ne 0 ]]; then
        status="failed"
    fi

    local size=0
    if [[ -f "$output_file" ]]; then
        size=$(wc -c < "$output_file" | tr -d ' ')
    fi

    if [[ "$size" -lt 200 ]]; then
        status="failed"
    fi

    if [[ ! -f "$report_file" ]]; then
        status="failed"
    fi

    if [[ "$status" == "passed" ]]; then
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
    else
        log_test_fail "$name" "assertion failed"
        record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    fi

    local canonical_file="${PTY_CANONICAL_FILE:-}"
    caps_report_load "$report_file"
    jsonl_pty_capture "$output_file" "$cols" "$rows" "$exit_code" "$canonical_file"
    emit_caps_case_end "$name" "$status" "$duration_ms" "$mode" "$cols" "$rows" "$output_file" "$report_file" "$canonical_file"

    if [[ "$status" != "passed" ]]; then
        return 1
    fi
    return 0
}

DEMO_BIN="$(ensure_demo_bin || true)"
if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/terminal_caps_missing.log"
    caps_report_load ""
    for t in caps_alt_80x24 caps_alt_120x40 caps_inline_80x24 caps_inline_120x40; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        emit_caps_case_end "$t" "skipped" 0 "unknown" 0 0 "" ""
    done
    exit 0
fi

CAPS_SCREEN="$(detect_caps_screen "$DEMO_BIN" || true)"
if [[ -z "$CAPS_SCREEN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/terminal_caps_missing.log"
    caps_report_load ""
    for t in caps_alt_80x24 caps_alt_120x40 caps_inline_80x24 caps_inline_120x40; do
        log_test_skip "$t" "Terminal Capabilities screen not registered in --help"
        record_result "$t" "skipped" 0 "$LOG_FILE" "screen missing"
        emit_caps_case_end "$t" "skipped" 0 "unknown" 0 0 "" ""
    done
    exit 0
fi

export FTUI_DEMO_SCREEN="$CAPS_SCREEN"

FAILURES=0
run_caps_case "caps_alt_80x24" "alt" 80 24 || FAILURES=$((FAILURES + 1))
run_caps_case "caps_alt_120x40" "alt" 120 40 || FAILURES=$((FAILURES + 1))
run_caps_case "caps_inline_80x24" "inline" 80 24 || FAILURES=$((FAILURES + 1))
run_caps_case "caps_inline_120x40" "inline" 120 40 || FAILURES=$((FAILURES + 1))

if [[ "$FAILURES" -gt 0 ]]; then
    log_error "Terminal Capabilities E2E tests: $FAILURES failure(s)"
    exit 1
fi

log_info "Terminal Capabilities E2E tests: all passed"
exit 0

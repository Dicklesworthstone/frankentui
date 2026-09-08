#!/bin/bash
set -euo pipefail

# E2E: Bidi + VOI overlay integration sweep (bd-3dh8m)
#
# Coverage:
# - Scenarios: bidi (i18n Stress Lab), voi (VOI Overlay)
# - Modes: alt + inline
# - Sizes: 80x24, 120x40
# - Deterministic seeds/time
#
# JSONL emits per-case entries with:
# schema_version, scenario, mode, dims, hash, timing, status, error

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

export E2E_DETERMINISTIC="${E2E_DETERMINISTIC:-1}"
export E2E_TIME_STEP_MS="${E2E_TIME_STEP_MS:-100}"
export E2E_SEED="${E2E_SEED:-0}"

e2e_fixture_init "bidi_voi" "$E2E_SEED" "$E2E_TIME_STEP_MS"

E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_logs}"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
LOG_FILE="${LOG_FILE:-$E2E_LOG_DIR/bidi_voi_overlay.log}"
E2E_JSONL_FILE="${E2E_JSONL_FILE:-$E2E_LOG_DIR/e2e.jsonl}"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
export E2E_LOG_DIR E2E_RESULTS_DIR LOG_FILE E2E_JSONL_FILE E2E_RUN_CMD
export E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"

INLINE_UI_HEIGHT="${BIDI_VOI_UI_HEIGHT:-18}"

mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
jsonl_init
jsonl_assert "artifact_log_dir" "pass" "log_dir=$E2E_LOG_DIR"

if [[ -z "$E2E_PYTHON" ]]; then
    log_error "python3/python is required for PTY helpers"
    exit 1
fi

ensure_demo_bin() {
    if [[ -n "${E2E_DEMO_BIN:-}" ]]; then
        [[ -x "$E2E_DEMO_BIN" ]] || return 1
        printf '%s\n' "$E2E_DEMO_BIN"
        return 0
    fi
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

sha256_file() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1 && [[ -f "$file" ]]; then
        sha256sum "$file" | awk '{print $1}'
        return 0
    fi
    echo ""
    return 0
}

emit_case_jsonl() {
    local scenario="$1"
    local mode="$2"
    local cols="$3"
    local rows="$4"
    local status="$5"
    local hash="$6"
    local duration_ms="$7"
    local error="$8"
    local screen="$9"

    local ts
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi
    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "case" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg scenario "$scenario" \
            --arg mode "$mode" \
            --arg status "$status" \
            --arg hash "$hash" \
            --arg error "$error" \
            --arg screen "$screen" \
            --argjson cols "$cols" \
            --argjson rows "$rows" \
            --argjson duration_ms "$duration_ms" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,scenario:$scenario,mode:$mode,cols:$cols,rows:$rows,status:$status,hash:$hash,duration_ms:$duration_ms,error:$error,screen:$screen}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"case\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"scenario\":\"$(json_escape "$scenario")\",\"mode\":\"$(json_escape "$mode")\",\"cols\":${cols},\"rows\":${rows},\"status\":\"$(json_escape "$status")\",\"hash\":\"$(json_escape "$hash")\",\"duration_ms\":${duration_ms},\"error\":\"$(json_escape "$error")\",\"screen\":\"$(json_escape "$screen")\"}"
    fi
}

run_case() {
    local scenario="$1"
    local screen="$2"
    local mode="$3"
    local cols="$4"
    local rows="$5"
    local send_data="$6"
    local send_delay_ms="$7"
    local case_id="${scenario}_${mode}_${cols}x${rows}"
    local start_ms end_ms duration_ms

    LOG_FILE="$E2E_LOG_DIR/${case_id}.log"
    local output_file="$E2E_LOG_DIR/${case_id}.pty"

    log_test_start "$case_id"
    start_ms="$(e2e_now_ms)"

    local ui_height=""
    if [[ "$mode" == "inline" ]]; then
        ui_height="${FTUI_DEMO_UI_HEIGHT:-$INLINE_UI_HEIGHT}"
    fi

    local status="passed"
    local error=""
    local exit_code=0
    PTY_COLS="$cols" \
    PTY_ROWS="$rows" \
    PTY_SEND="$send_data" \
    PTY_SEND_DELAY_MS="$send_delay_ms" \
    PTY_TIMEOUT=6 \
    FTUI_DEMO_DETERMINISTIC=1 \
    FTUI_DEMO_SEED="$E2E_SEED" \
    FTUI_DEMO_TICK_MS="$E2E_TIME_STEP_MS" \
    FTUI_DEMO_SCREEN_MODE="$mode" \
    FTUI_DEMO_UI_HEIGHT="$ui_height" \
    FTUI_DEMO_SCREEN="$screen" \
    FTUI_DEMO_EXIT_AFTER_MS=1600 \
        pty_run "$output_file" "$DEMO_BIN" || exit_code=$?

    if [[ "$exit_code" -ne 0 ]]; then
        status="failed"
        error="pty_exit_${exit_code}"
    fi

    local size=0
    if [[ -f "$output_file" ]]; then
        size=$(wc -c < "$output_file" | tr -d ' ')
    fi

    if [[ "$status" == "passed" && "$size" -lt 200 ]]; then
        status="failed"
        error="output_too_small"
    fi

    if [[ "$status" == "passed" ]]; then
        if [[ "$scenario" == "bidi" ]]; then
            if ! command grep -a -q "Locale: Arabic" "$output_file"; then
                status="failed"
                error="rtl_locale_not_selected"
            fi
        else
            if ! command grep -a -q "VOI" "$output_file"; then
                status="failed"
                error="voi_marker_missing"
            fi
        fi
    fi

    end_ms="$(e2e_now_ms)"
    duration_ms=$((end_ms - start_ms))
    local hash
    hash="$(sha256_file "$output_file")"

    if [[ "$status" == "passed" ]]; then
        log_test_pass "$case_id"
        record_result "$case_id" "passed" "$duration_ms" "$LOG_FILE"
    else
        log_test_fail "$case_id" "$error"
        record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "$error"
    fi

    emit_case_jsonl "$scenario" "$mode" "$cols" "$rows" "$status" "$hash" "$duration_ms" "$error" "$screen"
    jsonl_assert "$case_id" "$status" "scenario=${scenario} mode=${mode} cols=${cols} rows=${rows} hash=${hash} duration_ms=${duration_ms} error=${error} screen=${screen}"

    if [[ "$status" == "passed" ]]; then
        jsonl_step_end "$case_id" "success" "$duration_ms"
        return 0
    fi
    jsonl_step_end "$case_id" "failed" "$duration_ms"
    return 1
}

DEMO_BIN="$(ensure_demo_bin || true)"
if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/bidi_voi_missing.log"
    for t in bidi_alt_80x24 bidi_inline_80x24 bidi_alt_120x40 bidi_inline_120x40 \
             voi_alt_80x24 voi_inline_80x24 voi_alt_120x40 voi_inline_120x40; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        emit_case_jsonl "${t%%_*}" "unknown" 0 0 "skipped" "" 0 "binary missing" ""
    done
    exit 0
fi

DEMO_HELP="$("$DEMO_BIN" --help)"
BIDI_SCREEN="$(printf '%s\n' "$DEMO_HELP" | awk '$1 ~ /^[0-9]+$/ && $2 == "i18n" && $3 == "Stress" && $4 == "Lab" {print $1; exit}')"
VOI_SCREEN="$(printf '%s\n' "$DEMO_HELP" | awk '$1 ~ /^[0-9]+$/ && $2 == "VOI" && $3 == "Overlay" {print $1; exit}')"
if [[ ! "$BIDI_SCREEN" =~ ^[0-9]+$ || ! "$VOI_SCREEN" =~ ^[0-9]+$ ]]; then
    log_error "i18n Stress Lab or VOI Overlay screen not registered in --help"
    exit 1
fi

RIGHT=$'\x1b[C'
BIDI_SEND="${RIGHT}${RIGHT}${RIGHT}${RIGHT}"

overall_failures=0
modes=("alt" "inline")
sizes=("80x24" "120x40")

for mode in "${modes[@]}"; do
    for size in "${sizes[@]}"; do
        cols="${size%x*}"
        rows="${size#*x}"
        if ! run_case "bidi" "$BIDI_SCREEN" "$mode" "$cols" "$rows" "$BIDI_SEND" 300; then
            overall_failures=$((overall_failures + 1))
        fi
        if ! run_case "voi" "$VOI_SCREEN" "$mode" "$cols" "$rows" "" 0; then
            overall_failures=$((overall_failures + 1))
        fi
    done
done

if [[ "$overall_failures" -gt 0 ]]; then
    exit 1
fi

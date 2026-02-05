#!/usr/bin/env bash
# Demo Showcase Screen Sweep E2E (bd-34m9w)
#
# Runs every demo screen via FTUI_DEMO_SCREEN across alt + inline modes
# and standard sizes. Emits JSONL logs with per-screen PTY hashes + timing.
#
# Usage:
#   ./scripts/demo_showcase_screen_sweep_e2e.sh [--verbose] [--quick] [--large] [--no-large]
#
# Environment:
#   LOG_DIR               Output directory (default: /tmp/ftui_demo_sweep_<run>)
#   SWEEP_LARGE=1         Include 200x50 size in the sweep (default: 1)
#   EXIT_AFTER_TICKS=8    Auto-exit tick count per screen
#   TICK_MS=100           Demo tick cadence in ms
#   INLINE_UI_HEIGHT=12   Inline mode UI height

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

VERBOSE=false
QUICK=false
INCLUDE_LARGE="${SWEEP_LARGE:-1}"

for arg in "$@"; do
    case "$arg" in
        --verbose|-v) VERBOSE=true ;;
        --quick|-q) QUICK=true ;;
        --large) INCLUDE_LARGE=1 ;;
        --no-large) INCLUDE_LARGE=0 ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--quick] [--large] [--no-large]"
            echo "  --verbose  Show full output"
            echo "  --quick    Skip build step"
            echo "  --large    Include 200x50 size sweep (default)"
            echo "  --no-large Skip 200x50 size sweep"
            exit 0
            ;;
    esac
done

log_info() {
    echo -e "\033[1;34m[INFO]\033[0m $*"
}

log_pass() {
    echo -e "\033[1;32m[PASS]\033[0m $*"
}

log_fail() {
    echo -e "\033[1;31m[FAIL]\033[0m $*"
}

e2e_fixture_init "demo_sweep"
TIMESTAMP="$(e2e_log_stamp)"
LOG_DIR="${LOG_DIR:-/tmp/ftui_demo_sweep_${E2E_RUN_ID}_${TIMESTAMP}}"
E2E_LOG_DIR="$LOG_DIR"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$LOG_DIR/results}"
E2E_JSONL_FILE="$LOG_DIR/demo_showcase_screen_sweep.jsonl"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"
export E2E_LOG_DIR E2E_RESULTS_DIR E2E_JSONL_FILE E2E_RUN_CMD E2E_RUN_START_MS
mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
jsonl_init
jsonl_set_context "" "" "" "${E2E_SEED:-0}"

TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
DEMO_BIN="$TARGET_DIR/debug/ftui-demo-showcase"

if ! $QUICK; then
    log_info "Building ftui-demo-showcase..."
    if $VERBOSE; then
        cargo build -p ftui-demo-showcase 2>&1 | tee "$LOG_DIR/build.log"
    else
        cargo build -p ftui-demo-showcase > "$LOG_DIR/build.log" 2>&1
    fi
fi

if [[ ! -x "$DEMO_BIN" ]]; then
    log_fail "Demo binary not found at $DEMO_BIN"
    jsonl_run_end "failed" "$(( $(e2e_now_ms) - E2E_RUN_START_MS ))" 1
    exit 1
fi

EXIT_AFTER_TICKS="${EXIT_AFTER_TICKS:-8}"
TICK_MS="${TICK_MS:-100}"
INLINE_UI_HEIGHT="${INLINE_UI_HEIGHT:-12}"

SIZES=("80x24" "120x40")
if [[ "$INCLUDE_LARGE" == "1" ]]; then
    SIZES+=("200x50")
fi

SCREENS=(
    "1|guided_tour|Guided Tour"
    "2|dashboard|Dashboard"
    "3|shakespeare|Shakespeare"
    "4|code_explorer|Code Explorer"
    "5|widget_gallery|Widget Gallery"
    "6|layout_lab|Layout Lab"
    "7|forms_input|Forms & Input"
    "8|data_viz|Data Viz"
    "9|file_browser|File Browser"
    "10|advanced|Advanced"
    "11|table_themes|Table Themes"
    "12|terminal_caps|Terminal Caps"
    "13|macro_recorder|Macro Recorder"
    "14|performance|Performance"
    "15|markdown|Markdown"
    "16|visual_effects|Visual Effects"
    "17|responsive|Responsive"
    "18|log_search|Log Search"
    "19|notifications|Notifications"
    "20|action_timeline|Action Timeline"
    "21|sizing|Sizing"
    "22|layout_inspector|Layout Inspector"
    "23|text_editor|Text Editor"
    "24|mouse_playground|Mouse Playground"
    "25|form_validation|Form Validation"
    "26|virtualized_search|Virtualized Search"
    "27|async_tasks|Async Tasks"
    "28|theme_studio|Theme Studio"
    "29|time_travel_studio|Time-Travel Studio"
    "30|performance_hud|Performance Challenge"
    "31|i18n_stress_lab|i18n Stress Lab"
    "32|voi_overlay|VOI Overlay"
    "33|inline_mode|Inline Mode"
    "34|accessibility|Accessibility"
    "35|widget_builder|Widget Builder"
    "36|palette_evidence|Palette Evidence"
    "37|determinism_lab|Determinism Lab"
    "38|links|Links"
)

PASSED=0
FAILED=0

run_screen_case() {
    local screen_id="$1"
    local screen_slug="$2"
    local screen_label="$3"
    local mode="$4"
    local cols="$5"
    local rows="$6"

    local case_id="demo_${screen_id}_${screen_slug}_${mode}_${cols}x${rows}"
    local case_dir="$LOG_DIR/screens"
    mkdir -p "$case_dir"

    local out_pty="$case_dir/${case_id}.pty"
    local run_log="$case_dir/${case_id}.log"

    jsonl_set_context "$mode" "$cols" "$rows" "${E2E_SEED:-0}"
    jsonl_case_step_start "$case_id" "run" "launch" "screen=${screen_id} label=${screen_label}"

    local start_ms
    start_ms="$(e2e_now_ms)"

    local exit_code=0
    if FTUI_DEMO_DETERMINISTIC=1 \
        FTUI_DEMO_SEED="${E2E_SEED:-0}" \
        FTUI_DEMO_RUN_ID="${E2E_RUN_ID}_${case_id}" \
        FTUI_DEMO_SCREEN="$screen_id" \
        FTUI_DEMO_SCREEN_MODE="$mode" \
        FTUI_DEMO_UI_HEIGHT="$INLINE_UI_HEIGHT" \
        FTUI_DEMO_TICK_MS="$TICK_MS" \
        FTUI_DEMO_EXIT_AFTER_TICKS="$EXIT_AFTER_TICKS" \
        PTY_COLS="$cols" \
        PTY_ROWS="$rows" \
        PTY_TIMEOUT=10 \
        PTY_TEST_NAME="$case_id" \
        pty_run "$out_pty" "$DEMO_BIN" > "$run_log" 2>&1; then
        exit_code=0
    else
        exit_code=$?
    fi

    local end_ms
    end_ms="$(e2e_now_ms)"
    local duration_ms=$((end_ms - start_ms))

    pty_record_metadata "$out_pty" "$exit_code" "$cols" "$rows"
    jsonl_artifact "pty_output" "$out_pty" "present"

    local status="pass"
    if [[ "$exit_code" -ne 0 ]]; then
        status="fail"
    fi
    if [[ ! -s "$out_pty" ]]; then
        status="fail"
        jsonl_assert "pty_output_${case_id}" "failed" "missing $out_pty"
    else
        jsonl_assert "pty_output_${case_id}" "pass" "ok"
    fi

    local seed_val="${E2E_SEED:-0}"
    local hash_key
    hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed_val")"
    local hash=""
    hash="$(sha256_file "$out_pty" 2>/dev/null || true)"
    if [[ -z "$hash" ]]; then
        hash="missing"
    fi
    jsonl_assert "screen_sweep_${case_id}" "$status" \
        "screen_id=${screen_id} screen=${screen_slug} label=${screen_label} mode=${mode} cols=${cols} rows=${rows} seed=${seed_val} hash_key=${hash_key} hash=${hash} exit=${exit_code} duration_ms=${duration_ms}"

    local seed_val="${E2E_SEED:-0}"
    local hash_key
    hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed_val")"
    local hash=""
    hash="$(sha256_file "$out_pty" 2>/dev/null || true)"
    if [[ -z "$hash" ]]; then
        hash="missing"
    fi

    jsonl_case_step_end "$case_id" "run" "$status" "$duration_ms" "launch" \
        "screen=${screen_id} label=${screen_label} output=${out_pty} hash_key=${hash_key} hash=${hash}"

    jsonl_assert "screen_sweep_${case_id}" "$status" \
        "screen_id=${screen_id} screen=${screen_slug} label=${screen_label} mode=${mode} cols=${cols} rows=${rows} seed=${seed_val} hash_key=${hash_key} hash=${hash} exit=${exit_code} duration_ms=${duration_ms}"

    if [[ "$status" == "pass" ]]; then
        PASSED=$((PASSED + 1))
        log_pass "$case_id (${duration_ms}ms)"
        return 0
    fi

    FAILED=$((FAILED + 1))
    log_fail "$case_id (${duration_ms}ms, exit=$exit_code)"
    log_fail "  Log: $run_log"
    return 1
}

log_info "Demo Showcase Screen Sweep (bd-34m9w)"
log_info "Log directory: $LOG_DIR"
log_info "Sizes: ${SIZES[*]}"
log_info "Modes: alt inline"
log_info "Exit after ticks: $EXIT_AFTER_TICKS (tick_ms=$TICK_MS)"

SUITE_FAILURES=0
for mode in alt inline; do
    for size in "${SIZES[@]}"; do
        cols="${size%x*}"
        rows="${size#*x}"
        for entry in "${SCREENS[@]}"; do
            IFS="|" read -r screen_id screen_slug screen_label <<< "$entry"
            if ! run_screen_case "$screen_id" "$screen_slug" "$screen_label" "$mode" "$cols" "$rows"; then
                SUITE_FAILURES=$((SUITE_FAILURES + 1))
            fi
        done
    done
done

total=$((PASSED + FAILED))
log_info "Sweep complete: total=$total pass=$PASSED fail=$FAILED"

run_duration_ms=$(( $(e2e_now_ms) - E2E_RUN_START_MS ))
if [[ "$FAILED" -gt 0 ]]; then
    jsonl_run_end "failed" "$run_duration_ms" "$FAILED"
    exit 1
fi
jsonl_run_end "success" "$run_duration_ms" 0

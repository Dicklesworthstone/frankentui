#!/bin/bash
# FrankenTermJS Runtime Option / Theming Mutation Cross-Engine E2E Matrix (bd-2vr05.13.5)
#
# Drives the production runtime option API (ftui-web runtime_options: atomic
# validation, capability gating, theming) across a matrix of simulated rendering
# engines, emitting structured JSONL evidence per cell.
#
# Usage:
#   ./scripts/frankenterm_js_runtime_options_e2e.sh [--verbose] [--quick]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"
PRESET_E2E_JSONL_FILE="${E2E_JSONL_FILE:-}"

# shellcheck source=/dev/null
if [[ -f "$LIB_DIR/common.sh" ]]; then
    source "$LIB_DIR/common.sh"
fi
if [[ -f "$LIB_DIR/logging.sh" ]]; then
    source "$LIB_DIR/logging.sh"
fi
if ! declare -f e2e_timestamp >/dev/null 2>&1; then
    e2e_timestamp() { date -Iseconds; }
fi
if ! declare -f e2e_log_stamp >/dev/null 2>&1; then
    e2e_log_stamp() { date +%Y%m%d_%H%M%S; }
fi
if ! declare -f e2e_now_ms >/dev/null 2>&1; then
    e2e_now_ms() { date +%s%3N; }
fi

VERBOSE=false
QUICK=false

for arg in "$@"; do
    case "$arg" in
        --verbose|-v) VERBOSE=true ;;
        --quick)      QUICK=true ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--quick]"
            echo "  --verbose  Show full output"
            echo "  --quick    Skip compilation, run tests only"
            exit 0
            ;;
    esac
done

e2e_fixture_init "frankenterm_js_runtime_options"
TIMESTAMP="$(e2e_log_stamp)"
LOG_DIR="${LOG_DIR:-/tmp/ftui-frankenterm-js-runtime-options-${E2E_RUN_ID}-${TIMESTAMP}}"
E2E_LOG_DIR="$LOG_DIR"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$LOG_DIR/results}"
E2E_JSONL_FILE="${FRANKENTERM_JS_RUNTIME_OPTIONS_JSONL_FILE:-${PRESET_E2E_JSONL_FILE:-$LOG_DIR/frankenterm_js_runtime_options_e2e.jsonl}}"
E2E_CHILD_JSONL_DIR="${E2E_CHILD_JSONL_DIR:-$LOG_DIR/child-jsonl}"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"
export E2E_LOG_DIR E2E_RESULTS_DIR E2E_JSONL_FILE E2E_RUN_CMD E2E_RUN_START_MS
mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR" "$E2E_CHILD_JSONL_DIR"
jsonl_init
jsonl_assert "artifact_log_dir" "pass" "log_dir=$E2E_LOG_DIR"
jsonl_assert "child_jsonl_dir" "pass" "child_jsonl_dir=$E2E_CHILD_JSONL_DIR"
jsonl_set_context "host" "${COLUMNS:-}" "${LINES:-}" "${E2E_SEED:-0}"

PASSED=0
FAILED=0
SKIPPED=0

run_step() {
    local name="$1"
    shift
    local step_start
    step_start=$(e2e_now_ms)

    jsonl_step_start "$name"

    local exit_code=0
    local output_file="$LOG_DIR/${name}.log"

    if $VERBOSE; then
        "$@" 2>&1 | tee "$output_file" || exit_code=$?
    else
        "$@" > "$output_file" 2>&1 || exit_code=$?
    fi

    local step_end
    step_end=$(e2e_now_ms)
    local elapsed=$(( step_end - step_start ))

    if [ "$exit_code" -eq 0 ]; then
        PASSED=$((PASSED + 1))
        jsonl_step_end "$name" "success" "$elapsed"
        printf "  %-50s  PASS  (%s ms)\n" "$name" "$elapsed"
    else
        FAILED=$((FAILED + 1))
        jsonl_step_end "$name" "failed" "$elapsed"
        printf "  %-50s  FAIL  (exit %s, %s ms)\n" "$name" "$exit_code" "$elapsed"
        echo "    Log: $output_file"
    fi
}

skip_step() {
    local name="$1"
    SKIPPED=$((SKIPPED + 1))
    jsonl_step_start "$name"
    jsonl_step_end "$name" "skipped" 0
    printf "  %-50s  SKIP\n" "$name"
}

echo "=========================================="
echo " FrankenTermJS Runtime Options Cross-Engine Matrix (bd-2vr05.13.5)"
echo "=========================================="
echo ""
echo "  Log directory: $LOG_DIR"
echo ""

FEAT="--features input-parser"

if ! $QUICK; then
    run_step "cargo_check" \
        cargo check -p ftui-web $FEAT --test frankenterm_js_runtime_options_e2e --quiet
    run_step "cargo_clippy" \
        cargo clippy -p ftui-web $FEAT --test frankenterm_js_runtime_options_e2e -- -D warnings
else
    skip_step "cargo_check"
    skip_step "cargo_clippy"
fi

# Capture the structured FTUI_RUNTIME_OPTIONS_MATRIX evidence lines into a JSONL artifact.
MATRIX_LOG="$E2E_CHILD_JSONL_DIR/frankenterm_js_runtime_options_matrix.jsonl"
run_step "runtime_options_matrix_tests" bash -c "
    cd '$PROJECT_ROOT' &&
    cargo test -p ftui-web $FEAT --test frankenterm_js_runtime_options_e2e -- --nocapture \
        | tee '$LOG_DIR/runtime_options_matrix.raw.log' \
        | (grep '^FTUI_RUNTIME_OPTIONS_MATRIX ' || true) \
        | sed 's/^FTUI_RUNTIME_OPTIONS_MATRIX //' > '$MATRIX_LOG'
    test -s '$MATRIX_LOG'
"

if [[ -s "$MATRIX_LOG" ]]; then
    MATRIX_CELLS=$(wc -l < "$MATRIX_LOG" | tr -d ' ')
    jsonl_assert "matrix_evidence_lines" "pass" "cells=$MATRIX_CELLS file=$MATRIX_LOG"
    echo "  Captured $MATRIX_CELLS matrix evidence cells -> $MATRIX_LOG"
fi

echo ""
echo "=========================================="
TOTAL=$((PASSED + FAILED + SKIPPED))
echo "  Total: $TOTAL  Passed: $PASSED  Failed: $FAILED  Skipped: $SKIPPED"
echo "=========================================="
echo ""

run_status="success"
if [ "$FAILED" -ne 0 ]; then
    run_status="failed"
fi
jsonl_run_end "$run_status" "$(( $(e2e_now_ms) - ${E2E_RUN_START_MS:-$(e2e_now_ms)} ))" "$FAILED"

[[ $FAILED -eq 0 ]]

#!/usr/bin/env bash
# End-to-End Verification for the FrankenTUI Performance Regression Gate (G25)
#
# Phases:
#   1. Clean pass against tests/baseline.json + JSONL schema validation
#   2. Injected regression failure (present_80x24_sparse.p99_ns = 1 in --check-only mode)
#   3. README & baseline markdown provenance verification (render_perf_readme.py --check)
#      plus 13-case synthetic decision matrix (check_perf_gate_decisions_e2e.sh)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"

export E2E_DETERMINISTIC="${E2E_DETERMINISTIC:-1}"
export E2E_SEED="${E2E_SEED:-0}"

WORK_DIR="${1:-/tmp/ftui_e2e_perf_gate_$(date +%Y%m%d_%H%M%S)}"
if [[ "$WORK_DIR" == "--full" ]]; then
    WORK_DIR="/tmp/ftui_e2e_perf_gate_$(date +%Y%m%d_%H%M%S)"
fi
export E2E_LOG_DIR="$WORK_DIR/logs"
export E2E_RESULTS_DIR="$WORK_DIR/results"
export E2E_JSONL_FILE="$E2E_LOG_DIR/perf_gate_e2e.jsonl"
mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"

e2e_fixture_init "perf_gate" "$E2E_SEED"
jsonl_init

RUN_FULL=false
if [[ "${1:-}" == "--full" ]] || [[ "${FTUI_PERF_E2E_FULL:-0}" == "1" ]]; then
    RUN_FULL=true
fi

SCHEMA_FILE="$PROJECT_ROOT/tests/perf/regression_report_schema.json"
REPORT_FILE="$PROJECT_ROOT/target/regression-gate/regression_report.jsonl"

# ---------------------------------------------------------------------------
# Phase 1: Clean pass
# ---------------------------------------------------------------------------
START_MS="$(e2e_now_ms)"
log_test_start "perf_gate_clean_pass"

GATE_MODE="--check-only"
if [[ "$RUN_FULL" == "true" ]] || [[ ! -s "$PROJECT_ROOT/target/regression-gate/presenter_bench.txt" ]]; then
    GATE_MODE="--quick"
fi

PHASE1_STDOUT="$WORK_DIR/phase1_stdout.log"
PHASE1_STDERR="$WORK_DIR/phase1_stderr.log"

set +e
(cd "$PROJECT_ROOT" && ./scripts/perf_regression_gate.sh "$GATE_MODE" --json) \
    >"$PHASE1_STDOUT" 2>"$PHASE1_STDERR"
PHASE1_RC=$?
set -e

if [[ "$PHASE1_RC" -ne 0 ]]; then
    DURATION_MS=$(( $(e2e_now_ms) - START_MS ))
    log_test_fail "perf_gate_clean_pass" "expected exit 0, got $PHASE1_RC" "$DURATION_MS"
    record_result "perf_gate_clean_pass" "failed" "$DURATION_MS" "$PHASE1_STDOUT" "exit $PHASE1_RC"
    cat "$PHASE1_STDOUT" "$PHASE1_STDERR" >&2
    exit 1
fi

if ! grep -q "SUMMARY total=27 passed=27 failed=0 skipped=0 incomplete=0" "$PHASE1_STDOUT"; then
    DURATION_MS=$(( $(e2e_now_ms) - START_MS ))
    log_test_fail "perf_gate_clean_pass" "unexpected SUMMARY line" "$DURATION_MS"
    record_result "perf_gate_clean_pass" "failed" "$DURATION_MS" "$PHASE1_STDOUT" "unexpected SUMMARY"
    cat "$PHASE1_STDOUT" >&2
    exit 1
fi

cp "$REPORT_FILE" "$WORK_DIR/phase1_regression_report.jsonl"
python3 "$PROJECT_ROOT/tests/e2e/lib/validate_jsonl.py" \
    "$WORK_DIR/phase1_regression_report.jsonl" \
    --schema "$SCHEMA_FILE"

DURATION_MS=$(( $(e2e_now_ms) - START_MS ))
log_test_pass "perf_gate_clean_pass" "$DURATION_MS"
record_result "perf_gate_clean_pass" "passed" "$DURATION_MS" "$PHASE1_STDOUT"

# ---------------------------------------------------------------------------
# Phase 2: Injected regression failure (present_80x24_sparse.p99_ns = 1)
# ---------------------------------------------------------------------------
START_MS="$(e2e_now_ms)"
log_test_start "perf_gate_injected_regression"

TIGHTENED_BASELINE="$WORK_DIR/baseline_tightened.json"
jq '.present_80x24_sparse.p99_ns = 1' "$PROJECT_ROOT/tests/baseline.json" > "$TIGHTENED_BASELINE"

PHASE2_STDOUT="$WORK_DIR/phase2_stdout.log"
PHASE2_STDERR="$WORK_DIR/phase2_stderr.log"

set +e
(cd "$PROJECT_ROOT" && FTUI_PERF_BASELINE="$TIGHTENED_BASELINE" ./scripts/perf_regression_gate.sh --check-only --json) \
    >"$PHASE2_STDOUT" 2>"$PHASE2_STDERR"
PHASE2_RC=$?
set -e

if [[ "$PHASE2_RC" -ne 1 ]]; then
    DURATION_MS=$(( $(e2e_now_ms) - START_MS ))
    log_test_fail "perf_gate_injected_regression" "expected exit 1 on regression, got $PHASE2_RC" "$DURATION_MS"
    record_result "perf_gate_injected_regression" "failed" "$DURATION_MS" "$PHASE2_STDOUT" "exit $PHASE2_RC"
    cat "$PHASE2_STDOUT" "$PHASE2_STDERR" >&2
    exit 1
fi

if ! grep -q "SUMMARY total=27 passed=26 failed=1 skipped=0 incomplete=0" "$PHASE2_STDOUT"; then
    DURATION_MS=$(( $(e2e_now_ms) - START_MS ))
    log_test_fail "perf_gate_injected_regression" "expected failed=1 incomplete=0 in SUMMARY" "$DURATION_MS"
    record_result "perf_gate_injected_regression" "failed" "$DURATION_MS" "$PHASE2_STDOUT" "bad SUMMARY"
    cat "$PHASE2_STDOUT" >&2
    exit 1
fi

cp "$REPORT_FILE" "$WORK_DIR/phase2_regression_report.jsonl"
# Restore clean report in target/regression-gate/regression_report.jsonl
cp "$WORK_DIR/phase1_regression_report.jsonl" "$REPORT_FILE"

python3 "$PROJECT_ROOT/tests/e2e/lib/validate_jsonl.py" \
    "$WORK_DIR/phase2_regression_report.jsonl" \
    --schema "$SCHEMA_FILE"

if ! jq -e 'select(.event == "row" and .category == "present_80x24_sparse" and .status == "fail" and .reason == "exceeded_threshold")' \
    "$WORK_DIR/phase2_regression_report.jsonl" >/dev/null; then
    DURATION_MS=$(( $(e2e_now_ms) - START_MS ))
    log_test_fail "perf_gate_injected_regression" "missing fail row for present_80x24_sparse" "$DURATION_MS"
    record_result "perf_gate_injected_regression" "failed" "$DURATION_MS" "$WORK_DIR/phase2_regression_report.jsonl" "missing fail row"
    exit 1
fi

DURATION_MS=$(( $(e2e_now_ms) - START_MS ))
log_test_pass "perf_gate_injected_regression" "$DURATION_MS"
record_result "perf_gate_injected_regression" "passed" "$DURATION_MS" "$PHASE2_STDOUT"

# ---------------------------------------------------------------------------
# Phase 3: README & baseline markdown provenance + synthetic decision matrix
# ---------------------------------------------------------------------------
START_MS="$(e2e_now_ms)"
log_test_start "perf_gate_readme_provenance_and_decisions"

PHASE3_LOG="$WORK_DIR/phase3.log"
(
    cd "$PROJECT_ROOT"
    python3 -B scripts/render_perf_readme.py --self-test
    python3 -B scripts/render_perf_readme.py --check
    ./scripts/check_perf_gate_decisions_e2e.sh "$WORK_DIR/decisions"
) >"$PHASE3_LOG" 2>&1

DURATION_MS=$(( $(e2e_now_ms) - START_MS ))
log_test_pass "perf_gate_readme_provenance_and_decisions" "$DURATION_MS"
record_result "perf_gate_readme_provenance_and_decisions" "passed" "$DURATION_MS" "$PHASE3_LOG"

finalize_summary "$E2E_RESULTS_DIR/summary.json"
echo "perf_gate_e2e: ALL PHASES PASSED (artifacts in $WORK_DIR)"

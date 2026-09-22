#!/usr/bin/env bash
set -euo pipefail

# bd-2vr05.14.5 ligature/shaping deterministic verification contract.
#
# Runs focused unit checks, browser-facing remote E2E scenario, and benchmark
# smoke through rch and emits replay-friendly JSONL evidence records.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
E2E_LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"

# shellcheck source=/dev/null
source "$E2E_LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$E2E_LIB_DIR/logging.sh"

require_cmd rch

export E2E_DETERMINISTIC="${E2E_DETERMINISTIC:-1}"
export E2E_SEED="${E2E_SEED:-0}"
export E2E_TIME_STEP_MS="${E2E_TIME_STEP_MS:-100}"

e2e_fixture_init "bd-2vr05.14.5" "$E2E_SEED" "$E2E_TIME_STEP_MS"

BEAD_ID="bd-2vr05.14.5"
SCENARIO="ligature-shaping-fallback"
RUN_ROOT="${RUN_ROOT:-/tmp/ftui_e2e_${BEAD_ID//./_}_$(e2e_log_stamp)}"
LOG_ROOT="$RUN_ROOT/logs"
EVIDENCE_JSONL="${EVIDENCE_JSONL:-$RUN_ROOT/${BEAD_ID}.jsonl}"
SUMMARY_JSON="${SUMMARY_JSON:-$RUN_ROOT/${BEAD_ID}_summary.json}"
REMOTE_LOG_ROOT="$RUN_ROOT/remote_ligature_shaping"
BENCH_MAX_MS="${BENCH_MAX_MS:-240000}"

mkdir -p "$LOG_ROOT" "$REMOTE_LOG_ROOT"

emit_evidence() {
    local step="$1"
    local expected="$2"
    local actual="$3"
    local outcome="$4"
    local latency_ms="$5"
    local error_code="$6"
    local fallback_triggered="$7"

    local ts
    ts="$(e2e_timestamp)"

    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg bead_id "$BEAD_ID" \
            --arg scenario "$SCENARIO" \
            --arg step "$step" \
            --arg expected "$expected" \
            --arg actual "$actual" \
            --arg outcome "$outcome" \
            --arg error_code "$error_code" \
            --argjson latency_ms "$latency_ms" \
            --argjson fallback_triggered "$fallback_triggered" \
            '{timestamp:$timestamp,run_id:$run_id,bead_id:$bead_id,scenario:$scenario,step:$step,expected:$expected,actual:$actual,outcome:$outcome,latency_ms:$latency_ms,error_code:$error_code,fallback_triggered:$fallback_triggered}' \
            >>"$EVIDENCE_JSONL"
    else
        printf '{"timestamp":"%s","run_id":"%s","bead_id":"%s","scenario":"%s","step":"%s","expected":"%s","actual":"%s","outcome":"%s","latency_ms":%s,"error_code":"%s","fallback_triggered":%s}\n' \
            "$ts" "$E2E_RUN_ID" "$BEAD_ID" "$SCENARIO" "$step" "$expected" "$actual" "$outcome" "$latency_ms" "$error_code" "$fallback_triggered" \
            >>"$EVIDENCE_JSONL"
    fi
}

run_step() {
    local step_name="$1"
    local expected="$2"
    local command="$3"
    local max_ms="${4:-0}"

    local log_file="$LOG_ROOT/${step_name}.log"
    local start_ms end_ms latency_ms rc
    local start_wall_ms end_wall_ms wall_latency_ms
    local outcome="pass"
    local actual="exit 0"
    local error_code=""
    local fallback_triggered=false

    start_ms="$(e2e_now_ms)"
    start_wall_ms="$(e2e_monotonic_ms)"
    set +e
    bash -lc "$command" >"$log_file" 2>&1
    rc=$?
    set -e
    end_ms="$(e2e_now_ms)"
    end_wall_ms="$(e2e_monotonic_ms)"
    latency_ms=$((end_ms - start_ms))
    wall_latency_ms=$((end_wall_ms - start_wall_ms))

    if [[ $rc -ne 0 ]]; then
        outcome="fail"
        actual="exit $rc"
        error_code="command_failed"
        fallback_triggered=true
    elif [[ "$max_ms" -gt 0 && "$wall_latency_ms" -gt "$max_ms" ]]; then
        outcome="fail"
        actual="wall latency ${wall_latency_ms}ms exceeded budget ${max_ms}ms"
        error_code="perf_budget_exceeded"
        fallback_triggered=true
        rc=1
    fi

    emit_evidence "$step_name" "$expected" "$actual" "$outcome" "$latency_ms" "$error_code" "$fallback_triggered"

    if [[ $rc -ne 0 ]]; then
        return $rc
    fi
}

overall_status=0

run_step \
    "unit_cache_ligature_toggle" \
    "cache key differs when standard ligature features toggle" \
    "rch exec -- cargo test -p ftui-text cache_miss_on_ligature_feature_toggle -- --nocapture" || overall_status=1

run_step \
    "unit_cache_invalidation_font_change" \
    "cache generation invalidation forces recompute for ligature entries" \
    "rch exec -- cargo test -p ftui-text cache_invalidation_recomputes_ligature_entries_after_font_change -- --nocapture" || overall_status=1

run_step \
    "unit_selection_cursor_boundaries_enabled" \
    "enabled ligatures preserve interaction invariants and extraction correctness" \
    "rch exec -- cargo test -p ftui-text ligature_mode_enabled_with_capability_shapes -- --nocapture" || overall_status=1

run_step \
    "unit_selection_cursor_boundaries_disabled" \
    "disabled ligatures force canonical grapheme boundaries" \
    "rch exec -- cargo test -p ftui-text ligature_mode_disabled_forces_canonical_boundaries -- --nocapture" || overall_status=1

run_step \
    "unit_memory_bound_lru_eviction" \
    "shaping cache respects bounded-capacity eviction contract" \
    "rch exec -- cargo test -p ftui-text cache_resize_evicts_lru -- --nocapture" || overall_status=1

run_step \
    "browser_e2e_remote_ligature_scenario" \
    "remote ligature scenario emits deterministic JSONL and failure-injection diagnostics" \
    "REMOTE_LOG_DIR='$REMOTE_LOG_ROOT' E2E_LOG_DIR='$REMOTE_LOG_ROOT' E2E_SEED='$E2E_SEED' E2E_DETERMINISTIC='$E2E_DETERMINISTIC' bash '$PROJECT_ROOT/tests/e2e/scripts/test_remote_ligature_shaping.sh'" || overall_status=1

run_step \
    "benchmark_shaped_render_smoke" \
    "criterion shaped-render benchmark completes within budget" \
    "rch exec -- cargo bench -p ftui-text --bench shaped_render_bench -- --sample-size 10 --warm-up-time 0.1 --measurement-time 0.2 --noplot" \
    "$BENCH_MAX_MS" || overall_status=1

python3 - "$SUMMARY_JSON" "$RUN_ROOT" "$EVIDENCE_JSONL" "$REMOTE_LOG_ROOT/ligature_rendering_report.json" "$overall_status" "$BENCH_MAX_MS" <<'PY'
import json
import os
import sys
from pathlib import Path

summary_path = Path(sys.argv[1])
run_root = Path(sys.argv[2])
evidence_jsonl = Path(sys.argv[3])
remote_report = Path(sys.argv[4])
overall_status = int(sys.argv[5])
bench_budget_ms = int(sys.argv[6])

payload = {
    "bead_id": "bd-2vr05.14.5",
    "suite": "ligature-shaping-fallback",
    "status": "pass" if overall_status == 0 else "fail",
    "run_root": str(run_root),
    "evidence_jsonl": str(evidence_jsonl),
    "bench_budget_ms": bench_budget_ms,
    "remote_report": str(remote_report) if remote_report.exists() else None,
}

if remote_report.exists():
    try:
        payload["remote_report_summary"] = json.loads(remote_report.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        payload["remote_report_summary"] = {"error": "invalid_json"}

summary_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
PY

if [[ $overall_status -eq 0 ]]; then
    emit_evidence "suite_summary" "all steps pass" "all steps pass" "pass" 0 "" false
else
    emit_evidence "suite_summary" "all steps pass" "one or more steps failed" "fail" 0 "suite_failed" true
fi

echo "bd-2vr05.14.5 evidence JSONL: $EVIDENCE_JSONL"
echo "bd-2vr05.14.5 summary report: $SUMMARY_JSON"
echo "bd-2vr05.14.5 logs: $LOG_ROOT"
echo "bd-2vr05.14.5 remote artifacts: $REMOTE_LOG_ROOT"

exit $overall_status

#!/usr/bin/env bash
set -euo pipefail

# bd-2vr05.14.6 addon-parity deterministic compatibility harness.
#
# Covers fit/web-font lifecycle, OSC8 links, image protocol behavior, progress
# invariants, ligature fallback, and differential checks with replay-grade
# JSONL evidence and summary metadata.

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

e2e_fixture_init "bd-2vr05.14.6" "$E2E_SEED" "$E2E_TIME_STEP_MS"

BEAD_ID="bd-2vr05.14.6"
SCENARIO="addon-parity-compatibility"
RUN_ROOT="${RUN_ROOT:-/tmp/ftui_e2e_${BEAD_ID//./_}_$(e2e_log_stamp)}"
LOG_ROOT="$RUN_ROOT/logs"
EVIDENCE_JSONL="${EVIDENCE_JSONL:-$RUN_ROOT/${BEAD_ID}.jsonl}"
SUMMARY_JSON="${SUMMARY_JSON:-$RUN_ROOT/${BEAD_ID}_summary.json}"

REMOTE_TYPOGRAPHY_ROOT="$RUN_ROOT/remote_typography_rescale"
REMOTE_OSC8_ROOT="$RUN_ROOT/remote_osc8"
REMOTE_LIGATURE_ROOT="$RUN_ROOT/remote_ligature_shaping"
REMOTE_DIFF_ROOT="$RUN_ROOT/remote_resize_differential"

ENABLE_REMOTE_CROSS_BROWSER_DIFF="${ENABLE_REMOTE_CROSS_BROWSER_DIFF:-0}"
REMOTE_DIFF_BROWSERS="${REMOTE_DIFF_BROWSERS:-chromium,webkit}"

mkdir -p \
    "$LOG_ROOT" \
    "$REMOTE_TYPOGRAPHY_ROOT" \
    "$REMOTE_OSC8_ROOT" \
    "$REMOTE_LIGATURE_ROOT" \
    "$REMOTE_DIFF_ROOT"

# Deterministic reruns must start from a clean evidence stream.
: >"$EVIDENCE_JSONL"

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

skip_step() {
    local step_name="$1"
    local expected="$2"
    local actual="$3"
    local error_code="${4:-step_skipped}"

    emit_evidence "$step_name" "$expected" "$actual" "skipped" 0 "$error_code" false
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
    "unit_fit_determinism" \
    "fit-to-container produces deterministic geometry for identical inputs" \
    "rch exec -- cargo test -p ftui-render fit_deterministic_across_calls -- --nocapture" || overall_status=1

run_step \
    "unit_font_lifecycle_refit_sync" \
    "dynamic web-font lifecycle events coalesce into deterministic refit output" \
    "rch exec -- cargo test -p ftui-render lifecycle_dynamic_font_event_stream_keeps_fit_in_sync -- --nocapture" || overall_status=1

run_step \
    "unit_osc8_hyperlink_roundtrip" \
    "OSC8 start/end handling preserves link metadata deterministically" \
    "rch exec -- cargo test -p ftui-render osc8_hyperlinks -- --nocapture" || overall_status=1

run_step \
    "unit_osc8_multiple_link_ids" \
    "multiple hyperlinks maintain stable and distinct link IDs" \
    "rch exec -- cargo test -p ftui-render multiple_hyperlinks_get_different_ids -- --nocapture" || overall_status=1

run_step \
    "unit_image_protocol_encoding" \
    "kitty image protocol encoding emits deterministic chunked payloads" \
    "rch exec -- cargo test -p ftui-extras image_encode_kitty_produces_chunks -- --nocapture" || overall_status=1

run_step \
    "unit_image_zero_dimension_bounds" \
    "image fit logic clamps zero-dimension edge cases deterministically" \
    "rch exec -- cargo test -p ftui-extras scale_to_fit_handles_zero_dimensions -- --nocapture" || overall_status=1

run_step \
    "unit_progress_ratio_clamp" \
    "progress ratio values above bounds clamp deterministically to 1.0" \
    "rch exec -- cargo test -p ftui-widgets ratio_clamped_above_one -- --nocapture" || overall_status=1

run_step \
    "unit_progress_async_bounds" \
    "async task progress remains bounded within [0,1] across deterministic ticks" \
    "rch exec -- cargo test -p ftui-demo-showcase e2e_progress_bounded -- --nocapture" || overall_status=1

run_step \
    "unit_ligature_cache_key_toggle" \
    "ligature feature toggles invalidate cache keys predictably" \
    "rch exec -- cargo test -p ftui-text cache_miss_on_ligature_feature_toggle -- --nocapture" || overall_status=1

run_step \
    "browser_e2e_typography_rescale" \
    "typography/rescale/browser evidence bundle is deterministic and replay-ready" \
    "REMOTE_LOG_DIR='$REMOTE_TYPOGRAPHY_ROOT' E2E_LOG_DIR='$REMOTE_TYPOGRAPHY_ROOT' E2E_SEED='$E2E_SEED' E2E_DETERMINISTIC='$E2E_DETERMINISTIC' E2E_TYPOGRAPHY_RESCALE_CROSS_BROWSER='0' bash '$PROJECT_ROOT/tests/e2e/scripts/test_remote_typography_rescale.sh'" || overall_status=1

run_step \
    "browser_e2e_remote_osc8_links" \
    "remote OSC8 link scenario emits deterministic JSONL traces and transcripts" \
    "REMOTE_LOG_DIR='$REMOTE_OSC8_ROOT' E2E_LOG_DIR='$REMOTE_OSC8_ROOT' E2E_SEED='$E2E_SEED' E2E_DETERMINISTIC='$E2E_DETERMINISTIC' bash '$PROJECT_ROOT/tests/e2e/scripts/test_remote_osc8_links.sh'" || overall_status=1

run_step \
    "browser_e2e_remote_ligature_shaping" \
    "remote ligature scenario emits deterministic success/failure diagnostics" \
    "REMOTE_LOG_DIR='$REMOTE_LIGATURE_ROOT' E2E_LOG_DIR='$REMOTE_LIGATURE_ROOT' E2E_SEED='$E2E_SEED' E2E_DETERMINISTIC='$E2E_DETERMINISTIC' bash '$PROJECT_ROOT/tests/e2e/scripts/test_remote_ligature_shaping.sh'" || overall_status=1

run_step \
    "differential_virtual_terminal_reference" \
    "differential replay remains aligned with virtual terminal references on scoped fixtures" \
    "rch exec -- cargo test -p ftui-extras differential_replay_matches_virtual_terminal_for_ -- --nocapture" || overall_status=1

if [[ "$ENABLE_REMOTE_CROSS_BROWSER_DIFF" == "1" ]]; then
    run_step \
        "differential_cross_browser_resize_semantics" \
        "cross-browser resize traces are classified with known-vs-unknown divergence reporting" \
        "E2E_LOG_DIR='$REMOTE_DIFF_ROOT' E2E_DIFF_LOG_DIR='$REMOTE_DIFF_ROOT' E2E_DIFF_BROWSERS='$REMOTE_DIFF_BROWSERS' E2E_SEED='$E2E_SEED' E2E_DETERMINISTIC='$E2E_DETERMINISTIC' bash '$PROJECT_ROOT/tests/e2e/scripts/test_remote_resize_storm_cross_browser_diff.sh'" || overall_status=1
else
    skip_step \
        "differential_cross_browser_resize_semantics" \
        "cross-browser differential runs when explicitly enabled" \
        "set ENABLE_REMOTE_CROSS_BROWSER_DIFF=1 to execute cross-browser diff suite" \
        "cross_browser_diff_disabled"
fi

if [[ -d "$PROJECT_ROOT/crates/frankenterm-web" ]]; then
    run_step \
        "progress_event_ordering_contract" \
        "terminal progress event ordering contract remains deterministic under attach/resize/input bursts" \
        "E2E_SEED='$E2E_SEED' E2E_DETERMINISTIC='$E2E_DETERMINISTIC' bash '$PROJECT_ROOT/tests/e2e/scripts/test_frankenterm_event_ordering_contract.sh'" || overall_status=1
else
    skip_step \
        "progress_event_ordering_contract" \
        "frankenterm-web event-ordering fixture executes when crate is present" \
        "missing crates/frankenterm-web in current workspace; skipping contract fixture" \
        "missing_frankenterm_web_crate"
fi

if [[ $overall_status -eq 0 ]]; then
    emit_evidence "suite_summary" "all mandatory addon parity checks pass" "all mandatory addon parity checks pass" "pass" 0 "" false
else
    emit_evidence "suite_summary" "all mandatory addon parity checks pass" "one or more mandatory checks failed" "fail" 0 "suite_failed" true
fi

python3 - \
    "$SUMMARY_JSON" \
    "$RUN_ROOT" \
    "$EVIDENCE_JSONL" \
    "$REMOTE_TYPOGRAPHY_ROOT/remote_typography_rescale/typography_rescale_e2e.jsonl" \
    "$REMOTE_TYPOGRAPHY_ROOT/remote_typography_rescale/typography_rescale_e2e_report.json" \
    "$REMOTE_OSC8_ROOT/osc8_links.jsonl" \
    "$REMOTE_LIGATURE_ROOT/ligature_rendering_report.json" \
    "$REMOTE_DIFF_ROOT/resize_storm_cross_browser_report.json" \
    "$overall_status" <<'PY'
import json
import sys
from pathlib import Path


def maybe_json(path: Path):
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {"error": "invalid_json", "path": str(path)}


summary_path = Path(sys.argv[1])
run_root = Path(sys.argv[2])
evidence_jsonl = Path(sys.argv[3])
typography_jsonl = Path(sys.argv[4])
typography_report = Path(sys.argv[5])
osc8_jsonl = Path(sys.argv[6])
ligature_report = Path(sys.argv[7])
diff_report = Path(sys.argv[8])
overall_status = int(sys.argv[9])

steps = []
counts = {"pass": 0, "fail": 0, "skipped": 0}

if evidence_jsonl.exists():
    for idx, line in enumerate(evidence_jsonl.read_text(encoding="utf-8").splitlines(), start=1):
        raw = line.strip()
        if not raw:
            continue
        try:
            event = json.loads(raw)
        except json.JSONDecodeError:
            steps.append({"step": f"jsonl_line_{idx}", "outcome": "invalid_json", "error_code": "parse_error"})
            continue
        outcome = str(event.get("outcome", "unknown"))
        if outcome in counts:
            counts[outcome] += 1
        steps.append(
            {
                "step": event.get("step"),
                "outcome": outcome,
                "latency_ms": event.get("latency_ms", 0),
                "error_code": event.get("error_code", ""),
                "expected": event.get("expected", ""),
                "actual": event.get("actual", ""),
            }
        )

payload = {
    "bead_id": "bd-2vr05.14.6",
    "suite": "addon-parity-compatibility",
    "status": "pass" if overall_status == 0 else "fail",
    "run_root": str(run_root),
    "evidence_jsonl": str(evidence_jsonl),
    "replay_command": f"RUN_ROOT='{run_root}' bash scripts/e2e/bd-2vr05.14.6.sh",
    "step_outcomes": counts,
    "steps": steps,
    "artifacts": {
        "typography_suite_jsonl": str(typography_jsonl) if typography_jsonl.exists() else None,
        "typography_suite_report": str(typography_report) if typography_report.exists() else None,
        "osc8_jsonl": str(osc8_jsonl) if osc8_jsonl.exists() else None,
        "ligature_report": str(ligature_report) if ligature_report.exists() else None,
        "cross_browser_diff_report": str(diff_report) if diff_report.exists() else None,
    },
    "artifact_summaries": {
        "typography_suite_report": maybe_json(typography_report),
        "ligature_report": maybe_json(ligature_report),
        "cross_browser_diff_report": maybe_json(diff_report),
    },
}

summary_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
PY

echo "bd-2vr05.14.6 evidence JSONL: $EVIDENCE_JSONL"
echo "bd-2vr05.14.6 summary report: $SUMMARY_JSON"
echo "bd-2vr05.14.6 logs: $LOG_ROOT"
echo "bd-2vr05.14.6 typography artifacts: $REMOTE_TYPOGRAPHY_ROOT"
echo "bd-2vr05.14.6 osc8 artifacts: $REMOTE_OSC8_ROOT"
echo "bd-2vr05.14.6 ligature artifacts: $REMOTE_LIGATURE_ROOT"
echo "bd-2vr05.14.6 differential artifacts: $REMOTE_DIFF_ROOT"

exit $overall_status

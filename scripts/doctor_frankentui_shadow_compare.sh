#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/shadow_compare_${TIMESTAMP_UTC}}"
SCENARIO_FILTER="${2:-all}"
LOG_DIR="${RUN_ROOT}/logs"
ARTIFACT_DIR="${RUN_ROOT}/artifacts"
META_DIR="${RUN_ROOT}/meta"
RUN_INDEX_TSV="${META_DIR}/run_index.tsv"
COMMAND_MANIFEST="${META_DIR}/command_manifest.txt"
REPORT_JSON="${ARTIFACT_DIR}/shadow_report.json"
SUMMARY_JSON="${META_DIR}/summary.json"
SUMMARY_TXT="${META_DIR}/summary.txt"
MANIFEST_JSON="${META_DIR}/artifact_manifest.json"
REPLAY_SH="${ARTIFACT_DIR}/replay.sh"

mkdir -p "${RUN_ROOT}" "${LOG_DIR}" "${ARTIFACT_DIR}" "${META_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[doctor-shadow] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

validate_scenario_filter() {
  case "${SCENARIO_FILTER}" in
    all|happy|failure) ;;
    *)
      echo "[doctor-shadow] scenario must be one of: all, happy, failure (got: ${SCENARIO_FILTER})" >&2
      exit 2
      ;;
  esac
}

record_command() {
  local label="$1"
  shift
  printf '[%s] ' "${label}" >> "${COMMAND_MANIFEST}"
  printf '%q ' "$@" >> "${COMMAND_MANIFEST}"
  printf '\n' >> "${COMMAND_MANIFEST}"
}

run_workflow() {
  local scenario="$1"
  local mode="$2"
  local workflow_script="${ROOT_DIR}/scripts/doctor_frankentui_${scenario}_e2e.sh"
  local workflow_root="${RUN_ROOT}/${scenario}/${mode}"
  local stdout_log="${LOG_DIR}/${scenario}_${mode}.stdout.log"
  local stderr_log="${LOG_DIR}/${scenario}_${mode}.stderr.log"
  local summary_json="${workflow_root}/meta/summary.json"
  local summary_txt="${workflow_root}/meta/summary.txt"
  local events_jsonl="${workflow_root}/meta/events.jsonl"
  local validation_json="${workflow_root}/meta/events_validation_report.json"
  local artifact_manifest_json="${workflow_root}/meta/artifact_manifest.json"
  local replay_report_json=""
  local replay_stdout_log=""
  local replay_stderr_log=""
  local workflow_exit_code=0
  local replay_exit_code=-1
  local replay_duration_seconds=0
  local start_epoch=0
  local end_epoch=0
  local duration_seconds=0

  if [[ ! -x "${workflow_script}" ]]; then
    echo "[doctor-shadow] required workflow script missing or not executable: ${workflow_script}" >&2
    exit 2
  fi

  start_epoch="$(date +%s)"
  if [[ "${mode}" == "conservative" ]]; then
    record_command "${scenario}_${mode}" env DOCTOR_FRANKENTUI_CONSERVATIVE=1 "${workflow_script}" "${workflow_root}"
    set +e
    env DOCTOR_FRANKENTUI_CONSERVATIVE=1 "${workflow_script}" "${workflow_root}" > "${stdout_log}" 2> "${stderr_log}"
    workflow_exit_code=$?
    set -e
  else
    record_command "${scenario}_${mode}" "${workflow_script}" "${workflow_root}"
    set +e
    "${workflow_script}" "${workflow_root}" > "${stdout_log}" 2> "${stderr_log}"
    workflow_exit_code=$?
    set -e
  fi

  end_epoch="$(date +%s)"
  duration_seconds=$((end_epoch - start_epoch))

  if [[ "${scenario}" == "failure" ]]; then
    local replay_start_epoch=0
    local replay_end_epoch=0
    replay_report_json="${workflow_root}/meta/replay_triage_report.json"
    replay_stdout_log="${LOG_DIR}/${scenario}_${mode}.replay.stdout.log"
    replay_stderr_log="${LOG_DIR}/${scenario}_${mode}.replay.stderr.log"
    record_command \
      "${scenario}_${mode}_replay" \
      "${ROOT_DIR}/scripts/doctor_frankentui_replay_triage.py" \
      --run-root "${workflow_root}" \
      --output-json "${replay_report_json}" \
      --max-signals 8 \
      --max-timeline 80
    replay_start_epoch="$(date +%s)"
    set +e
    "${ROOT_DIR}/scripts/doctor_frankentui_replay_triage.py" \
      --run-root "${workflow_root}" \
      --output-json "${replay_report_json}" \
      --max-signals 8 \
      --max-timeline 80 > "${replay_stdout_log}" 2> "${replay_stderr_log}"
    replay_exit_code=$?
    set -e
    replay_end_epoch="$(date +%s)"
    replay_duration_seconds=$((replay_end_epoch - replay_start_epoch))
  fi

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${scenario}" \
    "${mode}" \
    "${workflow_root}" \
    "${workflow_exit_code}" \
    "${duration_seconds}" \
    "${summary_json}" \
    "${summary_txt}" \
    "${events_jsonl}" \
    "${validation_json}" \
    "${artifact_manifest_json}" \
    "${stdout_log}" \
    "${stderr_log}" \
    "${replay_exit_code}" \
    "${replay_duration_seconds}" \
    "${replay_report_json}" >> "${RUN_INDEX_TSV}"
}

require_command "bash" "install bash"
require_command "python3" "install Python 3"
require_command "jq" "install jq"

validate_scenario_filter

: > "${RUN_INDEX_TSV}"
: > "${COMMAND_MANIFEST}"

# doctor_frankentui:no-fake-allow: replay delegates to this real workflow, which
# runs the happy/failure tools and preserves their exit codes and artifact logs.
cat > "${REPLAY_SH}" <<EOF
#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${ROOT_DIR}"
SCENARIO_FILTER="\${1:-${SCENARIO_FILTER}}"
RUN_ROOT="\${2:-/tmp/doctor_frankentui/shadow_compare_replay/\$(date -u +%Y%m%dT%H%M%SZ)}"

cd "\${ROOT_DIR}"
"\${ROOT_DIR}/scripts/doctor_frankentui_shadow_compare.sh" "\${RUN_ROOT}" "\${SCENARIO_FILTER}"
EOF
chmod +x "${REPLAY_SH}"

scenarios=()
if [[ "${SCENARIO_FILTER}" == "all" || "${SCENARIO_FILTER}" == "happy" ]]; then
  scenarios+=("happy")
fi
if [[ "${SCENARIO_FILTER}" == "all" || "${SCENARIO_FILTER}" == "failure" ]]; then
  scenarios+=("failure")
fi

for scenario in "${scenarios[@]}"; do
  run_workflow "${scenario}" "baseline"
  run_workflow "${scenario}" "conservative"
done

python3 - \
  "${RUN_INDEX_TSV}" \
  "${REPORT_JSON}" \
  "${SUMMARY_JSON}" \
  "${SUMMARY_TXT}" \
  "${MANIFEST_JSON}" \
  "${REPLAY_SH}" \
  "${COMMAND_MANIFEST}" \
  "${LOG_DIR}" \
  "${RUN_ROOT}" \
  "${SCENARIO_FILTER}" <<'PY'
from __future__ import annotations

import hashlib
import json
import statistics
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

run_index_tsv = Path(sys.argv[1])
report_json = Path(sys.argv[2])
summary_json = Path(sys.argv[3])
summary_txt = Path(sys.argv[4])
manifest_json = Path(sys.argv[5])
replay_sh = Path(sys.argv[6])
command_manifest = Path(sys.argv[7])
log_dir = Path(sys.argv[8])
run_root = Path(sys.argv[9]).resolve()
scenario_filter = sys.argv[10]


def now_utc_timestamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_json_details(path: Path) -> tuple[dict[str, Any], str | None]:
    if not path.exists():
        return {}, "missing"
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {}, "invalid_json"
    if isinstance(value, dict):
        return value, None
    return {}, "expected_object"


def mean_or_zero(values: list[float]) -> float:
    if not values:
        return 0.0
    return float(statistics.fmean(values))


rows: list[dict[str, Any]] = []
for raw_line in run_index_tsv.read_text(encoding="utf-8").splitlines():
    line = raw_line.rstrip("\r\n")
    if not line:
        continue
    (
        scenario,
        mode,
        workflow_root,
        workflow_exit_code,
        workflow_duration_seconds,
        row_summary_json,
        row_summary_txt,
        events_jsonl,
        validation_json,
        artifact_manifest_json,
        stdout_log,
        stderr_log,
        replay_exit_code,
        replay_duration_seconds,
        replay_report_json,
    ) = line.split("\t")
    rows.append(
        {
            "scenario": scenario,
            "mode": mode,
            "workflow_root": Path(workflow_root),
            "workflow_exit_code": int(workflow_exit_code),
            "workflow_duration_seconds": int(workflow_duration_seconds),
            "summary_json": Path(row_summary_json),
            "summary_txt": Path(row_summary_txt),
            "events_jsonl": Path(events_jsonl),
            "validation_json": Path(validation_json),
            "artifact_manifest_json": Path(artifact_manifest_json),
            "stdout_log": Path(stdout_log),
            "stderr_log": Path(stderr_log),
            "replay_exit_code": int(replay_exit_code),
            "replay_duration_seconds": int(replay_duration_seconds),
            "replay_report_json": Path(replay_report_json) if replay_report_json else None,
        }
    )

grouped: dict[str, dict[str, dict[str, Any]]] = {}
for row in rows:
    grouped.setdefault(row["scenario"], {})[row["mode"]] = row


def build_lane(row: dict[str, Any]) -> dict[str, Any]:
    summary, summary_parse_error = load_json_details(row["summary_json"])
    validation, validation_parse_error = load_json_details(row["validation_json"])
    top_level_artifact_manifest_present = row["artifact_manifest_json"].exists()
    top_level_manifest: dict[str, Any] = {}
    top_level_manifest_parse_error = None
    if top_level_artifact_manifest_present:
        top_level_manifest, top_level_manifest_parse_error = load_json_details(row["artifact_manifest_json"])

    replay_report: dict[str, Any] = {}
    replay_report_parse_error = None
    if row["replay_report_json"]:
        replay_report, replay_report_parse_error = load_json_details(row["replay_report_json"])

    workflow_root = row["workflow_root"]
    run_meta_paths = sorted(workflow_root.rglob("run_meta.json"))
    run_manifest_paths = sorted(workflow_root.rglob("run_artifact_manifest.json"))

    fallback_active_true_count = 0
    run_replay_command_count = 0
    run_meta_errors: list[str] = []
    run_manifest_errors: list[str] = []
    for run_meta_path in run_meta_paths:
        run_meta, run_meta_parse_error = load_json_details(run_meta_path)
        if run_meta_parse_error is not None:
            run_meta_errors.append(f"{run_meta_parse_error}:{run_meta_path}")
        elif run_meta.get("fallback_active") is True:
            fallback_active_true_count += 1
    for run_manifest_path in run_manifest_paths:
        manifest, run_manifest_parse_error = load_json_details(run_manifest_path)
        if run_manifest_parse_error is not None:
            run_manifest_errors.append(f"{run_manifest_parse_error}:{run_manifest_path}")
        else:
            replay_commands = manifest.get("replay_commands")
            if isinstance(replay_commands, list):
                run_replay_command_count += len(replay_commands)
            else:
                run_manifest_errors.append(f"invalid_replay_commands:{run_manifest_path}")

    phase_durations: list[float] = []
    primary_count = 0
    primary_success_count = 0
    case_results_path = None
    case_results_parse_error = None
    case_artifact_count = 0
    case_missing_artifact_count = 0
    if row["scenario"] == "happy":
        steps = summary.get("steps")
        if isinstance(steps, list):
            primary_count = len(steps)
            for step in steps:
                if isinstance(step, dict):
                    duration = step.get("duration_seconds")
                    if isinstance(duration, (int, float)):
                        phase_durations.append(float(duration))
                    if step.get("exit_code") == 0:
                        primary_success_count += 1
    elif row["scenario"] == "failure":
        total_cases = summary.get("total_cases")
        passed_cases = summary.get("passed_cases")
        if isinstance(total_cases, int):
            primary_count = total_cases
        if isinstance(passed_cases, int):
            primary_success_count = passed_cases
        case_results_value = summary.get("case_results")
        if isinstance(case_results_value, str) and case_results_value:
            case_results_path = Path(case_results_value)
            case_results, case_results_parse_error = load_json_details(case_results_path)
            cases = case_results.get("cases")
            if isinstance(cases, list):
                for case in cases:
                    if isinstance(case, dict):
                        duration = case.get("duration_seconds")
                        if isinstance(duration, (int, float)):
                            phase_durations.append(float(duration))
                        artifact_hashes = case.get("artifact_hashes")
                        if isinstance(artifact_hashes, dict):
                            case_artifact_count += len(artifact_hashes)
                        missing_artifacts = case.get("missing_artifacts")
                        if isinstance(missing_artifacts, list):
                            case_missing_artifact_count += len(missing_artifacts)
        validation_duration_ms = summary.get("events_validation_duration_ms")
        if isinstance(validation_duration_ms, (int, float)):
            phase_durations.append(float(validation_duration_ms) / 1000.0)

    missing_paths: list[str] = []
    required_paths = [
        row["summary_json"],
        row["summary_txt"],
        row["events_jsonl"],
        row["validation_json"],
        row["stdout_log"],
        row["stderr_log"],
    ]
    if row["scenario"] == "happy" or top_level_artifact_manifest_present:
        required_paths.append(row["artifact_manifest_json"])
    if case_results_path is not None:
        required_paths.append(case_results_path)
    if row["scenario"] == "failure" and row["replay_report_json"] is not None:
        required_paths.append(row["replay_report_json"])
    for required_path in required_paths:
        if not required_path.exists():
            missing_paths.append(str(required_path))

    return {
        "mode": row["mode"],
        "run_root": str(workflow_root),
        "workflow_exit_code": row["workflow_exit_code"],
        "workflow_status": summary.get("status", "missing"),
        "summary_parse_error": summary_parse_error,
        "workflow_duration_seconds": row["workflow_duration_seconds"],
        "replay_duration_seconds": row["replay_duration_seconds"],
        "total_duration_seconds": row["workflow_duration_seconds"] + row["replay_duration_seconds"],
        "operator_phase_average_seconds": round(mean_or_zero(phase_durations), 3),
        "operator_phase_tail_seconds": round(max(phase_durations, default=0.0), 3),
        "summary_path": str(row["summary_json"]),
        "summary_txt_path": str(row["summary_txt"]),
        "events_path": str(row["events_jsonl"]),
        "events_validation_report_path": str(row["validation_json"]),
        "events_validation_status": validation.get("status", "missing"),
        "events_validation_parse_error": validation_parse_error,
        "event_count": validation.get("total_events", 0),
        "artifact_manifest_path": str(row["artifact_manifest_json"]),
        "top_level_artifact_manifest_present": top_level_artifact_manifest_present,
        "top_level_artifact_manifest_parse_error": top_level_manifest_parse_error,
        "top_level_artifact_count": top_level_manifest.get("artifact_count", 0)
        if top_level_artifact_manifest_present
        else 0,
        "top_level_missing_count": top_level_manifest.get("missing_count", 0)
        if top_level_artifact_manifest_present
        else 0,
        "primary_count": primary_count,
        "primary_success_count": primary_success_count,
        "case_artifact_count": case_artifact_count,
        "case_missing_artifact_count": case_missing_artifact_count,
        "case_results_parse_error": case_results_parse_error,
        "run_meta_count": len(run_meta_paths),
        "run_meta_errors": run_meta_errors,
        "run_artifact_manifest_count": len(run_manifest_paths),
        "run_replay_command_count": run_replay_command_count,
        "fallback_active_true_count": fallback_active_true_count,
        "run_artifact_manifest_errors": run_manifest_errors,
        "replay_triage_exit_code": row["replay_exit_code"],
        "replay_triage_report_path": str(row["replay_report_json"]) if row["replay_report_json"] else None,
        "replay_triage_status": replay_report.get("status") if replay_report else None,
        "replay_triage_parse_error": replay_report_parse_error,
        "replay_signal_count": replay_report.get("signal_count") if replay_report else None,
        "missing_paths": missing_paths,
    }


scenario_reports: list[dict[str, Any]] = []
baseline_total_durations: list[float] = []
conservative_total_durations: list[float] = []
matched_scenarios = 0
total_mismatches = 0

for scenario in sorted(grouped.keys()):
    lanes = grouped[scenario]
    comparison_errors: list[str] = []

    baseline_row = lanes.get("baseline")
    conservative_row = lanes.get("conservative")
    if baseline_row is None or conservative_row is None:
        comparison_errors.append("missing baseline or conservative lane")
        baseline_lane = build_lane(baseline_row) if baseline_row is not None else {"mode": "baseline"}
        conservative_lane = (
            build_lane(conservative_row) if conservative_row is not None else {"mode": "conservative"}
        )
    else:
        baseline_lane = build_lane(baseline_row)
        conservative_lane = build_lane(conservative_row)

        for lane_name, lane in (("baseline", baseline_lane), ("conservative", conservative_lane)):
            if lane.get("workflow_exit_code") != 0:
                comparison_errors.append(f"{lane_name} workflow exited non-zero")
            if lane.get("summary_parse_error") is not None:
                comparison_errors.append(f"{lane_name} summary json is invalid or missing")
            if lane.get("workflow_status") == "missing":
                comparison_errors.append(f"{lane_name} summary missing workflow status")
            if lane.get("events_validation_parse_error") is not None:
                comparison_errors.append(f"{lane_name} events validation report is invalid or missing")
            if lane.get("events_validation_status") == "missing":
                comparison_errors.append(f"{lane_name} events validation report missing status")
            if lane.get("missing_paths"):
                comparison_errors.append(f"{lane_name} missing required artifacts")
            if lane.get("run_meta_errors"):
                comparison_errors.append(f"{lane_name} run_meta artifacts malformed")
            if lane.get("run_artifact_manifest_errors"):
                comparison_errors.append(f"{lane_name} run artifact manifests malformed")
            if scenario == "happy":
                if not lane.get("top_level_artifact_manifest_present"):
                    comparison_errors.append(f"{lane_name} happy lane missing top-level artifact manifest")
                if lane.get("top_level_artifact_manifest_parse_error") is not None:
                    comparison_errors.append(f"{lane_name} happy lane artifact manifest is invalid or missing")
                if lane.get("top_level_missing_count", 0) != 0:
                    comparison_errors.append(f"{lane_name} top-level artifact manifest has missing entries")
                if lane.get("run_replay_command_count", 0) == 0:
                    comparison_errors.append(f"{lane_name} run artifact manifests missing replay commands")
            elif scenario == "failure":
                if lane.get("case_results_parse_error") is not None:
                    comparison_errors.append(f"{lane_name} failure case results are invalid or missing")
                if lane.get("case_missing_artifact_count", 0) != 0:
                    comparison_errors.append(f"{lane_name} failure lane has missing case artifacts")
                if lane.get("replay_triage_parse_error") is not None:
                    comparison_errors.append(f"{lane_name} replay triage report is invalid or missing")
                if lane.get("replay_triage_status") is None:
                    comparison_errors.append(f"{lane_name} replay triage report missing status")

        if baseline_lane.get("workflow_status") != conservative_lane.get("workflow_status"):
            comparison_errors.append("workflow status diverged between baseline and conservative lanes")
        if baseline_lane.get("events_validation_status") != conservative_lane.get("events_validation_status"):
            comparison_errors.append("events validation status diverged between baseline and conservative lanes")
        if baseline_lane.get("primary_count") != conservative_lane.get("primary_count"):
            comparison_errors.append("primary workflow counts diverged between baseline and conservative lanes")
        if baseline_lane.get("primary_success_count") != conservative_lane.get("primary_success_count"):
            comparison_errors.append("primary success counts diverged between baseline and conservative lanes")
        if scenario == "happy":
            if baseline_lane.get("run_artifact_manifest_count") != conservative_lane.get("run_artifact_manifest_count"):
                comparison_errors.append("run artifact manifest counts diverged between lanes")
            if baseline_lane.get("run_replay_command_count") != conservative_lane.get("run_replay_command_count"):
                comparison_errors.append("run replay command counts diverged between lanes")
            if baseline_lane.get("top_level_artifact_count") != conservative_lane.get("top_level_artifact_count"):
                comparison_errors.append("top-level artifact counts diverged between lanes")
            if baseline_lane.get("run_artifact_manifest_count", 0) == 0:
                comparison_errors.append("happy scenario emitted zero run_artifact_manifest.json files")
            if conservative_lane.get("fallback_active_true_count", 0) <= baseline_lane.get("fallback_active_true_count", 0):
                comparison_errors.append("conservative lane did not increase fallback_active coverage")

        if scenario == "failure":
            if baseline_lane.get("case_artifact_count") != conservative_lane.get("case_artifact_count"):
                comparison_errors.append("failure case artifact counts diverged between lanes")
            if baseline_lane.get("case_missing_artifact_count") != conservative_lane.get("case_missing_artifact_count"):
                comparison_errors.append("failure case missing-artifact counts diverged between lanes")
            if baseline_lane.get("replay_triage_exit_code") != 0:
                comparison_errors.append("baseline replay triage exited non-zero")
            if conservative_lane.get("replay_triage_exit_code") != 0:
                comparison_errors.append("conservative replay triage exited non-zero")
            if baseline_lane.get("replay_triage_status") != conservative_lane.get("replay_triage_status"):
                comparison_errors.append("replay triage status diverged between lanes")
            if baseline_lane.get("replay_signal_count") != conservative_lane.get("replay_signal_count"):
                comparison_errors.append("replay triage signal count diverged between lanes")

        baseline_total_durations.append(float(baseline_lane.get("total_duration_seconds", 0)))
        conservative_total_durations.append(float(conservative_lane.get("total_duration_seconds", 0)))

    matched = not comparison_errors
    if matched:
        matched_scenarios += 1
    total_mismatches += len(comparison_errors)

    scenario_reports.append(
        {
            "scenario": scenario,
            "scenario_kind": "negative_control" if scenario == "failure" else "positive_control",
            "matched": matched,
            "mismatches": comparison_errors,
            "baseline": baseline_lane,
            "conservative": conservative_lane,
            "operator_turnaround": {
                "baseline_total_duration_seconds": baseline_lane.get("total_duration_seconds", 0),
                "conservative_total_duration_seconds": conservative_lane.get("total_duration_seconds", 0),
                "average_total_duration_seconds": round(
                    mean_or_zero(
                        [
                            float(baseline_lane.get("total_duration_seconds", 0)),
                            float(conservative_lane.get("total_duration_seconds", 0)),
                        ]
                    ),
                    3,
                ),
                "tail_total_duration_seconds": max(
                    float(baseline_lane.get("total_duration_seconds", 0)),
                    float(conservative_lane.get("total_duration_seconds", 0)),
                ),
                "delta_total_duration_seconds": round(
                    float(conservative_lane.get("total_duration_seconds", 0))
                    - float(baseline_lane.get("total_duration_seconds", 0)),
                    3,
                ),
            },
        }
    )

diverged_scenarios = len(scenario_reports) - matched_scenarios
status = "passed" if diverged_scenarios == 0 else "failed"

report_payload = {
    "generated_at_utc": now_utc_timestamp(),
    "status": status,
    "run_root": str(run_root),
    "scenario_filter": scenario_filter,
    "summary": {
        "total_scenarios": len(scenario_reports),
        "matched_scenarios": matched_scenarios,
        "diverged_scenarios": diverged_scenarios,
        "total_mismatches": total_mismatches,
        "baseline_average_total_duration_seconds": round(mean_or_zero(baseline_total_durations), 3),
        "conservative_average_total_duration_seconds": round(
            mean_or_zero(conservative_total_durations),
            3,
        ),
        "baseline_tail_total_duration_seconds": round(max(baseline_total_durations, default=0.0), 3),
        "conservative_tail_total_duration_seconds": round(
            max(conservative_total_durations, default=0.0),
            3,
        ),
    },
    "scenarios": scenario_reports,
}
report_json.write_text(json.dumps(report_payload, indent=2) + "\n", encoding="utf-8")

summary_payload = {
    "status": status,
    "run_root": str(run_root),
    "scenario_filter": scenario_filter,
    "report_json": str(report_json),
    "matched_scenarios": matched_scenarios,
    "diverged_scenarios": diverged_scenarios,
    "total_mismatches": total_mismatches,
    "baseline_average_total_duration_seconds": report_payload["summary"][
        "baseline_average_total_duration_seconds"
    ],
    "conservative_average_total_duration_seconds": report_payload["summary"][
        "conservative_average_total_duration_seconds"
    ],
    "baseline_tail_total_duration_seconds": report_payload["summary"][
        "baseline_tail_total_duration_seconds"
    ],
    "conservative_tail_total_duration_seconds": report_payload["summary"][
        "conservative_tail_total_duration_seconds"
    ],
}
summary_json.write_text(json.dumps(summary_payload, indent=2) + "\n", encoding="utf-8")

summary_lines = [
    f"status={status}",
    f"run_root={run_root}",
    f"scenario_filter={scenario_filter}",
    f"report_json={report_json}",
    f"matched_scenarios={matched_scenarios}",
    f"diverged_scenarios={diverged_scenarios}",
    f"total_mismatches={total_mismatches}",
    (
        "baseline_average_total_duration_seconds="
        f"{report_payload['summary']['baseline_average_total_duration_seconds']}"
    ),
    (
        "conservative_average_total_duration_seconds="
        f"{report_payload['summary']['conservative_average_total_duration_seconds']}"
    ),
    (
        "baseline_tail_total_duration_seconds="
        f"{report_payload['summary']['baseline_tail_total_duration_seconds']}"
    ),
    (
        "conservative_tail_total_duration_seconds="
        f"{report_payload['summary']['conservative_tail_total_duration_seconds']}"
    ),
]
for scenario_report in scenario_reports:
    summary_lines.append(
        f"scenario={scenario_report['scenario']} matched={int(bool(scenario_report['matched']))} "
        f"mismatches={len(scenario_report['mismatches'])}"
    )
    for mismatch in scenario_report["mismatches"]:
        summary_lines.append(f"- {mismatch}")
summary_txt.write_text("\n".join(summary_lines) + "\n", encoding="utf-8")

artifact_paths = [
    run_index_tsv,
    command_manifest,
    report_json,
    summary_json,
    summary_txt,
    replay_sh,
]
artifact_paths.extend(sorted(log_dir.glob("*.log")))

manifest_entries = []
for artifact_path in artifact_paths:
    payload = artifact_path.read_bytes()
    manifest_entries.append(
        {
            "path": str(artifact_path),
            "size_bytes": len(payload),
            "sha256": hashlib.sha256(payload).hexdigest(),
        }
    )

manifest_json.write_text(
    json.dumps(
        {
            "artifact_count": len(manifest_entries),
            "artifacts": manifest_entries,
        },
        indent=2,
    )
    + "\n",
    encoding="utf-8",
)

print("\n".join(summary_lines))
raise SystemExit(0 if status == "passed" else 1)
PY

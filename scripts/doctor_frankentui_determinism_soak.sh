#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/determinism_soak_${TIMESTAMP_UTC}}"
ITERATIONS="${2:-${DOCTOR_FRANKENTUI_SOAK_RUNS:-3}}"
LOG_DIR="${RUN_ROOT}/logs"
META_DIR="${RUN_ROOT}/meta"
RUN_INDEX_TSV="${META_DIR}/run_index.tsv"
REPORT_JSON="${META_DIR}/determinism_report.json"
REPORT_TXT="${META_DIR}/determinism_report.txt"
SCHEMA_PATH="${ROOT_DIR}/crates/doctor_frankentui/coverage/e2e_jsonl_schema.json"

mkdir -p "${RUN_ROOT}" "${LOG_DIR}" "${META_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[determinism] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

if [[ ! "${ITERATIONS}" =~ ^[0-9]+$ ]] || [[ "${ITERATIONS}" -lt 1 ]]; then
  echo "[determinism] iterations must be a positive integer (got: ${ITERATIONS})" >&2
  exit 2
fi

if [[ ! -f "${SCHEMA_PATH}" ]]; then
  echo "[determinism] schema file not found: ${SCHEMA_PATH}" >&2
  exit 2
fi

require_command "bash" "install bash"
require_command "python3" "install Python 3"
require_command "jq" "install jq for JSON checks"

: > "${RUN_INDEX_TSV}"

run_workflow_iteration() {
  local workflow="$1"
  local iteration="$2"
  local script_path="${ROOT_DIR}/scripts/doctor_frankentui_${workflow}_e2e.sh"
  local run_dir="${RUN_ROOT}/${workflow}_run_${iteration}"
  local stdout_log="${LOG_DIR}/${workflow}_run_${iteration}.stdout.log"
  local stderr_log="${LOG_DIR}/${workflow}_run_${iteration}.stderr.log"
  local summary_json="${run_dir}/meta/summary.json"
  local events_jsonl="${run_dir}/meta/events.jsonl"
  local validation_json="${run_dir}/meta/events_validation_report.json"

  if [[ ! -x "${script_path}" ]]; then
    echo "[determinism] required workflow script missing or not executable: ${script_path}" >&2
    exit 2
  fi

  echo "[determinism] running ${workflow} iteration ${iteration}/${ITERATIONS}"
  set +e
  "${script_path}" "${run_dir}" > "${stdout_log}" 2> "${stderr_log}"
  local exit_code=$?
  set -e

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${workflow}" \
    "${iteration}" \
    "${run_dir}" \
    "${exit_code}" \
    "${summary_json}" \
    "${events_jsonl}" \
    "${validation_json}" \
    "${stdout_log}" \
    "${stderr_log}" >> "${RUN_INDEX_TSV}"
}

for ((i=1; i<=ITERATIONS; i++)); do
  run_workflow_iteration "happy" "${i}"
  run_workflow_iteration "failure" "${i}"
done

python3 - \
  "${RUN_INDEX_TSV}" \
  "${REPORT_JSON}" \
  "${REPORT_TXT}" \
  "${RUN_ROOT}" \
  "${ITERATIONS}" \
  "${SCHEMA_PATH}" <<'PY'
from __future__ import annotations

import hashlib
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

run_index_tsv = Path(sys.argv[1])
report_json_path = Path(sys.argv[2])
report_txt_path = Path(sys.argv[3])
soak_root = Path(sys.argv[4]).resolve()
iterations = int(sys.argv[5])
schema_path = Path(sys.argv[6])

VOLATILE_ARTIFACT_SUFFIX_ALLOWLIST = [
    "/run_meta.json",
    "/run_summary.txt",
    "/suite_summary.txt",
    "/report.json",
    "/index.html",
    "/custom_report.json",
    "/custom_report.html",
    "/case_results.json",
    "/summary.json",
    "/summary.txt",
    "/vhs.log",
    "/capture.tape",
    "/snapshot.png",
    "/session.txt",
    "/env_snapshot.txt",
    "/tool_versions.txt",
    "/command_manifest.txt",
    "/step_results.tsv",
]

CORRELATION_SUFFIX_RE = re.compile(r"^(?P<prefix>.+)-corr-(?P<seq>\d+)$")


def now_utc_timestamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def is_hex64(value: Any) -> bool:
    return isinstance(value, str) and bool(re.fullmatch(r"[0-9a-f]{64}", value))


def parse_json_file(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
        if isinstance(loaded, dict):
            return loaded
        return {}
    except json.JSONDecodeError:
        return {}


def parse_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.exists():
        return []
    events: list[dict[str, Any]] = []
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line:
            continue
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            return []
        if isinstance(payload, dict):
            events.append(payload)
    return events


def normalize_path_value(value: str, run_dir: Path) -> str:
    text = value
    text = text.replace(str(soak_root), "<SOAK_ROOT>")
    text = text.replace(str(run_dir), "<RUN_DIR>")
    return text


def normalize_value(value: Any, run_dir: Path) -> Any:
    if isinstance(value, str):
        return normalize_path_value(value, run_dir)
    if isinstance(value, list):
        return [normalize_value(item, run_dir) for item in value]
    if isinstance(value, dict):
        return {key: normalize_value(value[key], run_dir) for key in sorted(value.keys())}
    return value


def is_volatile_artifact(path: str) -> bool:
    return any(path.endswith(suffix) for suffix in VOLATILE_ARTIFACT_SUFFIX_ALLOWLIST)


def normalize_event(event: dict[str, Any], run_dir: Path) -> dict[str, Any]:
    artifact_hashes = event.get("artifact_hashes", {})
    stable_artifact_hashes: dict[str, str] = {}
    volatile_artifact_count = 0
    artifact_hash_shape_errors: list[str] = []

    if isinstance(artifact_hashes, dict):
        for raw_path, raw_hash in sorted(artifact_hashes.items()):
            normalized_path = normalize_path_value(str(raw_path), run_dir)
            if not is_hex64(raw_hash):
                artifact_hash_shape_errors.append(normalized_path)
                continue
            if is_volatile_artifact(normalized_path):
                volatile_artifact_count += 1
                continue
            stable_artifact_hashes[normalized_path] = raw_hash
    else:
        artifact_hash_shape_errors.append("artifact_hashes_not_object")

    return {
        "schema_version": event.get("schema_version"),
        "case_id": normalize_value(event.get("case_id"), run_dir),
        "step_id": normalize_value(event.get("step_id"), run_dir),
        "event_type": event.get("event_type"),
        "command": normalize_value(event.get("command"), run_dir),
        "env_hash": event.get("env_hash"),
        "exit_code": event.get("exit_code"),
        "expected": normalize_value(event.get("expected", {}), run_dir),
        "actual": normalize_value(event.get("actual", {}), run_dir),
        "stable_artifact_hashes": stable_artifact_hashes,
        "artifact_hash_count": len(artifact_hashes) if isinstance(artifact_hashes, dict) else 0,
        "volatile_artifact_hash_count": volatile_artifact_count,
        "artifact_hash_shape_errors": artifact_hash_shape_errors,
    }


def validate_event_order(
    workflow: str,
    events: list[dict[str, Any]],
) -> list[str]:
    errors: list[str] = []
    if not events:
        errors.append("events_jsonl is empty")
        return errors

    first_event_type = events[0].get("event_type")
    last_event_type = events[-1].get("event_type")
    if first_event_type != "run_start":
        errors.append(f"first event_type must be run_start (got {first_event_type!r})")
    if last_event_type != "run_end":
        errors.append(f"last event_type must be run_end (got {last_event_type!r})")

    run_id = events[0].get("run_id")
    if not isinstance(run_id, str) or not run_id:
        errors.append("missing run_id on first event")
        run_id = ""

    expected_seq = 1
    seen_correlation: set[str] = set()
    for index, event in enumerate(events, start=1):
        correlation_id = event.get("correlation_id")
        if not isinstance(correlation_id, str) or not correlation_id:
            errors.append(f"event {index}: correlation_id missing or invalid")
            continue
        if correlation_id in seen_correlation:
            errors.append(f"event {index}: duplicate correlation_id {correlation_id}")
            continue
        seen_correlation.add(correlation_id)
        match = CORRELATION_SUFFIX_RE.match(correlation_id)
        if match is None:
            errors.append(f"event {index}: invalid correlation_id format {correlation_id}")
            continue
        prefix = match.group("prefix")
        seq = int(match.group("seq"))
        if run_id and prefix != run_id:
            errors.append(
                f"event {index}: correlation prefix {prefix!r} does not match run_id {run_id!r}"
            )
        if seq != expected_seq:
            errors.append(
                f"event {index}: correlation sequence expected {expected_seq} got {seq}"
            )
            expected_seq = seq + 1
        else:
            expected_seq += 1

    if workflow == "happy":
        steps: dict[str, set[str]] = {}
        for event in events:
            step_id = event.get("step_id")
            event_type = event.get("event_type")
            if isinstance(step_id, str) and step_id:
                steps.setdefault(step_id, set()).add(str(event_type))
        for step_id, seen in sorted(steps.items()):
            missing = {"step_start", "step_end"} - seen
            for event_type in sorted(missing):
                errors.append(f"step_id {step_id!r} missing {event_type}")

    if workflow == "failure":
        cases: dict[str, set[str]] = {}
        for event in events:
            case_id = event.get("case_id")
            event_type = event.get("event_type")
            if isinstance(case_id, str) and case_id and case_id != "__run__":
                cases.setdefault(case_id, set()).add(str(event_type))
        for case_id, seen in sorted(cases.items()):
            missing = {"case_start", "case_end"} - seen
            for event_type in sorted(missing):
                errors.append(f"case_id {case_id!r} missing {event_type}")

    return errors


def required_field_errors(events: list[dict[str, Any]], required_fields: list[str]) -> list[str]:
    errors: list[str] = []
    for index, event in enumerate(events, start=1):
        for field in required_fields:
            if field not in event:
                errors.append(f"event {index}: missing required field {field!r}")
    return errors


rows: list[dict[str, Any]] = []
for raw in run_index_tsv.read_text(encoding="utf-8").splitlines():
    if not raw.strip():
        continue
    (
        workflow,
        iteration,
        run_dir,
        exit_code,
        summary_json,
        events_jsonl,
        validation_json,
        stdout_log,
        stderr_log,
    ) = raw.split("\t")
    run_dir_path = Path(run_dir).resolve()
    summary = parse_json_file(Path(summary_json))
    validation = parse_json_file(Path(validation_json))
    events = parse_jsonl(Path(events_jsonl))

    rows.append(
        {
            "workflow": workflow,
            "iteration": int(iteration),
            "run_dir": str(run_dir_path),
            "exit_code": int(exit_code),
            "summary_json": summary_json,
            "events_jsonl": events_jsonl,
            "events_validation_report_json": validation_json,
            "stdout_log": stdout_log,
            "stderr_log": stderr_log,
            "summary": summary,
            "events": events,
            "validation": validation,
        }
    )

rows.sort(key=lambda row: (row["workflow"], row["iteration"]))

schema_obj = parse_json_file(schema_path)
required_fields = schema_obj.get("required_fields", [])
if not isinstance(required_fields, list):
    required_fields = []

workflow_runs: dict[str, list[dict[str, Any]]] = {"happy": [], "failure": []}
for row in rows:
    workflow_runs.setdefault(row["workflow"], []).append(row)

workflow_reports: dict[str, Any] = {}
divergence_entries: list[dict[str, Any]] = []
global_errors: list[str] = []
overall_skipped = True

for workflow, run_list in workflow_runs.items():
    if not run_list:
        global_errors.append(f"workflow {workflow!r} has no runs")
        workflow_reports[workflow] = {"status": "failed", "runs": []}
        overall_skipped = False
        continue

    run_reports: list[dict[str, Any]] = []
    skipped_iterations: list[int] = []
    non_skipped_iterations: list[int] = []

    normalized_baseline: list[dict[str, Any]] | None = None
    baseline_iteration: int | None = None

    for row in run_list:
        summary_status = row["summary"].get("status", "unknown")
        validation_status = row["validation"].get("status", "unknown")
        events = row["events"]
        run_dir = Path(row["run_dir"])

        run_errors: list[str] = []
        if row["exit_code"] != 0:
            run_errors.append(f"workflow script exited {row['exit_code']}")

        if summary_status not in {"passed", "skipped"}:
            run_errors.append(f"unexpected summary status: {summary_status!r}")
        if validation_status not in {"passed", "skipped"}:
            run_errors.append(f"unexpected validation status: {validation_status!r}")

        if summary_status == "skipped":
            skipped_iterations.append(row["iteration"])
        else:
            non_skipped_iterations.append(row["iteration"])
            run_errors.extend(required_field_errors(events, required_fields))
            run_errors.extend(validate_event_order(workflow, events))

            normalized_events = [normalize_event(event, run_dir) for event in events]
            fingerprint_payload = json.dumps(
                normalized_events,
                sort_keys=True,
                separators=(",", ":"),
            ).encode("utf-8")
            fingerprint = sha256_bytes(fingerprint_payload)

            artifact_shape_errors = [
                {
                    "event_index": index + 1,
                    "invalid_entries": entry["artifact_hash_shape_errors"],
                }
                for index, entry in enumerate(normalized_events)
                if entry["artifact_hash_shape_errors"]
            ]
            if artifact_shape_errors:
                run_errors.append(
                    f"artifact hash shape errors detected: {artifact_shape_errors}"
                )

            if normalized_baseline is None:
                normalized_baseline = normalized_events
                baseline_iteration = row["iteration"]
            else:
                divergence: dict[str, Any] | None = None
                if len(normalized_events) != len(normalized_baseline):
                    divergence = {
                        "reason": "event_count_mismatch",
                        "baseline_event_count": len(normalized_baseline),
                        "current_event_count": len(normalized_events),
                        "first_divergence_event_index": 1,
                    }
                else:
                    for event_index, (baseline_event, current_event) in enumerate(
                        zip(normalized_baseline, normalized_events),
                        start=1,
                    ):
                        if baseline_event != current_event:
                            divergence = {
                                "reason": "normalized_event_mismatch",
                                "first_divergence_event_index": event_index,
                                "baseline_event_type": baseline_event.get("event_type"),
                                "current_event_type": current_event.get("event_type"),
                                "baseline_step_id": baseline_event.get("step_id"),
                                "current_step_id": current_event.get("step_id"),
                                "baseline_case_id": baseline_event.get("case_id"),
                                "current_case_id": current_event.get("case_id"),
                            }
                            break

                if divergence is not None:
                    divergence_entry = {
                        "workflow": workflow,
                        "baseline_iteration": baseline_iteration,
                        "current_iteration": row["iteration"],
                        "run_dir": row["run_dir"],
                        "events_jsonl": row["events_jsonl"],
                        "summary_json": row["summary_json"],
                        "stdout_log": row["stdout_log"],
                        "stderr_log": row["stderr_log"],
                        **divergence,
                    }
                    divergence_entries.append(divergence_entry)
                    run_errors.append(
                        "divergence from baseline detected at event index "
                        f"{divergence['first_divergence_event_index']}"
                    )

            row_report = {
                "iteration": row["iteration"],
                "run_dir": row["run_dir"],
                "status": summary_status,
                "exit_code": row["exit_code"],
                "events_count": len(events),
                "events_validation_status": validation_status,
                "summary_json": row["summary_json"],
                "events_jsonl": row["events_jsonl"],
                "events_validation_report_json": row["events_validation_report_json"],
                "stdout_log": row["stdout_log"],
                "stderr_log": row["stderr_log"],
                "normalized_fingerprint": fingerprint,
                "errors": run_errors,
            }
            run_reports.append(row_report)
            continue

        row_report = {
            "iteration": row["iteration"],
            "run_dir": row["run_dir"],
            "status": summary_status,
            "exit_code": row["exit_code"],
            "events_count": len(events),
            "events_validation_status": validation_status,
            "summary_json": row["summary_json"],
            "events_jsonl": row["events_jsonl"],
            "events_validation_report_json": row["events_validation_report_json"],
            "stdout_log": row["stdout_log"],
            "stderr_log": row["stderr_log"],
            "skip_reason": row["summary"].get("reason", "unknown"),
            "errors": run_errors,
        }
        run_reports.append(row_report)

    workflow_status = "passed"
    if skipped_iterations and non_skipped_iterations:
        workflow_status = "failed"
        global_errors.append(
            f"workflow {workflow!r} mixed skipped/non-skipped runs: "
            f"skipped={skipped_iterations}, non_skipped={non_skipped_iterations}"
        )
    elif non_skipped_iterations:
        overall_skipped = False

    for report in run_reports:
        if report["errors"]:
            workflow_status = "failed"
            overall_skipped = False

    workflow_reports[workflow] = {
        "status": workflow_status if non_skipped_iterations else "skipped",
        "iterations": len(run_reports),
        "skipped_iterations": skipped_iterations,
        "non_skipped_iterations": non_skipped_iterations,
        "runs": run_reports,
    }

if divergence_entries:
    overall_skipped = False

if overall_skipped:
    overall_status = "skipped"
elif global_errors or divergence_entries or any(
    report.get("status") == "failed" for report in workflow_reports.values()
):
    overall_status = "failed"
else:
    overall_status = "passed"

first_divergence = divergence_entries[0] if divergence_entries else None

report = {
    "generated_at": now_utc_timestamp(),
    "status": overall_status,
    "iterations_requested": iterations,
    "run_root": str(soak_root),
    "volatile_artifact_suffix_allowlist": VOLATILE_ARTIFACT_SUFFIX_ALLOWLIST,
    "workflow_reports": workflow_reports,
    "global_errors": global_errors,
    "divergences": divergence_entries,
    "first_divergence": first_divergence,
}

report_json_path.parent.mkdir(parents=True, exist_ok=True)
report_json_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

lines: list[str] = []
lines.append(f"status={overall_status}")
lines.append(f"run_root={soak_root}")
lines.append(f"iterations={iterations}")
lines.append(f"report_json={report_json_path}")
for workflow, data in workflow_reports.items():
    lines.append(
        f"workflow={workflow} status={data['status']} "
        f"runs={data['iterations']} non_skipped={len(data['non_skipped_iterations'])} "
        f"skipped={len(data['skipped_iterations'])}"
    )
if first_divergence is not None:
    lines.append("first_divergence:")
    lines.append(
        "workflow={workflow} baseline_iteration={baseline} current_iteration={current} "
        "event_index={index} reason={reason}".format(
            workflow=first_divergence["workflow"],
            baseline=first_divergence["baseline_iteration"],
            current=first_divergence["current_iteration"],
            index=first_divergence["first_divergence_event_index"],
            reason=first_divergence["reason"],
        )
    )
    lines.append(f"events_jsonl={first_divergence['events_jsonl']}")
    lines.append(f"summary_json={first_divergence['summary_json']}")
    lines.append(f"stdout_log={first_divergence['stdout_log']}")
    lines.append(f"stderr_log={first_divergence['stderr_log']}")
if global_errors:
    lines.append("global_errors:")
    for error in global_errors:
        lines.append(f"- {error}")

report_txt_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
print("\n".join(lines))

if overall_status == "failed":
    raise SystemExit(1)
raise SystemExit(0)
PY

echo "[determinism] report_json=${REPORT_JSON}"
echo "[determinism] report_txt=${REPORT_TXT}"

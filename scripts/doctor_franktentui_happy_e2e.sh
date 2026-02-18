#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_franktentui/e2e/happy_${TIMESTAMP_UTC}}"
PROJECT_DIR="${RUN_ROOT}/project"
LOG_DIR="${RUN_ROOT}/logs"
META_DIR="${RUN_ROOT}/meta"
STEP_RESULTS_TSV="${META_DIR}/step_results.tsv"
COMMAND_MANIFEST="${META_DIR}/command_manifest.txt"
ENV_SNAPSHOT="${META_DIR}/env_snapshot.txt"
VERSIONS_TXT="${META_DIR}/tool_versions.txt"
ARTIFACT_MANIFEST_JSON="${META_DIR}/artifact_manifest.json"
SUMMARY_JSON="${META_DIR}/summary.json"
SUMMARY_TXT="${META_DIR}/summary.txt"
EVENTS_JSONL="${META_DIR}/events.jsonl"
EVENT_VALIDATION_REPORT_JSON="${META_DIR}/events_validation_report.json"
TARGET_DIR=""
BIN_PATH=""
MISSING_RUNTIME_TOOLS=()

mkdir -p "${PROJECT_DIR}" "${LOG_DIR}" "${META_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[e2e] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

detect_runtime_toolchain_gaps() {
  local command
  for command in vhs ffmpeg ffprobe; do
    if ! command -v "${command}" >/dev/null 2>&1; then
      MISSING_RUNTIME_TOOLS+=("${command}")
    fi
  done
}

emit_skip_and_exit() {
  local missing_json
  missing_json="$(printf '%s\n' "${MISSING_RUNTIME_TOOLS[@]}" | jq -R . | jq -s .)"
  local reason="missing runtime tools required for real-behavior e2e capture"

  : > "${STEP_RESULTS_TSV}"
  : > "${COMMAND_MANIFEST}"
  : > "${EVENTS_JSONL}"

  jq -n \
    --arg status "skipped" \
    --arg reason "${reason}" \
    --arg run_root "${RUN_ROOT}" \
    --arg events_jsonl "${EVENTS_JSONL}" \
    --arg validation_report "${EVENT_VALIDATION_REPORT_JSON}" \
    --argjson missing_tools "${missing_json}" \
    '{
      status: $status,
      reason: $reason,
      run_root: $run_root,
      events_jsonl: $events_jsonl,
      events_validation_report: $validation_report,
      missing_tools: $missing_tools,
      steps: [],
      missing_artifacts: []
    }' > "${SUMMARY_JSON}"

  jq -n \
    --arg run_root "${RUN_ROOT}" \
    --argjson missing_tools "${missing_json}" \
    '{
      run_root: $run_root,
      artifact_count: 0,
      missing_count: 0,
      artifacts: [],
      missing: [],
      missing_tools: $missing_tools
    }' > "${ARTIFACT_MANIFEST_JSON}"

  jq -n \
    --arg status "skipped" \
    --argjson missing_tools "${missing_json}" \
    '{
      status: $status,
      errors: [],
      total_events: 0,
      workflow: "happy",
      missing_tools: $missing_tools
    }' > "${EVENT_VALIDATION_REPORT_JSON}"

  {
    echo "status=skipped"
    echo "reason=${reason}"
    echo "run_root=${RUN_ROOT}"
    echo "missing_tools=$(IFS=,; echo "${MISSING_RUNTIME_TOOLS[*]}")"
    echo "summary_json=${SUMMARY_JSON}"
    echo "events_jsonl=${EVENTS_JSONL}"
    echo "events_validation_report=${EVENT_VALIDATION_REPORT_JSON}"
  } > "${SUMMARY_TXT}"

  cat "${SUMMARY_TXT}"
  exit 0
}

require_command "cargo" "install Rust/Cargo toolchain"
require_command "jq" "install jq for JSON parsing"
require_command "python3" "install Python 3"

{
  echo "timestamp_utc=${TIMESTAMP_UTC}"
  echo "run_root=${RUN_ROOT}"
  echo "root_dir=${ROOT_DIR}"
  echo "project_dir=${PROJECT_DIR}"
} > "${META_DIR}/session.txt"

{
  env | sort | grep -E '^(CI|TERM|SHELL|USER|HOME|PATH|RUSTUP_TOOLCHAIN|SQLMODEL_|FASTAPI_)=' || true
} > "${ENV_SNAPSHOT}"

{
  echo "doctor_franktentui_happy_e2e"
  echo "timestamp_utc=${TIMESTAMP_UTC}"
  echo "git_rev=$(git -C "${ROOT_DIR}" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "cargo_version=$(cargo --version)"
  echo "rustc_version=$(rustc --version 2>/dev/null || echo rustc-missing)"
  echo "doctor_version_prebuild=unknown"
  echo "vhs_path=$(command -v vhs 2>/dev/null || echo missing)"
  echo "ffmpeg_path=$(command -v ffmpeg 2>/dev/null || echo missing)"
  echo "ffprobe_path=$(command -v ffprobe 2>/dev/null || echo missing)"
  echo "vhs_version=$(vhs --version 2>/dev/null | head -n 1 || echo unknown)"
  echo "ffmpeg_version=$(ffmpeg -version 2>/dev/null | head -n 1 || echo unknown)"
  echo "ffprobe_version=$(ffprobe -version 2>/dev/null | head -n 1 || echo unknown)"
} > "${VERSIONS_TXT}"

detect_runtime_toolchain_gaps
if [[ "${#MISSING_RUNTIME_TOOLS[@]}" -gt 0 ]]; then
  emit_skip_and_exit
fi

: > "${STEP_RESULTS_TSV}"
: > "${COMMAND_MANIFEST}"

run_step() {
  local step_id="$1"
  shift
  local stdout_log="${LOG_DIR}/${step_id}.stdout.log"
  local stderr_log="${LOG_DIR}/${step_id}.stderr.log"

  printf '[%s] %s\n' "${step_id}" "$*" >> "${COMMAND_MANIFEST}"

  local start_epoch
  start_epoch="$(date +%s)"

  set +e
  "$@" > "${stdout_log}" 2> "${stderr_log}"
  local exit_code=$?
  set -e

  local end_epoch
  end_epoch="$(date +%s)"
  local duration_seconds=$((end_epoch - start_epoch))

  printf '%s\t%s\t%s\t%s\t%s\n' \
    "${step_id}" "${exit_code}" "${duration_seconds}" "${stdout_log}" "${stderr_log}" >> "${STEP_RESULTS_TSV}"

  if [[ "${exit_code}" -ne 0 ]]; then
    echo "[e2e] step failed: ${step_id} (exit=${exit_code})" >&2
    echo "[e2e] stderr log: ${stderr_log}" >&2
    exit "${exit_code}"
  fi
}

run_step build_doctor cargo build -p doctor_franktentui

TARGET_DIR="$(cargo metadata --format-version=1 --no-deps | jq -r '.target_directory')"
BIN_PATH="${TARGET_DIR}/debug/doctor_franktentui"
if [[ ! -x "${BIN_PATH}" ]]; then
  echo "[e2e] expected binary not found: ${BIN_PATH}" >&2
  exit 2
fi

{
  echo "target_dir=${TARGET_DIR}"
  echo "bin_path=${BIN_PATH}"
  echo "doctor_version=$(${BIN_PATH} --version 2>/dev/null || echo unknown)"
} >> "${VERSIONS_TXT}"

run_step doctor_full \
  "${BIN_PATH}" doctor \
  --project-dir "${PROJECT_DIR}" \
  --run-root "${RUN_ROOT}/doctor" \
  --app-command "echo demo" \
  --full

run_step capture_happy \
  "${BIN_PATH}" capture \
  --profile analytics-empty \
  --project-dir "${PROJECT_DIR}" \
  --run-root "${RUN_ROOT}/captures" \
  --run-name happy_capture \
  --app-command "echo demo"

run_step suite_happy \
  "${BIN_PATH}" suite \
  --profiles analytics-empty,messages-seeded \
  --project-dir "${PROJECT_DIR}" \
  --run-root "${RUN_ROOT}/suites" \
  --suite-name happy_suite \
  --app-command "echo demo"

run_step report_happy \
  "${BIN_PATH}" report \
  --suite-dir "${RUN_ROOT}/suites/happy_suite" \
  --output-json "${RUN_ROOT}/suites/happy_suite/custom_report.json" \
  --output-html "${RUN_ROOT}/suites/happy_suite/custom_report.html" \
  --title "doctor_franktentui happy e2e"

python3 - \
  "${STEP_RESULTS_TSV}" \
  "${COMMAND_MANIFEST}" \
  "${ENV_SNAPSHOT}" \
  "${RUN_ROOT}" \
  "${LOG_DIR}" \
  "${ARTIFACT_MANIFEST_JSON}" \
  "${EVENTS_JSONL}" \
  "${EVENT_VALIDATION_REPORT_JSON}" \
  "${SUMMARY_JSON}" \
  "${SUMMARY_TXT}" \
  "${ROOT_DIR}/scripts/doctor_franktentui_validate_jsonl.py" <<'PY'
from __future__ import annotations

import hashlib
import json
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

step_results_tsv = Path(sys.argv[1])
command_manifest = Path(sys.argv[2])
env_snapshot = Path(sys.argv[3])
run_root = Path(sys.argv[4])
log_dir = Path(sys.argv[5])
artifact_manifest_json = Path(sys.argv[6])
events_jsonl = Path(sys.argv[7])
event_validation_report_json = Path(sys.argv[8])
summary_json = Path(sys.argv[9])
summary_txt = Path(sys.argv[10])
validator_script = Path(sys.argv[11])


def now_utc_timestamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def sha256_or_none(path: Path) -> str | None:
    if path.exists():
        return sha256_file(path)
    return None

steps = []
for line in step_results_tsv.read_text().splitlines():
    step_id, exit_code, duration_seconds, stdout_log, stderr_log = line.split("\t")
    steps.append(
        {
            "step_id": step_id,
            "exit_code": int(exit_code),
            "duration_seconds": int(duration_seconds),
            "stdout_log": stdout_log,
            "stderr_log": stderr_log,
        }
    )

step_command = {}
line_pattern = re.compile(r"^\[(?P<step>[^\]]+)\]\s+(?P<command>.+)$")
for raw in command_manifest.read_text(encoding="utf-8").splitlines():
    match = line_pattern.match(raw.strip())
    if not match:
        continue
    step_command[match.group("step")] = match.group("command")

artifact_paths = [
    run_root / "doctor" / "doctor_dry_run" / "run_meta.json",
    run_root / "doctor" / "doctor_dry_run" / "run_summary.txt",
    run_root / "doctor" / "doctor_full_run" / "run_meta.json",
    run_root / "doctor" / "doctor_full_run" / "run_summary.txt",
    run_root / "captures" / "happy_capture" / "run_meta.json",
    run_root / "captures" / "happy_capture" / "run_summary.txt",
    run_root / "captures" / "happy_capture" / "evidence_ledger.jsonl",
    run_root / "suites" / "happy_suite" / "suite_summary.txt",
    run_root / "suites" / "happy_suite" / "suite_manifest.json",
    run_root / "suites" / "happy_suite" / "report.json",
    run_root / "suites" / "happy_suite" / "index.html",
    run_root / "suites" / "happy_suite" / "custom_report.json",
    run_root / "suites" / "happy_suite" / "custom_report.html",
]

artifacts = []
missing = []
for path in artifact_paths:
    if not path.exists():
        missing.append(str(path))
        continue

    data = path.read_bytes()
    sha256 = hashlib.sha256(data).hexdigest()
    stat = path.stat()
    artifacts.append(
        {
            "path": str(path),
            "size_bytes": stat.st_size,
            "mtime_epoch": int(stat.st_mtime),
            "sha256": sha256,
        }
    )

artifact_manifest = {
    "run_root": str(run_root),
    "artifact_count": len(artifacts),
    "missing_count": len(missing),
    "artifacts": artifacts,
    "missing": missing,
}
artifact_manifest_json.write_text(json.dumps(artifact_manifest, indent=2) + "\n")

run_id = f"happy-{run_root.name}"
env_hash = sha256_file(env_snapshot)
correlation_index = 0


def next_correlation_id() -> str:
    global correlation_index
    correlation_index += 1
    return f"{run_id}-corr-{correlation_index:04d}"


events: list[dict] = []

events.append(
    {
        "schema_version": "1.0.0",
        "timestamp_utc": now_utc_timestamp(),
        "run_id": run_id,
        "correlation_id": next_correlation_id(),
        "case_id": None,
        "step_id": None,
        "event_type": "run_start",
        "command": "doctor_franktentui_happy_e2e",
        "env_hash": env_hash,
        "duration_ms": 0,
        "exit_code": 0,
        "stdout_sha256": None,
        "stderr_sha256": None,
        "artifact_hashes": {},
        "expected": {},
        "actual": {"status": "started"},
    }
)

for step in steps:
    stdout_log = Path(step["stdout_log"])
    stderr_log = Path(step["stderr_log"])
    command = step_command.get(step["step_id"], "")

    events.append(
        {
            "schema_version": "1.0.0",
            "timestamp_utc": now_utc_timestamp(),
            "run_id": run_id,
            "correlation_id": next_correlation_id(),
            "case_id": None,
            "step_id": step["step_id"],
            "event_type": "step_start",
            "command": command,
            "env_hash": env_hash,
            "duration_ms": 0,
            "exit_code": 0,
            "stdout_sha256": None,
            "stderr_sha256": None,
            "artifact_hashes": {},
            "expected": {},
            "actual": {},
        }
    )

    events.append(
        {
            "schema_version": "1.0.0",
            "timestamp_utc": now_utc_timestamp(),
            "run_id": run_id,
            "correlation_id": next_correlation_id(),
            "case_id": None,
            "step_id": step["step_id"],
            "event_type": "step_end",
            "command": command,
            "env_hash": env_hash,
            "duration_ms": int(step["duration_seconds"]) * 1000,
            "exit_code": int(step["exit_code"]),
            "stdout_sha256": sha256_or_none(stdout_log),
            "stderr_sha256": sha256_or_none(stderr_log),
            "artifact_hashes": {},
            "expected": {"exit_code": 0},
            "actual": {"exit_code": int(step["exit_code"])},
        }
    )

artifact_event_paths = []
for artifact in artifacts:
    artifact_path = artifact["path"]
    artifact_event_paths.append(artifact_path)
    events.append(
        {
            "schema_version": "1.0.0",
            "timestamp_utc": now_utc_timestamp(),
            "run_id": run_id,
            "correlation_id": next_correlation_id(),
            "case_id": None,
            "step_id": None,
            "event_type": "artifact",
            "command": "artifact_manifest",
            "env_hash": env_hash,
            "duration_ms": 0,
            "exit_code": 0,
            "stdout_sha256": None,
            "stderr_sha256": None,
            "artifact_hashes": {artifact_path: artifact["sha256"]},
            "expected": {},
            "actual": {"size_bytes": artifact["size_bytes"]},
        }
    )

artifact_manifest_paths = [artifact["path"] for artifact in artifacts]
artifact_manifest_set = set(artifact_manifest_paths)
artifact_event_set = set(artifact_event_paths)
artifact_mismatch_errors = []
if artifact_manifest_set != artifact_event_set:
    missing_event_paths = sorted(artifact_manifest_set - artifact_event_set)
    extra_event_paths = sorted(artifact_event_set - artifact_manifest_set)
    artifact_mismatch_errors.append(
        {
            "message": "artifact event set mismatch",
            "missing_event_paths": missing_event_paths,
            "extra_event_paths": extra_event_paths,
        }
    )

status_before_validation = (
    "passed"
    if all(step["exit_code"] == 0 for step in steps)
    and not missing
    and not artifact_mismatch_errors
    else "failed"
)

events.append(
    {
        "schema_version": "1.0.0",
        "timestamp_utc": now_utc_timestamp(),
        "run_id": run_id,
        "correlation_id": next_correlation_id(),
        "case_id": None,
        "step_id": None,
        "event_type": "run_end",
        "command": "doctor_franktentui_happy_e2e",
        "env_hash": env_hash,
        "duration_ms": 0,
        "exit_code": 0 if status_before_validation == "passed" else 1,
        "stdout_sha256": None,
        "stderr_sha256": None,
        "artifact_hashes": {},
        "expected": {"status": "passed"},
        "actual": {
            "status": status_before_validation,
            "step_count": len(steps),
            "artifact_count": len(artifacts),
            "missing_artifact_count": len(missing),
        },
    }
)

events_jsonl.write_text(
    "".join(json.dumps(event, separators=(",", ":")) + "\n" for event in events),
    encoding="utf-8",
)

validator_stdout_log = log_dir / "validate_happy_jsonl.stdout.log"
validator_stderr_log = log_dir / "validate_happy_jsonl.stderr.log"
validator_start = time.monotonic()

validator_command = [
    str(validator_script),
    "--input",
    str(events_jsonl),
    "--workflow",
    "happy",
    "--report-json",
    str(event_validation_report_json),
]
validator_proc = subprocess.run(validator_command, capture_output=True, text=True)
validator_duration_ms = int((time.monotonic() - validator_start) * 1000)
validator_stdout_log.write_text(validator_proc.stdout, encoding="utf-8")
validator_stderr_log.write_text(validator_proc.stderr, encoding="utf-8")

steps.append(
    {
        "step_id": "validate_happy_jsonl",
        "exit_code": int(validator_proc.returncode),
        "duration_seconds": int(round(validator_duration_ms / 1000.0)),
        "stdout_log": str(validator_stdout_log),
        "stderr_log": str(validator_stderr_log),
    }
)

status = (
    "passed"
    if all(step["exit_code"] == 0 for step in steps)
    and not missing
    and not artifact_mismatch_errors
    and validator_proc.returncode == 0
    else "failed"
)

summary = {
    "status": status,
    "run_root": str(run_root),
    "steps": steps,
    "artifact_manifest": str(artifact_manifest_json),
    "missing_artifacts": missing,
    "events_jsonl": str(events_jsonl),
    "events_validation_report": str(event_validation_report_json),
    "artifact_event_crosscheck_errors": artifact_mismatch_errors,
}
summary_json.write_text(json.dumps(summary, indent=2) + "\n")

lines = []
lines.append(f"status={status}")
lines.append(f"run_root={run_root}")
lines.append("steps:")
for step in steps:
    lines.append(
        f"- {step['step_id']}: exit={step['exit_code']} duration_s={step['duration_seconds']}"
    )
lines.append(f"artifact_manifest={artifact_manifest_json}")
lines.append(f"events_jsonl={events_jsonl}")
lines.append(f"events_validation_report={event_validation_report_json}")
if missing:
    lines.append("missing_artifacts:")
    for item in missing:
        lines.append(f"- {item}")
if artifact_mismatch_errors:
    lines.append("artifact_event_crosscheck_errors:")
    for item in artifact_mismatch_errors:
        lines.append(f"- {item['message']}")
summary_txt.write_text("\n".join(lines) + "\n")

print("\n".join(lines))

if status != "passed":
    sys.exit(1)
PY

echo "[e2e] PASS doctor_franktentui happy workflow"
echo "[e2e] run_root=${RUN_ROOT}"
echo "[e2e] summary_json=${SUMMARY_JSON}"

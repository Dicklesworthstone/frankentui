#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_franktentui/e2e/happy_${TIMESTAMP_UTC}}"
PROJECT_DIR="${RUN_ROOT}/project"
TOOLS_DIR="${RUN_ROOT}/tools"
LOG_DIR="${RUN_ROOT}/logs"
META_DIR="${RUN_ROOT}/meta"
STEP_RESULTS_TSV="${META_DIR}/step_results.tsv"
COMMAND_MANIFEST="${META_DIR}/command_manifest.txt"
ENV_SNAPSHOT="${META_DIR}/env_snapshot.txt"
VERSIONS_TXT="${META_DIR}/tool_versions.txt"
ARTIFACT_MANIFEST_JSON="${META_DIR}/artifact_manifest.json"
SUMMARY_JSON="${META_DIR}/summary.json"
SUMMARY_TXT="${META_DIR}/summary.txt"
TARGET_DIR=""
BIN_PATH=""

mkdir -p "${PROJECT_DIR}" "${TOOLS_DIR}" "${LOG_DIR}" "${META_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[e2e] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

require_command "cargo" "install Rust/Cargo toolchain"
require_command "jq" "install jq for JSON parsing"
require_command "python3" "install Python 3"

cat > "${TOOLS_DIR}/vhs" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
tape_path="${1:-}"
if [[ -n "$tape_path" && -f "$tape_path" ]]; then
  output_path="$(grep -E '^Output "' "$tape_path" | sed -E 's/^Output "(.*)"$/\1/' | head -n 1 || true)"
  if [[ -n "$output_path" ]]; then
    mkdir -p "$(dirname "$output_path")"
    : > "$output_path"
  fi
fi
exit 0
SH
chmod +x "${TOOLS_DIR}/vhs"

cat > "${TOOLS_DIR}/ffmpeg" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
out="${@: -1}"
mkdir -p "$(dirname "$out")"
: > "$out"
exit 0
SH
chmod +x "${TOOLS_DIR}/ffmpeg"

cat > "${TOOLS_DIR}/ffprobe" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "1.0"
exit 0
SH
chmod +x "${TOOLS_DIR}/ffprobe"

export PATH="${TOOLS_DIR}:${PATH}"

{
  echo "timestamp_utc=${TIMESTAMP_UTC}"
  echo "run_root=${RUN_ROOT}"
  echo "root_dir=${ROOT_DIR}"
  echo "project_dir=${PROJECT_DIR}"
  echo "tools_dir=${TOOLS_DIR}"
} > "${META_DIR}/session.txt"

{
  env | sort | grep -E '^(CI|TERM|SHELL|USER|HOME|PATH|RUSTUP_TOOLCHAIN|SQLMODEL_|FASTAPI_)=' || true
} > "${ENV_SNAPSHOT}"

{
  echo "doctor_franktentui_happy_e2e"
  echo "timestamp_utc=${TIMESTAMP_UTC}"
  echo "git_rev=$(git -C "${ROOT_DIR}" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "cargo_version=$(cargo --version)"
  echo "rustc_version=$(rustc --version)"
  echo "doctor_version_prebuild=unknown"
  echo "vhs_version=$("${TOOLS_DIR}/vhs" --version 2>/dev/null || echo fake-vhs)"
} > "${VERSIONS_TXT}"

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

python3 - "${STEP_RESULTS_TSV}" "${RUN_ROOT}" "${ARTIFACT_MANIFEST_JSON}" "${SUMMARY_JSON}" "${SUMMARY_TXT}" <<'PY'
import hashlib
import json
import os
import sys
from pathlib import Path

step_results_tsv = Path(sys.argv[1])
run_root = Path(sys.argv[2])
artifact_manifest_json = Path(sys.argv[3])
summary_json = Path(sys.argv[4])
summary_txt = Path(sys.argv[5])

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

status = "passed" if all(step["exit_code"] == 0 for step in steps) and not missing else "failed"
summary = {
    "status": status,
    "run_root": str(run_root),
    "steps": steps,
    "artifact_manifest": str(artifact_manifest_json),
    "missing_artifacts": missing,
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
if missing:
    lines.append("missing_artifacts:")
    for item in missing:
        lines.append(f"- {item}")
summary_txt.write_text("\n".join(lines) + "\n")

print("\n".join(lines))

if status != "passed":
    sys.exit(1)
PY

echo "[e2e] PASS doctor_franktentui happy workflow"
echo "[e2e] run_root=${RUN_ROOT}"
echo "[e2e] summary_json=${SUMMARY_JSON}"

#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_franktentui/e2e/failure_${TIMESTAMP_UTC}}"
CASES_DIR="${RUN_ROOT}/cases"
LOG_DIR="${RUN_ROOT}/logs"
META_DIR="${RUN_ROOT}/meta"
CASE_RESULTS_TSV="${META_DIR}/case_results.tsv"
CASE_RESULTS_JSON="${META_DIR}/case_results.json"
SUMMARY_JSON="${META_DIR}/summary.json"
SUMMARY_TXT="${META_DIR}/summary.txt"
EVENTS_JSONL="${META_DIR}/events.jsonl"
EVENT_VALIDATION_REPORT_JSON="${META_DIR}/events_validation_report.json"
COMMAND_MANIFEST="${META_DIR}/command_manifest.txt"
ENV_SNAPSHOT="${META_DIR}/env_snapshot.txt"
VERSIONS_TXT="${META_DIR}/tool_versions.txt"
TARGET_DIR=""
BIN_PATH=""
MISSING_RUNTIME_TOOLS=()

mkdir -p "${CASES_DIR}" "${LOG_DIR}" "${META_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[e2e-failure] missing required command: ${command} (${hint})" >&2
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
  local reason="missing runtime tools required for real-behavior e2e failure matrix"

  : > "${CASE_RESULTS_TSV}"
  : > "${COMMAND_MANIFEST}"
  : > "${EVENTS_JSONL}"

  jq -n \
    --argjson missing_tools "${missing_json}" \
    '{cases: [], skipped: true, missing_tools: $missing_tools}' > "${CASE_RESULTS_JSON}"

  jq -n \
    --arg status "skipped" \
    --arg reason "${reason}" \
    --arg run_root "${RUN_ROOT}" \
    --arg case_results "${CASE_RESULTS_JSON}" \
    --arg events_jsonl "${EVENTS_JSONL}" \
    --arg events_validation_report "${EVENT_VALIDATION_REPORT_JSON}" \
    --argjson missing_tools "${missing_json}" \
    '{
      status: $status,
      reason: $reason,
      run_root: $run_root,
      total_cases: 0,
      passed_cases: 0,
      failed_cases: 0,
      case_results: $case_results,
      events_jsonl: $events_jsonl,
      events_validation_report: $events_validation_report,
      missing_tools: $missing_tools
    }' > "${SUMMARY_JSON}"

  jq -n \
    --arg status "skipped" \
    --argjson missing_tools "${missing_json}" \
    '{
      status: $status,
      errors: [],
      total_events: 0,
      workflow: "failure",
      missing_tools: $missing_tools
    }' > "${EVENT_VALIDATION_REPORT_JSON}"

  {
    echo "status=skipped"
    echo "reason=${reason}"
    echo "run_root=${RUN_ROOT}"
    echo "total_cases=0"
    echo "passed_cases=0"
    echo "failed_cases=0"
    echo "case_results=${CASE_RESULTS_JSON}"
    echo "events_jsonl=${EVENTS_JSONL}"
    echo "events_validation_report=${EVENT_VALIDATION_REPORT_JSON}"
    echo "missing_tools=$(IFS=,; echo "${MISSING_RUNTIME_TOOLS[*]}")"
  } > "${SUMMARY_TXT}"

  cat "${SUMMARY_TXT}"
  exit 0
}

require_command "cargo" "install Rust/Cargo toolchain"
require_command "jq" "install jq for JSON parsing"
require_command "rg" "install ripgrep for regex checks"
require_command "python3" "install Python 3"

{
  env | sort | grep -E '^(CI|TERM|SHELL|USER|HOME|PATH|RUSTUP_TOOLCHAIN|SQLMODEL_|FASTAPI_)=' || true
} > "${ENV_SNAPSHOT}"

{
  echo "doctor_franktentui_failure_e2e"
  echo "timestamp_utc=${TIMESTAMP_UTC}"
  echo "git_rev=$(git -C "${ROOT_DIR}" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "cargo_version=$(cargo --version)"
  echo "rustc_version=$(rustc --version 2>/dev/null || echo rustc-missing)"
  echo "vhs_path=$(command -v vhs 2>/dev/null || echo missing)"
  echo "ffmpeg_path=$(command -v ffmpeg 2>/dev/null || echo missing)"
  echo "ffprobe_path=$(command -v ffprobe 2>/dev/null || echo missing)"
  echo "vhs_version=$(vhs --version 2>/dev/null | head -n 1 || echo unknown)"
  echo "ffmpeg_version=$(ffmpeg -version 2>/dev/null | head -n 1 || echo unknown)"
  echo "ffprobe_version=$(ffprobe -version 2>/dev/null | head -n 1 || echo unknown)"
} > "${VERSIONS_TXT}"

: > "${CASE_RESULTS_TSV}"
: > "${COMMAND_MANIFEST}"

detect_runtime_toolchain_gaps
if [[ "${#MISSING_RUNTIME_TOOLS[@]}" -gt 0 ]]; then
  emit_skip_and_exit
fi

run_build() {
  local stdout_log="${LOG_DIR}/build_doctor.stdout.log"
  local stderr_log="${LOG_DIR}/build_doctor.stderr.log"

  printf '[build_doctor] cargo build -p doctor_franktentui\n' >> "${COMMAND_MANIFEST}"
  cargo build -p doctor_franktentui > "${stdout_log}" 2> "${stderr_log}"

  TARGET_DIR="$(cargo metadata --format-version=1 --no-deps | jq -r '.target_directory')"
  BIN_PATH="${TARGET_DIR}/debug/doctor_franktentui"
  if [[ ! -x "${BIN_PATH}" ]]; then
    echo "[e2e-failure] expected binary not found: ${BIN_PATH}" >&2
    exit 2
  fi

  {
    echo "target_dir=${TARGET_DIR}"
    echo "bin_path=${BIN_PATH}"
    echo "doctor_version=$(${BIN_PATH} --version 2>/dev/null || echo unknown)"
  } >> "${VERSIONS_TXT}"
}

json_validate_stdout() {
  local stdout_log="$1"
  python3 - "$stdout_log" <<'PY'
import json
import sys

path = sys.argv[1]
lines = [line.strip() for line in open(path, encoding='utf-8').read().splitlines() if line.strip()]
if len(lines) != 1:
    raise SystemExit(1)
value = json.loads(lines[0])
if not isinstance(value, dict):
    raise SystemExit(1)
PY
}

append_artifact() {
  local artifact_list="$1"
  shift
  for path in "$@"; do
    if [[ -n "$path" ]]; then
      printf '%s\n' "$path" >> "$artifact_list"
    fi
  done
}

run_case() {
  local case_id="$1"
  local expected_exit="$2"
  local expected_regex="$3"
  local expect_json="$4"
  local match_extra_files="$5"
  shift 5

  local case_root="${CASES_DIR}/${case_id}"
  local case_logs="${case_root}/logs"
  local case_meta="${case_root}/meta"
  local stdout_log="${case_logs}/${case_id}.stdout.log"
  local stderr_log="${case_logs}/${case_id}.stderr.log"
  local command_file="${case_meta}/command.txt"
  local case_env_snapshot="${case_meta}/env_snapshot.txt"
  local artifact_list="${case_meta}/key_artifacts.txt"

  mkdir -p "${case_logs}" "${case_meta}"
  : > "${artifact_list}"

  printf '[%s] ' "$case_id" >> "${COMMAND_MANIFEST}"
  printf '%q ' "$@" > "${command_file}"
  cat "${command_file}" >> "${COMMAND_MANIFEST}"
  printf '\n' >> "${COMMAND_MANIFEST}"

  {
    echo "case_id=${case_id}"
    echo "expected_exit=${expected_exit}"
    echo "expected_regex=${expected_regex}"
    env | sort | grep -E '^(CI|TERM|SHELL|USER|HOME|PATH|RUSTUP_TOOLCHAIN|SQLMODEL_|FASTAPI_)=' || true
  } > "${case_env_snapshot}"

  local start_epoch
  start_epoch="$(date +%s)"

  set +e
  "$@" > "${stdout_log}" 2> "${stderr_log}"
  local actual_exit=$?
  set -e

  local end_epoch
  end_epoch="$(date +%s)"
  local duration_seconds=$((end_epoch - start_epoch))

  local regex_matched=0
  local search_files=("${stdout_log}" "${stderr_log}")

  if [[ -n "${match_extra_files}" ]]; then
    IFS=',' read -r -a extras <<< "${match_extra_files}"
    for extra in "${extras[@]}"; do
      if [[ -n "${extra}" && -e "${extra}" ]]; then
        search_files+=("${extra}")
      fi
    done
  fi

  if rg -n -- "${expected_regex}" "${search_files[@]}" > /dev/null 2>&1; then
    regex_matched=1
  fi

  local json_valid=1
  if [[ "${expect_json}" -eq 1 ]]; then
    json_valid=0
    if json_validate_stdout "${stdout_log}"; then
      json_valid=1
    fi
  fi

  local pass=0
  if [[ "${actual_exit}" -eq "${expected_exit}" && "${regex_matched}" -eq 1 && "${json_valid}" -eq 1 ]]; then
    pass=1
  fi

  append_artifact "${artifact_list}" \
    "${stdout_log}" \
    "${stderr_log}" \
    "${command_file}" \
    "${case_env_snapshot}"

  if [[ -n "${match_extra_files}" ]]; then
    IFS=',' read -r -a extras <<< "${match_extra_files}"
    append_artifact "${artifact_list}" "${extras[@]}"
  fi

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${case_id}" \
    "${expected_exit}" \
    "${actual_exit}" \
    "${pass}" \
    "${regex_matched}" \
    "${expected_regex}" \
    "${expect_json}" \
    "${json_valid}" \
    "${duration_seconds}" \
    "${stdout_log}" \
    "${stderr_log}" \
    "${case_env_snapshot}" \
    "${artifact_list}" >> "${CASE_RESULTS_TSV}"

  if [[ "${pass}" -eq 1 ]]; then
    echo "[case] PASS ${case_id} (exit=${actual_exit})"
  else
    echo "[case] FAIL ${case_id} (expected_exit=${expected_exit}, actual_exit=${actual_exit}, regex_matched=${regex_matched}, json_valid=${json_valid})"
  fi
}

prepare_seed_retry_wrapper() {
  local case_root="$1"
  local mode="$2"
  local server_script="${case_root}/server.py"
  local runner_script="${case_root}/run_case.sh"
  local server_stdout="${case_root}/logs/server.stdout.log"
  local server_stderr="${case_root}/logs/server.stderr.log"
  local seed_log="${case_root}/logs/seed_rpc.log"
  local port_file="${case_root}/logs/server.port"

  cat > "${server_script}" <<PY
#!/usr/bin/env python3
import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

MODE = ${mode@Q}
PORT_FILE = ${port_file@Q}

class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        payload_raw = self.rfile.read(length)
        method_name = "unknown"
        request_id = 0

        try:
            payload = json.loads(payload_raw.decode("utf-8"))
            request_id = payload.get("id", 0)
            method_name = payload.get("params", {}).get("name", "unknown")
        except Exception:
            pass

        if method_name == "health_check":
            body = json.dumps({"jsonrpc": "2.0", "id": request_id, "result": {"status": "ok"}}).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        if MODE == "always_success":
            body = json.dumps({"jsonrpc": "2.0", "id": request_id, "result": {"ok": True, "method": method_name}}).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        body = json.dumps({"id": request_id, "result": {"mode": "non_jsonrpc"}}).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        return

server = HTTPServer(("127.0.0.1", 0), Handler)
with open(PORT_FILE, "w", encoding="utf-8") as handle:
    handle.write(str(server.server_address[1]))
    handle.flush()
    os.fsync(handle.fileno())
server.serve_forever()
PY
  chmod +x "${server_script}"

  cat > "${runner_script}" <<SHRUN
#!/usr/bin/env bash
set -euo pipefail
python3 "${server_script}" > "${server_stdout}" 2> "${server_stderr}" &
server_pid=\$!
cleanup() {
  kill "\${server_pid}" >/dev/null 2>&1 || true
  wait "\${server_pid}" 2>/dev/null || true
}
trap cleanup EXIT
port=""
for _ in \$(seq 1 100); do
  if [[ -s "${port_file}" ]]; then
    port="\$(cat "${port_file}")"
    break
  fi
  sleep 0.05
done

if [[ -z "\${port}" ]]; then
  echo "[seed-wrapper] server port was not published" >&2
  exit 97
fi

ready=0
for _ in \$(seq 1 100); do
  if python3 - "\${port}" <<'PY'
import socket
import sys

sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.settimeout(0.2)
try:
    sock.connect(("127.0.0.1", int(sys.argv[1])))
except OSError:
    raise SystemExit(1)
finally:
    sock.close()
raise SystemExit(0)
PY
  then
    ready=1
    break
  fi
  sleep 0.05
done

if [[ "\${ready}" -ne 1 ]]; then
  echo "[seed-wrapper] server did not become ready on port \${port}" >&2
  exit 98
fi

"${BIN_PATH}" seed-demo \
  --host 127.0.0.1 \
  --port "\${port}" \
  --path /mcp/ \
  --timeout 5 \
  --project-key "${case_root}/project" \
  --messages 2 \
  --log-file "${seed_log}"
SHRUN
  chmod +x "${runner_script}"

  printf '%s\n' "${seed_log},${server_stdout},${server_stderr},${port_file}"
}

run_build

# Case 1: invalid profile.
case_id="invalid_profile"
case_root="${CASES_DIR}/${case_id}"
mkdir -p "${case_root}/project"
run_case \
  "${case_id}" \
  1 \
  "profile not found: definitely-not-a-profile" \
  0 \
  "" \
  "${BIN_PATH}" capture \
  --profile definitely-not-a-profile \
  --project-dir "${case_root}/project" \
  --run-root "${case_root}/runs"

# Case 2: incompatible flags.
case_id="seed_required_without_seed_demo"
case_root="${CASES_DIR}/${case_id}"
mkdir -p "${case_root}/project"
run_case \
  "${case_id}" \
  1 \
  "--seed-required requires demo seeding to be enabled" \
  0 \
  "" \
  "${BIN_PATH}" capture \
  --profile analytics-empty \
  --project-dir "${case_root}/project" \
  --run-root "${case_root}/runs" \
  --seed-required \
  --no-seed-demo

# Case 3: missing legacy binary.
case_id="missing_legacy_binary"
case_root="${CASES_DIR}/${case_id}"
mkdir -p "${case_root}/project"
run_case \
  "${case_id}" \
  1 \
  "required path does not exist: .*missing-demo-binary" \
  0 \
  "" \
  "${BIN_PATH}" capture \
  --profile analytics-empty \
  --project-dir "${case_root}/project" \
  --run-root "${case_root}/runs" \
  --binary "${case_root}/missing-demo-binary" \
  --host 127.0.0.1

# Case 4: suite surfaces report generation failure.
case_id="suite_report_failure"
case_root="${CASES_DIR}/${case_id}"
suite_name="broken_suite"
mkdir -p "${case_root}/project"
mkdir -p "${case_root}/suites/${suite_name}/report.json"
mkdir -p "${case_root}/suites/${suite_name}/index.html"
run_case \
  "${case_id}" \
  1 \
  "suite report generation failed" \
  0 \
  "${case_root}/suites/${suite_name}/suite_report.log" \
  "${BIN_PATH}" suite \
  --profiles analytics-empty \
  --project-dir "${case_root}/project" \
  --run-root "${case_root}/suites" \
  --suite-name "${suite_name}" \
  --app-command "echo demo"

# Case 5: seed timeout boundary.
case_id="seed_timeout_boundary"
case_root="${CASES_DIR}/${case_id}"
mkdir -p "${case_root}/logs"
: > "${case_root}/logs/seed_timeout.log"
run_case \
  "${case_id}" \
  1 \
  "Timed out waiting for server" \
  0 \
  "${case_root}/logs/seed_timeout.log" \
  "${BIN_PATH}" seed-demo \
  --host 127.0.0.1 \
  --port 1 \
  --timeout 1 \
  --project-key "${case_root}/project" \
  --messages 1 \
  --log-file "${case_root}/logs/seed_timeout.log"

# Case 6: seed retry boundary (non-JSON response after health check).
case_id="seed_retry_non_json"
case_root="${CASES_DIR}/${case_id}"
mkdir -p "${case_root}/logs"
extra_files="$(prepare_seed_retry_wrapper "${case_root}" "always_non_json")"
run_case \
  "${case_id}" \
  1 \
  "RPC non-JSON-RPC response for ensure_project|retry method=ensure_project attempt=2" \
  0 \
  "${extra_files}" \
  "${case_root}/run_case.sh"

# JSON mode contracts across commands.
case_id="json_doctor_contract"
case_root="${CASES_DIR}/${case_id}"
mkdir -p "${case_root}/project"
run_case \
  "${case_id}" \
  0 \
  '"command":"doctor"' \
  1 \
  "" \
  env SQLMODEL_JSON=1 "${BIN_PATH}" doctor \
  --project-dir "${case_root}/project" \
  --run-root "${case_root}/doctor" \
  --app-command "echo demo"

case_id="json_capture_contract"
case_root="${CASES_DIR}/${case_id}"
mkdir -p "${case_root}/project"
run_case \
  "${case_id}" \
  0 \
  '"command":"capture"' \
  1 \
  "" \
  env SQLMODEL_JSON=1 "${BIN_PATH}" capture \
  --profile analytics-empty \
  --project-dir "${case_root}/project" \
  --run-root "${case_root}/runs" \
  --run-name json_capture \
  --app-command "echo demo" \
  --dry-run

case_id="json_suite_contract"
case_root="${CASES_DIR}/${case_id}"
suite_name="json_suite"
mkdir -p "${case_root}/project"
mkdir -p "${case_root}/suites/${suite_name}/report.json"
mkdir -p "${case_root}/suites/${suite_name}/index.html"
run_case \
  "${case_id}" \
  1 \
  '"command":"suite"' \
  1 \
  "${case_root}/suites/${suite_name}/suite_report.log" \
  env SQLMODEL_JSON=1 "${BIN_PATH}" suite \
  --profiles analytics-empty \
  --project-dir "${case_root}/project" \
  --run-root "${case_root}/suites" \
  --suite-name "${suite_name}" \
  --app-command "echo demo"

case_id="json_report_contract"
case_root="${CASES_DIR}/${case_id}"
mkdir -p "${case_root}/suite/run_a"
cat > "${case_root}/suite/run_a/run_meta.json" <<'RUNMETA'
{
  "status": "ok",
  "started_at": "2026-02-17T00:00:00Z",
  "profile": "analytics-empty",
  "output": "/tmp/capture.mp4",
  "run_dir": "/tmp/run-a"
}
RUNMETA
run_case \
  "${case_id}" \
  0 \
  '"command":"report"' \
  1 \
  "" \
  env SQLMODEL_JSON=1 "${BIN_PATH}" report \
  --suite-dir "${case_root}/suite" \
  --title "json report contract"

case_id="json_seed_contract"
case_root="${CASES_DIR}/${case_id}"
mkdir -p "${case_root}/logs"
extra_files="$(prepare_seed_retry_wrapper "${case_root}" "always_success")"
run_case \
  "${case_id}" \
  0 \
  '"command":"seed-demo"' \
  1 \
  "${extra_files}" \
  env SQLMODEL_JSON=1 "${case_root}/run_case.sh"

python3 - \
  "${CASE_RESULTS_TSV}" \
  "${CASE_RESULTS_JSON}" \
  "${SUMMARY_JSON}" \
  "${SUMMARY_TXT}" \
  "${RUN_ROOT}" \
  "${EVENTS_JSONL}" \
  "${EVENT_VALIDATION_REPORT_JSON}" \
  "${LOG_DIR}" \
  "${ROOT_DIR}/scripts/doctor_franktentui_validate_jsonl.py" \
  "${ENV_SNAPSHOT}" <<'PY'
from __future__ import annotations

import hashlib
import json
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

rows_path = Path(sys.argv[1])
case_results_json = Path(sys.argv[2])
summary_json = Path(sys.argv[3])
summary_txt = Path(sys.argv[4])
run_root = Path(sys.argv[5])
events_jsonl = Path(sys.argv[6])
event_validation_report_json = Path(sys.argv[7])
log_dir = Path(sys.argv[8])
validator_script = Path(sys.argv[9])
global_env_snapshot = Path(sys.argv[10])


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def sha256_or_none(path: Path) -> str | None:
    if path.exists():
        return sha256_file(path)
    return None


def now_utc_timestamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


rows = []
for line in rows_path.read_text(encoding="utf-8").splitlines():
    if not line.strip():
        continue
    (
        case_id,
        expected_exit,
        actual_exit,
        pass_flag,
        regex_matched,
        expected_regex,
        expect_json,
        json_valid,
        duration_seconds,
        stdout_log,
        stderr_log,
        env_snapshot,
        artifact_list,
    ) = line.split("\t")

    key_artifacts = []
    missing_artifacts = []
    artifact_hashes = {}
    artifact_file = Path(artifact_list)
    if artifact_file.exists():
        for raw_path in artifact_file.read_text(encoding="utf-8").splitlines():
            path = raw_path.strip()
            if not path:
                continue
            key_artifacts.append(path)
            artifact_path = Path(path)
            if artifact_path.exists():
                artifact_hashes[path] = sha256_file(artifact_path)
            else:
                missing_artifacts.append(path)
    else:
        missing_artifacts.append(str(artifact_file))

    base_pass = pass_flag == "1"
    case_pass = base_pass and not missing_artifacts

    rows.append(
        {
            "case_id": case_id,
            "expected_exit": int(expected_exit),
            "actual_exit": int(actual_exit),
            "pass": case_pass,
            "regex_matched": regex_matched == "1",
            "expected_regex": expected_regex,
            "expect_json": expect_json == "1",
            "json_valid": json_valid == "1",
            "duration_seconds": int(duration_seconds),
            "stdout_log": stdout_log,
            "stderr_log": stderr_log,
            "env_snapshot": env_snapshot,
            "key_artifact_paths": key_artifacts,
            "artifact_hashes": artifact_hashes,
            "missing_artifacts": missing_artifacts,
        }
    )

passed = [row for row in rows if row["pass"]]
failed = [row for row in rows if not row["pass"]]

case_results_json.write_text(json.dumps({"cases": rows}, indent=2) + "\n", encoding="utf-8")

run_id = f"failure-{run_root.name}"
env_hash = sha256_file(global_env_snapshot)
counter = {"value": 0}


def next_correlation_id() -> str:
    counter["value"] += 1
    return f"{run_id}-corr-{counter['value']:04d}"


events = []
events.append(
    {
        "schema_version": "1.0.0",
        "timestamp_utc": now_utc_timestamp(),
        "run_id": run_id,
        "correlation_id": next_correlation_id(),
        "case_id": "__run__",
        "step_id": None,
        "event_type": "run_start",
        "command": "doctor_franktentui_failure_e2e",
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

for row in rows:
    stdout_path = Path(row["stdout_log"])
    stderr_path = Path(row["stderr_log"])

    events.append(
        {
            "schema_version": "1.0.0",
            "timestamp_utc": now_utc_timestamp(),
            "run_id": run_id,
            "correlation_id": next_correlation_id(),
            "case_id": row["case_id"],
            "step_id": row["case_id"],
            "event_type": "case_start",
            "command": "case_runner",
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
            "case_id": row["case_id"],
            "step_id": row["case_id"],
            "event_type": "case_end",
            "command": "case_runner",
            "env_hash": env_hash,
            "duration_ms": int(row["duration_seconds"]) * 1000,
            "exit_code": int(row["actual_exit"]),
            "stdout_sha256": sha256_or_none(stdout_path),
            "stderr_sha256": sha256_or_none(stderr_path),
            "artifact_hashes": row["artifact_hashes"],
            "expected": {
                "exit_code": int(row["expected_exit"]),
                "regex_match": True,
                "json_valid": bool(row["expect_json"]),
                "regex_pattern": row["expected_regex"],
            },
            "actual": {
                "exit_code": int(row["actual_exit"]),
                "regex_match": bool(row["regex_matched"]),
                "json_valid": bool(row["json_valid"]),
                "pass": bool(row["pass"]),
                "stdout_log": row["stdout_log"],
                "stderr_log": row["stderr_log"],
                "env_snapshot": row["env_snapshot"],
                "missing_artifacts": row["missing_artifacts"],
            },
        }
    )

    for artifact_path, artifact_sha in row["artifact_hashes"].items():
        artifact_size = 0
        artifact_file = Path(artifact_path)
        if artifact_file.exists():
            artifact_size = int(artifact_file.stat().st_size)
        events.append(
            {
                "schema_version": "1.0.0",
                "timestamp_utc": now_utc_timestamp(),
                "run_id": run_id,
                "correlation_id": next_correlation_id(),
                "case_id": row["case_id"],
                "step_id": None,
                "event_type": "artifact",
                "command": "case_artifacts",
                "env_hash": env_hash,
                "duration_ms": 0,
                "exit_code": 0,
                "stdout_sha256": None,
                "stderr_sha256": None,
                "artifact_hashes": {artifact_path: artifact_sha},
                "expected": {},
                "actual": {"size_bytes": artifact_size},
            }
        )

events.append(
    {
        "schema_version": "1.0.0",
        "timestamp_utc": now_utc_timestamp(),
        "run_id": run_id,
        "correlation_id": next_correlation_id(),
        "case_id": "__run__",
        "step_id": None,
        "event_type": "run_end",
        "command": "doctor_franktentui_failure_e2e",
        "env_hash": env_hash,
        "duration_ms": 0,
        "exit_code": 0 if not failed else 1,
        "stdout_sha256": None,
        "stderr_sha256": None,
        "artifact_hashes": {},
        "expected": {
            "exit_code": 0,
            "regex_match": True,
            "json_valid": True,
        },
        "actual": {
            "exit_code": 0 if not failed else 1,
            "regex_match": all(row["regex_matched"] for row in rows),
            "json_valid": all(row["json_valid"] for row in rows if row["expect_json"]),
            "failed_case_ids": [row["case_id"] for row in failed],
            "total_cases": len(rows),
            "passed_cases": len(passed),
            "failed_cases": len(failed),
        },
    }
)

events_jsonl.write_text(
    "".join(json.dumps(event, separators=(",", ":")) + "\n" for event in events),
    encoding="utf-8",
)

validator_stdout_log = log_dir / "validate_failure_jsonl.stdout.log"
validator_stderr_log = log_dir / "validate_failure_jsonl.stderr.log"
validator_start = time.monotonic()
validator_proc = subprocess.run(
    [
        str(validator_script),
        "--input",
        str(events_jsonl),
        "--workflow",
        "failure",
        "--report-json",
        str(event_validation_report_json),
    ],
    capture_output=True,
    text=True,
)
validator_duration_ms = int((time.monotonic() - validator_start) * 1000)
validator_stdout_log.write_text(validator_proc.stdout, encoding="utf-8")
validator_stderr_log.write_text(validator_proc.stderr, encoding="utf-8")

summary = {
    "status": "passed" if not failed and validator_proc.returncode == 0 else "failed",
    "run_root": str(run_root),
    "total_cases": len(rows),
    "passed_cases": len(passed),
    "failed_cases": len(failed),
    "case_results": str(case_results_json),
    "events_jsonl": str(events_jsonl),
    "events_validation_report": str(event_validation_report_json),
    "events_validation_exit_code": int(validator_proc.returncode),
    "events_validation_duration_ms": validator_duration_ms,
    "events_validation_stdout_log": str(validator_stdout_log),
    "events_validation_stderr_log": str(validator_stderr_log),
}
summary_json.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")

lines = [
    f"status={summary['status']}",
    f"run_root={run_root}",
    f"total_cases={summary['total_cases']}",
    f"passed_cases={summary['passed_cases']}",
    f"failed_cases={summary['failed_cases']}",
    f"case_results={case_results_json}",
    f"events_jsonl={events_jsonl}",
    f"events_validation_report={event_validation_report_json}",
    f"events_validation_exit_code={validator_proc.returncode}",
]

if failed:
    lines.append("failed_case_ids:")
    for row in failed:
        lines.append(f"- {row['case_id']}")

summary_txt.write_text("\n".join(lines) + "\n", encoding="utf-8")
print("\n".join(lines))

if failed or validator_proc.returncode != 0:
    raise SystemExit(1)
PY

echo "[e2e-failure] PASS doctor_franktentui failure matrix"
echo "[e2e-failure] run_root=${RUN_ROOT}"
echo "[e2e-failure] summary_json=${SUMMARY_JSON}"

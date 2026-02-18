#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_franktentui/e2e/failure_${TIMESTAMP_UTC}}"
CASES_DIR="${RUN_ROOT}/cases"
TOOLS_DIR="${RUN_ROOT}/tools"
LOG_DIR="${RUN_ROOT}/logs"
META_DIR="${RUN_ROOT}/meta"
CASE_RESULTS_TSV="${META_DIR}/case_results.tsv"
CASE_RESULTS_JSON="${META_DIR}/case_results.json"
SUMMARY_JSON="${META_DIR}/summary.json"
SUMMARY_TXT="${META_DIR}/summary.txt"
COMMAND_MANIFEST="${META_DIR}/command_manifest.txt"
ENV_SNAPSHOT="${META_DIR}/env_snapshot.txt"
VERSIONS_TXT="${META_DIR}/tool_versions.txt"
TARGET_DIR=""
BIN_PATH=""

mkdir -p "${CASES_DIR}" "${TOOLS_DIR}" "${LOG_DIR}" "${META_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[e2e-failure] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

require_command "cargo" "install Rust/Cargo toolchain"
require_command "jq" "install jq for JSON parsing"
require_command "rg" "install ripgrep for regex checks"
require_command "python3" "install Python 3"

cat > "${TOOLS_DIR}/vhs" <<'VHS'
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
VHS
chmod +x "${TOOLS_DIR}/vhs"

cat > "${TOOLS_DIR}/ffmpeg" <<'FFMPEG'
#!/usr/bin/env bash
set -euo pipefail
out="${@: -1}"
mkdir -p "$(dirname "$out")"
: > "$out"
exit 0
FFMPEG
chmod +x "${TOOLS_DIR}/ffmpeg"

cat > "${TOOLS_DIR}/ffprobe" <<'FFPROBE'
#!/usr/bin/env bash
set -euo pipefail
echo "1.0"
exit 0
FFPROBE
chmod +x "${TOOLS_DIR}/ffprobe"

export PATH="${TOOLS_DIR}:${PATH}"

{
  env | sort | grep -E '^(CI|TERM|SHELL|USER|HOME|PATH|RUSTUP_TOOLCHAIN|SQLMODEL_|FASTAPI_)=' || true
} > "${ENV_SNAPSHOT}"

{
  echo "doctor_franktentui_failure_e2e"
  echo "timestamp_utc=${TIMESTAMP_UTC}"
  echo "git_rev=$(git -C "${ROOT_DIR}" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "cargo_version=$(cargo --version)"
  echo "rustc_version=$(rustc --version)"
  echo "vhs_version=$("${TOOLS_DIR}/vhs" --version 2>/dev/null || echo fake-vhs)"
} > "${VERSIONS_TXT}"

: > "${CASE_RESULTS_TSV}"
: > "${COMMAND_MANIFEST}"

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

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${case_id}" \
    "${expected_exit}" \
    "${actual_exit}" \
    "${pass}" \
    "${regex_matched}" \
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

python3 - "${CASE_RESULTS_TSV}" "${CASE_RESULTS_JSON}" "${SUMMARY_JSON}" "${SUMMARY_TXT}" "${RUN_ROOT}" <<'PY'
import json
import sys
from pathlib import Path

rows_path = Path(sys.argv[1])
case_results_json = Path(sys.argv[2])
summary_json = Path(sys.argv[3])
summary_txt = Path(sys.argv[4])
run_root = Path(sys.argv[5])

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
    artifact_file = Path(artifact_list)
    if artifact_file.exists():
        for raw_path in artifact_file.read_text(encoding="utf-8").splitlines():
            path = raw_path.strip()
            if not path:
                continue
            key_artifacts.append(path)
            if not Path(path).exists():
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
            "expect_json": expect_json == "1",
            "json_valid": json_valid == "1",
            "duration_seconds": int(duration_seconds),
            "stdout_log": stdout_log,
            "stderr_log": stderr_log,
            "env_snapshot": env_snapshot,
            "key_artifact_paths": key_artifacts,
            "missing_artifacts": missing_artifacts,
        }
    )

passed = [row for row in rows if row["pass"]]
failed = [row for row in rows if not row["pass"]]

case_results_json.write_text(json.dumps({"cases": rows}, indent=2) + "\n", encoding="utf-8")

summary = {
    "status": "passed" if not failed else "failed",
    "run_root": str(run_root),
    "total_cases": len(rows),
    "passed_cases": len(passed),
    "failed_cases": len(failed),
    "case_results": str(case_results_json),
}
summary_json.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")

lines = [
    f"status={summary['status']}",
    f"run_root={run_root}",
    f"total_cases={summary['total_cases']}",
    f"passed_cases={summary['passed_cases']}",
    f"failed_cases={summary['failed_cases']}",
    f"case_results={case_results_json}",
]

if failed:
    lines.append("failed_case_ids:")
    for row in failed:
        lines.append(f"- {row['case_id']}")

summary_txt.write_text("\n".join(lines) + "\n", encoding="utf-8")
print("\n".join(lines))

if failed:
    raise SystemExit(1)
PY

echo "[e2e-failure] PASS doctor_franktentui failure matrix"
echo "[e2e-failure] run_root=${RUN_ROOT}"
echo "[e2e-failure] summary_json=${SUMMARY_JSON}"

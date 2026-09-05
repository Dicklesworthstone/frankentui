#!/usr/bin/env bash
# E2E wrapper for the doctor_frankentui supervised-orchestration substrate
# (bd-1dccp / bd-11ngr). Drives the real-subprocess integration suite -- which
# exercises success, non-zero exit (with/without retry), output capture,
# deadline-driven kill, and token cancellation, all with bounded teardown -- and
# emits operator-grade artifacts (report JSON, summary, manifest, replay helper)
# under RUN_ROOT. This is the cancellation/timeout E2E arm called for by the
# feature's acceptance criteria.
#
# Usage:
#   scripts/doctor_frankentui_supervised_orchestration_e2e.sh [RUN_ROOT]
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui_supervised_orchestration/${TIMESTAMP_UTC}}"
LOG_DIR="${RUN_ROOT}/logs"
ARTIFACT_DIR="${RUN_ROOT}/artifacts"
META_DIR="${RUN_ROOT}/meta"
STDOUT_LOG="${LOG_DIR}/cargo.stdout.log"
STDERR_LOG="${LOG_DIR}/cargo.stderr.log"
REPORT_JSON="${ARTIFACT_DIR}/supervised_orchestration_report.json"
REPLAY_SH="${ARTIFACT_DIR}/replay.sh"
SUMMARY_TXT="${META_DIR}/summary.txt"
MANIFEST_JSON="${META_DIR}/artifact_manifest.json"
COMMAND_TXT="${META_DIR}/command.txt"
STATUS=0

mkdir -p "${LOG_DIR}" "${ARTIFACT_DIR}" "${META_DIR}"
cd "${ROOT_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[supervised-orchestration] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

require_command "rch" "install or configure remote_compilation_helper"
require_command "jq" "install jq"
require_command "python3" "install Python 3"

CMD=(
  rch exec --
  cargo
  test
  -p
  doctor_frankentui
  --test
  supervised_orchestration_integration
  --
  --nocapture
)

printf '%q ' "${CMD[@]}" > "${COMMAND_TXT}"
printf '\n' >> "${COMMAND_TXT}"

if "${CMD[@]}" >"${STDOUT_LOG}" 2>"${STDERR_LOG}"; then
  STATUS=0
else
  STATUS=$?
fi

# Parse the libtest result line. Under `rch` the remote test output is surfaced
# on stderr, so parse both streams.
if python3 - "${STDOUT_LOG}" "${STDERR_LOG}" "${REPORT_JSON}" "${STATUS}" <<'PY'
import json
import re
import sys
from pathlib import Path

stdout_log = Path(sys.argv[1])
stderr_log = Path(sys.argv[2])
report_json = Path(sys.argv[3])
status = int(sys.argv[4])

text = stdout_log.read_text(encoding="utf-8", errors="replace") + "\n" + stderr_log.read_text(
    encoding="utf-8", errors="replace"
)

cases = []
for match in re.finditer(r"^test (\S+) \.\.\. (ok|FAILED|ignored)$", text, re.MULTILINE):
    cases.append({"name": match.group(1), "outcome": match.group(2)})

result = re.search(
    r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored;",
    text,
)
if result:
    verdict = result.group(1)
    passed = int(result.group(2))
    failed = int(result.group(3))
    ignored = int(result.group(4))
else:
    verdict = "unknown"
    passed = sum(1 for c in cases if c["outcome"] == "ok")
    failed = sum(1 for c in cases if c["outcome"] == "FAILED")
    ignored = sum(1 for c in cases if c["outcome"] == "ignored")

report = {
    "feature": "bd-1dccp",
    "task": "bd-11ngr",
    "suite": "supervised_orchestration_integration",
    "exit_status": status,
    "verdict": verdict,
    "summary": {
        "passed": passed,
        "failed": failed,
        "ignored": ignored,
        "total": passed + failed + ignored,
    },
    "cases": cases,
}
required = {
    "subprocess_success_records_ok": "subprocess_success",
    "subprocess_nonzero_without_retry_is_fatal": "subprocess_nonzero_without_retry_is_fatal",
    "subprocess_nonzero_with_retry_exhausts_budget": "subprocess_nonzero_with_retry_exhausts_budget",
    "subprocess_captures_stdout": "subprocess_captures_stdout",
    "subprocess_deadline_kills_child_promptly": "subprocess_deadline_kills_child_promptly",
    "subprocess_cancellation_kills_child_promptly": "subprocess_cancellation_kills_child_promptly",
    "subprocess_retries_then_succeeds": "subprocess_retries_then_succeeds",
    "subprocess_record_evidence_is_serializable_and_triage_ready": "subprocess_evidence_serializable_and_triage_ready",
}
observed = {case["name"]: case["outcome"] for case in cases}
errors = []
if status != 0:
    errors.append(f"cargo exited {status}")
if result is None or verdict != "ok" or passed == 0 or failed != 0 or ignored != 0:
    errors.append("missing or unsuccessful nonempty libtest summary")
if len(observed) != len(cases):
    errors.append("duplicate test case observations")
if len(cases) != passed + failed + ignored or sum(c["outcome"] == "ok" for c in cases) != passed:
    errors.append("case observations disagree with libtest summary")
report["covered_contracts"] = [contract for case, contract in required.items() if observed.get(case) == "ok"]
report["missing_contracts"] = [contract for case, contract in required.items() if observed.get(case) != "ok"]
if report["missing_contracts"]:
    errors.append("required contracts did not pass")
report["validation_errors"] = errors
if errors:
    report["verdict"] = "failed"
report_json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
raise SystemExit(1 if errors else 0)
PY
then
  :
else
  validation_status=$?
  if [[ "${STATUS}" -eq 0 ]]; then
    STATUS="${validation_status}"
  fi
fi

# doctor_frankentui:no-fake-allow: replay reruns this actual subprocess suite
# workflow; its libtest observations and process statuses determine the result.
cat > "${REPLAY_SH}" <<EOF
#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${ROOT_DIR}"
RUN_ROOT="\${1:-/tmp/doctor_frankentui_supervised_orchestration_replay/\$(date -u +%Y%m%dT%H%M%SZ)}"

cd "\${ROOT_DIR}"
"${ROOT_DIR}/scripts/doctor_frankentui_supervised_orchestration_e2e.sh" "\${RUN_ROOT}"
EOF
chmod +x "${REPLAY_SH}"

python3 - "${REPORT_JSON}" "${REPLAY_SH}" "${STDOUT_LOG}" "${STDERR_LOG}" "${COMMAND_TXT}" "${MANIFEST_JSON}" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

paths = [Path(p) for p in sys.argv[1:6]]
manifest_json = Path(sys.argv[6])

entries = []
for path in paths:
    payload = path.read_bytes()
    entries.append(
        {
            "path": str(path),
            "size_bytes": len(payload),
            "sha256": hashlib.sha256(payload).hexdigest(),
        }
    )

manifest_json.write_text(
    json.dumps({"artifact_count": len(entries), "artifacts": entries}, indent=2) + "\n",
    encoding="utf-8",
)
PY

{
  echo "status=${STATUS}"
  echo "run_root=${RUN_ROOT}"
  echo "report_json=${REPORT_JSON}"
  echo "replay_sh=${REPLAY_SH}"
  echo "manifest_json=${MANIFEST_JSON}"
  echo "stdout_log=${STDOUT_LOG}"
  echo "stderr_log=${STDERR_LOG}"
  jq -r '.summary | "passed=\(.passed)\nfailed=\(.failed)\nignored=\(.ignored)\ntotal=\(.total)"' "${REPORT_JSON}"
  jq -r '.verdict | "verdict=\(.)"' "${REPORT_JSON}"
} > "${SUMMARY_TXT}"

cat "${SUMMARY_TXT}"
exit "${STATUS}"

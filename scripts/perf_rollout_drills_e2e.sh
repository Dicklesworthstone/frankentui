#!/usr/bin/env bash
# E2E wrapper for the perf rollout drills (bd-lilcl). Runs the standard drill
# suite — shadow, canary, fallback, rollback, recovery, each through clean
# AND failure paths — and harvests the drill report JSONL into operator
# artifacts (reports, per-drill summary, replay helper, manifest).
#
# Usage:
#   scripts/perf_rollout_drills_e2e.sh [RUN_ROOT]
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/ftui_perf_rollout_drills/${TIMESTAMP_UTC}}"
LOG_DIR="${RUN_ROOT}/logs"
ARTIFACT_DIR="${RUN_ROOT}/artifacts"
META_DIR="${RUN_ROOT}/meta"
STDOUT_LOG="${LOG_DIR}/cargo.stdout.log"
STDERR_LOG="${LOG_DIR}/cargo.stderr.log"
REPORT_JSON="${ARTIFACT_DIR}/drill_validation_report.json"
REPORTS_JSONL="${ARTIFACT_DIR}/drill_reports.jsonl"
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
    echo "[perf-rollout-drills] missing required command: ${command} (${hint})" >&2
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
  ftui-harness
  --test
  perf_rollout_drills_e2e
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

# Harvest DRILL_REPORT JSONL and the libtest result. Under `rch` the remote
# test output is surfaced on stderr, so parse both streams.
python3 - "${STDOUT_LOG}" "${STDERR_LOG}" "${REPORT_JSON}" "${REPORTS_JSONL}" "${STATUS}" <<'PY'
import json
import re
import sys
from pathlib import Path

stdout_log = Path(sys.argv[1])
stderr_log = Path(sys.argv[2])
report_json = Path(sys.argv[3])
reports_jsonl = Path(sys.argv[4])
status = int(sys.argv[5])

text = stdout_log.read_text(encoding="utf-8", errors="replace") + "\n" + stderr_log.read_text(
    encoding="utf-8", errors="replace"
)

cases = []
for match in re.finditer(r"^test (\S+) \.\.\. (ok|FAILED|ignored)$", text, re.MULTILINE):
    cases.append({"name": match.group(1), "outcome": match.group(2)})

drills = []
seen = set()
for match in re.finditer(r"^DRILL_REPORT (\{.*\})$", text, re.MULTILINE):
    try:
        payload = json.loads(match.group(1))
    except json.JSONDecodeError:
        continue
    key = (payload.get("drill"), payload.get("scenario"))
    if key in seen:
        continue
    seen.add(key)
    drills.append(payload)

reports_jsonl.write_text(
    "".join(json.dumps(d, sort_keys=True) + "\n" for d in drills), encoding="utf-8"
)

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

drill_matrix = [
    {
        "drill": d.get("drill"),
        "scenario": d.get("scenario"),
        "mechanism_ok": d.get("mechanism_ok"),
        "risk_controlled": d.get("risk_controlled"),
    }
    for d in drills
]

report = {
    "bead": "bd-lilcl",
    "suite": "perf_rollout_drills_e2e",
    "exit_status": status,
    "verdict": verdict,
    "summary": {
        "passed": passed,
        "failed": failed,
        "ignored": ignored,
        "total": passed + failed + ignored,
    },
    "cases": cases,
    "drill_reports": len(drills),
    "drill_matrix": drill_matrix,
    "all_mechanisms_ok": all(d.get("mechanism_ok") for d in drills) if drills else False,
    "covered_contracts": [
        "e2e_standard_drill_suite_runs_and_emits_artifacts",
        "e2e_drill_suite_replays_byte_identically",
        "e2e_reports_are_operator_comprehensible",
        "e2e_failure_paths_are_first_class",
    ],
}
report_json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY

cat > "${REPLAY_SH}" <<EOF
#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${ROOT_DIR}"
RUN_ROOT="\${1:-/tmp/ftui_perf_rollout_drills_replay/\$(date -u +%Y%m%dT%H%M%SZ)}"

cd "\${ROOT_DIR}"
"${ROOT_DIR}/scripts/perf_rollout_drills_e2e.sh" "\${RUN_ROOT}"
EOF
chmod +x "${REPLAY_SH}"

python3 - "${REPORT_JSON}" "${REPORTS_JSONL}" "${REPLAY_SH}" "${STDOUT_LOG}" "${STDERR_LOG}" "${COMMAND_TXT}" "${MANIFEST_JSON}" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

paths = [Path(p) for p in sys.argv[1:7]]
manifest_json = Path(sys.argv[7])

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
  echo "reports_jsonl=${REPORTS_JSONL}"
  echo "replay_sh=${REPLAY_SH}"
  echo "manifest_json=${MANIFEST_JSON}"
  jq -r '.summary | "passed=\(.passed)\nfailed=\(.failed)\nignored=\(.ignored)\ntotal=\(.total)"' "${REPORT_JSON}"
  jq -r '"verdict=\(.verdict)\ndrill_reports=\(.drill_reports)\nall_mechanisms_ok=\(.all_mechanisms_ok)"' "${REPORT_JSON}"
  jq -r '.drill_matrix[] | "drill=\(.drill) scenario=\(.scenario) mechanism_ok=\(.mechanism_ok)"' "${REPORT_JSON}"
} > "${SUMMARY_TXT}"

cat "${SUMMARY_TXT}"
exit "${STATUS}"

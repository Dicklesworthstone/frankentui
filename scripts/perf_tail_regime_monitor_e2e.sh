#!/usr/bin/env bash
# E2E wrapper for the tail-risk / regime-shift monitor validation (bd-zzfhe).
# Runs the scripted validation suite (pass / warning / hard-gate paths, the
# full challenge-fixture self-test, replay determinism, machine-readability)
# and harvests the monitor report JSONL into operator artifacts.
#
# Usage:
#   scripts/perf_tail_regime_monitor_e2e.sh [RUN_ROOT]
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/ftui_perf_tail_regime_monitor/${TIMESTAMP_UTC}}"
LOG_DIR="${RUN_ROOT}/logs"
ARTIFACT_DIR="${RUN_ROOT}/artifacts"
META_DIR="${RUN_ROOT}/meta"
STDOUT_LOG="${LOG_DIR}/cargo.stdout.log"
STDERR_LOG="${LOG_DIR}/cargo.stderr.log"
REPORT_JSON="${ARTIFACT_DIR}/monitor_validation_report.json"
REPORTS_JSONL="${ARTIFACT_DIR}/monitor_reports.jsonl"
SELFTEST_JSON="${ARTIFACT_DIR}/monitor_selftest.json"
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
    echo "[tail-regime-monitor] missing required command: ${command} (${hint})" >&2
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
  tail_regime_monitor_e2e
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

# Harvest monitor report JSONL + the self-test verdict, and parse the libtest
# result. Under `rch` the remote test output is surfaced on stderr, so parse
# both streams.
python3 - "${STDOUT_LOG}" "${STDERR_LOG}" "${REPORT_JSON}" "${REPORTS_JSONL}" "${SELFTEST_JSON}" "${STATUS}" <<'PY'
import json
import re
import sys
from pathlib import Path

stdout_log = Path(sys.argv[1])
stderr_log = Path(sys.argv[2])
report_json = Path(sys.argv[3])
reports_jsonl = Path(sys.argv[4])
selftest_json = Path(sys.argv[5])
status = int(sys.argv[6])

text = stdout_log.read_text(encoding="utf-8", errors="replace") + "\n" + stderr_log.read_text(
    encoding="utf-8", errors="replace"
)

cases = []
for match in re.finditer(r"^test (\S+) \.\.\. (ok|FAILED|ignored)$", text, re.MULTILINE):
    cases.append({"name": match.group(1), "outcome": match.group(2)})

reports = []
for match in re.finditer(r"^MONITOR_REPORT scenario=(\S+) (\{.*\})$", text, re.MULTILINE):
    try:
        payload = json.loads(match.group(2))
    except json.JSONDecodeError:
        continue
    payload["scenario"] = match.group(1)
    reports.append(payload)

reports_jsonl.write_text(
    "".join(json.dumps(r, sort_keys=True) + "\n" for r in reports), encoding="utf-8"
)

selftest = {}
selftest_match = re.search(r"^MONITOR_SELFTEST (\{.*\})$", text, re.MULTILINE)
if selftest_match:
    try:
        selftest = json.loads(selftest_match.group(1))
    except json.JSONDecodeError:
        selftest = {"parse_error": True}
selftest_json.write_text(json.dumps(selftest, indent=2, sort_keys=True) + "\n", encoding="utf-8")

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

by_action = {}
for r in reports:
    action = r.get("gate_action", "unknown")
    by_action[action] = by_action.get(action, 0) + 1

report = {
    "bead": "bd-zzfhe",
    "suite": "tail_regime_monitor_e2e",
    "exit_status": status,
    "verdict": verdict,
    "summary": {
        "passed": passed,
        "failed": failed,
        "ignored": ignored,
        "total": passed + failed + ignored,
    },
    "cases": cases,
    "monitor_reports": len(reports),
    "gate_actions_observed": by_action,
    "selftest_passed": selftest.get("passed"),
    "covered_contracts": [
        "e2e_pass_path_proceeds",
        "e2e_warning_path_requires_review",
        "e2e_hard_gate_blocks_rollout",
        "e2e_self_test_all_fixtures_behave_as_designed",
        "e2e_reports_are_replayable_and_machine_readable",
        "e2e_lanes_round_trip_through_reports",
        "e2e_negative_control_alerting_is_visible_in_json",
    ],
}
report_json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY

cat > "${REPLAY_SH}" <<EOF
#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${ROOT_DIR}"
RUN_ROOT="\${1:-/tmp/ftui_perf_tail_regime_monitor_replay/\$(date -u +%Y%m%dT%H%M%SZ)}"

cd "\${ROOT_DIR}"
"${ROOT_DIR}/scripts/perf_tail_regime_monitor_e2e.sh" "\${RUN_ROOT}"
EOF
chmod +x "${REPLAY_SH}"

python3 - "${REPORT_JSON}" "${REPORTS_JSONL}" "${SELFTEST_JSON}" "${REPLAY_SH}" "${STDOUT_LOG}" "${STDERR_LOG}" "${COMMAND_TXT}" "${MANIFEST_JSON}" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

paths = [Path(p) for p in sys.argv[1:8]]
manifest_json = Path(sys.argv[8])

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
  echo "selftest_json=${SELFTEST_JSON}"
  echo "replay_sh=${REPLAY_SH}"
  echo "manifest_json=${MANIFEST_JSON}"
  jq -r '.summary | "passed=\(.passed)\nfailed=\(.failed)\nignored=\(.ignored)\ntotal=\(.total)"' "${REPORT_JSON}"
  jq -r '"verdict=\(.verdict)\nmonitor_reports=\(.monitor_reports)\nselftest_passed=\(.selftest_passed)"' "${REPORT_JSON}"
  jq -r '.gate_actions_observed | to_entries | map("gate_action_\(.key)=\(.value)") | join("\n")' "${REPORT_JSON}"
} > "${SUMMARY_TXT}"

cat "${SUMMARY_TXT}"
exit "${STATUS}"

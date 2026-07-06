#!/usr/bin/env bash
# E2E wrapper for the load / cancellation / shutdown / terminal-safety
# non-interference gauntlet (bd-lu69j). Drives the real headless `Program`
# through event bursts, effect-queue floods with backpressure drops,
# cancellation races, and adversarial governor configurations (degradation
# storms, pathological policies, always-overloaded watermarks), then emits
# operator-grade artifacts (report JSON, per-scenario performance metrics
# JSONL, summary, manifest, replay helper) under RUN_ROOT.
#
# Usage:
#   scripts/runtime_load_noninterference_gauntlet_e2e.sh [RUN_ROOT]
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/ftui_runtime_load_noninterference/${TIMESTAMP_UTC}}"
LOG_DIR="${RUN_ROOT}/logs"
ARTIFACT_DIR="${RUN_ROOT}/artifacts"
META_DIR="${RUN_ROOT}/meta"
STDOUT_LOG="${LOG_DIR}/cargo.stdout.log"
STDERR_LOG="${LOG_DIR}/cargo.stderr.log"
REPORT_JSON="${ARTIFACT_DIR}/gauntlet_report.json"
METRICS_JSONL="${ARTIFACT_DIR}/gauntlet_metrics.jsonl"
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
    echo "[load-noninterference] missing required command: ${command} (${hint})" >&2
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
  ftui-runtime
  --test
  load_noninterference_gauntlet
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

# Parse the libtest result line and harvest the per-scenario performance
# metric lines the gauntlet prints (correctness verdicts and performance
# metrics come from the same runs). Under `rch` the remote test output is
# surfaced on stderr, so parse both streams.
python3 - "${STDOUT_LOG}" "${STDERR_LOG}" "${REPORT_JSON}" "${METRICS_JSONL}" "${STATUS}" <<'PY'
import json
import re
import sys
from pathlib import Path

stdout_log = Path(sys.argv[1])
stderr_log = Path(sys.argv[2])
report_json = Path(sys.argv[3])
metrics_jsonl = Path(sys.argv[4])
status = int(sys.argv[5])

text = stdout_log.read_text(encoding="utf-8", errors="replace") + "\n" + stderr_log.read_text(
    encoding="utf-8", errors="replace"
)

cases = []
for match in re.finditer(r"^test (\S+) \.\.\. (ok|FAILED|ignored)$", text, re.MULTILINE):
    cases.append({"name": match.group(1), "outcome": match.group(2)})

metrics = []
for line in text.splitlines():
    line = line.strip()
    if not line.startswith('{"gauntlet":"load_noninterference"'):
        continue
    try:
        metrics.append(json.loads(line))
    except json.JSONDecodeError:
        continue

metrics_jsonl.write_text(
    "".join(json.dumps(m, sort_keys=True) + "\n" for m in metrics), encoding="utf-8"
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

scenarios = sorted({(m.get("scenario"), m.get("governor"), m.get("mode")) for m in metrics})
report = {
    "bead": "bd-lu69j",
    "suite": "load_noninterference_gauntlet",
    "exit_status": status,
    "verdict": verdict,
    "summary": {
        "passed": passed,
        "failed": failed,
        "ignored": ignored,
        "total": passed + failed + ignored,
    },
    "cases": cases,
    "scenario_runs": len(metrics),
    "scenario_matrix": [
        {"scenario": s, "governor": g, "mode": m} for (s, g, m) in scenarios
    ],
    "covered_contracts": [
        "policy_normalization_is_total_and_ordered (property)",
        "governor_variants_preserve_model_state_under_burst",
        "effect_flood_and_ticks_do_not_perturb_model_state",
        "always_degraded_governor_never_drops_scripted_input",
        "cancellation_race_shutdown_bounded_and_well_formed",
        "terminal_mode_safety_preserved_under_always_degraded_governor",
        "unmeetable_budget_suppresses_presentation_but_preserves_input",
        "evidence_ledger_governor_decisions_are_replayable",
        "negative_control_instruments_detect_violations",
    ],
}
report_json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY

cat > "${REPLAY_SH}" <<EOF
#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${ROOT_DIR}"
RUN_ROOT="\${1:-/tmp/ftui_runtime_load_noninterference_replay/\$(date -u +%Y%m%dT%H%M%SZ)}"

cd "\${ROOT_DIR}"
"${ROOT_DIR}/scripts/runtime_load_noninterference_gauntlet_e2e.sh" "\${RUN_ROOT}"
EOF
chmod +x "${REPLAY_SH}"

python3 - "${REPORT_JSON}" "${METRICS_JSONL}" "${REPLAY_SH}" "${STDOUT_LOG}" "${STDERR_LOG}" "${COMMAND_TXT}" "${MANIFEST_JSON}" <<'PY'
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
  echo "metrics_jsonl=${METRICS_JSONL}"
  echo "replay_sh=${REPLAY_SH}"
  echo "manifest_json=${MANIFEST_JSON}"
  echo "stdout_log=${STDOUT_LOG}"
  echo "stderr_log=${STDERR_LOG}"
  jq -r '.summary | "passed=\(.passed)\nfailed=\(.failed)\nignored=\(.ignored)\ntotal=\(.total)"' "${REPORT_JSON}"
  jq -r '"verdict=\(.verdict)\nscenario_runs=\(.scenario_runs)"' "${REPORT_JSON}"
} > "${SUMMARY_TXT}"

cat "${SUMMARY_TXT}"
exit "${STATUS}"

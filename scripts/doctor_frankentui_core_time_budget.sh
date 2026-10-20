#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUDGET_S="${DOCTOR_CORE_TIME_BUDGET_S:-300}"
OUT_JSONL="${1:-${DOCTOR_CORE_TIME_JSONL:-${ROOT_DIR}/target/doctor_core_time.jsonl}}"
RUN_ID="doctor_core_time_$(date -u +%Y%m%dT%H%M%SZ)_$$"
TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

export TMPDIR="${DOCTOR_CORE_TIME_TMPDIR:-${ROOT_DIR}/target/doctor_frankentui_ci/tmp}"
mkdir -p "$(dirname "${OUT_JSONL}")" "${TMPDIR}"

START_NS="$(date +%s%N)"
set +e
RCH_CARGO_WRAPPER_BYPASS="${RCH_CARGO_WRAPPER_BYPASS:-1}" \
  cargo test -p doctor_frankentui --all-targets
CARGO_EXIT=$?
set -e
END_NS="$(date +%s%N)"

python3 - "${RUN_ID}" "${TS}" "${START_NS}" "${END_NS}" "${BUDGET_S}" "${CARGO_EXIT}" "${OUT_JSONL}" <<'PY'
import json
import sys
from pathlib import Path

run_id, ts, start_ns, end_ns, budget_s, cargo_exit, out_jsonl = sys.argv[1:8]
elapsed_s = round((int(end_ns) - int(start_ns)) / 1_000_000_000.0, 3)
budget = float(budget_s)
exit_code = int(cargo_exit)
passed = (exit_code == 0) and (elapsed_s <= budget)
record = {
    "run_id": run_id,
    "ts": ts,
    "crate": "doctor_frankentui",
    "seconds": elapsed_s,
    "budget_s": int(budget) if budget.is_integer() else budget,
    "cargo_exit": exit_code,
    "status": "pass" if passed else "fail",
}
line = json.dumps(record)
out_path = Path(out_jsonl)
with out_path.open("a", encoding="utf-8") as fh:
    fh.write(line + "\n")
print(line)
if not passed:
    sys.exit(1)
PY

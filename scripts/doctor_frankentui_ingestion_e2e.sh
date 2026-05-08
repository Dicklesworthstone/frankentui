#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/e2e/ingestion_${TIMESTAMP_UTC}}"
LOG_DIR="${RUN_ROOT}/logs"
META_DIR="${RUN_ROOT}/meta"
SUMMARY_JSON="${META_DIR}/summary.json"
SUMMARY_TXT="${META_DIR}/summary.txt"
VALIDATION_REPORT_JSON="${META_DIR}/validation_report.json"
EVENTS_JSONL="${META_DIR}/events.jsonl"
STDOUT_LOG="${LOG_DIR}/cargo_test.stdout.log"
STDERR_LOG="${LOG_DIR}/cargo_test.stderr.log"
RUN_ID="${DOCTOR_FRANKENTUI_INGESTION_E2E_RUN_ID:-ingestion-e2e-seed-${E2E_SEED:-0}}"
CARGO_BIN="${CARGO:-cargo}"
REPRODUCTION_COMMAND="DOCTOR_FRANKENTUI_INGESTION_E2E_RUN_ID=${RUN_ID} ${BASH_SOURCE[0]} ${RUN_ROOT}"

mkdir -p "${LOG_DIR}" "${META_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[ingestion-e2e] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

write_failure() {
  local parser_stage="$1"
  local reason="$2"
  python3 - \
    "${SUMMARY_JSON}" \
    "${SUMMARY_TXT}" \
    "${VALIDATION_REPORT_JSON}" \
    "${EVENTS_JSONL}" \
    "${RUN_ROOT}" \
    "${RUN_ID}" \
    "${parser_stage}" \
    "${reason}" \
    "${STDOUT_LOG}" \
    "${STDERR_LOG}" \
    "${REPRODUCTION_COMMAND}" <<'PY'
from __future__ import annotations

import json
import sys
from pathlib import Path

summary_json = Path(sys.argv[1])
summary_txt = Path(sys.argv[2])
validation_report_json = Path(sys.argv[3])
events_jsonl = Path(sys.argv[4])
run_root = Path(sys.argv[5])
run_id = sys.argv[6]
parser_stage = sys.argv[7]
reason = sys.argv[8]
stdout_log = Path(sys.argv[9])
stderr_log = Path(sys.argv[10])
reproduction_command = sys.argv[11]

event = {
    "schema_version": "doctor-ingestion-e2e-v1",
    "run_id": run_id,
    "fixture_id": "__run__",
    "fixture_kind": "e2e",
    "fixture_path": None,
    "parser_stage": parser_stage,
    "stage_index": -1,
    "status": "failed",
    "normalization_hash": None,
    "counts": {},
    "diagnostics": [
        {
            "message": reason,
            "stdout_log": str(stdout_log),
            "stderr_log": str(stderr_log),
        }
    ],
    "reproduction_command": reproduction_command,
}
events_jsonl.write_text(json.dumps(event, sort_keys=True) + "\n", encoding="utf-8")

report = {
    "status": "failed",
    "run_id": run_id,
    "parser_stage": parser_stage,
    "reason": reason,
    "reproduction_command": reproduction_command,
}
validation_report_json.write_text(
    json.dumps(report, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)

summary = {
    "status": "failed",
    "run_root": str(run_root),
    "run_id": run_id,
    "events_jsonl": str(events_jsonl),
    "validation_report": str(validation_report_json),
    "stdout_log": str(stdout_log),
    "stderr_log": str(stderr_log),
    "parser_stage": parser_stage,
    "reason": reason,
    "reproduction_command": reproduction_command,
}
summary_json.write_text(
    json.dumps(summary, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
summary_txt.write_text(
    "\n".join(
        [
            "status=failed",
            f"run_root={run_root}",
            f"run_id={run_id}",
            f"parser_stage={parser_stage}",
            f"reason={reason}",
            f"events_jsonl={events_jsonl}",
            f"validation_report={validation_report_json}",
            f"reproduction_command={reproduction_command}",
        ]
    )
    + "\n",
    encoding="utf-8",
)
print(summary_txt.read_text(encoding="utf-8"), end="")
PY
}

validate_success_artifacts() {
  python3 - \
    "${SUMMARY_JSON}" \
    "${SUMMARY_TXT}" \
    "${VALIDATION_REPORT_JSON}" \
    "${EVENTS_JSONL}" \
    "${RUN_ROOT}" \
    "${RUN_ID}" \
    "${REPRODUCTION_COMMAND}" <<'PY'
from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from typing import Any

summary_json = Path(sys.argv[1])
summary_txt = Path(sys.argv[2])
validation_report_json = Path(sys.argv[3])
events_jsonl = Path(sys.argv[4])
run_root = Path(sys.argv[5])
run_id = sys.argv[6]
reproduction_command = sys.argv[7]

meta_dir = run_root / "meta"
manifest_path = meta_dir / "ingestion_manifest.json"
trace_a_path = meta_dir / "ingestion_trace_a.jsonl"
trace_b_path = meta_dir / "ingestion_trace_b.jsonl"
required_stages = [
    "module_graph",
    "parse",
    "composition",
    "state_effects",
    "style",
    "lowering",
]
required_kinds = {"happy", "edge", "malformed", "adversarial"}


def fail(stage: str, message: str) -> None:
    raise SystemExit(f"{stage}: {message}")


def read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        fail("manifest", f"missing {path}")
    except json.JSONDecodeError as exc:
        fail("manifest", f"invalid JSON in {path}: {exc}")


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    try:
        raw_lines = path.read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        fail("trace", f"missing {path}")
    rows: list[dict[str, Any]] = []
    for line_no, raw in enumerate(raw_lines, start=1):
        if not raw.strip():
            fail("trace", f"blank line {line_no} in {path}")
        try:
            row = json.loads(raw)
        except json.JSONDecodeError as exc:
            fail("trace", f"invalid JSONL line {line_no} in {path}: {exc}")
        if not isinstance(row, dict):
            fail("trace", f"line {line_no} in {path} is not an object")
        rows.append(row)
    return rows


manifest = read_json(manifest_path)
trace_a_text = trace_a_path.read_text(encoding="utf-8")
trace_b_text = trace_b_path.read_text(encoding="utf-8")
if trace_a_text != trace_b_text:
    fail("determinism", "trace_a and trace_b differ for identical seed/context")

rows = read_jsonl(trace_a_path)
if manifest.get("run_id") != run_id:
    fail("manifest", "run_id mismatch")
if manifest.get("required_stages") != required_stages:
    fail("manifest", "required_stages mismatch")
if manifest.get("fixture_count") != 4:
    fail("manifest", "fixture_count must be 4")
if manifest.get("trace_line_count") != len(rows):
    fail("manifest", "trace_line_count does not match JSONL rows")

fixtures = manifest.get("fixtures")
if not isinstance(fixtures, list) or len(fixtures) != 4:
    fail("manifest", "fixtures must contain 4 records")
manifest_kinds = {fixture.get("kind") for fixture in fixtures if isinstance(fixture, dict)}
if manifest_kinds != required_kinds:
    fail("manifest", f"fixture kinds mismatch: {sorted(manifest_kinds)}")

seen: dict[str, set[str]] = {}
for row in rows:
    fixture_id = row.get("fixture_id")
    parser_stage = row.get("parser_stage")
    if not isinstance(fixture_id, str) or not fixture_id:
        fail("trace", "row missing fixture_id")
    if parser_stage not in required_stages:
        fail("trace", f"unexpected parser_stage {parser_stage!r}")
    if row.get("status") != "ok":
        fail(str(parser_stage), "success trace contains non-ok status")
    normalization_hash = row.get("normalization_hash")
    if not isinstance(normalization_hash, str) or len(normalization_hash) != 64:
        fail(str(parser_stage), "normalization_hash must be 64 hex chars")
    command = row.get("reproduction_command")
    if not isinstance(command, str) or "doctor_frankentui_ingestion_e2e.sh" not in command:
        fail(str(parser_stage), "missing reproduction command")
    seen.setdefault(fixture_id, set()).add(str(parser_stage))

for fixture in fixtures:
    if not isinstance(fixture, dict):
        fail("manifest", "fixture manifest row is not an object")
    fixture_id = fixture.get("fixture_id")
    if not isinstance(fixture_id, str):
        fail("manifest", "fixture missing fixture_id")
    if seen.get(fixture_id) != set(required_stages):
        fail("manifest", f"fixture {fixture_id} missing stage coverage")

events_jsonl.write_text(trace_a_text, encoding="utf-8")
trace_sha256 = hashlib.sha256(trace_a_text.encode("utf-8")).hexdigest()

report = {
    "status": "passed",
    "run_id": run_id,
    "trace_sha256": trace_sha256,
    "trace_line_count": len(rows),
    "fixture_count": len(fixtures),
    "required_stages": required_stages,
}
validation_report_json.write_text(
    json.dumps(report, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)

summary = {
    "status": "passed",
    "run_root": str(run_root),
    "run_id": run_id,
    "manifest": str(manifest_path),
    "events_jsonl": str(events_jsonl),
    "trace_a": str(trace_a_path),
    "trace_b": str(trace_b_path),
    "validation_report": str(validation_report_json),
    "trace_sha256": trace_sha256,
    "reproduction_command": reproduction_command,
}
summary_json.write_text(
    json.dumps(summary, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
summary_txt.write_text(
    "\n".join(
        [
            "status=passed",
            f"run_root={run_root}",
            f"run_id={run_id}",
            f"manifest={manifest_path}",
            f"events_jsonl={events_jsonl}",
            f"trace_a={trace_a_path}",
            f"trace_b={trace_b_path}",
            f"validation_report={validation_report_json}",
            f"trace_sha256={trace_sha256}",
        ]
    )
    + "\n",
    encoding="utf-8",
)
print(summary_txt.read_text(encoding="utf-8"), end="")
PY
}

require_command "${CARGO_BIN}" "install Rust/Cargo toolchain"
require_command "python3" "install Python 3"

export DOCTOR_FRANKENTUI_INGESTION_E2E_RUN_ROOT="${RUN_ROOT}"
export DOCTOR_FRANKENTUI_INGESTION_E2E_RUN_ID="${RUN_ID}"

set +e
(
  cd "${ROOT_DIR}"
  "${CARGO_BIN}" test \
    -p doctor_frankentui \
    --test ir_invariant_tests \
    ingestion_e2e_trace_export_is_deterministic_and_manifest_complete \
    -- --nocapture
) >"${STDOUT_LOG}" 2>"${STDERR_LOG}"
cargo_status=$?
set -e

if [[ "${cargo_status}" -ne 0 ]]; then
  write_failure "cargo_test" "cargo test failed with exit ${cargo_status}"
  exit "${cargo_status}"
fi

set +e
validation_output="$(validate_success_artifacts 2>&1)"
validation_status=$?
set -e

if [[ "${validation_status}" -ne 0 ]]; then
  write_failure "validate_trace" "${validation_output}"
  exit "${validation_status}"
fi

printf '%s' "${validation_output}"

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
SECURITY_AUDIT_JSONL="${META_DIR}/security_audit.jsonl"
SECURITY_REPORT_JSON="${META_DIR}/adversarial_security_report.json"
SECURITY_STDOUT_LOG="${LOG_DIR}/security_audit.stdout.log"
SECURITY_STDERR_LOG="${LOG_DIR}/security_audit.stderr.log"
RUN_ID="${DOCTOR_FRANKENTUI_INGESTION_E2E_RUN_ID:-ingestion-e2e-seed-${E2E_SEED:-0}}"
CARGO_CMD_TEXT="${CARGO_E2E_CMD:-${CARGO:-cargo}}"
read -r -a CARGO_CMD <<<"${CARGO_CMD_TEXT}"
CARGO_BIN="${CARGO_CMD[0]}"
REPRODUCTION_COMMAND="DOCTOR_FRANKENTUI_INGESTION_E2E_RUN_ID=${RUN_ID} DOCTOR_FRANKENTUI_ADVERSARIAL_INGESTION_E2E_RUN_ID=${RUN_ID} ${BASH_SOURCE[0]} ${RUN_ROOT}"

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
  local stdout_log="${3:-${STDOUT_LOG}}"
  local stderr_log="${4:-${STDERR_LOG}}"
  python3 - \
    "${SUMMARY_JSON}" \
    "${SUMMARY_TXT}" \
    "${VALIDATION_REPORT_JSON}" \
    "${EVENTS_JSONL}" \
    "${RUN_ROOT}" \
    "${RUN_ID}" \
    "${parser_stage}" \
    "${reason}" \
    "${stdout_log}" \
    "${stderr_log}" \
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
    "${REPRODUCTION_COMMAND}" \
    "${SECURITY_AUDIT_JSONL}" \
    "${SECURITY_REPORT_JSON}" <<'PY'
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
security_audit_path = Path(sys.argv[8])
security_report_path = Path(sys.argv[9])

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
required_security_fixtures = {
    "path-traversal-env",
    "sensitive-token-file",
    "malicious-subprocess",
    "blocked-network",
    "oversized-payload",
    "secret-leak-probe",
}


def fail(stage: str, message: str) -> None:
    raise SystemExit(f"{stage}: {message}")


def read_json(path: Path, stage: str = "manifest") -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        fail(stage, f"missing {path}")
    except json.JSONDecodeError as exc:
        fail(stage, f"invalid JSON in {path}: {exc}")


def read_jsonl(path: Path, stage: str = "trace") -> list[dict[str, Any]]:
    try:
        raw_lines = path.read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        fail(stage, f"missing {path}")
    rows: list[dict[str, Any]] = []
    for line_no, raw in enumerate(raw_lines, start=1):
        if not raw.strip():
            fail(stage, f"blank line {line_no} in {path}")
        try:
            row = json.loads(raw)
        except json.JSONDecodeError as exc:
            fail(stage, f"invalid JSONL line {line_no} in {path}: {exc}")
        if not isinstance(row, dict):
            fail(stage, f"line {line_no} in {path} is not an object")
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

security_rows = read_jsonl(security_audit_path, "security_audit")
if len(security_rows) != len(required_security_fixtures):
    fail(
        "security_audit",
        f"expected {len(required_security_fixtures)} security rows, got {len(security_rows)}",
    )
security_fixture_ids = {row.get("fixture_id") for row in security_rows}
if security_fixture_ids != required_security_fixtures:
    fail(
        "security_audit",
        f"fixture ids mismatch: {sorted(str(item) for item in security_fixture_ids)}",
    )

for row in security_rows:
    fixture_id = row.get("fixture_id")
    if row.get("schema_version") != "doctor-adversarial-ingestion-security-e2e-v1":
        fail("security_audit", f"{fixture_id}: schema_version mismatch")
    if row.get("run_id") != run_id:
        fail("security_audit", f"{fixture_id}: run_id mismatch")
    if not isinstance(row.get("replay_id"), str) or not row["replay_id"]:
        fail("security_audit", f"{fixture_id}: missing replay_id")
    replay_command = row.get("replay_command")
    if not isinstance(replay_command, str) or "sandbox_redaction_tests" not in replay_command:
        fail("security_audit", f"{fixture_id}: missing replay_command")
    if not isinstance(row.get("policy_decision"), str):
        fail("security_audit", f"{fixture_id}: missing policy_decision")

    if fixture_id == "secret-leak-probe":
        if row.get("status") != "redacted":
            fail("security_audit", "secret-leak-probe must be redacted")
        if not isinstance(row.get("redaction_count"), int) or row["redaction_count"] <= 0:
            fail("security_audit", "secret-leak-probe must record redactions")
    else:
        if row.get("status") != "blocked":
            fail("security_audit", f"{fixture_id}: hostile fixture must be blocked")
        if row.get("blocked") is not True:
            fail("security_audit", f"{fixture_id}: blocked flag must be true")
        exit_code = row.get("exit_code")
        if not isinstance(exit_code, int) or not 50 <= exit_code <= 59:
            fail("security_audit", f"{fixture_id}: exit_code must be in 50-59")
        if not isinstance(row.get("violation_kind"), str):
            fail("security_audit", f"{fixture_id}: missing violation_kind")

security_report = read_json(security_report_path, "security_report")
if security_report.get("schema_version") != "doctor-adversarial-ingestion-security-report-v1":
    fail("security_report", "schema_version mismatch")
if security_report.get("status") != "passed":
    fail("security_report", "status must be passed")
if security_report.get("run_id") != run_id:
    fail("security_report", "run_id mismatch")
if security_report.get("blocked_operations") != 5:
    fail("security_report", "blocked_operations must be 5")
if security_report.get("redacted_secret_probes") != 1:
    fail("security_report", "redacted_secret_probes must be 1")
replay_ids = security_report.get("replay_ids")
if not isinstance(replay_ids, list) or len(replay_ids) != len(required_security_fixtures):
    fail("security_report", "replay_ids must cover every adversarial fixture")

security_text = security_audit_path.read_text(encoding="utf-8") + security_report_path.read_text(
    encoding="utf-8"
)
for forbidden in ["ghp_", "AKIAIOSFODNN7EXAMPLE", "supersecret"]:
    if forbidden in security_text:
        fail("security_audit", f"unredacted secret marker leaked: {forbidden}")

report = {
    "status": "passed",
    "run_id": run_id,
    "trace_sha256": trace_sha256,
    "trace_line_count": len(rows),
    "fixture_count": len(fixtures),
    "required_stages": required_stages,
    "security_audit_jsonl": str(security_audit_path),
    "security_report": str(security_report_path),
    "security_trace_line_count": len(security_rows),
    "blocked_operations": security_report["blocked_operations"],
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
    "security_audit_jsonl": str(security_audit_path),
    "security_report": str(security_report_path),
    "security_trace_line_count": len(security_rows),
    "blocked_operations": security_report["blocked_operations"],
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
            f"security_audit_jsonl={security_audit_path}",
            f"security_report={security_report_path}",
            f"security_trace_line_count={len(security_rows)}",
            f"blocked_operations={security_report['blocked_operations']}",
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
export DOCTOR_FRANKENTUI_ADVERSARIAL_INGESTION_E2E_RUN_ROOT="${RUN_ROOT}"
export DOCTOR_FRANKENTUI_ADVERSARIAL_INGESTION_E2E_RUN_ID="${RUN_ID}"

set +e
(
  cd "${ROOT_DIR}"
  "${CARGO_CMD[@]}" test \
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
(
  cd "${ROOT_DIR}"
  "${CARGO_CMD[@]}" test \
    -p doctor_frankentui \
    --test sandbox_redaction_tests \
    adversarial_ingestion_security_audit_export_is_fail_closed_and_redacted \
    -- --nocapture
) >"${SECURITY_STDOUT_LOG}" 2>"${SECURITY_STDERR_LOG}"
security_status=$?
set -e

if [[ "${security_status}" -ne 0 ]]; then
  write_failure \
    "security_audit" \
    "security audit test failed with exit ${security_status}" \
    "${SECURITY_STDOUT_LOG}" \
    "${SECURITY_STDERR_LOG}"
  exit "${security_status}"
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

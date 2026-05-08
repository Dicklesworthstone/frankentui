#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/e2e/proof_artifacts_${TIMESTAMP_UTC}}"
LOG_DIR="${RUN_ROOT}/logs"
META_DIR="${RUN_ROOT}/meta"
ARTIFACT_DIR="${RUN_ROOT}/artifacts"
VERIFIER_DIR="${RUN_ROOT}/verifier"
REPLAY_DIR="${RUN_ROOT}/replay"
SESSION_TXT="${META_DIR}/session.txt"
ENV_SNAPSHOT="${META_DIR}/env_snapshot.txt"
VERSIONS_TXT="${META_DIR}/tool_versions.txt"
COMMAND_MANIFEST="${META_DIR}/command_manifest.txt"
SUMMARY_JSON="${META_DIR}/summary.json"
SUMMARY_TXT="${META_DIR}/summary.txt"
EVENTS_JSONL="${META_DIR}/events.jsonl"
EVENT_VALIDATION_REPORT_JSON="${META_DIR}/events_validation_report.json"
PROOF_LEDGER_JSONL="${META_DIR}/proof_artifact_ledger.jsonl"
VERIFIER_SUMMARY_JSON="${VERIFIER_DIR}/proof_verifier_summary.json"
ARTIFACT_MANIFEST_JSON="${META_DIR}/artifact_manifest.json"
CARGO_STDOUT_LOG="${LOG_DIR}/cargo_proof_artifacts_tests.stdout.log"
CARGO_STDERR_LOG="${LOG_DIR}/cargo_proof_artifacts_tests.stderr.log"
RUN_ID="${DOCTOR_FRANKENTUI_PROOF_E2E_RUN_ID:-proof-artifacts-e2e-seed-${E2E_SEED:-0}}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/rch_target_frankentui_proof_artifacts_${TIMESTAMP_UTC}}"
CARGO_EXIT_CODE=0

mkdir -p "${LOG_DIR}" "${META_DIR}" "${ARTIFACT_DIR}" "${VERIFIER_DIR}" "${REPLAY_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[proof-artifacts-e2e] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

require_command "python3" "install Python 3"
if [[ "${DOCTOR_FRANKENTUI_PROOF_E2E_SKIP_CARGO:-0}" != "1" ]]; then
  require_command "cargo" "install Rust/Cargo toolchain"
fi
if [[ "${DOCTOR_FRANKENTUI_USE_RCH:-0}" == "1" ]]; then
  require_command "rch" "install rch or unset DOCTOR_FRANKENTUI_USE_RCH"
fi

{
  echo "timestamp_utc=${TIMESTAMP_UTC}"
  echo "run_id=${RUN_ID}"
  echo "run_root=${RUN_ROOT}"
  echo "root_dir=${ROOT_DIR}"
  echo "cargo_target_dir=${CARGO_TARGET_DIR}"
  echo "use_rch=${DOCTOR_FRANKENTUI_USE_RCH:-0}"
  echo "skip_cargo=${DOCTOR_FRANKENTUI_PROOF_E2E_SKIP_CARGO:-0}"
  echo "inject_failure=${DOCTOR_FRANKENTUI_PROOF_E2E_INJECT_FAILURE:-}"
} > "${SESSION_TXT}"

{
  env | sort | grep -E '^(CI|TERM|SHELL|USER|HOME|PATH|RUSTUP_TOOLCHAIN|CARGO_TARGET_DIR|DOCTOR_FRANKENTUI_|E2E_SEED)=' || true
} > "${ENV_SNAPSHOT}"

{
  echo "doctor_frankentui_proof_artifacts_e2e"
  echo "timestamp_utc=${TIMESTAMP_UTC}"
  echo "git_rev=$(git -C "${ROOT_DIR}" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "cargo_version=$(cargo --version 2>/dev/null || echo cargo-skipped)"
  echo "rustc_version=$(rustc --version 2>/dev/null || echo rustc-skipped)"
  echo "python_version=$(python3 --version 2>/dev/null || echo python3-missing)"
  echo "rch_path=$(command -v rch 2>/dev/null || echo missing)"
  echo "rch_version=$(rch --version 2>/dev/null | head -n 1 || echo unknown)"
} > "${VERSIONS_TXT}"

run_cargo_proof_gate() {
  local command_display
  if [[ "${DOCTOR_FRANKENTUI_PROOF_E2E_SKIP_CARGO:-0}" == "1" ]]; then
    command_display="skip cargo proof gate (DOCTOR_FRANKENTUI_PROOF_E2E_SKIP_CARGO=1)"
    echo "[proof_artifacts_tests] ${command_display}" > "${COMMAND_MANIFEST}"
    {
      echo "skipped cargo proof-artifacts tests"
      echo "reason=DOCTOR_FRANKENTUI_PROOF_E2E_SKIP_CARGO=1"
    } > "${CARGO_STDOUT_LOG}"
    : > "${CARGO_STDERR_LOG}"
    CARGO_EXIT_CODE=0
    return
  fi

  command_display="cargo test -p doctor_frankentui --test proof_artifacts_tests -- --nocapture"
  if [[ "${DOCTOR_FRANKENTUI_USE_RCH:-0}" == "1" ]]; then
    command_display="RCH_ENV_ALLOWLIST=CARGO_TARGET_DIR rch exec -- env CARGO_TARGET_DIR=${CARGO_TARGET_DIR} ${command_display}"
  else
    command_display="env CARGO_TARGET_DIR=${CARGO_TARGET_DIR} ${command_display}"
  fi
  echo "[proof_artifacts_tests] ${command_display}" > "${COMMAND_MANIFEST}"

  set +e
  if [[ "${DOCTOR_FRANKENTUI_USE_RCH:-0}" == "1" ]]; then
    RCH_ENV_ALLOWLIST=CARGO_TARGET_DIR \
      rch exec -- env CARGO_TARGET_DIR="${CARGO_TARGET_DIR}" \
        cargo test -p doctor_frankentui --test proof_artifacts_tests -- --nocapture \
        >"${CARGO_STDOUT_LOG}" 2>"${CARGO_STDERR_LOG}"
  else
    env CARGO_TARGET_DIR="${CARGO_TARGET_DIR}" \
      cargo test -p doctor_frankentui --test proof_artifacts_tests -- --nocapture \
      >"${CARGO_STDOUT_LOG}" 2>"${CARGO_STDERR_LOG}"
  fi
  CARGO_EXIT_CODE=$?
  set -e
}

run_cargo_proof_gate

python3 - \
  "${ROOT_DIR}" \
  "${RUN_ROOT}" \
  "${RUN_ID}" \
  "${CARGO_EXIT_CODE}" \
  "${SESSION_TXT}" \
  "${ENV_SNAPSHOT}" \
  "${VERSIONS_TXT}" \
  "${COMMAND_MANIFEST}" \
  "${CARGO_STDOUT_LOG}" \
  "${CARGO_STDERR_LOG}" \
  "${ARTIFACT_DIR}" \
  "${VERIFIER_DIR}" \
  "${REPLAY_DIR}" \
  "${PROOF_LEDGER_JSONL}" \
  "${VERIFIER_SUMMARY_JSON}" \
  "${EVENTS_JSONL}" \
  "${EVENT_VALIDATION_REPORT_JSON}" \
  "${SUMMARY_JSON}" \
  "${SUMMARY_TXT}" \
  "${ARTIFACT_MANIFEST_JSON}" <<'PY'
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

root_dir = Path(sys.argv[1])
run_root = Path(sys.argv[2])
run_id = sys.argv[3]
cargo_exit_code = int(sys.argv[4])
session_txt = Path(sys.argv[5])
env_snapshot = Path(sys.argv[6])
versions_txt = Path(sys.argv[7])
command_manifest = Path(sys.argv[8])
cargo_stdout_log = Path(sys.argv[9])
cargo_stderr_log = Path(sys.argv[10])
artifact_dir = Path(sys.argv[11])
verifier_dir = Path(sys.argv[12])
replay_dir = Path(sys.argv[13])
proof_ledger_jsonl = Path(sys.argv[14])
verifier_summary_json = Path(sys.argv[15])
events_jsonl = Path(sys.argv[16])
event_validation_report_json = Path(sys.argv[17])
summary_json = Path(sys.argv[18])
summary_txt = Path(sys.argv[19])
artifact_manifest_json = Path(sys.argv[20])

schema_version = "doctor-proof-artifacts-e2e-v1"
validator_script = root_dir / "scripts" / "doctor_frankentui_validate_jsonl.py"
schema_path = root_dir / "crates" / "doctor_frankentui" / "coverage" / "e2e_jsonl_schema.json"
inject_failure = os.environ.get("DOCTOR_FRANKENTUI_PROOF_E2E_INJECT_FAILURE", "")


def now_utc_timestamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_text(text: str) -> str:
    return sha256_bytes(text.encode("utf-8"))


def sha256_json(value: Any) -> str:
    return sha256_text(
        json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    )


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def sha256_or_none(path: Path) -> str | None:
    if path.exists():
        return sha256_file(path)
    return None


def witness(fixture_id: str, obligation_id: str, clause_id: str, subject: str) -> dict[str, Any]:
    evidence = {
        "fixture_id": fixture_id,
        "obligation_id": obligation_id,
        "clause_id": clause_id,
        "subject": subject,
        "semantic_trace_hash": sha256_text(f"{fixture_id}:{subject}:trace"),
    }
    return {
        "witness_id": f"wit-{obligation_id}",
        "obligation_id": obligation_id,
        "clause_id": clause_id,
        "evidence": evidence,
        "evidence_sha256": sha256_json(evidence),
    }


fixtures = [
    {
        "fixture_id": "proof-counter-green-path",
        "clauses": ["clause-state", "clause-render", "clause-effect"],
        "obligations": [
            {"obligation_id": "obl-state", "clause_id": "clause-state", "status": "satisfied"},
            {"obligation_id": "obl-render", "clause_id": "clause-render", "status": "satisfied"},
            {"obligation_id": "obl-effect", "clause_id": "clause-effect", "status": "satisfied"},
        ],
    },
    {
        "fixture_id": "proof-process-fallback-path",
        "clauses": ["clause-subscription", "clause-cmd-task", "clause-cleanup"],
        "obligations": [
            {
                "obligation_id": "obl-subscription",
                "clause_id": "clause-subscription",
                "status": "satisfied",
            },
            {"obligation_id": "obl-cmd-task", "clause_id": "clause-cmd-task", "status": "satisfied"},
            {"obligation_id": "obl-cleanup", "clause_id": "clause-cleanup", "status": "satisfied"},
        ],
    },
]

if inject_failure == "orphan_obligation":
    fixtures[0]["obligations"].append(
        {
            "obligation_id": "obl-orphan",
            "clause_id": "clause-not-in-verdict",
            "status": "satisfied",
        }
    )

artifact_paths: list[Path] = []
for fixture in fixtures:
    fixture_id = fixture["fixture_id"]
    fixture_dir = artifact_dir / fixture_id
    fixture_dir.mkdir(parents=True, exist_ok=True)

    witnesses = [
        witness(fixture_id, obligation["obligation_id"], obligation["clause_id"], "semantic-preservation")
        for obligation in fixture["obligations"]
    ]
    if inject_failure == "witness_hash_mismatch" and fixture_id == fixtures[0]["fixture_id"]:
        witnesses[0]["evidence_sha256"] = "0" * 64

    artifact = {
        "schema_version": "doctor_frankentui.semantic_proof_artifact.v1",
        "artifact_id": f"semantic-proof-{fixture_id}",
        "fixture_id": fixture_id,
        "run_id": run_id,
        "verdict_clauses": [
            {"clause_id": clause_id, "status": "accepted"} for clause_id in fixture["clauses"]
        ],
        "proof_obligations": [
            {
                **obligation,
                "witness_ids": [
                    item["witness_id"] for item in witnesses if item["obligation_id"] == obligation["obligation_id"]
                ],
            }
            for obligation in fixture["obligations"]
        ],
        "witnesses": witnesses,
        "certification_passed": True,
    }
    artifact["artifact_sha256"] = sha256_json(artifact)
    artifact_path = fixture_dir / "semantic_proof_artifact.json"
    artifact_path.write_text(json.dumps(artifact, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    artifact_paths.append(artifact_path)


def validate_artifact(artifact_path: Path) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    artifact = json.loads(artifact_path.read_text(encoding="utf-8"))
    fixture_id = artifact["fixture_id"]
    clauses = {clause["clause_id"] for clause in artifact.get("verdict_clauses", [])}
    obligations = {
        obligation["obligation_id"]: obligation for obligation in artifact.get("proof_obligations", [])
    }
    witnesses_by_id = {witness["witness_id"]: witness for witness in artifact.get("witnesses", [])}
    transcript: list[dict[str, Any]] = []
    errors: list[dict[str, Any]] = []

    def record(check_id: str, status: str, message: str, *, obligation_id: str | None = None) -> None:
        trace_id = f"{run_id}:{fixture_id}:{check_id}"
        row = {
            "schema_version": schema_version,
            "run_id": run_id,
            "fixture_id": fixture_id,
            "trace_id": trace_id,
            "check_id": check_id,
            "obligation_id": obligation_id,
            "status": status,
            "message": message,
        }
        transcript.append(row)
        if status == "failed":
            errors.append(row)

    if not obligations:
        record("obligations-present", "failed", "proof artifact has no obligations")
    else:
        record("obligations-present", "passed", f"{len(obligations)} obligations present")

    for obligation_id, obligation in sorted(obligations.items()):
        clause_id = obligation.get("clause_id")
        if clause_id not in clauses:
            record(
                f"{obligation_id}:clause-linked",
                "failed",
                f"orphan obligation references missing verdict clause {clause_id}",
                obligation_id=obligation_id,
            )
        else:
            record(
                f"{obligation_id}:clause-linked",
                "passed",
                f"obligation links to verdict clause {clause_id}",
                obligation_id=obligation_id,
            )

        witness_ids = obligation.get("witness_ids", [])
        if not witness_ids:
            record(
                f"{obligation_id}:witness-present",
                "failed",
                "obligation has no witness ids",
                obligation_id=obligation_id,
            )
            continue

        for witness_id in witness_ids:
            item = witnesses_by_id.get(witness_id)
            if item is None:
                record(
                    f"{obligation_id}:{witness_id}:witness-linked",
                    "failed",
                    "obligation references a missing witness",
                    obligation_id=obligation_id,
                )
                continue
            if item.get("obligation_id") != obligation_id:
                record(
                    f"{obligation_id}:{witness_id}:obligation-backlink",
                    "failed",
                    "witness obligation id does not match obligation",
                    obligation_id=obligation_id,
                )
            else:
                record(
                    f"{obligation_id}:{witness_id}:obligation-backlink",
                    "passed",
                    "witness links back to obligation",
                    obligation_id=obligation_id,
                )

            expected_hash = sha256_json(item.get("evidence", {}))
            if item.get("evidence_sha256") != expected_hash:
                record(
                    f"{obligation_id}:{witness_id}:witness-hash",
                    "failed",
                    "witness hash mismatch",
                    obligation_id=obligation_id,
                )
            else:
                record(
                    f"{obligation_id}:{witness_id}:witness-hash",
                    "passed",
                    "witness hash matches evidence payload",
                    obligation_id=obligation_id,
                )

    report = {
        "schema_version": schema_version,
        "run_id": run_id,
        "fixture_id": fixture_id,
        "artifact_path": str(artifact_path),
        "artifact_sha256": sha256_file(artifact_path),
        "check_count": len(transcript),
        "failed_check_count": len(errors),
        "failed_trace_ids": [row["trace_id"] for row in errors],
        "certification_passed": not errors,
    }
    return report, transcript


ledger_rows: list[dict[str, Any]] = []
verifier_reports: list[dict[str, Any]] = []
failed_trace_ids: list[str] = []
replay_transcripts: list[Path] = []

for artifact_path in artifact_paths:
    report, transcript = validate_artifact(artifact_path)
    fixture_id = report["fixture_id"]
    fixture_verifier_dir = verifier_dir / fixture_id
    fixture_replay_dir = replay_dir / fixture_id
    fixture_verifier_dir.mkdir(parents=True, exist_ok=True)
    fixture_replay_dir.mkdir(parents=True, exist_ok=True)

    report_path = fixture_verifier_dir / "proof_verifier_report.json"
    transcript_path = fixture_replay_dir / "replay_transcript.jsonl"
    report["report_path"] = str(report_path)
    report["replay_transcript"] = str(transcript_path)
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    transcript_path.write_text(
        "".join(json.dumps(row, sort_keys=True) + "\n" for row in transcript),
        encoding="utf-8",
    )

    replay_transcripts.append(transcript_path)
    verifier_reports.append(report)
    failed_trace_ids.extend(report["failed_trace_ids"])

    for row in transcript:
        ledger_rows.append(
            {
                "schema_version": schema_version,
                "run_id": run_id,
                "correlation_id": row["trace_id"],
                "fixture_id": fixture_id,
                "trace_id": row["trace_id"],
                "check_id": row["check_id"],
                "obligation_id": row["obligation_id"],
                "status": row["status"],
                "message": row["message"],
                "artifact_path": str(artifact_path),
                "artifact_sha256": report["artifact_sha256"],
                "verifier_report": str(report_path),
                "replay_transcript": str(transcript_path),
            }
        )

proof_ledger_jsonl.write_text(
    "".join(json.dumps(row, sort_keys=True) + "\n" for row in ledger_rows),
    encoding="utf-8",
)

verifier_summary = {
    "schema_version": schema_version,
    "run_id": run_id,
    "fixture_count": len(verifier_reports),
    "check_count": sum(report["check_count"] for report in verifier_reports),
    "failed_check_count": sum(report["failed_check_count"] for report in verifier_reports),
    "failed_trace_ids": failed_trace_ids,
    "reports": verifier_reports,
}
verifier_summary_json.write_text(
    json.dumps(verifier_summary, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)

env_hash = sha256_file(env_snapshot)
ledger_hash = sha256_file(proof_ledger_jsonl)
command_text = command_manifest.read_text(encoding="utf-8").strip()


def event(
    sequence: int,
    event_type: str,
    *,
    case_id: str | None = None,
    step_id: str | None = None,
    duration_ms: int = 0,
    exit_code: int = 0,
    stdout_sha256: str | None = None,
    stderr_sha256: str | None = None,
    artifact_hashes: dict[str, str] | None = None,
    expected: dict[str, Any] | None = None,
    actual: dict[str, Any] | None = None,
) -> dict[str, Any]:
    return {
        "schema_version": schema_version,
        "timestamp_utc": now_utc_timestamp(),
        "run_id": run_id,
        "correlation_id": f"{run_id}-event-{sequence:04d}",
        "case_id": case_id,
        "step_id": step_id,
        "event_type": event_type,
        "command": command_text,
        "env_hash": env_hash,
        "duration_ms": duration_ms,
        "exit_code": exit_code,
        "stdout_sha256": stdout_sha256,
        "stderr_sha256": stderr_sha256,
        "artifact_hashes": artifact_hashes or {},
        "expected": expected or {},
        "actual": actual or {},
    }


events: list[dict[str, Any]] = [
    event(
        1,
        "run_start",
        step_id="proof_artifact_validation",
        expected={"cargo_exit_code": 0, "failed_trace_ids": []},
        actual={"fixture_count": len(verifier_reports), "inject_failure": inject_failure},
    )
]
sequence = 2
for report in verifier_reports:
    events.append(
        event(
            sequence,
            "case_start",
            case_id=report["fixture_id"],
            step_id="proof_artifact_validation",
            expected={"artifact_sha256": report["artifact_sha256"]},
            actual={"artifact_path": report["artifact_path"]},
        )
    )
    sequence += 1
    events.append(
        event(
            sequence,
            "case_end",
            case_id=report["fixture_id"],
            step_id="proof_artifact_validation",
            exit_code=0 if report["failed_check_count"] == 0 else 1,
            artifact_hashes={
                "proof_verifier_report_json": sha256_file(Path(report["report_path"])),
                "replay_transcript_jsonl": sha256_file(Path(report["replay_transcript"])),
            },
            expected={"failed_check_count": 0},
            actual={
                "check_count": report["check_count"],
                "failed_check_count": report["failed_check_count"],
                "failed_trace_ids": report["failed_trace_ids"],
            },
        )
    )
    sequence += 1

events.append(
    event(
        sequence,
        "artifact",
        step_id="proof_artifact_validation",
        stdout_sha256=sha256_or_none(cargo_stdout_log),
        stderr_sha256=sha256_or_none(cargo_stderr_log),
        artifact_hashes={
            "proof_artifact_ledger_jsonl": ledger_hash,
            "proof_verifier_summary_json": sha256_file(verifier_summary_json),
            "cargo_stdout_log": sha256_or_none(cargo_stdout_log) or sha256_text("missing"),
            "cargo_stderr_log": sha256_or_none(cargo_stderr_log) or sha256_text("missing"),
        },
        actual={"replay_transcripts": [str(path) for path in replay_transcripts]},
    )
)
sequence += 1
events.append(
    event(
        sequence,
        "run_end",
        step_id="proof_artifact_validation",
        exit_code=0 if cargo_exit_code == 0 and not failed_trace_ids else 1,
        artifact_hashes={"proof_artifact_ledger_jsonl": ledger_hash},
        expected={"cargo_exit_code": 0, "failed_trace_ids": []},
        actual={
            "cargo_exit_code": cargo_exit_code,
            "failed_trace_ids": failed_trace_ids,
            "ledger_rows": len(ledger_rows),
        },
    )
)

events_jsonl.write_text(
    "".join(json.dumps(row, sort_keys=True) + "\n" for row in events),
    encoding="utf-8",
)

validator = subprocess.run(
    [
        sys.executable,
        str(validator_script),
        "--input",
        str(events_jsonl),
        "--schema",
        str(schema_path),
        "--workflow",
        "generic",
        "--report-json",
        str(event_validation_report_json),
    ],
    cwd=root_dir,
    text=True,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    check=False,
)
if validator.returncode != 0 and not event_validation_report_json.exists():
    event_validation_report_json.write_text(
        json.dumps(
            {
                "status": "failed",
                "stdout": validator.stdout,
                "stderr": validator.stderr,
                "returncode": validator.returncode,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )

status = (
    "passed"
    if cargo_exit_code == 0 and validator.returncode == 0 and not failed_trace_ids
    else "failed"
)
summary = {
    "schema_version": schema_version,
    "status": status,
    "run_id": run_id,
    "run_root": str(run_root),
    "events_jsonl": str(events_jsonl),
    "events_validation_report": str(event_validation_report_json),
    "proof_artifact_ledger_jsonl": str(proof_ledger_jsonl),
    "proof_verifier_summary": str(verifier_summary_json),
    "artifact_manifest": str(artifact_manifest_json),
    "cargo_stdout_log": str(cargo_stdout_log),
    "cargo_stderr_log": str(cargo_stderr_log),
    "cargo_exit_code": cargo_exit_code,
    "validator_exit_code": validator.returncode,
    "fixture_count": len(verifier_reports),
    "ledger_rows": len(ledger_rows),
    "failed_trace_ids": failed_trace_ids,
    "replay_transcripts": [str(path) for path in replay_transcripts],
}
summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")

summary_lines = [
    f"status={status}",
    f"run_root={run_root}",
    f"run_id={run_id}",
    f"fixture_count={len(verifier_reports)}",
    f"ledger_rows={len(ledger_rows)}",
    f"failed_trace_count={len(failed_trace_ids)}",
    f"cargo_exit_code={cargo_exit_code}",
    f"validator_exit_code={validator.returncode}",
    f"events_jsonl={events_jsonl}",
    f"events_validation_report={event_validation_report_json}",
    f"proof_artifact_ledger_jsonl={proof_ledger_jsonl}",
    f"proof_verifier_summary={verifier_summary_json}",
    f"artifact_manifest={artifact_manifest_json}",
]
summary_txt.write_text("\n".join(summary_lines) + "\n", encoding="utf-8")

artifact_paths_for_manifest = [
    session_txt,
    env_snapshot,
    versions_txt,
    command_manifest,
    cargo_stdout_log,
    cargo_stderr_log,
    proof_ledger_jsonl,
    verifier_summary_json,
    events_jsonl,
    event_validation_report_json,
    summary_json,
    summary_txt,
    *artifact_paths,
    *sorted(verifier_dir.glob("*/*.json")),
    *replay_transcripts,
]
artifacts = []
missing = []
for path in artifact_paths_for_manifest:
    if not path.exists():
        missing.append(str(path))
        continue
    artifacts.append(
        {
            "path": str(path),
            "size_bytes": path.stat().st_size,
            "sha256": sha256_file(path),
        }
    )

artifact_manifest = {
    "schema_version": schema_version,
    "status": status,
    "run_id": run_id,
    "run_root": str(run_root),
    "artifact_count": len(artifacts),
    "missing_count": len(missing),
    "artifacts": artifacts,
    "missing": missing,
}
artifact_manifest_json.write_text(
    json.dumps(artifact_manifest, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)

print(summary_txt.read_text(encoding="utf-8"), end="")
raise SystemExit(0 if status == "passed" else 1)
PY

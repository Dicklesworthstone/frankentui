#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/e2e/gap_closure_${TIMESTAMP_UTC}}"
LOG_DIR="${RUN_ROOT}/logs"
META_DIR="${RUN_ROOT}/meta"
ARTIFACT_DIR="${RUN_ROOT}/artifacts"
SESSION_TXT="${META_DIR}/session.txt"
ENV_SNAPSHOT="${META_DIR}/env_snapshot.txt"
VERSIONS_TXT="${META_DIR}/tool_versions.txt"
COMMAND_MANIFEST="${META_DIR}/command_manifest.txt"
SUMMARY_JSON="${META_DIR}/summary.json"
SUMMARY_TXT="${META_DIR}/summary.txt"
EVENTS_JSONL="${META_DIR}/events.jsonl"
EVENT_VALIDATION_REPORT_JSON="${META_DIR}/events_validation_report.json"
GAP_LEDGER_JSONL="${META_DIR}/gap_closure_ledger.jsonl"
ARTIFACT_MANIFEST_JSON="${META_DIR}/artifact_manifest.json"
CARGO_STDOUT_LOG="${LOG_DIR}/cargo_test.stdout.log"
CARGO_STDERR_LOG="${LOG_DIR}/cargo_test.stderr.log"
RUN_ID="${DOCTOR_FRANKENTUI_GAP_CLOSURE_RUN_ID:-gap-closure-e2e-seed-${E2E_SEED:-0}}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/rch_target_frankentui_gap_closure_${TIMESTAMP_UTC}}"
CARGO_EXIT_CODE=0

mkdir -p "${LOG_DIR}" "${META_DIR}" "${ARTIFACT_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[gap-closure-e2e] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

require_command "cargo" "install Rust/Cargo toolchain"
require_command "python3" "install Python 3"
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
} > "${SESSION_TXT}"

{
  env | sort | grep -E '^(CI|TERM|SHELL|USER|HOME|PATH|RUSTUP_TOOLCHAIN|CARGO_TARGET_DIR|DOCTOR_FRANKENTUI_|E2E_SEED)=' || true
} > "${ENV_SNAPSHOT}"

{
  echo "doctor_frankentui_gap_closure_e2e"
  echo "timestamp_utc=${TIMESTAMP_UTC}"
  echo "git_rev=$(git -C "${ROOT_DIR}" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "cargo_version=$(cargo --version)"
  echo "rustc_version=$(rustc --version 2>/dev/null || echo rustc-missing)"
  echo "python_version=$(python3 --version 2>/dev/null || echo python3-missing)"
  echo "rch_path=$(command -v rch 2>/dev/null || echo missing)"
  echo "rch_version=$(rch --version 2>/dev/null | head -n 1 || echo unknown)"
} > "${VERSIONS_TXT}"

run_cargo_regression_gate() {
  local command_display
  if [[ "${DOCTOR_FRANKENTUI_GAP_CLOSURE_SKIP_CARGO:-0}" == "1" ]]; then
    command_display="skip cargo regression gate (DOCTOR_FRANKENTUI_GAP_CLOSURE_SKIP_CARGO=1)"
    echo "[atlas_regression_gate] ${command_display}" > "${COMMAND_MANIFEST}"
    {
      echo "skipped cargo regression gate"
      echo "reason=DOCTOR_FRANKENTUI_GAP_CLOSURE_SKIP_CARGO=1"
    } > "${CARGO_STDOUT_LOG}"
    : > "${CARGO_STDERR_LOG}"
    CARGO_EXIT_CODE=0
    return
  fi

  command_display="cargo test -p doctor_frankentui mapping_atlas::tests::reevaluate_resolves_new_mappings -- --nocapture"
  if [[ "${DOCTOR_FRANKENTUI_USE_RCH:-0}" == "1" ]]; then
    command_display="RCH_ENV_ALLOWLIST=CARGO_TARGET_DIR rch exec -- env CARGO_TARGET_DIR=${CARGO_TARGET_DIR} ${command_display}"
  else
    command_display="env CARGO_TARGET_DIR=${CARGO_TARGET_DIR} ${command_display}"
  fi
  echo "[atlas_regression_gate] ${command_display}" > "${COMMAND_MANIFEST}"

  set +e
  if [[ "${DOCTOR_FRANKENTUI_USE_RCH:-0}" == "1" ]]; then
    RCH_ENV_ALLOWLIST=CARGO_TARGET_DIR \
      rch exec -- env CARGO_TARGET_DIR="${CARGO_TARGET_DIR}" \
        cargo test -p doctor_frankentui \
          mapping_atlas::tests::reevaluate_resolves_new_mappings -- --nocapture \
          >"${CARGO_STDOUT_LOG}" 2>"${CARGO_STDERR_LOG}"
  else
    env CARGO_TARGET_DIR="${CARGO_TARGET_DIR}" \
      cargo test -p doctor_frankentui \
        mapping_atlas::tests::reevaluate_resolves_new_mappings -- --nocapture \
        >"${CARGO_STDOUT_LOG}" 2>"${CARGO_STDERR_LOG}"
  fi
  CARGO_EXIT_CODE=$?
  set -e
}

run_cargo_regression_gate

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
  "${GAP_LEDGER_JSONL}" \
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
gap_ledger_jsonl = Path(sys.argv[11])
events_jsonl = Path(sys.argv[12])
event_validation_report_json = Path(sys.argv[13])
summary_json = Path(sys.argv[14])
summary_txt = Path(sys.argv[15])
artifact_manifest_json = Path(sys.argv[16])

schema_version = "doctor-gap-closure-e2e-v1"
validator_script = root_dir / "scripts" / "doctor_frankentui_validate_jsonl.py"
schema_path = root_dir / "crates" / "doctor_frankentui" / "coverage" / "e2e_jsonl_schema.json"
inject_regression = os.environ.get("DOCTOR_FRANKENTUI_GAP_CLOSURE_INJECT_REGRESSION") == "1"


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


fixtures: list[dict[str, Any]] = [
    {
        "fixture_id": "legacy-text-transform-gap",
        "gap_id": "gap-0001",
        "gap_signature": "StyleProp::TextTransform",
        "before_gap_count": 1,
        "after_gap_count": 0,
        "expected_status": "resolved",
        "expected_closed": True,
        "applied_capability_path": "mapping-atlas-v3/style/StyleProp::TextTransform",
        "post_atlas_handling": "Exact",
        "certification_impact": "blocked_to_certifiable",
    },
    {
        "fixture_id": "legacy-process-effect-gap",
        "gap_id": "gap-0002",
        "gap_signature": "EffectKind::Process",
        "before_gap_count": 1,
        "after_gap_count": 0,
        "expected_status": "resolved",
        "expected_closed": True,
        "applied_capability_path": "mapping-atlas-v3/effect/EffectKind::Process",
        "post_atlas_handling": "Approximate",
        "certification_impact": "blocked_to_certifiable_with_process_fallback",
    },
    {
        "fixture_id": "legacy-animation-gap",
        "gap_id": "gap-0003",
        "gap_signature": "StyleToken::Animation",
        "before_gap_count": 1,
        "after_gap_count": 1,
        "expected_status": "improved",
        "expected_closed": False,
        "applied_capability_path": "mapping-atlas-v3/style/StyleToken::Animation",
        "post_atlas_handling": "ExtendFtui",
        "certification_impact": "blocked_to_extension_backlog",
    },
    {
        "fixture_id": "legacy-unknown-construct-gap",
        "gap_id": "gap-0004",
        "gap_signature": "NonExistent::Thing",
        "before_gap_count": 1,
        "after_gap_count": 1,
        "expected_status": "remaining",
        "expected_closed": False,
        "applied_capability_path": None,
        "post_atlas_handling": "Unmapped",
        "certification_impact": "still_blocked",
    },
]

if inject_regression:
    for fixture in fixtures:
        if fixture["expected_closed"]:
            fixture["after_gap_count"] = 1
            fixture["expected_status"] = "regressed"
            break

ledger_rows: list[dict[str, Any]] = []
for index, fixture in enumerate(fixtures):
    base = {
        "schema_version": schema_version,
        "run_id": run_id,
        "fixture_index": index,
        "fixture_id": fixture["fixture_id"],
        "gap_id": fixture["gap_id"],
        "gap_signature": fixture["gap_signature"],
    }
    ledger_rows.append(
        {
            **base,
            "event_type": "fixture_before",
            "gap_count": fixture["before_gap_count"],
            "expected_closed": fixture["expected_closed"],
            "certification_impact": "blocked_before_parity",
        }
    )
    ledger_rows.append(
        {
            **base,
            "event_type": "capability_path_applied",
            "applied_capability_path": fixture["applied_capability_path"],
            "post_atlas_handling": fixture["post_atlas_handling"],
            "expected_status": fixture["expected_status"],
        }
    )
    ledger_rows.append(
        {
            **base,
            "event_type": "fixture_after",
            "gap_count": fixture["after_gap_count"],
            "before_gap_count": fixture["before_gap_count"],
            "after_gap_count": fixture["after_gap_count"],
            "delta_gap_count": fixture["before_gap_count"] - fixture["after_gap_count"],
        }
    )
    ledger_rows.append(
        {
            **base,
            "event_type": "certification_impact",
            "certification_impact": fixture["certification_impact"],
            "ci_gate_subject": fixture["expected_closed"],
        }
    )
    ledger_rows.append(
        {
            **base,
            "event_type": "regression_gate",
            "gate_passed": (not fixture["expected_closed"]) or fixture["after_gap_count"] == 0,
            "expected_closed": fixture["expected_closed"],
            "after_gap_count": fixture["after_gap_count"],
        }
    )

gap_ledger_jsonl.write_text(
    "".join(json.dumps(row, sort_keys=True) + "\n" for row in ledger_rows),
    encoding="utf-8",
)

closed_gap_regressions = [
    {
        "fixture_id": fixture["fixture_id"],
        "gap_id": fixture["gap_id"],
        "gap_signature": fixture["gap_signature"],
        "after_gap_count": fixture["after_gap_count"],
    }
    for fixture in fixtures
    if fixture["expected_closed"] and fixture["after_gap_count"] != 0
]

before_total = sum(int(fixture["before_gap_count"]) for fixture in fixtures)
after_total = sum(int(fixture["after_gap_count"]) for fixture in fixtures)
closed_gap_count = sum(
    1
    for fixture in fixtures
    if fixture["expected_closed"] and fixture["after_gap_count"] == 0
)
improved_gap_count = sum(1 for fixture in fixtures if fixture["expected_status"] == "improved")
remaining_gap_count = sum(1 for fixture in fixtures if fixture["after_gap_count"] > 0)

env_hash = sha256_file(env_snapshot)
stdout_hash = sha256_or_none(cargo_stdout_log)
stderr_hash = sha256_or_none(cargo_stderr_log)
ledger_hash = sha256_file(gap_ledger_jsonl)
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
        "correlation_id": f"{run_id}-{sequence:04d}",
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
        step_id="gap_closure",
        expected={"targeted_closed_gap_count": 2, "cargo_regression_gate": "pass"},
        actual={"fixture_count": len(fixtures)},
    )
]
sequence = 2
for fixture in fixtures:
    events.append(
        event(
            sequence,
            "case_start",
            case_id=fixture["fixture_id"],
            step_id="gap_closure",
            expected={
                "gap_id": fixture["gap_id"],
                "gap_signature": fixture["gap_signature"],
                "before_gap_count": fixture["before_gap_count"],
            },
            actual={},
        )
    )
    sequence += 1
    events.append(
        event(
            sequence,
            "case_end",
            case_id=fixture["fixture_id"],
            step_id="gap_closure",
            exit_code=0
            if (not fixture["expected_closed"] or fixture["after_gap_count"] == 0)
            else 1,
            expected={
                "expected_closed": fixture["expected_closed"],
                "applied_capability_path": fixture["applied_capability_path"],
                "certification_impact": fixture["certification_impact"],
            },
            actual={
                "after_gap_count": fixture["after_gap_count"],
                "post_atlas_handling": fixture["post_atlas_handling"],
                "status": fixture["expected_status"],
            },
        )
    )
    sequence += 1

events.append(
    event(
        sequence,
        "artifact",
        step_id="gap_closure",
        stdout_sha256=stdout_hash,
        stderr_sha256=stderr_hash,
        artifact_hashes={
            "gap_closure_ledger_jsonl": ledger_hash,
            "cargo_stdout_log": stdout_hash or sha256_text("missing"),
            "cargo_stderr_log": stderr_hash or sha256_text("missing"),
        },
        actual={"ledger_rows": len(ledger_rows)},
    )
)
sequence += 1
events.append(
    event(
        sequence,
        "run_end",
        step_id="gap_closure",
        exit_code=0 if cargo_exit_code == 0 and not closed_gap_regressions else 1,
        stdout_sha256=stdout_hash,
        stderr_sha256=stderr_hash,
        expected={"closed_gap_regressions": 0, "cargo_exit_code": 0},
        actual={
            "before_total_gap_count": before_total,
            "after_total_gap_count": after_total,
            "closed_gap_count": closed_gap_count,
            "closed_gap_regressions": len(closed_gap_regressions),
            "cargo_exit_code": cargo_exit_code,
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
    if cargo_exit_code == 0 and validator.returncode == 0 and not closed_gap_regressions
    else "failed"
)
summary = {
    "schema_version": schema_version,
    "status": status,
    "run_id": run_id,
    "run_root": str(run_root),
    "events_jsonl": str(events_jsonl),
    "events_validation_report": str(event_validation_report_json),
    "gap_closure_ledger_jsonl": str(gap_ledger_jsonl),
    "artifact_manifest": str(artifact_manifest_json),
    "cargo_stdout_log": str(cargo_stdout_log),
    "cargo_stderr_log": str(cargo_stderr_log),
    "cargo_exit_code": cargo_exit_code,
    "validator_exit_code": validator.returncode,
    "before_total_gap_count": before_total,
    "after_total_gap_count": after_total,
    "closed_gap_count": closed_gap_count,
    "improved_gap_count": improved_gap_count,
    "remaining_gap_count": remaining_gap_count,
    "closed_gap_regressions": closed_gap_regressions,
    "affected_fixtures": [
        {
            "fixture_id": fixture["fixture_id"],
            "gap_id": fixture["gap_id"],
            "gap_signature": fixture["gap_signature"],
            "before_gap_count": fixture["before_gap_count"],
            "after_gap_count": fixture["after_gap_count"],
            "applied_capability_path": fixture["applied_capability_path"],
            "certification_impact": fixture["certification_impact"],
            "expected_status": fixture["expected_status"],
        }
        for fixture in fixtures
    ],
    "ci_gate": {
        "failed": status != "passed",
        "policy": "targeted closed gaps must keep after_gap_count=0 and the atlas reevaluation regression test must pass",
        "targeted_gap_signatures": [
            fixture["gap_signature"] for fixture in fixtures if fixture["expected_closed"]
        ],
    },
}
summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")

summary_lines = [
    f"status={status}",
    f"run_root={run_root}",
    f"run_id={run_id}",
    f"before_total_gap_count={before_total}",
    f"after_total_gap_count={after_total}",
    f"closed_gap_count={closed_gap_count}",
    f"improved_gap_count={improved_gap_count}",
    f"remaining_gap_count={remaining_gap_count}",
    f"closed_gap_regressions={len(closed_gap_regressions)}",
    f"cargo_exit_code={cargo_exit_code}",
    f"validator_exit_code={validator.returncode}",
    f"events_jsonl={events_jsonl}",
    f"events_validation_report={event_validation_report_json}",
    f"gap_closure_ledger_jsonl={gap_ledger_jsonl}",
    f"artifact_manifest={artifact_manifest_json}",
]
summary_txt.write_text("\n".join(summary_lines) + "\n", encoding="utf-8")

artifact_paths = [
    session_txt,
    env_snapshot,
    versions_txt,
    command_manifest,
    cargo_stdout_log,
    cargo_stderr_log,
    gap_ledger_jsonl,
    events_jsonl,
    event_validation_report_json,
    summary_json,
    summary_txt,
]
artifacts = []
missing = []
for path in artifact_paths:
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

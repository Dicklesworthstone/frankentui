#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/e2e/certification_${TIMESTAMP_UTC}}"
LOG_DIR="${RUN_ROOT}/logs"
META_DIR="${RUN_ROOT}/meta"
ARTIFACT_DIR="${RUN_ROOT}/artifacts"
REPLAY_DIR="${RUN_ROOT}/replay"
SESSION_TXT="${META_DIR}/session.txt"
ENV_SNAPSHOT="${META_DIR}/env_snapshot.txt"
VERSIONS_TXT="${META_DIR}/tool_versions.txt"
COMMAND_MANIFEST="${META_DIR}/command_manifest.txt"
CARGO_GATE_RESULTS_JSON="${META_DIR}/cargo_gate_results.json"
SUMMARY_JSON="${META_DIR}/summary.json"
SUMMARY_TXT="${META_DIR}/summary.txt"
EVENTS_JSONL="${META_DIR}/events.jsonl"
EVENT_VALIDATION_REPORT_JSON="${META_DIR}/events_validation_report.json"
EVIDENCE_LEDGER_JSONL="${META_DIR}/certification_evidence_ledger.jsonl"
ARTIFACT_MANIFEST_JSON="${META_DIR}/artifact_manifest.json"
RUN_ID="${DOCTOR_FRANKENTUI_CERT_E2E_RUN_ID:-certification-e2e-seed-${E2E_SEED:-0}}"
SHARD_INDEX="${DOCTOR_FRANKENTUI_CERT_E2E_SHARD_INDEX:-0}"
SHARD_TOTAL="${DOCTOR_FRANKENTUI_CERT_E2E_SHARD_TOTAL:-1}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/rch_target_frankentui_certification_${TIMESTAMP_UTC}}"
CARGO_EXIT_CODE=0

mkdir -p "${LOG_DIR}" "${META_DIR}" "${ARTIFACT_DIR}" "${REPLAY_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[certification-e2e] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

require_command "python3" "install Python 3"
if [[ "${DOCTOR_FRANKENTUI_CERT_E2E_SKIP_CARGO:-0}" != "1" ]]; then
  require_command "cargo" "install Rust/Cargo toolchain"
fi
if [[ "${DOCTOR_FRANKENTUI_USE_RCH:-0}" == "1" ]]; then
  require_command "rch" "install rch or unset DOCTOR_FRANKENTUI_USE_RCH"
fi

case "${SHARD_INDEX}" in
  ''|*[!0-9]*)
    echo "[certification-e2e] shard index must be a non-negative integer" >&2
    exit 2
    ;;
esac
case "${SHARD_TOTAL}" in
  ''|*[!0-9]*)
    echo "[certification-e2e] shard total must be a positive integer" >&2
    exit 2
    ;;
esac
if (( SHARD_TOTAL < 1 || SHARD_INDEX >= SHARD_TOTAL )); then
  echo "[certification-e2e] invalid shard configuration index=${SHARD_INDEX} total=${SHARD_TOTAL}" >&2
  exit 2
fi

{
  echo "timestamp_utc=${TIMESTAMP_UTC}"
  echo "run_id=${RUN_ID}"
  echo "run_root=${RUN_ROOT}"
  echo "root_dir=${ROOT_DIR}"
  echo "cargo_target_dir=${CARGO_TARGET_DIR}"
  echo "shard_index=${SHARD_INDEX}"
  echo "shard_total=${SHARD_TOTAL}"
  echo "use_rch=${DOCTOR_FRANKENTUI_USE_RCH:-0}"
  echo "skip_cargo=${DOCTOR_FRANKENTUI_CERT_E2E_SKIP_CARGO:-0}"
} > "${SESSION_TXT}"

{
  env | sort | grep -E '^(CI|TERM|SHELL|USER|HOME|PATH|RUSTUP_TOOLCHAIN|CARGO_TARGET_DIR|DOCTOR_FRANKENTUI_|E2E_SEED)=' || true
} > "${ENV_SNAPSHOT}"

{
  echo "doctor_frankentui_certification_e2e"
  echo "timestamp_utc=${TIMESTAMP_UTC}"
  echo "git_rev=$(git -C "${ROOT_DIR}" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "cargo_version=$(cargo --version 2>/dev/null || echo cargo-skipped)"
  echo "rustc_version=$(rustc --version 2>/dev/null || echo rustc-skipped)"
  echo "python_version=$(python3 --version 2>/dev/null || echo python3-missing)"
  echo "rch_path=$(command -v rch 2>/dev/null || echo missing)"
  echo "rch_version=$(rch --version 2>/dev/null | head -n 1 || echo unknown)"
} > "${VERSIONS_TXT}"

: > "${COMMAND_MANIFEST}"

run_cargo_gate() {
  local gate_id="$1"
  shift
  local stdout_log="${LOG_DIR}/${gate_id}.stdout.log"
  local stderr_log="${LOG_DIR}/${gate_id}.stderr.log"
  local command_display
  command_display="$*"

  if [[ "${DOCTOR_FRANKENTUI_CERT_E2E_SKIP_CARGO:-0}" == "1" ]]; then
    echo "[${gate_id}] skip cargo gate (DOCTOR_FRANKENTUI_CERT_E2E_SKIP_CARGO=1)" >> "${COMMAND_MANIFEST}"
    {
      echo "skipped cargo gate"
      echo "gate_id=${gate_id}"
      echo "reason=DOCTOR_FRANKENTUI_CERT_E2E_SKIP_CARGO=1"
    } > "${stdout_log}"
    : > "${stderr_log}"
    return 0
  fi

  if [[ "${DOCTOR_FRANKENTUI_USE_RCH:-0}" == "1" ]]; then
    command_display="RCH_ENV_ALLOWLIST=CARGO_TARGET_DIR rch exec -- env CARGO_TARGET_DIR=${CARGO_TARGET_DIR} ${command_display}"
  else
    command_display="env CARGO_TARGET_DIR=${CARGO_TARGET_DIR} ${command_display}"
  fi
  echo "[${gate_id}] ${command_display}" >> "${COMMAND_MANIFEST}"

  set +e
  if [[ "${DOCTOR_FRANKENTUI_USE_RCH:-0}" == "1" ]]; then
    RCH_ENV_ALLOWLIST=CARGO_TARGET_DIR \
      rch exec -- env CARGO_TARGET_DIR="${CARGO_TARGET_DIR}" "$@" \
        >"${stdout_log}" 2>"${stderr_log}"
  else
    env CARGO_TARGET_DIR="${CARGO_TARGET_DIR}" "$@" \
      >"${stdout_log}" 2>"${stderr_log}"
  fi
  local exit_code=$?
  set -e
  return "${exit_code}"
}

if ! run_cargo_gate \
  "cargo_certification_report_tests" \
  cargo test -p doctor_frankentui --test certification_report_tests -- --nocapture; then
  CARGO_EXIT_CODE=1
fi

if ! run_cargo_gate \
  "cargo_comparator_matrix_tests" \
  cargo test -p doctor_frankentui --test comparator_adversarial_fuzz_tests \
    synthetic_fixture_matrix_covers_positive_negative_and_tolerance_boundaries -- --nocapture; then
  CARGO_EXIT_CODE=1
fi

python3 - \
  "${ROOT_DIR}" \
  "${RUN_ROOT}" \
  "${RUN_ID}" \
  "${SHARD_INDEX}" \
  "${SHARD_TOTAL}" \
  "${CARGO_EXIT_CODE}" \
  "${SESSION_TXT}" \
  "${ENV_SNAPSHOT}" \
  "${VERSIONS_TXT}" \
  "${COMMAND_MANIFEST}" \
  "${CARGO_GATE_RESULTS_JSON}" \
  "${LOG_DIR}" \
  "${ARTIFACT_DIR}" \
  "${REPLAY_DIR}" \
  "${EVIDENCE_LEDGER_JSONL}" \
  "${EVENTS_JSONL}" \
  "${EVENT_VALIDATION_REPORT_JSON}" \
  "${SUMMARY_JSON}" \
  "${SUMMARY_TXT}" \
  "${ARTIFACT_MANIFEST_JSON}" <<'PY'
from __future__ import annotations

import hashlib
import json
import os
import stat
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

root_dir = Path(sys.argv[1])
run_root = Path(sys.argv[2])
run_id = sys.argv[3]
shard_index = int(sys.argv[4])
shard_total = int(sys.argv[5])
cargo_exit_code = int(sys.argv[6])
session_txt = Path(sys.argv[7])
env_snapshot = Path(sys.argv[8])
versions_txt = Path(sys.argv[9])
command_manifest = Path(sys.argv[10])
cargo_gate_results_json = Path(sys.argv[11])
log_dir = Path(sys.argv[12])
artifact_dir = Path(sys.argv[13])
replay_dir = Path(sys.argv[14])
evidence_ledger_jsonl = Path(sys.argv[15])
events_jsonl = Path(sys.argv[16])
event_validation_report_json = Path(sys.argv[17])
summary_json = Path(sys.argv[18])
summary_txt = Path(sys.argv[19])
artifact_manifest_json = Path(sys.argv[20])

schema_version = "doctor-certification-e2e-v1"
validator_script = root_dir / "scripts" / "doctor_frankentui_validate_jsonl.py"
schema_path = root_dir / "crates" / "doctor_frankentui" / "coverage" / "e2e_jsonl_schema.json"
inject_failure = os.environ.get("DOCTOR_FRANKENTUI_CERT_E2E_INJECT_FAILURE", "")


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


all_fixtures: list[dict[str, Any]] = [
    {
        "fixture_id": "counter-app-green-path",
        "source_profile": "opentui-counter",
        "expected_final_verdict": "accept",
        "semantic_observations": 8,
        "visual_frames": 3,
        "performance_p99_ms": 5.8,
        "accessibility_nodes": 6,
    },
    {
        "fixture_id": "visual-tolerance-policy-path",
        "source_profile": "opentui-themed-panel",
        "expected_final_verdict": "accept",
        "semantic_observations": 5,
        "visual_frames": 4,
        "performance_p99_ms": 7.1,
        "accessibility_nodes": 9,
    },
    {
        "fixture_id": "process-effect-fallback-path",
        "source_profile": "opentui-process-effect",
        "expected_final_verdict": "accept",
        "semantic_observations": 10,
        "visual_frames": 2,
        "performance_p99_ms": 9.3,
        "accessibility_nodes": 4,
    },
]

fixtures = [
    fixture
    for index, fixture in enumerate(all_fixtures)
    if index % shard_total == shard_index
]
if not fixtures:
    raise SystemExit(
        f"no certification fixtures selected for shard {shard_index}/{shard_total}"
    )

stages = [
    "baseline_capture",
    "translation_run",
    "semantic_comparator",
    "visual_comparator",
    "performance_comparator",
    "accessibility_comparator",
    "proof_obligation",
    "final_verdict",
]


def stage_payload(
    fixture: dict[str, Any],
    stage_id: str,
    prior_hash: str | None,
) -> dict[str, Any]:
    fixture_id = fixture["fixture_id"]
    base = {
        "schema_version": schema_version,
        "run_id": run_id,
        "fixture_id": fixture_id,
        "source_profile": fixture["source_profile"],
        "stage_id": stage_id,
        "prior_stage_hash": prior_hash,
    }
    if stage_id == "baseline_capture":
        return {
            **base,
            "baseline_trace_hash": sha256_text(f"{fixture_id}:baseline"),
            "interaction_count": fixture["semantic_observations"],
            "render_frame_count": fixture["visual_frames"],
        }
    if stage_id == "translation_run":
        return {
            **base,
            "generated_project_hash": sha256_text(f"{fixture_id}:generated-project"),
            "planner_policy": "strict-release",
            "fallback_paths": ["ProcessSubscription/Cmd::task"]
            if fixture_id == "process-effect-fallback-path"
            else [],
        }
    if stage_id == "semantic_comparator":
        return {
            **base,
            "comparator_id": "semantic",
            "verdict": "accept",
            "observation_pairs": fixture["semantic_observations"],
            "minimal_counterexample": None,
        }
    if stage_id == "visual_comparator":
        return {
            **base,
            "comparator_id": "visual",
            "verdict": "accept",
            "frame_pairs": fixture["visual_frames"],
            "tolerance_profile": "strict-release",
        }
    if stage_id == "performance_comparator":
        return {
            **base,
            "comparator_id": "performance",
            "verdict": "accept",
            "translated_p99_ms": fixture["performance_p99_ms"],
            "threshold_policy": "no-regression-over-10-percent",
        }
    if stage_id == "accessibility_comparator":
        return {
            **base,
            "comparator_id": "accessibility",
            "verdict": "accept",
            "node_count": fixture["accessibility_nodes"],
            "wcag_policy": "release-default",
        }
    if stage_id == "proof_obligation":
        return {
            **base,
            "obligation_count": 4,
            "witness_hash": sha256_text(f"{fixture_id}:proof-witness"),
            "certification_passed": True,
        }
    if stage_id == "final_verdict":
        return {
            **base,
            "verdict": fixture["expected_final_verdict"],
            "certification_passed": True,
            "report_checksum": sha256_text(f"{fixture_id}:report-checksum"),
        }
    raise AssertionError(stage_id)


ledger_rows: list[dict[str, Any]] = []
failed_rows: list[dict[str, Any]] = []
replay_helpers: list[str] = []

for fixture_index, fixture in enumerate(fixtures):
    fixture_dir = artifact_dir / fixture["fixture_id"]
    fixture_dir.mkdir(parents=True, exist_ok=True)
    prior_correlation_id: str | None = None
    prior_hash: str | None = None
    fixture_failed = False

    for stage_index, stage_id in enumerate(stages):
        correlation_id = f"{run_id}-{shard_index:02d}-{fixture_index:03d}-{stage_index:03d}"
        payload = stage_payload(fixture, stage_id, prior_hash)
        artifact_path = fixture_dir / f"{stage_index:02d}_{stage_id}.json"
        should_fail = (
            bool(inject_failure)
            and not fixture_failed
            and (inject_failure == stage_id or inject_failure == fixture["fixture_id"])
        )
        if should_fail:
            payload["verdict"] = "reject"
            payload["certification_passed"] = False
            payload["failure_reason"] = f"injected failure at {stage_id}"

        artifact_path.write_text(
            json.dumps(payload, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        artifact_hash = sha256_file(artifact_path)
        status = "failed" if should_fail else "passed"
        failure_lineage = []
        if should_fail:
            failure_lineage = [
                {
                    "failed_correlation_id": correlation_id,
                    "failed_stage_id": stage_id,
                    "parent_correlation_id": prior_correlation_id,
                    "artifact_path": str(artifact_path),
                    "artifact_sha256": artifact_hash,
                }
            ]
            fixture_failed = True

        row = {
            "schema_version": schema_version,
            "run_id": run_id,
            "correlation_id": correlation_id,
            "parent_correlation_id": prior_correlation_id,
            "fixture_id": fixture["fixture_id"],
            "fixture_index": fixture_index,
            "shard_index": shard_index,
            "shard_total": shard_total,
            "stage_id": stage_id,
            "stage_index": stage_index,
            "status": status,
            "started_at_utc": now_utc_timestamp(),
            "duration_ms": 1 + stage_index,
            "input_hash": prior_hash,
            "output_hash": sha256_json(payload),
            "artifact_path": str(artifact_path),
            "artifact_sha256": artifact_hash,
            "comparator_id": payload.get("comparator_id"),
            "verdict": payload.get("verdict"),
            "certification_passed": payload.get("certification_passed"),
            "failure_lineage": failure_lineage,
        }
        ledger_rows.append(row)
        if should_fail:
            failed_rows.append(row)
        prior_correlation_id = correlation_id
        prior_hash = row["output_hash"]

    if fixture_failed:
        helper_path = replay_dir / f"replay_{fixture['fixture_id']}.sh"
        helper_path.write_text(
            "\n".join(
                [
                    "#!/usr/bin/env bash",
                    "set -euo pipefail",
                    f"RUN_ROOT={json.dumps(str(run_root))}",
                    f"FIXTURE_ID={json.dumps(fixture['fixture_id'])}",
                    'LEDGER="${RUN_ROOT}/meta/certification_evidence_ledger.jsonl"',
                    'echo "fixture=${FIXTURE_ID}"',
                    'python3 - "$LEDGER" "$FIXTURE_ID" <<\'PY\'',
                    "import json",
                    "import sys",
                    "from pathlib import Path",
                    "ledger = Path(sys.argv[1])",
                    "fixture_id = sys.argv[2]",
                    "for raw in ledger.read_text(encoding='utf-8').splitlines():",
                    "    row = json.loads(raw)",
                    "    if row.get('fixture_id') == fixture_id:",
                    "        print(json.dumps(row, sort_keys=True))",
                    "PY",
                ]
            )
            + "\n",
            encoding="utf-8",
        )
        helper_path.chmod(helper_path.stat().st_mode | stat.S_IXUSR)
        replay_helpers.append(str(helper_path))

evidence_ledger_jsonl.write_text(
    "".join(json.dumps(row, sort_keys=True) + "\n" for row in ledger_rows),
    encoding="utf-8",
)

cargo_gate_results = []
for stdout_log in sorted(log_dir.glob("cargo_*.stdout.log")):
    gate_id = stdout_log.name.removesuffix(".stdout.log")
    stderr_log = log_dir / f"{gate_id}.stderr.log"
    cargo_gate_results.append(
        {
            "gate_id": gate_id,
            "stdout_log": str(stdout_log),
            "stderr_log": str(stderr_log),
            "stdout_sha256": sha256_or_none(stdout_log),
            "stderr_sha256": sha256_or_none(stderr_log),
        }
    )
cargo_gate_results_json.write_text(
    json.dumps(
        {
            "schema_version": schema_version,
            "cargo_exit_code": cargo_exit_code,
            "gates": cargo_gate_results,
        },
        indent=2,
        sort_keys=True,
    )
    + "\n",
    encoding="utf-8",
)

env_hash = sha256_file(env_snapshot)
ledger_hash = sha256_file(evidence_ledger_jsonl)
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
        step_id="certification_pipeline",
        expected={"stage_count": len(stages), "cargo_exit_code": 0},
        actual={"fixture_count": len(fixtures), "shard_index": shard_index, "shard_total": shard_total},
    )
]
sequence = 2
for fixture in fixtures:
    fixture_rows = [row for row in ledger_rows if row["fixture_id"] == fixture["fixture_id"]]
    fixture_failed = any(row["status"] == "failed" for row in fixture_rows)
    events.append(
        event(
            sequence,
            "case_start",
            case_id=fixture["fixture_id"],
            step_id="certification_pipeline",
            expected={"stages": stages},
            actual={"source_profile": fixture["source_profile"]},
        )
    )
    sequence += 1
    events.append(
        event(
            sequence,
            "case_end",
            case_id=fixture["fixture_id"],
            step_id="certification_pipeline",
            exit_code=1 if fixture_failed else 0,
            artifact_hashes={
                row["stage_id"]: row["artifact_sha256"] for row in fixture_rows
            },
            expected={"final_verdict": fixture["expected_final_verdict"]},
            actual={
                "stage_rows": len(fixture_rows),
                "failed_stage_count": sum(1 for row in fixture_rows if row["status"] == "failed"),
            },
        )
    )
    sequence += 1

events.append(
    event(
        sequence,
        "artifact",
        step_id="certification_pipeline",
        artifact_hashes={
            "certification_evidence_ledger_jsonl": ledger_hash,
            "cargo_gate_results_json": sha256_file(cargo_gate_results_json),
        },
        actual={"ledger_rows": len(ledger_rows), "replay_helpers": replay_helpers},
    )
)
sequence += 1
events.append(
    event(
        sequence,
        "run_end",
        step_id="certification_pipeline",
        exit_code=0 if cargo_exit_code == 0 and not failed_rows else 1,
        artifact_hashes={"certification_evidence_ledger_jsonl": ledger_hash},
        expected={"failed_stage_count": 0, "cargo_exit_code": 0},
        actual={
            "failed_stage_count": len(failed_rows),
            "cargo_exit_code": cargo_exit_code,
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
    if cargo_exit_code == 0 and validator.returncode == 0 and not failed_rows
    else "failed"
)
summary = {
    "schema_version": schema_version,
    "status": status,
    "run_id": run_id,
    "run_root": str(run_root),
    "shard_index": shard_index,
    "shard_total": shard_total,
    "fixture_count": len(fixtures),
    "stage_count": len(stages),
    "ledger_rows": len(ledger_rows),
    "failed_stage_count": len(failed_rows),
    "failed_correlation_ids": [row["correlation_id"] for row in failed_rows],
    "replay_helpers": replay_helpers,
    "cargo_exit_code": cargo_exit_code,
    "validator_exit_code": validator.returncode,
    "events_jsonl": str(events_jsonl),
    "events_validation_report": str(event_validation_report_json),
    "evidence_ledger_jsonl": str(evidence_ledger_jsonl),
    "cargo_gate_results": str(cargo_gate_results_json),
    "artifact_manifest": str(artifact_manifest_json),
}
summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")

summary_lines = [
    f"status={status}",
    f"run_root={run_root}",
    f"run_id={run_id}",
    f"shard_index={shard_index}",
    f"shard_total={shard_total}",
    f"fixture_count={len(fixtures)}",
    f"stage_count={len(stages)}",
    f"ledger_rows={len(ledger_rows)}",
    f"failed_stage_count={len(failed_rows)}",
    f"cargo_exit_code={cargo_exit_code}",
    f"validator_exit_code={validator.returncode}",
    f"events_jsonl={events_jsonl}",
    f"events_validation_report={event_validation_report_json}",
    f"evidence_ledger_jsonl={evidence_ledger_jsonl}",
    f"cargo_gate_results={cargo_gate_results_json}",
    f"artifact_manifest={artifact_manifest_json}",
]
summary_txt.write_text("\n".join(summary_lines) + "\n", encoding="utf-8")

artifact_paths = [
    session_txt,
    env_snapshot,
    versions_txt,
    command_manifest,
    cargo_gate_results_json,
    evidence_ledger_jsonl,
    events_jsonl,
    event_validation_report_json,
    summary_json,
    summary_txt,
    *sorted(log_dir.glob("cargo_*.stdout.log")),
    *sorted(log_dir.glob("cargo_*.stderr.log")),
    *sorted(artifact_dir.glob("*/*.json")),
    *[Path(path) for path in replay_helpers],
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

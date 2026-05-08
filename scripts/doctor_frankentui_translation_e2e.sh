#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/e2e/translation_${TIMESTAMP_UTC}}"
LOG_DIR="${RUN_ROOT}/logs"
META_DIR="${RUN_ROOT}/meta"
SUMMARY_JSON="${META_DIR}/summary.json"
SUMMARY_TXT="${META_DIR}/summary.txt"
VALIDATION_REPORT_JSON="${META_DIR}/validation_report.json"
EVENTS_JSONL="${META_DIR}/events.jsonl"
STDOUT_LOG="${LOG_DIR}/cargo_test.stdout.log"
STDERR_LOG="${LOG_DIR}/cargo_test.stderr.log"
RUN_ID="${DOCTOR_FRANKENTUI_TRANSLATION_E2E_RUN_ID:-translation-e2e-seed-${E2E_SEED:-0}}"
CARGO_BIN="${CARGO:-cargo}"
REPRODUCTION_COMMAND="DOCTOR_FRANKENTUI_TRANSLATION_E2E_RUN_ID=${RUN_ID} ${BASH_SOURCE[0]} ${RUN_ROOT}"

mkdir -p "${LOG_DIR}" "${META_DIR}"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[translation-e2e] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

write_failure() {
  local parser_stage="$1"
  local fixture_id="$2"
  local reason="$3"
  python3 - \
    "${SUMMARY_JSON}" \
    "${SUMMARY_TXT}" \
    "${VALIDATION_REPORT_JSON}" \
    "${EVENTS_JSONL}" \
    "${RUN_ROOT}" \
    "${RUN_ID}" \
    "${parser_stage}" \
    "${fixture_id}" \
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
fixture_id = sys.argv[8]
reason = sys.argv[9]
stdout_log = Path(sys.argv[10])
stderr_log = Path(sys.argv[11])
reproduction_command = sys.argv[12]

event = {
    "schema_version": "doctor-translation-e2e-v1",
    "run_id": run_id,
    "fixture_id": fixture_id,
    "fixture_index": -1,
    "fixture_path": None,
    "parser_stage": parser_stage,
    "stage_index": -1,
    "status": "failed",
    "duration_ms": 0,
    "counts": {},
    "hashes": {},
    "stage_hash": None,
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
    "fixture_id": fixture_id,
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
    "fixture_id": fixture_id,
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
            f"fixture_id={fixture_id}",
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

validate_and_build() {
  python3 - \
    "${SUMMARY_JSON}" \
    "${SUMMARY_TXT}" \
    "${VALIDATION_REPORT_JSON}" \
    "${EVENTS_JSONL}" \
    "${RUN_ROOT}" \
    "${RUN_ID}" \
    "${REPRODUCTION_COMMAND}" \
    "${CARGO_BIN}" <<'PY'
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import tarfile
import time
from pathlib import Path
from typing import Any

summary_json = Path(sys.argv[1])
summary_txt = Path(sys.argv[2])
validation_report_json = Path(sys.argv[3])
events_jsonl = Path(sys.argv[4])
run_root = Path(sys.argv[5])
run_id = sys.argv[6]
reproduction_command = sys.argv[7]
cargo_bin = sys.argv[8]

meta_dir = run_root / "meta"
log_dir = run_root / "logs"
artifacts_dir = run_root / "artifacts"
manifest_path = meta_dir / "translation_manifest.json"
ledger_path = meta_dir / "translation_ledger.jsonl"
generated_root = run_root / "generated"
source_root = run_root / "source"
generated_tar = artifacts_dir / "generated_sources.tar"
required_stages = [
    "ingest",
    "ir_lower",
    "plan",
    "translate",
    "emit",
    "optimize",
    "write_generated",
]
schema_version = "doctor-translation-e2e-v1"


def fail(stage: str, fixture_id: str, message: str) -> None:
    raise SystemExit(json.dumps({"stage": stage, "fixture_id": fixture_id, "message": message}))


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


def read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        fail("manifest", "__run__", f"missing {path}")
    except json.JSONDecodeError as exc:
        fail("manifest", "__run__", f"invalid JSON in {path}: {exc}")


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    try:
        raw_lines = path.read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        fail("ledger", "__run__", f"missing {path}")
    rows: list[dict[str, Any]] = []
    for line_no, raw in enumerate(raw_lines, start=1):
        if not raw.strip():
            fail("ledger", "__run__", f"blank line {line_no} in {path}")
        try:
            row = json.loads(raw)
        except json.JSONDecodeError as exc:
            fail("ledger", "__run__", f"invalid JSONL line {line_no}: {exc}")
        if not isinstance(row, dict):
            fail("ledger", "__run__", f"line {line_no} is not an object")
        rows.append(row)
    return rows


def validate_prebuild_row(row: dict[str, Any]) -> None:
    fixture_id = row.get("fixture_id")
    parser_stage = row.get("parser_stage")
    if not isinstance(fixture_id, str) or not fixture_id:
        fail("ledger", "__run__", "row missing fixture_id")
    if parser_stage not in required_stages:
        fail("ledger", fixture_id, f"unexpected parser_stage {parser_stage!r}")
    if row.get("schema_version") != schema_version:
        fail(str(parser_stage), fixture_id, "wrong schema_version")
    if row.get("run_id") != run_id:
        fail(str(parser_stage), fixture_id, "run_id mismatch")
    if row.get("status") != "ok":
        fail(str(parser_stage), fixture_id, "prebuild row is not ok")
    counts = row.get("counts")
    hashes = row.get("hashes")
    if not isinstance(counts, dict) or not isinstance(hashes, dict):
        fail(str(parser_stage), fixture_id, "counts and hashes must be objects")
    expected_hash = sha256_json(
        {
            "counts": counts,
            "fixture_id": fixture_id,
            "hashes": hashes,
            "parser_stage": parser_stage,
        }
    )
    if row.get("stage_hash") != expected_hash:
        fail(str(parser_stage), fixture_id, "stage_hash mismatch")
    command = row.get("reproduction_command")
    if not isinstance(command, str) or "doctor_frankentui_translation_e2e.sh" not in command:
        fail(str(parser_stage), fixture_id, "missing replay command")


def append_build_row(
    ledger_rows: list[dict[str, Any]],
    fixture: dict[str, Any],
    status: str,
    duration_ms: int,
    stdout_path: Path,
    stderr_path: Path,
    return_code: int,
) -> dict[str, Any]:
    fixture_id = str(fixture["fixture_id"])
    counts = {
        "return_code": return_code,
        "stdout_bytes": stdout_path.stat().st_size if stdout_path.exists() else 0,
        "stderr_bytes": stderr_path.stat().st_size if stderr_path.exists() else 0,
    }
    hashes = {
        "stdout_sha256": sha256_file(stdout_path) if stdout_path.exists() else None,
        "stderr_sha256": sha256_file(stderr_path) if stderr_path.exists() else None,
    }
    stage_hash = sha256_json(
        {
            "counts": counts,
            "fixture_id": fixture_id,
            "hashes": hashes,
            "parser_stage": "build",
        }
    )
    row = {
        "schema_version": schema_version,
        "run_id": run_id,
        "fixture_id": fixture_id,
        "fixture_index": fixture["fixture_index"],
        "fixture_path": fixture["source_path"],
        "parser_stage": "build",
        "stage_index": len(ledger_rows),
        "status": status,
        "duration_ms": duration_ms,
        "counts": counts,
        "hashes": hashes,
        "stage_hash": stage_hash,
        "diagnostics": []
        if status == "ok"
        else [
            {
                "message": f"cargo check failed with exit {return_code}",
                "stdout_log": str(stdout_path),
                "stderr_log": str(stderr_path),
            }
        ],
        "reproduction_command": reproduction_command,
    }
    ledger_rows.append(row)
    return row


def write_ledger(rows: list[dict[str, Any]]) -> None:
    ledger_path.write_text(
        "".join(json.dumps(row, sort_keys=True) + "\n" for row in rows),
        encoding="utf-8",
    )
    events_jsonl.write_text(ledger_path.read_text(encoding="utf-8"), encoding="utf-8")


def create_generated_tar() -> str:
    artifacts_dir.mkdir(parents=True, exist_ok=True)
    with tarfile.open(generated_tar, "w") as archive:
        for path in sorted(generated_root.rglob("*")):
            if not path.is_file():
                continue
            arcname = path.relative_to(run_root).as_posix()
            info = archive.gettarinfo(str(path), arcname)
            info.uid = 0
            info.gid = 0
            info.uname = ""
            info.gname = ""
            info.mtime = 0
            info.mode = 0o755 if os.access(path, os.X_OK) else 0o644
            with path.open("rb") as handle:
                archive.addfile(info, handle)
    return sha256_file(generated_tar)


manifest = read_json(manifest_path)
if manifest.get("schema_version") != "doctor-translation-e2e-manifest-v1":
    fail("manifest", "__run__", "schema_version mismatch")
if manifest.get("run_id") != run_id:
    fail("manifest", "__run__", "run_id mismatch")
if manifest.get("required_stages") != required_stages:
    fail("manifest", "__run__", "required_stages mismatch")

fixtures = manifest.get("fixtures")
if not isinstance(fixtures, list) or len(fixtures) != 3:
    fail("manifest", "__run__", "expected exactly 3 fixtures")
fixture_ids = [fixture.get("fixture_id") for fixture in fixtures]
if fixture_ids != ["counter", "status-panel", "search-box"]:
    fail("manifest", "__run__", f"fixture order mismatch: {fixture_ids!r}")

rows = read_jsonl(ledger_path)
if manifest.get("prebuild_ledger_line_count") != len(rows):
    fail("ledger", "__run__", "prebuild ledger count mismatch")
seen: dict[str, set[str]] = {}
for row in rows:
    validate_prebuild_row(row)
    seen.setdefault(str(row["fixture_id"]), set()).add(str(row["parser_stage"]))
for fixture in fixtures:
    fixture_id = str(fixture["fixture_id"])
    if seen.get(fixture_id) != set(required_stages):
        fail("ledger", fixture_id, "missing prebuild stage coverage")
    generated_dir = run_root / str(fixture["generated_dir"])
    if not generated_dir.is_dir():
        fail("generated", fixture_id, f"missing generated dir {generated_dir}")
    cargo_toml = generated_dir / "Cargo.toml"
    if not cargo_toml.exists():
        fail("generated", fixture_id, "missing generated Cargo.toml")
    cargo_text = cargo_toml.read_text(encoding="utf-8")
    if "path = " not in cargo_text:
        fail("generated", fixture_id, "generated Cargo.toml must use local path deps")
    source_dir = source_root / fixture_id
    if not source_dir.is_dir():
        fail("source", fixture_id, f"missing captured source dir {source_dir}")

for fixture in fixtures:
    fixture_id = str(fixture["fixture_id"])
    generated_dir = run_root / str(fixture["generated_dir"])
    stdout_path = run_root / str(fixture["build_stdout"])
    stderr_path = run_root / str(fixture["build_stderr"])
    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    stderr_path.parent.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.setdefault("CARGO_TARGET_DIR", str(run_root / "target" / fixture_id))
    command = [
        cargo_bin,
        "check",
        "--manifest-path",
        str(generated_dir / "Cargo.toml"),
    ]
    started = time.monotonic()
    proc = subprocess.run(
        command,
        cwd=generated_dir,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    duration_ms = int((time.monotonic() - started) * 1000)
    stdout_path.write_text(proc.stdout, encoding="utf-8")
    stderr_path.write_text(proc.stderr, encoding="utf-8")
    status = "ok" if proc.returncode == 0 else "failed"
    append_build_row(rows, fixture, status, duration_ms, stdout_path, stderr_path, proc.returncode)
    write_ledger(rows)
    if proc.returncode != 0:
        fail("build", fixture_id, f"cargo check failed with exit {proc.returncode}")

tar_sha256 = create_generated_tar()
write_ledger(rows)

stage_coverage: dict[str, set[str]] = {}
for row in rows:
    stage_coverage.setdefault(str(row["fixture_id"]), set()).add(str(row["parser_stage"]))
for fixture in fixtures:
    fixture_id = str(fixture["fixture_id"])
    expected = set(required_stages + ["build"])
    if stage_coverage.get(fixture_id) != expected:
        fail("ledger", fixture_id, "missing final stage coverage")

report = {
    "status": "passed",
    "run_id": run_id,
    "fixture_count": len(fixtures),
    "ledger_line_count": len(rows),
    "generated_source_tar": str(generated_tar),
    "generated_source_tar_sha256": tar_sha256,
    "generated_root": str(generated_root),
    "source_root": str(source_root),
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
    "ledger": str(ledger_path),
    "events_jsonl": str(events_jsonl),
    "generated_root": str(generated_root),
    "generated_source_tar": str(generated_tar),
    "generated_source_tar_sha256": tar_sha256,
    "validation_report": str(validation_report_json),
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
            f"ledger={ledger_path}",
            f"events_jsonl={events_jsonl}",
            f"generated_root={generated_root}",
            f"generated_source_tar={generated_tar}",
            f"generated_source_tar_sha256={tar_sha256}",
            f"validation_report={validation_report_json}",
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

export DOCTOR_FRANKENTUI_TRANSLATION_E2E_RUN_ROOT="${RUN_ROOT}"
export DOCTOR_FRANKENTUI_TRANSLATION_E2E_RUN_ID="${RUN_ID}"

set +e
(
  cd "${ROOT_DIR}"
  "${CARGO_BIN}" test \
    -p doctor_frankentui \
    --test translation_pipeline_unit_tests \
    translation_e2e_export_has_batch_order_and_generated_projects \
    -- --nocapture
) >"${STDOUT_LOG}" 2>"${STDERR_LOG}"
cargo_status=$?
set -e

if [[ "${cargo_status}" -ne 0 ]]; then
  write_failure "cargo_test" "__run__" "cargo test failed with exit ${cargo_status}"
  exit "${cargo_status}"
fi

set +e
validation_output="$(validate_and_build 2>&1)"
validation_status=$?
set -e

if [[ "${validation_status}" -ne 0 ]]; then
  write_failure "validate_or_build" "__run__" "${validation_output}"
  exit "${validation_status}"
fi

printf '%s' "${validation_output}"

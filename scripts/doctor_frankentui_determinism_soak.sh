#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
COMPARE_ONLY=false
REPORT_DIR=""
if [[ "${1:-}" == "--compare-only" ]]; then
  if [[ $# -lt 3 || $# -gt 4 ]]; then
    echo "usage: $0 --compare-only RUN_ROOT NEW_REPORT_DIR [ITERATIONS]" >&2
    exit 2
  fi
  COMPARE_ONLY=true
  shift
  REPORT_DIR="$2"
  ITERATIONS="${3:-${DOCTOR_FRANKENTUI_SOAK_RUNS:-3}}"
else
  ITERATIONS="${2:-${DOCTOR_FRANKENTUI_SOAK_RUNS:-3}}"
fi
RUN_ROOT="${1:-/tmp/doctor_frankentui/determinism_soak_${TIMESTAMP_UTC}}"
LOG_DIR="${RUN_ROOT}/logs"
META_DIR="${REPORT_DIR:-${RUN_ROOT}/meta}"
RUN_INDEX_TSV="${RUN_ROOT}/meta/run_index.tsv"
REPORT_JSON="${META_DIR}/determinism_report.json"
REPORT_TXT="${META_DIR}/determinism_report.txt"
SCHEMA_PATH="${ROOT_DIR}/crates/doctor_frankentui/coverage/e2e_jsonl_schema.json"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[determinism] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

if [[ ! "${ITERATIONS}" =~ ^[0-9]+$ ]] || [[ "${ITERATIONS}" -lt 2 ]]; then
  echo "[determinism] comparison requires at least two iterations (got: ${ITERATIONS})" >&2
  exit 2
fi

if [[ ! -f "${SCHEMA_PATH}" ]]; then
  echo "[determinism] schema file not found: ${SCHEMA_PATH}" >&2
  exit 2
fi

require_command "bash" "install bash"
require_command "python3" "install Python 3"
if [[ -e "${REPORT_JSON}" || -e "${REPORT_TXT}" ]]; then
  echo "[determinism] reports already exist; choose a new report directory" >&2
  exit 2
fi
if [[ "${COMPARE_ONLY}" == true ]]; then
  if [[ ! -f "${RUN_INDEX_TSV}" ]]; then
    echo "[determinism] retained run index not found: ${RUN_INDEX_TSV}" >&2
    exit 2
  fi
  mkdir -p "${META_DIR}"
else
  require_command "jq" "install jq for JSON checks"
  if [[ -e "${RUN_ROOT}" ]]; then
    echo "[determinism] run root already exists; choose a new run directory" >&2
    exit 2
  fi
  mkdir -p "$(dirname "${RUN_ROOT}")"
  mkdir "${RUN_ROOT}"
  mkdir -p "${LOG_DIR}" "${META_DIR}"
  : > "${RUN_INDEX_TSV}"
fi

run_workflow_iteration() {
  local workflow="$1"
  local iteration="$2"
  local script_path="${ROOT_DIR}/scripts/doctor_frankentui_${workflow}_e2e.sh"
  local run_dir="${RUN_ROOT}/${workflow}_run_${iteration}"
  local stdout_log="${LOG_DIR}/${workflow}_run_${iteration}.stdout.log"
  local stderr_log="${LOG_DIR}/${workflow}_run_${iteration}.stderr.log"
  local summary_json="${run_dir}/meta/summary.json"
  local events_jsonl="${run_dir}/meta/events.jsonl"
  local validation_json="${run_dir}/meta/events_validation_report.json"

  if [[ ! -x "${script_path}" ]]; then
    echo "[determinism] required workflow script missing or not executable: ${script_path}" >&2
    exit 2
  fi

  echo "[determinism] running ${workflow} iteration ${iteration}/${ITERATIONS}"
  set +e
  "${script_path}" "${run_dir}" > "${stdout_log}" 2> "${stderr_log}"
  local exit_code=$?
  set -e

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${workflow}" \
    "${iteration}" \
    "${run_dir}" \
    "${exit_code}" \
    "${summary_json}" \
    "${events_jsonl}" \
    "${validation_json}" \
    "${stdout_log}" \
    "${stderr_log}" >> "${RUN_INDEX_TSV}"
}

if [[ "${COMPARE_ONLY}" == false ]]; then
  for ((i=1; i<=ITERATIONS; i++)); do
    run_workflow_iteration "happy" "${i}"
    run_workflow_iteration "failure" "${i}"
  done
fi

python3 -B - \
  "${RUN_INDEX_TSV}" \
  "${REPORT_JSON}" \
  "${REPORT_TXT}" \
  "${RUN_ROOT}" \
  "${ITERATIONS}" \
  "${SCHEMA_PATH}" \
  "${ROOT_DIR}/scripts" <<'PY'
from __future__ import annotations

import hashlib
import json
import math
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

run_index_tsv = Path(sys.argv[1])
report_json_path = Path(sys.argv[2])
report_txt_path = Path(sys.argv[3])
soak_root_lexical = Path(sys.argv[4])
soak_root = soak_root_lexical.resolve()
iterations = int(sys.argv[5])
schema_path = Path(sys.argv[6])
sys.path.insert(0, sys.argv[7])
import doctor_frankentui_validate_jsonl as event_validator

VOLATILE_ARTIFACT_SUFFIX_ALLOWLIST = [
    "/run_meta.json",
    "/run_summary.txt",
    "/suite_summary.txt",
    "/report.json",
    "/index.html",
    "/custom_report.json",
    "/custom_report.html",
    "/case_results.json",
    "/summary.json",
    "/summary.txt",
    "/vhs.log",
    "/capture.tape",
    "/snapshot.png",
    "/session.txt",
    "/env_snapshot.txt",
    "/tool_versions.txt",
    "/command_manifest.txt",
    "/step_results.tsv",
]

CORRELATION_SUFFIX_RE = re.compile(r"^(?P<prefix>.+)-corr-(?P<seq>\d+)$")


def now_utc_timestamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def is_hex64(value: Any) -> bool:
    return isinstance(value, str) and bool(re.fullmatch(r"[0-9a-f]{64}", value))


def parse_json_file(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
        if isinstance(loaded, dict):
            return loaded
        return {}
    except json.JSONDecodeError:
        return {}


def parse_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.exists():
        return []
    events: list[dict[str, Any]] = []
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line:
            continue
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            return []
        if not isinstance(payload, dict):
            return []
        events.append(payload)
    return events


def normalize_path_value(value: str, run_dir: Path) -> str:
    text = value
    for root, replacement in [(run_dir, "<RUN_DIR>"), (soak_root_lexical, "<SOAK_ROOT>")]:
        aliases = {str(root.absolute()), str(root.resolve())}
        for alias in sorted(aliases, key=len, reverse=True):
            text = text.replace(alias, replacement)
    return text


def normalize_value(value: Any, run_dir: Path) -> Any:
    if isinstance(value, str):
        return normalize_path_value(value, run_dir)
    if isinstance(value, list):
        return [normalize_value(item, run_dir) for item in value]
    if isinstance(value, dict):
        return {key: normalize_value(value[key], run_dir) for key in sorted(value.keys())}
    return value


def is_volatile_artifact(path: str) -> bool:
    return any(path.endswith(suffix) for suffix in VOLATILE_ARTIFACT_SUFFIX_ALLOWLIST)


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")


def timestamp(value: Any) -> datetime:
    if not isinstance(value, str) or not value.endswith("Z"):
        raise ValueError(f"invalid UTC timestamp: {value!r}")
    return datetime.fromisoformat(value.removesuffix("Z") + "+00:00")


def trace_id(value: Any) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError("missing or invalid trace_id")
    return value


def normalize_capture_times(meta: dict[str, Any]) -> None:
    if timestamp(meta["finished_at"]) < timestamp(meta["started_at"]):
        raise ValueError("capture finished before it started")
    meta["started_at"] = "<TIMESTAMP>"
    meta["finished_at"] = "<TIMESTAMP>"
    # Capture wall time and encoded video duration are measurements, not decisions.
    # Keep their raw values in the retained artifacts; validate before canonicalizing.
    for key in ("duration_seconds", "video_duration_seconds"):
        if key not in meta or meta[key] is None:
            continue
        value = meta[key]
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0:
            raise ValueError(f"invalid {key}: {value!r}")
        if key == "video_duration_seconds" and meta.get("video_exists") and value == 0:
            raise ValueError("existing video has zero duration")
        meta[key] = "<DURATION>"


def normalize_ledger(path: Path, run_dir: Path, expected_trace: str | None = None) -> list[dict[str, Any]]:
    records = parse_jsonl(path)
    if not records:
        raise ValueError("evidence ledger is empty or invalid")
    meta = parse_json_file(path.with_name("run_meta.json"))
    linked_trace = trace_id(meta.get("trace_id"))
    if expected_trace is not None and expected_trace != linked_trace:
        raise ValueError("ledger run_meta trace_id does not match suite run")
    seen_decisions: set[str] = set()
    previous_time: datetime | None = None
    normalized = []
    for record in records:
        required = {"timestamp", "trace_id", "decision_id", "action", "evidence_terms", "fallback_active", "fallback_reason", "policy_id"}
        if not required.issubset(record):
            raise ValueError("ledger record is missing required fields")
        if trace_id(record.get("trace_id")) != linked_trace:
            raise ValueError("ledger trace_id does not match run_meta")
        decision = record.get("decision_id")
        if not isinstance(decision, str) or not decision or decision in seen_decisions:
            raise ValueError("missing or duplicate ledger decision_id")
        seen_decisions.add(decision)
        current_time = timestamp(record.get("timestamp"))
        if previous_time is not None and current_time < previous_time:
            raise ValueError("ledger timestamps are out of order")
        previous_time = current_time
        value = normalize_value(record, run_dir)
        value["timestamp"] = "<TIMESTAMP>"
        value["trace_id"] = "<TRACE>"
        normalized.append(value)
    return normalized


def normalize_suite_manifest(path: Path, run_dir: Path) -> dict[str, Any]:
    manifest = parse_json_file(path)
    value = normalize_value(manifest, run_dir)
    normalize_capture_times(value)
    index = manifest["run_index"]
    runs = manifest["runs"]
    if not isinstance(index, list) or not isinstance(runs, list):
        raise ValueError("suite run_index and runs must be arrays")
    expected_traces = [trace_id(entry["trace_id"]) for entry in index if entry.get("trace_id") is not None]
    if manifest["trace_ids"] != expected_traces or len(set(expected_traces)) != len(expected_traces):
        raise ValueError("suite trace_ids do not match unique run_index identities")
    indexed_dirs = [entry["run_dir"] for entry in index]
    if len(set(indexed_dirs)) != len(indexed_dirs):
        raise ValueError("duplicate suite run directory")
    run_dirs = [entry["run_dir"] for entry in runs]
    if len(set(run_dirs)) != len(run_dirs):
        raise ValueError("duplicate suite metadata run directory")
    trace_map = {raw: f"<TRACE:{i}>" for i, raw in enumerate(expected_traces)}
    value["trace_ids"] = [trace_map[raw] for raw in expected_traces]
    ledgers = []
    for entry, normalized in zip(index, value["run_index"]):
        raw_trace = entry.get("trace_id")
        if raw_trace is not None:
            normalized["trace_id"] = trace_map[raw_trace]
        elif not entry.get("capture_error_reason"):
            raise ValueError("suite run without trace_id must record a capture error")
    for run, normalized in zip(runs, value["runs"]):
        if run["run_dir"] not in indexed_dirs:
            raise ValueError("suite metadata run is absent from run_index")
        entry = index[indexed_dirs.index(run["run_dir"])]
        for key in ("profile", "status", "run_dir", "trace_id", "fallback_reason", "capture_error_reason", "evidence_ledger", "artifact_manifest", "ttyd_runtime_log", "tmux_session", "tmux_pane_capture", "tmux_pane_log"):
            if run.get(key) != entry.get(key):
                raise ValueError(f"suite run_index disagrees with run metadata: {key}")
        raw_trace = trace_id(run.get("trace_id"))
        disk_meta = parse_json_file(Path(run["run_dir"]) / "run_meta.json")
        if disk_meta != run:
            raise ValueError("suite embedded metadata differs from run_meta.json")
        normalized["trace_id"] = trace_map[raw_trace]
        normalize_capture_times(normalized)
        ledgers.append(normalize_ledger(Path(run["evidence_ledger"]), run_dir, raw_trace))
    if set(expected_traces) != {run["trace_id"] for run in runs}:
        raise ValueError("suite run_index has trace identities without run metadata")
    return {"manifest": value, "evidence_ledgers": ledgers}


def normalize_text_artifact(path: Path, data: bytes, run_dir: Path) -> bytes:
    text = data.decode("utf-8")
    port_path = path.parent / "server.port"
    if port_path.is_file():
        port = port_path.read_text(encoding="utf-8")
        if not re.fullmatch(r"[0-9]+", port) or not 1 <= int(port) <= 65535:
            raise ValueError("invalid bound server.port")
        if path.name == "server.port":
            return b"<BOUND_PORT>"
        for endpoint in re.finditer(r"http://127\.0\.0\.1:([0-9]+)/mcp/", text):
            if endpoint.group(1) != port:
                raise ValueError("endpoint does not match bound server.port")
        text = text.replace(f"http://127.0.0.1:{port}/mcp/", "http://127.0.0.1:<BOUND_PORT>/mcp/")
    if path.name.endswith(".stdout.log") and text.lstrip().startswith("{"):
        payload = json.loads(text)
        if isinstance(payload, dict) and payload.get("command") == "suite":
            manifest = parse_json_file(Path(payload["manifest_path"]))
            if payload.get("trace_ids") != manifest.get("trace_ids"):
                raise ValueError("suite stdout trace_ids do not match manifest")
            normalized = normalize_value(payload, run_dir)
            normalized["trace_ids"] = normalize_suite_manifest(Path(payload["manifest_path"]), run_dir)["manifest"]["trace_ids"]
            return canonical_bytes(normalized)
        if isinstance(payload, dict) and payload.get("command") == "doctor":
            timestamp(payload["generated_at"])
            summary = parse_json_file(Path(payload["doctor_summary_path"]))
            if summary != {key: value for key, value in payload.items() if key != "doctor_summary_path"}:
                raise ValueError("doctor stdout differs from saved summary")
            payload["generated_at"] = "<TIMESTAMP>"
            return canonical_bytes(normalize_value(payload, run_dir))
    if path.name in {"seed_rpc.log", "seed_timeout.log"}:
        lines = []
        previous_time: datetime | None = None
        timeout_ms: int | None = None
        for line in text.splitlines():
            match = re.fullmatch(r"\[([^\]]+)\] (.*)", line)
            if match is None:
                raise ValueError("invalid timestamped seed log record")
            current_time = timestamp(match.group(1))
            if previous_time is not None and current_time < previous_time:
                raise ValueError("seed log timestamps are out of order")
            previous_time = current_time
            message = match.group(2)
            if message.startswith("event=seed_start "):
                timeout = re.search(r"\btimeout_seconds=([0-9]+)(?: |$)", message)
                if timeout is None or int(timeout.group(1)) < 1 or timeout_ms is not None:
                    raise ValueError("seed log must declare one positive timeout")
                timeout_ms = int(timeout.group(1)) * 1000
            if timeout_ms is None:
                raise ValueError("seed log must begin with seed_start")
            # Only producer-owned timing fields before a diagnostic are volatile.
            # Keep retries, backoff/deadline policy, RPC bodies and reasons verbatim.
            if re.match(r"event=(?:seed_|server_|rpc_cancelled\b)", message):
                fields, separator, reason = message.partition(" reason=")
                tokens = fields.split(" ")
                for i, token in enumerate(tokens):
                    if token.startswith(("elapsed_ms=", "remaining_ms=")):
                        key, number = token.split("=", 1)
                        if not number.isdecimal() or (key == "remaining_ms" and int(number) > timeout_ms):
                            raise ValueError(f"invalid seed timing field: {token}")
                        tokens[i] = f"{key}=<DURATION>"
                message = " ".join(tokens) + separator + reason
            lines.append(f"[<TIMESTAMP>] {message}")
        if not lines:
            raise ValueError("empty seed log")
        text = "\n".join(lines) + "\n"
    if path.name.endswith(".stderr.log"):
        lines = []
        active_pid: str | None = None
        for line in text.splitlines(keepends=True):
            spawn = re.fullmatch(r"\[doctor:subprocess\] vhs spawned pid=([0-9]+) timeout=([0-9]+)s\n?", line)
            observation = re.fullmatch(r"(\[doctor:subprocess\] (?:vhs exited|vhs timeout|vhs stream fatal|defunct ttyd observed)) pid=([0-9]+)(.*?) elapsed=([0-9]+)ms(.*?)\n?", line.rstrip("\n"))
            if spawn:
                if active_pid is not None or int(spawn.group(1)) < 1 or int(spawn.group(2)) < 1:
                    raise ValueError("invalid or overlapping VHS process identity")
                active_pid = spawn.group(1)
                line = line.replace(f"pid={active_pid}", "pid=<PID>", 1)
            elif observation:
                if observation.group(2) != active_pid:
                    raise ValueError("VHS observation pid does not match spawn")
                line = f"{observation.group(1)} pid=<PID>{observation.group(3)} elapsed=<DURATION>ms{observation.group(5)}\n"
                if "vhs exited" in observation.group(1) or "vhs timeout" in observation.group(1) or "vhs stream fatal" in observation.group(1):
                    active_pid = None
            if path.name == "build_doctor.stderr.log":
                line = re.sub(r"^(    Finished `[^`]+` profile .* in )[0-9]+(?:\.[0-9]+)?s$", r"\1<DURATION>s", line)
            lines.append(line)
        if active_pid is not None:
            raise ValueError("VHS spawn has no final observation")
        text = "".join(lines)
    return normalize_path_value(text, run_dir).encode("utf-8")


def normalize_event(event: dict[str, Any], run_dir: Path) -> dict[str, Any]:
    artifact_hashes = event.get("artifact_hashes", {})
    stable_artifact_hashes: dict[str, str] = {}
    volatile_artifact_count = 0
    artifact_hash_shape_errors: list[str] = []
    canonical_sizes: dict[str, int] = {}
    raw_sizes: dict[str, int] = {}
    stream_hashes: dict[str, str] = {}

    if isinstance(artifact_hashes, dict):
        for raw_path, raw_hash in sorted(artifact_hashes.items()):
            normalized_path = normalize_path_value(str(raw_path), run_dir)
            if not is_hex64(raw_hash):
                artifact_hash_shape_errors.append(normalized_path)
                continue
            try:
                artifact_path = Path(raw_path)
                artifact_bytes = artifact_path.read_bytes()
                if sha256_bytes(artifact_bytes) != raw_hash:
                    raise ValueError("artifact hash mismatch")
                raw_sizes[raw_path] = len(artifact_bytes)
                if is_volatile_artifact(normalized_path):
                    volatile_artifact_count += 1
                    continue
                if artifact_path.name == "evidence_ledger.jsonl":
                    artifact_bytes = canonical_bytes(normalize_ledger(artifact_path, run_dir))
                elif artifact_path.name == "suite_manifest.json":
                    artifact_bytes = canonical_bytes(normalize_suite_manifest(artifact_path, run_dir))
                elif normalized_path.endswith(("/command.txt", ".stderr.log", ".stdout.log", "/seed_rpc.log", "/seed_timeout.log", "/server.port")):
                    artifact_bytes = normalize_text_artifact(artifact_path, artifact_bytes, run_dir)
                canonical_sizes[raw_path] = len(artifact_bytes)
                stable_artifact_hashes[normalized_path] = sha256_bytes(artifact_bytes)
            except (OSError, UnicodeError, ValueError, KeyError, TypeError) as error:
                artifact_hash_shape_errors.append(f"{normalized_path}: {error}")
    else:
        artifact_hash_shape_errors.append("artifact_hashes_not_object")

    actual = normalize_value(event.get("actual", {}), run_dir)
    if isinstance(event.get("actual"), dict) and isinstance(artifact_hashes, dict):
        for stream in ("stdout", "stderr"):
            raw_path = event["actual"].get(f"{stream}_log")
            raw_hash = event.get(f"{stream}_sha256")
            if raw_path is not None:
                try:
                    data = Path(raw_path).read_bytes()
                    if sha256_bytes(data) != raw_hash:
                        raise ValueError(f"{stream}_sha256 does not match referenced log")
                    stream_hashes[stream] = sha256_bytes(normalize_text_artifact(Path(raw_path), data, run_dir))
                except (OSError, UnicodeError, ValueError, KeyError, TypeError) as error:
                    artifact_hash_shape_errors.append(f"{stream}: {error}")
    if event.get("event_type") == "artifact" and isinstance(artifact_hashes, dict) and len(artifact_hashes) == 1 and isinstance(actual, dict) and "size_bytes" in actual:
        artifact_path = next(iter(artifact_hashes))
        size = actual["size_bytes"]
        if isinstance(size, bool) or not isinstance(size, int) or size < 0 or size != raw_sizes.get(artifact_path):
            artifact_hash_shape_errors.append("artifact size_bytes does not match raw artifact")
        if artifact_path in canonical_sizes:
            actual["size_bytes"] = canonical_sizes[artifact_path]
    if (
        event.get("event_type") == "artifact"
        and isinstance(artifact_hashes, dict)
        and len(artifact_hashes) == 1
        and next(iter(artifact_hashes)).endswith("/snapshot.png")
        and is_volatile_artifact(next(iter(artifact_hashes)))
        and isinstance(actual, dict)
        and "size_bytes" in actual
    ):
        size = actual["size_bytes"]
        if isinstance(size, int) and not isinstance(size, bool) and size > 0:
            actual["size_bytes"] = {"positive": True}
        else:
            actual["size_bytes"] = {"positive": False, "value": size}
            artifact_hash_shape_errors.append("snapshot size_bytes must be a positive integer")

    return {
        "schema_version": event.get("schema_version"),
        "case_id": normalize_value(event.get("case_id"), run_dir),
        "step_id": normalize_value(event.get("step_id"), run_dir),
        "event_type": event.get("event_type"),
        "command": normalize_value(event.get("command"), run_dir),
        "env_hash": event.get("env_hash"),
        "exit_code": event.get("exit_code"),
        "expected": normalize_value(event.get("expected", {}), run_dir),
        "actual": actual,
        "stable_artifact_hashes": stable_artifact_hashes,
        "stream_hashes": stream_hashes,
        "artifact_hash_count": len(artifact_hashes) if isinstance(artifact_hashes, dict) else 0,
        "volatile_artifact_hash_count": volatile_artifact_count,
        "artifact_hash_shape_errors": artifact_hash_shape_errors,
    }


def validate_event_order(
    workflow: str,
    events: list[dict[str, Any]],
) -> list[str]:
    errors: list[str] = []
    if not events:
        errors.append("events_jsonl is empty")
        return errors

    first_event_type = events[0].get("event_type")
    last_event_type = events[-1].get("event_type")
    if first_event_type != "run_start":
        errors.append(f"first event_type must be run_start (got {first_event_type!r})")
    if last_event_type != "run_end":
        errors.append(f"last event_type must be run_end (got {last_event_type!r})")

    run_id = events[0].get("run_id")
    if not isinstance(run_id, str) or not run_id:
        errors.append("missing run_id on first event")
        run_id = ""

    expected_seq = 1
    seen_correlation: set[str] = set()
    for index, event in enumerate(events, start=1):
        if event.get("run_id") != run_id:
            errors.append(f"event {index}: run_id does not match first event")
        correlation_id = event.get("correlation_id")
        if not isinstance(correlation_id, str) or not correlation_id:
            errors.append(f"event {index}: correlation_id missing or invalid")
            continue
        if correlation_id in seen_correlation:
            errors.append(f"event {index}: duplicate correlation_id {correlation_id}")
            continue
        seen_correlation.add(correlation_id)
        match = CORRELATION_SUFFIX_RE.match(correlation_id)
        if match is None:
            errors.append(f"event {index}: invalid correlation_id format {correlation_id}")
            continue
        prefix = match.group("prefix")
        seq = int(match.group("seq"))
        if run_id and prefix != run_id:
            errors.append(
                f"event {index}: correlation prefix {prefix!r} does not match run_id {run_id!r}"
            )
        if seq != expected_seq:
            errors.append(
                f"event {index}: correlation sequence expected {expected_seq} got {seq}"
            )
            expected_seq = seq + 1
        else:
            expected_seq += 1

    if workflow == "happy":
        steps: dict[str, set[str]] = {}
        for event in events:
            step_id = event.get("step_id")
            event_type = event.get("event_type")
            if isinstance(step_id, str) and step_id:
                steps.setdefault(step_id, set()).add(str(event_type))
        for step_id, seen in sorted(steps.items()):
            missing = {"step_start", "step_end"} - seen
            for event_type in sorted(missing):
                errors.append(f"step_id {step_id!r} missing {event_type}")

    if workflow == "failure":
        cases: dict[str, set[str]] = {}
        for event in events:
            case_id = event.get("case_id")
            event_type = event.get("event_type")
            if isinstance(case_id, str) and case_id and case_id != "__run__":
                cases.setdefault(case_id, set()).add(str(event_type))
        for case_id, seen in sorted(cases.items()):
            missing = {"case_start", "case_end"} - seen
            for event_type in sorted(missing):
                errors.append(f"case_id {case_id!r} missing {event_type}")

    return errors


def required_field_errors(events: list[dict[str, Any]], required_fields: list[str]) -> list[str]:
    errors: list[str] = []
    for index, event in enumerate(events, start=1):
        for field in required_fields:
            if field not in event:
                errors.append(f"event {index}: missing required field {field!r}")
    return errors


rows: list[dict[str, Any]] = []
global_errors: list[str] = []
seen_runs: set[tuple[str, int]] = set()
seen_run_dirs: set[Path] = set()
for line_number, raw in enumerate(run_index_tsv.read_text(encoding="utf-8").splitlines(), start=1):
    if not raw.strip():
        continue
    fields = raw.split("\t")
    if len(fields) != 9:
        global_errors.append(f"run index line {line_number}: expected nine fields")
        continue
    (
        workflow,
        iteration,
        run_dir,
        exit_code,
        summary_json,
        events_jsonl,
        validation_json,
        stdout_log,
        stderr_log,
    ) = fields
    try:
        iteration_number = int(iteration)
        script_exit = int(exit_code)
    except ValueError:
        global_errors.append(f"run index line {line_number}: invalid iteration or exit code")
        continue
    if workflow not in {"happy", "failure"} or iteration_number not in range(1, iterations + 1):
        global_errors.append(f"run index line {line_number}: unexpected workflow/iteration")
        continue
    key = (workflow, iteration_number)
    run_dir_path = Path(run_dir)
    if key in seen_runs or run_dir_path.resolve() in seen_run_dirs:
        global_errors.append(f"run index line {line_number}: duplicate run")
        continue
    seen_runs.add(key)
    seen_run_dirs.add(run_dir_path.resolve())
    summary = parse_json_file(Path(summary_json))
    validation = parse_json_file(Path(validation_json))
    events = parse_jsonl(Path(events_jsonl))

    rows.append(
        {
            "workflow": workflow,
            "iteration": iteration_number,
            "run_dir": str(run_dir_path),
            "exit_code": script_exit,
            "summary_json": summary_json,
            "events_jsonl": events_jsonl,
            "events_validation_report_json": validation_json,
            "stdout_log": stdout_log,
            "stderr_log": stderr_log,
            "summary": summary,
            "events": events,
            "validation": validation,
        }
    )

rows.sort(key=lambda row: (row["workflow"], row["iteration"]))
for workflow in ("happy", "failure"):
    for iteration in range(1, iterations + 1):
        if (workflow, iteration) not in seen_runs:
            global_errors.append(f"missing run: {workflow} iteration {iteration}")

schema_obj = parse_json_file(schema_path)
for workflow in ("happy", "failure"):
    global_errors.extend(event_validator.validate_schema_contract(schema_obj, workflow))
required_fields = schema_obj.get("required_fields", [])
if not isinstance(required_fields, list) or not required_fields or not all(isinstance(field, str) for field in required_fields):
    global_errors.append("event schema required_fields is empty or invalid")
    required_fields = []

workflow_runs: dict[str, list[dict[str, Any]]] = {"happy": [], "failure": []}
for row in rows:
    workflow_runs.setdefault(row["workflow"], []).append(row)

workflow_reports: dict[str, Any] = {}
divergence_entries: list[dict[str, Any]] = []
overall_skipped = True

for workflow, run_list in workflow_runs.items():
    if not run_list:
        global_errors.append(f"workflow {workflow!r} has no runs")
        workflow_reports[workflow] = {"status": "failed", "runs": [], "iterations": 0, "non_skipped_iterations": [], "skipped_iterations": []}
        overall_skipped = False
        continue

    run_reports: list[dict[str, Any]] = []
    skipped_iterations: list[int] = []
    non_skipped_iterations: list[int] = []

    normalized_baseline: list[dict[str, Any]] | None = None
    baseline_iteration: int | None = None

    for row in run_list:
        summary_status = row["summary"].get("status", "unknown")
        validation_status = row["validation"].get("status", "unknown")
        events = row["events"]
        run_dir = Path(row["run_dir"])

        run_errors: list[str] = []
        if row["exit_code"] != 0:
            run_errors.append(f"workflow script exited {row['exit_code']}")

        if summary_status not in {"passed", "skipped"}:
            run_errors.append(f"unexpected summary status: {summary_status!r}")
        if validation_status not in {"passed", "skipped"}:
            run_errors.append(f"unexpected validation status: {validation_status!r}")
        if summary_status != validation_status:
            run_errors.append("summary and event validation statuses disagree")

        if summary_status == "skipped":
            skipped_iterations.append(row["iteration"])
        else:
            non_skipped_iterations.append(row["iteration"])
            run_errors.extend(required_field_errors(events, required_fields))
            run_errors.extend(validate_event_order(workflow, events))
            if not event_validator.validate_schema_contract(schema_obj, workflow):
                validation_errors, _ = event_validator.validate_stream(events, schema_obj, workflow)
                run_errors.extend(validation_errors)

            normalized_events = [normalize_event(event, run_dir) for event in events]
            fingerprint_payload = json.dumps(
                normalized_events,
                sort_keys=True,
                separators=(",", ":"),
            ).encode("utf-8")
            fingerprint = sha256_bytes(fingerprint_payload)

            artifact_shape_errors = [
                {
                    "event_index": index + 1,
                    "invalid_entries": entry["artifact_hash_shape_errors"],
                }
                for index, entry in enumerate(normalized_events)
                if entry["artifact_hash_shape_errors"]
            ]
            if artifact_shape_errors:
                run_errors.append(
                    f"artifact hash shape errors detected: {artifact_shape_errors}"
                )

            if normalized_baseline is None:
                normalized_baseline = normalized_events
                baseline_iteration = row["iteration"]
            else:
                divergence: dict[str, Any] | None = None
                if len(normalized_events) != len(normalized_baseline):
                    divergence = {
                        "reason": "event_count_mismatch",
                        "baseline_event_count": len(normalized_baseline),
                        "current_event_count": len(normalized_events),
                        "first_divergence_event_index": 1,
                    }
                else:
                    for event_index, (baseline_event, current_event) in enumerate(
                        zip(normalized_baseline, normalized_events),
                        start=1,
                    ):
                        if baseline_event != current_event:
                            divergence = {
                                "reason": "normalized_event_mismatch",
                                "first_divergence_event_index": event_index,
                                "baseline_event_type": baseline_event.get("event_type"),
                                "current_event_type": current_event.get("event_type"),
                                "baseline_step_id": baseline_event.get("step_id"),
                                "current_step_id": current_event.get("step_id"),
                                "baseline_case_id": baseline_event.get("case_id"),
                                "current_case_id": current_event.get("case_id"),
                            }
                            break

                if divergence is not None:
                    divergence_entry = {
                        "workflow": workflow,
                        "baseline_iteration": baseline_iteration,
                        "current_iteration": row["iteration"],
                        "run_dir": row["run_dir"],
                        "events_jsonl": row["events_jsonl"],
                        "summary_json": row["summary_json"],
                        "stdout_log": row["stdout_log"],
                        "stderr_log": row["stderr_log"],
                        **divergence,
                    }
                    divergence_entries.append(divergence_entry)
                    run_errors.append(
                        "divergence from baseline detected at event index "
                        f"{divergence['first_divergence_event_index']}"
                    )

            row_report = {
                "iteration": row["iteration"],
                "run_dir": row["run_dir"],
                "status": summary_status,
                "exit_code": row["exit_code"],
                "events_count": len(events),
                "events_validation_status": validation_status,
                "summary_json": row["summary_json"],
                "events_jsonl": row["events_jsonl"],
                "events_validation_report_json": row["events_validation_report_json"],
                "stdout_log": row["stdout_log"],
                "stderr_log": row["stderr_log"],
                "normalized_fingerprint": fingerprint,
                "errors": run_errors,
            }
            run_reports.append(row_report)
            continue

        row_report = {
            "iteration": row["iteration"],
            "run_dir": row["run_dir"],
            "status": summary_status,
            "exit_code": row["exit_code"],
            "events_count": len(events),
            "events_validation_status": validation_status,
            "summary_json": row["summary_json"],
            "events_jsonl": row["events_jsonl"],
            "events_validation_report_json": row["events_validation_report_json"],
            "stdout_log": row["stdout_log"],
            "stderr_log": row["stderr_log"],
            "skip_reason": row["summary"].get("reason", "unknown"),
            "errors": run_errors,
        }
        run_reports.append(row_report)

    workflow_status = "passed"
    if skipped_iterations and non_skipped_iterations:
        workflow_status = "failed"
        global_errors.append(
            f"workflow {workflow!r} mixed skipped/non-skipped runs: "
            f"skipped={skipped_iterations}, non_skipped={non_skipped_iterations}"
        )
    elif non_skipped_iterations:
        overall_skipped = False

    for report in run_reports:
        if report["errors"]:
            workflow_status = "failed"
            overall_skipped = False

    workflow_reports[workflow] = {
        "status": workflow_status if non_skipped_iterations or workflow_status == "failed" else "skipped",
        "iterations": len(run_reports),
        "skipped_iterations": skipped_iterations,
        "non_skipped_iterations": non_skipped_iterations,
        "runs": run_reports,
    }

if divergence_entries:
    overall_skipped = False

if global_errors or divergence_entries or any(
    report.get("status") == "failed" for report in workflow_reports.values()
):
    overall_status = "failed"
elif overall_skipped:
    overall_status = "skipped"
elif any(report.get("status") == "skipped" for report in workflow_reports.values()):
    overall_status = "failed"
    global_errors.append("all workflows must have non-skipped runs to establish determinism")
else:
    overall_status = "passed"

first_divergence = divergence_entries[0] if divergence_entries else None

report = {
    "generated_at": now_utc_timestamp(),
    "comparator_sha256": sha256_bytes((Path(sys.argv[7]) / "doctor_frankentui_determinism_soak.sh").read_bytes()),
    "event_schema_sha256": sha256_bytes(schema_path.read_bytes()),
    "event_validator_sha256": sha256_bytes((Path(sys.argv[7]) / "doctor_frankentui_validate_jsonl.py").read_bytes()),
    "status": overall_status,
    "iterations_requested": iterations,
    "run_root": str(soak_root),
    "volatile_artifact_suffix_allowlist": VOLATILE_ARTIFACT_SUFFIX_ALLOWLIST,
    "canonical_fields": {
        "evidence_ledger.jsonl": ["timestamp", "trace_id (linked to run_meta)"],
        "suite_manifest.json": ["started_at", "finished_at", "trace_ids (linked to run_index/runs)", "runs.started_at", "runs.finished_at", "runs.duration_seconds", "runs.video_duration_seconds", "runs.trace_id", "run_index.trace_id"],
        "doctor stdout JSON": ["generated_at (linked to saved summary)"],
        "suite stdout JSON": ["trace_ids (linked to manifest)"],
        "seed logs": ["timestamp prefix", "elapsed_ms", "remaining_ms", "loopback endpoint port (linked to server.port)"],
        "text artifacts": ["run-directory aliases", "loopback endpoint port (linked to server.port when present)"],
        "VHS subprocess stderr": ["pid (linked to spawn/final observation)", "elapsed"],
        "Cargo build_doctor stderr": ["Finished profile duration"],
        "artifact size_bytes": "verified raw size, then canonical byte length; snapshot.png requires positive integer",
    },
    "workflow_reports": workflow_reports,
    "global_errors": global_errors,
    "divergences": divergence_entries,
    "first_divergence": first_divergence,
}

report_json_path.parent.mkdir(parents=True, exist_ok=True)
with report_json_path.open("x", encoding="utf-8") as output:
    output.write(json.dumps(report, indent=2) + "\n")

lines: list[str] = []
lines.append(f"status={overall_status}")
lines.append(f"run_root={soak_root}")
lines.append(f"iterations={iterations}")
lines.append(f"report_json={report_json_path}")
for workflow, data in workflow_reports.items():
    lines.append(
        f"workflow={workflow} status={data['status']} "
        f"runs={data['iterations']} non_skipped={len(data['non_skipped_iterations'])} "
        f"skipped={len(data['skipped_iterations'])}"
    )
if first_divergence is not None:
    lines.append("first_divergence:")
    lines.append(
        "workflow={workflow} baseline_iteration={baseline} current_iteration={current} "
        "event_index={index} reason={reason}".format(
            workflow=first_divergence["workflow"],
            baseline=first_divergence["baseline_iteration"],
            current=first_divergence["current_iteration"],
            index=first_divergence["first_divergence_event_index"],
            reason=first_divergence["reason"],
        )
    )
    lines.append(f"events_jsonl={first_divergence['events_jsonl']}")
    lines.append(f"summary_json={first_divergence['summary_json']}")
    lines.append(f"stdout_log={first_divergence['stdout_log']}")
    lines.append(f"stderr_log={first_divergence['stderr_log']}")
if global_errors:
    lines.append("global_errors:")
    for error in global_errors:
        lines.append(f"- {error}")

with report_txt_path.open("x", encoding="utf-8") as output:
    output.write("\n".join(lines) + "\n")
print("\n".join(lines))

if overall_status != "passed":
    raise SystemExit(1)
raise SystemExit(0)
PY

echo "[determinism] report_json=${REPORT_JSON}"
echo "[determinism] report_txt=${REPORT_TXT}"

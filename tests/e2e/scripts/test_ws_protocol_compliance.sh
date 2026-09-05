#!/bin/bash
set -euo pipefail

# E2E: FrankenTerm WebSocket protocol compliance (bd-2vr05.10.3)
#
# Validates handshake policy, binary input forwarding, text control warnings,
# close control, invalid control error handling, bridge telemetry, and a
# detailed JSONL evidence ledger.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

USER_E2E_LOG_DIR="${E2E_LOG_DIR:-}"
USER_E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-}"
USER_E2E_JSONL_FILE="${E2E_JSONL_FILE:-}"
USER_REPORT_OUT="${REPORT_OUT:-}"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"

BASE_LOG_DIR="${USER_E2E_LOG_DIR:-/tmp/ftui_e2e_logs}"
E2E_LOG_DIR="${BASE_LOG_DIR}/ws_protocol_compliance"
E2E_RESULTS_DIR="${USER_E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
export E2E_LOG_DIR E2E_RESULTS_DIR

mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"

e2e_fixture_init "ws_protocol_compliance"

E2E_JSONL_FILE="${USER_E2E_JSONL_FILE:-$E2E_LOG_DIR/ws_protocol_compliance_${E2E_RUN_ID}.jsonl}"
REPORT_OUT="${USER_REPORT_OUT:-$E2E_RESULTS_DIR/ws_protocol_compliance_report_${E2E_RUN_ID}.json}"
export E2E_JSONL_FILE REPORT_OUT

# Observe readiness without consuming the bridge's single accepted connection.
required_tools=(cargo "${E2E_PYTHON:-python3}" cat tail)
if [[ "$OSTYPE" == darwin* ]]; then
    required_tools+=(lsof)
else
    required_tools+=(ss)
fi
require_tools "${required_tools[@]}" || exit 2
require_python_module websockets || exit 2

REMOTE_TARGET_DIR="${REMOTE_TARGET_DIR:-${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/frankentui-ws-protocol-e2e}}"
REMOTE_ALLOW_LOCAL_CARGO_FALLBACK="${REMOTE_ALLOW_LOCAL_CARGO_FALLBACK:-1}"
export REMOTE_TARGET_DIR REMOTE_ALLOW_LOCAL_CARGO_FALLBACK

# shellcheck source=/dev/null
source "$LIB_DIR/remote.sh"

echo "=== WebSocket Protocol Compliance E2E Test ==="

python_ws_client="${E2E_PYTHON:-python3}"
if ! "$python_ws_client" "$LIB_DIR/ws_client.py" --self-test >/dev/null; then
    echo "[FAIL] ws_client self-tests failed"
    exit 1
fi

if ! remote_build_bridge; then
    echo "[FAIL] Unable to build frankenterm_ws_bridge"
    exit 1
fi

BRIDGE_BIN="$(remote_bridge_path)"

"$python_ws_client" - "$BRIDGE_BIN" "$E2E_LOG_DIR" "$E2E_JSONL_FILE" "$REPORT_OUT" "$E2E_RUN_ID" "$E2E_SEED" <<'PY'
import asyncio
import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import websockets

bridge_bin = Path(sys.argv[1])
log_dir = Path(sys.argv[2])
jsonl_path = Path(sys.argv[3])
report_path = Path(sys.argv[4])
run_id = sys.argv[5]
seed = int(sys.argv[6])

allowed_origin = "https://allowed.example"
denied_origin = "https://denied.example"
token = "protocol-secret"

event_seq = 0
assertions_total = 0
assertions_failed = 0
case_results: list[dict[str, Any]] = []
bridge_processes: list[subprocess.Popen[str]] = []


def timestamp() -> str:
    global event_seq
    event_seq += 1
    if os.environ.get("E2E_DETERMINISTIC", "1") == "1":
        return f"T{event_seq:06d}"
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def emit(event_type: str, **payload: Any) -> None:
    record = {
        "schema_version": "e2e-jsonl-v1",
        "type": event_type,
        "timestamp": timestamp(),
        "run_id": run_id,
        "seed": seed,
        **payload,
    }
    with jsonl_path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, sort_keys=True) + "\n")


def record_assert(name: str, passed: bool, details: str = "", **payload: Any) -> None:
    global assertions_total, assertions_failed
    assertions_total += 1
    if not passed:
        assertions_failed += 1
    emit(
        "assert",
        assertion=name,
        status="passed" if passed else "failed",
        details=details,
        **payload,
    )
    if not passed:
        raise AssertionError(f"{name}: {details}")


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def wait_listening(port: int, proc: subprocess.Popen[str]) -> None:
    if sys.platform == "darwin":
        probe = ["lsof", "-nP", "-a", "-p", str(proc.pid), f"-iTCP:{port}", "-sTCP:LISTEN"]
    else:
        probe = ["ss", "-tln", f"sport = :{port}"]
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            stderr = proc.stderr.read() if proc.stderr else ""
            raise RuntimeError(f"bridge exited before listen; rc={proc.returncode}; stderr={stderr}")
        result = subprocess.run(
            probe,
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode == 0 and "LISTEN" in result.stdout:
            return
        time.sleep(0.05)
    raise TimeoutError(f"bridge did not listen on port {port}")


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def find_status(exc: BaseException) -> int | None:
    response = getattr(exc, "response", None)
    for attr in ("status_code", "status"):
        status = getattr(response, attr, None)
        if isinstance(status, int):
            return status
    return None


def start_bridge(case_id: str, *, require_auth: bool = True) -> tuple[subprocess.Popen[str], int, Path]:
    port = free_port()
    telemetry = log_dir / f"{case_id}_bridge_telemetry.jsonl"
    args = [
        str(bridge_bin),
        "--bind",
        f"127.0.0.1:{port}",
        "--cmd",
        "/bin/sh",
        "--arg",
        "-c",
        "--arg",
        "cat",
        "--cols",
        "100",
        "--rows",
        "30",
        "--telemetry",
        str(telemetry),
        "--idle-ms",
        "1",
        "--accept-once",
    ]
    if require_auth:
        args.extend(["--origin", allowed_origin, "--token", token])

    proc = subprocess.Popen(
        args,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    bridge_processes.append(proc)
    wait_listening(port, proc)
    emit("bridge_start", case=case_id, port=port, telemetry=str(telemetry), pid=proc.pid)
    return proc, port, telemetry


def cleanup_running_bridges() -> None:
    for proc in bridge_processes:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.communicate(timeout=5)


def wait_bridge(proc: subprocess.Popen[str], case_id: str, *, expect_success: bool) -> int:
    try:
        stdout, stderr = proc.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        proc.terminate()
        stdout, stderr = proc.communicate(timeout=5)
        raise AssertionError(f"{case_id}: bridge did not exit after client session")
    rc = int(proc.returncode or 0)
    emit("bridge_exit", case=case_id, exit_code=rc, stderr_tail=stderr[-400:], stdout_tail=stdout[-400:])
    if expect_success:
        record_assert(f"{case_id}_bridge_exit_zero", rc == 0, f"exit_code={rc}")
    else:
        record_assert(f"{case_id}_bridge_exit_nonzero", rc != 0, f"exit_code={rc}")
    return rc


async def recv_until(ws: Any, predicate: Any, timeout_s: float, label: str) -> Any:
    deadline = time.monotonic() + timeout_s
    last_message = None
    while time.monotonic() < deadline:
        try:
            message = await asyncio.wait_for(ws.recv(), timeout=max(0.05, deadline - time.monotonic()))
        except TimeoutError:
            break
        last_message = message
        if predicate(message):
            return message
    raise AssertionError(f"timed out waiting for {label}; last_message={last_message!r}")


async def case_authorized_echo_warning_close() -> None:
    case_id = "authorized_echo_warning_close"
    proc, port, telemetry = start_bridge(case_id)
    url = f"ws://127.0.0.1:{port}/ws?token={token}"
    emit("case_start", case=case_id, url=url)
    async with websockets.connect(url, origin=allowed_origin, open_timeout=5, close_timeout=2) as ws:
        payload = b"protocol-echo\n"
        await ws.send(payload)
        echo = await recv_until(
            ws,
            lambda msg: isinstance(msg, bytes) and b"protocol-echo" in msg,
            5.0,
            "binary echo",
        )
        record_assert(
            "authorized_binary_echo",
            isinstance(echo, bytes) and b"protocol-echo" in echo,
            f"bytes={len(echo) if isinstance(echo, bytes) else 0}",
            case=case_id,
        )

        await ws.send(json.dumps({"type": "unknown-control"}))
        warning = await recv_until(
            ws,
            lambda msg: isinstance(msg, str) and "unknown_control_message" in msg,
            5.0,
            "unknown control warning",
        )
        parsed_warning = json.loads(warning)
        record_assert(
            "unknown_control_warning_frame",
            parsed_warning.get("type") == "warning"
            and parsed_warning.get("message") == "unknown_control_message",
            warning,
            case=case_id,
        )

        await ws.send(json.dumps({"type": "close"}))

    wait_bridge(proc, case_id, expect_success=True)
    events = read_jsonl(telemetry)
    names = [event.get("event") for event in events]
    for required in ("bridge_session_start", "bridge_input", "bridge_session_end"):
        record_assert(
            f"{case_id}_{required}_telemetry",
            required in names,
            f"events={names}",
            case=case_id,
        )
    case_results.append({"case": case_id, "status": "passed", "telemetry": str(telemetry)})
    emit("case_end", case=case_id, status="passed")


async def case_rejected_missing_token() -> None:
    case_id = "rejected_missing_token"
    proc, port, telemetry = start_bridge(case_id)
    url = f"ws://127.0.0.1:{port}/ws"
    emit("case_start", case=case_id, url=url)
    rejected = False
    detail = ""
    try:
        async with websockets.connect(url, origin=allowed_origin, open_timeout=5):
            pass
    except Exception as exc:
        status = find_status(exc)
        detail = f"{type(exc).__name__}: {exc}"
        rejected = status == 401 or "401" in detail or "Unauthorized" in detail
    record_assert("missing_token_rejected", rejected, detail, case=case_id)
    wait_bridge(proc, case_id, expect_success=False)
    events = read_jsonl(telemetry)
    names = [event.get("event") for event in events]
    record_assert(
        f"{case_id}_telemetry_error",
        "bridge_session_error" in names,
        f"events={names}",
        case=case_id,
    )
    case_results.append({"case": case_id, "status": "passed", "telemetry": str(telemetry)})
    emit("case_end", case=case_id, status="passed")


async def case_rejected_bad_origin() -> None:
    case_id = "rejected_bad_origin"
    proc, port, telemetry = start_bridge(case_id)
    url = f"ws://127.0.0.1:{port}/ws?token={token}"
    emit("case_start", case=case_id, url=url)
    rejected = False
    detail = ""
    try:
        async with websockets.connect(url, origin=denied_origin, open_timeout=5):
            pass
    except Exception as exc:
        status = find_status(exc)
        detail = f"{type(exc).__name__}: {exc}"
        rejected = status == 403 or "403" in detail or "Forbidden" in detail
    record_assert("bad_origin_rejected", rejected, detail, case=case_id)
    wait_bridge(proc, case_id, expect_success=False)
    events = read_jsonl(telemetry)
    names = [event.get("event") for event in events]
    record_assert(
        f"{case_id}_telemetry_error",
        "bridge_session_error" in names,
        f"events={names}",
        case=case_id,
    )
    case_results.append({"case": case_id, "status": "passed", "telemetry": str(telemetry)})
    emit("case_end", case=case_id, status="passed")


async def case_invalid_resize_error() -> None:
    case_id = "invalid_resize_error"
    proc, port, telemetry = start_bridge(case_id)
    url = f"ws://127.0.0.1:{port}/ws?token={token}"
    emit("case_start", case=case_id, url=url)
    closed = False
    detail = ""
    try:
        async with websockets.connect(url, origin=allowed_origin, open_timeout=5, close_timeout=2) as ws:
            await ws.send(json.dumps({"type": "resize", "cols": 0, "rows": 24}))
            try:
                await asyncio.wait_for(ws.recv(), timeout=3)
            except websockets.exceptions.ConnectionClosed as exc:
                detail = f"{type(exc).__name__}: {exc}"
                closed = True
            except TimeoutError as exc:
                detail = f"{type(exc).__name__}: {exc}"
                closed = False
            except Exception as exc:
                detail = f"{type(exc).__name__}: {exc}"
                closed = True
    except Exception as exc:
        detail = f"{type(exc).__name__}: {exc}"
        closed = True
    record_assert("invalid_resize_closes_session", closed, detail, case=case_id)
    wait_bridge(proc, case_id, expect_success=False)
    events = read_jsonl(telemetry)
    names = [event.get("event") for event in events]
    errors = [
        str(event.get("payload", {}).get("error", ""))
        for event in events
        if event.get("event") == "bridge_session_error"
    ]
    record_assert(
        f"{case_id}_telemetry_error",
        "bridge_session_error" in names and any("resize dimensions" in error for error in errors),
        f"events={names} errors={errors}",
        case=case_id,
    )
    case_results.append({"case": case_id, "status": "passed", "telemetry": str(telemetry)})
    emit("case_end", case=case_id, status="passed")


async def main() -> None:
    emit(
        "run_start",
        command="tests/e2e/scripts/test_ws_protocol_compliance.sh",
        log_dir=str(log_dir),
        results_dir=str(report_path.parent),
    )
    try:
        await case_authorized_echo_warning_close()
        await case_rejected_missing_token()
        await case_rejected_bad_origin()
        await case_invalid_resize_error()
    finally:
        cleanup_running_bridges()
        report = {
            "suite": "ws_protocol_compliance",
            "status": "pass" if assertions_failed == 0 else "fail",
            "run_id": run_id,
            "seed": seed,
            "assertions_total": assertions_total,
            "assertions_failed": assertions_failed,
            "cases": case_results,
            "jsonl": str(jsonl_path),
        }
        report_path.write_text(json.dumps(report, indent=2, sort_keys=True), encoding="utf-8")
        emit(
            "artifact",
            artifact_type="report",
            path=str(report_path),
            status="present" if report_path.exists() else "missing",
        )
        emit(
            "run_end",
            status="passed" if assertions_failed == 0 else "failed",
            failed_count=assertions_failed,
            assertions_total=assertions_total,
        )


asyncio.run(main())
PY

"$python_ws_client" - "$E2E_JSONL_FILE" "$REPORT_OUT" <<'PY'
import json
import sys
from pathlib import Path

jsonl_path = Path(sys.argv[1])
report_path = Path(sys.argv[2])
events = [json.loads(line) for line in jsonl_path.read_text(encoding="utf-8").splitlines() if line.strip()]
if not events:
    raise SystemExit("protocol compliance JSONL is empty")

asserts = [event for event in events if event.get("type") == "assert"]
if len(asserts) < 10:
    raise SystemExit(f"expected at least 10 assert events, got {len(asserts)}")
failed = [event for event in asserts if event.get("status") != "passed"]
if failed:
    names = [event.get("assertion", "?") for event in failed]
    raise SystemExit(f"unexpected failed assertions: {names}")

run_end = [event for event in events if event.get("type") == "run_end"]
if len(run_end) != 1 or run_end[0].get("status") != "passed":
    raise SystemExit(f"invalid run_end events: {run_end}")

report = json.loads(report_path.read_text(encoding="utf-8"))
case_names = {case.get("case") for case in report.get("cases", [])}
expected = {
    "authorized_echo_warning_close",
    "rejected_missing_token",
    "rejected_bad_origin",
    "invalid_resize_error",
}
if case_names != expected:
    raise SystemExit(f"case set mismatch: expected {sorted(expected)}, got {sorted(case_names)}")
PY

echo "[PASS] WebSocket protocol compliance validated"
echo "  JSONL:  $E2E_JSONL_FILE"
echo "  Report: $REPORT_OUT"

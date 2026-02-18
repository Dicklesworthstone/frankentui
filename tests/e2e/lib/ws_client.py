#!/usr/bin/env python3
"""WebSocket client for scripted remote terminal sessions.

Connects to the frankenterm_ws_bridge, sends scripted input sequences,
captures output, computes checksums, and emits JSONL event logs.

Usage:
    python3 ws_client.py --url ws://127.0.0.1:9231 --scenario scenario.json
    python3 ws_client.py --url ws://127.0.0.1:9231 --scenario scenario.json --golden golden.transcript

Scenario JSON format:
{
    "name": "resize_storm",
    "description": "Rapid resize events over WebSocket",
    "initial_cols": 120,
    "initial_rows": 40,
    "steps": [
        {"type": "send", "data_hex": "6c730a", "delay_ms": 100},
        {"type": "resize", "cols": 80, "rows": 24, "delay_ms": 50},
        {"type": "send", "data_b64": "bHMgLWxhCg==", "delay_ms": 100},
        {"type": "wait", "ms": 500},
        {"type": "drain"}
    ],
    "timeout_s": 30
}
"""

import argparse
import asyncio
import base64
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
import time
import unicodedata
import unittest
from pathlib import Path
from typing import Any

try:
    import websockets
except ImportError:
    print("ERROR: 'websockets' package not available", file=sys.stderr)
    sys.exit(1)


def git_sha() -> str:
    """Return short git SHA of the working tree."""
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, timeout=5
        )
        return result.stdout.strip() if result.returncode == 0 else "unknown"
    except Exception:
        return "unknown"


def make_run_id(seed: int) -> str:
    """Deterministic run ID from seed."""
    if os.environ.get("E2E_DETERMINISTIC", "1") == "1":
        return f"remote-{seed:08x}"
    return f"remote-{int(time.time() * 1000):x}"


def sha256_hex(data: bytes) -> str:
    """Compute SHA-256 hex digest."""
    return hashlib.sha256(data).hexdigest()


def command_version(command: str) -> str:
    """Return '<command> --version' first line, or 'unknown'."""
    try:
        result = subprocess.run(
            [command, "--version"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        if result.returncode != 0:
            return "unknown"
        line = result.stdout.strip().splitlines()
        return line[0] if line else "unknown"
    except Exception:
        return "unknown"


def git_dirty() -> bool:
    """Best-effort git dirty check."""
    try:
        result = subprocess.run(
            ["git", "status", "--porcelain"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        return result.returncode == 0 and bool(result.stdout.strip())
    except Exception:
        return False


def frame_hash_key(mode: str, cols: int | None, rows: int | None, seed: int) -> str:
    """Build deterministic hash-key used by e2e JSONL validators."""
    if cols is None or rows is None:
        return f"{mode}-unknown-seed{seed}"
    return f"{mode}-{cols}x{rows}-seed{seed}"


def _as_non_negative_int(value: Any) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int) and value >= 0:
        return value
    return None


def _as_positive_int(value: Any) -> int | None:
    out = _as_non_negative_int(value)
    if out is None or out == 0:
        return None
    return out


def _as_number(value: Any) -> float | int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return value
    return None


def _extract_frame_overrides(raw: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    if isinstance(raw.get("hash_algo"), str):
        out["hash_algo"] = raw["hash_algo"]
    if isinstance(raw.get("frame_hash"), str):
        out["frame_hash"] = raw["frame_hash"]
    if isinstance(raw.get("patch_hash"), str):
        out["patch_hash"] = raw["patch_hash"]
    if isinstance(raw.get("mode"), str):
        out["mode"] = raw["mode"]
    if isinstance(raw.get("hash_key"), str):
        out["hash_key"] = raw["hash_key"]
    if isinstance(raw.get("interaction_hash"), str):
        out["interaction_hash"] = raw["interaction_hash"]
    if isinstance(raw.get("selection_active"), bool):
        out["selection_active"] = raw["selection_active"]

    for key in ("frame_idx", "ts_ms", "cols", "rows", "patch_bytes", "patch_cells", "patch_runs",
                "present_bytes", "hovered_link_id", "cursor_offset", "cursor_style",
                "selection_start", "selection_end"):
        value = raw.get(key)
        if key in ("cols", "rows"):
            parsed = _as_positive_int(value)
        else:
            parsed = _as_non_negative_int(value)
        if parsed is not None:
            out[key] = parsed

    for key in ("render_ms", "present_ms"):
        parsed = _as_number(raw.get(key))
        if parsed is not None:
            out[key] = parsed
    return out


def _decode_structured_frame_message(message: str) -> tuple[bytes, dict[str, Any]] | None:
    try:
        obj = json.loads(message)
    except json.JSONDecodeError:
        return None
    if not isinstance(obj, dict):
        return None

    payload = obj.get("payload")
    frame: dict[str, Any] = dict(obj)
    if isinstance(payload, dict):
        frame.update(payload)
    if frame.get("type") != "frame":
        return None

    raw_b64 = frame.get("data_b64")
    if isinstance(raw_b64, str):
        try:
            data = base64.b64decode(raw_b64, validate=True)
        except Exception:
            return None
    elif isinstance(frame.get("bytes_b64"), str):
        try:
            data = base64.b64decode(frame["bytes_b64"], validate=True)
        except Exception:
            return None
    elif isinstance(frame.get("data"), str):
        data = frame["data"].encode("utf-8")
    else:
        return None

    return data, _extract_frame_overrides(frame)


def _percentile(sorted_values: list[float], q: float) -> float:
    if not sorted_values:
        return 0.0
    if len(sorted_values) == 1:
        return sorted_values[0]
    pos = (len(sorted_values) - 1) * q
    lo = int(pos)
    hi = min(lo + 1, len(sorted_values) - 1)
    frac = pos - lo
    return sorted_values[lo] + (sorted_values[hi] - sorted_values[lo]) * frac


def histogram_summary(values_ms: list[float]) -> dict[str, Any]:
    """Compact histogram/quantile summary for JSONL logs."""
    if not values_ms:
        return {
            "count": 0,
            "min": 0.0,
            "max": 0.0,
            "p50": 0.0,
            "p95": 0.0,
            "p99": 0.0,
            "mean": 0.0,
        }
    sorted_values = sorted(values_ms)
    total = sum(sorted_values)
    n = len(sorted_values)
    return {
        "count": n,
        "min": round(sorted_values[0], 3),
        "max": round(sorted_values[-1], 3),
        "p50": round(_percentile(sorted_values, 0.50), 3),
        "p95": round(_percentile(sorted_values, 0.95), 3),
        "p99": round(_percentile(sorted_values, 0.99), 3),
        "mean": round(total / n, 3),
    }


def _normalize_text(text: str, normalization: str) -> str:
    mode = normalization.lower()
    if mode == "none":
        return text
    if mode not in {"nfc", "nfd", "nfkc", "nfkd"}:
        raise ValueError(f"unsupported normalization mode: {normalization}")
    return unicodedata.normalize(mode.upper(), text)


def _assertion_excerpt(text: str, start: int, span: int = 96) -> str:
    if not text:
        return ""
    safe_start = max(0, start - 20)
    end = min(len(text), safe_start + span)
    return text[safe_start:end].replace("\n", "\\n").replace("\r", "\\r")


def _evaluate_scenario_assertions(
    assertions: Any,
    output: bytes,
    recorder: "SessionRecorder",
) -> list[str]:
    errors: list[str] = []
    if assertions is None:
        return errors
    if not isinstance(assertions, list):
        msg = "scenario assertions must be an array"
        recorder.emit("assert", {
            "assertion": "scenario_assertions_schema",
            "status": "failed",
            "details": msg,
        })
        recorder.emit("error", {"message": msg})
        return [msg]

    output_text = output.decode("utf-8", errors="replace")
    for idx, raw in enumerate(assertions):
        assertion_id = f"scenario_assert_{idx:03d}"
        if not isinstance(raw, dict):
            msg = f"{assertion_id}: assertion entry must be an object"
            recorder.emit("assert", {
                "assertion": assertion_id,
                "status": "failed",
                "details": msg,
            })
            recorder.emit("error", {"message": msg})
            errors.append(msg)
            continue

        if isinstance(raw.get("id"), str) and raw["id"]:
            assertion_id = raw["id"]
        category = raw.get("category", "unspecified")
        if not isinstance(category, str):
            category = "unspecified"
        kind = raw.get("kind", "contains")
        if not isinstance(kind, str):
            kind = "contains"
        kind = kind.lower()
        pattern = raw.get("pattern")
        normalization = raw.get("normalization", "none")
        if not isinstance(normalization, str):
            normalization = "none"
        normalization = normalization.lower()

        if not isinstance(pattern, str) or pattern == "":
            msg = f"{assertion_id}: pattern must be a non-empty string"
            recorder.emit("assert", {
                "assertion": assertion_id,
                "status": "failed",
                "details": f"category={category} {msg}",
            })
            recorder.emit("error", {"message": msg, "details": f"category={category}"})
            errors.append(msg)
            continue

        try:
            haystack = _normalize_text(output_text, normalization)
        except ValueError as exc:
            msg = f"{assertion_id}: {exc}"
            recorder.emit("assert", {
                "assertion": assertion_id,
                "status": "failed",
                "details": f"category={category} {msg}",
            })
            recorder.emit("error", {"message": msg, "details": f"category={category}"})
            errors.append(msg)
            continue

        min_count = _as_positive_int(raw.get("min_count")) or 1
        passed = False
        detail = ""

        if kind == "contains":
            count = haystack.count(pattern)
            passed = count >= min_count
            first = haystack.find(pattern)
            excerpt = _assertion_excerpt(haystack, first if first >= 0 else 0)
            detail = (
                f"category={category} kind=contains normalization={normalization} "
                f"pattern={pattern!r} count={count} min_count={min_count} excerpt={excerpt!r}"
            )
        elif kind == "not_contains":
            count = haystack.count(pattern)
            passed = count == 0
            first = haystack.find(pattern)
            excerpt = _assertion_excerpt(haystack, first if first >= 0 else 0)
            detail = (
                f"category={category} kind=not_contains normalization={normalization} "
                f"pattern={pattern!r} count={count} excerpt={excerpt!r}"
            )
        elif kind == "regex":
            regex_flags = re.MULTILINE
            if raw.get("case_insensitive") is True:
                regex_flags |= re.IGNORECASE
            try:
                matches = list(re.finditer(pattern, haystack, flags=regex_flags))
            except re.error as exc:
                msg = f"{assertion_id}: invalid regex {pattern!r}: {exc}"
                recorder.emit("assert", {
                    "assertion": assertion_id,
                    "status": "failed",
                    "details": f"category={category} {msg}",
                })
                recorder.emit("error", {"message": msg, "details": f"category={category}"})
                errors.append(msg)
                continue
            count = len(matches)
            passed = count >= min_count
            first = matches[0].start() if matches else 0
            excerpt = _assertion_excerpt(haystack, first)
            detail = (
                f"category={category} kind=regex normalization={normalization} "
                f"pattern={pattern!r} count={count} min_count={min_count} excerpt={excerpt!r}"
            )
        else:
            msg = f"{assertion_id}: unsupported assertion kind: {kind}"
            recorder.emit("assert", {
                "assertion": assertion_id,
                "status": "failed",
                "details": f"category={category} {msg}",
            })
            recorder.emit("error", {"message": msg, "details": f"category={category}"})
            errors.append(msg)
            continue

        recorder.emit("assert", {
            "assertion": assertion_id,
            "status": "passed" if passed else "failed",
            "details": detail,
        })
        if not passed:
            msg = f"{assertion_id}: assertion failed ({category})"
            recorder.emit("error", {"message": msg, "details": detail})
            errors.append(msg)

    return errors


class SessionRecorder:
    """Records session events and computes rolling checksums."""

    def __init__(
        self,
        run_id: str,
        scenario_name: str,
        jsonl_path: str | None,
        initial_cols: int,
        initial_rows: int,
    ):
        self.run_id = run_id
        self.scenario_name = scenario_name
        self.jsonl_path = jsonl_path
        self.jsonl_file = None
        self.events: list[dict] = []
        self.output_chunks: list[bytes] = []
        self.total_ws_in = 0
        self.total_ws_out = 0
        self.messages_tx = 0
        self.messages_rx = 0
        self.frame_idx = 0
        self.checksum_chain = "0" * 64
        self.current_cols = initial_cols
        self.current_rows = initial_rows
        self.event_idx = 0
        self.start_monotonic = time.perf_counter()
        self.last_frame_monotonic = self.start_monotonic
        self.frame_gap_ms: list[float] = []
        self.seed = int(os.environ.get("E2E_SEED", "0"))
        self.correlation_id = os.environ.get(
            "E2E_CORRELATION_ID",
            f"{self.run_id}:{self.scenario_name}",
        )

        if jsonl_path:
            self.jsonl_file = open(jsonl_path, "w", encoding="utf-8")

    def emit(self, event_type: str, data: dict | None = None):
        """Emit a JSONL event."""
        event = {
            "schema_version": "e2e-jsonl-v1",
            "type": event_type,
            "timestamp": self._timestamp(),
            "run_id": self.run_id,
            "seed": self.seed,
            "correlation_id": self.correlation_id,
        }
        if data:
            event.update(data)
        self.events.append(event)
        if self.jsonl_file:
            self.jsonl_file.write(json.dumps(event, separators=(",", ":")) + "\n")
            self.jsonl_file.flush()
        self.event_idx += 1

    def record_output(self, data: bytes, frame_meta: dict[str, Any] | None = None):
        """Record PTY output received over WebSocket."""
        now = time.perf_counter()
        gap_ms = (now - self.last_frame_monotonic) * 1000.0
        self.last_frame_monotonic = now
        if self.frame_idx > 0:
            self.frame_gap_ms.append(gap_ms)

        self.output_chunks.append(data)
        self.total_ws_out += len(data)
        chunk_hash = sha256_hex(data)
        self.checksum_chain = sha256_hex(
            (self.checksum_chain + chunk_hash).encode()
        )
        self.frame_idx += 1
        ts_ms = int((now - self.start_monotonic) * 1000.0)
        effective_mode = "remote"
        effective_cols = self.current_cols
        effective_rows = self.current_rows
        if frame_meta:
            meta_mode = frame_meta.get("mode")
            if isinstance(meta_mode, str):
                effective_mode = meta_mode
            meta_cols = frame_meta.get("cols")
            if isinstance(meta_cols, int) and meta_cols > 0:
                effective_cols = meta_cols
            meta_rows = frame_meta.get("rows")
            if isinstance(meta_rows, int) and meta_rows > 0:
                effective_rows = meta_rows
        derived_hash_key = frame_hash_key(effective_mode, effective_cols, effective_rows, self.seed)
        event = {
            "frame_idx": self.frame_idx,
            "hash_algo": "sha256",
            "frame_hash": f"sha256:{chunk_hash}",
            "ts_ms": ts_ms,
            "mode": effective_mode,
            "hash_key": derived_hash_key,
            "cols": effective_cols,
            "rows": effective_rows,
            "patch_hash": f"sha256:{chunk_hash}",
            "patch_bytes": len(data),
            # Binary stream proxies: exact cell/run counts are unavailable at this layer.
            "patch_cells": len(data),
            "patch_runs": 1,
            "present_ms": round(gap_ms, 3),
            "present_bytes": len(data),
            "checksum_chain": f"sha256:{self.checksum_chain}",
        }
        if frame_meta:
            event.update(frame_meta)
            if "hash_key" not in frame_meta:
                event["hash_key"] = derived_hash_key
        self.current_cols = effective_cols
        self.current_rows = effective_rows
        self.emit("frame", event)

    def record_send(self, data: bytes):
        """Record data sent to PTY."""
        self.total_ws_in += len(data)
        self.messages_tx += 1

    def record_receive(self):
        """Record a WebSocket message received from the bridge."""
        self.messages_rx += 1

    def set_geometry(self, cols: int, rows: int):
        """Track current terminal geometry for frame/input metadata."""
        self.current_cols = cols
        self.current_rows = rows

    def full_output(self) -> bytes:
        """Return concatenated output."""
        return b"".join(self.output_chunks)

    def final_checksum(self) -> str:
        """Return the final rolling checksum."""
        return self.checksum_chain

    def summary(self) -> dict:
        """Return session summary dict."""
        output = self.full_output()
        return {
            "scenario": self.scenario_name,
            "ws_in_bytes": self.total_ws_in,
            "ws_out_bytes": self.total_ws_out,
            "messages_tx": self.messages_tx,
            "messages_rx": self.messages_rx,
            "frames": self.frame_idx,
            "output_sha256": f"sha256:{sha256_hex(output)}",
            "checksum_chain": f"sha256:{self.checksum_chain}",
            "frame_gap_histogram_ms": histogram_summary(self.frame_gap_ms),
        }

    def close(self):
        if self.jsonl_file:
            self.jsonl_file.close()
            self.jsonl_file = None

    def _timestamp(self) -> str:
        if os.environ.get("E2E_DETERMINISTIC", "1") == "1":
            step_ms = int(os.environ.get("E2E_TIME_STEP_MS", "100"))
            ts = self.event_idx * step_ms
            return f"T{ts:06d}"
        return time.strftime("%Y-%m-%dT%H:%M:%S%z")


async def run_session(url: str, scenario: dict, recorder: SessionRecorder,
                      golden_path: str | None = None) -> dict:
    """Execute a scripted WebSocket session."""
    timeout_s = scenario.get("timeout_s", 30)
    steps = scenario.get("steps", [])
    run_start = time.perf_counter()

    deterministic = os.environ.get("E2E_DETERMINISTIC", "1") == "1"
    rustc_version = command_version("rustc")
    cargo_version = command_version("cargo")
    browser = os.environ.get("E2E_BROWSER", "python-websockets")
    user_agent = os.environ.get(
        "E2E_BROWSER_USER_AGENT",
        f"python-websockets/{getattr(websockets, '__version__', 'unknown')}",
    )
    dpr = float(os.environ.get("E2E_BROWSER_DPR", "1.0"))
    log_dir = str(Path(recorder.jsonl_path).resolve().parent) if recorder.jsonl_path else os.environ.get("E2E_LOG_DIR", "")
    results_dir = os.environ.get("E2E_RESULTS_DIR", log_dir)
    command = f"python3 tests/e2e/lib/ws_client.py --url {url} --scenario {scenario.get('name', 'unknown')}"
    scenario_assertions = scenario.get("assertions", [])
    assertion_count = len(scenario_assertions) if isinstance(scenario_assertions, list) else 0

    recorder.emit("env", {
        "host": platform.node() or platform.platform(),
        "rustc": rustc_version,
        "cargo": cargo_version,
        "git_commit": git_sha(),
        "git_dirty": git_dirty(),
        "deterministic": deterministic,
        "term": os.environ.get("TERM", ""),
        "colorterm": os.environ.get("COLORTERM", ""),
        "no_color": os.environ.get("NO_COLOR", ""),
        "scenario": scenario["name"],
        "initial_cols": scenario.get("initial_cols", 120),
        "initial_rows": scenario.get("initial_rows", 40),
    })
    recorder.emit("browser_env", {
        "browser": browser,
        "browser_version": os.environ.get("E2E_BROWSER_VERSION", ""),
        "user_agent": user_agent,
        "dpr": dpr,
        "platform": platform.system(),
        "locale": os.environ.get("LANG", ""),
        "timezone": os.environ.get("TZ", ""),
        "headless": os.environ.get("E2E_HEADLESS", "true").lower() == "true",
    })
    recorder.emit("run_start", {
        "command": command,
        "log_dir": log_dir,
        "results_dir": results_dir,
        "scenario": scenario["name"],
        "step_count": len(steps),
        "timeout_s": timeout_s,
        "assertion_count": assertion_count,
    })
    fixture_categories = scenario.get("fixture_categories", [])
    if isinstance(fixture_categories, list) and fixture_categories:
        categories = [item for item in fixture_categories if isinstance(item, str) and item]
        recorder.emit("assert", {
            "assertion": "fixture_categories_declared",
            "status": "passed",
            "details": (
                f"count={len(categories)} categories="
                + ",".join(categories)
            ),
        })

    result = {"outcome": "pass", "errors": []}

    try:
        async with websockets.connect(
            url,
            max_size=256 * 1024,
            open_timeout=10,
            close_timeout=5,
        ) as ws:
            # Background reader task.
            read_task = asyncio.create_task(_read_loop(ws, recorder))

            for i, step in enumerate(steps):
                step_type = step["type"]
                delay_ms = step.get("delay_ms", 0)
                step_name = f"{i:03d}:{step_type}"
                recorder.emit("step_start", {
                    "step": step_name,
                    "mode": "remote",
                    "hash_key": frame_hash_key("remote", recorder.current_cols, recorder.current_rows, recorder.seed),
                    "cols": recorder.current_cols,
                    "rows": recorder.current_rows,
                })
                step_started = time.perf_counter()

                if delay_ms > 0:
                    await asyncio.sleep(delay_ms / 1000.0)

                if step_type == "send":
                    data = _decode_step_data(step)
                    await ws.send(data)
                    recorder.record_send(data)
                    recorder.emit("input", {
                        "input_type": step.get("input_type", "keys"),
                        "encoding": "base64",
                        "bytes_b64": base64.b64encode(data).decode("ascii"),
                        "input_hash": f"sha256:{sha256_hex(data)}",
                        "details": step.get("comment", ""),
                        "mode": "remote",
                        "hash_key": frame_hash_key(
                            "remote",
                            recorder.current_cols,
                            recorder.current_rows,
                            recorder.seed,
                        ),
                        "cols": recorder.current_cols,
                        "rows": recorder.current_rows,
                    })

                elif step_type == "resize":
                    cols = step["cols"]
                    rows = step["rows"]
                    msg = json.dumps({"type": "resize", "cols": cols, "rows": rows})
                    await ws.send(msg)
                    recorder.record_send(msg.encode("utf-8"))
                    recorder.set_geometry(cols, rows)
                    recorder.emit("input", {
                        "input_type": "resize",
                        "encoding": "json",
                        "input_hash": f"sha256:{sha256_hex(msg.encode('utf-8'))}",
                        "details": step.get("comment", ""),
                        "mode": "remote",
                        "hash_key": frame_hash_key("remote", cols, rows, recorder.seed),
                        "cols": cols,
                        "rows": rows,
                    })

                elif step_type == "wait":
                    wait_ms = step.get("ms", 100)
                    await asyncio.sleep(wait_ms / 1000.0)

                elif step_type == "drain":
                    # Wait for output to settle.
                    await asyncio.sleep(0.5)
                else:
                    raise ValueError(f"unknown step type: {step_type}")

                recorder.emit("step_end", {
                    "step": step_name,
                    "status": "passed",
                    "duration_ms": int((time.perf_counter() - step_started) * 1000.0),
                    "mode": "remote",
                    "hash_key": frame_hash_key(
                        "remote",
                        recorder.current_cols,
                        recorder.current_rows,
                        recorder.seed,
                    ),
                    "cols": recorder.current_cols,
                    "rows": recorder.current_rows,
                })

            # Give a final drain period.
            await asyncio.sleep(0.3)
            read_task.cancel()
            try:
                await read_task
            except asyncio.CancelledError:
                pass

    except Exception as e:
        result["outcome"] = "fail"
        result["errors"].append(str(e))
        recorder.emit("error", {"message": str(e)})

    # Compute summary.
    summary = recorder.summary()
    result.update(summary)

    # Golden transcript comparison.
    if golden_path and os.path.exists(golden_path):
        with open(golden_path, "r") as f:
            golden = json.load(f)
        expected_checksum = golden.get("checksum_chain", "")
        if expected_checksum and expected_checksum != summary["checksum_chain"]:
            result["outcome"] = "fail"
            result["errors"].append(
                f"Golden checksum mismatch: expected {expected_checksum}, "
                f"got {summary['checksum_chain']}"
            )
            recorder.emit("assert", {
                "assertion": "golden_checksum_chain",
                "status": "failed",
                "details": (
                    f"expected={expected_checksum} actual={summary['checksum_chain']} "
                    f"frames_expected={golden.get('frames', -1)} frames_actual={summary['frames']}"
                ),
            })
        else:
            recorder.emit("assert", {
                "assertion": "golden_checksum_chain",
                "status": "passed",
                "details": f"checksum={summary['checksum_chain']} frames={summary['frames']}",
            })

    assertion_errors = _evaluate_scenario_assertions(
        scenario_assertions,
        recorder.full_output(),
        recorder,
    )
    if assertion_errors:
        result["outcome"] = "fail"
        result["errors"].extend(assertion_errors)
    result["assertions_failed"] = len(assertion_errors)
    result["assertions_total"] = assertion_count

    recorder.emit("ws_metrics", {
        "label": scenario["name"],
        "ws_url": url,
        "bytes_tx": summary["ws_in_bytes"],
        "bytes_rx": summary["ws_out_bytes"],
        "messages_tx": summary["messages_tx"],
        "messages_rx": summary["messages_rx"],
        "latency_histogram_ms": summary["frame_gap_histogram_ms"],
    })

    duration_ms = int((time.perf_counter() - run_start) * 1000.0)
    recorder.emit("run_end", {
        "status": "passed" if result["outcome"] == "pass" else "failed",
        "duration_ms": duration_ms,
        "failed_count": len(result["errors"]),
        "outcome": result["outcome"],
        "ws_in_bytes": summary["ws_in_bytes"],
        "ws_out_bytes": summary["ws_out_bytes"],
        "frames": summary["frames"],
        "output_sha256": summary["output_sha256"],
        "checksum_chain": summary["checksum_chain"],
    })

    return result


async def _read_loop(ws, recorder: SessionRecorder):
    """Background task to read WebSocket output."""
    try:
        async for message in ws:
            recorder.record_receive()
            if isinstance(message, bytes):
                recorder.record_output(message)
            elif isinstance(message, str):
                structured = _decode_structured_frame_message(message)
                if structured is None:
                    recorder.record_output(message.encode("utf-8"))
                else:
                    data, frame_meta = structured
                    recorder.record_output(data, frame_meta=frame_meta)
    except websockets.exceptions.ConnectionClosed:
        pass


def _decode_step_data(step: dict) -> bytes:
    """Decode step data from hex, base64, or utf-8."""
    if "data_hex" in step:
        return bytes.fromhex(step["data_hex"])
    if "data_b64" in step:
        return base64.b64decode(step["data_b64"])
    if "data" in step:
        return step["data"].encode("utf-8")
    return b""


def save_transcript(output: bytes, path: str):
    """Save raw output as a transcript file."""
    with open(path, "wb") as f:
        f.write(output)


def run_self_tests() -> int:
    class WsClientTests(unittest.TestCase):
        def test_decode_structured_frame_message_top_level(self) -> None:
            msg = json.dumps({
                "type": "frame",
                "data_b64": base64.b64encode(b"abc").decode("ascii"),
                "frame_hash": "fnv1a64:deadbeef",
                "interaction_hash": "fnv1a64:cafebabe",
                "selection_active": True,
                "selection_start": 1,
                "selection_end": 3,
            })
            decoded = _decode_structured_frame_message(msg)
            self.assertIsNotNone(decoded)
            data, meta = decoded or (b"", {})
            self.assertEqual(data, b"abc")
            self.assertEqual(meta["frame_hash"], "fnv1a64:deadbeef")
            self.assertEqual(meta["interaction_hash"], "fnv1a64:cafebabe")
            self.assertTrue(meta["selection_active"])
            self.assertEqual(meta["selection_start"], 1)
            self.assertEqual(meta["selection_end"], 3)

        def test_decode_structured_frame_message_nested_payload(self) -> None:
            msg = json.dumps({
                "type": "event",
                "payload": {
                    "type": "frame",
                    "bytes_b64": base64.b64encode(b"xyz").decode("ascii"),
                    "hovered_link_id": 9,
                    "cursor_offset": 4,
                    "cursor_style": 2,
                },
            })
            decoded = _decode_structured_frame_message(msg)
            self.assertIsNotNone(decoded)
            data, meta = decoded or (b"", {})
            self.assertEqual(data, b"xyz")
            self.assertEqual(meta["hovered_link_id"], 9)
            self.assertEqual(meta["cursor_offset"], 4)
            self.assertEqual(meta["cursor_style"], 2)

        def test_record_output_applies_frame_meta_overrides(self) -> None:
            recorder = SessionRecorder("run-1", "scenario", None, 80, 24)
            recorder.record_output(
                b"abc",
                frame_meta={
                    "frame_hash": "sha256:override",
                    "interaction_hash": "fnv1a64:1234",
                    "selection_active": True,
                    "selection_start": 1,
                    "selection_end": 2,
                    "cols": 100,
                    "rows": 50,
                },
            )
            frame = recorder.events[-1]
            self.assertEqual(frame["frame_hash"], "sha256:override")
            self.assertEqual(frame["interaction_hash"], "fnv1a64:1234")
            self.assertTrue(frame["selection_active"])
            self.assertEqual(frame["selection_start"], 1)
            self.assertEqual(frame["selection_end"], 2)
            self.assertEqual(frame["cols"], 100)
            self.assertEqual(frame["rows"], 50)
            self.assertEqual(frame["hash_key"], frame_hash_key("remote", 100, 50, 0))
            self.assertEqual(recorder.current_cols, 100)
            self.assertEqual(recorder.current_rows, 50)

        def test_record_output_derives_hash_key_from_mode_override(self) -> None:
            recorder = SessionRecorder("run-1", "scenario", None, 80, 24)
            recorder.record_output(
                b"abc",
                frame_meta={
                    "mode": "inline",
                    "cols": 120,
                    "rows": 40,
                },
            )
            frame = recorder.events[-1]
            self.assertEqual(frame["mode"], "inline")
            self.assertEqual(frame["hash_key"], frame_hash_key("inline", 120, 40, 0))

        def test_record_output_keeps_explicit_hash_key_override(self) -> None:
            recorder = SessionRecorder("run-1", "scenario", None, 80, 24)
            recorder.record_output(
                b"abc",
                frame_meta={
                    "mode": "inline",
                    "cols": 120,
                    "rows": 40,
                    "hash_key": "explicit-key",
                },
            )
            frame = recorder.events[-1]
            self.assertEqual(frame["hash_key"], "explicit-key")

        def test_jsonl_writer_truncates_existing_file(self) -> None:
            with tempfile.TemporaryDirectory() as temp_dir:
                path = Path(temp_dir) / "events.jsonl"
                path.write_text('{"stale":true}\n', encoding="utf-8")
                recorder = SessionRecorder("run-1", "scenario", str(path), 80, 24)
                recorder.emit("run_start", {"scenario": "fresh"})
                recorder.close()
                lines = path.read_text(encoding="utf-8").splitlines()
                self.assertEqual(len(lines), 1)
                event = json.loads(lines[0])
                self.assertEqual(event["run_id"], "run-1")
                self.assertEqual(event["type"], "run_start")

        def test_extract_frame_overrides_rejects_invalid_types(self) -> None:
            out = _extract_frame_overrides({
                "selection_active": "true",
                "hovered_link_id": -1,
                "frame_hash": 1,
                "present_ms": "1.2",
                "cols": 0,
                "rows": True,
            })
            self.assertEqual(out, {})

        def test_emit_includes_correlation_id(self) -> None:
            recorder = SessionRecorder("run-1", "scenario", None, 80, 24)
            recorder.emit("run_start", {"command": "x"})
            self.assertEqual(recorder.events[0]["correlation_id"], "run-1:scenario")

        def test_scenario_assertions_support_normalization(self) -> None:
            recorder = SessionRecorder("run-1", "scenario", None, 80, 24)
            assertions = [
                {
                    "id": "norm_nfc",
                    "kind": "contains",
                    "pattern": "é",
                    "normalization": "nfc",
                    "category": "normalization",
                },
                {
                    "id": "regex_emoji",
                    "kind": "regex",
                    "pattern": r"emoji:\s+😀",
                    "normalization": "none",
                    "category": "emoji_zwj",
                },
            ]
            output = "combining: e\u0301\nemoji: 😀\n".encode("utf-8")
            errors = _evaluate_scenario_assertions(assertions, output, recorder)
            self.assertEqual(errors, [])
            assert_events = [e for e in recorder.events if e["type"] == "assert"]
            statuses = {e["assertion"]: e["status"] for e in assert_events}
            self.assertEqual(statuses.get("norm_nfc"), "passed")
            self.assertEqual(statuses.get("regex_emoji"), "passed")

        def test_scenario_assertions_emit_failure_context(self) -> None:
            recorder = SessionRecorder("run-1", "scenario", None, 80, 24)
            assertions = [
                {
                    "id": "expected_missing_marker",
                    "kind": "contains",
                    "pattern": "THIS_MARKER_DOES_NOT_EXIST",
                    "category": "failure_injection",
                }
            ]
            errors = _evaluate_scenario_assertions(assertions, b"hello world\n", recorder)
            self.assertEqual(len(errors), 1)
            assert_event = next(
                e for e in recorder.events
                if e["type"] == "assert" and e["assertion"] == "expected_missing_marker"
            )
            self.assertEqual(assert_event["status"], "failed")
            self.assertIn("failure_injection", assert_event["details"])
            error_event = next(e for e in recorder.events if e["type"] == "error")
            self.assertIn("expected_missing_marker", error_event["message"])

    suite = unittest.defaultTestLoader.loadTestsFromTestCase(WsClientTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def main():
    parser = argparse.ArgumentParser(description="WebSocket remote terminal client")
    parser.add_argument("--url", default="ws://127.0.0.1:9231", help="Bridge URL")
    parser.add_argument("--scenario", required=False, help="Scenario JSON file")
    parser.add_argument("--golden", default=None, help="Golden transcript JSON")
    parser.add_argument("--jsonl", default=None, help="JSONL output file")
    parser.add_argument("--transcript", default=None, help="Save raw output transcript")
    parser.add_argument("--summary", action="store_true", help="Print summary JSON to stdout")
    parser.add_argument("--self-test", action="store_true", help="Run ws_client unit tests and exit")
    args = parser.parse_args()

    if args.self_test:
        sys.exit(run_self_tests())
    if not args.scenario:
        parser.error("--scenario is required unless --self-test is set")

    with open(args.scenario, "r") as f:
        scenario = json.load(f)

    seed = int(os.environ.get("E2E_SEED", "0"))
    run_id = make_run_id(seed)
    recorder = SessionRecorder(
        run_id,
        scenario["name"],
        args.jsonl,
        int(scenario.get("initial_cols", 120)),
        int(scenario.get("initial_rows", 40)),
    )

    try:
        result = asyncio.run(run_session(args.url, scenario, recorder, args.golden))
    finally:
        recorder.close()

    if args.transcript:
        save_transcript(recorder.full_output(), args.transcript)

    if args.summary or not args.jsonl:
        print(json.dumps(result, indent=2))

    sys.exit(0 if result["outcome"] == "pass" else 1)


if __name__ == "__main__":
    main()

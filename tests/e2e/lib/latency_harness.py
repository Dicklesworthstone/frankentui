#!/usr/bin/env python3
"""Keystroke-to-photon latency measurement harness for remote terminal sessions.

Sends individual keystrokes over WebSocket and measures time-to-first-output-byte.
Computes p50/p95/p99 latency, round-trip jitter, and output throughput.

Usage:
    python3 latency_harness.py --url ws://127.0.0.1:9231 --scenario scenario.json
    python3 latency_harness.py --url ws://127.0.0.1:9231 --scenario scenario.json --budget budget.json

Budget JSON format:
{
    "p50_ms": 10,
    "p95_ms": 50,
    "p99_ms": 100,
    "max_jitter_ms": 30,
    "min_throughput_kbps": 100
}
"""

import argparse
import asyncio
import json
import math
import os
import subprocess
import sys
import time
from pathlib import Path

try:
    import websockets
except ImportError:
    print("ERROR: 'websockets' package not available", file=sys.stderr)
    sys.exit(1)


# Default latency budgets (in milliseconds).
DEFAULT_BUDGETS = {
    "p50_ms": 10.0,
    "p95_ms": 50.0,
    "p99_ms": 100.0,
    "max_jitter_ms": 30.0,
    "min_throughput_kbps": 50.0,
}


def git_sha() -> str:
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, timeout=5
        )
        return result.stdout.strip() if result.returncode == 0 else "unknown"
    except Exception:
        return "unknown"


def percentile(data: list[float], p: float) -> float:
    """Compute the p-th percentile of a sorted list."""
    if not data:
        return 0.0
    k = (len(data) - 1) * p / 100.0
    f = math.floor(k)
    c = math.ceil(k)
    if f == c:
        return data[int(k)]
    d0 = data[int(f)] * (c - k)
    d1 = data[int(c)] * (k - f)
    return d0 + d1


def compute_stats(latencies_ms: list[float]) -> dict:
    """Compute latency statistics from a list of measurements."""
    if not latencies_ms:
        return {"count": 0, "p50_ms": 0, "p95_ms": 0, "p99_ms": 0,
                "mean_ms": 0, "min_ms": 0, "max_ms": 0, "jitter_ms": 0}

    sorted_lat = sorted(latencies_ms)
    mean = sum(sorted_lat) / len(sorted_lat)

    # Jitter: standard deviation of inter-measurement deltas.
    if len(sorted_lat) > 1:
        diffs = [abs(sorted_lat[i+1] - sorted_lat[i]) for i in range(len(sorted_lat)-1)]
        jitter = (sum(d*d for d in diffs) / len(diffs)) ** 0.5
    else:
        jitter = 0.0

    return {
        "count": len(sorted_lat),
        "p50_ms": round(percentile(sorted_lat, 50), 3),
        "p95_ms": round(percentile(sorted_lat, 95), 3),
        "p99_ms": round(percentile(sorted_lat, 99), 3),
        "mean_ms": round(mean, 3),
        "min_ms": round(sorted_lat[0], 3),
        "max_ms": round(sorted_lat[-1], 3),
        "jitter_ms": round(jitter, 3),
    }


def check_budgets(stats: dict, budgets: dict) -> list[dict]:
    """Check stats against latency budgets. Return list of violations."""
    violations = []

    for key in ("p50_ms", "p95_ms", "p99_ms"):
        if key in budgets and stats.get(key, 0) > budgets[key]:
            violations.append({
                "metric": key,
                "actual": stats[key],
                "budget": budgets[key],
                "severity": "critical" if key == "p50_ms" else "warning",
            })

    if "max_jitter_ms" in budgets and stats.get("jitter_ms", 0) > budgets["max_jitter_ms"]:
        violations.append({
            "metric": "jitter_ms",
            "actual": stats["jitter_ms"],
            "budget": budgets["max_jitter_ms"],
            "severity": "warning",
        })

    return violations


async def measure_latency(url: str, scenario: dict) -> dict:
    """Execute a latency measurement session."""
    steps = scenario.get("steps", [])
    latencies: list[float] = []
    total_output_bytes = 0
    session_start = time.monotonic()

    try:
        async with websockets.connect(
            url,
            max_size=256 * 1024,
            open_timeout=10,
            close_timeout=5,
        ) as ws:
            # Event to signal output received.
            output_event = asyncio.Event()
            output_buf: list[bytes] = []

            async def reader():
                nonlocal total_output_bytes
                try:
                    async for msg in ws:
                        if isinstance(msg, bytes):
                            total_output_bytes += len(msg)
                            output_buf.append(msg)
                            output_event.set()
                except websockets.exceptions.ConnectionClosed:
                    pass

            read_task = asyncio.create_task(reader())

            for step in steps:
                step_type = step["type"]
                delay_ms = step.get("delay_ms", 0)

                if delay_ms > 0:
                    await asyncio.sleep(delay_ms / 1000.0)

                if step_type == "send":
                    is_probe = step.get("latency_probe", False)
                    data = _decode_data(step)

                    if is_probe:
                        # Clear any pending output.
                        output_event.clear()
                        output_buf.clear()

                        # Send and measure.
                        t_send = time.monotonic()
                        await ws.send(data)

                        # Wait for first output byte (with timeout).
                        try:
                            await asyncio.wait_for(output_event.wait(), timeout=2.0)
                            t_recv = time.monotonic()
                            latency_ms = (t_recv - t_send) * 1000.0
                            latencies.append(latency_ms)
                        except asyncio.TimeoutError:
                            latencies.append(2000.0)  # Timeout as 2000ms.
                    else:
                        await ws.send(data)

                elif step_type == "resize":
                    msg = json.dumps({"type": "resize", "cols": step["cols"], "rows": step["rows"]})
                    await ws.send(msg)

                elif step_type == "wait":
                    await asyncio.sleep(step.get("ms", 100) / 1000.0)

                elif step_type == "drain":
                    await asyncio.sleep(0.3)

            await asyncio.sleep(0.3)
            read_task.cancel()
            try:
                await read_task
            except asyncio.CancelledError:
                pass

    except Exception as e:
        return {
            "outcome": "error",
            "error": str(e),
            "latencies_ms": latencies,
            "stats": compute_stats(latencies),
        }

    session_duration_s = time.monotonic() - session_start
    throughput_kbps = (total_output_bytes / 1024.0) / max(session_duration_s, 0.001)

    stats = compute_stats(latencies)
    stats["throughput_kbps"] = round(throughput_kbps, 3)
    stats["session_duration_s"] = round(session_duration_s, 3)
    stats["total_output_bytes"] = total_output_bytes

    return {
        "outcome": "pass",
        "latencies_ms": [round(l, 3) for l in latencies],
        "stats": stats,
    }


def _decode_data(step: dict) -> bytes:
    import base64
    if "data_hex" in step:
        return bytes.fromhex(step["data_hex"])
    if "data_b64" in step:
        return base64.b64decode(step["data_b64"])
    if "data" in step:
        return step["data"].encode("utf-8")
    return b""


def main():
    parser = argparse.ArgumentParser(description="Keystroke-to-photon latency harness")
    parser.add_argument("--url", default="ws://127.0.0.1:9231", help="Bridge URL")
    parser.add_argument("--scenario", required=True, help="Scenario JSON file")
    parser.add_argument("--budget", default=None, help="Budget JSON file")
    parser.add_argument("--jsonl", default=None, help="JSONL output file")
    parser.add_argument("--gate", action="store_true", help="Exit non-zero on budget violations")
    args = parser.parse_args()

    with open(args.scenario) as f:
        scenario = json.load(f)

    budgets = dict(DEFAULT_BUDGETS)
    if args.budget:
        with open(args.budget) as f:
            budgets.update(json.load(f))

    result = asyncio.run(measure_latency(args.url, scenario))
    stats = result.get("stats", {})
    violations = check_budgets(stats, budgets) if result["outcome"] == "pass" else []

    report = {
        "scenario": scenario["name"],
        "git_commit": git_sha(),
        "outcome": result["outcome"],
        "stats": stats,
        "budgets": budgets,
        "violations": violations,
        "latencies_ms": result.get("latencies_ms", []),
    }

    if args.jsonl:
        with open(args.jsonl, "a") as f:
            event = {
                "schema_version": "e2e-jsonl-v1",
                "type": "latency_report",
                "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                "run_id": f"latency-{int(time.time())}",
                "seed": int(os.environ.get("E2E_SEED", "0")),
                **report,
            }
            f.write(json.dumps(event, separators=(",", ":")) + "\n")

    print(json.dumps(report, indent=2))

    if args.gate and violations:
        critical = [v for v in violations if v["severity"] == "critical"]
        if critical:
            sys.exit(2)
        sys.exit(1)

    if result["outcome"] != "pass":
        sys.exit(1)


if __name__ == "__main__":
    main()

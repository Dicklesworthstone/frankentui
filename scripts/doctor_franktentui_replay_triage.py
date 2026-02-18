#!/usr/bin/env python3
"""Replay/triage helper for doctor_franktentui e2e failures."""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass
class Signal:
    severity: int
    message: str
    event_index: int | None
    event_type: str | None
    case_id: str | None
    step_id: str | None
    pointers: list[str]
    expected: dict[str, Any]
    actual: dict[str, Any]

    def as_dict(self) -> dict[str, Any]:
        return {
            "severity": self.severity,
            "message": self.message,
            "event_index": self.event_index,
            "event_type": self.event_type,
            "case_id": self.case_id,
            "step_id": self.step_id,
            "pointers": self.pointers,
            "expected": self.expected,
            "actual": self.actual,
        }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate replay/triage summary from doctor_franktentui e2e artifacts",
    )
    parser.add_argument(
        "--run-root",
        required=True,
        help="Path to a doctor_franktentui e2e run root (contains meta/)",
    )
    parser.add_argument(
        "--output-json",
        default="",
        help="Optional output path for machine-readable triage report",
    )
    parser.add_argument(
        "--max-signals",
        type=int,
        default=5,
        help="Maximum number of top failure signals in compact output",
    )
    parser.add_argument(
        "--max-timeline",
        type=int,
        default=40,
        help="Maximum number of timeline entries in compact output",
    )
    return parser.parse_args()


def load_json(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {}
    if isinstance(value, dict):
        return value
    return {}


def load_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.exists():
        return []
    items: list[dict[str, Any]] = []
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line:
            continue
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(payload, dict):
            items.append(payload)
    return items


def collect_pointers(actual: dict[str, Any]) -> list[str]:
    pointer_keys = [
        "stdout_log",
        "stderr_log",
        "env_snapshot",
        "events_jsonl",
        "events_validation_report",
        "summary_json",
    ]
    pointers: list[str] = []
    for key in pointer_keys:
        value = actual.get(key)
        if isinstance(value, str) and value:
            pointers.append(f"{key}={value}")
    missing_artifacts = actual.get("missing_artifacts")
    if isinstance(missing_artifacts, list):
        for artifact in missing_artifacts:
            if isinstance(artifact, str) and artifact:
                pointers.append(f"missing_artifact={artifact}")
    return pointers


def collect_signals(
    summary: dict[str, Any],
    events: list[dict[str, Any]],
    validation_report: dict[str, Any],
) -> list[Signal]:
    signals: list[Signal] = []

    summary_status = summary.get("status")
    if summary_status == "failed":
        signals.append(
            Signal(
                severity=100,
                message="summary status is failed",
                event_index=None,
                event_type="summary",
                case_id=None,
                step_id=None,
                pointers=collect_pointers(summary),
                expected={},
                actual=summary,
            )
        )

    validation_status = validation_report.get("status")
    if validation_status == "failed":
        errors = validation_report.get("errors")
        message = "events validation report failed"
        if isinstance(errors, list) and errors:
            message = f"events validation report failed ({len(errors)} errors)"
        signals.append(
            Signal(
                severity=95,
                message=message,
                event_index=None,
                event_type="events_validation",
                case_id=None,
                step_id=None,
                pointers=collect_pointers(validation_report),
                expected={},
                actual={"errors": errors if isinstance(errors, list) else []},
            )
        )

    for index, event in enumerate(events, start=1):
        event_type = event.get("event_type")
        case_id = event.get("case_id") if isinstance(event.get("case_id"), str) else None
        step_id = event.get("step_id") if isinstance(event.get("step_id"), str) else None
        expected = event.get("expected") if isinstance(event.get("expected"), dict) else {}
        actual = event.get("actual") if isinstance(event.get("actual"), dict) else {}
        exit_code = event.get("exit_code")

        if event_type in {"step_end", "case_end", "run_end"} and isinstance(exit_code, int) and exit_code != 0:
            signals.append(
                Signal(
                    severity=90 if event_type == "run_end" else 75,
                    message=f"{event_type} exit_code={exit_code}",
                    event_index=index,
                    event_type=str(event_type),
                    case_id=case_id,
                    step_id=step_id,
                    pointers=collect_pointers(actual),
                    expected=expected,
                    actual=actual,
                )
            )

        if actual.get("pass") is False:
            signals.append(
                Signal(
                    severity=80,
                    message="case reported pass=false",
                    event_index=index,
                    event_type=str(event_type),
                    case_id=case_id,
                    step_id=step_id,
                    pointers=collect_pointers(actual),
                    expected=expected,
                    actual=actual,
                )
            )

        missing_artifacts = actual.get("missing_artifacts")
        if isinstance(missing_artifacts, list) and missing_artifacts:
            signals.append(
                Signal(
                    severity=70,
                    message=f"missing_artifacts count={len(missing_artifacts)}",
                    event_index=index,
                    event_type=str(event_type),
                    case_id=case_id,
                    step_id=step_id,
                    pointers=collect_pointers(actual),
                    expected=expected,
                    actual=actual,
                )
            )

        if expected and actual:
            mismatch_keys: list[str] = []
            for key, expected_value in expected.items():
                if key in actual and actual[key] != expected_value:
                    mismatch_keys.append(key)
            if mismatch_keys:
                signals.append(
                    Signal(
                        severity=65,
                        message=f"expected/actual mismatch keys={','.join(sorted(mismatch_keys))}",
                        event_index=index,
                        event_type=str(event_type),
                        case_id=case_id,
                        step_id=step_id,
                        pointers=collect_pointers(actual),
                        expected=expected,
                        actual=actual,
                    )
                )

    signals.sort(
        key=lambda signal: (
            -signal.severity,
            signal.event_index if signal.event_index is not None else sys.maxsize,
        )
    )
    return signals


def dedupe_signals(signals: list[Signal]) -> list[Signal]:
    seen: set[tuple[Any, ...]] = set()
    deduped: list[Signal] = []
    for signal in signals:
        fingerprint = (
            signal.severity,
            signal.message,
            signal.event_index,
            signal.event_type,
            signal.case_id,
            signal.step_id,
        )
        if fingerprint in seen:
            continue
        seen.add(fingerprint)
        deduped.append(signal)
    return deduped


def build_timeline(events: list[dict[str, Any]], max_timeline: int) -> list[dict[str, Any]]:
    timeline: list[dict[str, Any]] = []
    for index, event in enumerate(events, start=1):
        timeline.append(
            {
                "event_index": index,
                "timestamp_utc": event.get("timestamp_utc"),
                "event_type": event.get("event_type"),
                "case_id": event.get("case_id"),
                "step_id": event.get("step_id"),
                "command": event.get("command"),
                "exit_code": event.get("exit_code"),
            }
        )
    if max_timeline <= 0:
        return timeline
    return timeline[:max_timeline]


def main() -> int:
    args = parse_args()

    run_root = Path(args.run_root).resolve()
    meta_dir = run_root / "meta"
    summary_path = meta_dir / "summary.json"
    events_path = meta_dir / "events.jsonl"
    validation_path = meta_dir / "events_validation_report.json"

    summary = load_json(summary_path)
    events = load_jsonl(events_path)
    validation_report = load_json(validation_path)

    if not summary and not events:
        print(
            json.dumps(
                {
                    "status": "error",
                    "message": "could not load summary/events artifacts",
                    "run_root": str(run_root),
                    "summary_path": str(summary_path),
                    "events_path": str(events_path),
                }
            ),
            file=sys.stderr,
        )
        return 2

    timeline = build_timeline(events, max_timeline=max(0, args.max_timeline))
    signals = dedupe_signals(collect_signals(summary, events, validation_report))
    top_signals = signals[: max(1, args.max_signals)]

    status = summary.get("status", "unknown")
    if status == "unknown":
        if signals:
            status = "failed"
        elif events:
            status = "passed"

    report = {
        "status": status,
        "run_root": str(run_root),
        "summary_path": str(summary_path),
        "events_path": str(events_path),
        "events_validation_report_path": str(validation_path),
        "event_count": len(events),
        "timeline": timeline,
        "signal_count": len(signals),
        "top_failure_signals": [signal.as_dict() for signal in top_signals],
    }

    output_json = Path(args.output_json) if args.output_json else (meta_dir / "replay_triage_report.json")
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    lines: list[str] = []
    lines.append(f"status={status}")
    lines.append(f"run_root={run_root}")
    lines.append(f"event_count={len(events)}")
    lines.append(f"signal_count={len(signals)}")
    lines.append(f"report_json={output_json}")
    if top_signals:
        lines.append("top_failure_signals:")
        for signal in top_signals:
            location = "event=summary"
            if signal.event_index is not None:
                location = f"event_index={signal.event_index}"
            lines.append(
                f"- severity={signal.severity} {location} event_type={signal.event_type} "
                f"case_id={signal.case_id} step_id={signal.step_id} message={signal.message}"
            )
            for pointer in signal.pointers[:3]:
                lines.append(f"  pointer={pointer}")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

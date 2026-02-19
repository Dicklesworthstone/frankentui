#!/usr/bin/env python3
"""Validate doctor_frankentui e2e JSONL telemetry against schema profile."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

HEX64_RE = re.compile(r"^[0-9a-f]{64}$")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Validate doctor_frankentui e2e JSONL telemetry",
    )
    parser.add_argument("--input", required=True, help="Path to JSONL event stream")
    parser.add_argument(
        "--schema",
        default="crates/doctor_frankentui/coverage/e2e_jsonl_schema.json",
        help="Path to schema profile JSON",
    )
    parser.add_argument(
        "--workflow",
        choices=["generic", "happy", "failure"],
        default="generic",
        help="Workflow profile to enforce",
    )
    parser.add_argument(
        "--report-json",
        default="",
        help="Optional path to write machine-readable validation report",
    )
    return parser.parse_args()


def type_name(value: Any) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, int):
        return "integer"
    if isinstance(value, float):
        return "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    if isinstance(value, dict):
        return "object"
    return type(value).__name__


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise SystemExit(f"[validator] file not found: {path}") from exc
    except json.JSONDecodeError as exc:
        raise SystemExit(f"[validator] invalid JSON in {path}: {exc}") from exc


def validate_schema_contract(schema: dict[str, Any], workflow: str) -> list[str]:
    errors: list[str] = []
    required_schema_keys = [
        "schema_name",
        "schema_version",
        "required_fields",
        "field_types",
        "sha256_fields",
        "event_type_enum",
        "workflow_rules",
    ]
    for key in required_schema_keys:
        if key not in schema:
            errors.append(f"schema missing top-level key: {key}")

    workflow_rules = schema.get("workflow_rules", {})
    if workflow not in workflow_rules:
        errors.append(f"schema missing workflow rule for: {workflow}")
        return errors

    required_workflow_keys = [
        "required_event_types",
        "require_case_id",
        "expected_actual_required_keys",
        "expected_actual_enforced_event_types",
        "require_unique_correlation_ids",
        "require_monotonic_correlation_suffix",
        "required_step_event_pairs",
        "required_case_event_pairs",
        "min_artifact_events",
    ]
    for key in required_workflow_keys:
        if key not in workflow_rules[workflow]:
            errors.append(f"schema workflow '{workflow}' missing key: {key}")

    return errors


def validate_sha256(field: str, value: Any, errors: list[str], line_no: int) -> None:
    if value is None:
        return
    if not isinstance(value, str) or not HEX64_RE.fullmatch(value):
        errors.append(
            f"line {line_no}: field '{field}' must be null or lowercase sha256 hex"
        )


def validate_artifact_hashes(value: Any, errors: list[str], line_no: int) -> None:
    if not isinstance(value, dict):
        errors.append(f"line {line_no}: field 'artifact_hashes' must be an object")
        return

    for key, item in value.items():
        if not isinstance(key, str) or not key:
            errors.append(
                f"line {line_no}: artifact_hashes keys must be non-empty strings"
            )
            continue
        if not isinstance(item, str) or not HEX64_RE.fullmatch(item):
            errors.append(
                f"line {line_no}: artifact_hashes['{key}'] must be lowercase sha256 hex"
            )


def validate_event(
    event: dict[str, Any],
    line_no: int,
    schema: dict[str, Any],
    workflow: str,
) -> list[str]:
    errors: list[str] = []

    required_fields = schema["required_fields"]
    field_types = schema["field_types"]
    sha256_fields = schema["sha256_fields"]
    allowed_event_types = set(schema["event_type_enum"])

    for field in required_fields:
        if field not in event:
            errors.append(f"line {line_no}: missing required field '{field}'")

    for field, expected_types in field_types.items():
        if field not in event:
            continue
        actual = type_name(event[field])
        if actual not in expected_types:
            errors.append(
                f"line {line_no}: field '{field}' type mismatch (got={actual}, expected={expected_types})"
            )

    if "event_type" in event and event["event_type"] not in allowed_event_types:
        errors.append(
            f"line {line_no}: event_type '{event['event_type']}' not in allowed enum"
        )

    if "duration_ms" in event and isinstance(event["duration_ms"], int):
        if event["duration_ms"] < 0:
            errors.append(f"line {line_no}: duration_ms must be >= 0")

    for field in sha256_fields:
        if field in event:
            validate_sha256(field, event[field], errors, line_no)

    if "artifact_hashes" in event:
        validate_artifact_hashes(event["artifact_hashes"], errors, line_no)
    if event.get("event_type") == "artifact":
        artifact_hashes = event.get("artifact_hashes")
        if isinstance(artifact_hashes, dict) and not artifact_hashes:
            errors.append(
                f"line {line_no}: artifact event must include at least one artifact hash"
            )

    workflow_rule = schema["workflow_rules"][workflow]
    required_expected_actual_keys = workflow_rule["expected_actual_required_keys"]
    enforced_event_types = set(workflow_rule["expected_actual_enforced_event_types"])

    if workflow_rule.get("require_case_id", False):
        case_id = event.get("case_id")
        if not isinstance(case_id, str) or not case_id:
            errors.append(
                f"line {line_no}: workflow '{workflow}' requires non-empty string case_id"
            )

    expected = event.get("expected", {})
    actual = event.get("actual", {})
    event_type = event.get("event_type")
    if (
        isinstance(expected, dict)
        and isinstance(actual, dict)
        and isinstance(event_type, str)
        and event_type in enforced_event_types
    ):
        for key in required_expected_actual_keys:
            if key not in expected:
                errors.append(
                    f"line {line_no}: expected missing required key '{key}' for workflow '{workflow}'"
                )
            if key not in actual:
                errors.append(
                    f"line {line_no}: actual missing required key '{key}' for workflow '{workflow}'"
                )

    return errors


def validate_stream(
    events: list[dict[str, Any]], schema: dict[str, Any], workflow: str
) -> tuple[list[str], dict[str, Any]]:
    errors: list[str] = []

    workflow_rule = schema["workflow_rules"][workflow]
    seen_event_types: set[str] = set()
    seen_correlation_ids: list[str] = []
    duplicate_correlation_ids: set[str] = set()
    run_ids: set[str] = set()
    case_ids: set[str] = set()
    artifact_event_count = 0
    step_event_type_pairs: dict[str, set[str]] = {}
    case_event_type_pairs: dict[str, set[str]] = {}

    for line_no, event in enumerate(events, start=1):
        if not isinstance(event, dict):
            errors.append(f"line {line_no}: event must be a JSON object")
            continue

        errors.extend(validate_event(event, line_no, schema, workflow))

        run_id = event.get("run_id")
        if isinstance(run_id, str) and run_id:
            run_ids.add(run_id)

        correlation_id = event.get("correlation_id")
        if isinstance(correlation_id, str) and correlation_id:
            if correlation_id in seen_correlation_ids:
                duplicate_correlation_ids.add(correlation_id)
            seen_correlation_ids.append(correlation_id)

        case_id = event.get("case_id")
        if isinstance(case_id, str) and case_id:
            case_ids.add(case_id)

        event_type = event.get("event_type")
        if isinstance(event_type, str):
            seen_event_types.add(event_type)
            if event_type == "artifact":
                artifact_event_count += 1

            step_id = event.get("step_id")
            if isinstance(step_id, str) and step_id:
                step_event_type_pairs.setdefault(step_id, set()).add(event_type)

            if isinstance(case_id, str) and case_id and case_id != "__run__":
                case_event_type_pairs.setdefault(case_id, set()).add(event_type)

    required_events = set(workflow_rule["required_event_types"])
    missing_event_types = sorted(required_events - seen_event_types)
    for event_type in missing_event_types:
        errors.append(
            f"stream missing required event_type '{event_type}' for workflow '{workflow}'"
        )

    if len(run_ids) > 1:
        errors.append(
            f"stream has multiple run_id values (expected one): {sorted(run_ids)}"
        )

    if workflow_rule["require_unique_correlation_ids"] and duplicate_correlation_ids:
        errors.append(
            "stream has duplicate correlation_id values: "
            f"{sorted(duplicate_correlation_ids)}"
        )

    if workflow_rule["require_monotonic_correlation_suffix"] and run_ids:
        run_id = sorted(run_ids)[0]
        sequence_values: list[int] = []
        prefix = f"{run_id}-corr-"
        for correlation_id in seen_correlation_ids:
            if not correlation_id.startswith(prefix):
                errors.append(
                    "correlation_id does not follow run-scoped prefix "
                    f"'{prefix}': {correlation_id}"
                )
                continue
            suffix = correlation_id[len(prefix) :]
            if not suffix.isdigit():
                errors.append(
                    f"correlation_id suffix must be numeric for monotonic check: {correlation_id}"
                )
                continue
            sequence_values.append(int(suffix))

        if sequence_values:
            expected = list(range(1, len(sequence_values) + 1))
            if sequence_values != expected:
                errors.append(
                    "correlation_id sequence is not contiguous starting at 1: "
                    f"observed={sequence_values} expected={expected}"
                )

    required_step_events = set(workflow_rule["required_step_event_pairs"])
    if required_step_events:
        for step_id, step_events in sorted(step_event_type_pairs.items()):
            missing_for_step = sorted(required_step_events - step_events)
            for event_type in missing_for_step:
                errors.append(
                    f"step_id '{step_id}' missing required event_type '{event_type}'"
                )

    required_case_events = set(workflow_rule["required_case_event_pairs"])
    if required_case_events:
        for case_id, case_events in sorted(case_event_type_pairs.items()):
            missing_for_case = sorted(required_case_events - case_events)
            for event_type in missing_for_case:
                errors.append(
                    f"case_id '{case_id}' missing required event_type '{event_type}'"
                )

    min_artifact_events = int(workflow_rule["min_artifact_events"])
    if artifact_event_count < min_artifact_events:
        errors.append(
            f"artifact event count {artifact_event_count} is below required minimum {min_artifact_events}"
        )

    report = {
        "schema_name": schema["schema_name"],
        "schema_version": schema["schema_version"],
        "workflow": workflow,
        "total_events": len(events),
        "unique_run_ids": sorted(run_ids),
        "unique_case_ids": sorted(case_ids),
        "seen_event_types": sorted(seen_event_types),
        "artifact_event_count": artifact_event_count,
        "missing_required_event_types": missing_event_types,
        "errors": errors,
        "status": "passed" if not errors else "failed",
    }
    return errors, report


def load_jsonl(path: Path) -> list[dict[str, Any]]:
    lines = path.read_text(encoding="utf-8").splitlines()
    events: list[dict[str, Any]] = []
    for idx, raw in enumerate(lines, start=1):
        value = raw.strip()
        if not value:
            continue
        try:
            event = json.loads(value)
        except json.JSONDecodeError as exc:
            raise SystemExit(f"[validator] line {idx} is not valid JSON: {exc}") from exc
        events.append(event)
    return events


def write_report(path: Path, report: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()

    schema_path = Path(args.schema)
    input_path = Path(args.input)
    schema_obj = load_json(schema_path)

    if not isinstance(schema_obj, dict):
        raise SystemExit("[validator] schema must be a JSON object")

    schema_errors = validate_schema_contract(schema_obj, args.workflow)
    if schema_errors:
        for error in schema_errors:
            print(f"[validator] {error}", file=sys.stderr)
        return 2

    if not input_path.exists():
        print(f"[validator] input file not found: {input_path}", file=sys.stderr)
        return 2

    events = load_jsonl(input_path)
    errors, report = validate_stream(events, schema_obj, args.workflow)

    if args.report_json:
        write_report(Path(args.report_json), report)

    print(
        json.dumps(
            {
                "status": report["status"],
                "workflow": report["workflow"],
                "total_events": report["total_events"],
                "errors": len(report["errors"]),
                "report_json": args.report_json or None,
            }
        )
    )

    if errors:
        for error in errors:
            print(f"[validator] {error}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

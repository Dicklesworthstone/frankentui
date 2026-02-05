#!/usr/bin/env python3
import argparse
import json
import sys
import textwrap
import unittest
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple

TYPE_MAP = {
    "string": str,
    "number": (int, float),
    "integer": int,
    "boolean": bool,
    "null": type(None),
    "object": dict,
    "array": list,
}


@dataclass
class Schema:
    version: str
    common_required: List[str]
    common_types: Dict[str, Any]
    events: Dict[str, Dict[str, Any]]


class ValidationError(Exception):
    pass


def load_schema(path: str) -> Schema:
    with open(path, "r", encoding="utf-8") as handle:
        data = json.load(handle)
    if not isinstance(data, dict):
        raise ValidationError("schema root must be an object")
    version = data.get("schema_version")
    if not isinstance(version, str):
        raise ValidationError("schema_version must be a string")
    common_required = data.get("common_required")
    if not isinstance(common_required, list):
        raise ValidationError("common_required must be a list")
    common_types = data.get("common_types")
    if not isinstance(common_types, dict):
        raise ValidationError("common_types must be an object")
    events = data.get("events")
    if not isinstance(events, dict):
        raise ValidationError("events must be an object")
    return Schema(
        version=version,
        common_required=[str(item) for item in common_required],
        common_types=common_types,
        events=events,
    )


def type_matches(value: Any, expected: Any) -> bool:
    if isinstance(expected, list):
        return any(type_matches(value, item) for item in expected)
    if isinstance(expected, str):
        py_type = TYPE_MAP.get(expected)
        if py_type is None:
            return False
        if expected == "number" and isinstance(value, bool):
            return False
        if expected == "integer" and isinstance(value, bool):
            return False
        return isinstance(value, py_type)
    return False


def validate_event(schema: Schema, obj: Dict[str, Any]) -> List[str]:
    errors: List[str] = []

    event_type = obj.get("type")
    if not isinstance(event_type, str):
        errors.append("type must be a string")
        return errors

    event_schema = schema.events.get(event_type)
    if event_schema is None:
        errors.append(f"unknown event type: {event_type}")
        return errors

    required = list(schema.common_required)
    required.extend(event_schema.get("required", []))

    for field in required:
        if field not in obj:
            errors.append(f"missing required field: {field}")

    schema_version = obj.get("schema_version")
    if schema_version is not None and schema_version != schema.version:
        errors.append(
            f"schema_version mismatch: expected {schema.version}, got {schema_version}"
        )

    types = dict(schema.common_types)
    types.update(event_schema.get("types", {}))

    for field, expected in types.items():
        if field not in obj:
            continue
        if not type_matches(obj[field], expected):
            errors.append(
                f"field {field} has wrong type: expected {expected}, got {type(obj[field]).__name__}"
            )

    return errors


def validate_jsonl(schema: Schema, lines: Iterable[str]) -> List[Tuple[int, str]]:
    failures: List[Tuple[int, str]] = []
    for idx, line in enumerate(lines, start=1):
        stripped = line.strip()
        if not stripped:
            continue
        try:
            obj = json.loads(stripped)
        except json.JSONDecodeError as exc:
            failures.append((idx, f"invalid json: {exc.msg}"))
            continue
        if not isinstance(obj, dict):
            failures.append((idx, "jsonl line must be an object"))
            continue
        errors = validate_event(schema, obj)
        for err in errors:
            failures.append((idx, err))
    return failures


def example_events(schema_version: str) -> Dict[str, Dict[str, Any]]:
    return {
        "env": {
            "schema_version": schema_version,
            "type": "env",
            "timestamp": "T000001",
            "run_id": "run_123",
            "seed": 0,
            "host": "ci",
            "rustc": "rustc 1.x",
            "cargo": "cargo 1.x",
            "git_commit": "abc123",
            "git_dirty": False,
            "deterministic": True,
            "term": "xterm-256color",
            "colorterm": "truecolor",
            "no_color": "",
        },
        "run_start": {
            "schema_version": schema_version,
            "type": "run_start",
            "timestamp": "T000002",
            "run_id": "run_123",
            "seed": 0,
            "command": "tests/e2e/scripts/run_all.sh",
            "log_dir": "/tmp/ftui_e2e",
            "results_dir": "/tmp/ftui_e2e/results",
        },
        "step_end": {
            "schema_version": schema_version,
            "type": "step_end",
            "timestamp": "T000003",
            "run_id": "run_123",
            "seed": 0,
            "step": "inline",
            "status": "passed",
            "duration_ms": 42,
            "mode": "inline",
            "hash_key": "inline-80x24-seed0",
            "cols": 80,
            "rows": 24,
        },
        "case_step_start": {
            "schema_version": schema_version,
            "type": "case_step_start",
            "timestamp": "T000003",
            "run_id": "run_123",
            "seed": 0,
            "case": "core_navigation",
            "step": "dashboard",
            "action": "inject_keys",
            "details": "screen=2 keys=cemg",
            "mode": "alt",
            "hash_key": "alt-80x24-seed0",
            "cols": 80,
            "rows": 24,
        },
        "case_step_end": {
            "schema_version": schema_version,
            "type": "case_step_end",
            "timestamp": "T000004",
            "run_id": "run_123",
            "seed": 0,
            "case": "core_navigation",
            "step": "dashboard",
            "status": "pass",
            "duration_ms": 1200,
            "action": "inject_keys",
            "details": "screen=2 keys=cemg",
            "mode": "alt",
            "hash_key": "alt-80x24-seed0",
            "cols": 80,
            "rows": 24,
        },
        "case": {
            "schema_version": schema_version,
            "type": "case",
            "timestamp": "T000005",
            "run_id": "run_123",
            "seed": 0,
            "scenario": "bidi",
            "mode": "alt",
            "cols": 80,
            "rows": 24,
            "status": "passed",
            "hash": "deadbeef",
            "duration_ms": 100,
            "error": "",
            "screen": "31",
        },
        "pty_capture": {
            "schema_version": schema_version,
            "type": "pty_capture",
            "timestamp": "T000004",
            "run_id": "run_123",
            "seed": 0,
            "output_file": "/tmp/out.pty",
            "canonical_file": "",
            "output_sha256": "deadbeef",
            "canonical_sha256": "",
            "output_bytes": 100,
            "canonical_bytes": 0,
            "cols": 80,
            "rows": 24,
            "exit_code": 0,
        },
        "artifact": {
            "schema_version": schema_version,
            "type": "artifact",
            "timestamp": "T000004",
            "run_id": "run_123",
            "seed": 0,
            "artifact_type": "log_dir",
            "path": "/tmp/ftui_e2e",
            "status": "present",
            "sha256": "",
            "bytes": 0,
        },
        "large_screen_case": {
            "schema_version": schema_version,
            "type": "large_screen_case",
            "timestamp": "T000005",
            "run_id": "run_123",
            "seed": 0,
            "case": "large_inline",
            "status": "passed",
            "screen_mode": "inline",
            "cols": 200,
            "rows": 50,
            "ui_height": 12,
            "diff_bayesian": True,
            "bocpd": True,
            "conformal": True,
            "evidence_jsonl": "/tmp/evidence.jsonl",
            "pty_output": "/tmp/large.pty",
            "caps_file": "/tmp/caps.txt",
            "duration_ms": 1234,
        },
    }


def print_examples(schema_version: str) -> None:
    for event in example_events(schema_version).values():
        print(json.dumps(event, separators=(",", ":")))


def run_self_tests(schema_path: str) -> int:
    schema = load_schema(schema_path)

    class ValidatorTests(unittest.TestCase):
        def setUp(self) -> None:
            self.schema = schema

        def test_valid_examples(self) -> None:
            examples = example_events(schema.version)
            failures = validate_jsonl(self.schema, [json.dumps(v) for v in examples.values()])
            self.assertEqual(failures, [])

        def test_missing_required(self) -> None:
            bad = example_events(schema.version)["env"].copy()
            bad.pop("run_id")
            failures = validate_jsonl(self.schema, [json.dumps(bad)])
            self.assertTrue(any("missing required field" in err for _, err in failures))

        def test_wrong_type(self) -> None:
            bad = example_events(schema.version)["env"].copy()
            bad["seed"] = "oops"
            failures = validate_jsonl(self.schema, [json.dumps(bad)])
            self.assertTrue(any("wrong type" in err for _, err in failures))

        def test_unknown_type(self) -> None:
            bad = example_events(schema.version)["env"].copy()
            bad["type"] = "unknown"
            failures = validate_jsonl(self.schema, [json.dumps(bad)])
            self.assertTrue(any("unknown event type" in err for _, err in failures))

        def test_malformed_json(self) -> None:
            failures = validate_jsonl(self.schema, ["{not_json}"])
            self.assertTrue(any("invalid json" in err for _, err in failures))

    suite = unittest.defaultTestLoader.loadTestsFromTestCase(ValidatorTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Validate FrankenTUI E2E JSONL logs against schema."
    )
    parser.add_argument(
        "jsonl",
        nargs="?",
        help="Path to JSONL file to validate",
    )
    default_schema = Path(__file__).with_name("e2e_jsonl_schema.json")
    parser.add_argument(
        "--schema",
        default=str(default_schema),
        help="Path to JSON schema file",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Exit non-zero on validation errors",
    )
    parser.add_argument(
        "--warn",
        action="store_true",
        help="Warn only (default if --strict not set)",
    )
    parser.add_argument(
        "--print-examples",
        action="store_true",
        help="Print example JSONL lines and exit",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run validator self-tests and exit",
    )

    args = parser.parse_args()
    schema_path = args.schema

    if args.self_test:
        return run_self_tests(schema_path)

    schema = load_schema(schema_path)

    if args.print_examples:
        print_examples(schema.version)
        return 0

    if not args.jsonl:
        parser.error("jsonl file path is required")

    with open(args.jsonl, "r", encoding="utf-8") as handle:
        failures = validate_jsonl(schema, handle)

    if failures:
        summary = [f"line {line}: {err}" for line, err in failures]
        message = "\n".join(summary)
        sys.stderr.write("JSONL schema validation failed:\n")
        sys.stderr.write(message + "\n")
        return 1 if args.strict else 0

    return 0


if __name__ == "__main__":
    sys.exit(main())

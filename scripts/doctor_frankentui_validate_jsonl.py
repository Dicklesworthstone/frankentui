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
        description="Validate doctor_frankentui e2e JSONL telemetry or full verification bundle",
    )
    parser.add_argument("--input", default="", help="Path to JSONL event stream")
    parser.add_argument(
        "--bundle-root",
        default="",
        help="Path to a doctor_frankentui verification bundle root (e.g. /tmp/doctor_frankentui_ci)",
    )
    parser.add_argument(
        "--write-artifact-map",
        action="store_true",
        help="Generate <bundle-root>/artifact_map.txt before validating the bundle",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run built-in validator unit tests",
    )
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


CANONICAL_BUNDLE_ARTIFACTS: tuple[str, ...] = (
    "happy/meta/summary.json",
    "happy/meta/artifact_manifest.json",
    "happy/meta/events.jsonl",
    "happy/meta/events_validation_report.json",
    "failure/meta/summary.json",
    "failure/meta/case_results.json",
    "failure/meta/replay_triage_report.json",
    "failure/meta/events.jsonl",
    "failure/meta/events_validation_report.json",
    "determinism/meta/determinism_report.json",
    "determinism/meta/determinism_report.txt",
    "coverage/coverage_gate_report.json",
    "coverage/coverage_gate_report.txt",
)

AGENTS_REQUIRED_ARTIFACT_MAP_ENTRIES: tuple[str, ...] = (
    "happy/meta/summary.json",
    "happy/meta/artifact_manifest.json",
    "failure/meta/summary.json",
    "failure/meta/case_results.json",
    "failure/meta/replay_triage_report.json",
    "determinism/meta/determinism_report.json",
    "determinism/meta/determinism_report.txt",
    "coverage/coverage_gate_report.json",
    "coverage/coverage_gate_report.txt",
)


def write_bundle_artifact_map(bundle_root: Path) -> Path:
    bundle_root.mkdir(parents=True, exist_ok=True)
    map_path = bundle_root / "artifact_map.txt"
    lines = ["# doctor_frankentui canonical verification artifact map"]
    for rel in CANONICAL_BUNDLE_ARTIFACTS:
        target = bundle_root / rel
        lines.append(f"{rel}\t{target}")
    map_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return map_path


def validate_bundle(
    bundle_root: Path,
    schema_obj: dict[str, Any],
    write_map: bool = False,
) -> tuple[list[str], dict[str, Any]]:
    errors: list[str] = []
    if write_map:
        write_bundle_artifact_map(bundle_root)

    map_path = bundle_root / "artifact_map.txt"
    if not map_path.is_file() or map_path.stat().st_size == 0:
        errors.append(f"missing or empty artifact_map.txt at {map_path}")
    else:
        map_text = map_path.read_text(encoding="utf-8")
        for req_entry in AGENTS_REQUIRED_ARTIFACT_MAP_ENTRIES:
            if req_entry not in map_text:
                errors.append(
                    f"artifact_map.txt missing required entry: {req_entry}"
                )

    checked_files: list[str] = []
    for rel in CANONICAL_BUNDLE_ARTIFACTS:
        path = bundle_root / rel
        if not path.is_file():
            errors.append(f"missing required bundle artifact: {rel} ({path})")
        elif path.stat().st_size == 0:
            errors.append(f"empty required bundle artifact: {rel} ({path})")
        else:
            checked_files.append(rel)

    json_status_checks = (
        ("happy/meta/summary.json", {"passed", "ok"}),
        ("happy/meta/events_validation_report.json", {"passed"}),
        ("failure/meta/summary.json", {"passed", "ok"}),
        ("failure/meta/events_validation_report.json", {"passed"}),
        ("determinism/meta/determinism_report.json", {"passed", "ok"}),
        ("coverage/coverage_gate_report.json", {"passed"}),
    )
    loaded_json: dict[str, Any] = {}
    for rel, allowed_statuses in json_status_checks:
        path = bundle_root / rel
        if not path.is_file():
            continue
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            errors.append(f"{rel}: invalid JSON: {exc}")
            continue
        loaded_json[rel] = payload
        status = payload.get("status") if isinstance(payload, dict) else None
        if status not in allowed_statuses:
            errors.append(
                f"{rel}: status={status!r} not in allowed {sorted(allowed_statuses)}"
            )

    happy_summary = loaded_json.get("happy/meta/summary.json")
    if isinstance(happy_summary, dict):
        steps = happy_summary.get("steps")
        if isinstance(steps, list) and steps:
            steps_total = len(steps)
            steps_passed = sum(1 for s in steps if isinstance(s, dict) and s.get("exit_code") == 0)
            steps_failed = steps_total - steps_passed
        else:
            steps_total = happy_summary.get("steps_total", happy_summary.get("runs"))
            steps_passed = happy_summary.get("steps_passed", happy_summary.get("passed"))
            steps_failed = happy_summary.get("steps_failed", happy_summary.get("failed"))
        if not isinstance(steps_total, int) or steps_total <= 0:
            errors.append("happy/meta/summary.json: missing positive steps/runs count")
        if not isinstance(steps_passed, int) or steps_passed <= 0:
            errors.append("happy/meta/summary.json: missing positive steps_passed/passed count")
        if not isinstance(steps_failed, int) or steps_failed != 0:
            errors.append(f"happy/meta/summary.json: expected 0 failed steps, got {steps_failed!r}")

    case_results_path = bundle_root / "failure/meta/case_results.json"
    if case_results_path.is_file():
        try:
            raw_case_results = json.loads(case_results_path.read_text(encoding="utf-8"))
            case_results = (
                raw_case_results.get("cases")
                if isinstance(raw_case_results, dict)
                else raw_case_results
            )
            if not isinstance(case_results, list) or not case_results:
                errors.append("failure/meta/case_results.json: must contain a non-empty cases array")
            else:
                for idx, entry in enumerate(case_results):
                    if not isinstance(entry, dict):
                        errors.append(f"failure/meta/case_results.json[{idx}]: must be an object")
                        continue
                    has_case = isinstance(entry.get("case_id") or entry.get("case"), str)
                    has_expected = isinstance(entry.get("expected_exit"), int)
                    has_actual = isinstance(entry.get("actual_exit"), int)
                    if not (has_case and has_expected and has_actual):
                        errors.append(
                            f"failure/meta/case_results.json[{idx}]: requires case_id/case, expected_exit, actual_exit"
                        )
        except json.JSONDecodeError as exc:
            errors.append(f"failure/meta/case_results.json: invalid JSON: {exc}")

    det_report = loaded_json.get("determinism/meta/determinism_report.json")
    if isinstance(det_report, dict):
        divergent = det_report.get("divergent")
        divergences = det_report.get("divergences")
        if divergent is True or (isinstance(divergences, list) and len(divergences) > 0):
            errors.append("determinism/meta/determinism_report.json: reported divergence")

    cov_report = loaded_json.get("coverage/coverage_gate_report.json")
    if isinstance(cov_report, dict):
        scopes = cov_report.get("scopes")
        if isinstance(scopes, dict) and scopes:
            for scope_name, scope_data in scopes.items():
                if not isinstance(scope_data, dict):
                    continue
                measured = scope_data.get("measured", {})
                required = scope_data.get("required", {})
                if isinstance(measured, dict) and isinstance(required, dict):
                    m_lines = measured.get("lines")
                    r_lines = required.get("lines")
                    if isinstance(m_lines, (int, float)) and isinstance(r_lines, (int, float)):
                        if m_lines < r_lines:
                            errors.append(
                                f"coverage/coverage_gate_report.json: scope {scope_name} lines {m_lines} < {r_lines}"
                            )

    for rel, wf in (
        ("happy/meta/events.jsonl", "happy"),
        ("failure/meta/events.jsonl", "failure"),
    ):
        path = bundle_root / rel
        if not path.is_file():
            continue
        events = load_jsonl(path)
        wf_errors, _ = validate_stream(events, schema_obj, wf)
        for err in wf_errors:
            errors.append(f"{rel} ({wf}): {err}")

    cov_txt = bundle_root / "coverage/coverage_gate_report.txt"
    if cov_txt.is_file():
        first_line = (cov_txt.read_text(encoding="utf-8").splitlines() or [""])[0].strip()
        if first_line != "status=passed":
            errors.append(
                f"coverage/coverage_gate_report.txt: expected first line 'status=passed', got {first_line!r}"
            )

    report = {
        "schema_name": schema_obj.get("schema_name"),
        "schema_version": schema_obj.get("schema_version"),
        "mode": "bundle",
        "bundle_root": str(bundle_root),
        "checked_files": checked_files,
        "errors": errors,
        "status": "passed" if not errors else "failed",
    }
    return errors, report


def run_self_test(schema_path: Path) -> int:
    schema_obj = load_json(schema_path)
    assert isinstance(schema_obj, dict), "schema must be dict"
    assert not validate_schema_contract(schema_obj, "generic")

    def make_event(
        seq: int,
        event_type: str,
        step_id: str | None = "s1",
        case_id: str | None = None,
    ) -> dict[str, Any]:
        return {
            "schema_version": "1.0.0",
            "timestamp_utc": "2026-10-19T00:00:00Z",
            "run_id": "run_test",
            "correlation_id": f"run_test-corr-{seq:04d}",
            "case_id": case_id,
            "step_id": step_id,
            "event_type": event_type,
            "command": "doctor_frankentui",
            "env_hash": "a" * 64,
            "duration_ms": 10,
            "exit_code": 0,
            "stdout_sha256": "b" * 64,
            "stderr_sha256": "c" * 64,
            "artifact_hashes": {"summary": "d" * 64},
            "expected": {"exit_code": 0, "regex_match": True, "json_valid": True},
            "actual": {"exit_code": 0, "regex_match": True, "json_valid": True},
        }

    happy_events = [
        make_event(1, "run_start", None),
        make_event(2, "step_start", "step_1"),
        make_event(3, "step_end", "step_1"),
        make_event(4, "artifact", None),
        make_event(5, "run_end", None),
    ]
    errs, _ = validate_stream(happy_events, schema_obj, "happy")
    assert not errs, f"expected happy stream pass, got: {errs}"

    failure_events = [
        make_event(1, "run_start", None, case_id="c1"),
        make_event(2, "case_start", None, case_id="c1"),
        make_event(3, "case_end", None, case_id="c1"),
        make_event(4, "artifact", None, case_id="c1"),
        make_event(5, "run_end", None, case_id="c1"),
    ]
    errs_f, _ = validate_stream(failure_events, schema_obj, "failure")
    assert not errs_f, f"expected failure stream pass, got: {errs_f}"

    repo_root = Path(__file__).resolve().parents[1]
    root = repo_root / "target/doctor_frankentui_ci/validator_self_test"
    root.mkdir(parents=True, exist_ok=True)
    (root / "happy/meta").mkdir(parents=True, exist_ok=True)
    (root / "failure/meta").mkdir(parents=True, exist_ok=True)
    (root / "determinism/meta").mkdir(parents=True, exist_ok=True)
    cov_dir = root / "coverage"
    cov_dir.mkdir(parents=True, exist_ok=True)

    (root / "happy/meta/summary.json").write_text(
        json.dumps({"status": "passed", "steps_total": 8, "steps_passed": 8, "steps_failed": 0}) + "\n",
        encoding="utf-8",
    )
    (root / "happy/meta/artifact_manifest.json").write_text(
        json.dumps({"status": "passed"}) + "\n", encoding="utf-8"
    )
    (root / "happy/meta/events_validation_report.json").write_text(
        json.dumps({"status": "passed"}) + "\n", encoding="utf-8"
    )
    (root / "failure/meta/summary.json").write_text(
        json.dumps({"status": "passed", "total_cases": 12, "passed_cases": 12, "failed_cases": 0}) + "\n",
        encoding="utf-8",
    )
    (root / "failure/meta/case_results.json").write_text(
        json.dumps([{"case_id": "c1", "expected_exit": 2, "actual_exit": 2}]) + "\n",
        encoding="utf-8",
    )
    (root / "failure/meta/replay_triage_report.json").write_text(
        json.dumps({"status": "passed"}) + "\n", encoding="utf-8"
    )
    (root / "failure/meta/events_validation_report.json").write_text(
        json.dumps({"status": "passed"}) + "\n", encoding="utf-8"
    )
    (root / "determinism/meta/determinism_report.json").write_text(
        json.dumps({"status": "passed", "divergences": []}) + "\n", encoding="utf-8"
    )
    (root / "determinism/meta/determinism_report.txt").write_text(
        "status=passed\n", encoding="utf-8"
    )
    (cov_dir / "coverage_gate_report.json").write_text(
        json.dumps({
            "status": "passed",
            "scopes": {
                "total": {
                    "measured": {"lines": 91.5, "branches": 71.0, "functions": 92.6},
                    "required": {"lines": 89.0, "branches": 69.0, "functions": 86.0},
                }
            },
        }) + "\n",
        encoding="utf-8",
    )
    (cov_dir / "coverage_gate_report.txt").write_text(
        "status=passed\n", encoding="utf-8"
    )
    (root / "happy/meta/events.jsonl").write_text(
        "\n".join(json.dumps(e) for e in happy_events) + "\n", encoding="utf-8"
    )
    (root / "failure/meta/events.jsonl").write_text(
        "\n".join(json.dumps(e) for e in failure_events) + "\n", encoding="utf-8"
    )

    b_errs, b_report = validate_bundle(root, schema_obj, write_map=True)
    assert not b_errs, f"expected bundle pass, got: {b_errs}"
    assert b_report["status"] == "passed"

    (cov_dir / "coverage_gate_report.txt").write_text(
        "status=failed\n", encoding="utf-8"
    )
    b_errs_fail, _ = validate_bundle(root, schema_obj, write_map=False)
    assert b_errs_fail, "expected bundle failure when coverage status=failed"
    (cov_dir / "coverage_gate_report.txt").write_text(
        "status=passed\n", encoding="utf-8"
    )

    print(json.dumps({"status": "passed", "mode": "self_test"}))
    return 0


def main() -> int:
    args = parse_args()

    schema_path = Path(args.schema)
    if args.self_test:
        return run_self_test(schema_path)

    schema_obj = load_json(schema_path)

    if not isinstance(schema_obj, dict):
        raise SystemExit("[validator] schema must be a JSON object")

    if args.bundle_root:
        bundle_root = Path(args.bundle_root)
        errors, report = validate_bundle(
            bundle_root, schema_obj, write_map=args.write_artifact_map
        )
        if args.report_json:
            write_report(Path(args.report_json), report)
        print(
            json.dumps(
                {
                    "status": report["status"],
                    "mode": "bundle",
                    "bundle_root": str(bundle_root),
                    "checked_files": len(report["checked_files"]),
                    "errors": len(errors),
                    "report_json": args.report_json or None,
                }
            )
        )
        if errors:
            for error in errors:
                print(f"[validator] {error}", file=sys.stderr)
            return 1
        return 0

    if not args.input:
        print("[validator] either --input, --bundle-root, or --self-test is required", file=sys.stderr)
        return 2

    input_path = Path(args.input)

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

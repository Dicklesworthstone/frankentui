#!/usr/bin/env python3
import argparse
import json
import sys
import tempfile
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


REGISTRY_VERSION = "e2e-hash-registry-v1"
_PASS_STATUSES = {"pass", "passed", "success"}


@dataclass
class Schema:
    version: str
    common_required: List[str]
    common_types: Dict[str, Any]
    events: Dict[str, Dict[str, Any]]
    # Field that names the event: "type" for the E2E script logs,
    # "event" for the runtime evidence sink rows.
    key_field: str = "type"


class ValidationError(Exception):
    pass


@dataclass(frozen=True)
class HashRegistryEntry:
    event_type: str
    hash_key: str
    field: str
    value: str
    case: Optional[str] = None
    step: Optional[str] = None
    note: Optional[str] = None


@dataclass
class HashRegistry:
    version: str
    entries: List[HashRegistryEntry]


REGISTRY_FIELDS: Dict[str, List[str]] = {
    "span_diff_case": ["diff_hash"],
    "tile_skip_case": ["diff_hash"],
    "selector_case": ["decision_hash"],
    "budgeted_refresh_case": ["widget_refresh_hash"],
}


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
    key_field = data.get("key_field", "type")
    if not isinstance(key_field, str) or not key_field:
        raise ValidationError("key_field must be a non-empty string")
    return Schema(
        version=version,
        common_required=[str(item) for item in common_required],
        common_types=common_types,
        events=events,
        key_field=key_field,
    )


def load_registry(path: str) -> HashRegistry:
    with open(path, "r", encoding="utf-8") as handle:
        data = json.load(handle)
    if not isinstance(data, dict):
        raise ValidationError("registry root must be an object")
    version = data.get("registry_version")
    if not isinstance(version, str):
        raise ValidationError("registry_version must be a string")
    if version != REGISTRY_VERSION:
        raise ValidationError(
            f"registry_version must be {REGISTRY_VERSION}, got {version}"
        )
    entries_raw = data.get("entries")
    if not isinstance(entries_raw, list):
        raise ValidationError("entries must be a list")
    entries: List[HashRegistryEntry] = []
    for idx, item in enumerate(entries_raw, start=1):
        if not isinstance(item, dict):
            raise ValidationError(f"registry entry {idx} must be an object")
        event_type = item.get("event_type")
        hash_key = item.get("hash_key")
        field = item.get("field")
        value = item.get("value")
        if not isinstance(event_type, str) or not event_type:
            raise ValidationError(f"registry entry {idx} missing event_type")
        if not isinstance(hash_key, str) or not hash_key:
            raise ValidationError(f"registry entry {idx} missing hash_key")
        if not isinstance(field, str) or not field:
            raise ValidationError(f"registry entry {idx} missing field")
        if not isinstance(value, str):
            raise ValidationError(f"registry entry {idx} value must be a string")
        case = item.get("case")
        if case is not None and not isinstance(case, str):
            raise ValidationError(f"registry entry {idx} case must be a string")
        step = item.get("step")
        if step is not None and not isinstance(step, str):
            raise ValidationError(f"registry entry {idx} step must be a string")
        note = item.get("note")
        if note is not None and not isinstance(note, str):
            raise ValidationError(f"registry entry {idx} note must be a string")
        entries.append(
            HashRegistryEntry(
                event_type=event_type,
                hash_key=hash_key,
                field=field,
                value=value,
                case=case,
                step=step,
                note=note,
            )
        )
    return HashRegistry(version=version, entries=entries)


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


def is_non_negative_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def validate_frame_semantics(obj: Dict[str, Any]) -> List[str]:
    errors: List[str] = []
    integer_fields = (
        "hovered_link_id",
        "cursor_offset",
        "cursor_style",
        "selection_start",
        "selection_end",
    )
    for field in integer_fields:
        if field in obj and not is_non_negative_int(obj[field]):
            errors.append(f"field {field} must be a non-negative integer")

    has_selection_start = "selection_start" in obj
    has_selection_end = "selection_end" in obj
    if has_selection_start != has_selection_end:
        errors.append("selection_start and selection_end must be provided together")
    if has_selection_start and has_selection_end:
        start = obj["selection_start"]
        end = obj["selection_end"]
        if is_non_negative_int(start) and is_non_negative_int(end) and start > end:
            errors.append("selection_start must be <= selection_end")

    if obj.get("selection_active") is True and not (has_selection_start and has_selection_end):
        errors.append("selection_active=true requires selection_start and selection_end")

    return errors


def validate_event(schema: Schema, obj: Dict[str, Any]) -> List[str]:
    errors: List[str] = []

    event_type = obj.get(schema.key_field)
    if not isinstance(event_type, str):
        errors.append(f"{schema.key_field} must be a string")
        return errors

    event_schema = schema.events.get(event_type)
    if event_schema is None:
        label = "event type" if schema.key_field == "type" else "event"
        errors.append(f"unknown {label}: {event_type}")
        return errors

    required = list(schema.common_required)
    required.extend(event_schema.get("required", []))

    for field in required:
        if field not in obj:
            errors.append(f"missing required field: {field}")

    schema_version = obj.get("schema_version")
    # An event may carry its own versioned sub-schema (e.g. the BOCPD row's
    # "bocpd-v1"); the event entry names it, otherwise the file version applies.
    expected_version = event_schema.get("schema_version", schema.version)
    if schema_version is not None and schema_version != expected_version:
        errors.append(
            f"schema_version mismatch: expected {expected_version}, got {schema_version}"
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

    if event_type == "frame":
        errors.extend(validate_frame_semantics(obj))

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


def _seed_to_string(seed: Any) -> Optional[str]:
    if isinstance(seed, bool):
        return None
    if isinstance(seed, int):
        return str(seed)
    if isinstance(seed, float):
        if seed.is_integer():
            return str(int(seed))
        return str(seed)
    if isinstance(seed, str) and seed:
        return seed
    return None


def compute_hash_key(obj: Dict[str, Any]) -> Optional[str]:
    hash_key = obj.get("hash_key")
    if isinstance(hash_key, str) and hash_key:
        return hash_key
    mode = obj.get("mode")
    if mode is None:
        mode = obj.get("screen_mode")
    cols = obj.get("cols")
    rows = obj.get("rows")
    seed = obj.get("seed")
    if not isinstance(mode, str):
        return None
    if isinstance(cols, bool) or not isinstance(cols, int):
        return None
    if isinstance(rows, bool) or not isinstance(rows, int):
        return None
    seed_str = _seed_to_string(seed)
    if seed_str is None:
        return None
    return f"{mode}-{cols}x{rows}-seed{seed_str}"


def is_pass_status(status: Any) -> bool:
    if not isinstance(status, str):
        return True
    return status.lower() in _PASS_STATUSES


def validate_hash_registry(
    registry: HashRegistry, lines: Iterable[str]
) -> List[Tuple[int, str]]:
    failures: List[Tuple[int, str]] = []
    index: Dict[Tuple[str, str], List[HashRegistryEntry]] = {}
    for entry in registry.entries:
        index.setdefault((entry.event_type, entry.hash_key), []).append(entry)

    for idx, line in enumerate(lines, start=1):
        stripped = line.strip()
        if not stripped:
            continue
        try:
            obj = json.loads(stripped)
        except json.JSONDecodeError:
            continue
        if not isinstance(obj, dict):
            continue
        event_type = obj.get("type")
        if not isinstance(event_type, str):
            continue
        if not is_pass_status(obj.get("status")):
            continue
        hash_key = compute_hash_key(obj)
        if not hash_key:
            continue
        entries = index.get((event_type, hash_key))
        if not entries:
            continue
        event_case = obj.get("case")
        event_step = obj.get("step")
        event_screen = obj.get("screen")
        event_mode = obj.get("mode")
        if event_mode is None:
            event_mode = obj.get("screen_mode")
        event_cols = obj.get("cols")
        event_rows = obj.get("rows")
        event_seed = obj.get("seed")
        for entry in entries:
            if entry.case is not None and entry.case != event_case:
                continue
            if entry.step is not None and entry.step != event_step:
                continue
            if entry.field not in obj:
                failures.append(
                    (
                        idx,
                        "missing hash field "
                        f"{entry.field} for {event_type} {hash_key} "
                        f"mode={event_mode} cols={event_cols} rows={event_rows} seed={event_seed} "
                        f"case={event_case} step={event_step} screen={event_screen}",
                    )
                )
                continue
            actual = obj[entry.field]
            if not isinstance(actual, str):
                failures.append(
                    (
                        idx,
                        "hash field "
                        f"{entry.field} for {event_type} {hash_key} "
                        f"mode={event_mode} cols={event_cols} rows={event_rows} seed={event_seed} "
                        f"case={event_case} step={event_step} screen={event_screen} is not a string",
                    )
                )
                continue
            if actual != entry.value:
                failures.append(
                    (
                        idx,
                        "hash mismatch "
                        f"{event_type} {hash_key} "
                        f"mode={event_mode} cols={event_cols} rows={event_rows} seed={event_seed} "
                        f"case={event_case} step={event_step} screen={event_screen} "
                        f"field={entry.field} expected={entry.value} got={actual}",
                    )
                )
    return failures


def extract_registry_entries(lines: Iterable[str]) -> List[HashRegistryEntry]:
    entries: Dict[Tuple[str, str, str, Optional[str], Optional[str]], HashRegistryEntry] = {}
    for idx, line in enumerate(lines, start=1):
        stripped = line.strip()
        if not stripped:
            continue
        try:
            obj = json.loads(stripped)
        except json.JSONDecodeError:
            continue
        if not isinstance(obj, dict):
            continue
        event_type = obj.get("type")
        if not isinstance(event_type, str):
            continue
        fields = REGISTRY_FIELDS.get(event_type)
        if not fields:
            continue
        status = obj.get("status")
        if not is_pass_status(status):
            continue
        hash_key = compute_hash_key(obj)
        if not hash_key:
            continue
        event_case = obj.get("case")
        if event_case is not None and not isinstance(event_case, str):
            event_case = None
        event_step = obj.get("step")
        if event_step is not None and not isinstance(event_step, str):
            event_step = None
        for field in fields:
            value = obj.get(field)
            if not isinstance(value, str) or not value:
                continue
            key = (event_type, hash_key, field, event_case, event_step)
            existing = entries.get(key)
            if existing and existing.value != value:
                raise ValidationError(
                    f"conflicting registry values for {event_type} {hash_key} "
                    f"case={event_case} step={event_step} field={field}: "
                    f"{existing.value} vs {value} (line {idx})"
                )
            entries[key] = HashRegistryEntry(
                event_type=event_type,
                hash_key=hash_key,
                field=field,
                value=value,
                case=event_case,
                step=event_step,
            )
    return sorted(
        entries.values(),
        key=lambda item: (
            item.event_type,
            item.hash_key,
            item.case or "",
            item.step or "",
            item.field,
        ),
    )


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
        "browser_env": {
            "schema_version": schema_version,
            "type": "browser_env",
            "timestamp": "T000001",
            "run_id": "run_123",
            "seed": 0,
            "browser": "chromium",
            "browser_version": "123.0.0.0",
            "user_agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36",
            "dpr": 2.0,
            "platform": "Linux x86_64",
            "locale": "en-US",
            "timezone": "UTC",
            "headless": True,
            "viewport_css_px": {"width": 1200, "height": 800},
            "viewport_px": {"width": 2400, "height": 1600},
            "zoom": 1.0,
        },
        "gpu_adapter": {
            "schema_version": schema_version,
            "type": "gpu_adapter",
            "timestamp": "T000001",
            "run_id": "run_123",
            "seed": 0,
            "api": "webgpu",
            "adapter_name": "MockAdapter",
            "backend": "wgpu",
            "vendor": "0x1234",
            "device": "0x5678",
            "description": "Mock GPU adapter for tests",
            "features": ["timestamp-query"],
            "limits": {"maxTextureDimension2D": 8192},
            "is_fallback_adapter": False,
        },
        "ws_metrics": {
            "schema_version": schema_version,
            "type": "ws_metrics",
            "timestamp": "T000001",
            "run_id": "run_123",
            "seed": 0,
            "label": "bridge",
            "ws_url": "ws://127.0.0.1:12345/ws",
            "bytes_tx": 1234,
            "bytes_rx": 5678,
            "messages_tx": 12,
            "messages_rx": 34,
            "connect_ms": 10,
            "reconnects": 0,
            "close_code": None,
            "close_reason": "",
            "dropped_messages": 0,
            "rtt_histogram_ms": {"buckets": [1, 2, 5], "counts": [10, 2, 1]},
            "latency_histogram_ms": {"buckets": [1, 2, 5], "counts": [8, 3, 1]},
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
        "input": {
            "schema_version": schema_version,
            "type": "input",
            "timestamp": "T000002",
            "run_id": "run_123",
            "seed": 0,
            "input_type": "keys",
            "encoding": "utf8",
            "bytes_b64": "Y2VtZw==",
            "input_hash": "deadbeef",
            "details": "screen=2 keys=cemg",
        },
        "frame": {
            "schema_version": schema_version,
            "type": "frame",
            "timestamp": "T000002",
            "run_id": "run_123",
            "seed": 0,
            "frame_idx": 1,
            "ts_ms": 16,
            "mode": "alt",
            "hash_key": "alt-80x24-seed0",
            "cols": 80,
            "rows": 24,
            "hash_algo": "sha256",
            "frame_hash": "deadbeef",
            "patch_hash": "feedface",
            "patch_bytes": 2048,
            "patch_cells": 64,
            "patch_runs": 7,
            "render_ms": 3.1,
            "present_ms": 0.8,
            "present_bytes": 65536,
            "checksum_chain": "00ff00ff",
            "interaction_hash": "fnv1a64:1234abcd5678ef90",
            "hovered_link_id": 7,
            "cursor_offset": 42,
            "cursor_style": 2,
            "selection_active": True,
            "selection_start": 40,
            "selection_end": 64,
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
        "error": {
            "schema_version": schema_version,
            "type": "error",
            "timestamp": "T000003",
            "run_id": "run_123",
            "seed": 0,
            "message": "example failure",
            "exit_code": 1,
            "stack": "",
            "details": "case=core_navigation step=dashboard",
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

        def test_wrong_type_optional_frame_interaction_field(self) -> None:
            bad = example_events(schema.version)["frame"].copy()
            bad["selection_active"] = "oops"
            failures = validate_jsonl(self.schema, [json.dumps(bad)])
            self.assertTrue(any("wrong type" in err for _, err in failures))

        def test_frame_selection_active_requires_paired_bounds(self) -> None:
            bad = example_events(schema.version)["frame"].copy()
            bad.pop("selection_start")
            bad.pop("selection_end")
            failures = validate_jsonl(self.schema, [json.dumps(bad)])
            self.assertTrue(
                any("selection_active=true requires selection_start and selection_end" in err for _, err in failures)
            )

        def test_frame_selection_bounds_must_be_ordered(self) -> None:
            bad = example_events(schema.version)["frame"].copy()
            bad["selection_start"] = 11
            bad["selection_end"] = 7
            failures = validate_jsonl(self.schema, [json.dumps(bad)])
            self.assertTrue(any("selection_start must be <= selection_end" in err for _, err in failures))

        def test_frame_interaction_offsets_require_non_negative_integers(self) -> None:
            bad = example_events(schema.version)["frame"].copy()
            bad["cursor_offset"] = 4.5
            failures = validate_jsonl(self.schema, [json.dumps(bad)])
            self.assertTrue(any("cursor_offset must be a non-negative integer" in err for _, err in failures))

        def test_unknown_type(self) -> None:
            bad = example_events(schema.version)["env"].copy()
            bad["type"] = "unknown"
            failures = validate_jsonl(self.schema, [json.dumps(bad)])
            self.assertTrue(any("unknown event type" in err for _, err in failures))

        def test_malformed_json(self) -> None:
            failures = validate_jsonl(self.schema, ["{not_json}"])
            self.assertTrue(any("invalid json" in err for _, err in failures))

        def test_hash_registry_match(self) -> None:
            example = example_events(schema.version)["case_step_end"].copy()
            example["diff_hash"] = "abc123"
            registry_payload = {
                "registry_version": REGISTRY_VERSION,
                "entries": [
                    {
                        "event_type": "case_step_end",
                        "hash_key": example["hash_key"],
                        "field": "diff_hash",
                        "value": "abc123",
                        "case": example["case"],
                        "step": example["step"],
                    }
                ],
            }
            with tempfile.TemporaryDirectory() as tmp_dir:
                registry_path = Path(tmp_dir) / "registry.json"
                registry_path.write_text(
                    json.dumps(registry_payload, indent=2), encoding="utf-8"
                )
                registry = load_registry(str(registry_path))
                failures = validate_hash_registry(registry, [json.dumps(example)])
                self.assertFalse(failures)

        def test_hash_registry_mismatch(self) -> None:
            example = example_events(schema.version)["case_step_end"].copy()
            example["diff_hash"] = "abc123"
            registry_payload = {
                "registry_version": REGISTRY_VERSION,
                "entries": [
                    {
                        "event_type": "case_step_end",
                        "hash_key": example["hash_key"],
                        "field": "diff_hash",
                        "value": "zzz999",
                        "case": example["case"],
                        "step": example["step"],
                    }
                ],
            }
            with tempfile.TemporaryDirectory() as tmp_dir:
                registry_path = Path(tmp_dir) / "registry.json"
                registry_path.write_text(
                    json.dumps(registry_payload, indent=2), encoding="utf-8"
                )
                registry = load_registry(str(registry_path))
                failures = validate_hash_registry(registry, [json.dumps(example)])
                self.assertTrue(any("hash mismatch" in err for _, err in failures))

    suite = unittest.defaultTestLoader.loadTestsFromTestCase(ValidatorTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def check_examples(schema: Schema, examples_path: str) -> int:
    """Validate an examples file strictly and require one example per event.

    Used for the evidence schema, whose examples are hand-written rows taken
    from real runs: a producer that adds or renames a field must update the
    example, and a schema event without an example is a schema nobody
    exercised.
    """
    with open(examples_path, "r", encoding="utf-8") as handle:
        lines = list(handle)
    failures = validate_jsonl(schema, lines)
    seen = set()
    for line in lines:
        stripped = line.strip()
        if not stripped:
            continue
        try:
            obj = json.loads(stripped)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict) and isinstance(obj.get(schema.key_field), str):
            seen.add(obj[schema.key_field])
    missing = sorted(set(schema.events) - seen)
    if failures:
        sys.stderr.write("examples failed schema validation:\n")
        for line, err in failures:
            sys.stderr.write(f"line {line}: {err}\n")
    if missing:
        sys.stderr.write(
            "schema events without an example: " + ", ".join(missing) + "\n"
        )
    if failures or missing:
        return 1
    print(f"examples OK: {len(seen)} events covered by {examples_path}")
    return 0


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
        "--registry",
        default="",
        help="Path to hash registry file",
    )
    parser.add_argument(
        "--emit-registry",
        default="",
        help="Write hash registry JSON to path (use '-' for stdout)",
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
    parser.add_argument(
        "--examples",
        default="",
        help=(
            "Validate an examples JSONL file against the schema and require at "
            "least one example per schema event (exit non-zero on any gap)"
        ),
    )

    args = parser.parse_args()
    schema_path = args.schema
    registry_path = args.registry
    emit_registry_path = args.emit_registry

    if args.self_test:
        return run_self_tests(schema_path)

    schema = load_schema(schema_path)

    if args.print_examples:
        print_examples(schema.version)
        return 0

    if args.examples:
        return check_examples(schema, args.examples)

    if not args.jsonl:
        parser.error("jsonl file path is required")

    with open(args.jsonl, "r", encoding="utf-8") as handle:
        lines = list(handle)
    failures = validate_jsonl(schema, lines)

    registry_failures: List[Tuple[int, str]] = []
    if registry_path:
        registry_file = Path(registry_path)
        if registry_file.exists():
            registry = load_registry(str(registry_file))
            registry_failures = validate_hash_registry(registry, lines)
        else:
            registry_failures = [(0, f"registry file not found: {registry_path}")]

    if failures or registry_failures:
        if failures:
            summary = [f"line {line}: {err}" for line, err in failures]
            message = "\n".join(summary)
            sys.stderr.write("JSONL schema validation failed:\n")
            sys.stderr.write(message + "\n")
        if registry_failures:
            summary = [f"line {line}: {err}" for line, err in registry_failures]
            message = "\n".join(summary)
            sys.stderr.write("JSONL hash registry validation failed:\n")
            sys.stderr.write(message + "\n")
        return 1 if args.strict else 0

    if emit_registry_path:
        entries = extract_registry_entries(lines)
        payload = {
            "registry_version": REGISTRY_VERSION,
            "entries": [
                {
                    "event_type": entry.event_type,
                    "hash_key": entry.hash_key,
                    "field": entry.field,
                    "value": entry.value,
                    "case": entry.case,
                    "step": entry.step,
                    "note": entry.note,
                }
                for entry in entries
            ],
        }
        output = json.dumps(payload, indent=2)
        if emit_registry_path == "-":
            sys.stdout.write(output + "\n")
        else:
            Path(emit_registry_path).write_text(output + "\n", encoding="utf-8")

    return 0


if __name__ == "__main__":
    sys.exit(main())

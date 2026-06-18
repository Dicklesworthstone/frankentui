#!/usr/bin/env python3
"""Symbolized pane replay artifact contract (bd-1pvzq.1).

This tool turns the loose outputs of the pane profiling run (``pane_profile.sh``)
into a single, checksummed, schema-versioned bundle index and validates that
contract so the symbolized-replay path cannot silently degrade.

A *symbolized pane replay artifact* couples two things that are useless apart:

  * **Replay evidence** -- the long-lived pane-core profiling harness manifest
    (``pane_core_profile_harness/manifest.json``) carrying the deterministic
    baseline/final/aggregate state hashes, checkpoint/replay diagnostics, and
    allocation/retention diagnostics, plus the baseline/final tree snapshots and
    the verbose run log. Together these let any later session deterministically
    *replay* the exact pane operation history and verify the result.

  * **Symbolization manifest** -- per executed bench binary: the executed path,
    binary provenance (local / remote-fetched / preserved), GNU build-id,
    debug-info presence, stripped status, addr2line readiness, and a content
    hash. These let a profile of that *exact* binary be symbolized back to
    source, offline if necessary.

The index (``replay_artifact_index.json``) ties both halves together with
SHA-256 checksums over every referenced file, so a downstream gate can trust
that the replay evidence and the symbolization provenance describe one coherent
run.

Subcommands
-----------
``emit``
    Build ``replay_artifact_index.json`` from an out-dir produced by
    ``pane_profile.sh``. Fails if the replay manifest is missing.

``validate``
    Re-derive every checksum and check structural + replay completeness. With
    ``--require-symbolization`` it additionally fails unless every executed
    binary is symbolization-ready (build-id + debug-info + addr2line + not
    stripped) -- the strict mode CI uses where the binary is always local.

``selftest``
    Build a synthetic bundle in a temp dir, emit, validate (lenient + strict),
    and assert that a tampered checksum is rejected. No cargo build required.

Determinism: the index lists artifacts by path (sorted) with content hashes and
omits volatile fields like mtimes, so the replay/symbolization *evidence* is
stable for identical inputs even though absolute executed paths are provenance.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import tempfile
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

SCHEMA = "ftui.pane.replay_artifact_index"
SCHEMA_VERSION = 1
BEAD = "bd-1pvzq.1"

INDEX_FILENAME = "replay_artifact_index.json"
HARNESS_SUBDIR = "pane_core_profile_harness"
SYMBOL_METADATA_FILENAME = "symbol_metadata.txt"

# Bench binaries whose symbolization provenance the contract tracks. These match
# the prefixes captured by ``record_symbol_metadata`` in pane_profile.sh.
EXPECTED_BINARY_LABELS = (
    "pane_profile_harness",
    "layout_bench",
    "pane_terminal_bench",
    "pane_pointer_bench",
)

# Replay summary fields lifted out of the harness manifest into the index so the
# index is self-describing for downstream gates.
REPLAY_SUMMARY_FIELDS = (
    "scenario",
    "leaf_count",
    "operations_per_iteration",
    "iterations",
    "warmup_iterations",
    "baseline_hash",
    "final_hash",
    "aggregate_hash",
    "ns_per_iteration",
    "checkpoint_interval",
    "checkpoint_count",
    "checkpoint_hit",
    "replay_start_idx",
    "replay_depth",
)

# Per-binary fields the symbol metadata must carry for the contract to hold.
REQUIRED_BINARY_FIELDS = (
    "executed_path",
    "binary_source",
    "exact_binary_status",
    "build_id",
    "debug_info",
    "stripped",
    "addr2line_ready",
    "symbolization_ready",
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 16), b""):
            digest.update(chunk)
    return digest.hexdigest()


def is_hex(value: str) -> bool:
    if not value:
        return False
    try:
        int(value, 16)
        return True
    except ValueError:
        return False


def parse_symbol_metadata(text: str) -> List[Dict[str, Any]]:
    """Parse the ``== label ==`` blocks of symbol_metadata.txt into dicts.

    ``key=value`` lines become entries; lines without ``=`` (e.g. the raw
    ``file`` output) are collected under ``notes``.
    """
    blocks: List[Dict[str, Any]] = []
    current: Optional[Dict[str, Any]] = None
    for raw in text.splitlines():
        line = raw.rstrip("\n")
        stripped = line.strip()
        if stripped.startswith("== ") and stripped.endswith(" =="):
            label = stripped[3:-3].strip()
            current = {"label": label, "notes": []}
            blocks.append(current)
            continue
        if current is None:
            continue
        if not stripped:
            continue
        if "=" in stripped:
            key, _, value = stripped.partition("=")
            key = key.strip()
            value = value.strip()
            # First occurrence wins; ``debug_info`` can repeat with a suffix in
            # the degraded branch, keep the explicit machine field.
            if key not in current or current[key] in ("", "notes"):
                current[key] = value
        else:
            current["notes"].append(stripped)
    return blocks


# ---------------------------------------------------------------------------
# Emit
# ---------------------------------------------------------------------------


def build_index(
    out_dir: Path,
    *,
    test_mode: bool,
    perf_stat: bool,
    stack_reports: bool,
    runner: str,
) -> Dict[str, Any]:
    harness_dir = out_dir / HARNESS_SUBDIR
    manifest_path = harness_dir / "manifest.json"
    if not manifest_path.is_file():
        raise SystemExit(
            f"replay manifest missing: {manifest_path} "
            "(did the pane_profile_harness run produce artifacts?)"
        )
    try:
        manifest = json.loads(manifest_path.read_text())
    except json.JSONDecodeError as exc:
        raise SystemExit(f"replay manifest is not valid JSON: {manifest_path}: {exc}") from exc

    def rel(path: Path) -> str:
        return str(path.relative_to(out_dir))

    def file_ref(path: Path) -> Dict[str, Any]:
        return {
            "path": rel(path),
            "sha256": sha256_file(path),
            "size_bytes": path.stat().st_size,
        }

    # --- Replay evidence -------------------------------------------------
    snapshots = []
    for name in ("baseline_snapshot.json", "final_snapshot.json"):
        snap = harness_dir / name
        if snap.is_file():
            snapshots.append(file_ref(snap))
    run_log = harness_dir / "run.log"
    replay: Dict[str, Any] = {
        "manifest": file_ref(manifest_path),
        "snapshots": snapshots,
        "run_log": file_ref(run_log) if run_log.is_file() else None,
    }
    for field in REPLAY_SUMMARY_FIELDS:
        if field in manifest:
            replay[field] = manifest[field]
    # Carry the diagnostics blocks verbatim so later gates need not re-read
    # the manifest file to reason about allocation/retention budgets.
    for field in ("allocation_diagnostics", "retention_diagnostics"):
        if field in manifest:
            replay[field] = manifest[field]

    # --- Symbolization manifest -----------------------------------------
    symbol_meta_path = out_dir / SYMBOL_METADATA_FILENAME
    binaries: List[Dict[str, Any]] = []
    symbol_metadata_ref: Optional[Dict[str, Any]] = None
    if symbol_meta_path.is_file():
        symbol_metadata_ref = file_ref(symbol_meta_path)
        for block in parse_symbol_metadata(symbol_meta_path.read_text()):
            entry: Dict[str, Any] = {"label": block.get("label", "")}
            for key in (
                "executed_path",
                "worker",
                "worker_endpoint",
                "binary_source",
                "exact_binary_status",
                "exact_binary_local",
                "fetch_error",
                "local_candidate",
                "local_binary_status",
                "build_id",
                "debug_info",
                "stripped",
                "addr2line_ready",
                "binary_sha256",
                "symbolization_ready",
            ):
                if key in block:
                    entry[key] = block[key]
            entry["symbolization_ready"] = _is_truthy(block.get("symbolization_ready"))
            entry["addr2line_ready"] = _is_truthy(block.get("addr2line_ready"))
            binaries.append(entry)

    all_ready = bool(binaries) and all(b.get("symbolization_ready") for b in binaries)
    symbolization = {
        "metadata": symbol_metadata_ref,
        "binaries": binaries,
        "all_symbolization_ready": all_ready,
        "expected_labels": list(EXPECTED_BINARY_LABELS),
    }

    # --- Checksummed artifact manifest (every file in the bundle) --------
    artifacts: List[Dict[str, Any]] = []
    for path in sorted(out_dir.rglob("*")):
        if not path.is_file():
            continue
        if path.name == INDEX_FILENAME and path.parent == out_dir:
            continue
        artifacts.append(file_ref(path))

    index = {
        "schema": SCHEMA,
        "schema_version": SCHEMA_VERSION,
        "bead": BEAD,
        "generated_by": "pane_profile.sh",
        "runner": runner,
        "mode": {
            "test_mode": test_mode,
            "perf_stat": perf_stat,
            "stack_reports": stack_reports,
        },
        "out_dir": str(out_dir),
        "replay": replay,
        "symbolization": symbolization,
        "artifacts": artifacts,
    }
    return index


def _is_truthy(value: Any) -> bool:
    return str(value).strip().lower() == "true"


def cmd_emit(args: argparse.Namespace) -> int:
    out_dir = Path(args.out_dir).resolve()
    index = build_index(
        out_dir,
        test_mode=args.test_mode,
        perf_stat=args.perf_stat,
        stack_reports=args.stack_reports,
        runner=args.runner,
    )
    index_path = out_dir / INDEX_FILENAME
    index_path.write_text(json.dumps(index, indent=2, sort_keys=True) + "\n")
    print(f"replay_artifact_index={index_path}")
    print(f"replay_scenario={index['replay'].get('scenario', 'unknown')}")
    print(f"replay_baseline_hash={index['replay'].get('baseline_hash', 'unknown')}")
    print(f"replay_final_hash={index['replay'].get('final_hash', 'unknown')}")
    print(f"replay_aggregate_hash={index['replay'].get('aggregate_hash', 'unknown')}")
    print(
        "symbolization_ready="
        f"{index['symbolization']['all_symbolization_ready']} "
        f"binaries={len(index['symbolization']['binaries'])}"
    )
    return 0


# ---------------------------------------------------------------------------
# Validate
# ---------------------------------------------------------------------------


class ValidationError(Exception):
    pass


def validate_index(
    index_path: Path,
    *,
    out_dir: Optional[Path],
    require_symbolization: bool,
) -> Dict[str, Any]:
    if not index_path.is_file():
        raise ValidationError(f"index not found: {index_path}")
    try:
        index = json.loads(index_path.read_text())
    except json.JSONDecodeError as exc:
        raise ValidationError(f"index is not valid JSON: {exc}") from exc

    base = out_dir.resolve() if out_dir is not None else index_path.parent.resolve()
    errors: List[str] = []

    if index.get("schema") != SCHEMA:
        errors.append(f"schema mismatch: expected {SCHEMA!r}, got {index.get('schema')!r}")
    if index.get("schema_version") != SCHEMA_VERSION:
        errors.append(
            f"schema_version mismatch: expected {SCHEMA_VERSION}, got {index.get('schema_version')}"
        )
    for key in ("replay", "symbolization", "artifacts"):
        if key not in index:
            errors.append(f"missing top-level key: {key!r}")
    if errors:
        raise ValidationError("; ".join(errors))

    def check_ref(ref: Optional[Dict[str, Any]], what: str) -> None:
        if ref is None:
            errors.append(f"{what}: missing file reference")
            return
        rel = ref.get("path")
        if not rel:
            errors.append(f"{what}: reference has no path")
            return
        target = base / rel
        if not target.is_file():
            errors.append(f"{what}: file does not exist: {rel}")
            return
        actual = sha256_file(target)
        if actual != ref.get("sha256"):
            errors.append(
                f"{what}: checksum mismatch for {rel} "
                f"(index={ref.get('sha256')}, actual={actual})"
            )

    # --- Replay -----------------------------------------------------------
    replay = index["replay"]
    check_ref(replay.get("manifest"), "replay.manifest")
    for field in ("scenario", "baseline_hash", "final_hash", "aggregate_hash"):
        if field not in replay:
            errors.append(f"replay: missing summary field {field!r}")
    snapshots = replay.get("snapshots") or []
    if len(snapshots) < 2:
        errors.append(f"replay: expected >=2 snapshots, got {len(snapshots)}")
    for idx, snap in enumerate(snapshots):
        check_ref(snap, f"replay.snapshots[{idx}]")
    check_ref(replay.get("run_log"), "replay.run_log")

    # Cross-check: the index replay hashes must agree with the manifest itself,
    # so the index can never drift from the evidence it claims to summarize.
    manifest_ref = replay.get("manifest") or {}
    manifest_rel = manifest_ref.get("path")
    if manifest_rel and (base / manifest_rel).is_file():
        try:
            manifest = json.loads((base / manifest_rel).read_text())
        except json.JSONDecodeError as exc:
            raise ValidationError(f"replay manifest is not valid JSON: {manifest_rel}: {exc}") from exc
        for field in ("baseline_hash", "final_hash", "aggregate_hash", "scenario"):
            if field in replay and field in manifest and replay[field] != manifest[field]:
                errors.append(
                    f"replay.{field} drifted from manifest "
                    f"(index={replay[field]}, manifest={manifest[field]})"
                )

    # --- Symbolization ----------------------------------------------------
    symbolization = index["symbolization"]
    check_ref(symbolization.get("metadata"), "symbolization.metadata")
    binaries = symbolization.get("binaries") or []
    labels = {b.get("label") for b in binaries}
    for expected in EXPECTED_BINARY_LABELS:
        if expected not in labels:
            errors.append(f"symbolization: missing expected binary label {expected!r}")
    for binary in binaries:
        label = binary.get("label", "<unlabeled>")
        for field in REQUIRED_BINARY_FIELDS:
            if field not in binary:
                errors.append(f"symbolization[{label}]: missing field {field!r}")
        if require_symbolization:
            if not binary.get("symbolization_ready"):
                errors.append(
                    f"symbolization[{label}]: not symbolization-ready "
                    f"(debug_info={binary.get('debug_info')}, "
                    f"stripped={binary.get('stripped')}, "
                    f"addr2line_ready={binary.get('addr2line_ready')})"
                )
            if not is_hex(str(binary.get("build_id", ""))):
                errors.append(
                    f"symbolization[{label}]: build_id not a hex string: "
                    f"{binary.get('build_id')!r}"
                )

    # --- Checksummed artifact manifest -----------------------------------
    artifacts = index["artifacts"]
    if not artifacts:
        errors.append("artifacts: empty checksummed manifest")
    listed_paths = set()
    for ref in artifacts:
        check_ref(ref, "artifacts")
        if ref.get("path"):
            listed_paths.add(ref["path"])
    # Completeness: the core replay + symbolization files must be in the list.
    required_in_manifest = [
        f"{HARNESS_SUBDIR}/manifest.json",
        SYMBOL_METADATA_FILENAME,
    ]
    for rel in required_in_manifest:
        if rel not in listed_paths:
            errors.append(f"artifacts: required file not listed: {rel}")

    if errors:
        raise ValidationError("\n".join(f"  - {e}" for e in errors))

    return {
        "index": str(index_path),
        "scenario": replay.get("scenario"),
        "baseline_hash": replay.get("baseline_hash"),
        "final_hash": replay.get("final_hash"),
        "aggregate_hash": replay.get("aggregate_hash"),
        "binary_count": len(binaries),
        "all_symbolization_ready": symbolization.get("all_symbolization_ready"),
        "artifact_count": len(artifacts),
        "require_symbolization": require_symbolization,
    }


def cmd_validate(args: argparse.Namespace) -> int:
    index_path = Path(args.index).resolve()
    out_dir = Path(args.out_dir).resolve() if args.out_dir else None
    try:
        summary = validate_index(
            index_path,
            out_dir=out_dir,
            require_symbolization=args.require_symbolization,
        )
    except ValidationError as exc:
        if args.json:
            print(json.dumps({"ok": False, "error": str(exc)}, indent=2))
        else:
            print("PANE REPLAY ARTIFACT CONTRACT: FAIL", file=sys.stderr)
            print(str(exc), file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps({"ok": True, **summary}, indent=2, sort_keys=True))
    else:
        print("PANE REPLAY ARTIFACT CONTRACT: OK")
        print(f"  scenario={summary['scenario']}")
        print(
            f"  hashes baseline={summary['baseline_hash']} "
            f"final={summary['final_hash']} aggregate={summary['aggregate_hash']}"
        )
        print(
            f"  binaries={summary['binary_count']} "
            f"all_symbolization_ready={summary['all_symbolization_ready']} "
            f"artifacts={summary['artifact_count']} "
            f"strict={summary['require_symbolization']}"
        )
    return 0


# ---------------------------------------------------------------------------
# Selftest -- exercises emit + validate without a cargo build.
# ---------------------------------------------------------------------------


def _write_synthetic_bundle(out_dir: Path, *, symbolization_ready: bool) -> None:
    harness_dir = out_dir / HARNESS_SUBDIR
    harness_dir.mkdir(parents=True, exist_ok=True)
    manifest = {
        "scenario": "selftest-scenario",
        "leaf_count": 8,
        "operations_per_iteration": 8,
        "iterations": 4,
        "warmup_iterations": 1,
        "baseline_hash": 111,
        "final_hash": 222,
        "aggregate_hash": 333,
        "ns_per_iteration": 4242,
        "checkpoint_interval": 4,
        "checkpoint_count": 1,
        "checkpoint_hit": True,
        "replay_start_idx": 0,
        "replay_depth": 0,
        "allocation_diagnostics": {"allocations": 1, "bytes_allocated": 2},
        "retention_diagnostics": {"estimated_total_retained_bytes": 9},
    }
    (harness_dir / "manifest.json").write_text(json.dumps(manifest, indent=2))
    (harness_dir / "baseline_snapshot.json").write_text('{"nodes": []}\n')
    (harness_dir / "final_snapshot.json").write_text('{"nodes": [1]}\n')
    (harness_dir / "run.log").write_text("scenario=selftest-scenario\n")

    ready = "true" if symbolization_ready else "false"
    debug = "present" if symbolization_ready else "missing"
    stripped = "false" if symbolization_ready else "true"
    blocks = []
    for label in EXPECTED_BINARY_LABELS:
        blocks.append(
            "\n".join(
                [
                    f"== {label} ==",
                    f"executed_path=/synthetic/{label}-deadbeef",
                    "worker=local",
                    "worker_endpoint=local",
                    "binary_source=executed_local_preserved",
                    "exact_binary_status=available",
                    f"exact_binary_local=executed-binaries/{label}-deadbeef",
                    "fetch_error=none",
                    "local_candidate=missing",
                    f"build_id={'9a5b4fc8d6b5ea55511d50313b2d5fedfe087710' if symbolization_ready else 'missing'}",
                    f"debug_info={debug}",
                    f"stripped={stripped}",
                    f"addr2line_ready={ready}",
                    "binary_sha256=00",
                    f"symbolization_ready={ready}",
                    "",
                ]
            )
        )
    (out_dir / SYMBOL_METADATA_FILENAME).write_text("\n".join(blocks))


def cmd_selftest(_: argparse.Namespace) -> int:
    failures: List[str] = []

    def check(name: str, condition: bool) -> None:
        status = "ok" if condition else "FAIL"
        print(f"  [{status}] {name}")
        if not condition:
            failures.append(name)

    with tempfile.TemporaryDirectory() as tmp:
        # Strict-ready bundle.
        ready_dir = Path(tmp) / "ready"
        _write_synthetic_bundle(ready_dir, symbolization_ready=True)
        index = build_index(
            ready_dir, test_mode=True, perf_stat=False, stack_reports=False, runner="selftest"
        )
        (ready_dir / INDEX_FILENAME).write_text(json.dumps(index, indent=2, sort_keys=True) + "\n")
        index_path = ready_dir / INDEX_FILENAME

        try:
            validate_index(index_path, out_dir=None, require_symbolization=False)
            check("ready bundle passes lenient validation", True)
        except ValidationError as exc:
            check(f"ready bundle passes lenient validation ({exc})", False)
        try:
            validate_index(index_path, out_dir=None, require_symbolization=True)
            check("ready bundle passes strict validation", True)
        except ValidationError as exc:
            check(f"ready bundle passes strict validation ({exc})", False)

        # Tamper: change a referenced file after emission -> checksum must fail.
        (ready_dir / HARNESS_SUBDIR / "final_snapshot.json").write_text('{"nodes": [9,9]}\n')
        try:
            validate_index(index_path, out_dir=None, require_symbolization=False)
            check("tampered snapshot is rejected", False)
        except ValidationError:
            check("tampered snapshot is rejected", True)

        # Degraded bundle (no debug info) passes lenient but fails strict.
        degraded_dir = Path(tmp) / "degraded"
        _write_synthetic_bundle(degraded_dir, symbolization_ready=False)
        dindex = build_index(
            degraded_dir, test_mode=True, perf_stat=False, stack_reports=False, runner="selftest"
        )
        (degraded_dir / INDEX_FILENAME).write_text(json.dumps(dindex, indent=2, sort_keys=True) + "\n")
        dpath = degraded_dir / INDEX_FILENAME
        try:
            validate_index(dpath, out_dir=None, require_symbolization=False)
            check("degraded bundle passes lenient validation", True)
        except ValidationError as exc:
            check(f"degraded bundle passes lenient validation ({exc})", False)
        try:
            validate_index(dpath, out_dir=None, require_symbolization=True)
            check("degraded bundle fails strict validation", False)
        except ValidationError:
            check("degraded bundle fails strict validation", True)

        # Missing manifest -> emit must refuse.
        empty_dir = Path(tmp) / "empty"
        (empty_dir / HARNESS_SUBDIR).mkdir(parents=True)
        try:
            build_index(
                empty_dir, test_mode=True, perf_stat=False, stack_reports=False, runner="selftest"
            )
            check("emit refuses missing replay manifest", False)
        except SystemExit:
            check("emit refuses missing replay manifest", True)

    if failures:
        print(f"SELFTEST FAILED: {len(failures)} check(s) failed")
        return 1
    print("SELFTEST OK")
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    emit = sub.add_parser("emit", help="build replay_artifact_index.json from an out-dir")
    emit.add_argument("--out-dir", required=True, help="pane_profile.sh output directory")
    emit.add_argument("--test-mode", action="store_true")
    emit.add_argument("--perf-stat", action="store_true")
    emit.add_argument("--stack-reports", action="store_true")
    emit.add_argument("--runner", default="local", help="runner label (local|rch|ci)")
    emit.set_defaults(func=cmd_emit)

    validate = sub.add_parser("validate", help="validate a replay_artifact_index.json")
    validate.add_argument("--index", required=True, help="path to replay_artifact_index.json")
    validate.add_argument(
        "--out-dir",
        default=None,
        help="bundle root for resolving relative paths (default: index's directory)",
    )
    validate.add_argument(
        "--require-symbolization",
        action="store_true",
        help="fail unless every binary is symbolization-ready (strict CI mode)",
    )
    validate.add_argument("--json", action="store_true", help="emit machine-readable result")
    validate.set_defaults(func=cmd_validate)

    selftest = sub.add_parser("selftest", help="exercise emit+validate on synthetic bundles")
    selftest.set_defaults(func=cmd_selftest)

    return parser


def main(argv: Optional[List[str]] = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())

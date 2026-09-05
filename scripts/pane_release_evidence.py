#!/usr/bin/env python3
"""Pane release-evidence bundle (bd-1w0w4.7).

The pane workspace ships only when six independent evidence dimensions are
green: **unit**, **e2e**, **parity**, **perf**, **a11y**, and **logging**. Each
dimension already produces its own artifacts (test suites, replay indexes, soak
JSONL, compliance/traceability matrices, observability schema). This tool ties
them into ONE checksummed, schema-versioned ``pane_release_evidence.json``
bundle so a release decision can stand on a single coherent artifact instead of
a scatter of logs.

The bundle records, per dimension:

  * the **authoritative test suites** (crate + integration test target) that
    must be green for that dimension, with optional observed pass counts;
  * the **static artifacts** committed to the repo (compliance matrix,
    traceability matrix, parity contract, jsonl schema, golden oracle), each
    SHA-256 checksummed so a downstream gate can trust they describe this tree;
  * the **runtime artifacts** emitted by a CI run (replay index, differential
    certification, soak JSONL), checksummed when a ``--results-dir`` is given.

This tool deliberately does NOT compute the go/no-go verdict -- that policy
lives in the release gate (bd-1w0w4.5), which *consumes* this bundle. Here we
guarantee the evidence is present, coherent, and checksummed.

Subcommands
-----------
``collect``   Build ``pane_release_evidence.json`` from the repo (static
              artifacts) plus an optional CI ``--results-dir`` (runtime
              artifacts) and an optional ``--test-summary`` JSON (observed
              pass/fail counts per suite).
``validate``  Re-derive every checksum and check structural completeness. With
              ``--require-runtime`` it additionally fails unless every runtime
              artifact is present (the strict mode CI uses post-run).
``selftest``  Build a synthetic tree in a temp dir, round-trip collect+validate,
              and assert the contract holds -- runnable with zero real CI state.

Exit codes: 0 success, 1 contract/validation failure, 2 usage error.
"""

from __future__ import annotations

import argparse
from contextlib import nullcontext
import gzip
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path
from typing import Any

SCHEMA = "ftui.pane.release_evidence"
SCHEMA_VERSION = 2
SCOPE = "native terminal and Rust web-backend simulations; no real browser-engine or OS assistive-technology acceptance"

# The six release dimensions, each grounded in real, in-tree evidence. Paths are
# repo-relative. ``static`` artifacts are committed and checksummed from the
# repo; ``runtime`` artifacts are produced by a CI run and checksummed from the
# --results-dir; ``suites`` are the authoritative integration tests that must be
# green for the dimension.
DIMENSIONS: dict[str, dict[str, Any]] = {
    "unit": {
        "description": "split-tree invariants, solver stability, property/fuzz",
        "suites": [
            ("ftui-layout", "pane_invariant_fuzz"),
            ("ftui-layout", "pane_determinism_matrix"),
            ("ftui-layout", "pane_operation_family_equivalence"),
            ("ftui-layout", "pane_persistent_equivalence"),
            ("ftui-layout", "pane_monitor_gates"),
            ("ftui-layout", "pane_margin"),
        ],
        "static": [],
        "runtime": [],
    },
    "e2e": {
        "description": "terminal PTY + native Rust web-backend drag/resize/keyboard simulations",
        "suites": [
            ("ftui-harness", "pane_input_pty_e2e"),
            ("ftui-harness", "pane_splitter_drag_pty_e2e"),
            ("ftui-web", "pane_web_e2e"),
        ],
        "static": ["scripts/pane_e2e.sh"],
        "runtime": [],
    },
    "parity": {
        "description": "terminal vs native Rust web-backend simulation identity",
        "suites": [
            ("ftui-web", "pane_cross_host_parity"),
        ],
        "static": ["docs/spec/pane-parity-contract-and-program.md"],
        "runtime": [],
    },
    "perf": {
        "description": "replay/perf budgets, golden oracle, soak + rollback",
        "suites": [
            ("ftui-layout", "pane_soak_stress"),
            ("ftui-layout", "pane_soak_rollback"),
            ("ftui-layout", "pane_checkpoint_integration"),
            ("ftui-layout", "pane_semantic_replay_harness"),
        ],
        "static": ["scripts/pane_replay_golden.json"],
        "runtime": [
            "replay_artifact_index.json",
            "differential_certification.json",
        ],
    },
    "a11y": {
        "description": "accessibility compliance + discoverability",
        "suites": [
            ("ftui-demo-showcase", "pane_a11y_compliance_a11y"),
            ("ftui-demo-showcase", "pane_discoverability_a11y"),
        ],
        "static": ["tests/e2e/pane_a11y_compliance_matrix.json"],
        "runtime": [],
    },
    "logging": {
        "description": "structured observability schema + traceability matrix",
        "suites": [
            ("ftui-runtime", "e2e_observability_pipeline"),
            ("ftui-runtime", "traceability_matrix"),
        ],
        "static": [
            "tests/e2e/pane_traceability_matrix.json",
            "tests/e2e/lib/e2e_jsonl_schema.json",
        ],
        "runtime": [],
    },
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_json(path: Path) -> Any:
    """Reject ambiguous JSON before a duplicate identity can be overwritten."""
    def object_pairs(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON key: {key!r}")
            result[key] = value
        return result

    def reject_constant(value):
        raise ValueError(f"non-finite JSON value: {value}")

    return json.loads(path.read_text(), object_pairs_hook=object_pairs,
                      parse_constant=reject_constant)


def observation_errors(record: Any) -> list[str]:
    if not isinstance(record, dict):
        return ["observation must be an object"]
    errors = []
    for field in ("passed", "failed", "exit_code"):
        value = record.get(field)
        if type(value) is not int or value < 0:
            errors.append(f"{field} must be a nonnegative integer")
    if record.get("verdict") not in ("ok", "FAILED"):
        errors.append("verdict must be 'ok' or 'FAILED'")
    return errors


def observed_green(record: dict[str, Any]) -> bool:
    return (not observation_errors(record) and record["passed"] > 0
            and record["failed"] == 0 and record["exit_code"] == 0
            and record["verdict"] == "ok")


def contained_path(root: Path, relative: Any) -> Path:
    """Resolve relocatable references without allowing traversal or symlink escape."""
    if (not isinstance(relative, str) or not relative or "\\" in relative
            or Path(relative).is_absolute() or ".." in Path(relative).parts
            or Path(relative).as_posix() != relative):
        raise ValueError(f"invalid relative artifact path: {relative!r}")
    target = (root / relative).resolve()
    if not target.is_relative_to(root.resolve()):
        raise ValueError(f"artifact escapes its root: {relative!r}")
    return target


def build_identity(root: Path, run_id: str) -> dict[str, Any]:
    """Identity of the trusted producer's checkout and default-feature build.

    This is evidence binding, not an authenticity/signing service. The runner
    must capture its actual command and exit status; hashing a claim cannot
    make a dishonest producer truthful.
    """
    if not run_id:
        raise ValueError("PANE_RELEASE_RUN_ID is required for release provenance")
    def command(*argv):
        return subprocess.check_output(argv, cwd=root, text=True).strip()
    compiler = command("rustc", "-Vv")
    pin = tomllib.loads((root / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    if not isinstance(pin, str) or not re.fullmatch(r"nightly-\d{4}-\d{2}-\d{2}", pin):
        raise ValueError("release toolchain must be a dated nightly")
    if compiler != command("rustup", "run", pin, "rustc", "-Vv"):
        raise ValueError("active compiler differs from the repository toolchain pin")
    host = next(line.removeprefix("host: ") for line in compiler.splitlines()
                if line.startswith("host: "))
    return {
        "run_id": run_id,
        "commit": command("git", "rev-parse", "HEAD"),
        "tree": command("git", "rev-parse", "HEAD^{tree}"),
        "dirty": bool(command("git", "status", "--porcelain", "--untracked-files=no")),
        "lock_sha256": sha256_file(root / "Cargo.lock"),
        "toolchain_sha256": sha256_file(root / "rust-toolchain.toml"),
        "compiler": compiler,
        "target": os.environ.get("CARGO_BUILD_TARGET", host),
        "features": ["default"],
    }


def provenance(root: Path, producer: str, schema: str, version: int) -> dict[str, Any] | None:
    run_id = os.environ.get("PANE_RELEASE_RUN_ID")
    if not run_id:
        return None  # Local partial diagnostics cannot qualify for a release.
    return {"identity": build_identity(root, run_id), "observed_at": int(time.time()),
            "producer": producer, "producer_sha256": sha256_file(root / producer),
            "schema": schema, "schema_version": version}


def provenance_errors(value: Any, expected: dict[str, Any], root: Path,
                      producer: str, schema: str, version: int,
                      now: int | None = None) -> list[str]:
    if not isinstance(value, dict):
        return [f"{producer}: missing provenance"]
    errors = []
    if value.get("identity") != expected:
        errors.append(f"{producer}: build/run identity mismatch")
    if expected.get("dirty") is not False:
        errors.append(f"{producer}: release source tree is dirty")
    timestamp = value.get("observed_at")
    current = int(time.time()) if now is None else now
    if type(timestamp) is not int or not current - 86400 <= timestamp <= current:
        errors.append(f"{producer}: observation is stale, future-dated, or invalid")
    if value.get("producer") != producer or value.get("schema") != schema or type(value.get("schema_version")) is not int or value.get("schema_version") != version:
        errors.append(f"{producer}: producer/schema mismatch")
    try:
        if value.get("producer_sha256") != sha256_file(root / producer):
            errors.append(f"{producer}: producer checksum mismatch")
    except OSError as exc:
        errors.append(f"{producer}: {exc}")
    return errors


def checked_ref(ref: Any, root: Path) -> Path:
    if not isinstance(ref, dict):
        raise ValueError("missing artifact reference")
    target = contained_path(root, ref.get("path"))
    if not target.is_file() or sha256_file(target) != ref.get("sha256"):
        raise ValueError(f"missing artifact or checksum drift: {ref.get('path')}")
    return target


def archive_binary(binary: Path, results: Path) -> dict[str, Any]:
    binary_hash = sha256_file(binary)
    archive = results / "binaries" / f"{binary_hash}.gz"
    archive.parent.mkdir(parents=True, exist_ok=True)
    if not archive.exists():
        with binary.open("rb") as source, archive.open("xb") as raw, gzip.GzipFile(
                filename="", mode="wb", fileobj=raw, mtime=0) as dest:
            for chunk in iter(lambda: source.read(65536), b""):
                dest.write(chunk)
    return {"path": str(archive.relative_to(results)), "sha256": sha256_file(archive),
            "executable_sha256": binary_hash, "size_bytes": binary.stat().st_size}


def check_binary(binary: Any, results: Path) -> None:
    archive = checked_ref(binary, results)
    size = binary.get("size_bytes")
    if type(size) is not int or not 0 < size <= 2 * 1024**3:
        raise ValueError("invalid executable byte count")
    digest = hashlib.sha256()
    observed = 0
    with gzip.open(archive, "rb") as source:
        for chunk in iter(lambda: source.read(65536), b""):
            observed += len(chunk)
            if observed > size:
                raise ValueError("archived executable exceeds recorded byte count")
            digest.update(chunk)
    if observed != size or digest.hexdigest() != binary.get("executable_sha256"):
        raise ValueError("executable checksum/size mismatch")


def validate_observation_artifacts(record: dict[str, Any], crate: str, target: str,
                                   results: Path, root: Path,
                                   expected: dict[str, Any]) -> list[str]:
    from pane_test_summary_aggregate import capture, SCHEMA as summary_schema, SCHEMA_VERSION as summary_version
    errors = provenance_errors(record.get("provenance"), expected, root,
                                "scripts/pane_test_summary_aggregate.py", summary_schema, summary_version)
    try:
        log = checked_ref(record.get("log"), results)
        parsed = capture(log.read_text(errors="strict"), crate, target, record["exit_code"])
        for key in ("passed", "failed", "verdict", "exit_code"):
            if parsed[f"{crate}::{target}"][key] != record.get(key):
                errors.append(f"{crate}::{target}: {key} disagrees with archived log")
        command = ["cargo", "test", "-p", crate, "--test", target, "--", "--nocapture"]
        if record.get("command") != command:
            errors.append(f"{crate}::{target}: unexpected test command/features")
        check_binary(record.get("binary"), results)
    except (ValueError, OSError, KeyError, EOFError) as exc:
        errors.append(f"{crate}::{target}: {exc}")
    return errors


def validate_release_provenance(bundle: dict[str, Any], cert: Any, root: Path,
                                results: Path | None, expected: dict[str, Any]) -> list[str]:
    """Verify producer receipts and recompute the certification from its inputs."""
    from pane_replay_artifacts import (SCHEMA as replay_schema, SCHEMA_VERSION as replay_version,
                                      DIFF_CERT_SCHEMA, DIFF_CERT_SCHEMA_VERSION,
                                      build_certification, load_golden, validate_index, ValidationError)
    errors = provenance_errors(bundle.get("provenance"), expected, root,
                                "scripts/pane_release_evidence.py", SCHEMA, SCHEMA_VERSION)
    if results is None:
        return errors + ["results root required for release provenance"]
    for dim in bundle["dimensions"].values():
        for record in dim["suites"]:
            if record["status"] != "declared":
                errors.extend(validate_observation_artifacts(record, record["crate"],
                                                              record["target"], results, root, expected))
    try:
        arts = {art["name"]: art for art in bundle["dimensions"]["perf"]["runtime_artifacts"]}
        index_path = checked_ref(arts["replay_artifact_index.json"], results)
        cert_path = checked_ref(arts["differential_certification.json"], results)
        actual_cert = load_json(cert_path)
        if not isinstance(cert, dict) or actual_cert != cert:
            return errors + ["supplied certificate differs from the checksummed run certificate"]
        if cert.get("schema") != DIFF_CERT_SCHEMA or type(cert.get("schema_version")) is not int or cert.get("schema_version") != DIFF_CERT_SCHEMA_VERSION:
            return errors + ["certification schema/version mismatch"]
        errors.extend(provenance_errors(cert.get("provenance"), expected, root,
                                        "scripts/pane_replay_artifacts.py", DIFF_CERT_SCHEMA, DIFF_CERT_SCHEMA_VERSION))
        index = load_json(index_path)
        errors.extend(provenance_errors(index.get("provenance"), expected, root,
                                        "scripts/pane_replay_artifacts.py", replay_schema, replay_version))
        validate_index(index_path, out_dir=index_path.parent, require_symbolization=True)
        inputs = cert.get("inputs")
        if not isinstance(inputs, dict):
            return errors + ["certificate has no checksummed input binding"]
        golden_path = root / "scripts/pane_replay_golden.json"
        if inputs.get("index_sha256") != sha256_file(index_path) or inputs.get("golden_sha256") != sha256_file(golden_path):
            errors.append("certificate replay/golden input checksum mismatch")
        summary = load_json(checked_ref(inputs.get("differential_summary"), results))
        if not isinstance(summary, dict) or set(summary) != {"_meta", "ftui-layout::pane_determinism_matrix"}:
            return errors + ["certificate requires exactly the observed differential suite"]
        record = summary["ftui-layout::pane_determinism_matrix"]
        from pane_test_summary_aggregate import SCHEMA as summary_schema, SCHEMA_VERSION as summary_version
        meta = summary.get("_meta")
        if not isinstance(meta, dict) or meta.get("schema") != summary_schema or type(meta.get("schema_version")) is not int or meta.get("schema_version") != summary_version:
            return errors + ["differential summary schema/version mismatch"]
        invalid = observation_errors(record)
        if invalid:
            return errors + invalid
        errors.extend(validate_observation_artifacts(record, "ftui-layout", "pane_determinism_matrix", results, root, expected))
        recomputed = build_certification(index, load_golden(golden_path), matrix_passed=observed_green(record))
        for key in ("scenario", "classification", "golden_oracle", "differential_matrix", "timing", "summary"):
            if cert.get(key) != recomputed[key]:
                errors.append(f"certificate {key} disagrees with verified replay/differential evidence")
    except (ValueError, OSError, KeyError, TypeError, ValidationError, SystemExit) as exc:
        errors.append(f"release certification: {exc}")
    return errors


def _repo_root(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit).resolve()
    # scripts/ lives directly under the repo root.
    return Path(__file__).resolve().parent.parent


def _suite_record(crate: str, target: str, summary: dict[str, Any]) -> dict[str, Any]:
    key = f"{crate}::{target}"
    observed = summary.get(key)
    rec: dict[str, Any] = {"crate": crate, "target": target}
    if observed is not None:
        errors = observation_errors(observed)
        if errors:
            raise ValueError(f"{key}: {'; '.join(errors)}")
        rec.update(observed)
        rec["status"] = "green" if observed_green(observed) else "red"
    else:
        rec["status"] = "declared"
    return rec


def collect(
    repo_root: Path,
    results_dir: Path | None,
    summary: dict[str, Any],
    *, run_provenance: dict[str, Any] | None = None,
) -> tuple[dict[str, Any], list[str]]:
    if not isinstance(summary, dict):
        raise ValueError("test summary must be an object")
    if summary:
        from pane_test_summary_aggregate import SCHEMA as summary_schema, SCHEMA_VERSION as summary_version
        meta = summary.get("_meta")
        if not isinstance(meta, dict) or meta.get("schema") != summary_schema or type(meta.get("schema_version")) is not int or meta.get("schema_version") != summary_version:
            raise ValueError("test summary schema/version mismatch")
    expected = {f"{c}::{t}" for spec in DIMENSIONS.values() for c, t in spec["suites"]}
    unknown = set(summary) - expected - {"_meta"}
    if unknown:
        raise ValueError(f"unknown suite identities: {sorted(unknown)}")
    errors: list[str] = []
    dims_out: dict[str, Any] = {}

    for name, spec in DIMENSIONS.items():
        suites = [_suite_record(c, t, summary) for (c, t) in spec["suites"]]

        static_arts = []
        for rel in spec["static"]:
            target = repo_root / rel
            if not target.is_file():
                errors.append(f"[{name}] missing static artifact: {rel}")
                static_arts.append({"path": rel, "present": False})
                continue
            static_arts.append(
                {"path": rel, "present": True, "sha256": sha256_file(target)}
            )

        runtime_arts = []
        for rel in spec["runtime"]:
            entry: dict[str, Any] = {"name": rel, "present": False}
            if results_dir is not None:
                # Look for the artifact anywhere under the results dir.
                matches = sorted(p for p in results_dir.rglob(rel) if p.is_file())
                # The primary run owns root artifacts; nested certificates
                # belong to the separately certified extra golden scenarios.
                primary = results_dir / rel
                if primary.is_file():
                    matches = [primary]
                if len(matches) > 1:
                    errors.append(f"[{name}] ambiguous runtime artifact: {rel}")
                elif matches:
                    contained_path(results_dir, str(matches[0].relative_to(results_dir)))
                    entry = {
                        "name": rel,
                        "present": True,
                        "path": str(matches[0].relative_to(results_dir)),
                        "sha256": sha256_file(matches[0]),
                    }
            runtime_arts.append(entry)

        suite_red = [s for s in suites if s.get("status") == "red"]
        static_missing = [a for a in static_arts if not a["present"]]
        runtime_missing = [a for a in runtime_arts if not a["present"]]

        dims_out[name] = {
            "description": spec["description"],
            "suites": suites,
            "static_artifacts": static_arts,
            "runtime_artifacts": runtime_arts,
            "summary": {
                "suite_count": len(suites),
                "suites_red": len(suite_red),
                "static_present": len(static_arts) - len(static_missing),
                "static_total": len(static_arts),
                "runtime_present": len(runtime_arts) - len(runtime_missing),
                "runtime_total": len(runtime_arts),
            },
        }

    bundle: dict[str, Any] = {
        "schema": SCHEMA,
        "schema_version": SCHEMA_VERSION,
        "feature": "pane-workspace",
        "dimensions": dims_out,
        "overall": {
            "dimension_count": len(dims_out),
            "static_complete": all(
                d["summary"]["static_present"] == d["summary"]["static_total"]
                for d in dims_out.values()
            ),
            "runtime_complete": all(
                d["summary"]["runtime_present"] == d["summary"]["runtime_total"]
                for d in dims_out.values()
            ),
            "no_red_suites": all(
                d["summary"]["suites_red"] == 0 for d in dims_out.values()
            ),
        },
        "provenance": run_provenance,
        "scope": SCOPE,
    }
    return bundle, errors


def cmd_collect(args: argparse.Namespace) -> int:
    repo_root = _repo_root(args.repo_root)
    results_dir = Path(args.results_dir).resolve() if args.results_dir else None
    if results_dir is not None and not results_dir.is_dir():
        print(f"error: --results-dir not found: {results_dir}", file=sys.stderr)
        return 2

    summary: dict[str, Any] = {}
    if args.test_summary:
        summary_path = Path(args.test_summary)
        if not summary_path.is_file():
            print(f"error: --test-summary not found: {summary_path}", file=sys.stderr)
            return 2
        summary = load_json(summary_path)

    bundle, errors = collect(repo_root, results_dir, summary,
                             run_provenance=provenance(repo_root, "scripts/pane_release_evidence.py", SCHEMA, SCHEMA_VERSION))

    out_path = Path(args.out).resolve()
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(bundle, indent=2, sort_keys=True) + "\n")

    if args.json:
        print(json.dumps({"bundle": str(out_path), "errors": errors}, indent=2))
    else:
        print(f"wrote {out_path}")
        for err in errors:
            print(f"  warn: {err}", file=sys.stderr)

    # collect is non-fatal on missing static artifacts unless --strict.
    if errors and args.strict:
        return 1
    return 0


def validate_bundle(bundle: Any, require_runtime: bool, *,
                    require_observed: bool = False) -> list[str]:
    errors: list[str] = []
    if not isinstance(bundle, dict):
        return ["bundle must be an object"]
    if bundle.get("schema") != SCHEMA:
        errors.append(f"schema mismatch: expected {SCHEMA!r}, got {bundle.get('schema')!r}")
    if bundle.get("scope") != SCOPE:
        errors.append("evidence scope does not match the native/simulated suite inventory")
    if type(bundle.get("schema_version")) is not int or bundle.get("schema_version") != SCHEMA_VERSION:
        errors.append(
            f"schema_version mismatch: expected {SCHEMA_VERSION}, got "
            f"{bundle.get('schema_version')}"
        )
    dims = bundle.get("dimensions")
    if not isinstance(dims, dict):
        return errors + ["dimensions must be an object"]
    for name in sorted(set(dims) - set(DIMENSIONS)):
        errors.append(f"unknown dimension: {name}")
    for name, spec in DIMENSIONS.items():
        if name not in dims:
            errors.append(f"missing dimension: {name}")
            continue
        dim = dims[name]
        if not isinstance(dim, dict):
            errors.append(f"[{name}] dimension must be an object")
            continue
        suites = dim.get("suites")
        expected = {f"{c}::{t}" for c, t in spec["suites"]}
        seen = set()
        if not isinstance(suites, list):
            errors.append(f"[{name}] suites must be a list")
            suites = []
        for s in suites:
            if not isinstance(s, dict):
                errors.append(f"[{name}] suite must be an object")
                continue
            key = f"{s.get('crate')}::{s.get('target')}"
            if key not in expected or key in seen:
                errors.append(f"[{name}] unknown or duplicate suite: {key}")
            seen.add(key)
            if s.get("status") == "declared":
                if require_observed:
                    errors.append(f"[{name}] unobserved suite: {key}")
                if any(k in s for k in ("passed", "failed", "verdict", "exit_code")):
                    errors.append(f"[{name}] declared suite contains observed results: {key}")
            else:
                for error in observation_errors(s):
                    errors.append(f"[{name}] {key}: {error}")
                if s.get("status") not in ("green", "red"):
                    errors.append(f"[{name}] invalid suite status: {key}")
                if not observed_green(s) or s.get("status") != "green":
                    errors.append(f"[{name}] red suite or empty observation: {key}")
        for key in sorted(expected - seen):
            errors.append(f"[{name}] missing suite: {key}")
        for kind, identity in (("static", "path"), ("runtime", "name")):
            arts = dim.get(f"{kind}_artifacts")
            if not isinstance(arts, list):
                errors.append(f"[{name}] {kind}_artifacts must be a list")
                arts = []
            found = set()
            for art in arts:
                if not isinstance(art, dict) or not isinstance(art.get(identity), str):
                    errors.append(f"[{name}] invalid {kind} artifact record")
                    continue
                key = art[identity]
                if key not in spec[kind] or key in found:
                    errors.append(f"[{name}] unknown or duplicate {kind} artifact: {key}")
                found.add(key)
                if type(art.get("present")) is not bool:
                    errors.append(f"[{name}] {kind} present must be boolean: {key}")
                if art.get("present") is not True:
                    if kind == "static" or require_runtime:
                        errors.append(f"[{name}] missing {kind} artifact: {key}")
                else:
                    if not isinstance(art.get("sha256"), str) or not re.fullmatch(r"[0-9a-f]{64}", art["sha256"]):
                        errors.append(f"[{name}] invalid {kind} checksum: {key}")
                    try:
                        contained_path(Path("/"), art.get("path"))
                        if kind == "runtime" and Path(art["path"]).name != key:
                            errors.append(f"[{name}] runtime artifact filename differs from its identity: {key}")
                    except ValueError as exc:
                        errors.append(f"[{name}] {exc}")
            for key in sorted(set(spec[kind]) - found):
                errors.append(f"[{name}] missing {kind} artifact record: {key}")
    return errors


def validate_checksums(bundle: dict[str, Any], repo_root: Path,
                       results_dir: Path | None = None) -> list[str]:
    errors: list[str] = []
    for name, dim in bundle.get("dimensions", {}).items():
        for kind, root in (("static", repo_root), ("runtime", results_dir)):
            for art in dim.get(f"{kind}_artifacts", []):
                if not art.get("present"):
                    continue
                if root is None:
                    errors.append(f"[{name}] results root required to verify runtime artifact")
                    continue
                try:
                    target = contained_path(root, art.get("path"))
                    if not target.is_file():
                        errors.append(f"[{name}] {kind} artifact vanished: {art['path']}")
                    elif sha256_file(target) != art.get("sha256"):
                        errors.append(f"[{name}] checksum drift for {art['path']}")
                except (ValueError, OSError) as exc:
                    errors.append(f"[{name}] {exc}")
    return errors


def cmd_validate(args: argparse.Namespace) -> int:
    bundle_path = Path(args.bundle)
    if not bundle_path.is_file():
        print(f"error: bundle not found: {bundle_path}", file=sys.stderr)
        return 2
    bundle = load_json(bundle_path)

    errors = validate_bundle(bundle, require_runtime=args.require_runtime)
    if not errors and not args.no_checksums:
        results = Path(args.results_dir).resolve() if args.results_dir else None
        errors.extend(validate_checksums(bundle, _repo_root(args.repo_root), results))
    if not errors and args.require_runtime:
        if args.no_checksums:
            errors.append("release validation cannot skip checksums")
        else:
            root = _repo_root(args.repo_root)
            expected = build_identity(root, os.environ.get("PANE_RELEASE_RUN_ID", ""))
            cert_ref = next(art for art in bundle["dimensions"]["perf"]["runtime_artifacts"]
                            if art["name"] == "differential_certification.json")
            cert = load_json(checked_ref(cert_ref, results)) if results is not None else None
            errors.extend(validate_release_provenance(bundle, cert, root, results, expected))

    if args.json:
        print(json.dumps({"ok": not errors, "errors": errors}, indent=2))
    else:
        if errors:
            print("pane release-evidence validation FAILED:")
            for err in errors:
                print(f"  - {err}")
        else:
            print("pane release-evidence bundle is valid")
    return 1 if errors else 0


def _synthetic_release_fixture(*, cli_identity: bool = False) -> tuple[Path, Path, dict, dict, dict]:
    """Policy-test fixture; these fabricated observations are never live proof."""
    import pane_replay_artifacts as replay
    import pane_test_summary_aggregate as aggregate
    root = Path(tempfile.mkdtemp(prefix="pane-release-policy-fixture-"))
    results = root / "results"
    results.mkdir()
    source_root = Path(__file__).resolve().parent.parent
    for producer in ("scripts/pane_release_evidence.py", "scripts/pane_test_summary_aggregate.py",
                     "scripts/pane_replay_artifacts.py"):
        dest = root / producer
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes((source_root / producer).read_bytes())
    for spec in DIMENSIONS.values():
        for rel in spec["static"]:
            path = root / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"synthetic policy fixture: {rel}\n")
    golden = {"schema": replay.GOLDEN_SCHEMA, "schema_version": replay.GOLDEN_SCHEMA_VERSION,
              "scenarios": {"selftest-scenario": {"baseline_hash": 111, "final_hash": 222}}}
    golden_path = root / "scripts/pane_replay_golden.json"
    golden_path.write_text(json.dumps(golden))
    expected = {"run_id": "synthetic-policy-unit-test", "commit": "a" * 40,
                "tree": "b" * 40, "dirty": False, "lock_sha256": "c" * 64,
                "toolchain_sha256": "d" * 64, "compiler": "synthetic unit fixture",
                "target": "synthetic-test-target", "features": ["default"]}
    if cli_identity:
        # Exercise the unmodified CLI identity lookup against a tiny committed
        # fixture repository. Its fabricated suite results remain unit tests.
        for rel in ("Cargo.lock", "rust-toolchain.toml"):
            (root / rel).write_bytes((source_root / rel).read_bytes())
        subprocess.run(["git", "init", "--quiet", "--initial-branch=main", str(root)], check=True)
        subprocess.run(["git", "add", "-f", "scripts", "docs", "tests", "Cargo.lock", "rust-toolchain.toml"], cwd=root, check=True)
        subprocess.run(["git", "-c", "user.name=Policy Fixture", "-c", "user.email=fixture@localhost",
                        "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/nonexistent",
                        "commit", "--quiet", "-m", "Synthetic release policy fixture"], cwd=root, check=True)
        expected = build_identity(root, "synthetic-policy-unit-test")
    def receipt(producer, schema, version):
        return {"identity": dict(expected), "observed_at": int(time.time()), "producer": producer,
                "producer_sha256": sha256_file(root / producer), "schema": schema, "schema_version": version}
    summary = {}
    binary = results / "synthetic-unit-binary"
    binary.write_bytes(b"synthetic policy fixture, not an executed test binary")
    binary_ref = archive_binary(binary, results)
    for spec in DIMENSIONS.values():
        for crate, target in spec["suites"]:
            log = results / f"{crate}__{target}.log"
            log.write_text("test result: ok. 3 passed; 0 failed; 0 ignored;\n")
            key = f"{crate}::{target}"
            record = aggregate.capture(log.read_text(), crate, target, 0)[key]
            record.update({"provenance": receipt("scripts/pane_test_summary_aggregate.py", aggregate.SCHEMA, aggregate.SCHEMA_VERSION),
                           "log": {"path": log.name, "sha256": sha256_file(log)},
                           "binary": dict(binary_ref),
                           "command": ["cargo", "test", "-p", crate, "--test", target, "--", "--nocapture"]})
            summary[key] = record
    summary["_meta"] = {"schema": aggregate.SCHEMA, "schema_version": aggregate.SCHEMA_VERSION}
    differential = results / "differential_summary.json"
    differential.write_text(json.dumps({"_meta": {"schema": aggregate.SCHEMA, "schema_version": aggregate.SCHEMA_VERSION},
                                        "ftui-layout::pane_determinism_matrix": summary["ftui-layout::pane_determinism_matrix"]}))
    replay._write_synthetic_bundle(results, symbolization_ready=True)
    index = replay.build_index(results, test_mode=True, perf_stat=False, stack_reports=False, runner="synthetic-policy-test")
    index["provenance"] = receipt("scripts/pane_replay_artifacts.py", replay.SCHEMA, replay.SCHEMA_VERSION)
    index_path = results / replay.INDEX_FILENAME
    index_path.write_text(json.dumps(index))
    cert = replay.build_certification(index, golden, matrix_passed=True)
    cert["provenance"] = receipt("scripts/pane_replay_artifacts.py", replay.DIFF_CERT_SCHEMA, replay.DIFF_CERT_SCHEMA_VERSION)
    cert["inputs"] = {"index_sha256": sha256_file(index_path), "golden_sha256": sha256_file(golden_path),
                      "differential_summary": {"path": differential.name, "sha256": sha256_file(differential)}}
    (results / replay.DIFF_CERT_FILENAME).write_text(json.dumps(cert))
    bundle, errors = collect(root, results, summary,
                             run_provenance=receipt("scripts/pane_release_evidence.py", SCHEMA, SCHEMA_VERSION))
    assert not errors, errors
    return root, results, expected, bundle, cert


def cmd_selftest(_args: argparse.Namespace) -> int:
    """Round-trip the contract against a synthetic repo + results tree."""
    failures: list[str] = []
    # Retain fixtures for failure triage; no automatic file deletion.
    with nullcontext(tempfile.mkdtemp(prefix="pane-evidence-selftest-")) as tmp:
        root = Path(tmp)
        # Materialize every declared static artifact + scripts.
        for spec in DIMENSIONS.values():
            for rel in spec["static"]:
                p = root / rel
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text(f"synthetic::{rel}\n")
        # Materialize runtime artifacts in a results dir.
        results = root / "_results"
        results.mkdir()
        for spec in DIMENSIONS.values():
            for rel in spec["runtime"]:
                (results / rel).write_text(f"synthetic-runtime::{rel}\n")
        # A green test summary for every suite.
        summary: dict[str, Any] = {}
        for spec in DIMENSIONS.values():
            for crate, target in spec["suites"]:
                summary[f"{crate}::{target}"] = {"passed": 7, "failed": 0,
                                                 "verdict": "ok", "exit_code": 0}
        summary["_meta"] = {"schema": "ftui.pane.test_summary", "schema_version": 2}

        bundle, errors = collect(root, results, summary)
        if errors:
            failures.append(f"collect reported errors on complete tree: {errors}")

        # Full bundle must validate, including strict runtime + checksums.
        verrs = validate_bundle(bundle, require_runtime=True)
        verrs.extend(validate_checksums(bundle, root, results))
        if verrs:
            failures.append(f"complete bundle failed validation: {verrs}")

        if not bundle["overall"]["static_complete"]:
            failures.append("static_complete should be True on full tree")
        if not bundle["overall"]["runtime_complete"]:
            failures.append("runtime_complete should be True on full tree")
        if not bundle["overall"]["no_red_suites"]:
            failures.append("no_red_suites should be True with green summary")

        # Negative: unavailable root -> validation must catch vanished files.
        cerrs = validate_checksums(bundle, root / "absent-root", results)
        if not any("vanished" in e for e in cerrs):
            failures.append("checksum validation did not catch a vanished artifact")

        # Negative: a red suite -> validation must flag it.
        red_summary = dict(summary)
        a_key = next(iter(red_summary))
        red_summary[a_key] = {"passed": 1, "failed": 3, "verdict": "FAILED", "exit_code": 101}
        red_bundle, _ = collect(root, results, red_summary)
        if not any("red suite" in e for e in validate_bundle(red_bundle, False)):
            failures.append("validation did not catch a red suite")

        # Negative: missing runtime under strict mode.
        empty_results = root / "_empty"
        empty_results.mkdir()
        sparse, _ = collect(root, empty_results, summary)
        if not any(
            "missing runtime artifact" in e
            for e in validate_bundle(sparse, require_runtime=True)
        ):
            failures.append("strict runtime validation did not catch missing artifact")

    if failures:
        print("SELFTEST FAILED:")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("pane_release_evidence selftest: OK")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)

    p_collect = sub.add_parser("collect", help="build the release-evidence bundle")
    p_collect.add_argument("--out", required=True, help="output bundle path")
    p_collect.add_argument("--results-dir", help="CI run dir with runtime artifacts")
    p_collect.add_argument("--test-summary", help="JSON of per-suite pass/fail counts")
    p_collect.add_argument("--repo-root", help="repo root (default: inferred)")
    p_collect.add_argument("--strict", action="store_true", help="fail on missing static artifacts")
    p_collect.add_argument("--json", action="store_true")
    p_collect.set_defaults(func=cmd_collect)

    p_validate = sub.add_parser("validate", help="validate a bundle")
    p_validate.add_argument("--bundle", required=True)
    p_validate.add_argument("--repo-root", help="repo root (default: inferred)")
    p_validate.add_argument("--results-dir", help="root for runtime artifact references")
    p_validate.add_argument("--require-runtime", action="store_true")
    p_validate.add_argument("--no-checksums", action="store_true")
    p_validate.add_argument("--json", action="store_true")
    p_validate.set_defaults(func=cmd_validate)

    p_selftest = sub.add_parser("selftest", help="self-contained contract test")
    p_selftest.set_defaults(func=cmd_selftest)

    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except (ValueError, OSError) as exc:
        print(json.dumps({"ok": False, "errors": [str(exc)]}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

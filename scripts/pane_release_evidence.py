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
import hashlib
import json
import sys
import tempfile
from pathlib import Path
from typing import Any

SCHEMA = "ftui.pane.release_evidence"
SCHEMA_VERSION = 1

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
        "description": "terminal PTY + web drag/resize/keyboard flows",
        "suites": [
            ("ftui-harness", "pane_input_pty_e2e"),
            ("ftui-harness", "pane_splitter_drag_pty_e2e"),
            ("ftui-web", "pane_web_e2e"),
        ],
        "static": ["scripts/pane_e2e.sh"],
        "runtime": [],
    },
    "parity": {
        "description": "cross-host (terminal vs web) observational identity",
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
        rec["passed"] = int(observed.get("passed", 0))
        rec["failed"] = int(observed.get("failed", 0))
        rec["status"] = "green" if rec["failed"] == 0 and rec["passed"] > 0 else "red"
    else:
        rec["status"] = "declared"
    return rec


def collect(
    repo_root: Path,
    results_dir: Path | None,
    summary: dict[str, Any],
) -> tuple[dict[str, Any], list[str]]:
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
                matches = sorted(results_dir.rglob(rel))
                if matches:
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
        summary = json.loads(summary_path.read_text())

    bundle, errors = collect(repo_root, results_dir, summary)

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


def validate_bundle(bundle: dict[str, Any], require_runtime: bool) -> list[str]:
    errors: list[str] = []
    if bundle.get("schema") != SCHEMA:
        errors.append(f"schema mismatch: expected {SCHEMA!r}, got {bundle.get('schema')!r}")
    if bundle.get("schema_version") != SCHEMA_VERSION:
        errors.append(
            f"schema_version mismatch: expected {SCHEMA_VERSION}, got "
            f"{bundle.get('schema_version')}"
        )
    dims = bundle.get("dimensions", {})
    for name in DIMENSIONS:
        if name not in dims:
            errors.append(f"missing dimension: {name}")
            continue
        dim = dims[name]
        if not dim.get("suites"):
            errors.append(f"[{name}] no suites recorded")
        for s in dim.get("suites", []):
            if s.get("status") == "red":
                errors.append(f"[{name}] red suite: {s.get('crate')}::{s.get('target')}")
        for art in dim.get("static_artifacts", []):
            if not art.get("present"):
                errors.append(f"[{name}] missing static artifact: {art.get('path')}")
            elif "sha256" not in art:
                errors.append(f"[{name}] static artifact missing checksum: {art.get('path')}")
        if require_runtime:
            for art in dim.get("runtime_artifacts", []):
                if not art.get("present"):
                    errors.append(f"[{name}] missing runtime artifact: {art.get('name')}")
    return errors


def validate_checksums(bundle: dict[str, Any], repo_root: Path) -> list[str]:
    errors: list[str] = []
    for name, dim in bundle.get("dimensions", {}).items():
        for art in dim.get("static_artifacts", []):
            if not art.get("present"):
                continue
            target = repo_root / art["path"]
            if not target.is_file():
                errors.append(f"[{name}] static artifact vanished: {art['path']}")
                continue
            actual = sha256_file(target)
            if actual != art.get("sha256"):
                errors.append(
                    f"[{name}] checksum drift for {art['path']} "
                    f"(bundle={art.get('sha256')}, actual={actual})"
                )
    return errors


def cmd_validate(args: argparse.Namespace) -> int:
    bundle_path = Path(args.bundle)
    if not bundle_path.is_file():
        print(f"error: bundle not found: {bundle_path}", file=sys.stderr)
        return 2
    bundle = json.loads(bundle_path.read_text())

    errors = validate_bundle(bundle, require_runtime=args.require_runtime)
    if not args.no_checksums:
        errors.extend(validate_checksums(bundle, _repo_root(args.repo_root)))

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


def cmd_selftest(_args: argparse.Namespace) -> int:
    """Round-trip the contract against a synthetic repo + results tree."""
    failures: list[str] = []
    with tempfile.TemporaryDirectory() as tmp:
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
                summary[f"{crate}::{target}"] = {"passed": 7, "failed": 0}

        bundle, errors = collect(root, results, summary)
        if errors:
            failures.append(f"collect reported errors on complete tree: {errors}")

        # Full bundle must validate, including strict runtime + checksums.
        verrs = validate_bundle(bundle, require_runtime=True)
        verrs.extend(validate_checksums(bundle, root))
        if verrs:
            failures.append(f"complete bundle failed validation: {verrs}")

        if not bundle["overall"]["static_complete"]:
            failures.append("static_complete should be True on full tree")
        if not bundle["overall"]["runtime_complete"]:
            failures.append("runtime_complete should be True on full tree")
        if not bundle["overall"]["no_red_suites"]:
            failures.append("no_red_suites should be True with green summary")

        # Negative: drop one static artifact -> validation must catch it.
        first_static = next(
            rel for spec in DIMENSIONS.values() for rel in spec["static"]
        )
        (root / first_static).unlink()
        cerrs = validate_checksums(bundle, root)
        if not any("vanished" in e for e in cerrs):
            failures.append("checksum validation did not catch a vanished artifact")

        # Negative: a red suite -> validation must flag it.
        red_summary = dict(summary)
        a_key = next(iter(red_summary))
        red_summary[a_key] = {"passed": 1, "failed": 3}
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
    p_validate.add_argument("--require-runtime", action="store_true")
    p_validate.add_argument("--no-checksums", action="store_true")
    p_validate.add_argument("--json", action="store_true")
    p_validate.set_defaults(func=cmd_validate)

    p_selftest = sub.add_parser("selftest", help="self-contained contract test")
    p_selftest.set_defaults(func=cmd_selftest)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())

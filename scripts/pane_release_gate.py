#!/usr/bin/env python3
"""Pane release go/no-go gate (bd-1w0w4.5).

Turns the checksummed release-evidence bundle
(``pane_release_evidence.json``, see ``pane_release_evidence.py`` / bd-1w0w4.7)
into an **objective, automated** go/no-go decision across the release
dimensions: correctness (unit + e2e), parity, perf, accessibility, and
observability. The gate is deliberately mechanical -- a release is blocked
whenever any mandatory clause fails. No subjective "looks good to me" ships the
pane system.

Two modes:

``advisory``  Pre-merge / local. Requires the evidence to be structurally
              complete and free of observed red suites, but does NOT require the
              CI-only runtime artifacts (replay index, certification, soak).
              This is the gate a PR can pass before the full perf job runs.

``strict``    Release. Additionally requires every runtime artifact present and
              the differential certification to read ``certified`` -- the bar a
              build must clear to be tagged/shipped.

Inputs: the evidence bundle, and optionally the differential certification JSON
(``differential_certification.json``) so the perf clause can assert behavioral
certification rather than mere artifact presence.

Output: ``pane_release_gate.json`` (schema ``ftui.pane.release_gate`` v1) with a
per-clause breakdown and an overall ``GO``/``NO-GO`` verdict. Exit 0 on GO,
1 on NO-GO, 2 on usage error.
"""

from __future__ import annotations

import argparse
import copy
import json
import os
import shutil
import subprocess
import sys
import time
from functools import partial
from pathlib import Path
from typing import Any, Callable

import pane_release_evidence as evidence

SCHEMA = "ftui.pane.release_gate"
SCHEMA_VERSION = 1

CORRECTNESS_DIMS = ("unit", "e2e")
ALL_DIMS = tuple(evidence.DIMENSIONS)


class Clause:
    """One gate condition. ``required_in`` lists the modes where failing it is
    a hard block; in other modes it is reported but advisory."""

    def __init__(
        self,
        name: str,
        description: str,
        required_in: tuple[str, ...],
        check: Callable[[dict[str, Any], dict[str, Any] | None], tuple[bool, str]],
    ) -> None:
        self.name = name
        self.description = description
        self.required_in = required_in
        self.check = check


def _dim(bundle: dict[str, Any], name: str) -> dict[str, Any]:
    return bundle.get("dimensions", {}).get(name, {})


def _dim_no_red(bundle: dict[str, Any], names: tuple[str, ...]) -> tuple[bool, str]:
    red = []
    for n in names:
        for s in _dim(bundle, n).get("suites", []):
            if s.get("status") == "red":
                red.append(f"{n}:{s.get('crate')}::{s.get('target')}")
    return (not red, "no red suites" if not red else f"red: {', '.join(red)}")


def _dim_static_present(bundle: dict[str, Any], name: str) -> tuple[bool, str]:
    arts = _dim(bundle, name).get("static_artifacts", [])
    missing = [a.get("path") for a in arts if not a.get("present")]
    return (not missing, "static present" if not missing else f"missing: {missing}")


def _dim_runtime_present(bundle: dict[str, Any], name: str) -> tuple[bool, str]:
    arts = _dim(bundle, name).get("runtime_artifacts", [])
    missing = [a.get("name") for a in arts if not a.get("present")]
    return (not missing, "runtime present" if not missing else f"missing: {missing}")


def _all_suites_observed_green(bundle: dict[str, Any]) -> tuple[bool, str]:
    """GA clause (bd-nqxa5): every declared suite must be OBSERVED green.

    ``declared`` means the suite's results were never aggregated into the
    bundle (they ran — and were gated — in some other CI job, but this bundle
    cannot prove it). The GA gate consumes the cross-job aggregation from
    ``pane_test_summary_aggregate.py`` via ``pane_release_evidence.py
    --test-summary``, so a ``declared`` suite here means the aggregation is
    incomplete and the bundle must not assert end-to-end green.
    """
    not_green = []
    for name in ALL_DIMS:
        for s in _dim(bundle, name).get("suites", []):
            if s.get("status") != "green":
                status = s.get("status", "absent")
                not_green.append(f"{name}:{s.get('crate')}::{s.get('target')}={status}")
    return (
        not not_green,
        "every suite observed green"
        if not not_green
        else f"not observed green: {', '.join(not_green)}",
    )


def _build_clauses() -> list[Clause]:
    def all_dims(bundle, _cert):
        present = list(bundle.get("dimensions", {}).keys())
        missing = [d for d in ALL_DIMS if d not in present]
        return (not missing, "all 6 dimensions present" if not missing else f"missing: {missing}")

    def correctness(bundle, _cert):
        ok, detail = _dim_no_red(bundle, CORRECTNESS_DIMS)
        sok, sdetail = _dim_static_present(bundle, "e2e")
        return (ok and sok, f"{detail}; {sdetail}")

    def parity(bundle, _cert):
        ok, detail = _dim_no_red(bundle, ("parity",))
        sok, sdetail = _dim_static_present(bundle, "parity")
        return (ok and sok, f"{detail}; contract {sdetail}")

    def accessibility(bundle, _cert):
        ok, detail = _dim_no_red(bundle, ("a11y",))
        sok, sdetail = _dim_static_present(bundle, "a11y")
        return (ok and sok, f"{detail}; matrix {sdetail}")

    def observability(bundle, _cert):
        ok, detail = _dim_no_red(bundle, ("logging",))
        sok, sdetail = _dim_static_present(bundle, "logging")
        return (ok and sok, f"{detail}; schema {sdetail}")

    def perf_suites(bundle, _cert):
        return _dim_no_red(bundle, ("perf",))

    def perf_runtime(bundle, _cert):
        return _dim_runtime_present(bundle, "perf")

    def perf_certified(_bundle, cert):
        if not isinstance(cert, dict):
            return (False, "no differential_certification.json provided")
        classification = cert.get("classification")
        return (
            classification == "certified",
            f"certification classification = {classification!r}",
        )

    return [
        Clause("all_dimensions_present", "every release dimension is in the bundle",
               ("advisory", "strict"), all_dims),
        Clause("correctness", "unit + e2e suites green, e2e harness present",
               ("advisory", "strict"), correctness),
        Clause("parity", "cross-host parity suite green + contract present",
               ("advisory", "strict"), parity),
        Clause("accessibility", "a11y suites green + compliance matrix present",
               ("advisory", "strict"), accessibility),
        Clause("observability", "logging suites green + schema/traceability present",
               ("advisory", "strict"), observability),
        Clause("perf_suites", "perf/soak suites green", ("advisory", "strict"), perf_suites),
        Clause("perf_runtime_artifacts", "replay index + differential certification present",
               ("strict",), perf_runtime),
        Clause("perf_certified", "differential certification reads 'certified'",
               ("strict",), perf_certified),
        Clause("suites_observed_green",
               "every declared suite observed green via cross-job aggregation",
               ("ga",), lambda bundle, _cert: _all_suites_observed_green(bundle)),
    ]


def evaluate(
    bundle: dict[str, Any], cert: dict[str, Any] | None, mode: str, *,
    repo_root: Path | None = None, results_dir: Path | None = None,
    expected_identity: dict[str, Any] | None = None,
) -> dict[str, Any]:
    clauses = _build_clauses()
    errors = evidence.validate_bundle(bundle, require_runtime=mode in ("strict", "ga"),
                                      require_observed=mode == "ga")
    if mode not in ("advisory", "strict", "ga"):
        errors.append(f"unknown gate mode: {mode!r}")
    results = [{"clause": "evidence_contract", "description": "canonical typed evidence inventory",
                "passed": not errors, "required": True,
                "detail": "; ".join(errors) if errors else "canonical evidence contract satisfied"}]
    blocking_failures = ["evidence_contract"] if errors else []
    if not errors:
        root = repo_root or Path(__file__).resolve().parent.parent
        checked_bundle = bundle
        if mode == "advisory" and results_dir is None:
            # Advisory evaluates local static evidence. It does not claim
            # verification of runtime artifacts in the absent CI run root.
            checked_bundle = {"dimensions": {
                name: {**dim, "runtime_artifacts": []}
                for name, dim in bundle["dimensions"].items()
            }}
        digest_errors = evidence.validate_checksums(checked_bundle, root, results_dir)
        results.append({"clause": "artifact_checksums", "description": "verify referenced artifact bytes",
                        "passed": not digest_errors, "required": True,
                        "detail": "; ".join(digest_errors) if digest_errors else "artifact hashes verified"})
        if digest_errors:
            blocking_failures.append("artifact_checksums")
        if mode in ("strict", "ga"):
            try:
                expected = expected_identity if expected_identity is not None else evidence.build_identity(
                    root, os.environ.get("PANE_RELEASE_RUN_ID", ""))
                provenance_errors = evidence.validate_release_provenance(bundle, cert, root, results_dir, expected)
            except (ValueError, OSError) as exc:
                provenance_errors = [str(exc)]
            results.append({"clause": "release_provenance", "description": "same-build artifact and certificate binding",
                            "passed": not provenance_errors, "required": True,
                            "detail": "; ".join(provenance_errors) if provenance_errors else "producer identities and certificate inputs verified"})
            if provenance_errors:
                blocking_failures.append("release_provenance")
    for c in clauses:
        passed, detail = (False, "not evaluated: invalid evidence contract") if errors else c.check(bundle, cert)
        # "ga" is a strict superset: everything strict requires, plus the
        # ga-only clauses (observed-green suite aggregation, bd-nqxa5).
        required = mode in c.required_in or (mode == "ga" and "strict" in c.required_in)
        results.append(
            {
                "clause": c.name,
                "description": c.description,
                "passed": passed,
                "required": required,
                "detail": detail,
            }
        )
        if required and not passed:
            blocking_failures.append(c.name)

    verdict = "GO" if not blocking_failures else "NO-GO"
    return {
        "schema": SCHEMA,
        "schema_version": SCHEMA_VERSION,
        "feature": "pane-workspace",
        "scope": evidence.SCOPE,
        "mode": mode,
        "verdict": verdict,
        "blocking_failures": blocking_failures,
        "clauses": results,
    }


def cmd_evaluate(args: argparse.Namespace) -> int:
    bundle_path = Path(args.bundle)
    if not bundle_path.is_file():
        print(f"error: bundle not found: {bundle_path}", file=sys.stderr)
        return 2
    bundle = evidence.load_json(bundle_path)

    cert = None
    if args.certification:
        cert_path = Path(args.certification)
        if not cert_path.is_file():
            print(f"error: --certification not found: {cert_path}", file=sys.stderr)
            return 2
        cert = evidence.load_json(cert_path)
        if not isinstance(cert, dict):
            raise ValueError("certification must be an object")

    decision = evaluate(bundle, cert, args.mode,
                        repo_root=Path(args.repo_root).resolve() if args.repo_root else None,
                        results_dir=Path(args.results_dir).resolve() if args.results_dir else None)

    if args.out:
        out_path = Path(args.out)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(decision, indent=2, sort_keys=True) + "\n")

    if args.json:
        print(json.dumps(decision, indent=2, sort_keys=True))
    else:
        print(f"pane release gate ({args.mode}): {decision['verdict']}")
        for r in decision["clauses"]:
            mark = "PASS" if r["passed"] else ("BLOCK" if r["required"] else "warn")
            print(f"  [{mark}] {r['clause']}: {r['detail']}")
        if decision["blocking_failures"]:
            print(f"  blocked by: {', '.join(decision['blocking_failures'])}")

    return 0 if decision["verdict"] == "GO" else 1


def cmd_selftest(_args: argparse.Namespace) -> int:
    failures: list[str] = []
    root, run, expected, original, certified = evidence._synthetic_release_fixture(cli_identity=True)
    decide = partial(evaluate, repo_root=root, results_dir=run, expected_identity=expected)

    def green_bundle() -> dict[str, Any]:
        return copy.deepcopy(original)

    drift = {"classification": "semantic_drift"}

    # advisory GO with a fully green bundle, even without certification.
    adv = decide(green_bundle(), None, "advisory")
    if adv["verdict"] != "GO":
        failures.append(f"advisory full-green should GO, got {adv['verdict']} ({adv['blocking_failures']})")

    # strict GO requires certification == certified.
    strict_ok = decide(green_bundle(), certified, "strict")
    if strict_ok["verdict"] != "GO":
        failures.append(f"strict green+certified should GO, got {strict_ok['blocking_failures']}")

    # strict NO-GO when certification missing.
    strict_nocert = decide(green_bundle(), None, "strict")
    if strict_nocert["verdict"] != "NO-GO" or "perf_certified" not in strict_nocert["blocking_failures"]:
        failures.append("strict without certification should NO-GO on perf_certified")

    # strict NO-GO on semantic drift.
    strict_drift = decide(green_bundle(), drift, "strict")
    if strict_drift["verdict"] != "NO-GO":
        failures.append("strict with semantic_drift should NO-GO")

    # a red suite blocks even advisory.
    red = green_bundle()
    red["dimensions"]["unit"]["suites"][0]["status"] = "red"
    if decide(red, None, "advisory")["verdict"] != "NO-GO":
        failures.append("red unit suite should NO-GO advisory")

    # missing dimension blocks.
    miss = green_bundle()
    del miss["dimensions"]["parity"]
    if "all_dimensions_present" not in decide(miss, certified, "strict")["blocking_failures"]:
        failures.append("missing dimension should block")

    # missing runtime artifact blocks strict but not advisory.
    nort = green_bundle()
    nort["dimensions"]["perf"]["runtime_artifacts"][0]["present"] = False
    if decide(nort, certified, "advisory")["verdict"] != "GO":
        failures.append("missing perf runtime should NOT block advisory")
    if "perf_runtime_artifacts" not in decide(nort, certified, "strict")["blocking_failures"]:
        failures.append("missing perf runtime should block strict")

    # ga: observed-green everywhere + certified => GO.
    ga_ok = decide(green_bundle(), certified, "ga")
    if ga_ok["verdict"] != "GO":
        failures.append(f"ga fully-observed-green should GO, got {ga_ok['blocking_failures']}")

    # ga: a 'declared' suite passes strict but must block ga (bd-nqxa5).
    declared = green_bundle()
    declared["dimensions"]["a11y"]["suites"][0] = {
        "crate": "ftui-demo-showcase", "target": "pane_a11y_compliance_a11y", "status": "declared",
    }
    if decide(declared, certified, "strict")["verdict"] != "GO":
        failures.append("declared suite should still pass strict")
    ga_declared = decide(declared, certified, "ga")
    if ga_declared["verdict"] != "NO-GO" or "suites_observed_green" not in ga_declared["blocking_failures"]:
        failures.append("declared suite should block ga on suites_observed_green")

    # ga inherits every strict requirement (no certification => NO-GO).
    ga_nocert = decide(green_bundle(), None, "ga")
    if "perf_certified" not in ga_nocert["blocking_failures"]:
        failures.append("ga must inherit strict's perf_certified requirement")

    # The real CLI runs against a committed synthetic policy fixture. These
    # are adversarial tests of the gate, not live product/host acceptance.
    case_dir = run / "policy-cases"
    case_dir.mkdir()
    env = {**os.environ, "PANE_RELEASE_RUN_ID": expected["run_id"]}
    case_count = 0
    def cli_case(name, bundle, *, should_pass=False, results=run, raw=None):
        nonlocal case_count
        case_count += 1
        path = case_dir / f"{case_count:03}-{name}.json"
        path.write_text(json.dumps(bundle) if raw is None else raw)
        result = subprocess.run([sys.executable, str(Path(__file__).resolve()), "evaluate",
                                 "--bundle", str(path), "--certification", str(results / "differential_certification.json"),
                                 "--repo-root", str(root), "--results-dir", str(results),
                                 "--mode", "ga", "--json"], env=env, text=True, capture_output=True)
        (case_dir / f"{case_count:03}-{name}.stdout").write_text(result.stdout)
        (case_dir / f"{case_count:03}-{name}.stderr").write_text(result.stderr)
        try:
            decision = json.loads(result.stdout)
        except ValueError:
            failures.append(f"{name}: no machine-readable decision (exit {result.returncode}): {result.stderr}")
            return
        if should_pass:
            if result.returncode != 0 or decision.get("verdict") != "GO":
                failures.append(f"{name}: clean positive CLI failed: {decision}")
        elif result.returncode != 1 or decision.get("verdict") != "NO-GO" or not decision.get("blocking_failures"):
            failures.append(f"{name}: adversarial CLI input was not rejected: {decision}")

    cli_case("clean-generated-policy-fixture", original, should_pass=True)
    for name in ALL_DIMS:
        missing = green_bundle()
        del missing["dimensions"][name]
        cli_case(f"missing-dimension-{name}", missing)
        for index in range(len(original["dimensions"][name]["suites"])):
            missing = green_bundle()
            del missing["dimensions"][name]["suites"][index]
            cli_case(f"missing-suite-{name}-{index}", missing)
            duplicate = green_bundle()
            duplicate["dimensions"][name]["suites"].append(copy.deepcopy(duplicate["dimensions"][name]["suites"][index]))
            cli_case(f"duplicate-suite-{name}-{index}", duplicate)
    for field, values in (("passed", [0, -1, True, "3", 3.0, None]),
                          ("failed", [-1, True, "0", 1]),
                          ("exit_code", [1, True, "0", None]),
                          ("verdict", ["FAILED", "skipped", True, None])):
        for index, value in enumerate(values):
            mutated = green_bundle()
            mutated["dimensions"]["unit"]["suites"][0][field] = value
            cli_case(f"invalid-{field}-{index}", mutated)
    for index, value in enumerate((None, [], True, "unit", {})):
        mutated = green_bundle()
        mutated["dimensions"]["unit"] = value
        cli_case(f"invalid-dimension-type-{index}", mutated)
    empty = green_bundle()
    empty["dimensions"] = {name: {} for name in ALL_DIMS}
    cli_case("original-vacuous-regression", empty)
    unknown = green_bundle()
    unknown["dimensions"]["bonus"] = {}
    cli_case("unknown-dimension", unknown)
    unknown = green_bundle()
    unknown["dimensions"]["unit"]["suites"][0]["target"] = "undeclared-suite"
    cli_case("unknown-suite", unknown)
    for index, version in enumerate((True, 0, 999, "2")):
        mutated = green_bundle()
        mutated["schema_version"] = version
        cli_case(f"unknown-schema-version-{index}", mutated)
    for field in expected:
        mutated = green_bundle()
        mutated["provenance"]["identity"][field] = "substituted"
        cli_case(f"substituted-{field}", mutated)
    for name, timestamp in (("future", int(time.time()) + 3600), ("stale", int(time.time()) - 86401)):
        mutated = green_bundle()
        mutated["provenance"]["observed_at"] = timestamp
        cli_case(name, mutated)
    for kind in ("static", "runtime"):
        mutated = green_bundle()
        mutated["dimensions"]["perf"][f"{kind}_artifacts"][0]["sha256"] = "0" * 64
        cli_case(f"{kind}-digest-drift", mutated)
    for index, path in enumerate(("../escape", "/absolute", "nested/../../escape", "nested\\escape")):
        mutated = green_bundle()
        mutated["dimensions"]["perf"]["runtime_artifacts"][0]["path"] = path
        cli_case(f"path-escape-{index}", mutated)
    mutated = green_bundle()
    mutated["dimensions"]["unit"]["suites"][0]["binary"]["executable_sha256"] = "0" * 64
    cli_case("substituted-executable", mutated)
    cli_case("duplicate-json-identity", original, raw='{"schema": "old", "schema": "ftui.pane.release_evidence"}')
    cli_case("non-finite-json", original, raw='{"dimensions": NaN}')
    relocated = root / "relocated-run"
    shutil.copytree(run, relocated)
    cli_case("relocated-clean-run", original, should_pass=True, results=relocated)
    # Mutate archived bytes after collection, leaving the recorded digests intact.
    log_ref = original["dimensions"]["unit"]["suites"][0]["log"]
    (run / log_ref["path"]).write_text("tampered test output\n")
    cli_case("tampered-runtime-log", original)
    print(f"policy CLI cases={case_count}; synthetic fixture retained at {root}")

    if failures:
        print("SELFTEST FAILED:")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("pane_release_gate selftest: OK")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)

    p_eval = sub.add_parser("evaluate", help="render the go/no-go verdict")
    p_eval.add_argument("--bundle", required=True, help="pane_release_evidence.json")
    p_eval.add_argument("--certification", help="differential_certification.json")
    p_eval.add_argument("--repo-root", help="root for committed static artifacts")
    p_eval.add_argument("--results-dir", help="root for runtime artifact references (required for release)")
    p_eval.add_argument(
        "--mode",
        choices=("advisory", "strict", "ga"),
        default="advisory",
        help="ga = strict + every suite observed green via cross-job aggregation",
    )
    p_eval.add_argument("--out", help="write the decision JSON here")
    p_eval.add_argument("--json", action="store_true")
    p_eval.set_defaults(func=cmd_evaluate)

    p_self = sub.add_parser("selftest", help="self-contained policy test")
    p_self.set_defaults(func=cmd_selftest)

    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except (ValueError, OSError) as exc:
        print(json.dumps({"schema": SCHEMA, "schema_version": SCHEMA_VERSION,
                          "verdict": "NO-GO", "blocking_failures": ["input_json"],
                          "clauses": [{"clause": "input_json", "passed": False,
                                       "required": True, "detail": str(exc)}]}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

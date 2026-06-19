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
import json
import sys
from pathlib import Path
from typing import Any, Callable

SCHEMA = "ftui.pane.release_gate"
SCHEMA_VERSION = 1

CORRECTNESS_DIMS = ("unit", "e2e")
ALL_DIMS = ("unit", "e2e", "parity", "perf", "a11y", "logging")


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
        if cert is None:
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
    ]


def evaluate(
    bundle: dict[str, Any], cert: dict[str, Any] | None, mode: str
) -> dict[str, Any]:
    clauses = _build_clauses()
    results = []
    blocking_failures = []
    for c in clauses:
        passed, detail = c.check(bundle, cert)
        required = mode in c.required_in
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
    bundle = json.loads(bundle_path.read_text())
    if bundle.get("schema") != "ftui.pane.release_evidence":
        print(
            f"error: not a release-evidence bundle: schema={bundle.get('schema')!r}",
            file=sys.stderr,
        )
        return 2

    cert = None
    if args.certification:
        cert_path = Path(args.certification)
        if not cert_path.is_file():
            print(f"error: --certification not found: {cert_path}", file=sys.stderr)
            return 2
        cert = json.loads(cert_path.read_text())

    decision = evaluate(bundle, cert, args.mode)

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

    def green_bundle() -> dict[str, Any]:
        dims = {}
        for d in ALL_DIMS:
            dims[d] = {
                "suites": [{"crate": "c", "target": d, "status": "green",
                            "passed": 3, "failed": 0}],
                "static_artifacts": [{"path": f"{d}.json", "present": True, "sha256": "x"}],
                "runtime_artifacts": (
                    [{"name": "replay_artifact_index.json", "present": True},
                     {"name": "differential_certification.json", "present": True}]
                    if d == "perf" else []
                ),
            }
        return {"schema": "ftui.pane.release_evidence", "dimensions": dims}

    certified = {"classification": "certified"}
    drift = {"classification": "semantic_drift"}

    # advisory GO with a fully green bundle, even without certification.
    adv = evaluate(green_bundle(), None, "advisory")
    if adv["verdict"] != "GO":
        failures.append(f"advisory full-green should GO, got {adv['verdict']} ({adv['blocking_failures']})")

    # strict GO requires certification == certified.
    strict_ok = evaluate(green_bundle(), certified, "strict")
    if strict_ok["verdict"] != "GO":
        failures.append(f"strict green+certified should GO, got {strict_ok['blocking_failures']}")

    # strict NO-GO when certification missing.
    strict_nocert = evaluate(green_bundle(), None, "strict")
    if strict_nocert["verdict"] != "NO-GO" or "perf_certified" not in strict_nocert["blocking_failures"]:
        failures.append("strict without certification should NO-GO on perf_certified")

    # strict NO-GO on semantic drift.
    strict_drift = evaluate(green_bundle(), drift, "strict")
    if strict_drift["verdict"] != "NO-GO":
        failures.append("strict with semantic_drift should NO-GO")

    # a red suite blocks even advisory.
    red = green_bundle()
    red["dimensions"]["unit"]["suites"][0]["status"] = "red"
    if evaluate(red, None, "advisory")["verdict"] != "NO-GO":
        failures.append("red unit suite should NO-GO advisory")

    # missing dimension blocks.
    miss = green_bundle()
    del miss["dimensions"]["parity"]
    if "all_dimensions_present" not in evaluate(miss, certified, "strict")["blocking_failures"]:
        failures.append("missing dimension should block")

    # missing runtime artifact blocks strict but not advisory.
    nort = green_bundle()
    nort["dimensions"]["perf"]["runtime_artifacts"][0]["present"] = False
    if evaluate(nort, certified, "advisory")["verdict"] != "GO":
        failures.append("missing perf runtime should NOT block advisory")
    if "perf_runtime_artifacts" not in evaluate(nort, certified, "strict")["blocking_failures"]:
        failures.append("missing perf runtime should block strict")

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
    p_eval.add_argument("--mode", choices=("advisory", "strict"), default="advisory")
    p_eval.add_argument("--out", help="write the decision JSON here")
    p_eval.add_argument("--json", action="store_true")
    p_eval.set_defaults(func=cmd_evaluate)

    p_self = sub.add_parser("selftest", help="self-contained policy test")
    p_self.set_defaults(func=cmd_selftest)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())

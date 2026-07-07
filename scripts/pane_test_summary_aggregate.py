#!/usr/bin/env python3
"""Cross-job pane test-summary aggregation for the release gate (bd-nqxa5).

The pane release-evidence bundle (``pane_release_evidence.py``) records each
declared suite as ``"declared"`` unless an observed ``--test-summary`` maps
``"crate::target"`` to ``{"passed": N, "failed": M}``. Suite results normally
live in separate CI jobs, so the GA gate needs the summaries *aggregated
across jobs*. This tool provides that pipeline:

``capture``
    Parse a ``cargo test`` log into a single-suite summary fragment.
``merge``
    Merge many fragments (files or directories of ``*.json``) into one
    aggregated summary, refusing conflicting duplicates by default.
``check``
    Verify an aggregated summary covers every suite the release-evidence
    dimensions declare, and that every covered suite is green.
``selftest``
    Exercise capture/merge/check against synthetic inputs.

Typical CI flow: each suite job runs ``capture`` and uploads the fragment as
a workflow artifact; the gate job downloads all fragments, runs ``merge`` +
``check``, and passes the aggregate to
``pane_release_evidence.py collect --test-summary`` so the bundle asserts
every suite green end-to-end (consumed by ``pane_release_gate.py`` in ``ga``
mode).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
from pathlib import Path
from typing import Any

SCHEMA = "ftui.pane.test_summary"
SCHEMA_VERSION = 1

# The libtest summary line: "test result: ok. 12 passed; 0 failed; ..."
RESULT_RE = re.compile(
    r"^test result: (?P<verdict>\w+)\. (?P<passed>\d+) passed; (?P<failed>\d+) failed;",
    re.MULTILINE,
)


def _load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text())


def _fragment_paths(inputs: list[str]) -> list[Path]:
    paths: list[Path] = []
    for raw in inputs:
        p = Path(raw)
        if p.is_dir():
            paths.extend(sorted(p.rglob("*.json")))
        elif p.is_file():
            paths.append(p)
        else:
            raise FileNotFoundError(f"fragment not found: {raw}")
    return paths


def capture(log_text: str, crate: str, target: str) -> dict[str, Any]:
    """Parse a cargo-test log into a one-suite summary fragment.

    Multiple ``test result:`` lines (unit + doc tests) are summed; a log with
    none is an error (the suite never ran, which must not masquerade as
    green).
    """
    matches = list(RESULT_RE.finditer(log_text))
    if not matches:
        raise ValueError(
            f"no 'test result:' line found for {crate}::{target}; "
            "the suite did not run to completion"
        )
    passed = sum(int(m.group("passed")) for m in matches)
    failed = sum(int(m.group("failed")) for m in matches)
    return {
        "_meta": {
            "schema": SCHEMA,
            "schema_version": SCHEMA_VERSION,
            "sources": [f"{crate}::{target}"],
        },
        f"{crate}::{target}": {"passed": passed, "failed": failed},
    }


def merge(fragments: list[dict[str, Any]], on_conflict: str) -> dict[str, Any]:
    """Merge summary fragments into one aggregated summary.

    Duplicate suite keys with identical numbers are collapsed; disagreeing
    duplicates are an error (``on_conflict="error"``) or resolved toward the
    redder record (``on_conflict="worst"``) so a flaky re-run can never
    launder a red suite into green.
    """
    merged: dict[str, Any] = {}
    sources: list[str] = []
    for fragment in fragments:
        meta = fragment.get("_meta", {})
        sources.extend(meta.get("sources", []))
        for key, value in fragment.items():
            if key == "_meta":
                continue
            if "::" not in key:
                raise ValueError(f"malformed suite key (want crate::target): {key!r}")
            record = {
                "passed": int(value.get("passed", 0)),
                "failed": int(value.get("failed", 0)),
            }
            existing = merged.get(key)
            if existing is None or existing == record:
                merged[key] = record
                continue
            if on_conflict == "worst":
                merged[key] = {
                    "passed": min(existing["passed"], record["passed"]),
                    "failed": max(existing["failed"], record["failed"]),
                }
            else:
                raise ValueError(
                    f"conflicting summaries for {key}: {existing} vs {record} "
                    "(pass --on-conflict=worst to keep the redder record)"
                )
    merged["_meta"] = {
        "schema": SCHEMA,
        "schema_version": SCHEMA_VERSION,
        "sources": sorted(set(sources)),
    }
    return merged


def declared_suites() -> list[str]:
    """The suite keys the release-evidence bundle declares (import-shared)."""
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    try:
        import pane_release_evidence  # noqa: PLC0415  (sibling-script import)
    finally:
        sys.path.pop(0)
    keys: list[str] = []
    for spec in pane_release_evidence.DIMENSIONS.values():
        keys.extend(f"{crate}::{target}" for crate, target in spec["suites"])
    return sorted(set(keys))


def check(summary: dict[str, Any], require_all: bool) -> dict[str, Any]:
    """Cross-check an aggregated summary against the declared suite list."""
    declared = declared_suites()
    missing = [k for k in declared if k not in summary]
    red = []
    empty = []
    for key in declared:
        record = summary.get(key)
        if record is None:
            continue
        if int(record.get("failed", 0)) > 0:
            red.append(key)
        elif int(record.get("passed", 0)) == 0:
            empty.append(key)
    ok = not red and not empty and (not require_all or not missing)
    return {
        "schema": SCHEMA,
        "schema_version": SCHEMA_VERSION,
        "ok": ok,
        "declared": len(declared),
        "observed": len(declared) - len(missing),
        "missing": missing,
        "red": red,
        "empty": empty,
    }


# ----------------------------------------------------------------------------
# Commands
# ----------------------------------------------------------------------------


def cmd_capture(args: argparse.Namespace) -> int:
    log_path = Path(args.log)
    if not log_path.is_file():
        print(f"error: log not found: {log_path}", file=sys.stderr)
        return 2
    try:
        fragment = capture(log_path.read_text(errors="replace"), args.crate, args.target)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    out = json.dumps(fragment, indent=2, sort_keys=True) + "\n"
    if args.out:
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(out)
    if args.json or not args.out:
        print(out, end="")
    return 0


def cmd_merge(args: argparse.Namespace) -> int:
    try:
        paths = _fragment_paths(args.fragments)
        fragments = [_load_json(p) for p in paths]
        merged = merge(fragments, args.on_conflict)
    except (FileNotFoundError, ValueError, json.JSONDecodeError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    if not paths:
        print("error: no fragments given", file=sys.stderr)
        return 1
    out = json.dumps(merged, indent=2, sort_keys=True) + "\n"
    if args.out:
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(out)
    if args.json or not args.out:
        print(out, end="")
    return 0


def cmd_check(args: argparse.Namespace) -> int:
    summary_path = Path(args.summary)
    if not summary_path.is_file():
        print(f"error: summary not found: {summary_path}", file=sys.stderr)
        return 2
    report = check(_load_json(summary_path), args.require_all)
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["ok"] else 1


def cmd_selftest(_args: argparse.Namespace) -> int:
    failures: list[str] = []

    # capture: sums multiple result lines, rejects logs without one.
    log = (
        "running 3 tests\n...\n"
        "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured\n"
        "   Doc-tests demo\n"
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured\n"
    )
    frag = capture(log, "ftui-layout", "pane_margin")
    if frag["ftui-layout::pane_margin"] != {"passed": 4, "failed": 0}:
        failures.append(f"capture summed wrong: {frag}")
    try:
        capture("no summary here", "c", "t")
        failures.append("capture accepted a log without a result line")
    except ValueError:
        pass

    # merge: identical dupes collapse; conflicts error; worst keeps red.
    a = {"_meta": {"sources": ["a"]}, "c::t": {"passed": 5, "failed": 0}}
    b = {"_meta": {"sources": ["b"]}, "c::u": {"passed": 2, "failed": 0}}
    merged = merge([a, b, a], "error")
    if merged["c::t"] != {"passed": 5, "failed": 0} or merged["c::u"]["passed"] != 2:
        failures.append(f"merge wrong: {merged}")
    conflict = {"c::t": {"passed": 4, "failed": 1}}
    try:
        merge([a, conflict], "error")
        failures.append("merge accepted a conflicting duplicate")
    except ValueError:
        pass
    worst = merge([a, conflict], "worst")
    if worst["c::t"] != {"passed": 4, "failed": 1}:
        failures.append(f"worst-merge must keep the redder record: {worst}")

    # check: green-complete passes; red/missing/empty are named.
    complete = {k: {"passed": 1, "failed": 0} for k in declared_suites()}
    report = check(complete, require_all=True)
    if not report["ok"] or report["missing"]:
        failures.append(f"complete-green check failed: {report}")
    first = declared_suites()[0]
    red = dict(complete)
    red[first] = {"passed": 1, "failed": 2}
    report = check(red, require_all=True)
    if report["ok"] or report["red"] != [first]:
        failures.append(f"red suite not detected: {report}")
    partial = dict(complete)
    del partial[first]
    report = check(partial, require_all=True)
    if report["ok"] or report["missing"] != [first]:
        failures.append(f"missing suite not detected: {report}")
    if not check(partial, require_all=False)["ok"]:
        failures.append("partial summary must pass without --require-all")
    empty = dict(complete)
    empty[first] = {"passed": 0, "failed": 0}
    if check(empty, require_all=True)["ok"]:
        failures.append("zero-passed suite must not count as green")

    # round-trip through the CLI surface (tempdir).
    with tempfile.TemporaryDirectory() as tmp:
        frag_path = Path(tmp) / "frag.json"
        frag_path.write_text(json.dumps(frag))
        merged2 = merge([_load_json(p) for p in _fragment_paths([tmp])], "error")
        if "ftui-layout::pane_margin" not in merged2:
            failures.append("directory fragment discovery failed")

    if failures:
        for f in failures:
            print(f"SELFTEST FAIL: {f}", file=sys.stderr)
        return 1
    print(json.dumps({"selftest": "ok", "schema": SCHEMA}))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    p_capture = sub.add_parser("capture", help="parse a cargo test log into a fragment")
    p_capture.add_argument("--crate", required=True)
    p_capture.add_argument("--target", required=True)
    p_capture.add_argument("--log", required=True)
    p_capture.add_argument("--out")
    p_capture.add_argument("--json", action="store_true")
    p_capture.set_defaults(func=cmd_capture)

    p_merge = sub.add_parser("merge", help="merge fragments into one summary")
    p_merge.add_argument("fragments", nargs="+", help="fragment files or directories")
    p_merge.add_argument("--out")
    p_merge.add_argument("--on-conflict", choices=("error", "worst"), default="error")
    p_merge.add_argument("--json", action="store_true")
    p_merge.set_defaults(func=cmd_merge)

    p_check = sub.add_parser("check", help="verify coverage + greenness")
    p_check.add_argument("--summary", required=True)
    p_check.add_argument("--require-all", action="store_true")
    p_check.set_defaults(func=cmd_check)

    p_list = sub.add_parser(
        "list", help="print declared suites (crate<TAB>target), for CI loops"
    )
    p_list.set_defaults(
        func=lambda _a: (
            print("\n".join(k.replace("::", "\t") for k in declared_suites())) or 0
        )
    )

    p_self = sub.add_parser("selftest", help="run the built-in selftest")
    p_self.set_defaults(func=cmd_selftest)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Check every `criterion_name` in tests/baseline.json names a real benchmark.

Read-only, stdlib-only, JSONL output. bd-g00-root-epic-ewths.31.5 item 1.

A baseline row binds a performance budget to a criterion benchmark id. If that
id is one criterion never emits, the row cannot be evaluated — and because
`scripts/perf_regression_gate.sh` is fail-closed (`incomplete > 0` returns 3),
one wrong id makes the whole perf gate permanently INCOMPLETE. Not silent, but
stuck: no run can ever come back green while the row is there.

That is not hypothetical. `diff/sparse_5pct/200x60` is such a row today; see
`docs/baseline-bench-exceptions.txt`.

# How an id is resolved

Criterion composes ids as `<group>/<function>/<parameter>`, from
`c.benchmark_group("<group>")` and `BenchmarkId::new("<function>", <param>)`
(or `bench_function("<function>")`). Group names here contain slashes
themselves, so the split is ambiguous by eye: this resolves the *longest*
declared group that prefixes the id, then requires the next segment to be a
function declared in the same bench file.

Parameters are not checked. They are built at runtime from loop variables
(`format!("{w}x{h}")`) and reading them statically would mean interpreting Rust.
A wrong parameter still fails the real gate as `parse_failed`; this catches the
larger class, a group or function that does not exist at all.
"""

import argparse
import json
from pathlib import Path
import re
import sys


GROUP = re.compile(r'benchmark_group\(\s*"([^"]+)"')
BENCH_ID = re.compile(r'BenchmarkId::new\(\s*"([^"]+)"')
BENCH_FN = re.compile(r'bench_function\(\s*"([^"]+)"')


def baseline_rows(baseline):
    """Yield (key, row) for every object carrying a `criterion_name`."""
    def walk(node, key):
        if isinstance(node, dict):
            if "criterion_name" in node:
                yield key, node
            for name, value in node.items():
                yield from walk(value, name)
        elif isinstance(node, list):
            for value in node:
                yield from walk(value, key)
    yield from walk(baseline, "")


def declared(crates_dir):
    """Map declared group -> bench file, and (bench file, function) -> True."""
    groups, functions = {}, set()
    for source in sorted(crates_dir.glob("*/benches/*.rs")):
        text = source.read_text(encoding="utf-8", errors="replace")
        stem = source.stem
        for name in GROUP.findall(text):
            groups.setdefault(name, stem)
        for pattern in (BENCH_ID, BENCH_FN):
            for name in pattern.findall(text):
                functions.add((stem, name))
    return groups, functions


def resolve(criterion_name, groups, functions):
    """Return None when the id resolves, else why it does not."""
    candidates = [g for g in groups
                  if criterion_name == g or criterion_name.startswith(g + "/")]
    if not candidates:
        return "no_such_group"
    group = max(candidates, key=len)
    remainder = criterion_name[len(group):].lstrip("/")
    if not remainder:
        return None
    function = remainder.split("/")[0]
    if (groups[group], function) in functions:
        return None
    return "no_such_bench_function"


def load_exceptions(path):
    """Known-unresolved ids, each requiring an open bead id on the line.

    Same contract as `docs/module-reachability-allowlist.txt`: an entry exists
    to name who will fix it, not to make the gate quiet. The bead id is not
    verified here against `.beads/issues.jsonl` — that check belongs with the
    other ledger gates, and is worth adding when this list has more than the one
    entry it was created for.
    """
    if not path.is_file():
        return {}
    entries = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        criterion, _, bead = line.partition(" ")
        entries[criterion.strip()] = bead.strip()
    return entries


def check(root):
    baseline = json.loads((root / "tests/baseline.json").read_text(encoding="utf-8"))
    groups, functions = declared(root / "crates")
    exceptions = load_exceptions(root / "docs/baseline-bench-exceptions.txt")

    records, failures, excepted = [], 0, 0
    for key, row in baseline_rows(baseline):
        criterion = row.get("criterion_name") or ""
        if not criterion:
            continue
        reason = resolve(criterion, groups, functions)
        if reason is None:
            continue
        if criterion in exceptions:
            excepted += 1
            records.append(dict(kind="excepted", key=key, criterion_name=criterion,
                                reason=reason, bead=exceptions[criterion],
                                status="known"))
            continue
        failures += 1
        records.append(dict(kind="row", key=key, criterion_name=criterion,
                            reason=reason, bench_file=row.get("bench_file"),
                            slo_metric=row.get("slo_metric"), status="failed"))
    return records, failures, excepted, len(groups), len(functions)


def self_test():
    groups = {"diff/sparse_5pct": "diff_bench", "diff/sparse_5pct_rows": "diff_bench"}
    functions = {("diff_bench", "compute"), ("diff_bench", "compute_dirty")}

    # Resolves: longest group wins, then a declared function.
    assert resolve("diff/sparse_5pct/compute/200x60", groups, functions) is None
    assert resolve("diff/sparse_5pct_rows/compute_dirty/200x60", groups, functions) is None
    # The real defect this was written for: group exists, function does not.
    assert resolve("diff/sparse_5pct/200x60", groups, functions) == "no_such_bench_function"
    # A group that was never declared.
    assert resolve("diff/imaginary/compute/80x24", groups, functions) == "no_such_group"
    # Longest-prefix matters: `diff/sparse_5pct` also prefixes the `_rows` id as
    # a string, but `_rows` is the longer declared group and must win, or
    # `compute_dirty` would be looked up under the wrong group.
    assert max([g for g in groups if "diff/sparse_5pct_rows".startswith(g)], key=len) \
        == "diff/sparse_5pct_rows"
    # A bare group with no function segment is a complete id.
    assert resolve("diff/sparse_5pct", groups, functions) is None
    print(json.dumps(dict(kind="self-test", status="passed", checks=6)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path,
                        default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--quiet", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    try:
        if args.self_test:
            self_test()
            return 0
        records, failures, excepted, groups, functions = check(args.root)
        if not args.quiet:
            for record in records:
                print(json.dumps(record))
        print(json.dumps(dict(kind="summary", declared_groups=groups,
                              declared_bench_functions=functions,
                              unresolved=failures, excepted=excepted,
                              status="failed" if failures else "passed")))
        return int(bool(failures))
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(json.dumps(dict(kind="error", status="failed", detail=str(error))))
        return 1


if __name__ == "__main__":
    sys.exit(main())

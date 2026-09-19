#!/usr/bin/env python3
"""Audit bead close reasons against `.beads/policy.yaml`; read-only, stdlib-only.

`br` enforces that policy at close time, but only going forward and only when
it is not bypassed. This is the other half (bd-g00-root-epic-ewths.7.1): it
reads what is actually recorded in `.beads/issues.jsonl` and reports every
close that carries no usable evidence, whether it predates the policy, went
through `--bypass-policy`, or was written straight into the JSONL.

Why it does not fail on history
-------------------------------
1,047 of 2,900 closes recorded before 2026-09-19 have a reason under 20
characters or none at all. Those cannot be repaired -- the person who closed
them is gone and the evidence with them -- so failing on them would produce a
gate that can never be green, and a gate that can never be green is one nobody
runs. The exit code therefore covers closes on or after EPOCH only; everything
older is reported as context under `kind: legacy`.

Run `--self-test` for the retained fixtures. No temporary files, no cleanup.
"""

import argparse
import json
from pathlib import Path
import re
import sys


# The moment `.beads/policy.yaml` landed, to the minute rather than the day.
# Closes from here on are the gate; everything before it is history, reported
# but not enforced.
#
# The precision matters: 29 beads were closed earlier on 2026-09-19 by agents
# working without a policy, several with long and genuinely informative reasons
# that simply predate the `kind:value` convention. A date-only epoch would have
# made this gate red the moment it was installed, on closes nobody could have
# written differently -- which is how a gate earns a permanent `|| true`.
#
# `closed_at` is zero-padded ISO-8601 UTC, so a plain string comparison orders
# it correctly even though `br` records varying sub-second precision.
EPOCH = "2026-09-19T21:10:00Z"

# Mirrors close_policy.require_close_reason.min_length in .beads/policy.yaml.
MIN_LENGTH = 80

# Mirrors close_policy.require_typed_references.required_kinds. Kept as a
# literal rather than parsed out of the YAML because this script is stdlib-only
# and `pyyaml` is not a dependency of this repo; `--check-policy` re-reads the
# file and fails if the two have drifted, which is the part that matters.
KINDS = ("commit", "pr", "reviewer", "investigation", "agent-mail",
         "dashboard", "bead", "test", "path", "run")

# A typed reference is a contiguous `kind:value` token, matching how
# beads_rust's evaluate_typed_references scans the reason text.
REFERENCE = re.compile(
    r"(?<![A-Za-z0-9_-])(" + "|".join(re.escape(k) for k in KINDS) + r"):\S")


def violations(reason):
    """Return the policy gates an individual close reason fails."""
    text = (reason or "").strip()
    failed = []
    if not text:
        failed.append("no_reason")
    elif len(text) < MIN_LENGTH:
        failed.append("too_short")
    if not REFERENCE.search(text):
        failed.append("no_typed_reference")
    return failed


def closed_issues(jsonl):
    """Yield (id, closed_at, reason) for every closed bead, in file order."""
    for line in jsonl.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            issue = json.loads(line)
        except json.JSONDecodeError as error:
            raise ValueError(f"malformed JSONL line: {error}") from error
        if issue.get("status") != "closed":
            continue
        yield issue.get("id"), (issue.get("closed_at") or ""), issue.get("close_reason")


def audit(jsonl, epoch):
    """Split closes into enforced failures and reported legacy failures."""
    records, legacy, checked = [], 0, 0
    for issue_id, closed_at, reason in closed_issues(jsonl):
        failed = violations(reason)
        if not failed:
            continue
        # A close with no timestamp cannot be placed relative to the epoch.
        # Treat it as history: it is certainly not a close made under the
        # policy, since `br` records `closed_at` on every close it performs.
        recent = bool(closed_at) and closed_at >= epoch
        if recent:
            checked += 1
            records.append(dict(kind="close", id=issue_id, closed_at=closed_at,
                                violations=failed, status="failed"))
        else:
            legacy += 1
    return records, legacy, checked


def policy_matches(policy_path):
    """Report whether the constants above still match the policy file.

    Deliberately textual: the point is to notice drift between this script and
    `.beads/policy.yaml`, not to reimplement a YAML parser. A miss reports
    `unknown` rather than passing silently.
    """
    if not policy_path.is_file():
        return "missing"
    text = policy_path.read_text(encoding="utf-8", errors="replace")
    length = re.search(r"^\s*min_length:\s*(\d+)\s*$", text, re.M)
    if length is None:
        return "unknown"
    if int(length.group(1)) != MIN_LENGTH:
        return "drifted"
    listed = set(re.findall(r"^\s*-\s*([a-z-]+)\s*$", text, re.M))
    return "ok" if set(KINDS) <= listed else "drifted"


FIXTURE = Path("tests/fixtures/close_evidence/closed_beads.jsonl")


def self_test_audit(root):
    """Run `audit` over the retained fixture and check what it selects.

    The fixture is a real file under `tests/fixtures/`, not a temp directory
    built and torn down per run: Rule 1 forbids this repo's tooling from
    deleting anything, including files it created itself, and a test whose
    cleanup is the thing that breaks the rule is not worth the coverage.
    """
    jsonl = root / FIXTURE
    records, legacy, checked = audit(jsonl, "2026-09-19T21:10:00Z")
    flagged = {record["id"]: record["violations"] for record in records}

    assert flagged == {
        "fx-null-reason": ["no_reason", "no_typed_reference"],
        "fx-done": ["too_short", "no_typed_reference"],
        "fx-prose-no-reference": ["no_typed_reference"],
        "fx-short-but-referenced": ["too_short"],
    }, flagged
    # The two well-formed closes are not flagged, and neither are the open
    # and in-progress beads -- an unclosed bead has nothing to justify yet.
    assert "fx-valid-test" not in flagged and "fx-valid-commit" not in flagged
    assert "fx-open" not in flagged and "fx-in-progress" not in flagged
    assert checked == 4

    # Pre-epoch failures are counted, never enforced. `fx-no-timestamp` lands
    # here too: `br` records `closed_at` on every close it performs, so a close
    # without one was not made under the policy.
    assert legacy == 3, legacy

    # Widening the epoch moves history into the enforced set rather than
    # changing any verdict. `fx-no-timestamp` stays legacy at every epoch --
    # a close that records no time cannot be placed relative to one, and no
    # widening should make it enforceable.
    records, legacy, _ = audit(jsonl, "2026-01-01")
    assert len(records) == 6 and legacy == 1, (len(records), legacy)
    assert "fx-no-timestamp" not in {record["id"] for record in records}

    # An epoch after every close leaves nothing to enforce, which is what a
    # freshly installed policy looks like.
    records, legacy, _ = audit(jsonl, "2099-01-01")
    assert records == [] and legacy == 7

    # Report shape: the keys a consumer can rely on.
    records, _, _ = audit(jsonl, "2026-01-01")
    assert records
    for record in records:
        assert set(record) == {"kind", "id", "closed_at", "violations", "status"}
        assert record["kind"] == "close" and record["status"] == "failed"
        assert record["violations"]
    return len(flagged)


def self_test():
    """Fixtures for the reason checker and the reference matcher."""
    assert violations(None) == ["no_reason", "no_typed_reference"]
    assert violations("   ") == ["no_reason", "no_typed_reference"]
    assert violations("Completed") == ["too_short", "no_typed_reference"]
    # Long enough, but nothing to go look at.
    assert violations("x" * 120) == ["no_typed_reference"]
    # A reference alone does not excuse an empty reason.
    assert violations("commit:abc1234") == ["too_short"]
    assert violations("Fixed the scroll coalescer so every notch survives the "
                      "flush, with tests. commit:509710e0") == []
    # `kind:` must be a token, not a suffix of a longer word, or
    # "recommit:deadbeef" and "mypr:x" would satisfy the gate.
    assert violations("y" * 100 + " recommit:deadbeef") == ["no_typed_reference"]
    assert violations("y" * 100 + " uncommit:deadbeef") == ["no_typed_reference"]
    # A kind with an empty value is not evidence.
    assert violations("z" * 100 + " commit: ") == ["no_typed_reference"]
    # Hyphenated built-in kinds still match.
    assert violations("w" * 100 + " agent-mail:1558") == []
    return 10


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--epoch", default=EPOCH,
                        help=f"enforce closes at or after this ISO-8601 UTC instant "
                             f"(default {EPOCH}); pass a bare date to widen it")
    parser.add_argument("--jsonl", type=Path,
                        help="audit this JSONL instead of <root>/.beads/issues.jsonl")
    parser.add_argument("--quiet", action="store_true",
                        help="print the summary only, not each failing close")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    try:
        if args.self_test:
            reasons = self_test()
            flagged = self_test_audit(args.root)
            print(json.dumps(dict(kind="self-test", status="passed",
                                  reason_checks=reasons, fixture_flagged=flagged)))
            return 0
        jsonl = args.jsonl or (args.root / ".beads" / "issues.jsonl")
        records, legacy, checked = audit(jsonl, args.epoch)
        if not args.quiet:
            for record in records:
                print(json.dumps(record))
        print(json.dumps(dict(
            kind="summary", epoch=args.epoch, enforced_failures=len(records),
            legacy_failures=legacy, policy=policy_matches(args.root / ".beads" / "policy.yaml"),
            status="failed" if records else "passed")))
        return int(bool(records))
    except (OSError, ValueError, KeyError) as error:
        print(json.dumps(dict(kind="error", status="failed", detail=str(error))))
        return 1


if __name__ == "__main__":
    sys.exit(main())

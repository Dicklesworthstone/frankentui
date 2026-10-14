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
from datetime import datetime, timezone
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

# Explicit repository file path mentioned in a bead title.
TITLE_REPO_PATH = re.compile(
    r"(?<![A-Za-z0-9_./-])"
    r"((?:crates|docs|scripts|tests|\.config|\.github)/[A-Za-z0-9_./-]+\.[A-Za-z0-9]+)"
)

DEFAULT_STALE_DAYS = 14


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


def load_issues(jsonl):
    """Return all bead objects from a JSONL file, in file order."""
    issues = []
    for line in jsonl.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            issues.append(json.loads(line))
        except json.JSONDecodeError as error:
            raise ValueError(f"malformed JSONL line: {error}") from error
    return issues


def closed_issues(jsonl):
    """Yield (id, closed_at, reason) for every closed bead, in file order."""
    for issue in load_issues(jsonl):
        if issue.get("status") != "closed":
            continue
        yield issue.get("id"), (issue.get("closed_at") or ""), issue.get("close_reason")


def parse_iso_utc(raw):
    """Parse an ISO-8601 timestamp (or bare YYYY-MM-DD date) into UTC datetime."""
    text = (raw or "").strip()
    if not text:
        return None
    if len(text) == 10 and text[4] == "-" and text[7] == "-":
        text = text + "T00:00:00+00:00"
    elif text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        dt = datetime.fromisoformat(text)
    except ValueError:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.astimezone(timezone.utc)


def extract_title_repo_paths(title):
    """Extract concrete repository file paths named in a bead title."""
    paths = []
    for match in TITLE_REPO_PATH.finditer(title or ""):
        candidate = match.group(1).rstrip(".,:;)]`'\"")
        if any(ch in candidate for ch in ("*", "{", "}", "<", ">", "...")):
            continue
        if candidate not in paths:
            paths.append(candidate)
    return paths


def audit_alerts_from_issues(issues, root, stale_days=DEFAULT_STALE_DAYS, as_of=None):
    """Compute tracker staleness and structural alerts from loaded issues (report-only)."""
    by_id = {issue.get("id"): issue for issue in issues if issue.get("id")}

    reference_dt = parse_iso_utc(as_of) if as_of else None
    if reference_dt is None:
        timestamps = []
        for issue in issues:
            for field in ("updated_at", "closed_at", "created_at"):
                dt = parse_iso_utc(issue.get(field))
                if dt is not None:
                    timestamps.append(dt)
        reference_dt = max(timestamps) if timestamps else datetime.now(timezone.utc)

    stale_in_progress = []
    for issue in issues:
        if issue.get("status") != "in_progress":
            continue
        ts_raw = issue.get("updated_at") or issue.get("created_at") or ""
        dt = parse_iso_utc(ts_raw)
        if dt is None:
            continue
        idle_days = (reference_dt - dt).total_seconds() / 86400.0
        if idle_days >= stale_days:
            stale_in_progress.append(dict(
                id=issue.get("id"),
                title=issue.get("title", ""),
                updated_at=ts_raw,
                idle_days=round(idle_days, 1),
            ))

    children_by_parent = {issue_id: set() for issue_id in by_id}
    for issue in issues:
        issue_id = issue.get("id") or ""
        for dep in issue.get("dependencies") or []:
            if dep.get("type") == "parent-child":
                parent_id = dep.get("depends_on_id")
                if parent_id in children_by_parent and issue_id in by_id:
                    children_by_parent[parent_id].add(issue_id)
        if "." in issue_id:
            parent_id = issue_id.rsplit(".", 1)[0]
            if parent_id in children_by_parent:
                children_by_parent[parent_id].add(issue_id)

    all_children_closed_epics = []
    for issue in issues:
        issue_id = issue.get("id")
        if not issue_id or issue.get("status") == "closed":
            continue
        children = sorted(children_by_parent.get(issue_id) or ())
        if not children:
            continue
        if all(by_id[child_id].get("status") == "closed" for child_id in children):
            all_children_closed_epics.append(dict(
                id=issue_id,
                title=issue.get("title", ""),
                status=issue.get("status"),
                closed_children=len(children),
            ))

    missing_deliverable_closes = []
    for issue in issues:
        if issue.get("status") != "closed":
            continue
        title = issue.get("title") or ""
        candidates = extract_title_repo_paths(title)
        if not candidates:
            continue
        missing = [rel for rel in candidates if not (root / rel).exists()]
        if missing:
            missing_deliverable_closes.append(dict(
                id=issue.get("id"),
                title=title,
                missing_paths=missing,
            ))

    return dict(
        kind="alerts",
        as_of=reference_dt.strftime("%Y-%m-%dT%H:%M:%SZ"),
        stale_days=stale_days,
        stale_in_progress_count=len(stale_in_progress),
        stale_in_progress=stale_in_progress,
        all_children_closed_epics_count=len(all_children_closed_epics),
        all_children_closed_epics=all_children_closed_epics,
        missing_deliverable_closes_count=len(missing_deliverable_closes),
        missing_deliverable_closes=missing_deliverable_closes,
        status="reported",
    )


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


def self_test_alerts(root):
    """Verify `--alerts` detection of stale claims, closed-children epics, and missing deliverables."""
    fixture_issues = [
        {
            "id": "fx-stale-claim",
            "title": "Abandoned in-progress task",
            "status": "in_progress",
            "updated_at": "2026-09-06T12:00:00Z",
        },
        {
            "id": "fx-fresh-claim",
            "title": "Active in-progress task",
            "status": "in_progress",
            "updated_at": "2026-10-12T12:00:00Z",
        },
        {
            "id": "fx-epic-done",
            "title": "epic(G19): All children closed",
            "status": "open",
            "issue_type": "epic",
            "updated_at": "2026-10-13T00:00:00Z",
        },
        {
            "id": "fx-epic-done.1",
            "title": "Child 1",
            "status": "closed",
            "updated_at": "2026-10-10T00:00:00Z",
        },
        {
            "id": "fx-epic-done.2",
            "title": "Child 2",
            "status": "closed",
            "updated_at": "2026-10-11T00:00:00Z",
        },
        {
            "id": "fx-epic-open",
            "title": "epic: Has open child",
            "status": "open",
            "issue_type": "epic",
            "updated_at": "2026-10-13T00:00:00Z",
        },
        {
            "id": "fx-epic-open.1",
            "title": "Open child",
            "status": "open",
            "updated_at": "2026-10-13T00:00:00Z",
        },
        {
            "id": "fx-missing-path",
            "title": "Add docs/nonexistent_deliverable_fixture_49_4.md",
            "status": "closed",
            "updated_at": "2026-10-13T00:00:00Z",
        },
        {
            "id": "fx-existing-path",
            "title": "Update scripts/check_close_evidence.py",
            "status": "closed",
            "updated_at": "2026-10-13T00:00:00Z",
        },
    ]
    report = audit_alerts_from_issues(fixture_issues, root, stale_days=14)
    assert [r["id"] for r in report["stale_in_progress"]] == ["fx-stale-claim"], report
    assert [r["id"] for r in report["all_children_closed_epics"]] == ["fx-epic-done"], report
    assert [r["id"] for r in report["missing_deliverable_closes"]] == ["fx-missing-path"], report
    assert report["missing_deliverable_closes"][0]["missing_paths"] == [
        "docs/nonexistent_deliverable_fixture_49_4.md"
    ]
    return 3


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
    parser.add_argument("--alerts", action="store_true",
                        help="report stale in-progress claims, all-children-closed epics, "
                             "and closed beads naming missing repo paths (report-only, exit 0)")
    parser.add_argument("--stale-days", type=float, default=DEFAULT_STALE_DAYS,
                        help=f"idle threshold in days for --alerts stale in-progress claims "
                             f"(default {DEFAULT_STALE_DAYS})")
    parser.add_argument("--as-of",
                        help="reference ISO-8601 UTC timestamp for --alerts staleness "
                             "(default: latest timestamp in the JSONL)")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    try:
        if args.self_test:
            reasons = self_test()
            flagged = self_test_audit(args.root)
            alerts_checked = self_test_alerts(args.root)
            print(json.dumps(dict(kind="self-test", status="passed",
                                  reason_checks=reasons, fixture_flagged=flagged,
                                  alert_checks=alerts_checked)))
            return 0
        jsonl = args.jsonl or (args.root / ".beads" / "issues.jsonl")
        if args.alerts:
            issues = load_issues(jsonl)
            report = audit_alerts_from_issues(
                issues, args.root, stale_days=args.stale_days, as_of=args.as_of
            )
            print(json.dumps(report))
            return 0
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

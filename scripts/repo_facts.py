#!/usr/bin/env python3
"""Compute repository inventory facts from the tree and gate hand-edited drift.

Pure stdlib Python. No compilation, no network, no file deletion.

Modes:
  --json       Print computed facts as JSON.
  --check      Verify every <!-- fact:<name> --><val><!-- /fact --> marker and
               badge in README.md and AGENTS.md matches the tree, and reject
               un-marked inventory/line-count drift patterns.
  --write      Update <!-- fact:<name> -->...<!-- /fact --> markers and
               Shields.io inventory badges in README.md and AGENTS.md in place.
  --self-test  Run in-memory unit tests for fact extraction, marker checking,
               drift pattern rejection, and --write updates.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]

FACT_MARKER_RE = re.compile(
    r"<!--\s*fact:([a-z0-9_]+)\s*-->(.*?)<!--\s*/fact\s*-->",
    re.DOTALL,
)

WIDGET_IMPL_RE = re.compile(
    r"\bimpl\b[^{;]*?\b(?:Widget|StatefulWidget)\s+for\s+([A-Za-z_][A-Za-z0-9_]*)",
    re.DOTALL,
)

REQUIRED_FACTS = (
    "crates",
    "widgets",
    "widget_types",
    "widgets_workspace",
    "demo_screens",
    "table_presets",
    "snapshots_total",
    "snapshots_showcase",
    "snapshots_extras",
    "snapshots_harness",
    "integration_test_files",
    "integration_tests_runtime",
    "integration_tests_harness",
    "integration_tests_showcase",
)

BADGE_PATTERNS = (
    ("crates", re.compile(r"(https://img\.shields\.io/badge/crates-)([^)-]+)(-blue)")),
    ("widgets", re.compile(r"(https://img\.shields\.io/badge/widgets-)([^)-]+)(-blue)")),
    ("demo_screens", re.compile(r"(https://img\.shields\.io/badge/demo_screens-)([^)-]+)(-blue)")),
)

FORBIDDEN_DRIFT_PATTERNS = (
    (
        "bare 80+ widget claim",
        re.compile(r"\b80\+\s*(?:direct\s+)?(?:widget|Widget|impl)|badge/widgets-80%2B", re.IGNORECASE),
    ),
    (
        "heading line-count parenthetical",
        re.compile(r"^#{2,4}\s+[^\n]*\(\d[\d,]*\+?\s+Lines\)", re.MULTILINE),
    ),
    (
        "module/file line-count parenthetical",
        re.compile(
            r"\(\d[\d,]*\+?\s+lines?(?:\s+in\s+`?[^)\n]+)?\)"
            r"|\(\d+K\+\s+lines?[^)\n]*\)"
            r"|\(`[a-z0-9_/-]+\.rs`,\s*\d[\d,]*\+?\s+lines?\)",
            re.IGNORECASE,
        ),
    ),
    (
        "unbacked saturating_*/checked_* call-site count",
        re.compile(r"\b\d[\d,]*\s+`(?:saturating|checked)_\*`"),
    ),
    (
        "bare snapshot count comment in code block",
        re.compile(r"#\s*\d+\s+plain-text\s+\.snap|tests/snapshots/\s+#\s*\d+"),
    ),
    (
        "dated nextest test-count prose",
        re.compile(r"\b2[0-9],\d{3}\s+default\s+tests|runs\s+~2[0-9],\d{3}"),
    ),
    (
        "bare integration test file count without fact marker",
        re.compile(r"directory\s+--\s+\d+\s+files"),
    ),
)


def _load_reachability_gate():
    scripts_dir = str(Path(__file__).resolve().parent)
    added = scripts_dir not in sys.path
    if added:
        sys.path.insert(0, scripts_dir)
    try:
        import check_module_reachability

        return check_module_reachability
    except ImportError:  # pragma: no cover
        if added:
            sys.path.remove(scripts_dir)
        return None


_reachability_gate = _load_reachability_gate()


def strip_test_and_doc_lines(text: str) -> str:
    """Strip #[cfg(test)] regions and rustdoc comment lines before scanning impls."""
    if _reachability_gate is not None:
        text = _reachability_gate.strip_test_regions(text)
    lines = [
        line
        for line in text.splitlines()
        if not line.lstrip().startswith(("///", "//!"))
    ]
    return "\n".join(lines)


def count_widget_impls_in_dir(src_dir: Path) -> tuple[int, int]:
    """Return (total impl count, distinct target type count) under src_dir."""
    impl_count = 0
    types: set[str] = set()
    for rs_path in sorted(src_dir.rglob("*.rs")):
        cleaned = strip_test_and_doc_lines(
            rs_path.read_text(encoding="utf-8", errors="replace")
        )
        matches = WIDGET_IMPL_RE.findall(cleaned)
        impl_count += len(matches)
        types.update(matches)
    return impl_count, len(types)


def count_enum_variants(rs_path: Path, enum_name: str) -> int:
    """Count top-level variants in `pub enum <enum_name> { ... }`."""
    text = rs_path.read_text(encoding="utf-8", errors="replace")
    pattern = re.compile(
        rf"\bpub\s+enum\s+{re.escape(enum_name)}\s*\{{(.*?)\}}",
        re.DOTALL,
    )
    match = pattern.search(text)
    if not match:
        raise ValueError(f"Could not find `pub enum {enum_name}` in {rs_path}")
    body = match.group(1)
    variants = []
    for raw_line in body.splitlines():
        line = raw_line.split("//", 1)[0].strip()
        if not line or line.startswith("#"):
            continue
        for token in line.split(","):
            ident = token.strip()
            if ident and re.fullmatch(r"[A-Z][A-Za-z0-9_]*", ident):
                variants.append(ident)
    return len(variants)


def compute_facts(root: Path = ROOT) -> dict[str, int]:
    cargo_toml = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    crates = len(cargo_toml["workspace"]["members"])

    widgets, widget_types = count_widget_impls_in_dir(
        root / "crates" / "ftui-widgets" / "src"
    )

    widgets_workspace = 0
    for crate_src in sorted((root / "crates").glob("*/src")):
        count, _ = count_widget_impls_in_dir(crate_src)
        widgets_workspace += count

    demo_screens = count_enum_variants(
        root / "crates" / "ftui-demo-showcase" / "src" / "app.rs",
        "ScreenId",
    )
    table_presets = count_enum_variants(
        root / "crates" / "ftui-style" / "src" / "table_theme.rs",
        "TablePresetId",
    )

    snapshots_showcase = len(
        list((root / "crates" / "ftui-demo-showcase" / "tests" / "snapshots").glob("*.snap"))
    )
    snapshots_extras = len(
        list((root / "crates" / "ftui-extras" / "tests" / "snapshots").glob("*.snap"))
    )
    snapshots_harness = len(
        list((root / "crates" / "ftui-harness" / "tests" / "snapshots").glob("*.snap"))
    )
    snapshots_total = len(list((root / "crates").glob("*/tests/snapshots/*.snap")))

    integration_test_files = len(list((root / "crates").glob("*/tests/*.rs")))
    integration_tests_runtime = len(
        list((root / "crates" / "ftui-runtime" / "tests").glob("*.rs"))
    )
    integration_tests_harness = len(
        list((root / "crates" / "ftui-harness" / "tests").glob("*.rs"))
    )
    integration_tests_showcase = len(
        list((root / "crates" / "ftui-demo-showcase" / "tests").glob("*.rs"))
    )

    return {
        "crates": crates,
        "widgets": widgets,
        "widget_types": widget_types,
        "widgets_workspace": widgets_workspace,
        "demo_screens": demo_screens,
        "table_presets": table_presets,
        "snapshots_total": snapshots_total,
        "snapshots_showcase": snapshots_showcase,
        "snapshots_extras": snapshots_extras,
        "snapshots_harness": snapshots_harness,
        "integration_test_files": integration_test_files,
        "integration_tests_runtime": integration_tests_runtime,
        "integration_tests_harness": integration_tests_harness,
        "integration_tests_showcase": integration_tests_showcase,
    }


def check_documents(
    facts: dict[str, int],
    docs: dict[str, str],
    require_all_facts: bool = True,
) -> tuple[list[str], int]:
    """Validate fact markers, badges, and forbidden drift patterns across docs."""
    errors: list[str] = []
    seen_facts: set[str] = set()
    marker_count = 0

    for doc_name, text in docs.items():
        for match in FACT_MARKER_RE.finditer(text):
            marker_count += 1
            fact_name = match.group(1)
            actual_val = match.group(2).strip()
            line_no = text.count("\n", 0, match.start()) + 1
            if fact_name not in facts:
                errors.append(
                    f"{doc_name}:{line_no}: unknown fact name `{fact_name}` in marker"
                )
                continue
            seen_facts.add(fact_name)
            expected_val = str(facts[fact_name])
            if actual_val != expected_val:
                errors.append(
                    f"{doc_name}:{line_no}: fact `{fact_name}` drifted: "
                    f"doc has `{actual_val}`, tree has `{expected_val}`"
                )

        if doc_name.endswith("README.md"):
            for fact_name, badge_re in BADGE_PATTERNS:
                badge_match = badge_re.search(text)
                if not badge_match:
                    errors.append(
                        f"{doc_name}: missing Shields.io badge for `{fact_name}`"
                    )
                    continue
                actual_badge = badge_match.group(2)
                expected_badge = str(facts[fact_name])
                if actual_badge != expected_badge:
                    line_no = text.count("\n", 0, badge_match.start()) + 1
                    errors.append(
                        f"{doc_name}:{line_no}: badge `{fact_name}` drifted: "
                        f"badge has `{actual_badge}`, tree has `{expected_badge}`"
                    )

        for label, pattern in FORBIDDEN_DRIFT_PATTERNS:
            for drift_match in pattern.finditer(text):
                line_no = text.count("\n", 0, drift_match.start()) + 1
                snippet = drift_match.group(0).strip()
                errors.append(
                    f"{doc_name}:{line_no}: forbidden un-marked drift pattern "
                    f"({label}): `{snippet}`"
                )

    if require_all_facts:
        for req in REQUIRED_FACTS:
            if req not in seen_facts:
                errors.append(
                    f"missing required `<!-- fact:{req} -->` marker across checked docs"
                )

    return errors, marker_count


def rewrite_document(text: str, facts: dict[str, int], is_readme: bool = False) -> str:
    """Replace all <!-- fact:<name> -->...<!-- /fact --> values and README badges."""

    def _replace_marker(match: re.Match[str]) -> str:
        name = match.group(1)
        if name not in facts:
            return match.group(0)
        return f"<!-- fact:{name} -->{facts[name]}<!-- /fact -->"

    updated = FACT_MARKER_RE.sub(_replace_marker, text)
    if is_readme:
        for fact_name, badge_re in BADGE_PATTERNS:
            if fact_name in facts:
                updated = badge_re.sub(
                    rf"\g<1>{facts[fact_name]}\g<3>",
                    updated,
                )
    return updated


def self_test() -> None:
    facts = {name: idx + 10 for idx, name in enumerate(REQUIRED_FACTS)}
    facts.update(
        {
            "crates": 20,
            "widgets": 70,
            "widget_types": 55,
            "widgets_workspace": 93,
            "demo_screens": 45,
            "table_presets": 10,
            "snapshots_total": 604,
            "snapshots_showcase": 436,
            "snapshots_extras": 101,
            "snapshots_harness": 67,
            "integration_test_files": 302,
            "integration_tests_runtime": 51,
            "integration_tests_harness": 49,
            "integration_tests_showcase": 46,
        }
    )

    readme_good = (
        "![crates](https://img.shields.io/badge/crates-20-blue)\n"
        "![widgets](https://img.shields.io/badge/widgets-70-blue)\n"
        "![screens](https://img.shields.io/badge/demo_screens-45-blue)\n"
        + "\n".join(
            f"<!-- fact:{name} -->{facts[name]}<!-- /fact -->"
            for name in REQUIRED_FACTS
        )
    )
    agents_good = "<!-- fact:crates -->20<!-- /fact -->\n"

    errors, count = check_documents(
        facts, {"README.md": readme_good, "AGENTS.md": agents_good}
    )
    assert not errors, f"expected clean check, got: {errors}"
    assert count == len(REQUIRED_FACTS) + 1

    # 1. Mutating a fact value must fail --check
    readme_bad_val = readme_good.replace(
        "<!-- fact:widgets -->70<!-- /fact -->",
        "<!-- fact:widgets -->80<!-- /fact -->",
    )
    errors, _ = check_documents(
        facts, {"README.md": readme_bad_val, "AGENTS.md": agents_good}
    )
    assert any("fact `widgets` drifted" in e for e in errors), errors

    # 2. --write repairs drifted markers and badges
    readme_drifted_badge = readme_bad_val.replace("badge/widgets-70-blue", "badge/widgets-99-blue")
    repaired = rewrite_document(readme_drifted_badge, facts, is_readme=True)
    errors, _ = check_documents(
        facts, {"README.md": repaired, "AGENTS.md": agents_good}
    )
    assert not errors, f"expected repaired doc to pass, got: {errors}"

    # 3. Forbidden un-marked drift patterns must fail
    drift_samples = [
        "FrankenTUI ships 80+ direct Widget implementations.",
        "### Input Parser (4,300+ Lines)",
        "Pane workspace system (9,000+ lines in `ftui-layout/src/pane.rs`)",
        "coordinate ops — 3,216 `saturating_*` call sites",
        "# 599 plain-text .snap files",
        "Measured: 26,293 default tests in workspace",
        "directory -- 298 files on 2026-10-14",
    ]
    for sample in drift_samples:
        errs, _ = check_documents(
            facts,
            {"README.md": readme_good + "\n" + sample, "AGENTS.md": agents_good},
        )
        assert errs, f"expected drift sample to fail: {sample}"

    # 4. Verify live repository facts can be computed and are positive
    live_facts = compute_facts(ROOT)
    for key in REQUIRED_FACTS:
        assert live_facts.get(key, 0) > 0, f"expected positive fact for {key}, got {live_facts}"

    print(
        json.dumps(
            {
                "scope": "repo-facts-self-test",
                "status": "passed",
                "checked_facts": len(REQUIRED_FACTS),
                "drift_cases": len(drift_samples) + 2,
            }
        )
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--json", action="store_true", help="Print computed facts as JSON")
    mode.add_argument("--check", action="store_true", help="Verify README.md and AGENTS.md against tree facts")
    mode.add_argument("--write", action="store_true", help="Rewrite fact markers and badges in README.md and AGENTS.md")
    mode.add_argument("--self-test", action="store_true", help="Run in-memory self-tests")
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--readme", type=Path, default=None)
    parser.add_argument("--agents", type=Path, default=None)
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        self_test()
        return 0

    root = args.root.resolve()
    readme_path = (args.readme or (root / "README.md")).resolve()
    agents_path = (args.agents or (root / "AGENTS.md")).resolve()
    facts = compute_facts(root)

    if args.json:
        print(json.dumps(facts, indent=2, sort_keys=True))
        return 0

    if args.write:
        readme_text = readme_path.read_text(encoding="utf-8")
        agents_text = agents_path.read_text(encoding="utf-8")
        new_readme = rewrite_document(readme_text, facts, is_readme=True)
        new_agents = rewrite_document(agents_text, facts, is_readme=False)
        if new_readme != readme_text:
            readme_path.write_text(new_readme, encoding="utf-8")
        if new_agents != agents_text:
            agents_path.write_text(new_agents, encoding="utf-8")

    docs = {
        "README.md": readme_path.read_text(encoding="utf-8"),
        "AGENTS.md": agents_path.read_text(encoding="utf-8"),
    }
    errors, marker_count = check_documents(facts, docs)
    summary = {
        "scope": "repo-facts",
        "status": "failed" if errors else "passed",
        "markers_checked": marker_count,
        "facts": facts,
        "errors": errors,
    }
    if not args.quiet:
        print(json.dumps(summary))
    for err in errors:
        print(f"[repo-facts] {err}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())

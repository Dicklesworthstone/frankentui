#!/usr/bin/env python3
"""Tests for scripts/check_module_reachability.py (bd-g00-root-epic-ewths.11.2).

Run with either:
    python3 -m unittest discover -s scripts/tests
    python3 scripts/tests/test_check_module_reachability.py

stdlib `unittest` rather than pytest, so this runs anywhere the gate itself
runs. pytest collects `unittest.TestCase` classes too, so `pytest scripts/tests`
also works where it is installed.

Most of these pin bugs that the first working version of the gate actually had.
A reachability checker that is subtly wrong is worse than none: it either
greenlights dead code or condemns live code, and both teach people to ignore
it.
"""

from __future__ import annotations

import json
import sys
import textwrap
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import check_module_reachability as gate  # noqa: E402


class StripTestRegions(unittest.TestCase):
    """The single most damaging bug this script can have."""

    def test_keeps_production_code_after_an_inline_test_block(self):
        # ftui-runtime/src/program.rs has a #[cfg(test)] at line 4190 of
        # 19204. Cutting the file at the first marker loses three quarters of
        # the largest file in the workspace, and every module it references
        # then looks dead.
        source = textwrap.dedent(
            """\
            fn production_one() {}

            #[cfg(test)]
            mod inline_tests {
                fn helper() { let _ = "}"; }
            }

            fn production_two() { debug_trace::elapsed_ms(); }
            """
        )
        stripped = gate.strip_test_regions(source)
        self.assertIn("production_one", stripped)
        self.assertIn("production_two", stripped)
        self.assertIn("debug_trace::", stripped)
        self.assertNotIn("helper", stripped)

    def test_preserves_line_numbers(self):
        source = "fn a() {}\n#[cfg(test)]\nmod t {\n    fn x() {}\n}\nfn b() {}\n"
        stripped = gate.strip_test_regions(source)
        self.assertEqual(source.count("\n"), stripped.count("\n"))
        # `fn b` must still be reported on line 6.
        self.assertEqual(stripped.splitlines()[5].strip(), "fn b() {}")

    def test_braces_in_strings_do_not_end_the_block_early(self):
        source = textwrap.dedent(
            """\
            #[cfg(test)]
            mod t {
                fn x() { println!("{}", "{"); }
            }
            fn after() { target::thing(); }
            """
        )
        stripped = gate.strip_test_regions(source)
        self.assertIn("target::", stripped)
        self.assertNotIn("println", stripped)

    def test_handles_an_attribute_on_a_use_statement(self):
        source = "#[cfg(test)]\nuse crate::helper;\nfn after() { real::thing(); }\n"
        stripped = gate.strip_test_regions(source)
        self.assertNotIn("helper", stripped)
        self.assertIn("real::", stripped)

    def test_strips_several_blocks(self):
        source = (
            "#[cfg(test)]\nmod a { fn x() {} }\n"
            "fn mid() { kept::call(); }\n"
            "#[cfg(test)]\nmod b { fn y() {} }\n"
        )
        stripped = gate.strip_test_regions(source)
        self.assertIn("kept::", stripped)
        self.assertNotIn("fn x", stripped)
        self.assertNotIn("fn y", stripped)


class PathSegments(unittest.TestCase):
    def test_matches_a_nested_segment(self):
        # `ftui_widgets::borders::Borders` must count as a reference to
        # `borders`. An earlier version excluded any segment preceded by `::`,
        # which is exactly the cross-crate form, and reported most of the
        # widget library as dead.
        found = gate.PATH_SEGMENT_RE.findall("use ftui_widgets::borders::Borders;")
        self.assertIn("borders", found)

    def test_matches_a_leading_segment(self):
        found = gate.PATH_SEGMENT_RE.findall("borders::Borders::default()")
        self.assertIn("borders", found)

    def test_ignores_a_type_name(self):
        found = gate.PATH_SEGMENT_RE.findall("Borders::ALL")
        self.assertNotIn("Borders", found)


class Allowlist(unittest.TestCase):
    def test_rejects_an_entry_without_a_bead_id(self):
        with TemporaryDirectory() as tmp:
            path = Path(tmp) / "allow.txt"
            path.write_text("ftui-render::headless\n", encoding="utf-8")
            with self.assertRaises(SystemExit):
                gate.load_allowlist(path)

    def test_reads_entries_and_skips_comments(self):
        with TemporaryDirectory() as tmp:
            path = Path(tmp) / "allow.txt"
            path.write_text(
                "# a comment\n\nftui-render::headless  # bd-123\n", encoding="utf-8"
            )
            self.assertEqual(gate.load_allowlist(path), {"ftui-render::headless": "bd-123"})

    def test_missing_file_is_an_empty_allowlist(self):
        with TemporaryDirectory() as tmp:
            self.assertEqual(gate.load_allowlist(Path(tmp) / "nope.txt"), {})


def _workspace(tmp: Path, lib_rs: str, consumer: str | None = None) -> Path:
    """A one-crate workspace, enough to exercise evaluate()."""
    (tmp / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/demo"]\n', encoding="utf-8"
    )
    src = tmp / "crates" / "demo" / "src"
    src.mkdir(parents=True)
    (src / "lib.rs").write_text(lib_rs, encoding="utf-8")
    (src / "thing.rs").write_text("pub struct Thing;\n", encoding="utf-8")
    if consumer is not None:
        (src / "consumer.rs").write_text(consumer, encoding="utf-8")
    return tmp


class Evaluate(unittest.TestCase):
    def test_unreferenced_module_is_unreachable(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(Path(tmp), "pub mod thing;\n")
            findings = gate.evaluate(root, {}, None)
            self.assertEqual(
                [(f.qualified, f.verdict) for f in findings],
                [("demo::thing", "UNREACHABLE")],
            )

    def test_referenced_module_is_ok(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(
                Path(tmp),
                "pub mod thing;\npub mod consumer;\n",
                consumer="pub fn f() { crate::thing::Thing; }\n",
            )
            verdicts = {f.qualified: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["demo::thing"], "OK")

    def test_reexported_module_is_reachable(self):
        # `pub use thing::Thing;` makes the module public API; consumers write
        # `demo::Thing` and never name the module.
        with TemporaryDirectory() as tmp:
            root = _workspace(Path(tmp), "pub mod thing;\npub use thing::Thing;\n")
            verdicts = {f.qualified: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["demo::thing"], "OK")

    def test_experimental_module_is_exempt(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(
                Path(tmp),
                '#[cfg(feature = "experimental")]\npub mod thing;\n',
            )
            verdicts = {f.qualified: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["demo::thing"], "EXPERIMENTAL")

    def test_feature_gated_module_is_exempt(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(Path(tmp), '#[cfg(feature = "bidi")]\npub mod thing;\n')
            verdicts = {f.qualified: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["demo::thing"], "FEATURE_GATED")

    def test_reexport_only_module_is_exempt(self):
        # The facade's `pub mod advanced { pub use ftui_layout::..::*; }` IS
        # the public API - consumers reach it as `ftui::..::advanced::*` and no
        # workspace file names it. Reporting that as dead teaches people to
        # ignore the gate.
        with TemporaryDirectory() as tmp:
            root = _workspace(
                Path(tmp),
                "pub mod thing;\n"
                "pub mod facade {\n"
                "    // Re-exports only.\n"
                "    pub use crate::thing::Thing;\n"
                "}\n",
            )
            verdicts = {f.qualified: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["demo::facade"], "REEXPORT_ONLY")

    def test_inline_module_with_real_items_is_not_exempt(self):
        # The exemption must not swallow inline modules that carry code;
        # ftui-core::text_width is inline and very much alive.
        with TemporaryDirectory() as tmp:
            root = _workspace(
                Path(tmp),
                "pub mod inline_code {\n"
                "    pub use crate::thing::Thing;\n"
                "    pub fn helper() -> u8 {\n"
                "        1\n"
                "    }\n"
                "}\n",
            )
            verdicts = {f.qualified: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["demo::inline_code"], "UNREACHABLE")

    def test_module_used_only_by_tests_is_test_only(self):
        # "nothing uses this" and "only tests use this" are different states.
        # ftui_render::headless is 843 lines of documented CI harness with two
        # test consumers; reporting it as dead code is what makes a gate get
        # ignored, and acting on that report would break the tests.
        with TemporaryDirectory() as tmp:
            root = _workspace(Path(tmp), "pub mod thing;\n")
            tests = root / "crates" / "demo" / "tests"
            tests.mkdir(parents=True)
            (tests / "uses_thing.rs").write_text(
                "use demo::thing::Thing;\n#[test]\nfn t() { let _ = Thing; }\n",
                encoding="utf-8",
            )
            verdicts = {f.qualified: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["demo::thing"], "TEST_ONLY")

    def test_test_only_module_makes_its_allowlist_entry_stale(self):
        # Test infrastructure does not belong on a dead-code list, so the entry
        # should be reported as stale rather than silently accepted.
        with TemporaryDirectory() as tmp:
            root = _workspace(Path(tmp), "pub mod thing;\n")
            tests = root / "crates" / "demo" / "tests"
            tests.mkdir(parents=True)
            (tests / "uses_thing.rs").write_text(
                "use demo::thing::Thing;\n", encoding="utf-8"
            )
            findings = gate.evaluate(root, {"demo::thing": "bd-123"}, None)
            self.assertEqual(findings[0].verdict, "STALE_ALLOWLIST")

    def test_allowlisted_module_passes(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(Path(tmp), "pub mod thing;\n")
            findings = gate.evaluate(root, {"demo::thing": "bd-123"}, None)
            self.assertEqual(findings[0].verdict, "ALLOWLISTED")

    def test_allowlist_entry_that_became_reachable_is_stale(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(
                Path(tmp),
                "pub mod thing;\npub mod consumer;\n",
                consumer="pub fn f() { crate::thing::Thing; }\n",
            )
            findings = gate.evaluate(root, {"demo::thing": "bd-123"}, None)
            verdicts = {f.qualified: f.verdict for f in findings}
            self.assertEqual(verdicts["demo::thing"], "STALE_ALLOWLIST")

    def test_module_own_file_does_not_count_as_a_reference(self):
        # thing.rs saying `thing::` about itself proves nothing.
        with TemporaryDirectory() as tmp:
            root = _workspace(Path(tmp), "pub mod thing;\n")
            (root / "crates" / "demo" / "src" / "thing.rs").write_text(
                "pub struct Thing;\npub fn f() { thing::Thing; }\n", encoding="utf-8"
            )
            verdicts = {f.qualified: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["demo::thing"], "UNREACHABLE")

    def test_reference_from_a_test_block_does_not_count(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(
                Path(tmp),
                "pub mod thing;\npub mod consumer;\n",
                consumer=(
                    "pub fn f() {}\n"
                    "#[cfg(test)]\nmod tests {\n"
                    "    use crate::thing::Thing;\n}\n"
                ),
            )
            verdicts = {f.qualified: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["demo::thing"], "UNREACHABLE")


class DeclaredModules(unittest.TestCase):
    def test_recognises_inline_modules(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(Path(tmp), "pub mod inline_one {\n    pub fn f() {}\n}\n")
            modules = gate.declared_modules("demo", root / "crates" / "demo")
            self.assertEqual([m.name for m in modules], ["inline_one"])
            self.assertTrue(modules[0].inline)

    def test_attribute_run_does_not_leak_to_the_next_module(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(
                Path(tmp),
                '#[cfg(feature = "experimental")]\npub mod gated;\npub mod plain;\n',
            )
            modules = {m.name: m for m in gate.declared_modules("demo", root / "crates" / "demo")}
            self.assertTrue(modules["gated"].experimental)
            self.assertFalse(modules["plain"].experimental)


class ModuleReachabilityGateContract(unittest.TestCase):
    """Exact named tests specified in bd-g00-root-epic-ewths.11.4."""

    def test_declares_file_and_inline_modules(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(Path(tmp), "pub mod a;\npub mod b {\n    pub fn f() {}\n}\n")
            modules = {m.name: m for m in gate.declared_modules("demo", root / "crates" / "demo")}
            self.assertIn("a", modules)
            self.assertFalse(modules["a"].inline)
            self.assertIn("b", modules)
            self.assertTrue(modules["b"].inline)

    def test_experimental_cfg_is_skipped(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(
                Path(tmp),
                '#[cfg(feature = "experimental")]\npub mod x;\n',
            )
            verdicts = {f.name: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["x"], "EXPERIMENTAL")

    def test_reference_in_cfg_test_block_does_not_count(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(
                Path(tmp),
                "pub mod thing;\npub mod consumer;\n",
                consumer=(
                    "pub fn f() {}\n"
                    "#[cfg(test)]\nmod tests {\n"
                    "    use crate::thing::Thing;\n}\n"
                ),
            )
            verdicts = {f.qualified: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["demo::thing"], "UNREACHABLE")

    def test_reference_from_own_dir_does_not_count(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(Path(tmp), "pub mod x;\n")
            x_dir = root / "crates" / "demo" / "src" / "x"
            x_dir.mkdir(parents=True)
            (x_dir / "inner.rs").write_text("use crate::x;\n", encoding="utf-8")
            verdicts = {f.name: f.verdict for f in gate.evaluate(root, {}, None)}
            self.assertEqual(verdicts["x"], "UNREACHABLE")

    def test_cross_crate_use_counts(self):
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.toml").write_text(
                '[workspace]\nmembers = ["crates/ftui-a", "crates/ftui-b"]\n', encoding="utf-8"
            )
            src_a = root / "crates" / "ftui-a" / "src"
            src_a.mkdir(parents=True)
            (src_a / "lib.rs").write_text("pub mod x;\n", encoding="utf-8")
            (src_a / "x.rs").write_text("pub struct Thing;\n", encoding="utf-8")

            src_b = root / "crates" / "ftui-b" / "src"
            src_b.mkdir(parents=True)
            (src_b / "lib.rs").write_text("use ftui_a::x::Thing;\n", encoding="utf-8")

            findings = {f.qualified: f for f in gate.evaluate(root, {}, None)}
            self.assertEqual(findings["ftui-a::x"].verdict, "OK")
            self.assertIn("crates/ftui-b/src/lib.rs", findings["ftui-a::x"].detail)

    def test_showcase_reference_counts_for_widgets(self):
        root = Path(__file__).resolve().parent.parent.parent
        findings = {f.qualified: f for f in gate.evaluate(root, {}, "ftui-widgets")}
        self.assertIn("ftui-widgets::decision_card", findings)
        self.assertEqual(findings["ftui-widgets::decision_card"].verdict, "OK")
        self.assertIn(
            "crates/ftui-demo-showcase/src/screens/widget_gallery.rs",
            findings["ftui-widgets::decision_card"].detail,
        )

    def test_allowlist_only_shrinks(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(
                Path(tmp),
                "pub mod thing;\npub mod consumer;\n",
                consumer="pub fn f() { crate::thing::Thing; }\n",
            )
            findings = gate.evaluate(root, {"demo::thing": "bd-123"}, None)
            verdicts = {f.qualified: f.verdict for f in findings}
            self.assertEqual(verdicts["demo::thing"], "STALE_ALLOWLIST")

    def test_json_output_schema(self):
        with TemporaryDirectory() as tmp:
            root = _workspace(Path(tmp), "pub mod thing;\n")
            json_file = Path(tmp) / "report.json"
            empty_allowlist = Path(tmp) / "empty.txt"
            empty_allowlist.write_text("", encoding="utf-8")
            old_argv = sys.argv
            sys.argv = [
                "check_module_reachability.py",
                "--root",
                str(root),
                "--allowlist",
                str(empty_allowlist),
                "--json",
                str(json_file),
            ]
            try:
                gate.main()
            finally:
                sys.argv = old_argv

            data = json.loads(json_file.read_text(encoding="utf-8"))
            for key in [
                "schema",
                "git_commit",
                "generated_at",
                "crates",
                "unreachable",
                "stale_allowlist",
                "summary",
            ]:
                self.assertIn(key, data)

    def test_exit_code_zero_on_clean_tree(self):
        root = Path(__file__).resolve().parent.parent.parent
        allowlist = root / "docs" / "module-reachability-allowlist.txt"
        old_argv = sys.argv
        sys.argv = [
            "check_module_reachability.py",
            "--root",
            str(root),
            "--allowlist",
            str(allowlist),
            "--quiet",
        ]
        try:
            code = gate.main()
            self.assertEqual(code, 0)
        finally:
            sys.argv = old_argv

    declares_file_and_inline_modules = test_declares_file_and_inline_modules
    experimental_cfg_is_skipped = test_experimental_cfg_is_skipped
    reference_in_cfg_test_block_does_not_count = test_reference_in_cfg_test_block_does_not_count
    reference_from_own_dir_does_not_count = test_reference_from_own_dir_does_not_count
    cross_crate_use_counts = test_cross_crate_use_counts
    showcase_reference_counts_for_widgets = test_showcase_reference_counts_for_widgets
    allowlist_only_shrinks = test_allowlist_only_shrinks
    json_output_schema = test_json_output_schema
    exit_code_zero_on_clean_tree = test_exit_code_zero_on_clean_tree


if __name__ == "__main__":
    unittest.main()

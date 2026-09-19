#!/usr/bin/env python3
"""Fail when a public module is not reachable from production code.

Background (bd-g00-root-epic-ewths.11.2, gap G07): the 2026-09-01 reality check
found that "reachable from production" had never been part of the definition of
done, so 30 of ftui-runtime's 64 modules, three width caches, two e-process
controllers and two orphan files had accumulated with nothing to notice them.
Quarantining that set fixes today; this gate is what stops the next one.

A module is reachable when some non-test file, outside the module's own file or
directory, mentions it. Modules behind `#[cfg(feature = "experimental")]` are
exempt by construction: the feature exists to hold code that is deliberately
not wired up.

The allowlist carries modules that are known-unreachable and have a bead that
will wire or quarantine them. It may only shrink: a module that becomes
reachable while still allowlisted is an error, so entries cannot outlive their
reason.

Pure stdlib on purpose. This runs in environments where ripgrep is not
installed, so it must not shell out to one.

Exit status is 0 when every module is reachable, exempt or allowlisted, and 1
when any module is unreachable or any allowlist entry has gone stale.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import os
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

SCHEMA = "module-reachability-v1"

# `pub mod name;` and `pub mod name {`, with an optional attribute run above it.
MODULE_RE = re.compile(r"^[ \t]*pub mod (?P<name>[a-z0-9_]+)[ \t]*[;{]", re.MULTILINE)
ATTR_RE = re.compile(r"^[ \t]*#\[(?P<body>.+)\][ \t]*$")

# Directories whose contents never count as production references.
NON_PRODUCTION_DIRS = {"tests", "benches", "examples", "fuzz"}

# Crates that exist to be used by tests. Their modules are reached from `tests/`
# by design, so for these — and only these — a test file counts as production.
# Holding them to the library rule would report a test harness as dead code for
# doing its job.
TEST_SUPPORT_CRATES = {"ftui-harness", "ftui-pty"}


@dataclass
class Module:
    """A `pub mod` declaration in some crate's `lib.rs`."""

    crate: str
    name: str
    experimental: bool
    feature_gated: bool
    inline: bool
    reexport_only: bool = False
    own_paths: list[Path] = field(default_factory=list)


@dataclass
class Finding:
    crate: str
    name: str
    verdict: str
    detail: str = ""

    @property
    def qualified(self) -> str:
        return f"{self.crate}::{self.name}"


def crate_snake(crate: str) -> str:
    """`ftui-core` -> `ftui_core`, the name other crates import it by."""
    return crate.replace("-", "_")


def discover_crates(root: Path) -> list[tuple[str, Path]]:
    """Workspace members that have a `src/lib.rs`, in manifest order.

    Binary-only members have no module surface to check and are skipped, but
    they are still scanned for references later: the showcase is exactly where
    a widget is expected to be used.
    """
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    crates: list[tuple[str, Path]] = []
    for member in manifest.get("workspace", {}).get("members", []):
        crate_dir = root / member
        if not (crate_dir / "src" / "lib.rs").is_file():
            continue
        crates.append((crate_dir.name, crate_dir))
    return crates


def _skip_to_item_end(text: str, start: int) -> int:
    """End offset of the item beginning at or after `start`.

    Brace-matched, skipping string and character literals and comments so that
    a `format!("{}")` inside a test cannot end the block early. An attribute
    that decorates a `use` or a `const` with no block ends at its semicolon.
    """
    i = start
    n = len(text)
    depth = 0
    while i < n:
        ch = text[i]

        if ch == "/" and i + 1 < n:
            if text[i + 1] == "/":
                i = text.find("\n", i)
                if i == -1:
                    return n
                continue
            if text[i + 1] == "*":
                end = text.find("*/", i + 2)
                i = n if end == -1 else end + 2
                continue

        if ch == '"':
            # Raw strings (r"", r#""#) can contain anything, including quotes.
            raw_hashes = 0
            j = i - 1
            while j >= 0 and text[j] == "#":
                raw_hashes += 1
                j -= 1
            if j >= 0 and text[j] == "r":
                closing = '"' + "#" * raw_hashes
                end = text.find(closing, i + 1)
                i = n if end == -1 else end + len(closing)
                continue
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                    continue
                if text[i] == '"':
                    i += 1
                    break
                i += 1
            continue

        if ch == "'":
            # A lifetime, not a character literal, when no closing quote is near.
            if i + 2 < n and text[i + 1] == "\\":
                end = text.find("'", i + 2)
                i = n if end == -1 else end + 1
                continue
            if i + 2 < n and text[i + 2] == "'":
                i += 3
                continue
            i += 1
            continue

        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        elif ch == ";" and depth == 0:
            return i + 1

        i += 1

    return n


def strip_test_regions(text: str) -> str:
    """Blank out every `#[cfg(test)]` item, leaving the rest intact.

    Cutting the file at the first marker instead would be catastrophic here:
    `ftui-runtime/src/program.rs` carries an inline `#[cfg(test)]` at line 4190
    of 19204, so that shortcut discards three quarters of the largest file in
    the workspace and reports whatever it references as dead.

    Stripped spans are replaced by their own newlines so reported line numbers
    still match the file on disk.
    """
    out = text
    search_from = 0
    while True:
        marker = out.find("#[cfg(test)]", search_from)
        if marker == -1:
            return out
        end = _skip_to_item_end(out, marker + len("#[cfg(test)]"))
        removed = out[marker:end]
        out = out[:marker] + "\n" * removed.count("\n") + out[end:]
        search_from = marker + removed.count("\n")


def is_reexport_only(body: str) -> bool:
    """Whether an inline module's body is nothing but re-exports.

    A module like the facade's

        pub mod advanced {
            pub use ftui_layout::pane_execution::*;
            ...
        }

    *is* the public API: consumers reach it as `ftui::..::advanced::*` and no
    workspace file needs to name it. Reporting those as dead code is the kind
    of false positive that teaches people to ignore the gate, so they are
    exempt. A module with any real item in it is not covered by this.
    """
    meaningful = [
        line.strip()
        for line in body.splitlines()
        if line.strip() and not line.strip().startswith("//")
    ]
    if not meaningful:
        return False
    return all(
        line.startswith("pub use ") or line in {"{", "}"} for line in meaningful
    )


def declared_modules(crate: str, crate_dir: Path) -> list[Module]:
    """Parse `pub mod` declarations, carrying any `#[cfg(...)]` above them."""
    lib_rs = crate_dir / "src" / "lib.rs"
    source = lib_rs.read_text(encoding="utf-8")
    lines = source.splitlines()

    modules: list[Module] = []
    pending_attrs: list[str] = []
    for line in lines:
        attr = ATTR_RE.match(line)
        if attr:
            pending_attrs.append(attr.group("body"))
            continue

        match = MODULE_RE.match(line)
        if not match:
            # Any other non-blank line ends the attribute run.
            if line.strip():
                pending_attrs = []
            continue

        name = match.group("name")
        experimental = any('feature = "experimental"' in a for a in pending_attrs)
        # Any `#[cfg(feature = ...)]` module is conditionally compiled, so
        # whether it is referenced depends on a feature set this scan does not
        # resolve. `experimental` is the case the reality check named; the rest
        # are exempt for the same reason rather than reported as dead.
        gated = any("feature = " in a for a in pending_attrs)
        inline = line.rstrip().endswith("{")
        reexport_only = False
        if inline:
            decl = source.find(line)
            if decl != -1:
                # Scan from the opening brace, then keep only what is between
                # the braces: the declaration line itself is not part of the
                # body and would never look like a re-export.
                brace = decl + len(line) - 1
                end = _skip_to_item_end(source, brace)
                reexport_only = is_reexport_only(source[brace + 1 : end - 1])
        own = [crate_dir / "src" / f"{name}.rs", crate_dir / "src" / name]
        modules.append(
            Module(
                crate=crate,
                name=name,
                experimental=experimental,
                feature_gated=gated and not experimental,
                inline=inline,
                reexport_only=reexport_only,
                own_paths=own,
            )
        )
        pending_attrs = []

    return modules


def production_files(root: Path, crates: list[tuple[str, Path]]) -> list[Path]:
    """Every `.rs` file that counts as production code, across the workspace.

    Includes `src/**` of every workspace member (not only the library ones, so
    a binary crate's `main.rs` counts), plus `examples/**`, which is where the
    harness demonstrates the types it exports.
    """
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    members = manifest.get("workspace", {}).get("members", [])

    files: list[Path] = []
    for member in members:
        crate_dir = root / member
        for sub in ("src", "examples"):
            base = crate_dir / sub
            if not base.is_dir():
                continue
            for dirpath, dirnames, filenames in os.walk(base):
                dirnames[:] = [d for d in dirnames if d not in NON_PRODUCTION_DIRS]
                for filename in filenames:
                    if filename.endswith(".rs"):
                        files.append(Path(dirpath) / filename)
    return files


# A path segment naming a module: the `foo` of `foo::Bar` *and* of
# `ftui_widgets::foo::Bar`. The lookbehind deliberately allows a preceding `:`,
# since a nested segment is the common way one crate names another's module.
PATH_SEGMENT_RE = re.compile(r"(?<![A-Za-z0-9_])([a-z_][a-z0-9_]*)::")
# The tail of `use some::path::module;`, which has no trailing `::`.
USE_TAIL_RE = re.compile(r"\buse [^;{]*::([a-z_][a-z0-9_]*)[ \t]*(?:as [A-Za-z_]\w*)?;")
# Lowercase names inside `use some::path::{a, b::C, d};`.
USE_GROUP_RE = re.compile(r"\buse [^;]*::\{([^}]*)\}")
GROUP_NAME_RE = re.compile(r"(?<![\w:])([a-z_][a-z0-9_]*)")


def test_files(root: Path) -> list[Path]:
    """Every `.rs` file under a `tests/` directory, plus the workspace suite."""
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    members = manifest.get("workspace", {}).get("members", [])

    bases = [root / member / "tests" for member in members]
    bases.append(root / "tests")

    files: list[Path] = []
    for base in bases:
        if not base.is_dir():
            continue
        for dirpath, _dirnames, filenames in os.walk(base):
            for filename in filenames:
                if filename.endswith(".rs"):
                    files.append(Path(dirpath) / filename)
    return files


def is_own_file(path: Path, module: Module) -> bool:
    for own in module.own_paths:
        if path == own:
            return True
        if own.is_dir() and own in path.parents:
            return True
    return False


def build_index(files: list[Path]) -> tuple[list[Path], dict[str, dict[int, int]]]:
    """Map every module-ish name to the files and lines that mention it.

    One pass over the workspace, not one pass per module: the obvious shape of
    this check rescans ~30 MB of Rust for each of several hundred modules and
    takes minutes. Inverting it takes seconds, which is the difference between
    a gate people run and a gate people skip.
    """
    paths: list[Path] = []
    index: dict[str, dict[int, int]] = {}

    def note(name: str, file_idx: int, line_no: int) -> None:
        seen = index.setdefault(name, {})
        if file_idx not in seen:
            seen[file_idx] = line_no

    for path in files:
        try:
            text = strip_test_regions(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError):
            continue

        file_idx = len(paths)
        paths.append(path)

        for match in PATH_SEGMENT_RE.finditer(text):
            note(match.group(1), file_idx, text.count("\n", 0, match.start()) + 1)
        for match in USE_TAIL_RE.finditer(text):
            note(match.group(1), file_idx, text.count("\n", 0, match.start()) + 1)
        for match in USE_GROUP_RE.finditer(text):
            line_no = text.count("\n", 0, match.start()) + 1
            for inner in GROUP_NAME_RE.finditer(match.group(1)):
                note(inner.group(1), file_idx, line_no)

    return paths, index


def find_reference(
    module: Module,
    paths: list[Path],
    index: dict[str, dict[int, int]],
    root: Path,
) -> tuple[Path, int] | None:
    """First production reference to `module` from outside its own code."""
    lib_rs = root / "crates" / module.crate / "src" / "lib.rs"
    for file_idx, line_no in index.get(module.name, {}).items():
        path = paths[file_idx]
        # The declaring lib.rs names every module by definition, so it says
        # nothing about whether anyone uses one. Re-exports are judged
        # separately, by `reexports_module`.
        if path == lib_rs or is_own_file(path, module):
            continue
        return (path.relative_to(root), line_no)
    return None


def reexports_module(module: Module, root: Path) -> int | None:
    """Line of the crate-root `pub use <module>::...`, if there is one.

    A module the crate re-exports is part of its public API, and consumers
    reach it by the re-exported name (`ftui_widgets::Tabs`) without ever
    writing the module path. Treating those as unreachable would condemn most
    of the widget library.
    """
    lib_rs = root / "crates" / module.crate / "src" / "lib.rs"
    try:
        text = lib_rs.read_text(encoding="utf-8")
    except OSError:
        return None
    pattern = re.compile(rf"^[ \t]*pub use {re.escape(module.name)}::", re.MULTILINE)
    match = pattern.search(text)
    if not match:
        return None
    return text.count("\n", 0, match.start()) + 1


def load_allowlist(path: Path) -> dict[str, str]:
    """Map `crate::module` to the bead id that will resolve it."""
    entries: dict[str, str] = {}
    if not path.is_file():
        return entries
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        module, _, comment = line.partition("#")
        module = module.strip()
        bead = comment.strip()
        if not module:
            continue
        if not bead:
            raise SystemExit(
                f"{path}: '{module}' has no bead id. Every allowlist entry needs "
                f"the bead that will wire or quarantine it."
            )
        entries[module] = bead
    return entries


def evaluate(root: Path, allowlist: dict[str, str], only: str | None) -> list[Finding]:
    crates = discover_crates(root)
    if only:
        crates = [(name, d) for (name, d) in crates if name == only]
        if not crates:
            raise SystemExit(f"no workspace crate named {only!r} with a src/lib.rs")

    paths, index = build_index(production_files(root, crates))
    # A second index that also counts test files, consulted only for the
    # test-support crates.
    test_paths, test_index = build_index(test_files(root))
    findings: list[Finding] = []

    for crate, crate_dir in crates:
        for module in declared_modules(crate, crate_dir):
            qualified = f"{crate}::{module.name}"

            if module.experimental:
                findings.append(Finding(crate, module.name, "EXPERIMENTAL"))
                continue
            if module.feature_gated:
                findings.append(Finding(crate, module.name, "FEATURE_GATED"))
                continue
            if module.reexport_only:
                findings.append(Finding(crate, module.name, "REEXPORT_ONLY"))
                continue

            hit = find_reference(module, paths, index, root)
            detail = f"{hit[0]}:{hit[1]}" if hit else ""
            if not hit and crate in TEST_SUPPORT_CRATES:
                hit = find_reference(module, test_paths, test_index, root)
                if hit:
                    detail = f"test consumer {hit[0]}:{hit[1]}"
            if not hit:
                reexport_line = reexports_module(module, root)
                if reexport_line is not None:
                    hit = (Path(f"crates/{crate}/src/lib.rs"), reexport_line)
                    detail = f"re-exported at {hit[0]}:{reexport_line}"

            allowlisted = qualified in allowlist

            if hit and allowlisted:
                findings.append(
                    Finding(
                        crate, module.name, "STALE_ALLOWLIST", f"now reachable: {detail}"
                    )
                )
            elif hit:
                findings.append(Finding(crate, module.name, "OK", detail))
            elif allowlisted:
                findings.append(
                    Finding(crate, module.name, "ALLOWLISTED", allowlist[qualified])
                )
            else:
                findings.append(Finding(crate, module.name, "UNREACHABLE"))

    return findings


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Fail when a public module is unreachable from production code."
    )
    parser.add_argument(
        "--allowlist",
        type=Path,
        default=Path("docs/module-reachability-allowlist.txt"),
        help="known-unreachable modules, each with the bead that resolves it",
    )
    parser.add_argument("--json", type=Path, help="also write the report here")
    parser.add_argument("--crate", help="check a single crate (for local runs)")
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="workspace root (defaults to this script's repository)",
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="print only failures and the summary",
    )
    args = parser.parse_args()

    root = args.root.resolve()
    allowlist_path = (
        args.allowlist if args.allowlist.is_absolute() else root / args.allowlist
    )
    allowlist = load_allowlist(allowlist_path)

    findings = evaluate(root, allowlist, args.crate)

    failures = [f for f in findings if f.verdict in ("UNREACHABLE", "STALE_ALLOWLIST")]

    for finding in findings:
        if args.quiet and finding.verdict not in ("UNREACHABLE", "STALE_ALLOWLIST"):
            continue
        suffix = f" ({finding.detail})" if finding.detail else ""
        print(f"{finding.verdict} {finding.qualified}{suffix}")

    counts: dict[str, int] = {}
    for finding in findings:
        counts[finding.verdict] = counts.get(finding.verdict, 0) + 1
    summary = "  ".join(f"{k}={v}" for k, v in sorted(counts.items()))
    print(f"\n{len(findings)} modules: {summary}")

    if args.json:
        git_commit = "unknown"
        try:
            res = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD"],
                capture_output=True,
                text=True,
                check=False,
            )
            if res.returncode == 0:
                git_commit = res.stdout.strip()
        except Exception:
            pass

        report = {
            "schema": SCHEMA,
            "git_commit": git_commit,
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "crates": sorted({f.crate for f in findings}),
            "modules": [
                {
                    "crate": f.crate,
                    "module": f.name,
                    "verdict": f.verdict,
                    "detail": f.detail,
                }
                for f in findings
            ],
            "unreachable": [f.qualified for f in findings if f.verdict == "UNREACHABLE"],
            "stale_allowlist": [
                f.qualified for f in findings if f.verdict == "STALE_ALLOWLIST"
            ],
            "summary": {
                "ok": counts.get("OK", 0),
                "experimental": counts.get("EXPERIMENTAL", 0),
                "feature_gated": counts.get("FEATURE_GATED", 0),
                "allowlisted": counts.get("ALLOWLISTED", 0),
                "unreachable": counts.get("UNREACHABLE", 0),
                "stale_allowlist": counts.get("STALE_ALLOWLIST", 0),
                "stale": counts.get("STALE_ALLOWLIST", 0),
            },
        }
        json_path = args.json if args.json.is_absolute() else root / args.json
        json_path.parent.mkdir(parents=True, exist_ok=True)
        json_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    if failures:
        print(
            f"\nFAILED: {len(failures)} module(s) need wiring, quarantining, or an "
            f"allowlist entry with a bead id.",
            file=sys.stderr,
        )
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())

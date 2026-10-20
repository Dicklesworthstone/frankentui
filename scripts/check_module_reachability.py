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


def strip_comments_and_strings(text: str, strip_strings: bool = True) -> str:
    """Replace comments (and optionally string/char literals) with spaces, preserving newlines.

    Prevents string literals like `"cache::redis"` or doc comments mentioning
    another crate's module from counting as code references, while keeping line
    numbers identical to the file on disk.
    """
    out: list[str] = []
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if ch == "/" and i + 1 < n:
            if text[i + 1] == "/":
                end = text.find("\n", i + 2)
                if end == -1:
                    out.append(" " * (n - i))
                    break
                out.append(" " * (end - i))
                i = end
                continue
            if text[i + 1] == "*":
                depth = 1
                j = i + 2
                while j < n and depth > 0:
                    if text[j : j + 2] == "/*":
                        depth += 1
                        j += 2
                    elif text[j : j + 2] == "*/":
                        depth -= 1
                        j += 2
                    else:
                        j += 1
                span = text[i:j]
                out.append("".join("\n" if c == "\n" else " " for c in span))
                i = j
                continue
        if ch == '"':
            raw_hashes = 0
            k = i - 1
            while k >= 0 and text[k] == "#":
                raw_hashes += 1
                k -= 1
            is_raw = False
            if k >= 0 and text[k] == "r":
                if k == 0 or not (text[k - 1].isalnum() or text[k - 1] == "_"):
                    is_raw = True
                elif k >= 1 and text[k - 1] in ("b", "c") and (
                    k == 1 or not (text[k - 2].isalnum() or text[k - 2] == "_")
                ):
                    is_raw = True
            if is_raw:
                closing = '"' + "#" * raw_hashes
                end = text.find(closing, i + 1)
                j = n if end == -1 else end + len(closing)
                span = text[i:j]
                if strip_strings:
                    out.append("".join("\n" if c == "\n" else " " for c in span))
                else:
                    out.append(span)
                i = j
                continue
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == '"':
                    j += 1
                    break
                j += 1
            span = text[i:j]
            if strip_strings:
                out.append("".join("\n" if c == "\n" else " " for c in span))
            else:
                out.append(span)
            i = j
            continue
        if ch == "'":
            if i + 2 < n and text[i + 1] == "\\":
                end = text.find("'", i + 2)
                if end != -1 and end - i <= 12:
                    out.append(
                        " " * (end + 1 - i) if strip_strings else text[i : end + 1]
                    )
                    i = end + 1
                    continue
            elif i + 2 < n and text[i + 2] == "'" and text[i + 1] != "\n":
                out.append("   " if strip_strings else text[i : i + 3])
                i += 3
                continue
        out.append(ch)
        i += 1
    return "".join(out)


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


CFG_ATTR_RE = re.compile(r"#\[\s*cfg\s*\(([^]]*)\)\s*\]")


def strip_disabled_cfg_regions(
    text: str, crate_enabled: set[str] | None
) -> str:
    """Blank out `#[cfg(feature = ...)]` items whose feature is not enabled."""
    out = text
    search_from = 0
    while True:
        match = CFG_ATTR_RE.search(out, search_from)
        if not match:
            return out
        body = match.group(1)
        if "not(" in body:
            search_from = match.end()
            continue
        fnames = [m.group(1) for m in FEATURE_NAME_RE.finditer(body)]
        if not fnames:
            search_from = match.end()
            continue
        disabled = "experimental" in fnames or (
            crate_enabled is not None and not any(f in crate_enabled for f in fnames)
        )
        if not disabled:
            search_from = match.end()
            continue
        marker = match.start()
        end = _skip_to_item_end(out, match.end())
        removed = out[marker:end]
        out = out[:marker] + "\n" * removed.count("\n") + out[end:]
        search_from = marker + removed.count("\n")


ATTR_STRIP_RE = re.compile(r"#\[[^\]]*\]")
NESTED_MOD_OPEN_RE = re.compile(r"\bpub\s+mod\s+[a-z0-9_]+\s*\{")


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
    cleaned = strip_comments_and_strings(body)
    cleaned = ATTR_STRIP_RE.sub(" ", cleaned)
    cleaned = NESTED_MOD_OPEN_RE.sub(" ", cleaned)
    cleaned = cleaned.replace("{", " ").replace("}", " ")
    statements = [s.strip() for s in cleaned.split(";") if s.strip()]
    if not statements:
        return False
    return all(stmt.startswith("pub use ") for stmt in statements)


FEATURE_NAME_RE = re.compile(r'feature\s*=\s*"([^"]+)"')


def workspace_enabled_features(root: Path) -> dict[str, set[str]]:
    """Features enabled by workspace members, the public `ftui` facade, or README."""
    manifest_path = root / "Cargo.toml"
    if not manifest_path.is_file():
        return {}
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    members = manifest.get("workspace", {}).get("members", [])

    crate_features: dict[str, dict[str, list[str]]] = {}
    direct_enabled: set[tuple[str, str]] = set()
    ws_deps: dict[str, tuple[bool, list[str]]] = {}

    for dep, spec in manifest.get("workspace", {}).get("dependencies", {}).items():
        if isinstance(spec, dict):
            ws_deps[dep] = (
                spec.get("default-features", True),
                list(spec.get("features", [])),
            )
        else:
            ws_deps[dep] = (True, [])

    for member in members:
        cargo_toml = root / member / "Cargo.toml"
        if not cargo_toml.is_file():
            continue
        c_toml = tomllib.loads(cargo_toml.read_text(encoding="utf-8"))
        cname = c_toml.get("package", {}).get("name", Path(member).name)
        feats = c_toml.get("features", {})
        crate_features[cname] = {
            k: list(v) for k, v in feats.items() if isinstance(v, list)
        }
        if "default" in feats:
            direct_enabled.add((cname, "default"))
        if cname == "ftui":
            for fname in feats:
                if fname != "experimental":
                    direct_enabled.add((cname, fname))

    readme_path = root / "README.md"
    if readme_path.is_file():
        in_ff = False
        for line in readme_path.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.startswith("## "):
                in_ff = line.strip() == "## Feature Flags"
                continue
            if in_ff and line.startswith("| `"):
                parts = [c.strip().strip("`") for c in line.strip("|").split("|")]
                if len(parts) >= 2:
                    direct_enabled.add((parts[0], parts[1]))

    for member in members:
        cargo_toml = root / member / "Cargo.toml"
        if not cargo_toml.is_file():
            continue
        c_toml = tomllib.loads(cargo_toml.read_text(encoding="utf-8"))
        sections = [
            c_toml.get(s, {})
            for s in ("dependencies", "dev-dependencies", "build-dependencies")
        ]
        for tgt in c_toml.get("target", {}).values():
            sections.extend(
                tgt.get(s, {})
                for s in ("dependencies", "dev-dependencies", "build-dependencies")
            )
        for sec in sections:
            for dep, spec in sec.items():
                pkg = spec.get("package", dep) if isinstance(spec, dict) else dep
                use_ws = isinstance(spec, dict) and spec.get("workspace", False)
                def_f = (
                    spec.get("default-features", True)
                    if isinstance(spec, dict)
                    else True
                )
                f_list = (
                    list(spec.get("features", [])) if isinstance(spec, dict) else []
                )
                if use_ws and pkg in ws_deps:
                    ws_def, ws_flist = ws_deps[pkg]
                    def_f = def_f and ws_def
                    f_list = ws_flist + f_list
                if def_f and "default" in crate_features.get(pkg, {}):
                    direct_enabled.add((pkg, "default"))
                for feat in f_list:
                    direct_enabled.add((pkg, feat))

    enabled = set(direct_enabled)
    changed = True
    while changed:
        changed = False
        for cname, feat in list(enabled):
            for item in crate_features.get(cname, {}).get(feat, []):
                if "/" in item:
                    dep, df = item.split("/", 1)
                    dep = dep.rstrip("?")
                    if (dep, df) not in enabled:
                        enabled.add((dep, df))
                        changed = True
                elif not item.startswith("dep:"):
                    if (cname, item) not in enabled:
                        enabled.add((cname, item))
                        changed = True

    by_crate: dict[str, set[str]] = {}
    for cname, feat in enabled:
        by_crate.setdefault(cname, set()).add(feat)
    return by_crate


def declared_modules(
    crate: str,
    crate_dir: Path,
    enabled_features: dict[str, set[str]] | None = None,
) -> list[Module]:
    """Parse `pub mod` declarations, carrying any `#[cfg(...)]` above them."""
    lib_rs = crate_dir / "src" / "lib.rs"
    source = lib_rs.read_text(encoding="utf-8")
    lines = source.splitlines()
    crate_enabled = (
        enabled_features.get(crate, set()) if enabled_features is not None else None
    )

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
        feature_names = [
            m.group(1) for a in pending_attrs for m in FEATURE_NAME_RE.finditer(a)
        ]
        experimental = "experimental" in feature_names
        # A `#[cfg(feature = ...)]` module (other than `experimental`) is only
        # treated as feature-gated when some workspace member, the `ftui`
        # facade, or the README Feature Flags table enables that feature.
        non_exp_features = [f for f in feature_names if f != "experimental"]
        if non_exp_features:
            if crate_enabled is None:
                gated = True
            else:
                gated = any(f in crate_enabled for f in non_exp_features)
        else:
            gated = False
        inline = line.rstrip().endswith("{")
        reexport_only = False
        if inline:
            decl = source.find(line)
            if decl != -1:
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


MOD_DECL_WALK_RE = re.compile(
    r"(?:^|\n)[ \t]*(?:#\[[^\]]*\]\s*)*[ \t]*(?:pub(?:\([^)]*\))?\s+)?mod\s+([a-zA-Z0-9_]+)\s*;"
)
PATH_ATTR_WALK_RE = re.compile(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]')
INCLUDE_WALK_RE = re.compile(r'\binclude!\s*\(\s*"([^"]+)"\s*\)')


def _walk_compiled_file(
    entry: Path,
    seen: set[Path],
    crate_enabled: set[str] | None,
    is_root: bool = True,
) -> None:
    entry = entry.resolve()
    if not entry.is_file() or entry in seen:
        return
    seen.add(entry)
    try:
        raw = entry.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return
    cleaned = strip_comments_and_strings(
        strip_disabled_cfg_regions(strip_test_regions(raw), crate_enabled),
        strip_strings=False,
    )
    base_dir = (
        entry.parent
        if (is_root or entry.name == "mod.rs")
        else (entry.parent / entry.stem)
    )
    for match in MOD_DECL_WALK_RE.finditer(cleaned):
        full = match.group(0)
        fnames = [m.group(1) for m in FEATURE_NAME_RE.finditer(full)]
        if "experimental" in fnames:
            continue
        if fnames and crate_enabled is not None and not any(
            f in crate_enabled for f in fnames
        ):
            continue
        name = match.group(1)
        path_match = PATH_ATTR_WALK_RE.search(full)
        if path_match:
            rel = path_match.group(1)
            cand = (base_dir / rel).resolve()
            if not cand.is_file():
                cand = (entry.parent / rel).resolve()
            _walk_compiled_file(cand, seen, crate_enabled, False)
        else:
            for cand in (
                base_dir / f"{name}.rs",
                base_dir / name / "mod.rs",
                entry.parent / f"{name}.rs",
                entry.parent / name / "mod.rs",
            ):
                if cand.is_file():
                    _walk_compiled_file(cand, seen, crate_enabled, False)
                    break
    for inc in INCLUDE_WALK_RE.finditer(cleaned):
        cand = (entry.parent / inc.group(1)).resolve()
        if cand.is_file():
            _walk_compiled_file(cand, seen, crate_enabled, False)


def production_files(
    root: Path,
    crates: list[tuple[str, Path]],
    enabled_features: dict[str, set[str]] | None = None,
) -> tuple[list[Path], dict[Path, str]]:
    """Every compiled `.rs` file that counts as production code across the workspace.

    Walks `mod`, `#[path]`, and `include!` declarations from crate roots
    (`src/lib.rs`, `src/main.rs`, `src/bin/**`, `examples/**`) so uncompiled
    orphan files under `src/` never count as production consumers.
    """
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    members = manifest.get("workspace", {}).get("members", [])

    files: list[Path] = []
    file_crates: dict[Path, str] = {}
    for member in members:
        crate_dir = (root / member).resolve()
        crate_name = crate_dir.name
        crate_enabled = (
            enabled_features.get(crate_name, set())
            if enabled_features is not None
            else None
        )
        seen: set[Path] = set()
        for root_file in (crate_dir / "src" / "lib.rs", crate_dir / "src" / "main.rs"):
            if root_file.is_file():
                _walk_compiled_file(root_file, seen, crate_enabled, True)
        for sub in ("src/bin", "examples"):
            base = crate_dir / sub
            if not base.is_dir():
                continue
            for entry in sorted(base.rglob("*.rs")):
                if entry.name == "main.rs" or entry.parent == base:
                    _walk_compiled_file(entry, seen, crate_enabled, True)
        for path in sorted(seen):
            files.append(path)
            file_crates[path] = crate_name
    return files, file_crates


QUAL_SEGMENT_RE = re.compile(r"\b([a-z_][a-z0-9_]*)::([a-z_][a-z0-9_]*)\b")
QUAL_USE_GROUP_RE = re.compile(r"\buse\s+([a-z_][a-z0-9_]*)::\{([^}]*)\}")
GROUP_NAME_RE = re.compile(r"(?<![\w:])([a-z_][a-z0-9_]*)")


def test_files(root: Path) -> tuple[list[Path], dict[Path, str]]:
    """Every `.rs` file under a `tests/` directory, mapped to its owning crate."""
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    members = manifest.get("workspace", {}).get("members", [])

    files: list[Path] = []
    file_crates: dict[Path, str] = {}
    for member in members:
        crate_dir = (root / member).resolve()
        base = crate_dir / "tests"
        if not base.is_dir():
            continue
        for dirpath, _dirnames, filenames in os.walk(base):
            for filename in sorted(filenames):
                if filename.endswith(".rs"):
                    p = (Path(dirpath) / filename).resolve()
                    files.append(p)
                    file_crates[p] = crate_dir.name
    return files, file_crates


def is_own_file(path: Path, module: Module) -> bool:
    resolved = path.resolve()
    for own in module.own_paths:
        own_res = own.resolve()
        if resolved == own_res:
            return True
        if own_res.is_dir() and own_res in resolved.parents:
            return True
    return False


def build_index(
    files: list[Path],
    file_crates: dict[Path, str],
    snake_to_crate: dict[str, str],
    root: Path,
    enabled_features: dict[str, set[str]] | None = None,
    is_test: bool = False,
) -> tuple[list[Path], dict[tuple[str, str], dict[int, int]]]:
    """Map every `(crate, module)` pair to the files and lines that reference it.

    Resolves crate-qualified paths (`ftui_x::module`) and intra-crate root paths
    (`crate::module`, or `super::module` from a top-level `src/*.rs` child)
    after stripping `#[cfg(test)]` regions (for production files), disabled
    `#[cfg(feature = "...")]` regions, and all comments and string/char literals.
    """
    paths: list[Path] = []
    index: dict[tuple[str, str], dict[int, int]] = {}
    root_res = root.resolve()

    def note(crate_name: str, mod_name: str, file_idx: int, line_no: int) -> None:
        seen = index.setdefault((crate_name, mod_name), {})
        if file_idx not in seen:
            seen[file_idx] = line_no

    for path in files:
        try:
            raw = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue

        own_crate = file_crates.get(path)
        crate_enabled = (
            enabled_features.get(own_crate, set())
            if (enabled_features is not None and own_crate is not None)
            else None
        )
        stripped = (
            strip_disabled_cfg_regions(raw, crate_enabled)
            if is_test
            else strip_disabled_cfg_regions(strip_test_regions(raw), crate_enabled)
        )
        text = strip_comments_and_strings(stripped, strip_strings=True)

        file_idx = len(paths)
        paths.append(path)
        is_top_level_src = (
            own_crate is not None
            and path.parent == (root_res / "crates" / own_crate / "src")
        )

        for match in QUAL_SEGMENT_RE.finditer(text):
            prefix, mod_name = match.group(1), match.group(2)
            line_no = text.count("\n", 0, match.start()) + 1
            if prefix in ("crate", "self"):
                if own_crate:
                    note(own_crate, mod_name, file_idx, line_no)
            elif prefix == "super":
                if own_crate and is_top_level_src:
                    note(own_crate, mod_name, file_idx, line_no)
            elif prefix in snake_to_crate:
                note(snake_to_crate[prefix], mod_name, file_idx, line_no)

        for match in QUAL_USE_GROUP_RE.finditer(text):
            prefix, group_body = match.group(1), match.group(2)
            line_no = text.count("\n", 0, match.start()) + 1
            target_crate: str | None = None
            if prefix in ("crate", "self"):
                target_crate = own_crate
            elif prefix == "super" and is_top_level_src:
                target_crate = own_crate
            elif prefix in snake_to_crate:
                target_crate = snake_to_crate[prefix]
            if target_crate:
                for inner in GROUP_NAME_RE.finditer(group_body):
                    note(target_crate, inner.group(1), file_idx, line_no)

    return paths, index


def find_reference(
    module: Module,
    paths: list[Path],
    index: dict[tuple[str, str], dict[int, int]],
    root: Path,
) -> tuple[Path, int] | None:
    """First reference to `module` from outside its own code."""
    root_res = root.resolve()
    lib_rs = (root_res / "crates" / module.crate / "src" / "lib.rs").resolve()
    for file_idx, line_no in index.get((module.crate, module.name), {}).items():
        path = paths[file_idx].resolve()
        if path == lib_rs or is_own_file(path, module):
            continue
        return (path.relative_to(root_res), line_no)
    return None


def reexports_module(
    module: Module,
    root: Path,
    enabled_features: dict[str, set[str]] | None = None,
) -> int | None:
    """Line of an enabled crate-root `pub use <module>::...`, if there is one."""
    lib_rs = root / "crates" / module.crate / "src" / "lib.rs"
    try:
        raw = lib_rs.read_text(encoding="utf-8")
    except OSError:
        return None
    crate_enabled = (
        enabled_features.get(module.crate, set())
        if enabled_features is not None
        else None
    )
    text = strip_comments_and_strings(
        strip_disabled_cfg_regions(strip_test_regions(raw), crate_enabled),
        strip_strings=False,
    )
    pub_use_re = re.compile(
        rf"^[ \t]*pub use (?:crate::)?{re.escape(module.name)}::"
    )
    pending_attrs: list[str] = []
    for idx, line in enumerate(text.splitlines(), 1):
        attr = ATTR_RE.match(line)
        if attr:
            pending_attrs.append(attr.group("body"))
            continue
        if pub_use_re.match(line):
            fnames = [
                m.group(1) for a in pending_attrs for m in FEATURE_NAME_RE.finditer(a)
            ]
            if "experimental" in fnames:
                pending_attrs = []
                continue
            if (
                fnames
                and crate_enabled is not None
                and not any(f in crate_enabled for f in fnames)
            ):
                pending_attrs = []
                continue
            return idx
        if line.strip():
            pending_attrs = []
    return None



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


BEAD_ID_RE = re.compile(r"\bbd-[A-Za-z0-9][A-Za-z0-9._-]*")


def load_bead_status(root: Path) -> dict[str, str]:
    """Map bead id to status from the git-tracked Beads export.

    Returns an empty map when the export is absent, which keeps this script
    runnable outside a Beads checkout.
    """
    export = root / ".beads" / "issues.jsonl"
    if not export.is_file():
        return {}
    statuses: dict[str, str] = {}
    for raw in export.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line:
            continue
        try:
            issue = json.loads(line)
        except json.JSONDecodeError:
            continue
        ident = issue.get("id")
        status = issue.get("status")
        if isinstance(ident, str) and isinstance(status, str):
            statuses[ident] = status
    return statuses


def check_allowlist_owners(
    allowlist: dict[str, str],
    statuses: dict[str, str],
    path: Path,
    only: str | None = None,
) -> list[str]:
    """Report allowlist entries whose bead cannot resolve them.

    An entry outlives its owner silently: the bead closes, the module stays
    listed, and nothing ever revisits it. That has stranded two sets of modules
    already (.11.3's widgets, .11.5's harness modules), so a closed or unknown
    owner is an error rather than a note.

    `only` scopes the report to one crate, matching `--crate`, so a local run
    of a single crate is not failed by an unrelated entry.
    """
    if not statuses:
        return []
    errors: list[str] = []
    for module, comment in sorted(allowlist.items()):
        if only and not module.startswith(f"{only}::"):
            continue
        match = BEAD_ID_RE.search(comment)
        if not match:
            # `load_allowlist` only rejects an *empty* comment, so prose alone
            # would otherwise be a way to keep a module listed with no owner at
            # all -- the exact thing the bead id is there to prevent.
            errors.append(
                f"{path}: '{module}' has a comment but no bead id in it. "
                f"Name the bead that will wire or quarantine it."
            )
            continue
        bead = match.group(0)
        status = statuses.get(bead)
        if status is None:
            errors.append(
                f"{path}: '{module}' cites {bead}, which is not in "
                f".beads/issues.jsonl. Check the id, or run "
                f"`br sync --flush-only` if the bead is new."
            )
        elif status == "closed":
            errors.append(
                f"{path}: '{module}' cites {bead}, which is closed. A closed "
                f"bead will never wire or quarantine it; point the entry at a "
                f"bead that owns the decision."
            )
    return errors


def evaluate(root: Path, allowlist: dict[str, str], only: str | None) -> list[Finding]:
    all_crates = discover_crates(root)
    crates = all_crates
    if only:
        crates = [(name, d) for (name, d) in all_crates if name == only]
        if not crates:
            raise SystemExit(f"no workspace crate named {only!r} with a src/lib.rs")

    enabled_features = workspace_enabled_features(root)
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    members = manifest.get("workspace", {}).get("members", [])
    snake_to_crate = {crate_snake(Path(m).name): Path(m).name for m in members}

    prod_list, prod_file_crates = production_files(root, all_crates, enabled_features)
    paths, index = build_index(
        prod_list,
        prod_file_crates,
        snake_to_crate,
        root,
        enabled_features=enabled_features,
        is_test=False,
    )
    # A second index that also counts test files and inline `#[cfg(test)]`
    # regions, consulted only for the test-support crates and TEST_ONLY
    # classification.
    t_list, t_file_crates = test_files(root)
    all_test_files = list(prod_list)
    all_test_crates = dict(prod_file_crates)
    for p in t_list:
        if p not in all_test_crates:
            all_test_files.append(p)
            all_test_crates[p] = t_file_crates[p]
    test_paths, test_index = build_index(
        all_test_files,
        all_test_crates,
        snake_to_crate,
        root,
        enabled_features=enabled_features,
        is_test=True,
    )
    findings: list[Finding] = []

    for crate, crate_dir in crates:
        for module in declared_modules(crate, crate_dir, enabled_features):
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
                reexport_line = reexports_module(module, root, enabled_features)
                if reexport_line is not None:
                    hit = (Path(f"crates/{crate}/src/lib.rs"), reexport_line)
                    detail = f"re-exported at {hit[0]}:{reexport_line}"

            test_hit = None
            if not hit:
                test_hit = find_reference(module, test_paths, test_index, root)

            allowlisted = qualified in allowlist

            if hit and allowlisted:
                findings.append(
                    Finding(
                        crate, module.name, "STALE_ALLOWLIST", f"now reachable: {detail}"
                    )
                )
            elif hit:
                findings.append(Finding(crate, module.name, "OK", detail))
            elif test_hit and allowlisted:
                findings.append(
                    Finding(
                        crate,
                        module.name,
                        "STALE_ALLOWLIST",
                        f"test infrastructure, not dead: {test_hit[0]}:{test_hit[1]}",
                    )
                )
            elif test_hit:
                findings.append(
                    Finding(
                        crate,
                        module.name,
                        "TEST_ONLY",
                        f"only tests use it, e.g. {test_hit[0]}:{test_hit[1]}",
                    )
                )
            elif allowlisted:
                findings.append(
                    Finding(crate, module.name, "ALLOWLISTED", allowlist[qualified])
                )
            else:
                findings.append(Finding(crate, module.name, "UNREACHABLE"))

    return findings


def self_test() -> None:
    # 1. Comment & string stripping preserves line numbers and removes false matches.
    sample = (
        '// fit_metrics::Fake in line comment\n'
        '/* outer /* nested */ debug::Fake */\n'
        'let s = "cache::redis";\n'
        'let r = r#"help_registry::Fake"#;\n'
        'use ftui_core::animation::Spring;\n'
    )
    cleaned = strip_comments_and_strings(sample)
    assert cleaned.count("\n") == sample.count("\n")
    assert "fit_metrics" not in cleaned
    assert "debug" not in cleaned
    assert "cache" not in cleaned
    assert "help_registry" not in cleaned
    assert "use ftui_core::animation::Spring;" in cleaned

    # 2. Inline re-export-only module detection (including nested pub mod and multi-line pub use).
    facade_body = """
        /// Doc comment
        pub use ftui_layout::pane::*;
        #[cfg(feature = "runtime")]
        pub mod keyboard {
            pub use ftui_runtime::pane_keymap::{
                PaneKeymap,
                pane_keyboard_hints,
            };
        }
    """
    assert is_reexport_only(facade_body)
    assert not is_reexport_only("pub use a::b;\npub fn not_reexport() {}")

    # 3. Qualified path resolution on the real workspace:
    #    - `ftui-core::animation` must NOT be marked OK via `ftui-widgets/src/modal/mod.rs`
    #    - `ftui-render::fit_metrics` must NOT be marked OK via doc comment in `vertical_metrics.rs`
    #    - `ftui-layout::debug` must NOT be marked OK via `input_parser.rs`
    root = Path(__file__).resolve().parent.parent
    allowlist = load_allowlist(root / "docs/module-reachability-allowlist.txt")
    by_qual = {f.qualified: f for f in evaluate(root, allowlist, None)}
    assert by_qual["ftui-core::animation"].verdict == "TEST_ONLY", by_qual[
        "ftui-core::animation"
    ]
    assert by_qual["ftui-render::fit_metrics"].verdict == "TEST_ONLY", by_qual[
        "ftui-render::fit_metrics"
    ]
    assert by_qual["ftui-layout::debug"].verdict in ("ALLOWLISTED", "UNREACHABLE"), (
        by_qual["ftui-layout::debug"]
    )
    assert by_qual["ftui-text::normalization"].verdict in (
        "ALLOWLISTED",
        "UNREACHABLE",
    ), by_qual["ftui-text::normalization"]
    print(json.dumps({"scope": "reachability-self-test", "passed": 7}))


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Fail when a public module is unreachable from production code."
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run in-memory and workspace resolution self-tests",
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
    if args.self_test:
        self_test()
        return 0

    root = args.root.resolve()
    allowlist_path = (
        args.allowlist if args.allowlist.is_absolute() else root / args.allowlist
    )
    allowlist = load_allowlist(allowlist_path)
    owner_errors = check_allowlist_owners(
        allowlist, load_bead_status(root), allowlist_path, args.crate
    )

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
            "allowlist_owner_errors": owner_errors,
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

    if owner_errors:
        print(file=sys.stderr)
        for error in owner_errors:
            print(error, file=sys.stderr)

    if failures:
        print(
            f"\nFAILED: {len(failures)} module(s) need wiring, quarantining, or an "
            f"allowlist entry with a bead id.",
            file=sys.stderr,
        )
        return 1

    if owner_errors:
        print(
            f"\nFAILED: {len(owner_errors)} allowlist entry/entries cite a bead "
            f"that cannot resolve them.",
            file=sys.stderr,
        )
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())


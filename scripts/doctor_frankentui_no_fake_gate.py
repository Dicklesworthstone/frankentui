#!/usr/bin/env python3
"""No-fake realism gate for doctor_frankentui.

This is a lightweight CI guardrail to prevent regressions where tests/scripts
silently introduce fake external-tool shims (fake binaries, scripted stubs, etc.).

Policy (current, intentionally simple):
- If a file contains "synthetic helper" signals, it MUST include an explicit allow
  marker with a short justification:

    doctor_frankentui:no-fake-allow

Signals we currently gate on:
- Embedded shebangs inside Rust sources (e.g. "#!/bin/sh" inside test strings).
- Shebangs inside shell scripts beyond the file's own first-line shebang (usually
  indicates the script is generating another executable script).
- References to "fake-cli.sh" (unit-test helper script name).
"""

from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from pathlib import Path

ALLOW_MARKER = "doctor_frankentui:no-fake-allow"
SHEBANG_SNIPPETS = ("#!/bin/", "#!/usr/bin/env")
ALLOW_MARKER_WINDOW_LINES = 40


@dataclass(frozen=True)
class Match:
    kind: str
    line_no: int
    excerpt: str


@dataclass(frozen=True)
class Violation:
    path: Path
    matches: tuple[Match, ...]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Fail if doctor_frankentui contains unannotated fake/shim helpers.",
    )
    parser.add_argument(
        "--root",
        default=".",
        help="Repo root (defaults to cwd).",
    )
    return parser.parse_args()


def iter_scan_files(repo_root: Path) -> list[Path]:
    files: list[Path] = []
    rust_roots = [
        repo_root / "crates" / "doctor_frankentui" / "src",
        repo_root / "crates" / "doctor_frankentui" / "tests",
    ]
    for root in rust_roots:
        if root.exists():
            files.extend(sorted(root.rglob("*.rs")))

    scripts_root = repo_root / "scripts"
    if scripts_root.exists():
        files.extend(sorted(scripts_root.glob("doctor_frankentui_*.sh")))

    return files


def scan_file(path: Path) -> Violation | None:
    text = path.read_text(encoding="utf-8", errors="replace")
    matches: list[Match] = []
    lines = text.splitlines()
    marker_lines = [
        idx for idx, line in enumerate(lines, start=1) if ALLOW_MARKER in line
    ]

    def allowed(match_line: int) -> bool:
        for marker_line in reversed(marker_lines):
            if marker_line <= match_line <= marker_line + ALLOW_MARKER_WINDOW_LINES:
                return True
            if marker_line < match_line - ALLOW_MARKER_WINDOW_LINES:
                return False
        return False

    if path.suffix == ".rs":
        for idx, line in enumerate(lines, start=1):
            if any(snippet in line for snippet in SHEBANG_SNIPPETS):
                if not allowed(idx):
                    matches.append(Match("embedded_shebang", idx, line.strip()))
            if "fake-cli.sh" in line:
                if not allowed(idx):
                    matches.append(Match("fake_cli_script", idx, line.strip()))

    if path.suffix == ".sh":
        for idx, line in enumerate(lines, start=1):
            if idx == 1:
                continue
            if line.startswith("#!/"):
                if not allowed(idx):
                    matches.append(Match("generated_script_shebang", idx, line.strip()))

    if not matches:
        return None

    return Violation(path=path, matches=tuple(matches))


def format_violation(violation: Violation) -> str:
    lines = [f"[no-fake] violation: {violation.path}"]
    for match in violation.matches:
        lines.append(f"  - {match.kind} at line {match.line_no}: {match.excerpt}")
    lines.append(
        f"  fix: add a comment containing '{ALLOW_MARKER}' within "
        f"{ALLOW_MARKER_WINDOW_LINES} lines above the match with justification"
    )
    return "\n".join(lines)


def main() -> int:
    args = parse_args()
    repo_root = Path(args.root).resolve()

    violations: list[Violation] = []
    for path in iter_scan_files(repo_root):
        violation = scan_file(path)
        if violation is not None:
            violations.append(violation)

    if not violations:
        print("[no-fake] PASS: no unannotated fake/shim helpers detected")
        return 0

    print("[no-fake] FAIL: unannotated fake/shim helpers detected", file=sys.stderr)
    for violation in violations:
        print(format_violation(violation), file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())

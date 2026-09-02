#!/usr/bin/env python3
"""E2E for the inline-mode DECSTBM self-test (G05.4).

Runs the demo showcase in inline mode under a PTY while this script plays the
terminal for the self-test: it answers the cursor-position query (`ESC [ 6 n`)
the way a terminal that honours the scroll region would (row = region
bottom), the way one that ignores it would (row = screen bottom), or not at
all. It then checks what the writer did against the evidence file:

- honoured: strategy stays `hybrid` (DECRPM 2026 is answered as unsupported so
  sync output is off), `scroll_region_verified` is true, and the writer emits
  its own DECSTBM region after the probe's.
- ignored:  an `inline_strategy_fallback` row with reason `cpr_mismatch`, the
  strategy is `overlay_redraw`, and the only DECSTBM sequence is the probe's.
- silent:   the same fallback with reason `cpr_timeout`.

Exit status is non-zero when any scenario deviates. The binary is located via
`cargo metadata` unless `FTUI_DEMO_BIN` is set.
"""
from __future__ import annotations

import fcntl
import json
import os
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

ROWS, COLS, UI = 30, 100, 8
REGION_BOTTOM = ROWS - UI
DECSTBM = re.compile(rb"\x1b\[(\d+);(\d+)r")


def find_binary() -> str:
    if env_bin := os.environ.get("FTUI_DEMO_BIN"):
        return env_bin
    meta = subprocess.run(
        ["cargo", "metadata", "--format-version=1", "-q"],
        check=True,
        capture_output=True,
        text=True,
    )
    target = Path(json.loads(meta.stdout)["target_directory"])
    for candidate in (target / "debug" / "ftui-demo-showcase", target / "release" / "ftui-demo-showcase"):
        if candidate.exists():
            return str(candidate)
    raise SystemExit("ftui-demo-showcase binary not found; build it or set FTUI_DEMO_BIN")


def run(binary: str, mode: str, evidence: Path) -> tuple[list[tuple[int, int]], list[dict], int]:
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("WEZTERM", "TMUX", "STY", "ZELLIJ"))
    }
    for key in ("TERM_PROGRAM", "TERM_PROGRAM_VERSION", "FTUI_SCROLL_REGION", "FTUI_SYNC_OUTPUT", "FTUI_CAPS_PROBE"):
        env.pop(key, None)
    env.update(
        {
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "COLUMNS": str(COLS),
            "LINES": str(ROWS),
            "FTUI_DEMO_EVIDENCE_JSONL": str(evidence),
            "FTUI_DEMO_EXIT_AFTER_MS": "1500",
            "FTUI_DEMO_SCREEN_MODE": "inline",
            "FTUI_DEMO_UI_HEIGHT": str(UI),
            "FTUI_DEMO_SCREEN": "2",
        }
    )
    if evidence.exists():
        evidence.unlink()

    pid, fd = pty.fork()
    if pid == 0:
        os.execve(binary, [binary], env)

    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    out = bytearray()
    answered = 0
    start = time.time()
    while True:
        ready, _, _ = select.select([fd], [], [], 0.05)
        if ready:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
            for _ in range(chunk.count(b"\x1b[6n")):
                if mode == "honoured":
                    os.write(fd, f"\x1b[{REGION_BOTTOM};1R".encode())
                elif mode == "ignored":
                    os.write(fd, f"\x1b[{ROWS};1R".encode())
                answered += 1
            for _ in range(chunk.count(b"\x1b[?2026$p")):
                os.write(fd, b"\x1b[?2026;0$y")
        if time.time() - start > 10:
            os.kill(pid, 9)
            break
    os.waitpid(pid, 0)

    rows = []
    if evidence.exists():
        for line in evidence.read_text(encoding="utf-8").splitlines():
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                continue
            if row.get("event") in ("inline_strategy", "inline_strategy_fallback"):
                rows.append(row)
    return [(int(a), int(b)) for a, b in DECSTBM.findall(bytes(out))], rows, answered


def main() -> int:
    binary = find_binary()
    failures: list[str] = []
    with tempfile.TemporaryDirectory(prefix="ftui-inline-selftest-") as tmp:
        for mode in ("honoured", "ignored", "silent"):
            evidence = Path(tmp) / f"{mode}.jsonl"
            regions, rows, answered = run(binary, mode, evidence)
            strategy = next((r for r in rows if r.get("event") == "inline_strategy"), None)
            fallback = next((r for r in rows if r.get("event") == "inline_strategy_fallback"), None)
            summary = {
                "mode": mode,
                "cpr_queries": answered,
                "decstbm": regions[:4],
                "strategy": strategy,
                "fallback": fallback,
            }
            print(json.dumps(summary))
            if answered == 0:
                failures.append(f"{mode}: the self-test never asked for the cursor position")
                continue
            if mode == "honoured":
                if not strategy or strategy.get("strategy") != "hybrid" or strategy.get("scroll_region_verified") is not True:
                    failures.append(f"{mode}: expected hybrid with scroll_region_verified=true, got {strategy}")
                if regions.count((1, REGION_BOTTOM)) < 2:
                    failures.append(f"{mode}: expected the writer's DECSTBM after the probe's, got {regions}")
                if fallback:
                    failures.append(f"{mode}: unexpected fallback {fallback}")
            else:
                reason = "cpr_mismatch" if mode == "ignored" else "cpr_timeout"
                if not fallback or fallback.get("reason") != reason or fallback.get("to") != "overlay_redraw":
                    failures.append(f"{mode}: expected a {reason} fallback, got {fallback}")
                if not strategy or strategy.get("strategy") != "overlay_redraw" or strategy.get("scroll_region_verified") is not False:
                    failures.append(f"{mode}: expected overlay_redraw with scroll_region_verified=false, got {strategy}")
                if regions.count((1, REGION_BOTTOM)) != 1:
                    failures.append(f"{mode}: only the probe's DECSTBM may appear, got {regions}")
    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    print("inline scroll-region self-test E2E:", "FAIL" if failures else "PASS")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Terminal-identity compatibility matrix for FrankenTUI's output policy.

Runs a FrankenTUI binary (the demo showcase by default) under a real pty
once per terminal identity, optionally answering capability probes the way
that terminal would, sends `q` after a short settle time, and tallies the
escape sequences the program emitted. Each row asserts the policy outcome
that identity must produce: whether DEC 2026 synchronized-output brackets
were used, whether a DECSTBM scroll region was set in inline mode, and that
the terminal was restored on exit. One JSONL line per run is written so a CI
failure is diagnosable from the artifact alone.

Why this exists: the 2026-09-01 reality check found sync output silently
disabled on WezTerm, iTerm2, VS Code, Apple Terminal, `TERM=alacritty` and
plain `xterm-256color` because policy came from an identity allowlist only.
The DECRPM 2026 probe recovers it on terminals that answer; this matrix pins
both the allowlist and the probe path so they cannot regress unnoticed.

Usage:
    scripts/pty_identity_matrix.py [--bin PATH] [--out FILE.jsonl] [--only NAME]
Exit status is non-zero if any row's expectation fails.
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import pty
import re
import select
import signal
import struct
import sys
import termios
import time

DECRPM_2026_QUERY = b"\x1b[?2026$p"
XTGETTCAP_RGB_QUERY = b"\x1bP+q524742\x1b\\"

STRIP = ("TMUX", "TMUX_PANE", "STY", "ZELLIJ", "WEZTERM_PANE", "WEZTERM_UNIX_SOCKET",
         "WEZTERM_EXECUTABLE", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "KITTY_WINDOW_ID",
         "LC_TERMINAL", "NO_COLOR", "FTUI_SYNC_OUTPUT", "FTUI_SCROLL_REGION", "FTUI_CAPS_PROBE")

# name, env, decrpm_answer (status code the fake terminal reports for 2026, or
# None for "does not answer"), expectations.
ROWS = [
    # Allowlisted identities: sync output without any probe.
    dict(name="kitty", env={"TERM": "xterm-kitty", "KITTY_WINDOW_ID": "1"},
         decrpm=None, sync=True, decstbm=True),
    dict(name="ghostty", env={"TERM": "xterm-ghostty", "TERM_PROGRAM": "ghostty"},
         decrpm=None, sync=True, decstbm=True),
    dict(name="alacritty_term_program", env={"TERM": "xterm-256color", "TERM_PROGRAM": "Alacritty"},
         decrpm=None, sync=True, decstbm=True),
    dict(name="alacritty_term_only", env={"TERM": "alacritty"},
         decrpm=None, sync=True, decstbm=True),
    # Probe-recovered identities: sync only because the terminal answers DECRPM.
    dict(name="xterm256_answers_decrpm", env={"TERM": "xterm-256color", "COLORTERM": "truecolor"},
         decrpm=2, sync=True, decstbm=True),
    dict(name="iterm2_answers_decrpm", env={"TERM": "xterm-256color", "TERM_PROGRAM": "iTerm.app",
                                             "LC_TERMINAL": "iTerm2"},
         decrpm=2, sync=True, decstbm=True),
    dict(name="vscode_answers_decrpm", env={"TERM": "xterm-256color", "TERM_PROGRAM": "vscode"},
         decrpm=1, sync=True, decstbm=True),
    # Terminals that reject or ignore the probe stay conservative.
    dict(name="xterm256_rejects_decrpm", env={"TERM": "xterm-256color"},
         decrpm=0, sync=False, decstbm=True),
    dict(name="apple_terminal_silent", env={"TERM": "xterm-256color", "TERM_PROGRAM": "Apple_Terminal"},
         decrpm=None, sync=False, decstbm=True),
    # Multiplexers: never sync, never scroll region (policy: mux wins).
    dict(name="tmux", env={"TERM": "tmux-256color", "TMUX": "/tmp/tmux-1/default,1,0"},
         decrpm=2, sync=False, decstbm=False),
    # WezTerm identity is treated as multiplexer evidence by policy (owner
    # decision recorded in terminal_capabilities.rs); the operator switch is
    # the documented opt-in.
    dict(name="wezterm_default_policy", env={"TERM": "xterm-256color", "TERM_PROGRAM": "WezTerm"},
         decrpm=2, sync=False, decstbm=False),
    dict(name="wezterm_operator_opt_in", env={"TERM": "xterm-256color", "TERM_PROGRAM": "WezTerm",
                                              "FTUI_SYNC_OUTPUT": "1", "FTUI_SCROLL_REGION": "1"},
         decrpm=None, sync=True, decstbm=True),
    dict(name="operator_opt_out", env={"TERM": "xterm-kitty", "KITTY_WINDOW_ID": "1",
                                       "FTUI_SYNC_OUTPUT": "0"},
         decrpm=None, sync=False, decstbm=True),
    # An explicit opt-out must also beat a terminal that answers the probe.
    dict(name="operator_opt_out_beats_probe", env={"TERM": "xterm-256color", "FTUI_SYNC_OUTPUT": "0"},
         decrpm=2, sync=False, decstbm=True),
    # Probing can be switched off entirely; the terminal never sees a query.
    dict(name="probe_disabled", env={"TERM": "xterm-256color", "FTUI_CAPS_PROBE": "0"},
         decrpm=2, sync=False, decstbm=True, expect_queries=0),
]


def run_row(binary: str, row: dict, mode: str, settle: float, budget: float) -> dict:
    env = {k: v for k, v in os.environ.items() if k not in STRIP}
    env.update({"TERM": "xterm-256color", "COLORTERM": "truecolor", "FTUI_DEMO_SCREEN_MODE": mode})
    env.update(row["env"])
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(binary, [binary], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    buf = b""
    pending = b""
    t0 = time.time()
    sent_q = False
    answered_decrpm = 0
    answered_rgb = 0
    status = None
    eof = False
    while time.time() - t0 < budget:
        r, _, _ = select.select([fd], [], [], 0.02)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                data = b""
            if not data:
                eof = True
                break
            buf += data
            pending += data
            # Act like the terminal: answer probes the program sends.
            while DECRPM_2026_QUERY in pending:
                pending = pending.replace(DECRPM_2026_QUERY, b"", 1)
                if row["decrpm"] is not None:
                    os.write(fd, f"\x1b[?2026;{row['decrpm']}$y".encode())
                    answered_decrpm += 1
            while XTGETTCAP_RGB_QUERY in pending:
                pending = pending.replace(XTGETTCAP_RGB_QUERY, b"", 1)
                os.write(fd, b"\x1bP1+r524742\x1b\\")
                answered_rgb += 1
            if len(pending) > 4096:
                pending = pending[-256:]
        if not sent_q and time.time() - t0 > settle:
            os.write(fd, b"q")
            sent_q = True
        wp, st = os.waitpid(pid, os.WNOHANG)
        if wp == pid:
            status = st
            break
    if status is None:
        if eof:
            _, status = os.waitpid(pid, 0)
        else:
            wp, st = os.waitpid(pid, os.WNOHANG)
            if wp == pid:
                status = st
            else:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
    exit_code = None
    if status is not None:
        exit_code = os.WEXITSTATUS(status) if os.WIFEXITED(status) else -os.WTERMSIG(status)

    def count(pattern: bytes) -> int:
        return len(re.findall(pattern, buf))

    tail = buf[-800:]
    observed = {
        "exit_code": exit_code,
        "bytes": len(buf),
        "decrpm_queries": count(re.escape(DECRPM_2026_QUERY)),
        "decrpm_answered": answered_decrpm,
        "rgb_answered": answered_rgb,
        "sync_begin": count(rb"\x1b\[\?2026h"),
        "sync_end": count(rb"\x1b\[\?2026l"),
        "decstbm_set": count(rb"\x1b\[\d+;\d+r"),
        "decstbm_reset": count(rb"\x1b\[r"),
        "alt_enter": count(rb"\x1b\[\?1049h"),
        "alt_leave": count(rb"\x1b\[\?1049l"),
        "cursor_hide": count(rb"\x1b\[\?25l"),
        "cursor_show": count(rb"\x1b\[\?25h"),
        "paste_on": count(rb"\x1b\[\?2004h"),
        "paste_off": count(rb"\x1b\[\?2004l"),
        "tail_cursor_show": b"\x1b[?25h" in tail,
    }
    failures = []
    if exit_code != 0:
        failures.append(f"expected clean exit on q, got {exit_code}")
    if (observed["sync_begin"] > 0) != row["sync"]:
        failures.append(f"sync output expected={row['sync']} observed pairs={observed['sync_begin']}")
    if observed["sync_begin"] != observed["sync_end"]:
        failures.append("unbalanced sync brackets")
    if mode == "inline" and (observed["decstbm_set"] > 0) != row["decstbm"]:
        failures.append(f"scroll region expected={row['decstbm']} observed sets={observed['decstbm_set']}")
    if mode == "inline" and observed["alt_enter"]:
        failures.append("inline mode entered the alternate screen")
    if not observed["tail_cursor_show"]:
        failures.append("cursor not restored at exit")
    if observed["paste_on"] != observed["paste_off"]:
        failures.append("bracketed paste not restored")
    if "expect_queries" in row and observed["decrpm_queries"] != row["expect_queries"]:
        failures.append(f"expected {row['expect_queries']} DECRPM queries, saw {observed['decrpm_queries']}")
    return {"row": row["name"], "mode": mode, "env": row["env"], "decrpm_answer": row["decrpm"],
            "expected": {"sync": row["sync"], "decstbm": row["decstbm"]},
            "observed": observed, "failures": failures}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", default=os.environ.get("FTUI_MATRIX_BIN",
                        os.path.join(os.environ.get("CARGO_TARGET_DIR", "target"), "debug", "ftui-demo-showcase")))
    parser.add_argument("--out", default=os.environ.get("FTUI_MATRIX_OUT", "target/pty_identity_matrix.jsonl"))
    parser.add_argument("--mode", default="inline", choices=["inline", "alt"])
    parser.add_argument("--only", default=None, help="run a single row by name")
    parser.add_argument("--settle", type=float, default=2.0, help="seconds before sending q")
    parser.add_argument("--budget", type=float, default=10.0, help="seconds before the run is killed")
    args = parser.parse_args()
    if not os.access(args.bin, os.X_OK):
        print(f"binary not executable: {args.bin}", file=sys.stderr)
        return 2
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    run_id = f"identity-matrix-{time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())}-{os.getpid()}"
    failed = 0
    with open(args.out, "w", encoding="utf-8") as out:
        for row in ROWS:
            if args.only and row["name"] != args.only:
                continue
            result = run_row(args.bin, row, args.mode, args.settle, args.budget)
            result["run_id"] = run_id
            result["binary"] = args.bin
            out.write(json.dumps(result) + "\n")
            out.flush()
            verdict = "PASS" if not result["failures"] else "FAIL"
            obs = result["observed"]
            print(f"{verdict:4} {row['name']:28} sync={obs['sync_begin']}/{obs['sync_end']:<3} "
                  f"decstbm={obs['decstbm_set']} decrpm_q={obs['decrpm_queries']} exit={obs['exit_code']}"
                  + (f"  {'; '.join(result['failures'])}" if result["failures"] else ""))
            failed += bool(result["failures"])
    print(f"{'PASS' if not failed else 'FAIL'}: {failed} failing row(s); log at {args.out}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""G48 real-multiplexer E2E: every inline log line must reach history.

Runs `agent_shell --exit-when-child-exits -- <child>` inside real terminal
multiplexers and reads their history back, next to a control run of the same
child without FrankenTUI. Each case asserts that every numbered line 1..N is
present exactly once and in order, that the agent_shell UI appears exactly
once (its final frame; no UI rows leaked into history while running), and
that the shell's next output starts on its own line (teardown contract).

Multiplexers: tmux (private `-L` socket, `-f /dev/null`), GNU screen (private
session name), zellij (private `ZELLIJ_SOCKET_DIR`). Under tmux it also runs
agent_shell with the multiplexer identity removed, once taking the DECSTBM
scroll-region path and once with overlay forced (`FTUI_SCROLL_REGION=0`), so
both inline transports are measured in one real emulator.

Each multiplexer also runs a 1000-line mixed workload (wide graphemes, SGR
color, and a clear-screen/cursor-home/OSC-title injection) in the default
sanitized mode and with `--log-mode=sgr-only`: every line must reach history
once, in order, as exactly its visible text on one row; under tmux an
escape-keeping capture shows the color kept in sgr-only mode and gone when
sanitized.

Every multiplexer also runs the child under `agent_shell --pty`, gated on
`[ -t 0 ] && [ -t 1 ] && [ -t 2 ]`: its lines exist only if the child saw a
terminal on all three. Under tmux, two cases quit agent_shell through its
session deadline while the child's background `sleep 60` runs, then probe that
PID before the pane closes (the pane's SIGHUP would blur the contrast): with
`--pty` it must be gone, over pipes it survives (the control, killed after).

A multiplexer that is not installed FAILS its cases as
`missing_prerequisite` unless it is listed in `FTUI_E2E_MUX_OUT_OF_SCOPE`
(comma separated; logged). Nothing outside the output directory is written,
no session the script did not create is touched, and outputs are retained.

Under tmux, two flagship journeys (bd-g00-root-epic-ewths.42.7) drive the
agent shell like a user: a `--pty` child streams 100,000 hostile mixed lines
around a stdin prompt; the driver types a line, resizes the window 50 times
20 ms apart, sends Ctrl-C, restarts with F5 and quits. Every line must reach
history once, in order, as its visible text; the typed line, interrupt,
restart and exits must be logged once; injections stay inert; the pane's
modes are restored; the child's background process is gone; bytes per log
line stay bounded. The first storm changes only the height and also requires
no UI row in history. The second changes the width too, where tmux reflows
full-width UI rows and part of a frame can overflow into history: those
fragment rows are counted and reported, not failed.

With `--showcase`, each multiplexer also starts `ftui-demo-showcase` in inline
mode under 500 existing history lines: they must all remain, once and in
order, and the showcase's chrome must appear once (its final frame). Without
the binary that case fails as missing_prerequisite unless `showcase` is listed
in FTUI_E2E_MUX_OUT_OF_SCOPE.

Usage:
  tests/e2e/scripts/test_inline_scrollback_mux.py \
      --agent-shell target/debug/examples/agent_shell \
      --showcase target/debug/ftui-demo-showcase [--lines 10000] [--out DIR]

Exit status: 0 when every in-scope case passes, 1 otherwise, 2 on usage error.
JSONL: one row per case plus a summary row, in
<out>/inline_scrollback_mux.<run-id>.jsonl, next to each case's raw capture.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

COLS, ROWS = 100, 30
# agent_shell's status row, present in every frame: its right edge carries a
# key hint (`Ctrl-C Interrupt` while the child runs, `F5 Restart` after) and,
# when the pane is wide enough, the counters `L:<lines> B:<bytes> E:<stderr>`
# (no leading word boundary: zellij's dump-screen drops the padding).
STATUS_HINT = re.compile(r"(Ctrl-C Interrupt|F5 Restart)\s*$")
STATUS_COUNTS = re.compile(r"L:(\d+) B:\d+ E:\d+")
DONE_MARKER = "__FTUI_E2E_DONE__"
NUMBER_LINE = re.compile(r"^\s*(\d+)\s*$")
# Per-run file names: outputs are retained, never deleted, and a later run in
# the same directory must not mistake an earlier run's marker for its own.
RUN_ID = f"{os.getpid()}-{int(time.time())}"


def run(cmd: list[str], env: dict[str, str] | None = None, check: bool = True) -> str:
    proc = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=120)
    if check and proc.returncode != 0:
        raise RuntimeError(f"{shlex.join(cmd)} failed ({proc.returncode}): {proc.stderr.strip()}")
    return proc.stdout


def analyse(transcript: str, expected: int, is_agent: bool) -> dict:
    """Check numbered lines 1..expected: presence, uniqueness, order."""
    rows = transcript.splitlines()
    numbers = [int(m.group(1)) for r in rows if (m := NUMBER_LINE.match(r))]
    wanted = [n for n in numbers if 1 <= n <= expected]
    found = len(set(wanted))
    duplicates = len(wanted) - found
    in_order = all(a < b for a, b in zip(wanted, wanted[1:]))
    first_missing = next((n for n in range(1, expected + 1) if n not in set(wanted)), None)
    status_rows = [r for r in rows if STATUS_HINT.search(r)]
    status_counts = [int(m.group(1)) for r in status_rows if (m := STATUS_COUNTS.search(r))]
    # One status row: the final frame. More means UI rows leaked into history.
    ui_rows = len(status_rows)
    done_rows = [r for r in rows if DONE_MARKER in r]
    # Teardown contract: the shell's marker sits on a row of its own.
    done_clean = len(done_rows) == 1 and done_rows[0].strip().startswith(DONE_MARKER)
    ok = found == expected and duplicates == 0 and in_order and done_clean
    # Final frame: it must show the final line count (a narrow pane may
    # truncate the field; that is reported as None, not passed).
    final_status = None if not status_counts else status_counts[-1] == expected
    if is_agent:
        ok = ok and ui_rows == 1 and final_status is not False
    return {
        "expected": expected,
        "found": found,
        "duplicates": duplicates,
        "in_order": in_order,
        "first_missing": first_missing,
        "ui_rows": ui_rows,
        "done_marker_on_own_row": done_clean,
        "final_status_current": final_status,
        "ok": ok,
    }


# Mixed workload: line i carries the payload for i % 4. The raw child output
# holds wide graphemes, SGR color, and an injection (clear screen, cursor
# home, OSC title) between "inject" and "end"; what history must show is the
# visible text below on one row. If the injection executed, "end" would be
# drawn at the home position instead.
MIXED_VISIBLE = {0: "plain", 1: "wide 漢字🦀", 2: "sgr", 3: "inject end"}
MIXED_LINE = re.compile(r"^\s*(\d+) (.*?)\s*$")
SGR_RED = "\x1b[31m"
WIDE_PRINTF = "printf '%d wide \\346\\274\\242\\345\\255\\227\\360\\237\\246\\200\\n'"


def mixed_child(lines: int) -> str:
    """POSIX sh printing the mixed workload (octal escapes: UTF-8 and ESC)."""
    return (f"i=1; while [ $i -le {lines} ]; do case $((i % 4)) in "
            "0) printf '%d plain\\n' $i;; "
            f"1) {WIDE_PRINTF} $i;; "
            "2) printf '%d \\033[31msgr\\033[0m\\n' $i;; "
            "3) printf '%d inject \\033[2J\\033[H\\033]0;pwn\\007end\\n' $i;; "
            "esac; i=$((i+1)); done")


def control_wide_text(transcript: str) -> str | None:
    """How this multiplexer's history renders the wide line from a plain shell.

    GNU screen 4.09's `hardcopy -h` keeps only the low byte of each cell
    (漢 U+6F22 becomes `"`), whatever its encoding settings, so the wide line
    is checked against this control rather than against its UTF-8 text.
    """
    return next((m.group(2) for r in transcript.splitlines()
                 if (m := MIXED_LINE.match(r)) and m.group(1) == "1"), None)


def analyse_mixed(transcript: str, expected: int, escaped: str | None, sgr: bool,
                  wide: str | None) -> dict:
    """Every mixed line once, in order, with exactly its visible payload.

    `wide` is the control's rendering of the wide line. `escaped` is a capture
    that keeps SGR (tmux only); when present, the SGR lines must keep their
    color in sgr-only mode and lose it when sanitized.
    """
    visible = dict(MIXED_VISIBLE)
    if wide is not None:
        visible[1] = wide
    rows = transcript.splitlines()
    seen = [(int(m.group(1)), m.group(2)) for r in rows if (m := MIXED_LINE.match(r))
            and 1 <= int(m.group(1)) <= expected]
    good = [n for n, text in seen if text == visible[n % 4]]
    wrong = sorted({n for n, text in seen if text != visible[n % 4]})
    found = len(set(good))
    in_order = all(a < b for a, b in zip(good, good[1:]))
    base = analyse(transcript, 0, True)
    status_counts = [int(m.group(1)) for r in rows
                     if STATUS_HINT.search(r) and (m := STATUS_COUNTS.search(r))]
    final_status = None if not status_counts else status_counts[-1] == expected
    colored = None
    if escaped is not None:
        colored = sum(1 for r in escaped.splitlines()
                      if (m := MIXED_LINE.match(re.sub(r"\x1b\[[0-9;]*m", "", r)))
                      and int(m.group(1)) % 4 == 2 and SGR_RED in r)
    want_colored = expected // 4 + (1 if expected % 4 >= 2 else 0)
    color_ok = colored is None or colored == (want_colored if sgr else 0)
    ok = (wide is not None and found == expected and len(good) == expected and in_order
          and not wrong and base["ui_rows"] == 1 and base["done_marker_on_own_row"]
          and final_status is not False and color_ok)
    return {
        "wide_visible": visible[1],
        "expected": expected,
        "found": found,
        "duplicates": len(good) - found,
        "in_order": in_order,
        "wrong_payload": wrong[:5],
        "ui_rows": base["ui_rows"],
        "done_marker_on_own_row": base["done_marker_on_own_row"],
        "final_status_current": final_status,
        "sgr_rows_colored": colored,
        "ok": ok,
    }


# The showcase's first screen; its chrome row appears in each painted frame.
SHOWCASE_CHROME = "FRANKENTUI DASHBOARD"
PRESERVED_LINES = 500


def analyse_preserve(transcript: str) -> dict:
    """Inline mode keeps the history it started under and leaks no frames.

    The shell prints 1..PRESERVED_LINES before the showcase starts; all of
    them must remain, once and in order, and the showcase's chrome must
    appear exactly once: its final frame, not earlier frames in history.
    """
    base = analyse(transcript, PRESERVED_LINES, False)
    chrome_rows = sum(1 for r in transcript.splitlines() if SHOWCASE_CHROME in r)
    base["chrome_rows"] = chrome_rows
    base["ok"] = base["ok"] and chrome_rows == 1
    return base


ORPHAN = re.compile(r"^ORPHAN=(gone|alive)\s*$")

# Flagship journey (bd-g00-root-epic-ewths.42.7): line i carries kind i % 6.
# Kinds 1 and 2 are checked against a plain-shell control of the same bytes,
# as for the mixed workload; the injection holds clear-screen, cursor home, an
# OSC 0 title, an OSC 52 clipboard write and DECSTBM between "inject" and "end".
JOURNEY_VISIBLE = {0: "plain", 1: "wide 漢字🦀", 2: "zwj 👩‍💻", 3: "sgr", 4: "inject end", 5: "osc8"}
JOURNEY_PRINTF = {
    0: "printf '%d plain\\n'",
    1: WIDE_PRINTF,
    2: "printf '%d zwj \\360\\237\\221\\251\\342\\200\\215\\360\\237\\222\\273\\n'",
    3: "printf '%d \\033[31msgr\\033[0m\\n'",
    4: "printf '%d inject \\033[2J\\033[H\\033]0;pwn\\007\\033]52;c;cHdu\\007\\033[5;10rend\\n'",
    5: "printf '%d \\033]8;;http://example.invalid/\\033\\\\osc8\\033]8;;\\033\\\\\\n'",
}
JOURNEY_LINES = 100_000
JOURNEY_SIZE = (100, 30)
JOURNEY_STORM = [(120, 40), (80, 24), (100, 30), (90, 35), (110, 26)]
# O(1) bytes per log line, not a repaint of the screen per line: a 100x30
# repaint is ~3000 bytes, so a few hundred is the bound that separates them.
JOURNEY_MAX_BYTES_PER_LINE = 400


def journey_child(marker: Path, pid_file: Path) -> str:
    """The journey's child: 100k mixed lines around a prompt read from stdin,
    then it waits for SIGINT. A restart (marker present) prints one line."""
    cases = " ".join(f"{k}) {JOURNEY_PRINTF[k]} $1;;" for k in sorted(JOURNEY_PRINTF))
    half = JOURNEY_LINES // 2
    m, p = shlex.quote(str(marker)), shlex.quote(str(pid_file))
    return (f"if [ -e {m} ]; then echo second-run; exit 0; fi; : > {m}; "
            f"sleep 60 & echo $! > {p}; "
            f"line() {{ case $(($1 % 6)) in {cases} esac; }}; "
            f"i=1; while [ $i -le {half} ]; do line $i; i=$((i+1)); done; "
            "printf 'type now: '; IFS= read -r l; echo \"got:$l\"; "
            f"while [ $i -le {JOURNEY_LINES} ]; do line $i; i=$((i+1)); done; "
            "trap 'echo interrupted; exit 130' INT; echo waiting; "
            "while :; do sleep 0.1; done")


# Height-only storm: what every check, including no UI row in history, holds.
JOURNEY_HEIGHT_STORM = [(100, 40), (100, 24), (100, 30), (100, 35), (100, 26)]
# With width changes tmux reflows rows wider than the narrower pane, the UI's
# full-width rows among them, so part of a frame can overflow into history.
# Every log line still arrives once and in order; the fragments are reported.
REFLOW_LIMITATION = ("tmux reflows full-width UI rows when the pane narrows; the overflow "
                     "rows of a frame can reach history (bd-g00-root-epic-ewths.42.8)")


def run_tmux_journey(out: Path, agent: Path, timeout_s: float, storm_steps: int = 50,
                     storm_interval_s: float = 0.02,
                     storm_sizes: list[tuple[int, int]] | None = None,
                     reflow_expected: bool = False, case: str = "flagship_journey") -> dict:
    """Drive the agent shell like a user inside real tmux and check history.

    Start inline (5 rows) with a --pty child streaming 100k hostile mixed
    lines; type a line at its prompt; resize the window 50 times while it
    streams; Ctrl-C; F5 to restart; quit. Then check every line once, in
    order, as exactly its visible text; the typed line, interrupt, restart
    and exits logged once; one UI frame in history; no injection took effect
    (pane title, clipboard buffers, the injected rows themselves); the pane's
    terminal modes restored; the child's background process gone; bytes per
    log line bounded.
    """
    tmux = Tmux(out)
    tmux.start()
    base = tmux.base
    marker = out / f"{case}.{RUN_ID}.restarted"
    pid_file = out / f"{case}.{RUN_ID}.pid"
    raw = out / f"{case}.{RUN_ID}.raw"
    try:
        run(base + ["set-option", "-g", "set-clipboard", "on"])
        # How this tmux renders the two Unicode kinds from a plain shell.
        controls = tmux.capture(f"{case}_control", " ; ".join(
            f"{JOURNEY_PRINTF[k]} {k}" for k in (1, 2)) + f"; echo {DONE_MARKER}; sleep 1", 60)
        visible = dict(JOURNEY_VISIBLE)
        for row in controls.splitlines():
            if (m := MIXED_LINE.match(row)) and m.group(1) in ("1", "2"):
                visible[int(m.group(1))] = m.group(2)
        probe = (f"pid=$(cat {shlex.quote(str(pid_file))}); for i in 1 2 3 4 5 6 7 8 9 10; do "
                 "kill -0 \"$pid\" 2>/dev/null || break; sleep 0.2; done; "
                 "if kill -0 \"$pid\" 2>/dev/null; then echo ORPHAN=alive; kill -9 \"$pid\"; "
                 "else echo ORPHAN=gone; fi")
        child = shlex.quote(journey_child(marker, pid_file))
        cmd = (f"{shlex.quote(str(agent))} --pty --ui-height=5 -- sh -c {child}; rc=$?; "
               f"{probe}; echo {DONE_MARKER} rc=$rc; sleep 1")
        cols, rows = JOURNEY_SIZE
        run(base + ["new-session", "-d", "-s", case, "-x", str(cols), "-y", str(rows),
                    "bash", "-c", cmd])
        run(base + ["pipe-pane", "-t", case, "-o", f"cat >> {shlex.quote(str(raw))}"])

        def screen() -> str:
            return run(base + ["capture-pane", "-p", "-t", case], check=False)

        def await_screen(text: str, limit: float) -> None:
            if not wait_for(lambda: text in screen(), limit):
                raise TimeoutError(f"journey: {text!r} never appeared on screen")

        await_screen("type now", timeout_s)
        run(base + ["send-keys", "-t", case, "hello", "Enter"])
        sizes = storm_sizes or JOURNEY_STORM
        for step in range(storm_steps):
            width, height = sizes[step % len(sizes)]
            run(base + ["resize-window", "-t", case, "-x", str(width), "-y", str(height)])
            time.sleep(storm_interval_s)
        run(base + ["resize-window", "-t", case, "-x", str(cols), "-y", str(rows)])
        await_screen("waiting", timeout_s)
        run(base + ["send-keys", "-t", case, "C-c"])
        await_screen("EXIT 130", 60)
        # F5 is refused until the child's reaping is confirmed (polled every
        # 250 ms), so press it until the restarted run answers.
        restarted = False
        deadline = time.monotonic() + 30
        while not restarted and time.monotonic() < deadline:
            run(base + ["send-keys", "-t", case, "F5"])
            restarted = wait_for(lambda: "second-run" in screen(), 2)
        if not restarted:
            raise TimeoutError("journey: F5 never restarted the child")
        await_screen("EXIT 0", 60)
        run(base + ["send-keys", "-t", case, "q"])
        # Read the modes while the pane's shell is alive: tmux reports any
        # dead pane's cursor as hidden.
        await_screen(DONE_MARKER, 60)
        flags = run(base + ["display-message", "-p", "-t", case,
                            "#{alternate_on} #{cursor_flag} #{mouse_any_flag} #{origin_flag} "
                            "#{insert_flag} #{scroll_region_upper} #{scroll_region_lower}"]).split()
        title = run(base + ["display-message", "-p", "-t", case, "#{pane_title}"]).strip()
        dead = lambda: run(base + ["display-message", "-p", "-t", case, "#{pane_dead}"],
                           check=False).strip() == "1"
        if not wait_for(dead, 60):
            raise TimeoutError("journey: the pane did not finish after quit")
        buffers = run(base + ["list-buffers"], check=False).strip()
        text = settled(lambda: run(
            base + ["capture-pane", "-p", "-J", "-t", case, "-S", "-", "-E", "-"]))
        (out / f"tmux.{case}.{RUN_ID}.txt").write_text(text)
    finally:
        tmux.stop()

    rows_all = text.splitlines()
    # The agent's final frame (its 5 UI rows, which include a viewer showing
    # the latest lines) sits right above the pane shell's probe output; the
    # rest is history, where every logged line must appear exactly once.
    probe_row = next((i for i, r in enumerate(rows_all) if ORPHAN.match(r)), len(rows_all))
    frame = rows_all[max(0, probe_row - 5):probe_row]
    history = rows_all[:max(0, probe_row - 5)] + rows_all[probe_row:]
    # Log lines arrive as a stream and carry no padding; UI rows are drawn
    # across the full width. A padded numbered row is a viewer row of a UI
    # frame that reached history (tmux reflow can split a full-width row when
    # a pane narrows, so part of a frame may overflow), not a log line.
    log_rows = [r for r in history if r == r.rstrip()]
    fragments = [r for r in history if STATUS_HINT.search(r)
                 or (r != r.rstrip() and MIXED_LINE.match(r))]
    seen = [(int(m.group(1)), m.group(2)) for r in log_rows if (m := MIXED_LINE.match(r))
            and 1 <= int(m.group(1)) <= JOURNEY_LINES]
    good = [n for n, payload in seen if payload == visible[n % 6]]
    wrong = sorted({n for n, payload in seen if payload != visible[n % 6]})
    once = lambda needle: sum(1 for r in history if needle in r)
    expected_flags = ["0", "1", "0", "0", "0", "0", str(rows - 1)]
    bytes_per_line = round(raw.stat().st_size / JOURNEY_LINES, 1) if raw.exists() else None
    checks = {
        "lines_in_order_once": good == list(range(1, JOURNEY_LINES + 1)),
        "typed_line_reached_child": once("type now: got:hello") == 1,
        "typed_line_echoed": once("[stdin queued] hello") == 1,
        "interrupt_delivered": once("interrupted") == 1 and once("[process] EXIT 130") == 1,
        "restart_logged": once("[process] RESTART run=") == 1 and once("second-run") == 1
        and once("[process] EXIT 0") == 1,
        "one_ui_frame_in_history": (reflow_expected or not fragments)
        and sum(1 for r in frame if STATUS_HINT.search(r)) == 1,
        "injection_inert": title != "pwn" and not buffers and not wrong,
        "terminal_restored": flags == expected_flags,
        "child_tree_reaped": once("ORPHAN=gone") == 1,
        "shell_marker_own_row": any(r.strip().startswith(DONE_MARKER) for r in history),
        "bytes_per_line_bounded": bytes_per_line is not None
        and bytes_per_line <= JOURNEY_MAX_BYTES_PER_LINE,
    }
    return {
        "storm": f"{storm_steps} resizes every {round(storm_interval_s * 1000)} ms",
        "lines": JOURNEY_LINES,
        "found": len(set(good)),
        "duplicates": len(good) - len(set(good)),
        "missing": [n for n in range(1, JOURNEY_LINES + 1) if n not in set(good)][:10],
        "ui_fragment_rows": len(fragments),
        "known_limitation": REFLOW_LIMITATION if reflow_expected else None,
        "wrong_payload": wrong[:5],
        "visible_controls": {k: visible[k] for k in (1, 2)},
        "pane_flags": " ".join(flags),
        "pane_title": title,
        "bytes_per_line": bytes_per_line,
        "keystroke_latency": "not measured here (dispatch budget is an in-process benchmark)",
        **checks,
        "ok": all(checks.values()),
    }


def analyse_quit(transcript: str, expect_orphan: bool) -> dict:
    """Check the pane shell's probe of the child's background process."""
    probes = [m.group(1) for r in transcript.splitlines() if (m := ORPHAN.match(r))]
    orphan = probes[0] if len(probes) == 1 else None
    return {
        "orphan": orphan,
        "expected_orphan": "alive" if expect_orphan else "gone",
        "ok": orphan == ("alive" if expect_orphan else "gone"),
    }


def wait_for(predicate, timeout_s: float) -> bool:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.5)
    return False


def stable_file(path: Path, timeout_s: float = 20.0) -> bool:
    """Wait until an asynchronously written file exists and stops growing."""
    deadline = time.monotonic() + timeout_s
    last = -1
    while time.monotonic() < deadline:
        size = path.stat().st_size if path.exists() else -1
        if size > 0 and size == last:
            return True
        last = size
        time.sleep(0.5)
    return False


def settled(read, timeout_s: float = 30.0) -> str:
    """Read a capture until it holds the shell's trailing marker.

    A multiplexer can report the child as finished while output it has not
    parsed yet is still queued in the pty, so a capture taken at that moment
    is short. The marker is the last thing the case writes.
    """
    deadline = time.monotonic() + timeout_s
    text = read()
    while DONE_MARKER not in text and time.monotonic() < deadline:
        time.sleep(0.5)
        text = read()
    return text


class Tmux:
    name = "tmux"

    def __init__(self, out: Path) -> None:
        self.sock = f"ftui_scrollback_{os.getpid()}"
        self.base = ["tmux", "-L", self.sock, "-f", "/dev/null"]
        self.out = out

    def version(self) -> str:
        return run(["tmux", "-V"]).strip()

    def start(self) -> None:
        run(self.base + ["new-session", "-d", "-s", "boot", "-x", str(COLS), "-y", str(ROWS)])
        run(self.base + ["set-option", "-g", "history-limit", "500000"])
        run(self.base + ["set-option", "-g", "remain-on-exit", "on"])

    def stop(self) -> None:
        run(self.base + ["kill-server"], check=False)

    def capture(self, case: str, shell_cmd: str, timeout_s: float) -> str:
        run(self.base + ["new-session", "-d", "-s", case, "-x", str(COLS), "-y", str(ROWS),
                         "bash", "-c", shell_cmd])
        dead = lambda: run(self.base + ["display-message", "-p", "-t", case, "#{pane_dead}"],
                           check=False).strip() == "1"
        if not wait_for(dead, timeout_s):
            raise TimeoutError(f"tmux case {case} did not finish in {timeout_s}s")
        text = settled(lambda: run(
            self.base + ["capture-pane", "-p", "-J", "-t", case, "-S", "-", "-E", "-"]))
        # The same history with SGR kept, for checks on what color survived.
        self.escaped = run(
            self.base + ["capture-pane", "-p", "-e", "-J", "-t", case, "-S", "-", "-E", "-"])
        run(self.base + ["kill-session", "-t", case], check=False)
        return text


class Screen:
    name = "screen"

    def __init__(self, out: Path) -> None:
        self.prefix = f"ftui_scrollback_{os.getpid()}"
        self.out = out
        self.sessions: list[str] = []

    def version(self) -> str:
        return run(["screen", "--version"], check=False).strip()

    def start(self) -> None:
        pass

    def stop(self) -> None:
        for s in self.sessions:
            run(["screen", "-S", s, "-X", "quit"], check=False)

    def capture(self, case: str, shell_cmd: str, timeout_s: float) -> str:
        session = f"{self.prefix}_{case}"
        self.sessions.append(session)
        marker = self.out / f"{case}.{RUN_ID}.screen.done"
        wrapped = f"{shell_cmd}; touch {shlex.quote(str(marker))}; sleep 600"
        env = dict(os.environ, LINES=str(ROWS), COLUMNS=str(COLS))
        run(["screen", "-S", session, "-h", "500000", "-d", "-m", "bash", "-c", wrapped], env=env)
        if not wait_for(marker.exists, timeout_s):
            raise TimeoutError(f"screen case {case} did not finish in {timeout_s}s")
        attempts = iter(range(1_000))

        def read() -> str:
            # A fresh file per attempt, read only once screen stops writing it.
            dump = self.out / f"{case}.{RUN_ID}.{next(attempts)}.screen.hardcopy"
            run(["screen", "-S", session, "-X", "hardcopy", "-h", str(dump)])
            return dump.read_text(errors="replace") if stable_file(dump) else ""

        text = settled(read)
        run(["screen", "-S", session, "-X", "quit"], check=False)
        return text


class Zellij:
    name = "zellij"

    def __init__(self, out: Path) -> None:
        # zellij derives the longest allowed session name from the socket
        # path (a Unix socket path is ~108 bytes), so the private socket
        # directory must be short: prefer the per-user runtime directory.
        runtime = os.environ.get("XDG_RUNTIME_DIR") or None
        self.sockdir = Path(tempfile.mkdtemp(prefix="fz", dir=runtime))
        self.env = dict(os.environ, ZELLIJ_SOCKET_DIR=str(self.sockdir))
        self.config = out / "zellij.config.kdl"
        self.config.write_text("show_startup_tips false\nshow_release_notes false\n")
        self.out = out
        self.sessions: list[str] = []

    def version(self) -> str:
        return run(["zellij", "--version"]).strip()

    def start(self) -> None:
        pass

    def stop(self) -> None:
        for s in self.sessions:
            run(["zellij", "kill-session", s], env=self.env, check=False)

    def capture(self, case: str, shell_cmd: str, timeout_s: float) -> str:
        session = f"ftui{os.getpid()}{case.replace('_', '')}"[:30]
        self.sessions.append(session)
        marker = self.out / f"{case}.{RUN_ID}.zellij.done"
        layout = self.out / f"{case}.{RUN_ID}.zellij.kdl"
        wrapped = f"{shell_cmd}; touch {shlex.quote(str(marker))}; sleep 600"
        layout.write_text(
            "layout {\n  pane command=\"bash\" {\n    args \"-c\" "
            + json.dumps(wrapped)
            + "\n  }\n}\n"
        )
        run(["zellij", "--config", str(self.config), "--layout", str(layout),
             "attach", "--create-background", session], env=self.env)
        if not wait_for(marker.exists, timeout_s):
            raise TimeoutError(f"zellij case {case} did not finish in {timeout_s}s")
        attempts = iter(range(1_000))

        # The layout's only pane is focused, which dump-screen captures; the
        # file is written asynchronously: a fresh file per attempt, read only
        # once zellij stops writing it.
        def read() -> str:
            dump = self.out / f"{case}.{RUN_ID}.{next(attempts)}.zellij.dump"
            run(["zellij", "--session", session, "action", "dump-screen", "--full", str(dump)],
                env=self.env)
            if not stable_file(dump):
                raise RuntimeError(f"zellij dump-screen produced no output for {case}")
            return dump.read_text(errors="replace")

        return settled(read)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--agent-shell", required=True, type=Path)
    ap.add_argument("--showcase", type=Path, default=None,
                    help="ftui-demo-showcase binary; its cases fail as missing_prerequisite "
                         "without it unless 'showcase' is in FTUI_E2E_MUX_OUT_OF_SCOPE")
    ap.add_argument("--lines", type=int, default=10_000)
    ap.add_argument("--out", type=Path, default=None)
    ap.add_argument("--timeout", type=float, default=600.0, help="seconds per case")
    ap.add_argument("--mux", default="tmux,screen,zellij")
    args = ap.parse_args()

    agent = args.agent_shell.resolve()
    if not os.access(agent, os.X_OK):
        print(f"agent_shell binary not executable: {agent}", file=sys.stderr)
        return 2
    out = (args.out or Path(tempfile.mkdtemp(prefix="ftui_inline_scrollback_"))).resolve()
    out.mkdir(parents=True, exist_ok=True)
    out_of_scope = {m.strip() for m in os.environ.get("FTUI_E2E_MUX_OUT_OF_SCOPE", "").split(",") if m.strip()}
    n = args.lines
    child = f"seq 1 {n}"
    agent_cmd = f"{shlex.quote(str(agent))} --exit-when-child-exits -- {child}"
    tty_child = shlex.quote(f"[ -t 0 ] && [ -t 1 ] && [ -t 2 ] && {child}")
    pty_cmd = f"{shlex.quote(str(agent))} --pty --exit-when-child-exits -- sh -c {tty_child}"
    mixed_lines = 1000
    mixed = shlex.quote(mixed_child(mixed_lines))
    mixed_cmd = f"{shlex.quote(str(agent))} --exit-when-child-exits -- sh -c {mixed}"
    mixed_sgr_cmd = (f"{shlex.quote(str(agent))} --log-mode=sgr-only --exit-when-child-exits "
                     f"-- sh -c {mixed}")
    # The trailing sleep lets the multiplexer drain the pty before the
    # pane's process exits (tmux drops unread output of a dead pane).
    done = f"echo {DONE_MARKER} rc=$?; sleep 1"

    def quit_cmd(case: str, pty: bool) -> str:
        """agent_shell quits at its deadline; the pane shell then probes the
        child's background PID, retrying while a killed one awaits reaping."""
        pid_file = shlex.quote(str(out / f"{case}.{RUN_ID}.pid"))
        quit_child = shlex.quote(
            f"sleep 60 & echo $! > {pid_file}; echo started; while :; do sleep 0.1; done")
        mode = "--pty " if pty else ""
        return (f"{shlex.quote(str(agent))} {mode}--exit-after-ms=2000 -- sh -c {quit_child}; "
                f"pid=$(cat {pid_file}); for i in 1 2 3 4 5 6 7 8 9 10; do "
                f"kill -0 \"$pid\" 2>/dev/null || break; sleep 0.2; done; "
                f"if kill -0 \"$pid\" 2>/dev/null; then echo ORPHAN=alive; kill -9 \"$pid\"; "
                f"else echo ORPHAN=gone; fi; {done}")

    # kind: "control" and "agent" are scrollback cases; ("mixed", sgr-only)
    # checks the mixed workload's visible text; ("quit", orphan expected)
    # probes process-tree cleanup when agent_shell quits.
    mixed_cases = [
        ("control_wide", f"{WIDE_PRINTF} 1; {done}", "control_wide"),
        ("agent_shell_mixed_sanitized", f"{mixed_cmd}; {done}", ("mixed", False)),
        ("agent_shell_mixed_sgr_only", f"{mixed_sgr_cmd}; {done}", ("mixed", True)),
    ]
    # The showcase in inline mode, started under existing history; it quits
    # on its own timer. Missing binary: a failed case, unless declared.
    if args.showcase is not None:
        showcase_cmd = (f"seq 1 {PRESERVED_LINES}; FTUI_DEMO_EXIT_AFTER_MS=3000 "
                        f"{shlex.quote(str(args.showcase.resolve()))} --screen-mode=inline "
                        f"--ui-height=12; {done}")
        mixed_cases.append(("showcase_inline_preserves_history", showcase_cmd, "preserve"))
    elif "showcase" not in out_of_scope:
        mixed_cases.append(("showcase_inline_preserves_history", None, "preserve"))
    cases = {
        "tmux": [
            ("control_seq", f"{child}; {done}", "control"),
            ("agent_shell", f"{agent_cmd}; {done}", "agent"),
            ("agent_shell_pty", f"{pty_cmd}; {done}", "agent"),
            *mixed_cases,
            # TERM=tmux* alone also counts as tmux identity (mux wins), so
            # both markers go to reach the DECSTBM path inside tmux;
            # FTUI_SCROLL_REGION=1 skips the 150 ms CPR self-test, which a
            # loaded host can time out (silently falling back to overlay).
            ("agent_shell_no_mux_identity_region",
             f"env -u TMUX -u TMUX_PANE TERM=xterm-256color FTUI_SCROLL_REGION=1 "
             f"{agent_cmd}; {done}", "agent"),
            ("agent_shell_no_mux_identity_overlay",
             f"env -u TMUX -u TMUX_PANE TERM=xterm-256color FTUI_SCROLL_REGION=0 "
             f"{agent_cmd}; {done}", "agent"),
            ("agent_shell_pty_quit", quit_cmd("agent_shell_pty_quit", True), ("quit", False)),
            ("agent_shell_pipe_quit_control", quit_cmd("agent_shell_pipe_quit_control", False),
             ("quit", True)),
        ],
        "screen": [
            ("control_seq", f"{child}; {done}", "control"),
            ("agent_shell", f"{agent_cmd}; {done}", "agent"),
            ("agent_shell_pty", f"{pty_cmd}; {done}", "agent"),
            *mixed_cases,
        ],
        "zellij": [
            ("control_seq", f"{child}; {done}", "control"),
            ("agent_shell", f"{agent_cmd}; {done}", "agent"),
            ("agent_shell_pty", f"{pty_cmd}; {done}", "agent"),
            *mixed_cases,
        ],
    }
    drivers = {"tmux": Tmux, "screen": Screen, "zellij": Zellij}

    results = []
    jsonl = out / f"inline_scrollback_mux.{RUN_ID}.jsonl"
    with jsonl.open("w") as log:
        for mux in [m.strip() for m in args.mux.split(",") if m.strip()]:
            if mux in out_of_scope:
                row = {"mux": mux, "status": "out_of_scope", "reason": "FTUI_E2E_MUX_OUT_OF_SCOPE"}
                print(json.dumps(row))
                log.write(json.dumps(row) + "\n")
                continue
            if shutil.which(mux) is None:
                row = {"mux": mux, "status": "fail", "reason": "missing_prerequisite"}
                results.append(row)
                print(json.dumps(row))
                log.write(json.dumps(row) + "\n")
                continue
            driver = drivers[mux](out)
            version = driver.version()
            wide = None
            try:
                driver.start()
                for case, cmd, kind in cases[mux]:
                    started = time.monotonic()
                    try:
                        if cmd is None:
                            raise RuntimeError("missing_prerequisite: no --showcase binary")
                        text = driver.capture(case, cmd, args.timeout)
                        (out / f"{mux}.{case}.{RUN_ID}.txt").write_text(text)
                        shape = kind[0] if isinstance(kind, tuple) else kind
                        if shape == "quit":
                            checks = analyse_quit(text, kind[1])
                        elif shape == "control_wide":
                            wide = control_wide_text(text)
                            checks = {"wide_visible": wide,
                                      "utf8_intact": wide == MIXED_VISIBLE[1],
                                      "ok": wide is not None}
                        elif shape == "mixed":
                            checks = analyse_mixed(text, mixed_lines,
                                                   getattr(driver, "escaped", None), kind[1], wide)
                        elif shape == "preserve":
                            checks = analyse_preserve(text)
                        else:
                            checks = analyse(text, n, kind == "agent")
                        row = {"mux": mux, "version": version, "case": case, **checks}
                        row["status"] = "pass" if row.pop("ok") else "fail"
                    except (RuntimeError, TimeoutError, subprocess.TimeoutExpired) as err:
                        row = {"mux": mux, "version": version, "case": case,
                               "status": "fail", "reason": str(err)}
                    row["seconds"] = round(time.monotonic() - started, 1)
                    results.append(row)
                    print(json.dumps(row), flush=True)
                    log.write(json.dumps(row) + "\n")
            finally:
                driver.stop()
        # The flagship journeys run on their own tmux servers after the cases:
        # a height-only resize storm (every check) and one that also changes
        # the width (every check but the reflow fragments, which are reported).
        muxes = [m.strip() for m in args.mux.split(",") if m.strip()]
        if "tmux" in muxes and "tmux" not in out_of_scope and shutil.which("tmux"):
            for case, sizes, reflow in [
                ("flagship_journey", JOURNEY_HEIGHT_STORM, False),
                ("flagship_journey_width_resizes", JOURNEY_STORM, True),
            ]:
                started = time.monotonic()
                row = {"mux": "tmux", "version": Tmux(out).version(), "case": case}
                try:
                    row.update(run_tmux_journey(out, agent, args.timeout, storm_sizes=sizes,
                                                reflow_expected=reflow, case=case))
                    row["status"] = "pass" if row.pop("ok") else "fail"
                except (RuntimeError, TimeoutError, subprocess.TimeoutExpired) as err:
                    row.update(status="fail", reason=str(err))
                row["seconds"] = round(time.monotonic() - started, 1)
                results.append(row)
                print(json.dumps(row, ensure_ascii=False), flush=True)
                log.write(json.dumps(row) + "\n")
        failed = [r for r in results if r["status"] != "pass"]
        summary = {"summary": True, "cases": len(results), "failed": len(failed),
                   "lines": n, "out": str(out)}
        print(json.dumps(summary))
        log.write(json.dumps(summary) + "\n")
    return 1 if failed or not results else 0


if __name__ == "__main__":
    sys.exit(main())

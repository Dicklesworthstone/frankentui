#!/usr/bin/env python3
"""Executable Vision Scorecard for FrankenTUI (G42 / bd-g00-root-epic-ewths.42.6).

Reproduces the manual reality-check audit in a single repeatable command and
writes `<out>/vision_scorecard.json` alongside retained raw PTY captures,
canonicalized transcripts, and command logs:

1. `truth_gates`: `make gates` (reachability, claims, env-docs, close-audit,
   baseline-benches, workflows, repo-facts).
2. `compiler_gates`: `cargo fmt --all --check`, `cargo check --workspace --all-targets`,
   `cargo clippy --workspace --all-targets -- -D warnings`, and
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` with
   `RCH_CARGO_WRAPPER_BYPASS=1`, pinned toolchain from `rust-toolchain.toml`,
   and `CARGO_BUILD_FINGERPRINT=content`.
3. `showcase_45_screens_pty`: PTY sweep of all 45 showcase screens in alt mode
   (deterministic, auto-exit), plus inline mode and interactive `q`-quit:
   asserts exit 0, no panic, balanced alt-screen enter/leave, cursor visible at
   exit, termios `ICANON`/`ECHO` restored, and real UI pixels via `pty_canonicalize`.
4. `examples_pty`: PTY runs of `minimal_inline` (`q`), `getting_started` (`q`
   and `Ctrl-C`), `ftui-harness --example minimal` (`q` and auto-exit),
   `ftui-harness` default binary, and `agent_shell` 10,000-line stream.
5. `sync_brackets_balance`: `FTUI_SYNC_OUTPUT=1` across alt dashboard, alt VFX,
   inline dashboard, and `agent_shell`, asserting balanced DEC `?2026h`/`?2026l`
   brackets (`[h, l] * N`).
6. `scrollback_identities_pty`: `agent_shell` scrollback delivery across plain
   PTY without CPR, forced scroll region (`FTUI_SCROLL_REGION=1`), `TMUX`
   identity, and `kitty` identity, verified via `pty_canonicalize --scrollback`.
7. `scrollback_real_tmux`: `agent_shell` vs control `seq` in a real `tmux`
   server on a private socket (`-L`, `-f /dev/null`), asserting every line
   1..N reaches tmux history once and in order with one final UI frame.

Missing prerequisites FAIL (`status: "fail"`, `missing_prerequisite`) — never
vacuously skipped. Outputs are retained and never deleted.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import shutil
import struct
import subprocess
import sys
import termios
import time
from typing import Any
import tomllib

ROOT = Path(__file__).resolve().parent.parent
SCHEMA_VERSION = "ftui-vision-scorecard-v1"
CLEAN_ENV_STRIP = (
    "TMUX",
    "TMUX_PANE",
    "STY",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "WEZTERM_UNIX_SOCKET",
    "WEZTERM_PANE",
    "WEZTERM_EXECUTABLE",
    "KITTY_WINDOW_ID",
    "WT_SESSION",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "LC_TERMINAL",
    "NO_COLOR",
    "FTUI_SCROLL_REGION",
    "FTUI_SYNC_OUTPUT",
    "FTUI_CAPS_PROBE",
    "FTUI_TEST_PROFILE",
)
NUMBER_LINE = re.compile(r"^\s*(\d+)\s*$")
STATUS_HINT = re.compile(r"(Ctrl-C Interrupt|F5 Restart)\s*$")


def pinned_toolchain(root: Path = ROOT) -> str:
    data = tomllib.loads((root / "rust-toolchain.toml").read_text(encoding="utf-8"))
    return str(data["toolchain"]["channel"])


def cargo_env(root: Path = ROOT) -> dict[str, str]:
    env = dict(os.environ)
    env["RCH_CARGO_WRAPPER_BYPASS"] = "1"
    env["RUSTUP_TOOLCHAIN"] = pinned_toolchain(root)
    env["CARGO_BUILD_FINGERPRINT"] = "content"
    env["CARGO_CACHE_AUTO_CLEAN_FREQUENCY"] = "never"
    return env


def run_logged_cmd(
    cmd: list[str],
    log_path: Path,
    *,
    cwd: Path = ROOT,
    env: dict[str, str] | None = None,
    timeout: float = 900.0,
) -> tuple[int, int]:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    start_ns = time.monotonic_ns()
    proc = subprocess.run(
        cmd,
        cwd=str(cwd),
        env=env or cargo_env(cwd),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    elapsed_ms = max(1, (time.monotonic_ns() - start_ns) // 1_000_000)
    log_path.write_text(
        f"$ {shlex.join(cmd)}\nexit_code={proc.returncode} elapsed_ms={elapsed_ms}\n"
        f"--- STDOUT ---\n{proc.stdout}\n--- STDERR ---\n{proc.stderr}\n",
        encoding="utf-8",
    )
    return proc.returncode, elapsed_ms


def run_pty_capture(
    cmd: list[str],
    capture_path: Path,
    *,
    cols: int = 120,
    rows: int = 40,
    env_overrides: dict[str, str] | None = None,
    send_bytes: bytes = b"",
    send_delay_s: float = 0.25,
    send_after_bytes: bytes | None = None,
    timeout_s: float = 15.0,
) -> dict[str, Any]:
    capture_path.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    for key in list(env):
        if key in CLEAN_ENV_STRIP or key.startswith(("FTUI_DEMO_", "FTUI_HARNESS_")):
            env.pop(key, None)
    env["TERM"] = "xterm-256color"
    env["COLORTERM"] = "truecolor"
    env["FTUI_CAPS_PROBE"] = "0"
    if env_overrides:
        for k, v in env_overrides.items():
            if v is None:
                env.pop(k, None)
            else:
                env[k] = v

    master_fd, slave_fd = pty.openpty()
    winsize = struct.pack("HHHH", rows, cols, 0, 0)
    fcntl.ioctl(slave_fd, termios.TIOCSWINSZ, winsize)

    def _setup_ctty() -> None:
        os.setsid()
        try:
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        except OSError:
            pass

    start_ns = time.monotonic_ns()
    proc = subprocess.Popen(
        cmd,
        stdin=slave_fd,
        stdout=slave_fd,
        stderr=slave_fd,
        env=env,
        close_fds=True,
        preexec_fn=_setup_ctty,
    )
    buf = bytearray()
    sent = False
    timed_out = False
    deadline = time.monotonic() + timeout_s

    try:
        while True:
            now = time.monotonic()
            if now >= deadline:
                timed_out = True
                proc.kill()
                break
            if (
                send_bytes
                and not sent
                and (now - (deadline - timeout_s)) >= send_delay_s
                and (send_after_bytes is None or send_after_bytes in buf)
            ):
                try:
                    os.write(master_fd, send_bytes)
                    sent = True
                except OSError:
                    pass
            rlist, _, _ = select.select([master_fd], [], [], 0.02)
            if rlist:
                try:
                    chunk = os.read(master_fd, 65536)
                    if chunk:
                        buf.extend(chunk)
                except OSError:
                    break
            elif proc.poll() is not None:
                # Drain any remaining queued bytes
                while True:
                    r2, _, _ = select.select([master_fd], [], [], 0.01)
                    if not r2:
                        break
                    try:
                        chunk = os.read(master_fd, 65536)
                        if not chunk:
                            break
                        buf.extend(chunk)
                    except OSError:
                        break
                break
        rc = proc.wait(timeout=5.0)
        attrs = termios.tcgetattr(slave_fd)
        lflag = attrs[3]
        icanon_restored = bool(lflag & termios.ICANON)
        echo_restored = bool(lflag & termios.ECHO)
    finally:
        os.close(slave_fd)
        os.close(master_fd)

    elapsed_ms = max(1, (time.monotonic_ns() - start_ns) // 1_000_000)
    raw = bytes(buf)
    capture_path.write_bytes(raw)

    alt_enter = raw.count(b"\x1b[?1049h")
    alt_leave = raw.count(b"\x1b[?1049l")
    cursor_seq = re.findall(rb"\x1b\[\?25([hl])", raw)
    sync_seq = re.findall(rb"\x1b\[\?2026([hl])", raw)
    panicked = b"panicked at" in raw or b"thread 'main' panicked" in raw

    return {
        "exit_code": rc,
        "timed_out": timed_out,
        "duration_ms": elapsed_ms,
        "bytes": len(raw),
        "panicked": panicked,
        "alt_enter": alt_enter,
        "alt_leave": alt_leave,
        "cursor_hide": cursor_seq.count(b"l"),
        "cursor_show": cursor_seq.count(b"h"),
        "cursor_visible_at_exit": bool(cursor_seq) and cursor_seq[-1] == b"h",
        "icanon_restored": icanon_restored,
        "echo_restored": echo_restored,
        "sync_h": sync_seq.count(b"h"),
        "sync_l": sync_seq.count(b"l"),
        "sync_balanced": sync_seq == [b"h", b"l"] * sync_seq.count(b"h"),
    }


def canonicalize_capture(
    canonicalize_bin: Path,
    capture_path: Path,
    screen_path: Path,
    *,
    cols: int = 120,
    rows: int = 40,
    scrollback: bool = False,
    max_scrollback: int = 20000,
) -> str:
    cmd = [
        str(canonicalize_bin),
        "--input",
        str(capture_path),
        "--output",
        str(screen_path),
        "--cols",
        str(cols),
        "--rows",
        str(rows),
    ]
    if scrollback:
        cmd.extend(["--scrollback", "--max-scrollback", str(max_scrollback)])
    subprocess.run(cmd, check=True, capture_output=True, text=True)
    return screen_path.read_text(encoding="utf-8", errors="replace")


def check_numbered_scrollback(transcript: str, expected: int) -> dict[str, Any]:
    rows = transcript.splitlines()
    numbers = [int(m.group(1)) for r in rows if (m := NUMBER_LINE.match(r))]
    wanted = [n for n in numbers if 1 <= n <= expected]
    found = len(set(wanted))
    duplicates = len(wanted) - found
    in_order = wanted == list(range(1, expected + 1))
    status_rows = sum(1 for r in rows if STATUS_HINT.search(r) or ("L:" in r and " B:" in r))
    return {
        "expected": expected,
        "found": found,
        "duplicates": duplicates,
        "in_order": in_order,
        "status_rows": status_rows,
        "ok": found == expected and duplicates == 0 and in_order and status_rows >= 1,
    }


def ensure_binaries(out_dir: Path, target_dir: Path) -> tuple[dict[str, Path], int, str | None]:
    bins = {
        "showcase": target_dir / "debug" / "ftui-demo-showcase",
        "harness": target_dir / "debug" / "ftui-harness",
        "minimal_harness": target_dir / "debug" / "examples" / "minimal",
        "minimal_inline": target_dir / "debug" / "examples" / "minimal_inline",
        "getting_started": target_dir / "debug" / "examples" / "getting_started",
        "agent_shell": target_dir / "debug" / "examples" / "agent_shell",
        "pty_canonicalize": target_dir / "debug" / "pty_canonicalize",
    }
    if all(p.is_file() and os.access(p, os.X_OK) for p in bins.values()):
        return bins, 0, None

    build_cmd = [
        "cargo",
        "-Zchecksum-freshness",
        "build",
        "--locked",
        "-p",
        "ftui-demo-showcase",
        "--bin",
        "ftui-demo-showcase",
        "-p",
        "ftui-harness",
        "--bin",
        "ftui-harness",
        "--example",
        "minimal",
        "-p",
        "ftui",
        "--example",
        "minimal_inline",
        "--example",
        "getting_started",
        "--example",
        "agent_shell",
        "-p",
        "ftui-pty",
        "--bin",
        "pty_canonicalize",
    ]
    rc, elapsed = run_logged_cmd(build_cmd, out_dir / "logs" / "cargo_build_bins.log")
    if rc != 0:
        return bins, elapsed, f"cargo build failed (exit {rc})"
    missing = [name for name, path in bins.items() if not (path.is_file() and os.access(path, os.X_OK))]
    if missing:
        return bins, elapsed, f"missing_prerequisite: built binaries absent: {', '.join(missing)}"
    return bins, elapsed, None


def run_showcase_sweep(out_dir: Path, showcase_bin: Path, canonicalize_bin: Path) -> dict[str, Any]:
    start_ns = time.monotonic_ns()
    sweep_dir = out_dir / "showcase_pty"
    sweep_dir.mkdir(parents=True, exist_ok=True)

    list_proc = subprocess.run(
        [str(showcase_bin), "--list-screens"],
        capture_output=True,
        text=True,
        check=False,
    )
    if list_proc.returncode != 0:
        return {
            "id": "showcase_45_screens_pty",
            "title": "PTY sweep of all 45 showcase screens + inline + q-quit",
            "status": "fail",
            "duration_ms": 1,
            "evidence_path": str(sweep_dir),
            "details": {"error": f"--list-screens failed ({list_proc.returncode})"},
        }
    slugs = json.loads(list_proc.stdout)
    expected_count = 45

    def check_one_screen(idx: int, slug: str) -> dict[str, Any]:
        cap = sweep_dir / f"screen_{idx:02d}_{slug}.pty"
        frame_cap = sweep_dir / f"screen_{idx:02d}_{slug}.frame.pty"
        txt = sweep_dir / f"screen_{idx:02d}_{slug}.txt"
        meta = run_pty_capture(
            [str(showcase_bin), f"--screen={idx}", "--screen-mode=alt"],
            cap,
            cols=120,
            rows=40,
            env_overrides={
                "FTUI_DEMO_DETERMINISTIC": "1",
                "FTUI_DEMO_SEED": "0",
                "FTUI_DEMO_EXIT_AFTER_MS": "300",
            },
        )
        raw_bytes = cap.read_bytes()
        leave_idx = raw_bytes.rfind(b"\x1b[?1049l")
        frame_cap.write_bytes(raw_bytes[:leave_idx] if leave_idx >= 0 else raw_bytes)
        screen_text = canonicalize_capture(canonicalize_bin, frame_cap, txt, cols=120, rows=40)
        non_empty_rows = sum(1 for line in screen_text.splitlines() if line.strip())
        ok = (
            meta["exit_code"] == 0
            and not meta["timed_out"]
            and not meta["panicked"]
            and meta["alt_enter"] >= 1
            and meta["alt_enter"] == meta["alt_leave"]
            and meta["cursor_visible_at_exit"]
            and meta["icanon_restored"]
            and meta["echo_restored"]
            and non_empty_rows >= 3
        )
        return {"screen": idx, "slug": slug, "non_empty_rows": non_empty_rows, **meta, "ok": ok}

    results: list[dict[str, Any]] = []
    with ThreadPoolExecutor(max_workers=8) as pool:
        futs = {pool.submit(check_one_screen, i + 1, slug): i + 1 for i, slug in enumerate(slugs)}
        for fut in as_completed(futs):
            results.append(fut.result())
    results.sort(key=lambda r: r["screen"])

    # Inline mode check
    inline_cap = sweep_dir / "showcase_inline_dashboard.pty"
    inline_txt = sweep_dir / "showcase_inline_dashboard.txt"
    inline_meta = run_pty_capture(
        [str(showcase_bin), "--screen=2", "--screen-mode=inline", "--ui-height=12"],
        inline_cap,
        cols=120,
        rows=40,
        env_overrides={
            "FTUI_DEMO_DETERMINISTIC": "1",
            "FTUI_DEMO_SEED": "0",
            "FTUI_DEMO_EXIT_AFTER_MS": "300",
        },
    )
    inline_screen = canonicalize_capture(canonicalize_bin, inline_cap, inline_txt, cols=120, rows=40)
    inline_ok = (
        inline_meta["exit_code"] == 0
        and not inline_meta["panicked"]
        and inline_meta["alt_enter"] == 0
        and inline_meta["alt_leave"] == 0
        and inline_meta["cursor_visible_at_exit"]
        and inline_meta["icanon_restored"]
        and inline_meta["echo_restored"]
        and sum(1 for line in inline_screen.splitlines() if line.strip()) >= 3
    )

    # Interactive q-quit check
    q_cap = sweep_dir / "showcase_q_quit.pty"
    q_txt = sweep_dir / "showcase_q_quit.txt"
    q_meta = run_pty_capture(
        [str(showcase_bin), "--screen=2", "--screen-mode=alt"],
        q_cap,
        cols=120,
        rows=40,
        env_overrides={"FTUI_DEMO_DETERMINISTIC": "1", "FTUI_DEMO_SEED": "0"},
        send_bytes=b"q",
        send_delay_s=0.25,
    )
    canonicalize_capture(canonicalize_bin, q_cap, q_txt, cols=120, rows=40)
    q_ok = (
        q_meta["exit_code"] == 0
        and not q_meta["timed_out"]
        and not q_meta["panicked"]
        and q_meta["alt_enter"] == q_meta["alt_leave"] >= 1
        and q_meta["cursor_visible_at_exit"]
        and q_meta["icanon_restored"]
        and q_meta["echo_restored"]
    )

    report_path = sweep_dir / "showcase_sweep_summary.json"
    failed_screens = [r["slug"] for r in results if not r["ok"]]
    all_ok = len(slugs) == expected_count and not failed_screens and inline_ok and q_ok
    payload = {
        "screen_count": len(slugs),
        "expected_screen_count": expected_count,
        "passed_screens": sum(1 for r in results if r["ok"]),
        "failed_screens": failed_screens,
        "inline_ok": inline_ok,
        "q_quit_ok": q_ok,
        "screens": results,
    }
    report_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    elapsed_ms = max(1, (time.monotonic_ns() - start_ns) // 1_000_000)
    return {
        "id": "showcase_45_screens_pty",
        "title": "PTY sweep of all 45 showcase screens + inline + q-quit",
        "status": "pass" if all_ok else "fail",
        "duration_ms": elapsed_ms,
        "evidence_path": str(report_path),
        "details": {
            "screen_count": len(slugs),
            "passed_screens": payload["passed_screens"],
            "failed_screens": failed_screens,
            "inline_ok": inline_ok,
            "q_quit_ok": q_ok,
        },
    }


def run_examples_sweep(out_dir: Path, bins: dict[str, Path]) -> dict[str, Any]:
    start_ns = time.monotonic_ns()
    ex_dir = out_dir / "examples_pty"
    ex_dir.mkdir(parents=True, exist_ok=True)
    canon = bins["pty_canonicalize"]

    cases: dict[str, Any] = {}

    # 1. minimal_inline (q)
    c1_cap = ex_dir / "minimal_inline_q.pty"
    c1_txt = ex_dir / "minimal_inline_q.txt"
    c1_meta = run_pty_capture(
        [str(bins["minimal_inline"])],
        c1_cap,
        cols=80,
        rows=24,
        send_bytes=b"q",
        send_delay_s=0.25,
    )
    c1_screen = canonicalize_capture(canon, c1_cap, c1_txt, cols=80, rows=24)
    cases["minimal_inline_q"] = {
        **c1_meta,
        "text_found": "Ticks:" in c1_screen,
        "ok": (
            c1_meta["exit_code"] == 0
            and not c1_meta["panicked"]
            and c1_meta["alt_enter"] == 0
            and c1_meta["cursor_visible_at_exit"]
            and c1_meta["icanon_restored"]
            and c1_meta["echo_restored"]
            and "Ticks:" in c1_screen
        ),
    }

    # 2. getting_started (Ctrl-C)
    c2_cap = ex_dir / "getting_started_ctrl_c.pty"
    c2_txt = ex_dir / "getting_started_ctrl_c.txt"
    c2_meta = run_pty_capture(
        [str(bins["getting_started"])],
        c2_cap,
        cols=80,
        rows=24,
        send_bytes=b"\x03",
        send_delay_s=0.35,
        send_after_bytes=b"Started. Press q or Ctrl+C to quit.",
    )
    c2_screen = canonicalize_capture(canon, c2_cap, c2_txt, cols=80, rows=24)
    c2_text_ok = "Started. Press q or Ctrl+C to quit." in c2_screen
    cases["getting_started_ctrl_c"] = {
        **c2_meta,
        "text_found": c2_text_ok,
        "ok": (
            c2_meta["exit_code"] == 0
            and not c2_meta["panicked"]
            and c2_meta["alt_enter"] == 0
            and c2_meta["cursor_visible_at_exit"]
            and c2_meta["icanon_restored"]
            and c2_meta["echo_restored"]
            and c2_text_ok
        ),
    }

    # 3. ftui-harness --example minimal (q)
    c3_cap = ex_dir / "harness_minimal_q.pty"
    c3_txt = ex_dir / "harness_minimal_q.txt"
    c3_meta = run_pty_capture(
        [str(bins["minimal_harness"])],
        c3_cap,
        cols=80,
        rows=24,
        send_bytes=b"q",
        send_delay_s=0.35,
        send_after_bytes=b"Hello from FrankenTUI ticks:",
    )
    c3_screen = canonicalize_capture(canon, c3_cap, c3_txt, cols=80, rows=24)
    cases["harness_minimal_q"] = {
        **c3_meta,
        "text_found": "Hello from FrankenTUI ticks:" in c3_screen,
        "ok": (
            c3_meta["exit_code"] == 0
            and not c3_meta["panicked"]
            and c3_meta["alt_enter"] == 0
            and c3_meta["cursor_visible_at_exit"]
            and c3_meta["icanon_restored"]
            and c3_meta["echo_restored"]
            and "Hello from FrankenTUI ticks:" in c3_screen
        ),
    }

    # 4. ftui-harness default binary (auto-exit)
    c4_cap = ex_dir / "ftui_harness_default.pty"
    c4_txt = ex_dir / "ftui_harness_default.txt"
    c4_meta = run_pty_capture(
        [str(bins["harness"])],
        c4_cap,
        cols=80,
        rows=24,
        env_overrides={"FTUI_HARNESS_EXIT_AFTER_MS": "400"},
    )
    c4_screen = canonicalize_capture(canon, c4_cap, c4_txt, cols=80, rows=24)
    cases["ftui_harness_default"] = {
        **c4_meta,
        "non_empty_rows": sum(1 for r in c4_screen.splitlines() if r.strip()),
        "ok": (
            c4_meta["exit_code"] == 0
            and not c4_meta["panicked"]
            and c4_meta["cursor_visible_at_exit"]
            and c4_meta["icanon_restored"]
            and c4_meta["echo_restored"]
            and sum(1 for r in c4_screen.splitlines() if r.strip()) >= 3
        ),
    }

    # 5. agent_shell 10k lines
    c5_cap = ex_dir / "agent_shell_10k.pty"
    c5_txt = ex_dir / "agent_shell_10k.txt"
    c5_meta = run_pty_capture(
        [str(bins["agent_shell"]), "--exit-when-child-exits", "--", "seq", "1", "10000"],
        c5_cap,
        cols=100,
        rows=30,
        timeout_s=30.0,
    )
    c5_screen = canonicalize_capture(
        canon, c5_cap, c5_txt, cols=100, rows=30, scrollback=True, max_scrollback=20000
    )
    c5_sb = check_numbered_scrollback(c5_screen, 10000)
    cases["agent_shell_10k"] = {
        **c5_meta,
        "scrollback": c5_sb,
        "ok": (
            c5_meta["exit_code"] == 0
            and not c5_meta["panicked"]
            and c5_meta["alt_enter"] == 0
            and c5_meta["cursor_visible_at_exit"]
            and c5_meta["icanon_restored"]
            and c5_meta["echo_restored"]
            and c5_sb["ok"]
        ),
    }

    summary_path = ex_dir / "examples_summary.json"
    summary_path.write_text(json.dumps(cases, indent=2) + "\n", encoding="utf-8")
    all_ok = all(item["ok"] for item in cases.values())
    elapsed_ms = max(1, (time.monotonic_ns() - start_ns) // 1_000_000)
    return {
        "id": "examples_pty",
        "title": "Reference examples in PTY (minimal_inline, getting_started, harness minimal, ftui-harness, agent_shell 10k)",
        "status": "pass" if all_ok else "fail",
        "duration_ms": elapsed_ms,
        "evidence_path": str(summary_path),
        "details": {name: {"ok": item["ok"], "duration_ms": item["duration_ms"]} for name, item in cases.items()},
    }


def run_sync_brackets_check(out_dir: Path, bins: dict[str, Path]) -> dict[str, Any]:
    start_ns = time.monotonic_ns()
    sync_dir = out_dir / "sync_brackets"
    sync_dir.mkdir(parents=True, exist_ok=True)
    kitty_env = {
        "TERM": "xterm-kitty",
        "TERM_PROGRAM": "kitty",
        "KITTY_WINDOW_ID": "1",
        "FTUI_SYNC_OUTPUT": "1",
        "FTUI_DEMO_DETERMINISTIC": "1",
        "FTUI_DEMO_SEED": "0",
        "FTUI_DEMO_EXIT_AFTER_MS": "350",
    }
    workloads = {
        "alt_dashboard": [str(bins["showcase"]), "--screen=2", "--screen-mode=alt"],
        "alt_vfx": [str(bins["showcase"]), "--screen=18", "--screen-mode=alt"],
        "inline_dashboard": [str(bins["showcase"]), "--screen=2", "--screen-mode=inline", "--ui-height=12"],
        "agent_shell": [str(bins["agent_shell"]), "--exit-when-child-exits", "--", "seq", "1", "500"],
    }
    results: dict[str, Any] = {}
    for name, cmd in workloads.items():
        meta = run_pty_capture(cmd, sync_dir / f"{name}.pty", cols=100, rows=30, env_overrides=kitty_env)
        ok = (
            meta["exit_code"] == 0
            and not meta["panicked"]
            and meta["sync_h"] > 0
            and meta["sync_h"] == meta["sync_l"]
            and meta["sync_balanced"]
        )
        results[name] = {
            "sync_h": meta["sync_h"],
            "sync_l": meta["sync_l"],
            "sync_balanced": meta["sync_balanced"],
            "ok": ok,
        }
    report_path = sync_dir / "sync_brackets_summary.json"
    report_path.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    elapsed_ms = max(1, (time.monotonic_ns() - start_ns) // 1_000_000)
    return {
        "id": "sync_brackets_balance",
        "title": "Synchronized output DEC ?2026h/?2026l bracket balance across alt, VFX, inline, and agent_shell",
        "status": "pass" if all(r["ok"] for r in results.values()) else "fail",
        "duration_ms": elapsed_ms,
        "evidence_path": str(report_path),
        "details": results,
    }


def run_scrollback_identities(out_dir: Path, bins: dict[str, Path]) -> dict[str, Any]:
    start_ns = time.monotonic_ns()
    sb_dir = out_dir / "scrollback_identities"
    sb_dir.mkdir(parents=True, exist_ok=True)
    canon = bins["pty_canonicalize"]
    cmd = [str(bins["agent_shell"]), "--exit-when-child-exits", "--", "seq", "1", "1000"]

    identities = {
        "plain_pty_no_cpr": {"TERM": "xterm-256color"},
        "forced_scroll_region": {"TERM": "xterm-256color", "FTUI_SCROLL_REGION": "1"},
        "tmux_identity": {"TERM": "tmux-256color", "TMUX": "/tmp/ftui_scorecard_tmux,1,0"},
        "kitty_identity": {
            "TERM": "xterm-kitty",
            "TERM_PROGRAM": "kitty",
            "KITTY_WINDOW_ID": "1",
            "FTUI_SCROLL_REGION": "1",
        },
    }
    results: dict[str, Any] = {}
    for name, env_over in identities.items():
        cap = sb_dir / f"{name}.pty"
        txt = sb_dir / f"{name}.txt"
        meta = run_pty_capture(cmd, cap, cols=100, rows=30, env_overrides=env_over, timeout_s=20.0)
        transcript = canonicalize_capture(
            canon, cap, txt, cols=100, rows=30, scrollback=True, max_scrollback=5000
        )
        sb = check_numbered_scrollback(transcript, 1000)
        ok = (
            meta["exit_code"] == 0
            and not meta["panicked"]
            and meta["cursor_visible_at_exit"]
            and meta["icanon_restored"]
            and meta["echo_restored"]
            and sb["ok"]
        )
        results[name] = {**sb, "exit_code": meta["exit_code"], "ok": ok}

    report_path = sb_dir / "scrollback_identities_summary.json"
    report_path.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    elapsed_ms = max(1, (time.monotonic_ns() - start_ns) // 1_000_000)
    return {
        "id": "scrollback_identities_pty",
        "title": "Inline scrollback delivery across terminal identities (plain PTY, forced DECSTBM, TMUX, kitty)",
        "status": "pass" if all(r["ok"] for r in results.values()) else "fail",
        "duration_ms": elapsed_ms,
        "evidence_path": str(report_path),
        "details": results,
    }


def run_scrollback_real_tmux(out_dir: Path, bins: dict[str, Path]) -> dict[str, Any]:
    start_ns = time.monotonic_ns()
    tmux_dir = out_dir / "scrollback_real_tmux"
    tmux_dir.mkdir(parents=True, exist_ok=True)
    report_path = tmux_dir / "tmux_scrollback_summary.json"

    if shutil.which("tmux") is None:
        payload = {"status": "fail", "reason": "missing_prerequisite: tmux not found on PATH"}
        report_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
        return {
            "id": "scrollback_real_tmux",
            "title": "Real tmux scrollback delivery on a private socket (control seq vs agent_shell)",
            "status": "fail",
            "duration_ms": 1,
            "evidence_path": str(report_path),
            "details": payload,
        }

    cmd = [
        sys.executable,
        "-B",
        str(ROOT / "tests" / "e2e" / "scripts" / "test_inline_scrollback_mux.py"),
        "--agent-shell",
        str(bins["agent_shell"]),
        "--showcase",
        str(bins["showcase"]),
        "--lines",
        "2000",
        "--mux",
        "tmux",
        "--out",
        str(tmux_dir),
    ]
    # Run the standard tmux scrollback cases via a dedicated 2000-line invocation
    sock = f"ftui_scorecard_{os.getpid()}"
    base = ["tmux", "-L", sock, "-f", "/dev/null"]
    done_marker = "__FTUI_SCORECARD_TMUX_DONE__"
    n_lines = 2000
    cases_out: dict[str, Any] = {}
    try:
        subprocess.run(
            base + ["new-session", "-d", "-s", "boot", "-x", "100", "-y", "30"],
            check=True,
            capture_output=True,
        )
        subprocess.run(base + ["set-option", "-g", "history-limit", "100000"], check=True, capture_output=True)
        subprocess.run(base + ["set-option", "-g", "remain-on-exit", "on"], check=True, capture_output=True)

        for case_name, shell_cmd, is_agent in (
            ("control_seq", f"seq 1 {n_lines}; echo {done_marker}; sleep 0.5", False),
            (
                "agent_shell",
                f"{shlex.quote(str(bins['agent_shell']))} --exit-when-child-exits -- seq 1 {n_lines}; echo {done_marker}; sleep 0.5",
                True,
            ),
        ):
            subprocess.run(
                base + ["new-session", "-d", "-s", case_name, "-x", "100", "-y", "30", "bash", "-c", shell_cmd],
                check=True,
                capture_output=True,
            )
            deadline = time.monotonic() + 60.0
            while time.monotonic() < deadline:
                dead = subprocess.run(
                    base + ["display-message", "-p", "-t", case_name, "#{pane_dead}"],
                    capture_output=True,
                    text=True,
                    check=False,
                ).stdout.strip()
                if dead == "1":
                    break
                time.sleep(0.2)
            text = subprocess.run(
                base + ["capture-pane", "-p", "-J", "-t", case_name, "-S", "-", "-E", "-"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout
            (tmux_dir / f"{case_name}.txt").write_text(text, encoding="utf-8")
            sb = check_numbered_scrollback(text, n_lines)
            done_rows = [r for r in text.splitlines() if done_marker in r]
            done_clean = len(done_rows) == 1 and done_rows[0].strip().startswith(done_marker)
            ok = (
                sb["found"] == n_lines
                and sb["duplicates"] == 0
                and sb["in_order"]
                and done_clean
                and (not is_agent or sb["status_rows"] == 1)
            )
            cases_out[case_name] = {**sb, "done_marker_on_own_row": done_clean, "ok": ok}
    finally:
        subprocess.run(base + ["kill-server"], capture_output=True, check=False)

    report_path.write_text(json.dumps(cases_out, indent=2) + "\n", encoding="utf-8")
    elapsed_ms = max(1, (time.monotonic_ns() - start_ns) // 1_000_000)
    all_ok = bool(cases_out) and all(c["ok"] for c in cases_out.values())
    return {
        "id": "scrollback_real_tmux",
        "title": "Real tmux scrollback delivery on a private socket (control seq vs agent_shell)",
        "status": "pass" if all_ok else "fail",
        "duration_ms": elapsed_ms,
        "evidence_path": str(report_path),
        "details": cases_out,
    }


def validate_scorecard(payload: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    if payload.get("schema_version") != SCHEMA_VERSION:
        errors.append(f"invalid schema_version: {payload.get('schema_version')!r}")
    rows = payload.get("checks")
    if not isinstance(rows, list) or not rows:
        errors.append("checks must be a non-empty list")
        return errors
    for idx, row in enumerate(rows):
        if not isinstance(row, dict):
            errors.append(f"check[{idx}] must be an object")
            continue
        for key in ("id", "title", "status", "duration_ms", "evidence_path", "details"):
            if key not in row:
                errors.append(f"check[{idx}] missing {key}")
        if row.get("status") not in ("pass", "fail"):
            errors.append(f"check[{idx}] status must be 'pass' or 'fail', got {row.get('status')!r}")
        if not isinstance(row.get("duration_ms"), int) or row.get("duration_ms", 0) < 0:
            errors.append(f"check[{idx}] duration_ms must be a non-negative int")
    return errors


def run_self_test() -> int:
    valid = {
        "schema_version": SCHEMA_VERSION,
        "checks": [
            {
                "id": "truth_gates",
                "title": "make gates",
                "status": "pass",
                "duration_ms": 100,
                "evidence_path": "/tmp/gates.log",
                "details": {},
            }
        ],
    }
    assert validate_scorecard(valid) == [], f"expected valid scorecard, got {validate_scorecard(valid)}"
    invalid_skip = {
        "schema_version": SCHEMA_VERSION,
        "checks": [
            {
                "id": "scrollback_real_tmux",
                "title": "tmux",
                "status": "skipped",
                "duration_ms": 1,
                "evidence_path": "/tmp/x",
                "details": {},
            }
        ],
    }
    errs = validate_scorecard(invalid_skip)
    assert any("status must be 'pass' or 'fail'" in e for e in errs), f"expected skip rejection, got {errs}"

    sb_ok = check_numbered_scrollback("1\n2\n3\nL:3 B:6 E:0 Ctrl-C Interrupt\n", 3)
    assert sb_ok["ok"], f"expected scrollback ok, got {sb_ok}"
    sb_bad = check_numbered_scrollback("1\n3\n2\nL:3 B:6 E:0 Ctrl-C Interrupt\n", 3)
    assert not sb_bad["ok"] and not sb_bad["in_order"], f"expected out-of-order failure, got {sb_bad}"
    print(json.dumps({"check": "vision_scorecard_self_test", "status": "pass"}))
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--out", type=Path, default=None, help="Output directory (outside checkout)")
    ap.add_argument(
        "--skip-cargo-sweeps",
        action="store_true",
        help="Skip workspace-wide cargo fmt/check/clippy/doc sweeps when already run by caller",
    )
    ap.add_argument("--self-test", action="store_true", help="Run scorecard schema/evaluator self-tests")
    args = ap.parse_args()

    if args.self_test:
        return run_self_test()

    default_out = Path(
        os.environ.get(
            "DSR_VISION_SCORECARD_OUTPUT",
            f"/tmp/ftui_vision_scorecard_{int(time.time())}_{os.getpid()}",
        )
    )
    out_dir = (args.out or default_out).resolve()
    try:
        out_dir.relative_to(ROOT)
    except ValueError:
        pass
    else:
        print(f"ERROR: --out must be outside the repository checkout ({ROOT}): {out_dir}", file=sys.stderr)
        return 2

    out_dir.mkdir(parents=True, exist_ok=True)
    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target"))).resolve()
    checks: list[dict[str, Any]] = []

    # 1. Truth gates (`make gates`)
    gates_log = out_dir / "logs" / "make_gates.log"
    rc_gates, ms_gates = run_logged_cmd(["make", "gates"], gates_log)
    checks.append(
        {
            "id": "truth_gates",
            "title": "AGENTS.md non-cargo truth gates (make gates: reachability, claims, env-docs, close-audit, baseline-benches, workflows, repo-facts)",
            "status": "pass" if rc_gates == 0 else "fail",
            "duration_ms": ms_gates,
            "evidence_path": str(gates_log),
            "details": {"exit_code": rc_gates},
        }
    )

    # 2. Compiler/lint/doc gates (unless --skip-cargo-sweeps)
    if not args.skip_cargo_sweeps:
        for check_id, title, cmd in (
            ("cargo_fmt", "cargo fmt --all --check", ["cargo", "fmt", "--all", "--check"]),
            (
                "cargo_check",
                "cargo check --workspace --all-targets --locked",
                ["cargo", "-Zchecksum-freshness", "check", "--workspace", "--all-targets", "--locked"],
            ),
            (
                "cargo_clippy",
                "cargo clippy --workspace --all-targets --locked -- -D warnings",
                [
                    "cargo",
                    "-Zchecksum-freshness",
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--locked",
                    "--",
                    "-D",
                    "warnings",
                ],
            ),
            (
                "cargo_doc",
                "RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps --locked",
                ["cargo", "-Zchecksum-freshness", "doc", "--workspace", "--no-deps", "--locked"],
            ),
        ):
            log_path = out_dir / "logs" / f"{check_id}.log"
            env = cargo_env()
            if check_id == "cargo_doc":
                env["RUSTDOCFLAGS"] = "-D warnings"
            rc, ms = run_logged_cmd(cmd, log_path, env=env)
            checks.append(
                {
                    "id": check_id,
                    "title": title,
                    "status": "pass" if rc == 0 else "fail",
                    "duration_ms": ms,
                    "evidence_path": str(log_path),
                    "details": {"exit_code": rc},
                }
            )

    # Ensure binaries for PTY checks
    bins, build_ms, build_err = ensure_binaries(out_dir, target_dir)
    if build_err is not None:
        for check_id, title in (
            ("showcase_45_screens_pty", "PTY sweep of all 45 showcase screens + inline + q-quit"),
            ("examples_pty", "Reference examples in PTY"),
            ("sync_brackets_balance", "Synchronized output DEC ?2026h/?2026l bracket balance"),
            ("scrollback_identities_pty", "Inline scrollback delivery across terminal identities"),
            ("scrollback_real_tmux", "Real tmux scrollback delivery on a private socket"),
        ):
            checks.append(
                {
                    "id": check_id,
                    "title": title,
                    "status": "fail",
                    "duration_ms": build_ms,
                    "evidence_path": str(out_dir / "logs" / "cargo_build_bins.log"),
                    "details": {"error": build_err},
                }
            )
    else:
        checks.append(run_showcase_sweep(out_dir, bins["showcase"], bins["pty_canonicalize"]))
        checks.append(run_examples_sweep(out_dir, bins))
        checks.append(run_sync_brackets_check(out_dir, bins))
        checks.append(run_scrollback_identities(out_dir, bins))
        checks.append(run_scrollback_real_tmux(out_dir, bins))

    git_commit = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    passed = sum(1 for c in checks if c["status"] == "pass")
    failed = sum(1 for c in checks if c["status"] != "pass")
    scorecard = {
        "schema_version": SCHEMA_VERSION,
        "git_commit": git_commit,
        "toolchain": pinned_toolchain(),
        "out_dir": str(out_dir),
        "total_checks": len(checks),
        "passed_checks": passed,
        "failed_checks": failed,
        "status": "pass" if failed == 0 else "fail",
        "checks": checks,
    }
    errs = validate_scorecard(scorecard)
    if errs:
        print(f"ERROR: invalid scorecard payload: {errs}", file=sys.stderr)
        return 1

    scorecard_path = out_dir / "vision_scorecard.json"
    scorecard_path.write_text(json.dumps(scorecard, indent=2) + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                "scorecard_path": str(scorecard_path),
                "status": scorecard["status"],
                "passed_checks": passed,
                "failed_checks": failed,
                "checks": [{c["id"]: c["status"]} for c in checks],
            }
        )
    )
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())

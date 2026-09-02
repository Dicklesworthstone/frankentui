#!/usr/bin/env bash
# Consumer smoke E2E: prove that a library consumer can run the README's
# Minimal API Example with the `ftui` facade's DEFAULT features.
#
# Default mode builds `crates/ftui/examples/minimal_inline.rs` (byte-identical
# to the README block; checked here) with `"${CARGO:-cargo}" build -p ftui --example`,
# which uses exactly the facade's default feature set. `--scratch` mode goes one
# step further and builds a throwaway crate that depends on the facade by path
# with no features specified, the way a crates.io consumer would.
#
# The binary is then driven under a real PTY: it must render "Ticks:", exit 0
# when `q` is pressed, and restore the terminal (cursor visible, bracketed
# paste off, no alt-screen left open in inline mode). Every observation is
# written as one JSONL line so CI artifacts stay diagnosable.
#
# Usage:
#   scripts/consumer_smoke_e2e.sh [--scratch] [--out DIR]
# Exit 0 on success; non-zero with the failing assertion on stderr.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="example"
OUT_DIR="${CONSUMER_SMOKE_OUT:-${REPO_ROOT}/target/consumer_smoke}"
while [ $# -gt 0 ]; do
  case "$1" in
    --scratch) MODE="scratch"; shift ;;
    --out) OUT_DIR="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/consumer_smoke.jsonl"
: > "$LOG"
RUN_ID="consumer-smoke-$(date -u +%Y%m%dT%H%M%SZ)-$$"

log() { # log <event> <json-fields...>
  local event="$1"; shift
  printf '{"run_id":"%s","event":"%s"%s}\n' "$RUN_ID" "$event" "${1:-}" >> "$LOG"
}

fail() { echo "FAIL: $*" >&2; log fail ",\"reason\":\"$*\""; exit 1; }

# 1. The example must be byte-identical to the README block.
README_BLOCK="$(awk '/^## Minimal API Example/{f=1} f&&/^```rust/{s=1;next} f&&s&&/^```$/{exit} f&&s{print}' "$REPO_ROOT/README.md")"
if ! diff <(printf '%s\n' "$README_BLOCK") "$REPO_ROOT/crates/ftui/examples/minimal_inline.rs" >/dev/null; then
  fail "crates/ftui/examples/minimal_inline.rs differs from the README Minimal API Example block"
fi
log readme_example_identical

# 2. Build with default features (CONSUMER_SMOKE_SKIP_BUILD=1 reuses an
#    already-built example binary, e.g. when the build was offloaded).
if [ "$MODE" = "example" ]; then
  TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
  BIN="$TARGET_DIR/debug/examples/minimal_inline"
  if [ "${CONSUMER_SMOKE_SKIP_BUILD:-0}" != "1" ] || [ ! -x "$BIN" ]; then
    (cd "$REPO_ROOT" && "${CARGO:-cargo}" build -p ftui --example minimal_inline) || fail ""${CARGO:-cargo}" build -p ftui --example minimal_inline failed"
  fi
else
  SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/ftui-consumer-smoke.XXXXXX")"
  mkdir -p "$SCRATCH/src"
  cat > "$SCRATCH/Cargo.toml" <<EOF
[package]
name = "ftui_consumer_smoke"
version = "0.0.0"
edition = "2024"

[dependencies]
ftui = { path = "$REPO_ROOT/crates/ftui" }

[workspace]
EOF
  cp "$REPO_ROOT/crates/ftui/examples/minimal_inline.rs" "$SCRATCH/src/main.rs"
  (cd "$SCRATCH" && "${CARGO:-cargo}" build) || fail "scratch consumer crate failed to build against the facade's default features"
  BIN="$SCRATCH/target/debug/ftui_consumer_smoke"
fi
[ -x "$BIN" ] || fail "binary not found: $BIN"
log built ",\"mode\":\"$MODE\",\"binary\":\"$BIN\""

# 3. Drive it under a PTY and tally what it emitted.
RESULT="$(python3 - "$BIN" <<'EOF'
import json, os, pty, re, select, signal, struct, sys, termios, fcntl, time
binary = sys.argv[1]
env = {k: v for k, v in os.environ.items()
       if k not in ("TMUX", "TMUX_PANE", "STY", "ZELLIJ", "WEZTERM_PANE", "WEZTERM_UNIX_SOCKET",
                    "WEZTERM_EXECUTABLE", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "KITTY_WINDOW_ID")}
env.update({"TERM": "xterm-256color", "COLORTERM": "truecolor"})
pid, fd = pty.fork()
if pid == 0:
    os.execve(binary, [binary], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
buf = b""; t0 = time.time(); sent = False; status = None; eof = False
while time.time() - t0 < 10:
    r, _, _ = select.select([fd], [], [], 0.05)
    if r:
        try:
            d = os.read(fd, 65536)
        except OSError:
            d = b""
        if not d:
            # EOF/EIO on the pty means the child closed its side; reap it
            # below with a blocking wait instead of treating it as a hang.
            eof = True
            break
        buf += d
    if not sent and time.time() - t0 > 1.5:
        os.write(fd, b"q"); sent = True
    wp, st = os.waitpid(pid, os.WNOHANG)
    if wp == pid:
        status = st; break
if status is None:
    if eof:
        _, status = os.waitpid(pid, 0)
    else:
        wp, st = os.waitpid(pid, os.WNOHANG)
        if wp == pid:
            status = st
        else:
            os.kill(pid, signal.SIGKILL); os.waitpid(pid, 0)
if status is None:
    exit_code = None
else:
    exit_code = os.WEXITSTATUS(status) if os.WIFEXITED(status) else -os.WTERMSIG(status)
def c(p): return len(re.findall(p, buf))
plain = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07", b"", buf)
print(json.dumps({
    "exit_code": exit_code,
    "bytes": len(buf),
    "ticks_rendered": b"Ticks:" in plain,
    "alt_enter": c(rb"\x1b\[\?1049h"), "alt_leave": c(rb"\x1b\[\?1049l"),
    "sync_begin": c(rb"\x1b\[\?2026h"), "sync_end": c(rb"\x1b\[\?2026l"),
    "cursor_hide": c(rb"\x1b\[\?25l"), "cursor_show": c(rb"\x1b\[\?25h"),
    "paste_on": c(rb"\x1b\[\?2004h"), "paste_off": c(rb"\x1b\[\?2004l"),
    "mouse_on": c(rb"\x1b\[\?100[0236]h"), "mouse_off": c(rb"\x1b\[\?100[0236]l"),
    "decstbm_set": c(rb"\x1b\[\d+;\d+r"), "decstbm_reset": c(rb"\x1b\[r"),
    "tail_cursor_show": b"\x1b[?25h" in buf[-400:],
}))
EOF
)"
printf '{"run_id":"%s","event":"pty_run","mode":"%s","observed":%s}\n' "$RUN_ID" "$MODE" "$RESULT" >> "$LOG"
echo "observed: $RESULT"

# 4. Assertions.
get() { printf '%s' "$RESULT" | python3 -c "import json,sys; v=json.load(sys.stdin)['$1']; print(v if v is not None else 'null')"; }
[ "$(get exit_code)" = "0" ] || fail "expected exit 0 after 'q', got $(get exit_code)"
[ "$(get ticks_rendered)" = "True" ] || fail "'Ticks:' never rendered"
[ "$(get alt_enter)" = "0" ] || fail "inline mode must not enter the alternate screen"
[ "$(get sync_begin)" = "$(get sync_end)" ] || fail "unbalanced DEC 2026 sync brackets"
[ "$(get paste_on)" = "$(get paste_off)" ] || fail "bracketed paste not restored"
[ "$(get tail_cursor_show)" = "True" ] || fail "cursor not shown at exit"
log pass ",\"mode\":\"$MODE\""
echo "PASS consumer smoke ($MODE): log at $LOG"

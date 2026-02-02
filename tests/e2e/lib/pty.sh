#!/bin/bash
set -euo pipefail

pty_run() {
    local output_file="$1"
    shift

    if [[ -z "${E2E_PYTHON:-}" ]]; then
        echo "E2E_PYTHON is not set (python3/python not found)" >&2
        return 1
    fi

    local timeout="${PTY_TIMEOUT:-5}"
    local send_data="${PTY_SEND:-}"
    local send_delay_ms="${PTY_SEND_DELAY_MS:-0}"
    local cols="${PTY_COLS:-80}"
    local rows="${PTY_ROWS:-24}"

    PTY_OUTPUT="$output_file" \
    PTY_TIMEOUT="$timeout" \
    PTY_SEND="$send_data" \
    PTY_SEND_DELAY_MS="$send_delay_ms" \
    PTY_COLS="$cols" \
    PTY_ROWS="$rows" \
    "$E2E_PYTHON" - "$@" <<'PY'
import codecs
import os
import pty
import select
import subprocess
import sys
import time

cmd = sys.argv[1:]
if not cmd:
    print("No command provided", file=sys.stderr)
    sys.exit(2)

output_path = os.environ.get("PTY_OUTPUT")
if not output_path:
    print("PTY_OUTPUT not set", file=sys.stderr)
    sys.exit(2)

timeout = float(os.environ.get("PTY_TIMEOUT", "5"))
raw_send = os.environ.get("PTY_SEND", "")
send_delay_ms = int(os.environ.get("PTY_SEND_DELAY_MS", "0"))
cols = int(os.environ.get("PTY_COLS", "80"))
rows = int(os.environ.get("PTY_ROWS", "24"))

send_bytes = b""
if raw_send:
    send_bytes = codecs.decode(raw_send, "unicode_escape").encode("utf-8")

master_fd, slave_fd = pty.openpty()

try:
    import fcntl
    import struct
    import termios

    winsize = struct.pack("HHHH", rows, cols, 0, 0)
    fcntl.ioctl(slave_fd, termios.TIOCSWINSZ, winsize)
except Exception:
    pass

start = time.time()

proc = subprocess.Popen(
    cmd,
    stdin=slave_fd,
    stdout=slave_fd,
    stderr=slave_fd,
    close_fds=True,
    env=os.environ.copy(),
)

os.close(slave_fd)

captured = bytearray()
limit = start + timeout
sent = False

while True:
    now = time.time()
    if (not sent) and send_bytes and (now - start) >= (send_delay_ms / 1000.0):
        try:
            os.write(master_fd, send_bytes)
            sent = True
        except OSError:
            pass

    if now >= limit:
        try:
            proc.terminate()
        except Exception:
            pass
        time.sleep(0.2)
        if proc.poll() is None:
            try:
                proc.kill()
            except Exception:
                pass
        break

    rlist, _, _ = select.select([master_fd], [], [], 0.05)
    if rlist:
        try:
            chunk = os.read(master_fd, 4096)
        except OSError:
            break
        if not chunk:
            break
        captured.extend(chunk)

    if proc.poll() is not None and not rlist:
        break

exit_code = proc.poll()
if exit_code is None:
    exit_code = 124

with open(output_path, "wb") as handle:
    handle.write(captured)

sys.exit(exit_code)
PY
}

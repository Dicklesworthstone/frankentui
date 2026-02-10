#!/bin/bash
set -euo pipefail

PTY_CANONICALIZE="${PTY_CANONICALIZE:-0}"
PTY_CANONICALIZE_BIN="${PTY_CANONICALIZE_BIN:-}"
PTY_CANONICALIZE_BUILT="${PTY_CANONICALIZE_BUILT:-}"

pty_timestamp() {
    if declare -f e2e_timestamp >/dev/null 2>&1; then
        e2e_timestamp
        return 0
    fi
    date -Iseconds
}

resolve_canonicalize_bin() {
    if [[ -n "${PTY_CANONICALIZE_BIN:-}" && -x "$PTY_CANONICALIZE_BIN" ]]; then
        echo "$PTY_CANONICALIZE_BIN"
        return 0
    fi

    local debug_bin="$PROJECT_ROOT/target/debug/pty_canonicalize"
    local release_bin="$PROJECT_ROOT/target/release/pty_canonicalize"
    if [[ -x "$debug_bin" ]]; then
        echo "$debug_bin"
        return 0
    fi
    if [[ -x "$release_bin" ]]; then
        echo "$release_bin"
        return 0
    fi

    if [[ "$PTY_CANONICALIZE" != "1" ]]; then
        return 1
    fi

    if [[ -n "${PTY_CANONICALIZE_BUILT:-}" ]]; then
        return 1
    fi

    if ! command -v cargo >/dev/null 2>&1; then
        return 1
    fi

    (cd "$PROJECT_ROOT" && cargo build -q -p ftui-pty --bin pty_canonicalize) || return 1
    PTY_CANONICALIZE_BUILT=1

    if [[ -x "$debug_bin" ]]; then
        echo "$debug_bin"
        return 0
    fi
    if [[ -x "$release_bin" ]]; then
        echo "$release_bin"
        return 0
    fi

    return 1
}

pty_canonicalize_file() {
    local input_file="$1"
    local output_file="$2"
    local cols="$3"
    local rows="$4"
    local bin
    if ! bin="$(resolve_canonicalize_bin)"; then
        return 1
    fi
    "$bin" --input "$input_file" --output "$output_file" --cols "$cols" --rows "$rows"
}

pty_record_metadata() {
    local output_file="$1"
    local exit_code="$2"
    local cols="$3"
    local rows="$4"
    local jsonl="${PTY_JSONL:-}"
    if [[ -z "$jsonl" ]]; then
        if [[ -z "${E2E_LOG_DIR:-}" ]]; then
            return 0
        fi
        jsonl="$E2E_LOG_DIR/pty_metadata.jsonl"
    fi

    mkdir -p "$(dirname "$jsonl")"

    local canonical_file="${PTY_CANONICAL_FILE:-}"
    local output_bytes=0
    local canonical_bytes=0
    if [[ -f "$output_file" ]]; then
        output_bytes=$(wc -c < "$output_file" | tr -d ' ')
    fi
    if [[ -n "$canonical_file" && -f "$canonical_file" ]]; then
        canonical_bytes=$(wc -c < "$canonical_file" | tr -d ' ')
    fi

    local output_sha=""
    local canonical_sha=""
    if command -v sha256sum >/dev/null 2>&1; then
        if [[ -f "$output_file" ]]; then
            output_sha=$(sha256sum "$output_file" | awk '{print $1}')
        fi
        if [[ -n "$canonical_file" && -f "$canonical_file" ]]; then
            canonical_sha=$(sha256sum "$canonical_file" | awk '{print $1}')
        fi
    fi

    local test_name="${PTY_TEST_NAME:-}"
    if [[ -z "$test_name" ]]; then
        test_name="$(basename "$output_file")"
    fi

    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --arg timestamp "$(pty_timestamp)" \
            --arg test_name "$test_name" \
            --arg output_file "$output_file" \
            --arg canonical_file "$canonical_file" \
            --arg term "${TERM:-}" \
            --arg colorterm "${COLORTERM:-}" \
            --arg no_color "${NO_COLOR:-}" \
            --arg output_sha "$output_sha" \
            --arg canonical_sha "$canonical_sha" \
            --argjson output_bytes "$output_bytes" \
            --argjson canonical_bytes "$canonical_bytes" \
            --argjson cols "$cols" \
            --argjson rows "$rows" \
            --argjson exit_code "$exit_code" \
            '{timestamp:$timestamp,test_name:$test_name,output_file:$output_file,canonical_file:$canonical_file,cols:$cols,rows:$rows,exit_code:$exit_code,output_bytes:$output_bytes,canonical_bytes:$canonical_bytes,output_sha256:$output_sha,canonical_sha256:$canonical_sha,term:$term,colorterm:$colorterm,no_color:$no_color}' \
            >> "$jsonl"
    else
        printf '{"timestamp":"%s","test_name":"%s","output_file":"%s","canonical_file":"%s","cols":%s,"rows":%s,"exit_code":%s,"output_bytes":%s,"canonical_bytes":%s,"output_sha256":"%s","canonical_sha256":"%s","term":"%s","colorterm":"%s","no_color":"%s"}\n' \
            "$(pty_timestamp)" "$test_name" "$output_file" "$canonical_file" "$cols" "$rows" "$exit_code" "$output_bytes" "$canonical_bytes" "$output_sha" "$canonical_sha" "${TERM:-}" "${COLORTERM:-}" "${NO_COLOR:-}" \
            >> "$jsonl"
    fi

    if declare -f jsonl_pty_capture >/dev/null 2>&1; then
        jsonl_pty_capture "$output_file" "$cols" "$rows" "$exit_code" "$canonical_file"
    fi
}

pty_run() {
    local output_file="$1"
    shift

    if [[ -z "${E2E_PYTHON:-}" ]]; then
        echo "E2E_PYTHON is not set (python3/python not found)" >&2
        return 1
    fi

    local timeout="${PTY_TIMEOUT:-5}"
    local send_data="${PTY_SEND:-}"
    local send_file="${PTY_SEND_FILE:-}"
    local send_delay_ms="${PTY_SEND_DELAY_MS:-0}"
    local cols="${PTY_COLS:-80}"
    local rows="${PTY_ROWS:-24}"
    local drain_timeout_ms="${PTY_DRAIN_TIMEOUT_MS:-200}"
    local terminate_grace_ms="${PTY_TERMINATE_GRACE_MS:-300}"
    local read_poll_ms="${PTY_READ_POLL_MS:-50}"
    local read_chunk="${PTY_READ_CHUNK:-4096}"
    local retries="${PTY_RETRIES:-1}"
    local retry_delay_ms="${PTY_RETRY_DELAY_MS:-100}"
    local min_bytes="${PTY_MIN_BYTES:-0}"

    local attempt=1
    local exit_code=0
    while [[ "$attempt" -le "$retries" ]]; do
        if PTY_OUTPUT="$output_file" \
            PTY_TIMEOUT="$timeout" \
            PTY_SEND="$send_data" \
            PTY_SEND_FILE="$send_file" \
            PTY_SEND_DELAY_MS="$send_delay_ms" \
            PTY_COLS="$cols" \
            PTY_ROWS="$rows" \
            PTY_DRAIN_TIMEOUT_MS="$drain_timeout_ms" \
            PTY_TERMINATE_GRACE_MS="$terminate_grace_ms" \
            PTY_READ_POLL_MS="$read_poll_ms" \
            PTY_READ_CHUNK="$read_chunk" \
            "$E2E_PYTHON" - "$@" <<'PY'
import codecs
import os
import pty
import select
import subprocess
import sys
import time
import signal

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
send_file = os.environ.get("PTY_SEND_FILE", "")
send_delay_ms = int(os.environ.get("PTY_SEND_DELAY_MS", "0"))
cols = int(os.environ.get("PTY_COLS", "80"))
rows = int(os.environ.get("PTY_ROWS", "24"))
resize_delay_ms = int(os.environ.get("PTY_RESIZE_DELAY_MS", "0"))
resize_cols = os.environ.get("PTY_RESIZE_COLS")
resize_rows = os.environ.get("PTY_RESIZE_ROWS")
drain_timeout = float(os.environ.get("PTY_DRAIN_TIMEOUT_MS", "200")) / 1000.0
terminate_grace = float(os.environ.get("PTY_TERMINATE_GRACE_MS", "300")) / 1000.0
read_poll = float(os.environ.get("PTY_READ_POLL_MS", "50")) / 1000.0
read_chunk = int(os.environ.get("PTY_READ_CHUNK", "4096"))
capture_max_raw = os.environ.get("PTY_CAPTURE_MAX_BYTES", "")
capture_max = None
if capture_max_raw:
    try:
        capture_max = int(capture_max_raw)
        if capture_max < 0:
            capture_max = None
    except ValueError:
        print("Invalid PTY_CAPTURE_MAX_BYTES (expected integer)", file=sys.stderr)
        sys.exit(2)

send_bytes = b""
if send_file:
    try:
        with open(send_file, "rb") as handle:
            send_bytes = handle.read()
    except Exception as exc:
        print(f"Failed to read PTY_SEND_FILE: {exc}", file=sys.stderr)
        sys.exit(2)
elif raw_send:
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

try:
    import fcntl

    flags = fcntl.fcntl(master_fd, fcntl.F_GETFL)
    fcntl.fcntl(master_fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)
except Exception:
    pass

start = time.monotonic()
deadline = start + timeout
resize_at = None
resize_done = False
resize_cols_int = None
resize_rows_int = None
if resize_delay_ms > 0 and resize_cols and resize_rows:
    try:
        resize_cols_int = int(resize_cols)
        resize_rows_int = int(resize_rows)
        resize_at = start + (resize_delay_ms / 1000.0)
    except ValueError:
        resize_at = None

def _preexec_setup_controlling_tty() -> None:
    # `pty.openpty()` alone does not guarantee `/dev/tty` is available in the
    # child when launching as a new session. Make the slave PTY the controlling
    # terminal so terminal libraries (e.g. crossterm) can open `/dev/tty`.
    try:
        os.setsid()
    except Exception:
        pass
    try:
        import fcntl
        import termios

        fcntl.ioctl(0, termios.TIOCSCTTY, 0)
    except Exception:
        pass

proc = subprocess.Popen(
    cmd,
    stdin=slave_fd,
    stdout=slave_fd,
    stderr=slave_fd,
    close_fds=True,
    env=os.environ.copy(),
    start_new_session=False,
    preexec_fn=_preexec_setup_controlling_tty,
)
slave_fd_open = slave_fd

captured = bytearray()
sent = False
last_data = start
terminate_at = None
stop_at = None

try:
    while True:
        now = time.monotonic()
        if (not sent) and send_bytes and (now - start) >= (send_delay_ms / 1000.0):
            try:
                os.write(master_fd, send_bytes)
                sent = True
            except OSError:
                pass

        if resize_at is not None and (not resize_done) and now >= resize_at:
            if resize_cols_int and resize_rows_int:
                try:
                    import fcntl
                    import struct
                    import termios

                    winsize = struct.pack("HHHH", resize_rows_int, resize_cols_int, 0, 0)
                    try:
                        fcntl.ioctl(master_fd, termios.TIOCSWINSZ, winsize)
                    except Exception:
                        pass
                    if slave_fd_open is not None:
                        try:
                            fcntl.ioctl(slave_fd_open, termios.TIOCSWINSZ, winsize)
                        except Exception:
                            pass
                    try:
                        os.killpg(proc.pid, signal.SIGWINCH)
                    except Exception:
                        pass
                except Exception:
                    pass
            resize_done = True

        if terminate_at is None and now >= deadline:
            terminate_at = now + terminate_grace
            stop_at = terminate_at + drain_timeout
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except Exception:
                try:
                    proc.terminate()
                except Exception:
                    pass

        if terminate_at is not None and now >= terminate_at:
            if proc.poll() is None:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except Exception:
                    try:
                        proc.kill()
                    except Exception:
                        pass

        rlist, _, _ = select.select([master_fd], [], [], read_poll)
        if rlist:
            eof = False
            while True:
                try:
                    chunk = os.read(master_fd, read_chunk)
                except BlockingIOError:
                    break
                except OSError:
                    eof = True
                    break
                if not chunk:
                    eof = True
                    break
                if capture_max != 0:
                    captured.extend(chunk)
                    if capture_max is not None and capture_max > 0:
                        # Avoid O(n) tail-trimming on every read; trim lazily.
                        if len(captured) > capture_max * 2:
                            del captured[: len(captured) - capture_max]
                last_data = now
            if eof:
                break

        exit_code = proc.poll()
        if exit_code is not None:
            if now - last_data >= drain_timeout:
                break

        if stop_at is not None and now >= stop_at and (now - last_data >= drain_timeout):
            break
finally:
    try:
        os.close(master_fd)
    except Exception:
        pass
    if slave_fd_open is not None:
        try:
            os.close(slave_fd_open)
        except Exception:
            pass

exit_code = proc.poll()
if exit_code is None:
    exit_code = 124

if capture_max is not None and capture_max > 0 and len(captured) > capture_max:
    del captured[: len(captured) - capture_max]

with open(output_path, "wb") as handle:
    handle.write(captured)

sys.exit(exit_code)
PY
        then
            exit_code=0
        else
            exit_code=$?
        fi
        PTY_LAST_OUTPUT="$output_file"
        PTY_LAST_EXIT="$exit_code"
        PTY_CANONICAL_FILE=""

        if [[ "$PTY_CANONICALIZE" == "1" && -f "$output_file" ]]; then
            local canonical_file="${output_file%.pty}.canonical.txt"
            if pty_canonicalize_file "$output_file" "$canonical_file" "$cols" "$rows"; then
                PTY_CANONICAL_FILE="$canonical_file"
            fi
        fi

        pty_record_metadata "$output_file" "$exit_code" "$cols" "$rows"

        if [[ "$retries" -le 1 ]]; then
            return "$exit_code"
        fi
        local size=0
        if [[ -f "$output_file" ]]; then
            size=$(wc -c < "$output_file" | tr -d ' ')
        fi
        if [[ "$exit_code" -eq 0 ]] && [[ "$size" -ge "$min_bytes" ]]; then
            return 0
        fi
        if [[ "$attempt" -ge "$retries" ]]; then
            return "$exit_code"
        fi
        local retry_delay_s
        retry_delay_s="$(awk -v ms="$retry_delay_ms" 'BEGIN {printf "%.3f", ms/1000}' || true)"
        if [[ -z "$retry_delay_s" ]]; then
            retry_delay_s="0.1"
        fi
        sleep "$retry_delay_s"
        attempt=$((attempt + 1))
    done
}

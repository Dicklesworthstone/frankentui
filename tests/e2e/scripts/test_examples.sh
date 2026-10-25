#!/bin/bash
# Run through DSR on a native host with prebuilt binaries. Retain every capture.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/common.sh"
source "$SCRIPT_DIR/../lib/logging.sh"
source "$SCRIPT_DIR/../lib/pty.sh"

require_tools "$E2E_PYTHON" jq || exit 2
if [[ ! -x "${E2E_MINIMAL_BIN:-}" || ! -x "${PTY_CANONICALIZE_BIN:-}" ]]; then
    echo "Set E2E_MINIMAL_BIN and PTY_CANONICALIZE_BIN to DSR-built executables" >&2
    exit 2
fi
mkdir -p "$E2E_LOG_DIR"
CASE_ROOT="$(mktemp -d "$E2E_LOG_DIR/minimal.XXXXXXXX")"

run_example() (
    local name="$1" duration="$2" key="$3"
    local capture="$CASE_ROOT/$name.pty" screen="$CASE_ROOT/$name.screen.txt"
    LOG_FILE="$CASE_ROOT/$name.log"
    # Clear inherited experiment, capture and terminal-profile controls.
    while IFS= read -r variable; do
        case "$variable" in
            FTUI_*|PTY_*|TMUX*|STY|ZELLIJ*|WEZTERM*|NO_COLOR|KITTY_WINDOW_ID|WT_SESSION|TERM_PROGRAM_VERSION|LC_TERMINAL*)
                case "$variable" in PTY_CANONICALIZE_BIN) ;; *) unset "$variable" ;; esac
                ;;
        esac
    done < <(compgen -e)
    export TERM=xterm-kitty TERM_PROGRAM=kitty COLORTERM=truecolor
    export FTUI_SYNC_OUTPUT=1 FTUI_SCROLL_REGION=1 FTUI_CAPS_PROBE=0
    export PTY_TIMEOUT=5 PTY_RETRIES=1 PTY_COLS=80 PTY_ROWS=24 PTY_CANONICALIZE=0
    export PTY_TIMING_FILE="$capture.timing.json"
    export PTY_SEND="$key" PTY_SEND_DELAY_MS=550
    if [[ -n "$key" ]]; then
        export PTY_SEND_AFTER_OUTPUT="Hello from FrankenTUI ticks:"
    fi
    [[ "$duration" == unset ]] || export FTUI_HARNESS_EXIT_AFTER_MS="$duration"
    local start end elapsed status=0
    start="$(e2e_monotonic_ms)" || return 2
    pty_run "$capture" "$E2E_MINIMAL_BIN" > "$LOG_FILE" 2>&1 || status=$?
    end="$(e2e_monotonic_ms)" || return 2
    elapsed=$((end - start))
    if [[ "$status" == 0 ]]; then
        "$PTY_CANONICALIZE_BIN" --input "$capture" --output "$screen" --cols 80 --rows 24 >> "$LOG_FILE" 2>&1 || status=$?
    fi
    if [[ "$status" == 0 ]]; then
        "$E2E_PYTHON" - "$capture" "$screen" "$name" "$elapsed" "$E2E_MINIMAL_BIN" <<'PY' >> "$LOG_FILE" 2>&1 || status=$?
import hashlib
import json
from pathlib import Path
import re
import sys

capture, screen, name, elapsed, binary = sys.argv[1:]
raw = Path(capture).read_bytes()
timing = json.loads(Path(capture + '.timing.json').read_text())
child_ms = timing['spawn_to_exit_ms']
lines = Path(screen).read_text().splitlines()
cursor = re.findall(rb'\x1b\[\?25[hl]', raw)
sync = re.findall(rb'\x1b\[\?2026([hl])', raw)
ticks = [int(n) for n in re.findall(r'Hello from FrankenTUI ticks: (\d+)', '\n'.join(lines))]
bordered = any(len(top) == len(middle) == len(bottom) == 80
               and top.startswith('┌minimal') and top.endswith('┐')
               and middle.startswith('│Hello from FrankenTUI ticks:') and middle.endswith('│')
               and bottom.startswith('└') and bottom.endswith('┘')
               for top, middle, bottom in zip(lines, lines[1:], lines[2:]))
counts = dict(cursor_hide=raw.count(b'\x1b[?25l'), cursor_show=raw.count(b'\x1b[?25h'),
              alt_1049h=raw.count(b'\x1b[?1049h'), sync_h=sync.count(b'h'), sync_l=sync.count(b'l'),
              decstbm_set=len(re.findall(rb'\x1b\[\d+;\d+r', raw)), decstbm_reset=raw.count(b'\x1b[r'))
# Each frame opens and closes its own synchronized-output block (?2026h/?2026l).
checks = dict(duration=child_ms is not None and (950 if name == 'auto_exit' else 500) <= child_ms < 2000
                       and not timing['timed_out'],
              input=timing['input_chunks_sent'] == timing['input_chunks_expected']
                    and timing['input_while_alive']
                    and (name == 'auto_exit' or timing['input_chunks_sent'] > 0),
              visible=bordered, ticked=bool(ticks) and ticks[-1] > 0
                                     and b'Hello from FrankenTUI ticks: 0' in raw,
              cursor=bool(cursor) and cursor[0] == b'\x1b[?25l' and cursor[-1] == b'\x1b[?25h',
              inline=counts['alt_1049h'] == 0,
              sync=counts['sync_h'] > 0 and sync == [b'h', b'l'] * counts['sync_h'],
              scroll=counts['decstbm_set'] > 0 and counts['decstbm_reset'] == 2
                     and raw.rfind(b'\x1b7\x1b[r\x1b8') > raw.rfind(b'\x1b[?2026h'))
record = dict(scenario='minimal_example_' + name, identity='kitty', exit_code=0,
              duration_ms=child_ms, driver_duration_ms=int(elapsed), timing=timing,
              esc_tallies=counts, checks=checks,
              binary_sha256=hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
              capture_sha256=hashlib.sha256(raw).hexdigest(),
              text_found=bordered, status='passed' if all(checks.values()) else 'failed')
with Path(capture + '.json').open('x') as output:
    json.dump(record, output, indent=2)
print(json.dumps(record))
sys.exit(0 if all(checks.values()) else 1)
PY
    fi
    if [[ "$status" == 0 ]]; then
        log_test_pass "minimal_example_$name"
        record_result "minimal_example_$name" passed "$elapsed" "$LOG_FILE"
    else
        log_test_fail "minimal_example_$name" "PTY, parser or rendering assertion failed (exit $status)"
        record_result "minimal_example_$name" failed "$elapsed" "$LOG_FILE" "exit $status"
        return 1
    fi
)

failures=0
run_example auto_exit 800 '' || failures=$((failures + 1))
run_example quit_key unset q || failures=$((failures + 1))
run_example ctrl_c unset '\x03' || failures=$((failures + 1))
run_example escape unset '\x1b' || failures=$((failures + 1))
run_example zero 0 q || failures=$((failures + 1))
run_example invalid invalid q || failures=$((failures + 1))
run_example maximum 18446744073709551615 q || failures=$((failures + 1))
[[ "$failures" == 0 ]]

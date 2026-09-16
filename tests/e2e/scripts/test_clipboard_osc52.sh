#!/bin/bash
# Real showcase PTY clipboard wire, rendered-screen, and restoration checks.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/common.sh"
source "$SCRIPT_DIR/../lib/logging.sh"
source "$SCRIPT_DIR/../lib/pty.sh"

# Build orchestration owns compilation; missing prerequisites must fail.
DEMO_BIN="${FTUI_DEMO_BIN:-${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}/debug/ftui-demo-showcase}"
CANON_BIN="${PTY_CANONICALIZE_BIN:-${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}/debug/pty_canonicalize}"
[[ -x "$DEMO_BIN" && -x "$CANON_BIN" ]] || { echo 'Build showcase and pty_canonicalize first' >&2; exit 2; }
require_tools jq || exit 2
SCREEN="$("$DEMO_BIN" --list-screens | jq -er 'index("advanced_text_editor") | if . == null then error("editor screen missing") else . + 1 end')"
JSONL="$E2E_LOG_DIR/clipboard_osc52_e2e.jsonl"
[[ ! -e "$JSONL" ]] || { echo 'Use a fresh E2E_LOG_DIR to retain prior evidence' >&2; exit 2; }
export PTY_COLS=120 PTY_ROWS=40 PTY_CANONICALIZE=0
export FTUI_DEMO_EXIT_AFTER_MS=0 FTUI_TEXTEDITOR_DIAGNOSTICS=false
export FTUI_TEXTEDITOR_DETERMINISTIC=true FTUI_CAPS_PROBE=0
unset TMUX STY ZELLIJ ZELLIJ_SESSION_NAME FTUI_OSC52_CLIPBOARD
export TERM=xterm-kitty TERM_PROGRAM=kitty

run_clipboard_case() {
    local scenario="$1" sequence exit_code=0 start_ms duration_ms
    local capture="$E2E_LOG_DIR/$scenario.pty"
    LOG_FILE="$E2E_LOG_DIR/$scenario.log"
    log_test_start "$scenario"
    unset TMUX FTUI_OSC52_CLIPBOARD
    export TERM=xterm-kitty
    case "$scenario" in
        osc52_tmux_passthrough) export TMUX=/tmp/fake,1,0 TERM=screen-256color FTUI_OSC52_CLIPBOARD=1 ;;
        osc52_tmux_default_off) export TMUX=/tmp/fake,1,0 TERM=screen-256color ;;
        osc52_explicit_off) export FTUI_OSC52_CLIPBOARD=0 ;;
    esac
    sequence="$("$E2E_PYTHON" - "$scenario" <<'PY'
import json,sys
if sys.argv[1] == 'osc52_query_on_paste':
    steps = [(300,'\x01'), (600,'\x7f'), (900,'\x1b'), (1200,'p'),
             (1600,'\x1b]52;c;aGVsbG8=\x07'), (2400,'q')]
else:
    text = 'a' * 60000 if sys.argv[1] == 'osc52_payload_cap' else 'hello'
    steps = [(300,'\x01'), (600,'\x1b[200~'+text+'\x1b[201~'),
             (1000,'\x01'), (1300,'\x1b'), (1700,'y'), (2400,'q')]
print(json.dumps([{'delay_ms':delay, 'text':text} for delay,text in steps]))
PY
)" || return 1
    start_ms="$(e2e_monotonic_ms)"
    PTY_SEND_SEQUENCE="$sequence" PTY_SEND_AFTER_OUTPUT='Advanced Text Editor' \
    PTY_TIMING_FILE="$capture.timing.json" PTY_TIMEOUT=8 \
        pty_run "$capture" "$DEMO_BIN" --screen="$SCREEN" || exit_code=$?
    duration_ms=$(( $(e2e_monotonic_ms) - start_ms ))

    # Replay the actual application screen before alt-screen restoration.
    # Keep both the complete wire capture and the exact replay prefix.
    "$E2E_PYTHON" - "$capture" <<'PY'
import pathlib,sys
p=pathlib.Path(sys.argv[1]); wire=p.read_bytes()
leave=wire.find(b'\x1b[?1049l')
with pathlib.Path(str(p)+'.screen.pty').open('xb') as f:
    f.write(wire if leave < 0 else wire[:leave])
PY
    "$CANON_BIN" --input "$capture.screen.pty" --output "$capture.screen.txt" --cols 120 --rows 40 || return 1
    "$E2E_PYTHON" - "$scenario" "$capture" "$JSONL" "$exit_code" "$duration_ms" <<'PY'
import hashlib,json,os,pathlib,sys
scenario,capture,jsonl,exit_code,duration=sys.argv[1:]
p=pathlib.Path(capture); wire=p.read_bytes()
screen=pathlib.Path(capture+'.screen.txt').read_text()
timing=json.loads(pathlib.Path(capture+'.timing.json').read_text())
copy=b'\x1b]52;c;aGVsbG8=\x07'; query=b'\x1b]52;c;?\x07'
wrapped=b'\x1bPtmux;\x1b'+copy+b'\x1b\\'
wrapped_count=wire.count(wrapped)
bare=wire.replace(wrapped,b'')
set_count=bare.count(copy); query_count=wire.count(query)
alt_in=wire.count(b'\x1b[?1049h'); alt_out=wire.count(b'\x1b[?1049l')
sync_in=wire.count(b'\x1b[?2026h'); sync_out=wire.count(b'\x1b[?2026l')
errors=[]
def check(ok,message):
    if not ok: errors.append(message)
check(int(exit_code)==0, 'normal q exit must succeed')
check(not timing['timed_out'] and timing['input_while_alive'] and
      timing['input_chunks_sent']==timing['input_chunks_expected']==6, 'all input must reach a live process')
check(alt_in>0 and alt_in==alt_out, 'unbalanced alternate screen')
check(sync_in==sync_out, 'unbalanced synchronized output')
check(wire.rfind(b'\x1b[?25h')>wire.rfind(b'\x1b[?25l'), 'cursor not restored')
if scenario=='osc52_query_on_paste':
    check(query_count==1 and set_count==wrapped_count==0, 'expected exactly one query')
    check('hello' in screen and 'Pasted 5 chars' in screen, 'reply not rendered')
    check(wire.find(query)>=0 and wire.find(query)<wire.find(b'Pasted 5 chars'), 'query must precede paste rendering')
elif scenario=='osc52_payload_cap':
    check(b'\x1b]52;' not in wire, 'oversized payload emitted OSC52')
    check('Clipboard payload too large' in screen, 'missing rejection status')
elif scenario in ('osc52_tmux_default_off','osc52_explicit_off'):
    check(b'\x1b]52;' not in wire, 'disabled clipboard emitted OSC52')
    check('hello' in screen, 'editor content missing')
else:
    check('Copied 5 chars' in screen and 'hello' in screen, 'copy state not rendered')
    check(query_count==0, 'copy emitted query')
    check((wrapped_count,set_count)==((1,0) if scenario=='osc52_tmux_passthrough' else (0,1)), 'incorrect copy envelope/count')
position=wire.find(wrapped if wrapped_count else query if query_count else copy)
inside_sync=None if position<0 else wire[:position].rfind(b'\x1b[?2026h')>wire[:position].rfind(b'\x1b[?2026l')
record={'schema_version':'e2e-jsonl-v1','type':'case','timestamp':'T000001',
        'run_id':os.environ.get('E2E_RUN_ID','clipboard_osc52'), 'seed':0,
        'scenario':scenario,'mode':'altscreen','cols':120,'rows':40,
        'status':'failed' if errors else 'passed','hash':hashlib.sha256(screen.encode()).hexdigest(),
        'duration_ms':int(duration),'error':'; '.join(errors),'screen':capture+'.screen.txt',
        'extra':{'term':os.environ['TERM'],'term_program':os.environ['TERM_PROGRAM'],
                 'tmux_present':'TMUX' in os.environ,'osc52_policy':os.environ.get('FTUI_OSC52_CLIPBOARD'),
                 'set_count':set_count,'query_count':query_count,'wrapped_count':wrapped_count,
                 'reply_injected':scenario=='osc52_query_on_paste' and timing['input_chunks_sent']==6,
                 'payload_b64_len':80000 if scenario=='osc52_payload_cap' else 8,
                 'inside_sync':inside_sync,'alt_enter':alt_in,'alt_leave':alt_out,
                 'sync_begin':sync_in,'sync_end':sync_out,'exit_code':int(exit_code)}}
with pathlib.Path(jsonl).open('a') as f: f.write(json.dumps(record)+'\n')
if errors: print('; '.join(errors),file=sys.stderr)
sys.exit(bool(errors))
PY
}

failures=0
for scenario in osc52_set_on_yank osc52_query_on_paste osc52_tmux_passthrough \
                osc52_payload_cap osc52_tmux_default_off osc52_explicit_off; do
    if run_clipboard_case "$scenario"; then
        log_test_pass "$scenario"
    else
        log_test_fail "$scenario" 'clipboard assertions failed'
        failures=$((failures + 1))
    fi
done
"$E2E_PYTHON" "$SCRIPT_DIR/../lib/validate_jsonl.py" --strict "$JSONL" || failures=$((failures + 1))
echo "Clipboard OSC52: 6 cases, $failures failures"
exit "$failures"

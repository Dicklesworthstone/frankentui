#!/bin/bash
set -euo pipefail

# ─────────────────────────────────────────────────────────────────────────────
# E2E Tests: recognized gestures under a real PTY (bd-g00-root-epic-ewths.24.3)
#
# Drives the showcase's Mouse Playground with SGR mouse sequences on a timer
# and asserts the gestures ftui_core::gesture::GestureRecognizer recognized,
# as the screen's event log draws them: DoubleClick, TripleClick, DragStart
# (only past the 3-cell dead zone) and DragEnd. Unit tests cover the same
# recognizer on synthetic instants; this covers wall-clock timing, the SGR
# parser and the screen's wiring.
#
# Assertions read the canonicalized final screen, not the raw byte stream:
# the diff renderer redraws only changed cells, so a word in the raw capture
# can be split by cursor moves.
#
# Each case writes one `gesture_case` JSONL event (schema:
# tests/e2e/lib/e2e_jsonl_schema.json); the run validates them strictly.
# ─────────────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

E2E_SUITE_SCRIPT="$SCRIPT_DIR/test_gestures.sh"
export E2E_SUITE_SCRIPT
export PTY_CANONICALIZE=1
# The showcase runs on the alternate screen and leaves it on exit, which
# would leave only the primary screen to canonicalize. Treating the switch as
# a no-op keeps the last frame the playground drew.
export PTY_CANONICALIZE_ARGS="--quirk windows_no_alt_screen"
ONLY_CASE="${E2E_ONLY_CASE:-}"
PY="${E2E_PYTHON:-python3}"
GESTURES_JSONL="$E2E_LOG_DIR/gestures.jsonl"
RUN_ID="gestures-$(e2e_monotonic_ms)-$$"

ALL_CASES=(
    gesture_double_click
    gesture_triple_click
    gesture_drag_dead_zone
    gesture_mouse_modes_balanced
    hover_jitter_stable
    hover_intentional_switch
)

# run_all.sh exports FTUI_DEMO_BIN; standalone runs usually set E2E_DEMO_BIN.
DEMO_BIN="${E2E_DEMO_BIN:-${FTUI_DEMO_BIN:-}}"
if [[ ! -x "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/gestures_missing.log"
    for t in "${ALL_CASES[@]}"; do
        log_test_skip "$t" "ftui-demo-showcase binary missing (E2E_DEMO_BIN / FTUI_DEMO_BIN)"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

PLAYGROUND_SCREEN="$(e2e_demo_screen "$DEMO_BIN" mouse_playground)"
# Only this screen draws its own title in the status bar.
SCREEN_TITLE="Mouse Playground"

# SGR mouse (1-based cells): ESC[<b;x;yM press/motion, ...m release.
# b: 0 = left, 32 = left held while moving.
sgr() { printf '\x1b[<%s;%s;%s%s' "$1" "$2" "$3" "$4"; }

emit_gesture_case() {
    local scenario="$1" expected="$2" sends="$3" exit_code="$4" duration_ms="$5" status="$6" detail="$7"
    mkdir -p "$(dirname "$GESTURES_JSONL")"
    "$PY" - "$GESTURES_JSONL" "$E2E_JSONL_SCHEMA_VERSION" "$RUN_ID" "$scenario" "$expected" \
        "$sends" "$exit_code" "$duration_ms" "$status" "$detail" "$(e2e_timestamp)" <<'PY'
import json, sys
path, schema, run_id, scenario, expected, sends, code, ms, status, detail, ts = sys.argv[1:]
row = {
    "schema_version": schema, "type": "gesture_case", "timestamp": ts, "run_id": run_id,
    "seed": None, "scenario": scenario, "expected": expected, "sends": int(sends),
    "exit_code": int(code), "duration_ms": int(ms), "status": status, "detail": detail,
}
with open(path, "a", encoding="utf-8") as fh:
    fh.write(json.dumps(row) + "\n")
PY
}

# Run the playground with a timed input sequence; sets CANONICAL to the final
# screen text. $1 case name, $2 PTY_SEND_SEQUENCE JSON.
run_playground() {
    local name="$1" sequence="$2" exit_ms="${3:-3500}"
    local output_file="$E2E_LOG_DIR/${name}.pty"
    PTY_TEST_NAME="$name"
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_AFTER_OUTPUT="$SCREEN_TITLE" \
    PTY_SEND_SEQUENCE="$sequence" \
    FTUI_DEMO_SCREEN="$PLAYGROUND_SCREEN" \
    FTUI_DEMO_MOUSE=on \
    FTUI_DEMO_EXIT_AFTER_MS="$exit_ms" \
    PTY_TIMEOUT=20 \
        pty_run "$output_file" "$DEMO_BIN"
    RAW_OUTPUT="$output_file"
    CANONICAL="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "$SCREEN_TITLE" "$CANONICAL"
}

# Build a PTY_SEND_SEQUENCE from alternating (gap_ms, text) arguments.
# PTY_SEND_SEQUENCE takes delays measured from the start, so the gaps are
# accumulated.
sequence() {
    "$PY" - "$@" <<'PY'
import itertools, json, sys
args = sys.argv[1:]
gaps, texts = [int(g) for g in args[::2]], args[1::2]
print(json.dumps([{"delay_ms": d, "text": t} for d, t in zip(itertools.accumulate(gaps), texts)]))
PY
}

run_case() {
    local name="$1" expected="$2" sends="$3"
    shift 3
    LOG_FILE="$E2E_LOG_DIR/${name}.log"
    if [[ -n "$ONLY_CASE" && "$ONLY_CASE" != "$name" ]]; then
        log_test_skip "$name" "filtered (E2E_ONLY_CASE=$ONLY_CASE)"
        record_result "$name" "skipped" 0 "$LOG_FILE" "filtered"
        return 0
    fi
    log_test_start "$name"
    local start_ms end_ms
    start_ms="$(e2e_monotonic_ms)"
    CASE_DETAIL=""
    if "$@"; then
        end_ms="$(e2e_monotonic_ms)"
        log_test_pass "$name"
        record_result "$name" "passed" $((end_ms - start_ms)) "$LOG_FILE"
        emit_gesture_case "$name" "$expected" "$sends" 0 $((end_ms - start_ms)) passed "${CASE_DETAIL:-ok}"
        return 0
    fi
    end_ms="$(e2e_monotonic_ms)"
    log_test_fail "$name" "${CASE_DETAIL:-assertion failed}"
    record_result "$name" "failed" $((end_ms - start_ms)) "$LOG_FILE" "${CASE_DETAIL:-assertion failed}"
    emit_gesture_case "$name" "$expected" "$sends" 1 $((end_ms - start_ms)) failed "${CASE_DETAIL:-assertion failed}"
    return 1
}

# The event log lists the newest entry first; it keeps the last 12.
CLICK_X=60
CLICK_Y=20

gesture_double_click() {
    local down up
    down="$(sgr 0 $CLICK_X $CLICK_Y M)"
    up="$(sgr 0 $CLICK_X $CLICK_Y m)"
    run_playground gesture_double_click "$(sequence 300 "$down$up" 120 "$down$up")" || {
        CASE_DETAIL="playground never drew"; return 1; }
    grep -a -q "DoubleClick" "$CANONICAL" || { CASE_DETAIL="no DoubleClick in the event log"; return 1; }
    ! grep -a -q "TripleClick" "$CANONICAL" || { CASE_DETAIL="two clicks read as three"; return 1; }
}

gesture_triple_click() {
    local down up
    down="$(sgr 0 $CLICK_X $CLICK_Y M)"
    up="$(sgr 0 $CLICK_X $CLICK_Y m)"
    run_playground gesture_triple_click \
        "$(sequence 300 "$down$up" 100 "$down$up" 100 "$down$up")" || {
        CASE_DETAIL="playground never drew"; return 1; }
    grep -a -q "TripleClick" "$CANONICAL" || { CASE_DETAIL="no TripleClick in the event log"; return 1; }
}

gesture_drag_dead_zone() {
    # Press at x=40, move one cell at a time. Manhattan distance 3 (x=43) is
    # the default threshold, so DragStart must be logged right after the third
    # move, never after the first two.
    run_playground gesture_drag_dead_zone "$(sequence \
        300 "$(sgr 0 40 $CLICK_Y M)" \
        60 "$(sgr 32 41 $CLICK_Y M)" \
        60 "$(sgr 32 42 $CLICK_Y M)" \
        60 "$(sgr 32 43 $CLICK_Y M)" \
        60 "$(sgr 0 43 $CLICK_Y m)")" || {
        CASE_DETAIL="playground never drew"; return 1; }
    local start_line crossing_line
    start_line="$(grep -a -n "DragStart" "$CANONICAL" | head -1 | cut -d: -f1)"
    [[ -n "$start_line" ]] || { CASE_DETAIL="no DragStart"; return 1; }
    grep -a -q "DragEnd" "$CANONICAL" || { CASE_DETAIL="no DragEnd"; return 1; }
    # Newest first: the move that crossed the dead zone (0-based x=42) is the
    # line directly below DragStart.
    crossing_line="$(grep -a -n "Left Drag" "$CANONICAL" | grep -a "( 42," | head -1 | cut -d: -f1)"
    [[ "$crossing_line" == $((start_line + 1)) ]] || {
        CASE_DETAIL="DragStart (line $start_line) not logged right after the x=42 move (line ${crossing_line:-none})"
        return 1
    }
    ! grep -a -q " Click " "$CANONICAL" || { CASE_DETAIL="a drag was also read as a click"; return 1; }
}

# Hover stabilization (ftui_core::hover_stabilizer). At 120x40 the grid's
# first target T1 spans 1-based x=3..18 on row y=10, x=19..20 is the gap and
# T2 starts at x=21. The playground's stats line reads "Hover: <T|None>  Pos:
# (x, y)" with 0-based positions.
#
# The moves are spaced, as a hand makes them: the tty backend merges
# consecutive Moved events already in its queue (push_event_coalescing), so a
# burst reaches the app as its last position only. Here that would be a
# 7-cell jump out of T1, which the stabilizer rightly treats as an exit.
HOVER_Y=10
moves() {
    local args=() gap=300 x
    for x in "$@"; do
        args+=("$gap" "$(sgr 35 "$x" "$HOVER_Y" M)")
        gap=120
    done
    sequence "${args[@]}"
}

hover_jitter_stable() {
    # Into T1, walk to its right edge, then jitter between the edge (x=18)
    # and the gap (x=19), ending in the gap. Raw hit-testing says None there;
    # the stabilizer holds T1 because each gap sample is one cell from where
    # T1 was last seen, inside the hysteresis band.
    run_playground hover_jitter_stable "$(moves 12 15 17 18 19 18 19 18 19)" 5000 || {
        CASE_DETAIL="playground never drew"; return 1; }
    grep -a -q "Hover: T1  Pos: (18, 9)" "$CANONICAL" || {
        CASE_DETAIL="not held on T1 at the gap: $(grep -a -o "Hover: [^ ]*  Pos: ([0-9, ]*)" "$CANONICAL")"
        return 1
    }
}

hover_intentional_switch() {
    # From T1 straight into T2: well past the hysteresis band, so the switch
    # is immediate.
    run_playground hover_intentional_switch "$(moves 12 16 22 23)" 4000 || {
        CASE_DETAIL="playground never drew"; return 1; }
    grep -a -q "Hover: T2  Pos: (22, 9)" "$CANONICAL" || {
        CASE_DETAIL="did not switch to T2: $(grep -a -o "Hover: [^ ]*  Pos: ([0-9, ]*)" "$CANONICAL")"
        return 1
    }
}

gesture_mouse_modes_balanced() {
    run_playground gesture_mouse_modes_balanced "$(sequence 300 "$(sgr 0 $CLICK_X $CLICK_Y M)$(sgr 0 $CLICK_X $CLICK_Y m)")" || {
        CASE_DETAIL="playground never drew"; return 1; }
    # Every mouse mode the session turned on is turned off again on exit.
    local mode on off
    for mode in 1000 1002 1006; do
        on="$(grep -a -o -F "$(printf '\x1b[?%sh' "$mode")" "$RAW_OUTPUT" | wc -l | tr -d ' ')"
        off="$(grep -a -o -F "$(printf '\x1b[?%sl' "$mode")" "$RAW_OUTPUT" | wc -l | tr -d ' ')"
        [[ "$on" -ge 1 && "$off" -ge 1 ]] || {
            CASE_DETAIL="mode ?$mode enabled $on times, disabled $off times"; return 1; }
    done
}

FAILED=0
run_case gesture_double_click DoubleClick 2 gesture_double_click || FAILED=1
run_case gesture_triple_click TripleClick 3 gesture_triple_click || FAILED=1
run_case gesture_drag_dead_zone DragStart+DragEnd 5 gesture_drag_dead_zone || FAILED=1
run_case gesture_mouse_modes_balanced mouse-modes-balanced 1 gesture_mouse_modes_balanced || FAILED=1
run_case hover_jitter_stable hover-held-T1 9 hover_jitter_stable || FAILED=1
run_case hover_intentional_switch hover-T2 4 hover_intentional_switch || FAILED=1

if [[ -s "$GESTURES_JSONL" ]]; then
    "$PY" "$LIB_DIR/validate_jsonl.py" "$GESTURES_JSONL" --strict || FAILED=1
fi
exit "$FAILED"

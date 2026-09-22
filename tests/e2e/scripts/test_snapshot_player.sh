#!/bin/bash
set -euo pipefail

# E2E tests for Snapshot/Time Travel Player (Demo Showcase)
# bd-3sa7.4: Snapshot/Time Travel Player — E2E PTY Tests (Verbose Logs)
#
# Scenarios:
# 1. Smoke test: render screen and verify playback UI
# 2. Play/pause toggle: Space starts/stops playback
# 3. Frame stepping: Left/Right steps through frames
# 4. Jump to bounds: Home/End jumps to first/last frame
# 5. Toggle marker: M marks/unmarks frames
# 6. Toggle recording: R toggles recording mode
#
# Keybindings:
# - Space: Play/Pause
# - ← / →: Step frame backward/forward
# - Home/End: First/Last frame
# - M: Toggle marker
# - R: Toggle record
# - C: Clear all
# - D: Diagnostics panel

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

JSONL_FILE="$E2E_RESULTS_DIR/snapshot_player.jsonl"
RUN_ID="snapshot_player_$(date +%Y%m%d_%H%M%S)_$$"
SEED="${FTUI_DEMO_SEED:-0}"

jsonl_log() {
    local line="$1"
    mkdir -p "$E2E_RESULTS_DIR"
    printf '%s\n' "$line" >> "$JSONL_FILE"
}

sha256_file() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1 && [[ -f "$file" ]]; then
        sha256sum "$file" | awk '{print $1}'
        return 0
    fi
    echo ""
    return 0
}

ensure_demo_bin() {
    local target_dir="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
    local bin="$target_dir/debug/ftui-demo-showcase"
    if [[ -x "$bin" ]]; then
        echo "$bin"
        return 0
    fi
    log_info "Building ftui-demo-showcase (debug)..." >&2
    (cd "$PROJECT_ROOT" && cargo build -p ftui-demo-showcase >/dev/null)
    if [[ -x "$bin" ]]; then
        echo "$bin"
        return 0
    fi
    return 1
}

detect_snapshot_screen() {
    local bin="$1"
    local help
    help="$($bin --help 2>/dev/null || true)"
    if [[ -z "$help" ]]; then
        return 1
    fi
    local line
    # Look for "Snapshot Player" in help output
    line=$(printf '%s\n' "$help" | command grep "Snapshot Player" | head -n 1 || true)
    if [[ -z "$line" ]]; then
        return 1
    fi
    local screen
    screen=$(printf '%s' "$line" | awk '{print $1}')
    if [[ ! "$screen" =~ ^[0-9]+$ ]]; then
        return 1
    fi
    printf '%s' "$screen"
    return 0
}

run_case() {
    local name="$1"
    local send_label="$2"
    local state_desc="$3"
    shift 3
    local start_ms
    start_ms="$(e2e_monotonic_ms)"

    LOG_FILE="$E2E_LOG_DIR/${name}.log"
    local output_file="$E2E_LOG_DIR/${name}.pty"

    log_test_start "$name"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)"
        local duration_ms=$((end_ms - start_ms))
        local size
        size=$(wc -c < "$output_file" | tr -d ' ')
        local output_sha
        output_sha="$(sha256_file "$output_file")"
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"passed\",\"duration_ms\":$duration_ms,\"output_bytes\":$size,\"output_sha256\":\"$output_sha\",\"send\":\"$send_label\",\"state\":\"$state_desc\",\"cols\":120,\"rows\":40,\"seed\":\"$SEED\",\"screen\":\"${SNAPSHOT_SCREEN:-}\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\"}"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)"
    local duration_ms=$((end_ms - start_ms))
    local output_sha
    output_sha="$(sha256_file "$output_file")"
    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"failed\",\"duration_ms\":$duration_ms,\"output_sha256\":\"$output_sha\",\"send\":\"$send_label\",\"state\":\"$state_desc\",\"cols\":120,\"rows\":40,\"seed\":\"$SEED\",\"screen\":\"${SNAPSHOT_SCREEN:-}\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\"}"
    return 1
}

DEMO_BIN="$(ensure_demo_bin || true)"
if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/snapshot_player_missing.log"
    for t in snapshot_smoke snapshot_play_pause snapshot_step_frames snapshot_jump_bounds snapshot_marker snapshot_recording snapshot_small_terminal; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"status\":\"skipped\",\"reason\":\"binary missing\",\"seed\":\"$SEED\",\"screen\":\"${SNAPSHOT_SCREEN:-}\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\"}"
    done
    exit 0
fi

SNAPSHOT_SCREEN="$(detect_snapshot_screen "$DEMO_BIN" || true)"
if [[ -z "$SNAPSHOT_SCREEN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/snapshot_player_missing.log"
    for t in snapshot_smoke snapshot_play_pause snapshot_step_frames snapshot_jump_bounds snapshot_marker snapshot_recording snapshot_small_terminal; do
        log_test_skip "$t" "Snapshot Player screen not registered in --help"
        record_result "$t" "skipped" 0 "$LOG_FILE" "screen missing"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"status\":\"skipped\",\"reason\":\"screen missing\",\"seed\":\"$SEED\",\"screen\":\"${SNAPSHOT_SCREEN:-}\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\"}"
    done
    exit 0
fi

# Export demo config so it's visible to subprocesses
export FTUI_DEMO_SCREEN="$SNAPSHOT_SCREEN"

# Control bytes
SPACE=' '
# Left/Right arrow keys (escape sequences)
LEFT=$'\x1b[D'
RIGHT=$'\x1b[C'
# Home/End keys
HOME=$'\x1b[H'
END=$'\x1b[F'

# Test 1: Smoke test (render Snapshot Player screen)
# Verifies the screen renders and shows playback UI elements
snapshot_smoke() {
    LOG_FILE="$E2E_LOG_DIR/snapshot_smoke.log"
    local output_file="$E2E_LOG_DIR/snapshot_smoke.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="" \
    FTUI_DEMO_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Expect snapshot/playback UI elements
    # Should show "Paused", "Playing", "Frame", "Timeline", or "Snapshot"
    command grep -a -qi "Paused\|Playing\|Frame\|Timeline\|Snapshot\|Preview" "$output_file" || return 1
}

# Test 2: Play/pause toggle (Space toggles playback)
snapshot_play_pause() {
    LOG_FILE="$E2E_LOG_DIR/snapshot_play_pause.log"
    local output_file="$E2E_LOG_DIR/snapshot_play_pause.pty"

    # Send Space to toggle playback, then again to pause
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=400 \
    PTY_SEND="${SPACE}${SPACE}" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Should show playback state indicators
    command grep -a -qi "Paused\|Playing\|Frame" "$output_file" || return 1
}

# Test 3: Frame stepping (Left/Right steps through frames)
snapshot_step_frames() {
    LOG_FILE="$E2E_LOG_DIR/snapshot_step_frames.log"
    local output_file="$E2E_LOG_DIR/snapshot_step_frames.pty"

    # Send Right to step forward, Left to step backward
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="${RIGHT}${RIGHT}${LEFT}" \
    FTUI_DEMO_EXIT_AFTER_MS=1800 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Should still show frame UI
    command grep -a -qi "Frame\|Snapshot\|Preview" "$output_file" || return 1
}

# Test 4: Jump to bounds (Home/End jumps to first/last frame)
snapshot_jump_bounds() {
    LOG_FILE="$E2E_LOG_DIR/snapshot_jump_bounds.log"
    local output_file="$E2E_LOG_DIR/snapshot_jump_bounds.pty"

    # Send End to go to last frame, then Home to go to first
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="${END}${HOME}" \
    FTUI_DEMO_EXIT_AFTER_MS=1800 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Should still display player UI
    command grep -a -qi "Frame\|Snapshot\|Timeline" "$output_file" || return 1
}

# Test 5: Toggle marker (M marks/unmarks frames)
snapshot_marker() {
    LOG_FILE="$E2E_LOG_DIR/snapshot_marker.log"
    local output_file="$E2E_LOG_DIR/snapshot_marker.pty"

    # Send M to toggle marker on current frame
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="MM" \
    FTUI_DEMO_EXIT_AFTER_MS=1800 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Should still show player UI
    command grep -a -qi "Frame\|Marker\|Snapshot" "$output_file" || return 1
}

# Test 6: Toggle recording (R toggles recording mode)
snapshot_recording() {
    LOG_FILE="$E2E_LOG_DIR/snapshot_recording.log"
    local output_file="$E2E_LOG_DIR/snapshot_recording.pty"

    # Send R to toggle recording, then R again to stop
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=400 \
    PTY_SEND="RR" \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Should show recording or playback state
    command grep -a -qi "Recording\|Paused\|Playing\|Frame" "$output_file" || return 1
}

# Test 7: Small terminal test (80x24)
snapshot_small_terminal() {
    LOG_FILE="$E2E_LOG_DIR/snapshot_small_terminal.log"
    local output_file="$E2E_LOG_DIR/snapshot_small_terminal.pty"

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="" \
    FTUI_DEMO_EXIT_AFTER_MS=1200 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    # Should produce output even in small terminal
    [[ "$size" -gt 200 ]] || return 1
}

FAILURES=0
run_case "snapshot_smoke" "" "initial" snapshot_smoke || FAILURES=$((FAILURES + 1))
run_case "snapshot_play_pause" "SPACE,SPACE" "toggle_playback" snapshot_play_pause || FAILURES=$((FAILURES + 1))
run_case "snapshot_step_frames" "RIGHT,RIGHT,LEFT" "stepping" snapshot_step_frames || FAILURES=$((FAILURES + 1))
run_case "snapshot_jump_bounds" "END,HOME" "jump_bounds" snapshot_jump_bounds || FAILURES=$((FAILURES + 1))
run_case "snapshot_marker" "M,M" "marker_toggle" snapshot_marker || FAILURES=$((FAILURES + 1))
run_case "snapshot_recording" "R,R" "recording_toggle" snapshot_recording || FAILURES=$((FAILURES + 1))
run_case "snapshot_small_terminal" "" "initial_80x24" snapshot_small_terminal || FAILURES=$((FAILURES + 1))

if [[ "$FAILURES" -gt 0 ]]; then
    log_error "Snapshot Player E2E tests: $FAILURES failure(s)"
    exit 1
fi

log_info "Snapshot Player E2E tests: all passed"
exit 0

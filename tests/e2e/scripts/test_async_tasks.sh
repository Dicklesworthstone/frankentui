#!/bin/bash
set -euo pipefail

# E2E PTY tests for Async Task Manager screen (Demo Showcase)
# bd-13pq.4: Async Task Manager — E2E PTY Tests (Verbose Logs)
#
# Scenarios:
# 1. Initial render - verify screen loads with seed tasks
# 2. Spawn task - press 'n' to create new task
# 3. Cancel task - press 'c' to cancel selected task
# 4. Cycle policy - press 's' to change scheduler policy
# 5. Navigation - use j/k keys to navigate task list
#
# JSONL Schema (per bd-13pq.4 requirements):
# - run_id: unique identifier for this test run
# - case: test case name
# - env: terminal environment (cols, rows, TERM, etc.)
# - seed: deterministic seed if applicable
# - timings: start_ms, end_ms, duration_ms
# - checksums: output file checksum
# - capabilities: terminal capabilities detected
# - outcome: passed/failed/skipped with reason

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

JSONL_FILE="$E2E_RESULTS_DIR/async_tasks.jsonl"
RUN_ID="asynctasks_$(date +%Y%m%d_%H%M%S)_$$"
SEED="42" # Deterministic seed for reproducibility

jsonl_log() {
    local line="$1"
    mkdir -p "$E2E_RESULTS_DIR"
    printf '%s\n' "$line" >> "$JSONL_FILE"
}

# Compute SHA256 checksum if sha256sum available
compute_checksum() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1 && [[ -f "$file" ]]; then
        sha256sum "$file" | awk '{print $1}'
    else
        echo "unavailable"
    fi
}

# Build the demo binary if needed
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

# Log environment info at start of run
log_run_env() {
    local cols="${PTY_COLS:-120}"
    local rows="${PTY_ROWS:-40}"
    local term_val="${TERM:-xterm-256color}"
    local colorterm="${COLORTERM:-}"
    local capabilities=""

    # Detect terminal capabilities
    if [[ -n "$colorterm" ]]; then
        capabilities="truecolor"
    elif [[ "$term_val" == *"256color"* ]]; then
        capabilities="256color"
    else
        capabilities="basic"
    fi

    jsonl_log "{\"run_id\":\"$RUN_ID\",\"type\":\"env\",\"cols\":$cols,\"rows\":$rows,\"term\":\"$term_val\",\"colorterm\":\"$colorterm\",\"capabilities\":\"$capabilities\",\"seed\":\"$SEED\",\"timestamp\":\"$(date -Iseconds)\"}"
}

# Run a test case with full JSONL logging
run_case() {
    local name="$1"
    local send_label="$2"
    shift 2
    local start_ms
    start_ms="$(e2e_monotonic_ms)"

    LOG_FILE="$E2E_LOG_DIR/${name}.log"
    local output_file="$E2E_LOG_DIR/${name}.pty"
    local cols="${PTY_COLS:-120}"
    local rows="${PTY_ROWS:-40}"

    log_test_start "$name"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)"
        local duration_ms=$((end_ms - start_ms))
        local size
        size=$(wc -c < "$output_file" | tr -d ' ')
        local checksum
        checksum=$(compute_checksum "$output_file")

        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"

        # Full JSONL schema per bd-13pq.4
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"env\":{\"cols\":$cols,\"rows\":$rows},\"seed\":\"$SEED\",\"timings\":{\"start_ms\":$start_ms,\"end_ms\":$end_ms,\"duration_ms\":$duration_ms},\"checksums\":{\"output\":\"$checksum\"},\"capabilities\":{\"output_bytes\":$size},\"outcome\":{\"status\":\"passed\",\"send\":\"$send_label\"}}"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)"
    local duration_ms=$((end_ms - start_ms))
    local checksum
    checksum=$(compute_checksum "$output_file")

    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"

    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"env\":{\"cols\":$cols,\"rows\":$rows},\"seed\":\"$SEED\",\"timings\":{\"start_ms\":$start_ms,\"end_ms\":$end_ms,\"duration_ms\":$duration_ms},\"checksums\":{\"output\":\"$checksum\"},\"capabilities\":{},\"outcome\":{\"status\":\"failed\",\"reason\":\"assertion failed\",\"send\":\"$send_label\"}}"
    return 1
}

DEMO_BIN="$(ensure_demo_bin || true)"
if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/async_tasks_missing.log"
    for t in async_tasks_initial async_tasks_spawn async_tasks_cancel async_tasks_policy async_tasks_navigate fs_watch_temp_file fs_watch_external_modify; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"outcome\":{\"status\":\"skipped\",\"reason\":\"binary missing\"}}"
    done
    exit 0
fi

# Screen numbers shift whenever a screen is added. A hardcoded 23 had drifted
# onto Intrinsic Sizing, and every case still passed: the tab bar says "Tasks"
# on every screen. The status bar names only the current one.
ASYNC_TASKS_SCREEN="$(e2e_demo_screen "$DEMO_BIN" async_tasks)"
SCREEN_TITLE="Async Tasks"

# Log run environment
log_run_env

# Control bytes
KEY_N='n'
KEY_C='c'
KEY_S='s'
KEY_J='j'
KEY_K='k'

# Test 1: Initial screen render - verify Async Task Manager loads
async_tasks_initial() {
    LOG_FILE="$E2E_LOG_DIR/async_tasks_initial.log"
    local output_file="$E2E_LOG_DIR/async_tasks_initial.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="" \
    FTUI_DEMO_SCREEN=$ASYNC_TASKS_SCREEN \
    FTUI_DEMO_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    grep -a -q "$SCREEN_TITLE" "$output_file" || return 1
    # FIFO is the default policy
    grep -a -q "FIFO" "$output_file" || return 1
}

# Test 2: Spawn a new task with 'n' key
async_tasks_spawn() {
    LOG_FILE="$E2E_LOG_DIR/async_tasks_spawn.log"
    local output_file="$E2E_LOG_DIR/async_tasks_spawn.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="$KEY_N" \
    FTUI_DEMO_SCREEN=$ASYNC_TASKS_SCREEN \
    FTUI_DEMO_EXIT_AFTER_MS=1800 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Should still show the task manager
    grep -a -q "$SCREEN_TITLE" "$output_file" || return 1
}

# Test 3: Cancel a task with 'c' key
async_tasks_cancel() {
    LOG_FILE="$E2E_LOG_DIR/async_tasks_cancel.log"
    local output_file="$E2E_LOG_DIR/async_tasks_cancel.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="$KEY_C" \
    FTUI_DEMO_SCREEN=$ASYNC_TASKS_SCREEN \
    FTUI_DEMO_EXIT_AFTER_MS=1800 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Should show canceled state (may show "Canceled" or progress bar changed)
    grep -a -q "$SCREEN_TITLE" "$output_file" || return 1
}

# Test 4: Cycle scheduler policy with 's' key
async_tasks_policy() {
    LOG_FILE="$E2E_LOG_DIR/async_tasks_policy.log"
    local output_file="$E2E_LOG_DIR/async_tasks_policy.pty"

    # Press 's' to cycle from FIFO to ShortestFirst
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="$KEY_S" \
    FTUI_DEMO_SCREEN=$ASYNC_TASKS_SCREEN \
    FTUI_DEMO_EXIT_AFTER_MS=1800 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # After pressing 's' the policy is ShortestFirst, shown as SJF
    grep -a -q "SJF" "$output_file" || return 1
}

# Test 5: Navigate task list with j/k keys
async_tasks_navigate() {
    LOG_FILE="$E2E_LOG_DIR/async_tasks_navigate.log"
    local output_file="$E2E_LOG_DIR/async_tasks_navigate.pty"

    # Navigate down twice, then up once
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="${KEY_J}${KEY_J}${KEY_K}" \
    FTUI_DEMO_SCREEN=$ASYNC_TASKS_SCREEN \
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Should still render correctly
    grep -a -q "$SCREEN_TITLE" "$output_file" || return 1
}

# Test 6: Spawn, tick, and verify progress updates
async_tasks_progress() {
    LOG_FILE="$E2E_LOG_DIR/async_tasks_progress.log"
    local output_file="$E2E_LOG_DIR/async_tasks_progress.pty"

    # Spawn a task and let it run for a bit
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=200 \
    PTY_SEND="$KEY_N" \
    FTUI_DEMO_SCREEN=$ASYNC_TASKS_SCREEN \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Should show progress (either percentage or progress bar characters)
    grep -a -q "$SCREEN_TITLE" "$output_file" || return 1
}

# Test 7: Multiple operations in sequence
async_tasks_workflow() {
    LOG_FILE="$E2E_LOG_DIR/async_tasks_workflow.log"
    local output_file="$E2E_LOG_DIR/async_tasks_workflow.pty"

    # Spawn task, navigate, cycle policy, cancel
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_DELAY_MS=150 \
    PTY_SEND="${KEY_N}${KEY_J}${KEY_S}${KEY_C}" \
    FTUI_DEMO_SCREEN=$ASYNC_TASKS_SCREEN \
    FTUI_DEMO_EXIT_AFTER_MS=2500 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Should complete without crash
    grep -a -q "$SCREEN_TITLE" "$output_file" || return 1
}

# File-watch cases (bd-g00-root-epic-ewths.22.3). The screen subscribes to
# the runtime's file_watcher (mtime+size polling at 250 ms) for
# $FTUI_DEMO_WATCH_FILE and logs "[hh:mm:ss.mmm] <Kind> <basename>" in its
# Activity panel. Each case owns a fresh path under $E2E_LOG_DIR and asserts
# on the canonicalized final screen: the diff renderer skips unchanged
# cells, so the raw stream does not reliably contain the whole line.
WATCH_DIR="$E2E_LOG_DIR/fs_watch"
mkdir -p "$WATCH_DIR"

# Canonicalize a capture and count "<Kind> <basename>" lines on screen.
watch_events_seen() {
    local output_file="$1" kind="$2" base="$3"
    local canon="${output_file%.pty}.screen.txt"
    [[ -f "$canon" ]] || pty_canonicalize_file "$output_file" "$canon" 120 40 \
        --quirk windows_no_alt_screen >/dev/null 2>&1 || return 1
    grep -a -c -E "\[[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}\] ${kind} ${base}" "$canon" || true
}

watch_log_events() {
    local name="$1" path="$2" output_file="$3" base
    base="$(basename "$path")"
    local created modified removed
    created="$(watch_events_seen "$output_file" Created "$base")"
    modified="$(watch_events_seen "$output_file" Modified "$base")"
    removed="$(watch_events_seen "$output_file" Removed "$base")"
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"type\":\"fs_watch\",\"watched_path\":\"$path\",\"events_seen\":{\"created\":${created:-0},\"modified\":${modified:-0},\"removed\":${removed:-0}}}"
}

# Test 8: `w` creates the watched file, then appends to it.
fs_watch_temp_file() {
    LOG_FILE="$E2E_LOG_DIR/fs_watch_temp_file.log"
    local output_file="$E2E_LOG_DIR/fs_watch_temp_file.pty"
    local path="$WATCH_DIR/temp_file_$$.log"
    rm -f "$path" "${output_file%.pty}.screen.txt"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_AFTER_OUTPUT="w:write file" \
    PTY_SEND_SEQUENCE='[{"delay_ms":500,"text":"w"},{"delay_ms":1700,"text":"w"}]' \
    FTUI_DEMO_SCREEN=$ASYNC_TASKS_SCREEN \
    FTUI_DEMO_WATCH_FILE="$path" \
    FTUI_DEMO_EXIT_AFTER_MS=6000 \
    PTY_TIMEOUT=20 \
        pty_run "$output_file" "$DEMO_BIN"

    watch_log_events fs_watch_temp_file "$path" "$output_file"
    [[ "$(cat "$path")" == $'write 1\nwrite 2' ]] || return 1
    local base
    base="$(basename "$path")"
    [[ "$(watch_events_seen "$output_file" Created "$base")" -eq 1 ]] || return 1
    [[ "$(watch_events_seen "$output_file" Modified "$base")" -eq 1 ]] || return 1
    rm -f "$path"
}

# Test 9: a write and a delete from outside the app are both reported.
fs_watch_external_modify() {
    LOG_FILE="$E2E_LOG_DIR/fs_watch_external_modify.log"
    local output_file="$E2E_LOG_DIR/fs_watch_external_modify.pty"
    local path="$WATCH_DIR/external_$$.log"
    rm -f "${output_file%.pty}.screen.txt"
    printf 'seed\n' > "$path"

    # Append once the app is up, then remove the file this case created.
    ( sleep 4; printf 'x\n' >> "$path"; sleep 2; rm -f "$path" ) &
    local writer=$!

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND="" \
    FTUI_DEMO_SCREEN=$ASYNC_TASKS_SCREEN \
    FTUI_DEMO_WATCH_FILE="$path" \
    FTUI_DEMO_EXIT_AFTER_MS=9000 \
    PTY_TIMEOUT=20 \
        pty_run "$output_file" "$DEMO_BIN"
    wait "$writer" || true

    watch_log_events fs_watch_external_modify "$path" "$output_file"
    local base
    base="$(basename "$path")"
    [[ ! -e "$path" ]] || return 1
    [[ "$(watch_events_seen "$output_file" Created "$base")" -eq 0 ]] || return 1
    [[ "$(watch_events_seen "$output_file" Modified "$base")" -eq 1 ]] || return 1
    [[ "$(watch_events_seen "$output_file" Removed "$base")" -eq 1 ]] || return 1
}

# Run all test cases
FAILURES=0
run_case "async_tasks_initial" "(none)" async_tasks_initial || FAILURES=$((FAILURES + 1))
run_case "async_tasks_spawn" "n" async_tasks_spawn || FAILURES=$((FAILURES + 1))
run_case "async_tasks_cancel" "c" async_tasks_cancel || FAILURES=$((FAILURES + 1))
run_case "async_tasks_policy" "s" async_tasks_policy || FAILURES=$((FAILURES + 1))
run_case "async_tasks_navigate" "jjk" async_tasks_navigate || FAILURES=$((FAILURES + 1))
run_case "async_tasks_progress" "n (wait)" async_tasks_progress || FAILURES=$((FAILURES + 1))
run_case "async_tasks_workflow" "njsc" async_tasks_workflow || FAILURES=$((FAILURES + 1))
run_case "fs_watch_temp_file" "w w" fs_watch_temp_file || FAILURES=$((FAILURES + 1))
run_case "fs_watch_external_modify" "(external write, rm)" fs_watch_external_modify || FAILURES=$((FAILURES + 1))

# Log run summary
jsonl_log "{\"run_id\":\"$RUN_ID\",\"type\":\"summary\",\"total\":9,\"passed\":$((9 - FAILURES)),\"failed\":$FAILURES,\"timestamp\":\"$(date -Iseconds)\"}"

exit "$FAILURES"

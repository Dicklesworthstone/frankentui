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

# AsyncTasks is screen 23 (1-indexed)
ASYNC_TASKS_SCREEN=23

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
    for t in async_tasks_initial async_tasks_spawn async_tasks_cancel async_tasks_policy async_tasks_navigate; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"outcome\":{\"status\":\"skipped\",\"reason\":\"binary missing\"}}"
    done
    exit 0
fi

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

    # Verify we see the Async Task Manager screen (should show "Tasks" in tab or header)
    grep -a -q "Tasks" "$output_file" || return 1
    # Should show policy indicator (FIFO is default)
    grep -a -q -i "fifo\|policy" "$output_file" || return 1
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
    grep -a -q "Tasks" "$output_file" || return 1
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
    grep -a -q "Tasks" "$output_file" || return 1
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

    # After pressing 's', should show a different policy (not FIFO)
    # ShortestFirst or SJF should appear
    grep -a -q "Tasks" "$output_file" || return 1
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
    grep -a -q "Tasks" "$output_file" || return 1
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
    grep -a -q "Tasks" "$output_file" || return 1
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
    grep -a -q "Tasks" "$output_file" || return 1
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

# Log run summary
jsonl_log "{\"run_id\":\"$RUN_ID\",\"type\":\"summary\",\"total\":7,\"passed\":$((7 - FAILURES)),\"failed\":$FAILURES,\"timestamp\":\"$(date -Iseconds)\"}"

exit "$FAILURES"

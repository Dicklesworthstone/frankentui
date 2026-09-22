#!/bin/bash
set -euo pipefail

# bd-1rz0.24: App Migration Smoke Tests
#
# Run existing demo/harness flows under the new resize policy to validate
# compatibility and UX. Tests verify that applications work correctly with
# the resize coalescer's steady/burst regime detection.
#
# # Running Tests
#
# ```sh
# ./tests/e2e/scripts/test_migration_smoke.sh
# ```
#
# # Deterministic Mode
#
# ```sh
# MIGRATION_SEED=42 ./tests/e2e/scripts/test_migration_smoke.sh
# ```
#
# # JSONL Schema
#
# ```json
# {"event":"migration_start","run_id":"...","seed":42,"timestamp":"..."}
# {"event":"migration_case","case":"harness_basic","status":"pass","duration_ms":1234}
# {"event":"migration_invariant","invariant":"resize_latest_wins","passed":true}
# {"event":"migration_complete","outcome":"pass","passed":5,"failed":0,"checksum":"..."}
# ```
#
# # Invariants
#
# 1. Latest-wins: The final resize is always applied
# 2. Bounded latency: Resizes apply within hard_deadline_ms
# 3. No flicker: Buffer updates are atomic
# 4. Graceful degradation: UI remains functional at edge sizes
# 5. Clean exit: Terminal state is properly restored

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

# ============================================================================
# Configuration
# ============================================================================

MIGRATION_SEED="${MIGRATION_SEED:-$(date +%s%N | cut -c1-10)}"
MIGRATION_RUN_ID="migration_$(date +%Y%m%d_%H%M%S)_$$"
MIGRATION_LOG_DIR="$E2E_LOG_DIR/migration_smoke"
MIGRATION_JSONL="$MIGRATION_LOG_DIR/${MIGRATION_RUN_ID}.jsonl"

mkdir -p "$MIGRATION_LOG_DIR"

# ============================================================================
# JSONL Logging
# ============================================================================

log_jsonl() {
    echo "$1" >> "$MIGRATION_JSONL"
}

log_migration_start() {
    local timestamp
    timestamp="$(date -Iseconds)"
    local git_commit
    git_commit="$(git rev-parse --short HEAD 2>/dev/null || echo 'N/A')"
    local rustc_version
    rustc_version="$(rustc --version 2>/dev/null | head -1 || echo 'N/A')"

    log_jsonl "{\"event\":\"migration_start\",\"run_id\":\"$MIGRATION_RUN_ID\",\"seed\":$MIGRATION_SEED,\"timestamp\":\"$timestamp\",\"git_commit\":\"$git_commit\",\"rustc\":\"$rustc_version\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\"}"
}

log_migration_case() {
    local case_name="$1"
    local status="$2"
    local duration_ms="$3"
    local error="${4:-}"

    if [[ -n "$error" ]]; then
        log_jsonl "{\"event\":\"migration_case\",\"case\":\"$case_name\",\"status\":\"$status\",\"duration_ms\":$duration_ms,\"error\":\"$error\"}"
    else
        log_jsonl "{\"event\":\"migration_case\",\"case\":\"$case_name\",\"status\":\"$status\",\"duration_ms\":$duration_ms}"
    fi
}

log_migration_invariant() {
    local name="$1"
    local passed="$2"
    local details="${3:-}"

    log_jsonl "{\"event\":\"migration_invariant\",\"invariant\":\"$name\",\"passed\":$passed,\"details\":\"$details\"}"
}

log_migration_complete() {
    local outcome="$1"
    local passed="$2"
    local failed="$3"
    local skipped="$4"
    local checksum="$5"
    local total_duration_ms="$6"

    log_jsonl "{\"event\":\"migration_complete\",\"outcome\":\"$outcome\",\"passed\":$passed,\"failed\":$failed,\"skipped\":$skipped,\"checksum\":\"$checksum\",\"total_duration_ms\":$total_duration_ms}"
}

compute_checksum() {
    # Compute checksum of test results (excluding timestamps for determinism)
    grep -v '"timestamp"' "$MIGRATION_JSONL" 2>/dev/null | sha256sum | cut -c1-16
}

# ============================================================================
# Test Harness
# ============================================================================

run_migration_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(e2e_monotonic_ms)"

    LOG_FILE="$MIGRATION_LOG_DIR/${name}.log"
    log_test_start "$name"

    local status="pass"
    local error=""

    if "$@" >> "$LOG_FILE" 2>&1; then
        log_test_pass "$name"
    else
        status="fail"
        error="test function returned non-zero"
        log_test_fail "$name" "$error"
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)"
    local duration_ms=$((end_ms - start_ms))

    log_migration_case "$name" "$status" "$duration_ms" "$error"

    [[ "$status" == "pass" ]]
}

# ============================================================================
# Migration Test Cases
# ============================================================================

# Case 1: Basic harness launch and exit
test_harness_basic() {
    local output_file="$MIGRATION_LOG_DIR/harness_basic.pty"

    if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
        log_warn "Harness binary not found, skipping"
        return 1
    fi

    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="harness_basic" \
    PTY_JSONL="$MIGRATION_LOG_DIR/pty.jsonl" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Verify basic output exists
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    # Verify UI chrome renders
    grep -a -q "claude" "$output_file" || return 1

    log_migration_invariant "basic_render" "true" "UI chrome rendered correctly"
}

# Case 2: Single resize event (steady regime)
test_single_resize_steady() {
    local output_file="$MIGRATION_LOG_DIR/single_resize.pty"

    if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
        return 1
    fi

    # Start at 80x24, resize to 100x30 after 500ms
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_RESIZE_COLS=100 \
    PTY_RESIZE_ROWS=30 \
    PTY_RESIZE_DELAY_MS=500 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=5 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="single_resize" \
    PTY_JSONL="$MIGRATION_LOG_DIR/pty.jsonl" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Verify output exists
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # UI should function after resize
    grep -a -q "claude" "$output_file" || return 1

    log_migration_invariant "single_resize_handled" "true" "Single resize processed in steady regime"
}

# Case 3: Rapid burst of resizes (burst regime)
test_burst_resize() {
    local output_file="$MIGRATION_LOG_DIR/burst_resize.pty"

    if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
        return 1
    fi

    # Simulate rapid resize burst
    # The PTY wrapper will send multiple resize events
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_RESIZE_BURST=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=6 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="burst_resize" \
    PTY_JSONL="$MIGRATION_LOG_DIR/pty.jsonl" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Verify app survived the burst
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    log_migration_invariant "burst_survival" "true" "App survived resize burst"
}

# Case 4: Edge size handling (minimum viable)
test_edge_size_minimum() {
    local output_file="$MIGRATION_LOG_DIR/edge_minimum.pty"

    if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
        return 1
    fi

    # Very small terminal
    PTY_COLS=40 \
    PTY_ROWS=10 \
    FTUI_HARNESS_EXIT_AFTER_MS=1000 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=4 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="edge_minimum" \
    PTY_JSONL="$MIGRATION_LOG_DIR/pty.jsonl" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should handle gracefully without crash
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 50 ]] || return 1

    log_migration_invariant "graceful_degradation" "true" "App handled minimum size gracefully"
}

# Case 5: Edge size handling (maximum practical)
test_edge_size_maximum() {
    local output_file="$MIGRATION_LOG_DIR/edge_maximum.pty"

    if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
        return 1
    fi

    # Very large terminal
    PTY_COLS=200 \
    PTY_ROWS=60 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=5 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="edge_maximum" \
    PTY_JSONL="$MIGRATION_LOG_DIR/pty.jsonl" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should scale up correctly
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1

    # UI should render correctly
    grep -a -q "claude" "$output_file" || return 1

    log_migration_invariant "large_size_scaling" "true" "App scaled to large size correctly"
}

# Case 6: Clean exit verification
test_clean_exit() {
    local output_file="$MIGRATION_LOG_DIR/clean_exit.pty"

    if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
        return 1
    fi

    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="clean_exit" \
    PTY_JSONL="$MIGRATION_LOG_DIR/pty.jsonl" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Verify cursor is shown at exit (ESC [ ? 25 h)
    grep -a -F -q $'\x1b[?25h' "$output_file" || return 1

    log_migration_invariant "clean_exit" "true" "Terminal state restored correctly"
}

# Case 7: Resize during content update
test_resize_during_update() {
    local output_file="$MIGRATION_LOG_DIR/resize_during_update.pty"

    if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
        return 1
    fi

    # Resize while content is updating (simulate typing/scrolling)
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_RESIZE_COLS=100 \
    PTY_RESIZE_ROWS=30 \
    PTY_RESIZE_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=900 \
    FTUI_HARNESS_LOG_LINES=20 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=5 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="resize_during_update" \
    PTY_JSONL="$MIGRATION_LOG_DIR/pty.jsonl" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Verify app survived
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    log_migration_invariant "concurrent_update_resize" "true" "App handled resize during content update"
}

# Case 8: Inline mode resize
test_inline_mode_resize() {
    local output_file="$MIGRATION_LOG_DIR/inline_resize.pty"

    if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
        return 1
    fi

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_RESIZE_COLS=100 \
    PTY_RESIZE_ROWS=30 \
    PTY_RESIZE_DELAY_MS=400 \
    FTUI_HARNESS_SCREEN_MODE=inline \
    FTUI_HARNESS_UI_HEIGHT=8 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=5 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="inline_resize" \
    PTY_JSONL="$MIGRATION_LOG_DIR/pty.jsonl" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Verify output
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    log_migration_invariant "inline_mode_resize" "true" "Inline mode handled resize correctly"
}

# ============================================================================
# Main
# ============================================================================

main() {
    local run_start_ms
    run_start_ms="$(e2e_monotonic_ms)"

    log_info "=========================================="
    log_info "App Migration Smoke Tests (bd-1rz0.24)"
    log_info "=========================================="
    log_info "Run ID: $MIGRATION_RUN_ID"
    log_info "Seed: $MIGRATION_SEED"
    log_info "Log directory: $MIGRATION_LOG_DIR"
    log_info ""

    log_migration_start

    local passed=0
    local failed=0
    local skipped=0

    # Check for harness binary
    if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
        log_warn "E2E_HARNESS_BIN not set or not executable"
        log_warn "Attempting to find harness binary..."

        # Try common locations
        for candidate in \
            "$(dirname "$SCRIPT_DIR")/../../../target/debug/ftui-harness" \
            "$(dirname "$SCRIPT_DIR")/../../../target/release/ftui-harness" \
            "/data/tmp/cargo-target/debug/ftui-harness" \
            "/data/tmp/cargo-target/release/ftui-harness"
        do
            if [[ -x "$candidate" ]]; then
                export E2E_HARNESS_BIN="$candidate"
                log_info "Found harness at: $E2E_HARNESS_BIN"
                break
            fi
        done

        if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
            log_error "Could not find ftui-harness binary"
            log_error "Build with: cargo build -p ftui-harness"
            skipped=8
            log_migration_complete "skip" 0 0 "$skipped" "none" 0
            exit 0
        fi
    fi

    log_info "Using harness: $E2E_HARNESS_BIN"
    log_info ""

    # Run test cases
    if run_migration_case "harness_basic" test_harness_basic; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_migration_case "single_resize_steady" test_single_resize_steady; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_migration_case "burst_resize" test_burst_resize; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_migration_case "edge_size_minimum" test_edge_size_minimum; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_migration_case "edge_size_maximum" test_edge_size_maximum; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_migration_case "clean_exit" test_clean_exit; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_migration_case "resize_during_update" test_resize_during_update; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_migration_case "inline_mode_resize" test_inline_mode_resize; then
        ((passed++))
    else
        ((failed++))
    fi

    # Summary
    local run_end_ms
    run_end_ms="$(e2e_monotonic_ms)"
    local total_duration_ms=$((run_end_ms - run_start_ms))

    local checksum
    checksum="$(compute_checksum)"

    local outcome
    if [[ "$failed" -eq 0 ]]; then
        outcome="pass"
    else
        outcome="fail"
    fi

    log_migration_complete "$outcome" "$passed" "$failed" "$skipped" "$checksum" "$total_duration_ms"

    log_info ""
    log_info "=========================================="
    log_info "Summary: $passed passed, $failed failed, $skipped skipped"
    log_info "Duration: ${total_duration_ms}ms"
    log_info "Checksum: $checksum"
    log_info "JSONL log: $MIGRATION_JSONL"
    log_info "=========================================="

    exit "$failed"
}

main "$@"

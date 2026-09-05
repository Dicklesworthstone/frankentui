#!/bin/bash
set -euo pipefail

# =============================================================================
# E2E Test Suite: Drag-and-Drop (bd-1csc.6)
#
# Tests the drag-and-drop functionality via Rust E2E tests.
#
# Since DragDropDemo isn't wired to the ScreenId enum (it's implemented but
# not exposed in the main binary's view selector), we run the comprehensive
# Rust E2E tests directly and capture JSONL output.
#
# Coverage:
# - Sortable list item reordering
# - Cross-container transfer
# - Keyboard drag pick up, navigation, and drop
# - Mode switching
# - Determinism verification
# - Zero-area/boundary condition handling
#
# Run: ./tests/e2e/scripts/test_drag_drop.sh
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"

# -----------------------------------------------------------------------------
# Configuration
# -----------------------------------------------------------------------------

TEST_SUITE="drag_drop"
LOG_FILE="${E2E_LOG_DIR:-/tmp}/drag_drop_e2e.log"
RESULTS_FILE="${E2E_RESULTS_DIR:-/tmp}/drag_drop_results.jsonl"

mkdir -p "$(dirname "$LOG_FILE")" "$(dirname "$RESULTS_FILE")"

# Emit JSONL log entry
emit_jsonl() {
    local event="$1"
    shift
    local ts
    ts="$("${E2E_PYTHON:-python3}" -c 'from datetime import datetime; print(datetime.now().astimezone().isoformat(timespec="seconds"))')" || return 2
    local fields="\"ts\":\"$ts\",\"suite\":\"$TEST_SUITE\",\"event\":\"$event\""
    while [[ $# -gt 0 ]]; do
        local key="$1"
        local val="$2"
        # Escape quotes in value
        val="${val//\"/\\\"}"
        fields="$fields,\"$key\":\"$val\""
        shift 2
    done
    echo "{$fields}" >> "$RESULTS_FILE"
}

# -----------------------------------------------------------------------------
# Test Functions
# -----------------------------------------------------------------------------

run_rust_e2e_tests() {
    local name="rust_e2e_tests"
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2

    log_test_start "$name"
    emit_jsonl "test_start" "test" "$name"

    # Run the Rust E2E tests with JSONL output capture
    local output
    local exit_code=0

    output=$(cargo test -p ftui-demo-showcase --test drag_drop_e2e -- --nocapture 2>&1) || exit_code=$?

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))

    # Log output
    echo "$output" >> "$LOG_FILE"

    # Parse test results from cargo output
    local passed
    local failed
    passed=$(echo "$output" | grep -E "^test result:" | grep -oE '[0-9]+ passed' | grep -oE '[0-9]+' || echo "0")
    failed=$(echo "$output" | grep -E "^test result:" | grep -oE '[0-9]+ failed' | grep -oE '[0-9]+' || echo "0")

    if [[ $exit_code -eq 0 ]]; then
        log_test_pass "$name"
        emit_jsonl "test_pass" "test" "$name" "duration_ms" "$duration_ms" "passed" "$passed" "failed" "$failed"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    else
        log_test_fail "$name" "Rust tests failed"
        emit_jsonl "test_fail" "test" "$name" "duration_ms" "$duration_ms" "passed" "$passed" "failed" "$failed"
        record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "Rust tests failed"
        return 1
    fi
}

run_snapshot_tests() {
    local name="snapshot_tests"
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2

    log_test_start "$name"
    emit_jsonl "test_start" "test" "$name"

    # Check that snapshot files exist and are valid
    local snapshots_dir="$PROJECT_ROOT/crates/ftui-demo-showcase/tests/snapshots"
    local expected_snaps=(
        "drag_drop_initial_80x24.snap"
        "drag_drop_initial_120x40.snap"
        "drag_drop_keyboard_mode_80x24.snap"
    )

    local missing=0
    for snap in "${expected_snaps[@]}"; do
        if [[ ! -f "$snapshots_dir/$snap" ]]; then
            log_warn "Missing snapshot: $snap"
            missing=$((missing + 1))
        fi
    done

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))

    if [[ $missing -eq 0 ]]; then
        log_test_pass "$name"
        emit_jsonl "test_pass" "test" "$name" "duration_ms" "$duration_ms" "snapshots_found" "${#expected_snaps[@]}"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    else
        log_test_fail "$name" "$missing snapshots missing"
        emit_jsonl "test_fail" "test" "$name" "duration_ms" "$duration_ms" "missing_count" "$missing"
        record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "$missing snapshots missing"
        return 1
    fi
}

verify_invariants() {
    local name="invariants_check"
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2

    log_test_start "$name"
    emit_jsonl "test_start" "test" "$name"

    # Check that invariant tests exist and document the expected behaviors
    local test_file="$PROJECT_ROOT/crates/ftui-demo-showcase/tests/drag_drop_e2e.rs"

    local invariants_documented=0
    if grep -q "Item count preservation" "$test_file"; then
        invariants_documented=$((invariants_documented + 1))
    fi
    if grep -q "Selection bounds" "$test_file"; then
        invariants_documented=$((invariants_documented + 1))
    fi
    if grep -q "Mode transitions" "$test_file"; then
        invariants_documented=$((invariants_documented + 1))
    fi
    if grep -q "Drag lifecycle" "$test_file"; then
        invariants_documented=$((invariants_documented + 1))
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))

    if [[ $invariants_documented -ge 4 ]]; then
        log_test_pass "$name"
        emit_jsonl "test_pass" "test" "$name" "duration_ms" "$duration_ms" "invariants_found" "$invariants_documented"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    else
        log_test_fail "$name" "Only $invariants_documented/4 invariants documented"
        emit_jsonl "test_fail" "test" "$name" "duration_ms" "$duration_ms" "invariants_found" "$invariants_documented"
        record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "Missing invariant documentation"
        return 1
    fi
}

check_determinism() {
    local name="determinism_check"
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2

    log_test_start "$name"
    emit_jsonl "test_start" "test" "$name"

    # Run determinism test specifically
    local output
    local exit_code=0

    output=$(cargo test -p ftui-demo-showcase --test drag_drop_e2e e2e_deterministic -- --nocapture 2>&1) || exit_code=$?

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))

    # Check for hash match in output
    if echo "$output" | grep -q '"match":"true"'; then
        log_test_pass "$name"
        emit_jsonl "test_pass" "test" "$name" "duration_ms" "$duration_ms" "deterministic" "true"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    else
        log_test_fail "$name" "Determinism check failed"
        emit_jsonl "test_fail" "test" "$name" "duration_ms" "$duration_ms" "deterministic" "false"
        record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "Output not deterministic"
        return 1
    fi
}

# -----------------------------------------------------------------------------
# Main
# -----------------------------------------------------------------------------

main() {
    local suite_start_ms
    suite_start_ms="$(e2e_monotonic_ms)" || return 2

    log_info "=== Drag-and-Drop E2E Test Suite (bd-1csc.6) ==="
    emit_jsonl "suite_start" "version" "1.0.0"

    # Environment info
    emit_jsonl "env" \
        "rust_version" "$(rustc --version 2>/dev/null | head -1 || echo 'unknown')" \
        "cargo_version" "$(cargo --version 2>/dev/null | head -1 || echo 'unknown')" \
        "project_root" "$PROJECT_ROOT"

    local total=0
    local passed=0
    local failed=0

    # Run test functions
    for test_fn in run_rust_e2e_tests run_snapshot_tests verify_invariants check_determinism; do
        total=$((total + 1))
        if $test_fn; then
            passed=$((passed + 1))
        else
            failed=$((failed + 1))
        fi
    done

    local suite_end_ms
    suite_end_ms="$(e2e_monotonic_ms)" || return 2
    local suite_duration_ms=$((suite_end_ms - suite_start_ms))

    emit_jsonl "suite_complete" \
        "total" "$total" \
        "passed" "$passed" \
        "failed" "$failed" \
        "duration_ms" "$suite_duration_ms"

    log_info "=== Suite Complete: $passed/$total passed ==="

    if [[ $failed -gt 0 ]]; then
        return 1
    fi
    return 0
}

main "$@"

#!/bin/bash
set -euo pipefail

# E2E tests for Intrinsic Sizing System (bd-2dow.6)
# Tests MeasurableWidget implementations and FitContent layout integration.
#
# Environment Variables:
#   PROPTEST_SEED   - Seed for deterministic proptest runs
#   E2E_VERBOSE     - Set to "1" for verbose output
#
# JSONL Schema (per-case entry):
#   run_id, case, status, duration_ms, ts, seed, test_count, pass_count, fail_count

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# Fallback results directory if not sourced
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-/tmp/ftui_e2e_results}"
mkdir -p "$E2E_RESULTS_DIR"

# Source common utilities if available
if [[ -f "$LIB_DIR/common.sh" ]]; then
    # shellcheck source=/dev/null
    source "$LIB_DIR/common.sh"
fi
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh" # e2e_monotonic_ms

JSONL_FILE="$E2E_RESULTS_DIR/intrinsic_sizing.jsonl"
RUN_ID="intrinsic_$(date +%Y%m%d_%H%M%S)_$$"
TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# Deterministic seed for reproducibility
if [[ -z "${PROPTEST_SEED:-}" ]]; then
    PROPTEST_SEED="$(od -An -N4 -tu4 /dev/urandom 2>/dev/null | tr -d ' ' || date +%s)"
fi
export PROPTEST_SEED

echo "=== Intrinsic Sizing E2E Test Suite (bd-2dow.6) ==="
echo "Run ID: $RUN_ID"
echo "Seed: $PROPTEST_SEED"
echo "Log: $JSONL_FILE"
echo ""

# JSON logging helper
log_jsonl() {
    local case="$1"
    local status="$2"
    local duration="$3"
    local test_count="${4:-0}"
    local pass_count="${5:-0}"
    local fail_count="${6:-0}"

    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --arg run_id "$RUN_ID" \
            --arg case "$case" \
            --arg status "$status" \
            --argjson duration_ms "$duration" \
            --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            --arg seed "$PROPTEST_SEED" \
            --argjson test_count "$test_count" \
            --argjson pass_count "$pass_count" \
            --argjson fail_count "$fail_count" \
            '{run_id:$run_id,case:$case,status:$status,duration_ms:$duration_ms,ts:$ts,seed:$seed,test_count:$test_count,pass_count:$pass_count,fail_count:$fail_count}' \
            >> "$JSONL_FILE"
    else
        echo "{\"run_id\":\"$RUN_ID\",\"case\":\"$case\",\"status\":\"$status\",\"duration_ms\":$duration,\"seed\":\"$PROPTEST_SEED\"}" >> "$JSONL_FILE"
    fi
}

# Run a test suite and capture results
run_test_suite() {
    local name="$1"
    local package="$2"
    local filter="$3"

    echo "[$name] Running..."
    # Not `date +%s%3N`: BSD date prints "...3N", and the subtraction below
    # then aborted the script on macOS.
    local start_time
    start_time="$(e2e_monotonic_ms)"

    local output
    local exit_code=0
    output=$(cargo test -p "$package" -- "$filter" 2>&1) || exit_code=$?

    local end_time
    end_time="$(e2e_monotonic_ms)"
    local duration=$((end_time - start_time))

    # Extract test counts from output
    local test_count=$(echo "$output" | grep -oE '[0-9]+ passed' | head -1 | grep -oE '[0-9]+' || echo "0")
    local fail_count=$(echo "$output" | grep -oE '[0-9]+ failed' | head -1 | grep -oE '[0-9]+' || echo "0")

    if [[ $exit_code -eq 0 ]]; then
        echo "[$name] PASS ($test_count tests in ${duration}ms)"
        log_jsonl "$name" "pass" "$duration" "$test_count" "$test_count" "0"
        return 0
    else
        echo "[$name] FAIL ($fail_count failures)"
        if [[ "${E2E_VERBOSE:-0}" == "1" ]]; then
            echo "$output"
        fi
        log_jsonl "$name" "fail" "$duration" "$test_count" "$((test_count - fail_count))" "$fail_count"
        return 1
    fi
}

# Track overall results
TOTAL_SUITES=0
PASS_SUITES=0
FAIL_SUITES=0

# -----------------------------------------------------------------------------
# Test Suite 1: SizeConstraints unit tests
# -----------------------------------------------------------------------------
TOTAL_SUITES=$((TOTAL_SUITES + 1))
if run_test_suite "size_constraints" "ftui-widgets" "measurable::tests::size_constraints"; then
    PASS_SUITES=$((PASS_SUITES + 1))
else
    FAIL_SUITES=$((FAIL_SUITES + 1))
fi

# -----------------------------------------------------------------------------
# Test Suite 2: MeasurableWidget property tests
# -----------------------------------------------------------------------------
TOTAL_SUITES=$((TOTAL_SUITES + 1))
if run_test_suite "property_tests" "ftui-widgets" "measurable::tests::property_tests"; then
    PASS_SUITES=$((PASS_SUITES + 1))
else
    FAIL_SUITES=$((FAIL_SUITES + 1))
fi

# -----------------------------------------------------------------------------
# Test Suite 3: Layout solver FitContent tests
# -----------------------------------------------------------------------------
TOTAL_SUITES=$((TOTAL_SUITES + 1))
if run_test_suite "fit_content" "ftui-layout" "fit_content"; then
    PASS_SUITES=$((PASS_SUITES + 1))
else
    FAIL_SUITES=$((FAIL_SUITES + 1))
fi

# -----------------------------------------------------------------------------
# Test Suite 4: Widget measure tests (Paragraph, Block, Sparkline)
# -----------------------------------------------------------------------------
TOTAL_SUITES=$((TOTAL_SUITES + 1))
if run_test_suite "widget_measure" "ftui-widgets" "measure"; then
    PASS_SUITES=$((PASS_SUITES + 1))
else
    FAIL_SUITES=$((FAIL_SUITES + 1))
fi

# -----------------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------------
echo ""
echo "======================================"
echo "        TEST RESULTS SUMMARY          "
echo "======================================"
echo "Total suites: $TOTAL_SUITES"
echo "Passed:       $PASS_SUITES"
echo "Failed:       $FAIL_SUITES"
echo "Seed:         $PROPTEST_SEED"
echo "Results:      $JSONL_FILE"
echo ""

# Log summary
log_jsonl "summary" "$([ $FAIL_SUITES -eq 0 ] && echo 'pass' || echo 'fail')" "0" "$TOTAL_SUITES" "$PASS_SUITES" "$FAIL_SUITES"

# Exit with failure if any suite failed
[ $FAIL_SUITES -eq 0 ]

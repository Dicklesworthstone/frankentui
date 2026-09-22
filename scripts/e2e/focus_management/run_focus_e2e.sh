#!/bin/bash
# Focus Management E2E Test Suite
# bd-1n5t.6: End-to-end validation of focus management system
#
# This suite validates the focus management subsystem through:
# 1. Unit + integration test execution with structured JSONL output
# 2. Performance gate validation (focus transitions < 100ms)
# 3. Invariant verification (no focus loss, trap confinement, cycle detection)
#
# Usage:
#   ./scripts/e2e/focus_management/run_focus_e2e.sh              # Run all
#   ./scripts/e2e/focus_management/run_focus_e2e.sh --verbose    # Extra output
#   ./scripts/e2e/focus_management/run_focus_e2e.sh --json out.jsonl  # JSONL output
#
# Environment:
#   E2E_LOG_DIR     Override log directory
#   FOCUS_E2E_ONLY  Run only named category (nav|trap|persist|a11y|perf)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui-focus-e2e-${TIMESTAMP}}"
JSONL_OUT=""
VERBOSE=false
ONLY="${FOCUS_E2E_ONLY:-}"

PASS=0
FAIL=0
SKIP=0
TOTAL=0

# Milliseconds from the monotonic clock. Not `date +%s%3N`: BSD date prints
# "...3N", and the duration arithmetic then aborted the suite on macOS.
now_ms() {
    python3 -c 'import time; print(time.monotonic_ns() // 1_000_000)'
}

for arg in "$@"; do
    case "$arg" in
        --verbose|-v) VERBOSE=true ;;
        --json)       shift; JSONL_OUT="${1:-$LOG_DIR/results.jsonl}" ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--json <path>]"
            echo ""
            echo "Categories: nav, trap, persist, a11y, perf"
            echo "  FOCUS_E2E_ONLY=nav $0   # run only navigation tests"
            exit 0
            ;;
    esac
done

mkdir -p "$LOG_DIR"

# ============================================================================
# Helpers
# ============================================================================

emit_jsonl() {
    local name="$1" status="$2" duration_ms="$3" category="$4"
    local error="${5:-}"
    local ts
    ts="$(date -Iseconds)"
    local line="{\"name\":\"$name\",\"status\":\"$status\",\"duration_ms\":$duration_ms,\"category\":\"$category\",\"timestamp\":\"$ts\""
    if [[ -n "$error" ]]; then
        # Escape quotes in error
        error="$(printf '%s' "$error" | sed 's/"/\\"/g')"
        line="$line,\"error\":\"$error\""
    fi
    line="$line}"
    echo "$line" >> "$LOG_DIR/results.jsonl"
    if [[ -n "$JSONL_OUT" ]]; then
        echo "$line" >> "$JSONL_OUT"
    fi
}

log_info() {
    echo -e "\033[1;34m[INFO]\033[0m $(date +%H:%M:%S) $*"
}

log_pass() {
    echo -e "\033[1;32m[PASS]\033[0m $(date +%H:%M:%S) $*"
}

log_fail() {
    echo -e "\033[1;31m[FAIL]\033[0m $(date +%H:%M:%S) $*"
}

log_skip() {
    echo -e "\033[1;33m[SKIP]\033[0m $(date +%H:%M:%S) $*"
}

should_run() {
    local category="$1"
    [[ -z "$ONLY" || "$ONLY" == "$category" ]]
}

run_cargo_test() {
    local test_name="$1"
    local category="$2"
    local log_file="$LOG_DIR/${category}_${test_name}.log"
    TOTAL=$((TOTAL + 1))

    local start_ms
    start_ms="$(now_ms)"

    local exit_code=0
    if $VERBOSE; then
        if cargo test -p ftui-widgets --lib -- "focus::manager::tests::$test_name" 2>&1 | tee "$log_file"; then
            exit_code=0
        else
            exit_code=1
        fi
    else
        if cargo test -p ftui-widgets --lib -- "focus::manager::tests::$test_name" > "$log_file" 2>&1; then
            exit_code=0
        else
            exit_code=1
        fi
    fi

    local end_ms
    end_ms="$(now_ms)"
    local duration_ms=$((end_ms - start_ms))

    if [[ $exit_code -eq 0 ]]; then
        log_pass "$test_name (${duration_ms}ms)"
        PASS=$((PASS + 1))
        emit_jsonl "$test_name" "passed" "$duration_ms" "$category"
    else
        log_fail "$test_name (${duration_ms}ms)"
        FAIL=$((FAIL + 1))
        emit_jsonl "$test_name" "failed" "$duration_ms" "$category" "test failed (exit=$exit_code)"
    fi
}

run_integration_test() {
    local test_name="$1"
    local category="$2"
    local log_file="$LOG_DIR/${category}_${test_name}.log"
    TOTAL=$((TOTAL + 1))

    local start_ms
    start_ms="$(now_ms)"

    local exit_code=0
    if $VERBOSE; then
        if cargo test -p ftui-widgets --test focus_integration -- "$test_name" 2>&1 | tee "$log_file"; then
            exit_code=0
        else
            exit_code=1
        fi
    else
        if cargo test -p ftui-widgets --test focus_integration -- "$test_name" > "$log_file" 2>&1; then
            exit_code=0
        else
            exit_code=1
        fi
    fi

    local end_ms
    end_ms="$(now_ms)"
    local duration_ms=$((end_ms - start_ms))

    if [[ $exit_code -eq 0 ]]; then
        log_pass "$test_name (${duration_ms}ms)"
        PASS=$((PASS + 1))
        emit_jsonl "$test_name" "passed" "$duration_ms" "$category"
    else
        log_fail "$test_name (${duration_ms}ms)"
        FAIL=$((FAIL + 1))
        emit_jsonl "$test_name" "failed" "$duration_ms" "$category" "test failed (exit=$exit_code)"
    fi
}

run_test_batch() {
    local category="$1"
    local test_pattern="$2"
    local log_file="$LOG_DIR/${category}_batch.log"
    TOTAL=$((TOTAL + 1))

    local start_ms
    start_ms="$(now_ms)"

    local exit_code=0
    if $VERBOSE; then
        if cargo test -p ftui-widgets --lib -- "$test_pattern" 2>&1 | tee "$log_file"; then
            exit_code=0
        else
            exit_code=1
        fi
    else
        if cargo test -p ftui-widgets --lib -- "$test_pattern" > "$log_file" 2>&1; then
            exit_code=0
        else
            exit_code=1
        fi
    fi

    local end_ms
    end_ms="$(now_ms)"
    local duration_ms=$((end_ms - start_ms))

    # Count tests from output
    local test_count
    test_count=$(grep -c "^test .* ok$" "$log_file" 2>/dev/null || echo 0)

    if [[ $exit_code -eq 0 ]]; then
        log_pass "$category batch ($test_count tests, ${duration_ms}ms)"
        PASS=$((PASS + 1))
        emit_jsonl "${category}_batch" "passed" "$duration_ms" "$category"
    else
        log_fail "$category batch (${duration_ms}ms)"
        FAIL=$((FAIL + 1))
        emit_jsonl "${category}_batch" "failed" "$duration_ms" "$category" "batch failed (exit=$exit_code)"
    fi
}

run_integration_batch() {
    local category="$1"
    local test_pattern="$2"
    local log_file="$LOG_DIR/${category}_integration_batch.log"
    TOTAL=$((TOTAL + 1))

    local start_ms
    start_ms="$(now_ms)"

    local exit_code=0
    if $VERBOSE; then
        if cargo test -p ftui-widgets --test focus_integration -- "$test_pattern" 2>&1 | tee "$log_file"; then
            exit_code=0
        else
            exit_code=1
        fi
    else
        if cargo test -p ftui-widgets --test focus_integration -- "$test_pattern" > "$log_file" 2>&1; then
            exit_code=0
        else
            exit_code=1
        fi
    fi

    local end_ms
    end_ms="$(now_ms)"
    local duration_ms=$((end_ms - start_ms))

    local test_count
    test_count=$(grep -c "^test .* ok$" "$log_file" 2>/dev/null || echo 0)

    if [[ $exit_code -eq 0 ]]; then
        log_pass "$category integration ($test_count tests, ${duration_ms}ms)"
        PASS=$((PASS + 1))
        emit_jsonl "${category}_integration" "passed" "$duration_ms" "$category"
    else
        log_fail "$category integration (${duration_ms}ms)"
        FAIL=$((FAIL + 1))
        emit_jsonl "${category}_integration" "failed" "$duration_ms" "$category" "integration batch failed"
    fi
}

# ============================================================================
# Main
# ============================================================================

echo "=============================================="
echo "  Focus Management E2E Test Suite"
echo "  bd-1n5t.6"
echo "=============================================="
echo ""
log_info "Project root: $PROJECT_ROOT"
log_info "Log directory: $LOG_DIR"
log_info "Category filter: ${ONLY:-all}"
echo ""

cd "$PROJECT_ROOT"

# Record environment
{
    echo "{\"event\":\"environment\",\"timestamp\":\"$(date -Iseconds)\",\"rust_version\":\"$(rustc --version 2>/dev/null)\",\"git_commit\":\"$(git log -1 --format=%H 2>/dev/null || echo unknown)\"}"
} >> "$LOG_DIR/results.jsonl"

# ────────────────────────────────────────────────────────────────────────────
# 1. Basic Navigation Tests
# ────────────────────────────────────────────────────────────────────────────
if should_run "nav"; then
    echo ""
    log_info "=== 1. Basic Navigation ==="

    # Unit tests: tab order, next/prev, arrow navigation, wrapping
    run_test_batch "nav_unit" "focus::manager::tests::focus_next"
    run_test_batch "nav_unit_prev" "focus::manager::tests::focus_prev"
    run_test_batch "nav_unit_spatial" "focus::manager::tests::navigate_spatial"

    # Graph tests: tab chain, edges, cycle detection
    run_test_batch "nav_graph" "focus::graph::tests"

    # Spatial algorithm tests
    run_test_batch "nav_spatial" "focus::spatial::tests"

    # Integration: tab traversal, form layouts, spatial+manager
    run_integration_batch "nav" "tab_traversal"
    run_integration_batch "nav_spatial" "spatial"
    run_integration_batch "nav_form" "form_layout"
fi

# ────────────────────────────────────────────────────────────────────────────
# 2. Focus Trapping Tests
# ────────────────────────────────────────────────────────────────────────────
if should_run "trap"; then
    echo ""
    log_info "=== 2. Focus Trapping ==="

    # Unit tests: push/pop trap, trap confinement
    run_test_batch "trap_unit" "focus::manager::tests::push_trap"
    run_test_batch "trap_unit_pop" "focus::manager::tests::pop_trap"

    # Integration: modal confinement, nested traps
    run_integration_batch "trap_modal" "modal_trap"
    run_integration_batch "trap_nested" "nested_trap"
fi

# ────────────────────────────────────────────────────────────────────────────
# 3. Focus Persistence Tests
# ────────────────────────────────────────────────────────────────────────────
if should_run "persist"; then
    echo ""
    log_info "=== 3. Focus Persistence ==="

    # Unit tests: focus survives graph changes, history
    run_test_batch "persist_history" "focus::manager::tests::focus_back"
    run_test_batch "persist_blur" "focus::manager::tests::blur"

    # Integration: widget removal, re-render survival
    run_integration_batch "persist" "focus_survives"
    run_integration_batch "persist_removal" "removal"
fi

# ────────────────────────────────────────────────────────────────────────────
# 4. Accessibility Tests
# ────────────────────────────────────────────────────────────────────────────
if should_run "a11y"; then
    echo ""
    log_info "=== 4. Accessibility ==="

    # Focus events (announcements for screen readers)
    run_test_batch "a11y_events" "focus::manager::tests::focus_event"

    # Integration: focus event emission
    run_integration_batch "a11y" "focus_event"
fi

# ────────────────────────────────────────────────────────────────────────────
# 5. Performance Gate
# ────────────────────────────────────────────────────────────────────────────
if should_run "perf"; then
    echo ""
    log_info "=== 5. Performance Gate ==="

    # Run the full focus test suite and measure wall time
    TOTAL=$((TOTAL + 1))
    PERF_LOG="$LOG_DIR/perf_full_suite.log"

    perf_start_ms="$(now_ms)"

    if cargo test -p ftui-widgets --lib -- "focus::" > "$PERF_LOG" 2>&1; then
        perf_exit=0
    else
        perf_exit=1
    fi

    perf_end_ms="$(now_ms)"
    perf_duration_ms=$((perf_end_ms - perf_start_ms))

    # Count tests
    perf_test_count=$(grep -c "^test .* ok$" "$PERF_LOG" 2>/dev/null || echo 0)

    if [[ $perf_exit -eq 0 ]]; then
        log_pass "Full focus suite: $perf_test_count tests in ${perf_duration_ms}ms"
        PASS=$((PASS + 1))
        emit_jsonl "perf_full_suite" "passed" "$perf_duration_ms" "perf"
    else
        log_fail "Full focus suite failed (${perf_duration_ms}ms)"
        FAIL=$((FAIL + 1))
        emit_jsonl "perf_full_suite" "failed" "$perf_duration_ms" "perf" "suite failed"
    fi

    # Integration test performance
    TOTAL=$((TOTAL + 1))
    PERF_INT_LOG="$LOG_DIR/perf_integration.log"

    perf_int_start="$(now_ms)"

    if cargo test -p ftui-widgets --test focus_integration > "$PERF_INT_LOG" 2>&1; then
        perf_int_exit=0
    else
        perf_int_exit=1
    fi

    perf_int_end="$(now_ms)"
    perf_int_dur=$((perf_int_end - perf_int_start))

    perf_int_count=$(grep -c "^test .* ok$" "$PERF_INT_LOG" 2>/dev/null || echo 0)

    if [[ $perf_int_exit -eq 0 ]]; then
        log_pass "Integration suite: $perf_int_count tests in ${perf_int_dur}ms"
        PASS=$((PASS + 1))
        emit_jsonl "perf_integration" "passed" "$perf_int_dur" "perf"
    else
        log_fail "Integration suite failed (${perf_int_dur}ms)"
        FAIL=$((FAIL + 1))
        emit_jsonl "perf_integration" "failed" "$perf_int_dur" "perf" "integration suite failed"
    fi

    # Performance gate: check integration perf tests pass
    TOTAL=$((TOTAL + 1))
    PERF_GATE_LOG="$LOG_DIR/perf_gate.log"

    gate_start="$(now_ms)"

    if cargo test -p ftui-widgets --test focus_integration -- "perf_" > "$PERF_GATE_LOG" 2>&1; then
        gate_exit=0
    else
        gate_exit=1
    fi

    gate_end="$(now_ms)"
    gate_dur=$((gate_end - gate_start))

    if [[ $gate_exit -eq 0 ]]; then
        log_pass "Performance gates passed (${gate_dur}ms)"
        PASS=$((PASS + 1))
        emit_jsonl "perf_gates" "passed" "$gate_dur" "perf"
    else
        log_fail "Performance gates FAILED (${gate_dur}ms)"
        FAIL=$((FAIL + 1))
        emit_jsonl "perf_gates" "failed" "$gate_dur" "perf" "performance gate violation"
    fi
fi

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "=============================================="
echo "  Focus E2E Summary"
echo "=============================================="
echo ""
echo "  Passed:  $PASS"
echo "  Failed:  $FAIL"
echo "  Skipped: $SKIP"
echo "  Total:   $TOTAL"
echo ""
log_info "JSONL results: $LOG_DIR/results.jsonl"
if [[ -n "$JSONL_OUT" ]]; then
    log_info "JSONL copy: $JSONL_OUT"
fi
echo ""

# Emit summary JSONL
{
    echo "{\"event\":\"summary\",\"timestamp\":\"$(date -Iseconds)\",\"passed\":$PASS,\"failed\":$FAIL,\"skipped\":$SKIP,\"total\":$TOTAL}"
} >> "$LOG_DIR/results.jsonl"

if [[ $FAIL -gt 0 ]]; then
    echo -e "\033[1;31m$FAIL test group(s) failed!\033[0m"
    exit 1
else
    echo -e "\033[1;32mAll focus E2E tests passed!\033[0m"
    exit 0
fi

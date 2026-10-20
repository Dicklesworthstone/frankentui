#!/bin/bash
# =============================================================================
# test_flake_detector.sh - E2E test for flake detector (bd-1plj)
# =============================================================================
#
# Purpose:
# - Inject synthetic latency spikes and verify early failure detection
# - Run stable workload and verify no false positives
# - Log e-value trajectory and decision points
#
# Usage:
#   ./test_flake_detector.sh [--verbose]
#
# Exit codes:
#   0 - All tests passed
#   1 - Test failure
#   2 - Setup/runtime error
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="${PROJECT_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"

VERBOSE=false
LOG_LEVEL="${LOG_LEVEL:-INFO}"
OUTPUT_DIR="${E2E_RESULTS_DIR:-/tmp/ftui_flake_detector_e2e}"

# =============================================================================
# Argument parsing
# =============================================================================

for arg in "$@"; do
    case "$arg" in
        --verbose|-v)
            VERBOSE=true
            LOG_LEVEL="DEBUG"
            ;;
        --help|-h)
            echo "Usage: $0 [--verbose]"
            exit 0
            ;;
    esac
done

# =============================================================================
# Logging
# =============================================================================

log_info() {
    echo "[INFO] $(date -Iseconds) $*"
}

log_debug() {
    [[ "$VERBOSE" == "true" ]] && echo "[DEBUG] $(date -Iseconds) $*"
    return 0
}

log_success() {
    echo "[OK] $*"
}

log_fail() {
    echo "[FAIL] $*" >&2
}

# Milliseconds from the monotonic clock. Not `date +%s%3N`: BSD date prints
# "...3N", and the duration arithmetic below then aborted the script on macOS.
now_ms() {
    python3 -c 'import time; print(time.monotonic_ns() // 1_000_000)'
}

# =============================================================================
# Setup
# =============================================================================

mkdir -p "$OUTPUT_DIR"
START_TS="$(now_ms)"
TIMESTAMP="$(date +%Y%m%d_%H%M%S)"

log_info "Flake Detector E2E Test (bd-1plj)"
log_info "Project root: $PROJECT_ROOT"
log_info "Output dir: $OUTPUT_DIR"

# Environment log
cat > "$OUTPUT_DIR/env_${TIMESTAMP}.jsonl" <<EOF
{"event":"env","timestamp":"$(date -Iseconds)","user":"$(whoami)","hostname":"$(hostname)"}
{"event":"rust","rustc":"$(rustc --version 2>/dev/null || echo 'N/A')"}
{"event":"git","commit":"$(git rev-parse HEAD 2>/dev/null || echo 'N/A')"}
EOF

# =============================================================================
# Build
# =============================================================================

log_info "Building ftui-runtime tests..."
BUILD_START="$(now_ms)"

if ! cargo build -p ftui-runtime --features experimental --lib --tests 2>"$OUTPUT_DIR/build.log"; then
    log_fail "Build failed! See $OUTPUT_DIR/build.log"
    exit 2
fi

BUILD_END="$(now_ms)"
BUILD_MS=$((BUILD_END - BUILD_START))
log_debug "Build completed in ${BUILD_MS}ms"

# =============================================================================
# Run unit tests
# =============================================================================

log_info "Running flake detector unit tests..."
TEST_START="$(now_ms)"

TEST_OUTPUT="$OUTPUT_DIR/unit_tests.txt"
# flake_detector is `#[cfg(feature = "experimental")]`: without the feature
# zero tests compile and this step "passes" (bd-g00-root-epic-ewths.6.38).
if cargo test -p ftui-runtime --features experimental --lib flake_detector:: -- --nocapture 2>&1 | tee "$TEST_OUTPUT"; then
    log_success "All unit tests passed"
else
    log_fail "Unit tests failed"
    exit 1
fi

TEST_END="$(now_ms)"
TEST_MS=$((TEST_END - TEST_START))
log_debug "Tests completed in ${TEST_MS}ms"

# =============================================================================
# Verify key test outcomes
# =============================================================================

log_info "Verifying test outcomes..."

# Each named property test must have run and passed; a missing test fails the
# suite (it used to log and carry on to a hard-coded "pass").
prop_status() {  # prints true/false on stdout; logging goes to stderr
    if grep -q "$1 ... ok" "$TEST_OUTPUT"; then
        log_success "$2" >&2
        echo true
    else
        log_fail "$1 not found or failed"
        echo false
    fi
}
NO_FALSE_POSITIVES=$(prop_status unit_stable_run_no_false_positives "Stable run: no false positives")
SPIKE_DETECTION=$(prop_status unit_spike_detection "Spike detection: correctly identified")
EVALUE_NONNEGATIVE=$(prop_status unit_eprocess_nonnegative "E-values: always non-negative")
OPTIONAL_STOPPING=$(prop_status unit_optional_stopping "Optional stopping: preserves validity")

# =============================================================================
# Final results
# =============================================================================

END_TS="$(now_ms)"
TOTAL_MS=$((END_TS - START_TS))

# Count passed tests
PASSED=$(grep -c " ... ok" "$TEST_OUTPUT" || echo "0")
STATUS=pass
if [[ "$PASSED" -lt 1 || "$NO_FALSE_POSITIVES" != true || "$SPIKE_DETECTION" != true \
      || "$EVALUE_NONNEGATIVE" != true || "$OPTIONAL_STOPPING" != true ]]; then
    STATUS=fail
fi

cat > "$OUTPUT_DIR/results_${TIMESTAMP}.jsonl" <<EOF
{"event":"test_complete","status":"$STATUS","total_ms":$TOTAL_MS,"build_ms":$BUILD_MS,"test_ms":$TEST_MS}
{"event":"tests","passed":$PASSED}
{"event":"properties","no_false_positives":$NO_FALSE_POSITIVES,"spike_detection":$SPIKE_DETECTION,"evalue_nonnegative":$EVALUE_NONNEGATIVE,"optional_stopping_valid":$OPTIONAL_STOPPING}
EOF

log_info "================================================"
if [[ "$STATUS" != pass ]]; then
    log_fail "Flake Detector E2E Test FAILED (tests passed: $PASSED)"
    exit 1
fi
log_success "Flake Detector E2E Test PASSED"
log_info "Tests passed: $PASSED"
log_info "Total time: ${TOTAL_MS}ms"
log_info "Results: $OUTPUT_DIR"
log_info "================================================"

exit 0

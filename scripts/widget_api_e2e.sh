#!/bin/bash
# Widget API E2E Test Script for FrankenTUI
# bd-34lz: Comprehensive verification of Widget API with detailed logging
#
# This script validates:
# 1. Workspace builds successfully
# 2. All unit tests pass
# 3. Clippy finds no warnings
# 4. All feature combinations compile
# 5. Documentation builds
# 6. Widget signatures use Frame (not Buffer)
# 7. Snapshot tests pass (if available)
#
# Usage:
#   ./scripts/widget_api_e2e.sh              # Run all tests
#   ./scripts/widget_api_e2e.sh --verbose    # Extra output
#   ./scripts/widget_api_e2e.sh --quick      # Skip slow steps
#   LOG_DIR=/path/to/logs ./scripts/widget_api_e2e.sh  # Custom log dir

set -euo pipefail

# ============================================================================
# Configuration
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
LOG_DIR="${LOG_DIR:-/tmp/widget_api_e2e_${TIMESTAMP}}"

VERBOSE=false
QUICK=false
STEP_COUNT=0
PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0

# Parse arguments
for arg in "$@"; do
    case $arg in
        --verbose|-v)
            VERBOSE=true
            ;;
        --quick|-q)
            QUICK=true
            ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--quick]"
            echo ""
            echo "Options:"
            echo "  --verbose, -v   Show detailed output during execution"
            echo "  --quick, -q     Skip slow steps (docs, some feature combos)"
            echo "  --help, -h      Show this help message"
            echo ""
            echo "Environment:"
            echo "  LOG_DIR         Directory for log files (default: /tmp/widget_api_e2e_TIMESTAMP)"
            exit 0
            ;;
    esac
done

# ============================================================================
# Logging Functions
# ============================================================================

log_info() {
    echo -e "\033[1;34m[INFO]\033[0m $*"
}

log_pass() {
    echo -e "\033[1;32m[PASS]\033[0m $*"
}

log_fail() {
    echo -e "\033[1;31m[FAIL]\033[0m $*"
}

log_skip() {
    echo -e "\033[1;33m[SKIP]\033[0m $*"
}

log_step() {
    STEP_COUNT=$((STEP_COUNT + 1))
    echo ""
    echo -e "\033[1;36m[$STEP_COUNT/$TOTAL_STEPS]\033[0m $*"
}

# ============================================================================
# Step Runner
# ============================================================================

run_step() {
    local step_name="$1"
    local log_file="$2"
    shift 2
    local cmd=("$@")

    log_step "$step_name"

    local start_time
    start_time=$(date +%s.%N)

    if $VERBOSE; then
        if "${cmd[@]}" 2>&1 | tee "$log_file"; then
            local end_time
            end_time=$(date +%s.%N)
            local duration
            duration=$(echo "$end_time - $start_time" | bc)
            log_pass "$step_name completed in ${duration}s"
            PASS_COUNT=$((PASS_COUNT + 1))
            return 0
        else
            log_fail "$step_name failed. See: $log_file"
            FAIL_COUNT=$((FAIL_COUNT + 1))
            return 1
        fi
    else
        if "${cmd[@]}" > "$log_file" 2>&1; then
            local end_time
            end_time=$(date +%s.%N)
            local duration
            duration=$(echo "$end_time - $start_time" | bc)
            log_pass "$step_name completed in ${duration}s"
            PASS_COUNT=$((PASS_COUNT + 1))
            return 0
        else
            log_fail "$step_name failed. See: $log_file"
            FAIL_COUNT=$((FAIL_COUNT + 1))
            return 1
        fi
    fi
}

skip_step() {
    local step_name="$1"
    log_step "$step_name"
    log_skip "Skipped (--quick mode)"
    SKIP_COUNT=$((SKIP_COUNT + 1))
}

# ============================================================================
# Main Script
# ============================================================================

TOTAL_STEPS=7
if $QUICK; then
    TOTAL_STEPS=5
fi

echo "=============================================="
echo "  Widget API E2E Test Suite"
echo "=============================================="
echo ""
echo "Project root: $PROJECT_ROOT"
echo "Log directory: $LOG_DIR"
echo "Started at: $(date -Iseconds)"
echo "Mode: ${QUICK:+quick}${VERBOSE:+verbose}${QUICK:-${VERBOSE:-normal}}"

mkdir -p "$LOG_DIR"
cd "$PROJECT_ROOT"

# Record environment info
{
    echo "Environment Information"
    echo "======================="
    echo "Date: $(date -Iseconds)"
    echo "User: $(whoami)"
    echo "Hostname: $(hostname)"
    echo "Working directory: $(pwd)"
    echo "Rust version: $(rustc --version 2>/dev/null || echo 'N/A')"
    echo "Cargo version: $(cargo --version 2>/dev/null || echo 'N/A')"
    echo ""
    echo "Git status:"
    git status --short 2>/dev/null || echo "Not a git repo"
    echo ""
    echo "Git commit:"
    git log -1 --oneline 2>/dev/null || echo "N/A"
} > "$LOG_DIR/00_environment.log"

# Step 1: Workspace Build
run_step "Building workspace" "$LOG_DIR/01_build.log" \
    cargo build --workspace

# Step 2: Unit Tests
run_step "Running unit tests" "$LOG_DIR/02_tests.log" \
    cargo test --workspace --lib -- --test-threads=4

# Step 3: Clippy
run_step "Running clippy" "$LOG_DIR/03_clippy.log" \
    cargo clippy --workspace --all-targets -- -D warnings

# Step 4: Feature Combinations
log_step "Testing feature combinations"
{
    echo "Feature combination tests - $(date -Iseconds)"
    echo ""

    # ftui-extras features
    EXTRAS_FEATURES=("canvas" "charts" "forms" "markdown" "export" "clipboard" "syntax" "image")

    for feature in "${EXTRAS_FEATURES[@]}"; do
        echo "Testing ftui-extras --features $feature ..."
        if cargo check -p ftui-extras --features "$feature" 2>&1; then
            echo "  [PASS] $feature"
        else
            echo "  [FAIL] $feature"
            exit 1
        fi
    done

    echo ""
    echo "Testing ftui-widgets with debug-overlay feature..."
    if cargo check -p ftui-widgets --features debug-overlay 2>&1; then
        echo "  [PASS] debug-overlay"
    else
        echo "  [FAIL] debug-overlay"
        exit 1
    fi

    echo ""
    echo "All feature combinations passed!"

} > "$LOG_DIR/04_features.log" 2>&1 && {
    log_pass "Feature combinations passed"
    PASS_COUNT=$((PASS_COUNT + 1))
} || {
    log_fail "Feature combinations failed. See: $LOG_DIR/04_features.log"
    FAIL_COUNT=$((FAIL_COUNT + 1))
}

# Step 5: Widget Signature Verification
log_step "Verifying Widget signatures"
{
    echo "Widget signature verification - $(date -Iseconds)"
    echo ""

    WIDGET_DIR="$PROJECT_ROOT/crates/ftui-widgets/src"

    echo "Checking for old Widget trait Buffer signatures..."
    # Only match the Widget trait render signature pattern: fn render(&self, area: Rect, buf:
    # Helper methods that take Buffer directly (like render_borders) are expected and allowed.
    OLD_SIGS=$(grep -rn 'fn render(&self, area: Rect, buf: &mut Buffer)' "$WIDGET_DIR"/*.rs 2>/dev/null || true)

    if [ -n "$OLD_SIGS" ]; then
        echo "ERROR: Found old Widget trait Buffer signatures:"
        echo "$OLD_SIGS"
        exit 1
    else
        echo "  No old Widget trait Buffer signatures found"
        echo "  (Helper methods using Buffer directly are allowed)"
    fi

    echo ""
    echo "Checking for new Frame signatures..."
    NEW_SIGS=$(grep -rn 'fn render.*frame: &mut Frame' "$WIDGET_DIR"/*.rs 2>/dev/null || true)

    if [ -z "$NEW_SIGS" ]; then
        echo "WARNING: No Frame signatures found (might be empty or different pattern)"
    else
        echo "Found $(echo "$NEW_SIGS" | wc -l) Frame signatures:"
        echo "$NEW_SIGS"
    fi

    echo ""
    echo "Signature verification passed!"

} > "$LOG_DIR/05_signatures.log" 2>&1 && {
    log_pass "Widget signatures verified (Frame-based API)"
    PASS_COUNT=$((PASS_COUNT + 1))
} || {
    log_fail "Widget signature check failed. See: $LOG_DIR/05_signatures.log"
    FAIL_COUNT=$((FAIL_COUNT + 1))
}

# Step 6: Documentation Build (skip in quick mode)
if $QUICK; then
    skip_step "Building documentation (skipped)"
else
    run_step "Building documentation" "$LOG_DIR/06_docs.log" \
        cargo doc --workspace --no-deps
fi

# Step 7: Snapshot Tests (skip in quick mode)
if $QUICK; then
    skip_step "Running snapshot tests (skipped)"
else
    log_step "Running snapshot tests"
    if [ -f "$PROJECT_ROOT/crates/ftui-harness/tests/widget_snapshots.rs" ]; then
        if cargo test -p ftui-harness --test widget_snapshots > "$LOG_DIR/07_snapshots.log" 2>&1; then
            log_pass "Snapshot tests passed"
            PASS_COUNT=$((PASS_COUNT + 1))
        else
            log_fail "Snapshot tests failed. See: $LOG_DIR/07_snapshots.log"
            FAIL_COUNT=$((FAIL_COUNT + 1))
        fi
    else
        log_skip "Snapshot tests not found"
        SKIP_COUNT=$((SKIP_COUNT + 1))
    fi
fi

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "=============================================="
echo "  E2E Test Suite Complete"
echo "=============================================="
echo ""
echo "Ended at: $(date -Iseconds)"
echo "Log directory: $LOG_DIR"
echo ""
echo "Results:"
echo "  Passed: $PASS_COUNT"
echo "  Failed: $FAIL_COUNT"
echo "  Skipped: $SKIP_COUNT"
echo ""

# List log files with sizes
echo "Log files:"
ls -lh "$LOG_DIR"/*.log 2>/dev/null | awk '{print "  " $9 " (" $5 ")"}'

echo ""

# Generate summary file
{
    echo "E2E Test Summary"
    echo "================"
    echo "Date: $(date -Iseconds)"
    echo "Passed: $PASS_COUNT"
    echo "Failed: $FAIL_COUNT"
    echo "Skipped: $SKIP_COUNT"
    echo ""
    echo "Exit code: $( [ $FAIL_COUNT -eq 0 ] && echo 0 || echo 1 )"
} > "$LOG_DIR/SUMMARY.txt"

if [ $FAIL_COUNT -eq 0 ]; then
    echo -e "\033[1;32mAll tests passed!\033[0m"
    exit 0
else
    echo -e "\033[1;31m$FAIL_COUNT test(s) failed!\033[0m"
    exit 1
fi

#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

VERBOSE=false
QUICK=false

for arg in "$@"; do
    case "$arg" in
        --verbose|-v)
            VERBOSE=true
            LOG_LEVEL="DEBUG"
            ;;
        --quick|-q)
            QUICK=true
            ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--quick]"
            echo ""
            echo "Options:"
            echo "  --verbose, -v   Enable debug logging"
            echo "  --quick, -q     Run only core tests (inline + cleanup)"
            echo "  --help, -h      Show this help"
            exit 0
            ;;
    esac
done

TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_${TIMESTAMP}}"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
LOG_FILE="$E2E_LOG_DIR/e2e.log"

E2E_RESULTS_DIR="$E2E_LOG_DIR/results"

export E2E_LOG_DIR E2E_RESULTS_DIR LOG_FILE LOG_LEVEL
export E2E_RUN_START_MS="$(date +%s%3N)"

# Clean results from any previous run
rm -rf "$E2E_RESULTS_DIR"
mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"

log_info "FrankenTUI E2E Test Suite"
log_info "Project root: $PROJECT_ROOT"
log_info "Log directory: $E2E_LOG_DIR"
log_info "Results directory: $E2E_RESULTS_DIR"
log_info "Mode: $([ "$QUICK" = true ] && echo quick || echo normal)"

# Environment info
{
    echo "Environment Information"
    echo "======================="
    echo "Date: $(date -Iseconds)"
    echo "User: $(whoami)"
    echo "Hostname: $(hostname)"
    echo "Working directory: $(pwd)"
    echo "Rust version: $(rustc --version 2>/dev/null || echo 'N/A')"
    echo "Cargo version: $(cargo --version 2>/dev/null || echo 'N/A')"
    echo "Git status:"
    git status --short 2>/dev/null || echo "Not a git repo"
    echo "Git commit:"
    git log -1 --oneline 2>/dev/null || echo "N/A"
} > "$E2E_LOG_DIR/00_environment.log"

# Requirements
require_cmd cargo
if [[ -z "$E2E_PYTHON" ]]; then
    log_error "python3/python is required for PTY helpers"
    exit 1
fi

log_info "Building ftui-harness..."
if $VERBOSE; then
    cargo build -p ftui-harness 2>&1 | tee "$E2E_LOG_DIR/01_build.log"
else
    cargo build -p ftui-harness > "$E2E_LOG_DIR/01_build.log" 2>&1
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
E2E_HARNESS_BIN="$TARGET_DIR/debug/ftui-harness"
export E2E_HARNESS_BIN

if [[ ! -x "$E2E_HARNESS_BIN" ]]; then
    log_error "ftui-harness binary not found at $E2E_HARNESS_BIN"
    exit 1
fi

# Track overall failures (don't exit on first failure)
SUITE_FAILURES=0

run_suite() {
    local suite_name="$1"
    local suite_script="$2"
    log_info "--- Suite: $suite_name ---"
    if "$suite_script"; then
        log_info "Suite $suite_name: all cases passed"
    else
        log_error "Suite $suite_name: one or more cases failed"
        SUITE_FAILURES=$((SUITE_FAILURES + 1))
    fi
}

log_info "Running tests..."

# Core suites (always run)
run_suite "inline"  "$SCRIPT_DIR/test_inline.sh"
run_suite "cleanup" "$SCRIPT_DIR/test_cleanup.sh"

if $QUICK; then
    log_warn "Skipping extended tests (--quick)"
else
    run_suite "altscreen" "$SCRIPT_DIR/test_altscreen.sh"
    run_suite "input"     "$SCRIPT_DIR/test_input.sh"
    run_suite "ansi"      "$SCRIPT_DIR/test_ansi.sh"
    run_suite "unicode"   "$SCRIPT_DIR/test_unicode.sh"
fi

# Finalize JSON summary
SUMMARY_JSON="$E2E_RESULTS_DIR/summary.json"
finalize_summary "$SUMMARY_JSON"

# Print human-readable summary
log_info "========================================"
log_info "E2E RESULTS SUMMARY"
log_info "========================================"

if command -v jq >/dev/null 2>&1 && [[ -f "$SUMMARY_JSON" ]]; then
    TOTAL=$(jq '.total' "$SUMMARY_JSON")
    PASSED=$(jq '.passed' "$SUMMARY_JSON")
    FAILED=$(jq '.failed' "$SUMMARY_JSON")
    SKIPPED=$(jq '.skipped' "$SUMMARY_JSON")
    DURATION=$(jq '.duration_ms' "$SUMMARY_JSON")

    log_info "Total: $TOTAL  Passed: $PASSED  Failed: $FAILED  Skipped: $SKIPPED"
    log_info "Duration: ${DURATION}ms"

    # List failed tests with details
    if [[ "$FAILED" -gt 0 ]]; then
        log_error "Failed tests:"
        jq -r '.tests[] | select(.status=="failed") | "  - \(.name): \(.error // "unknown")"' "$SUMMARY_JSON"

        # Produce hex dumps for failed test PTY captures
        log_error "PTY capture hex dumps for failed tests:"
        for pty_file in "$E2E_LOG_DIR"/*.pty; do
            [[ -f "$pty_file" ]] || continue
            base="$(basename "$pty_file" .pty)"
            # Check if this test failed
            if jq -e ".tests[] | select(.name==\"$base\" and .status==\"failed\")" "$SUMMARY_JSON" >/dev/null 2>&1; then
                log_error "--- $base PTY capture (first 512 bytes hex) ---"
                xxd -l 512 "$pty_file" >> "$LOG_FILE" 2>&1 || true
                log_error "--- $base PTY capture (printable) ---"
                strings -n 3 "$pty_file" | head -20 >> "$LOG_FILE" 2>&1 || true
            fi
        done
    fi
else
    log_info "Results directory: $E2E_RESULTS_DIR"
    log_info "(install jq for detailed summary)"
fi

log_info "E2E summary: $SUMMARY_JSON"
log_info "E2E logs: $E2E_LOG_DIR"

if [[ "$SUITE_FAILURES" -gt 0 ]]; then
    log_error "$SUITE_FAILURES suite(s) had failures"
    exit 1
fi

log_info "All suites passed"
exit 0

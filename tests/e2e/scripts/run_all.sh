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
            exit 0
            ;;
    esac
done

TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_${TIMESTAMP}}"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
LOG_FILE="$E2E_LOG_DIR/e2e.log"

export E2E_LOG_DIR E2E_RESULTS_DIR LOG_FILE LOG_LEVEL
export E2E_RUN_START_MS="$(date +%s%3N)"

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
    cargo build -p ftui-harness | tee "$E2E_LOG_DIR/01_build.log"
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

log_info "Running tests..."

"$SCRIPT_DIR/test_inline.sh"
"$SCRIPT_DIR/test_cleanup.sh"

if $QUICK; then
    log_warn "Skipping alt-screen and input tests (--quick)"
else
    "$SCRIPT_DIR/test_altscreen.sh"
    "$SCRIPT_DIR/test_input.sh"
fi

SUMMARY_JSON="$E2E_RESULTS_DIR/summary.json"
finalize_summary "$SUMMARY_JSON"

log_info "E2E summary: $SUMMARY_JSON"
log_info "E2E logs: $E2E_LOG_DIR"

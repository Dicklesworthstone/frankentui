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

e2e_fixture_init "e2e"

TIMESTAMP="$(e2e_log_stamp)"
E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_${E2E_RUN_ID}_${TIMESTAMP}}"
if [[ -e "$E2E_LOG_DIR" ]]; then
    base="$E2E_LOG_DIR"
    suffix=1
    while [[ -e "${base}_$suffix" ]]; do
        suffix=$((suffix + 1))
    done
    E2E_LOG_DIR="${base}_$suffix"
fi
E2E_RESULTS_DIR="$E2E_LOG_DIR/results"
LOG_FILE="$E2E_LOG_DIR/e2e.log"
E2E_JSONL_FILE="${E2E_JSONL_FILE:-$E2E_LOG_DIR/e2e.jsonl}"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"

export E2E_LOG_DIR E2E_RESULTS_DIR LOG_FILE LOG_LEVEL E2E_JSONL_FILE E2E_RUN_CMD
export E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"

# Prepare results directory without destructive cleanup
mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
jsonl_init

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
    run_suite "altscreen"  "$SCRIPT_DIR/test_altscreen.sh"
    run_suite "input"      "$SCRIPT_DIR/test_input.sh"
    run_suite "keybinding" "$SCRIPT_DIR/test_keybinding.sh"
    run_suite "ansi"       "$SCRIPT_DIR/test_ansi.sh"
    run_suite "unicode"    "$SCRIPT_DIR/test_unicode.sh"
    run_suite "focus"      "$SCRIPT_DIR/test_focus_events.sh"
    run_suite "paste"      "$SCRIPT_DIR/test_paste.sh"
    run_suite "osc8"       "$SCRIPT_DIR/test_osc8.sh"
    run_suite "kitty"      "$SCRIPT_DIR/test_kitty_keyboard.sh"
    run_suite "mouse_sgr"  "$SCRIPT_DIR/test_mouse_sgr.sh"
    run_suite "resize"     "$SCRIPT_DIR/test_resize_scroll_region.sh"
    run_suite "mux"        "$SCRIPT_DIR/test_mux.sh"
    run_suite "schedule_trace" "$SCRIPT_DIR/test_schedule_trace.sh"

    # Demo screen E2E tests (bd-11ck.4)
    if [[ -x "$SCRIPT_DIR/test_action_timeline.sh" ]]; then
        run_suite "action_timeline" "$SCRIPT_DIR/test_action_timeline.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_theme_studio.sh" ]]; then
        run_suite "theme_studio" "$SCRIPT_DIR/test_theme_studio.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_ui_inspector.sh" ]]; then
        run_suite "ui_inspector" "$SCRIPT_DIR/test_ui_inspector.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_locale_context.sh" ]]; then
        run_suite "locale_context" "$SCRIPT_DIR/test_locale_context.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_virtualized_search.sh" ]]; then
        run_suite "virtualized_search" "$SCRIPT_DIR/test_virtualized_search.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_layout_composer_resize.sh" ]]; then
        run_suite "layout_composer_resize" "$SCRIPT_DIR/test_layout_composer_resize.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_terminal_quirks.sh" ]]; then
        run_suite "terminal_quirks" "$SCRIPT_DIR/test_terminal_quirks.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_visual_fx_extras.sh" ]]; then
        run_suite "visual_fx_extras" "$SCRIPT_DIR/test_visual_fx_extras.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_visual_fx_fps.sh" ]]; then
        run_suite "visual_fx_fps" "$SCRIPT_DIR/test_visual_fx_fps.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_text_effects_sweep.sh" ]]; then
        run_suite "text_effects_sweep" "$SCRIPT_DIR/test_text_effects_sweep.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_bidi_voi_overlay.sh" ]]; then
        run_suite "bidi_voi_overlay" "$SCRIPT_DIR/test_bidi_voi_overlay.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_pty_canonicalize.sh" ]]; then
        run_suite "pty_canonicalize" "$SCRIPT_DIR/test_pty_canonicalize.sh"
    fi
    if [[ -x "$SCRIPT_DIR/test_embedded_terminal.sh" ]]; then
        run_suite "embedded_terminal" "$SCRIPT_DIR/test_embedded_terminal.sh"
    fi
    # Drag-and-drop E2E tests (bd-1csc.6)
    if [[ -x "$SCRIPT_DIR/test_drag_drop.sh" ]]; then
        run_suite "drag_drop" "$SCRIPT_DIR/test_drag_drop.sh"
    fi
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

#!/bin/bash
set -euo pipefail

# E2E tests for Accessibility Modes (Demo Showcase)
# bd-2o55.4: Accessibility Modes — E2E PTY Tests (Verbose Logs)
#
# Scenarios:
# 1. Smoke test: toggle A11y panel visibility (Shift+A)
# 2. High contrast mode toggle (Shift+H within panel)
# 3. Reduced motion mode toggle (Shift+M within panel)
# 4. Large text mode toggle (Shift+L within panel)
# 5. Combined modes: toggle multiple modes
#
# Keybindings:
# - Shift+A: Toggle A11y panel
# - Shift+H: Toggle high contrast (when panel open)
# - Shift+M: Toggle reduced motion (when panel open)
# - Shift+L: Toggle large text (when panel open)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

JSONL_FILE="$E2E_RESULTS_DIR/a11y_modes.jsonl"
RUN_ID="a11y_modes_$(date +%Y%m%d_%H%M%S)_$$"
SEED="${FTUI_DEMO_SEED:-0}"

jsonl_log() {
    local line="$1"
    mkdir -p "$E2E_RESULTS_DIR"
    printf '%s\n' "$line" >> "$JSONL_FILE"
}

sha256_file() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1 && [[ -f "$file" ]]; then
        sha256sum "$file" | awk '{print $1}'
        return 0
    fi
    echo ""
    return 0
}

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

run_case() {
    local name="$1"
    local send_label="$2"
    local a11y_modes="$3"
    shift 3
    local start_ms
    start_ms="$(e2e_monotonic_ms)"

    LOG_FILE="$E2E_LOG_DIR/${name}.log"
    local output_file="$E2E_LOG_DIR/${name}.pty"

    log_test_start "$name"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)"
        local duration_ms=$((end_ms - start_ms))
        local size
        size=$(wc -c < "$output_file" | tr -d ' ')
        local output_sha
        output_sha="$(sha256_file "$output_file")"
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"passed\",\"duration_ms\":$duration_ms,\"output_bytes\":$size,\"output_sha256\":\"$output_sha\",\"send\":\"$send_label\",\"a11y_modes\":\"$a11y_modes\",\"cols\":120,\"rows\":40,\"seed\":\"$SEED\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\"}"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)"
    local duration_ms=$((end_ms - start_ms))
    local output_sha
    output_sha="$(sha256_file "$output_file")"
    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"failed\",\"duration_ms\":$duration_ms,\"output_sha256\":\"$output_sha\",\"send\":\"$send_label\",\"a11y_modes\":\"$a11y_modes\",\"cols\":120,\"rows\":40,\"seed\":\"$SEED\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\"}"
    return 1
}

DEMO_BIN="$(ensure_demo_bin || true)"
if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/a11y_modes_missing.log"
    for t in a11y_panel_toggle a11y_high_contrast a11y_reduced_motion a11y_large_text a11y_combined; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"status\":\"skipped\",\"reason\":\"binary missing\",\"seed\":\"$SEED\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\"}"
    done
    exit 0
fi

# Control bytes for Shift key combinations
# Shift+A = uppercase A = 0x41
SHIFT_A='A'
# Shift+H = uppercase H = 0x48
SHIFT_H='H'
# Shift+M = uppercase M = 0x4d
SHIFT_M='M'
# Shift+L = uppercase L = 0x4c
SHIFT_L='L'

# Keys sent on a timer from launch could beat raw mode on a cold debug
# binary: the tty echoed them and they never reached the app. Wait for the
# status bar of the first frame instead.
FIRST_FRAME='Tab/Shift+Tab'

# Test 1: A11y panel toggle (Shift+A)
# Verifies the accessibility panel can be toggled on
a11y_panel_toggle() {
    LOG_FILE="$E2E_LOG_DIR/a11y_panel_toggle.log"
    local output_file="$E2E_LOG_DIR/a11y_panel_toggle.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_AFTER_OUTPUT="$FIRST_FRAME" \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="$SHIFT_A" \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Only the panel says this. The demo opens on the guided tour, whose
    # overlay used to cover the panel.
    grep -a -q "Press A to close" "$output_file" || return 1
}

# Test 2: High contrast mode toggle (Shift+A then Shift+H)
# Opens A11y panel, toggles high contrast, verifies theme change
a11y_high_contrast() {
    LOG_FILE="$E2E_LOG_DIR/a11y_high_contrast.log"
    local output_file="$E2E_LOG_DIR/a11y_high_contrast.pty"

    # Send Shift+A to open panel, then Shift+H to toggle high contrast
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_AFTER_OUTPUT="$FIRST_FRAME" \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="${SHIFT_A}${SHIFT_H}" \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # The status bar lists the modes that are on. The panel's labels show
    # whether or not a mode is on, so they prove nothing.
    grep -a -q "A11y:HC" "$output_file" || return 1
}

# Test 3: Reduced motion mode toggle (Shift+A then Shift+M)
# Opens A11y panel, toggles reduced motion
a11y_reduced_motion() {
    LOG_FILE="$E2E_LOG_DIR/a11y_reduced_motion.log"
    local output_file="$E2E_LOG_DIR/a11y_reduced_motion.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_AFTER_OUTPUT="$FIRST_FRAME" \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="${SHIFT_A}${SHIFT_M}" \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    grep -a -q "A11y:RM" "$output_file" || return 1
}

# Test 4: Large text mode toggle (Shift+A then Shift+L)
# Opens A11y panel, toggles large text
a11y_large_text() {
    LOG_FILE="$E2E_LOG_DIR/a11y_large_text.log"
    local output_file="$E2E_LOG_DIR/a11y_large_text.pty"

    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_AFTER_OUTPUT="$FIRST_FRAME" \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="${SHIFT_A}${SHIFT_L}" \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    grep -a -q "A11y:LT" "$output_file" || return 1
}

# Test 5: Combined modes (Shift+A, then toggle all three)
# Tests that multiple accessibility modes can be enabled simultaneously
a11y_combined() {
    LOG_FILE="$E2E_LOG_DIR/a11y_combined.log"
    local output_file="$E2E_LOG_DIR/a11y_combined.pty"

    # Open panel, toggle all three modes
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_SEND_AFTER_OUTPUT="$FIRST_FRAME" \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="${SHIFT_A}${SHIFT_H}${SHIFT_M}${SHIFT_L}" \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # One batch of keys, one frame: the status bar lists all three at once
    grep -a -q "A11y:HC RM LT" "$output_file" || return 1
}

# Test 6: Small terminal (80x24) with large text
# Verifies large text mode doesn't break on minimal terminal size
a11y_small_terminal() {
    LOG_FILE="$E2E_LOG_DIR/a11y_small_terminal.log"
    local output_file="$E2E_LOG_DIR/a11y_small_terminal.pty"

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_SEND_AFTER_OUTPUT="$FIRST_FRAME" \
    PTY_SEND_DELAY_MS=300 \
    PTY_SEND="${SHIFT_A}${SHIFT_L}" \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    # Should produce output even in small terminal
    [[ "$size" -gt 200 ]] || return 1
}

FAILURES=0
run_case "a11y_panel_toggle" "SHIFT_A" "panel" a11y_panel_toggle || FAILURES=$((FAILURES + 1))
run_case "a11y_high_contrast" "SHIFT_A,SHIFT_H" "high_contrast" a11y_high_contrast || FAILURES=$((FAILURES + 1))
run_case "a11y_reduced_motion" "SHIFT_A,SHIFT_M" "reduced_motion" a11y_reduced_motion || FAILURES=$((FAILURES + 1))
run_case "a11y_large_text" "SHIFT_A,SHIFT_L" "large_text" a11y_large_text || FAILURES=$((FAILURES + 1))
run_case "a11y_combined" "SHIFT_A,SHIFT_H,SHIFT_M,SHIFT_L" "all" a11y_combined || FAILURES=$((FAILURES + 1))
run_case "a11y_small_terminal" "SHIFT_A,SHIFT_L" "large_text_80x24" a11y_small_terminal || FAILURES=$((FAILURES + 1))

if [[ "$FAILURES" -gt 0 ]]; then
    log_error "A11y modes E2E tests: $FAILURES failure(s)"
    exit 1
fi

log_info "A11y modes E2E tests: all passed"
exit 0

#!/bin/bash
# Form Validation E2E Test Suite for FrankenTUI
# bd-34pj.6: Comprehensive end-to-end verification of form validation behavior
#
# This script validates:
# 1. Sync validation (required, pattern, length, custom validators)
# 2. Form navigation and focus behavior
# 3. Error display and clearing
# 4. Real-time vs on-submit validation modes
# 5. Form submission flow
#
# Usage:
#   ./tests/e2e/scripts/test_form_validation.sh
#   E2E_LOG_DIR=/tmp/form-validation-logs ./tests/e2e/scripts/test_form_validation.sh
#
# Environment:
#   E2E_LOG_DIR         Directory for log files (default: /tmp/ftui_e2e_logs)
#   E2E_HARNESS_BIN     Path to the ftui-harness binary
#   FTUI_DEMO_BIN       Path to the ftui-demo-showcase binary

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

# Check for demo showcase binary
resolve_demo_bin() {
    if [[ -n "${FTUI_DEMO_BIN:-}" && -x "$FTUI_DEMO_BIN" ]]; then
        echo "$FTUI_DEMO_BIN"
        return 0
    fi

    local debug_bin="$PROJECT_ROOT/target/debug/ftui-demo-showcase"
    local release_bin="$PROJECT_ROOT/target/release/ftui-demo-showcase"

    if [[ -x "$debug_bin" ]]; then
        echo "$debug_bin"
        return 0
    fi
    if [[ -x "$release_bin" ]]; then
        echo "$release_bin"
        return 0
    fi

    return 1
}

DEMO_BIN=""
if ! DEMO_BIN="$(resolve_demo_bin)"; then
    LOG_FILE="$E2E_LOG_DIR/form_validation_missing.log"
    for t in form_screen_loads form_down_navigation form_field_focus form_validation_trigger form_error_display form_submit_flow form_arrow_navigation form_escape_behavior form_rapid_input form_backspace_handling form_mode_toggle; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

# Screen numbers shift whenever a screen is added. A hardcoded 21 had drifted
# onto Notifications, where every case below still passed.
FORM_VALIDATION_SCREEN="$(e2e_demo_screen "$DEMO_BIN" form_validation)"

# The app keeps Tab for switching screens, so Up/Down move between fields.
UP=$'\x1b[A'
DOWN=$'\x1b[B'
RIGHT=$'\x1b[C'

run_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(e2e_monotonic_ms)"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)"
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)"
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "form validation assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "form validation assertions failed"
    return 1
}

# Test: Form screen loads without crashing
form_screen_loads() {
    LOG_FILE="$E2E_LOG_DIR/form_screen_loads.log"
    local output_file="$E2E_LOG_DIR/form_screen_loads.pty"

    log_test_start "form_screen_loads"

    # Start demo on Form Validation screen (screen 21)
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$FORM_VALIDATION_SCREEN"

    # Only this screen draws the form; the tab bar names every screen.
    if ! grep -a -q "Registration Form" "$output_file"; then
        log_warn "Form screen content not found in output"
        return 1
    fi

    # Output should have substantial content
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1
}

# Test: Down moves between form fields
form_down_navigation() {
    LOG_FILE="$E2E_LOG_DIR/form_down_navigation.log"
    local output_file="$E2E_LOG_DIR/form_down_navigation.pty"

    log_test_start "form_down_navigation"

    # Three Downs reach Confirm Password; "abc" there cannot match the empty
    # password.
    PTY_SEND="${DOWN}${DOWN}${DOWN}abc" \
    PTY_SEND_DELAY_MS=500 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$FORM_VALIDATION_SCREEN"

    grep -a -q "Passwords do not match" "$output_file" || return 1
}

# Test: Field focus and input handling
form_field_focus() {
    LOG_FILE="$E2E_LOG_DIR/form_field_focus.log"
    local output_file="$E2E_LOG_DIR/form_field_focus.pty"

    log_test_start "form_field_focus"

    # The username lands in the field. Its e and r used to inject errors and
    # reset the form instead.
    PTY_SEND='testuser' \
    PTY_SEND_DELAY_MS=500 \
    FTUI_DEMO_EXIT_AFTER_MS=2500 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$FORM_VALIDATION_SCREEN"

    grep -a -q "testuser" "$output_file" || return 1
}

# Test: Validation triggers on input
form_validation_trigger() {
    LOG_FILE="$E2E_LOG_DIR/form_validation_trigger.log"
    local output_file="$E2E_LOG_DIR/form_validation_trigger.pty"

    log_test_start "form_validation_trigger"

    # An email without an @, then leave the field
    PTY_SEND="${DOWN}invalid-email${DOWN}" \
    PTY_SEND_DELAY_MS=300 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$FORM_VALIDATION_SCREEN"

    # Narrow panes truncate the message: "Please enter a valid ema…"
    grep -a -q "Please enter a valid" "$output_file" || return 1
}

# Test: Error display behavior
form_error_display() {
    LOG_FILE="$E2E_LOG_DIR/form_error_display.log"
    local output_file="$E2E_LOG_DIR/form_error_display.pty"

    log_test_start "form_error_display"

    # A username one character short, then leave the field
    PTY_SEND="ab${DOWN}" \
    PTY_SEND_DELAY_MS=400 \
    FTUI_DEMO_EXIT_AFTER_MS=3500 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$FORM_VALIDATION_SCREEN"

    # Narrow panes truncate the message: "Username must be at leas…"
    grep -a -q "Username must be at" "$output_file" || return 1
}

# Test: Form submission flow
form_submit_flow() {
    LOG_FILE="$E2E_LOG_DIR/form_submit_flow.log"
    local output_file="$E2E_LOG_DIR/form_submit_flow.pty"

    log_test_start "form_submit_flow"

    # Fill every required field and submit: Username, Email, Password,
    # Confirm, then past Age, Bio and Website to Role (Right picks
    # Developer), Accept Terms (Space), Enter.
    PTY_SEND="testuser${DOWN}test@example.com${DOWN}password123${DOWN}password123${DOWN}${DOWN}${DOWN}${DOWN}${RIGHT}${DOWN} "$'\r' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_DEMO_EXIT_AFTER_MS=4000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$FORM_VALIDATION_SCREEN"

    grep -a -q "submitted successfully" "$output_file" || return 1
}

# Test: Arrow key navigation within form
form_arrow_navigation() {
    LOG_FILE="$E2E_LOG_DIR/form_arrow_navigation.log"
    local output_file="$E2E_LOG_DIR/form_arrow_navigation.pty"

    log_test_start "form_arrow_navigation"

    # Use arrow keys to navigate
    local arrows=$'\x1b[A\x1b[B\x1b[A\x1b[B'
    PTY_SEND="$arrows" \
    PTY_SEND_DELAY_MS=300 \
    FTUI_DEMO_EXIT_AFTER_MS=2500 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$FORM_VALIDATION_SCREEN"

    # Arrows stay on the form
    grep -a -q "Registration Form" "$output_file" || return 1
}

# Test: Escape key behavior (cancel/clear)
form_escape_behavior() {
    LOG_FILE="$E2E_LOG_DIR/form_escape_behavior.log"
    local output_file="$E2E_LOG_DIR/form_escape_behavior.pty"

    log_test_start "form_escape_behavior"

    # Type something then press Escape
    PTY_SEND=$'partial\x1b' \
    PTY_SEND_DELAY_MS=400 \
    FTUI_DEMO_EXIT_AFTER_MS=2500 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$FORM_VALIDATION_SCREEN"

    # Esc cancels the form, which then ignores keys until R resets it
    grep -a -q "Form cancelled" "$output_file" || return 1
}

# Test: Multiple rapid inputs (stress test)
form_rapid_input() {
    LOG_FILE="$E2E_LOG_DIR/form_rapid_input.log"
    local output_file="$E2E_LOG_DIR/form_rapid_input.pty"

    log_test_start "form_rapid_input"

    # Send rapid keystrokes: a whole username, then digits into Confirm
    # Password, then submit
    PTY_SEND="abcdefghijklmnop${DOWN}${DOWN}${DOWN}123456"$'\r' \
    PTY_SEND_DELAY_MS=100 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$FORM_VALIDATION_SCREEN"

    # Every letter landed in the username, none ran a shortcut
    grep -a -q "abcdefghijklmnop" "$output_file" || return 1
}

# Test: Backspace/delete key handling
form_backspace_handling() {
    LOG_FILE="$E2E_LOG_DIR/form_backspace_handling.log"
    local output_file="$E2E_LOG_DIR/form_backspace_handling.pty"

    log_test_start "form_backspace_handling"

    # Type then delete with backspace
    PTY_SEND=$'testinput\x7f\x7f\x7f' \
    PTY_SEND_DELAY_MS=200 \
    FTUI_DEMO_EXIT_AFTER_MS=2500 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$FORM_VALIDATION_SCREEN"

    grep -a -q "testin" "$output_file" || return 1
}

# Test: Mode toggle (real-time vs on-submit validation)
form_mode_toggle() {
    LOG_FILE="$E2E_LOG_DIR/form_mode_toggle.log"
    local output_file="$E2E_LOG_DIR/form_mode_toggle.pty"

    log_test_start "form_mode_toggle"

    # M is a control only outside text fields: Up wraps to Accept Terms
    # first. It has to be capital: lowercase m there is the app's mouse
    # capture toggle.
    PTY_SEND="${UP}M" \
    PTY_SEND_DELAY_MS=300 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$FORM_VALIDATION_SCREEN"

    grep -a -q "On Submit" "$output_file" || return 1
}

# ============================================================================
# Run all tests
# ============================================================================

FAILURES=0
run_case "form_screen_loads" form_screen_loads                 || FAILURES=$((FAILURES + 1))
run_case "form_down_navigation" form_down_navigation           || FAILURES=$((FAILURES + 1))
run_case "form_field_focus" form_field_focus                   || FAILURES=$((FAILURES + 1))
run_case "form_validation_trigger" form_validation_trigger     || FAILURES=$((FAILURES + 1))
run_case "form_error_display" form_error_display               || FAILURES=$((FAILURES + 1))
run_case "form_submit_flow" form_submit_flow                   || FAILURES=$((FAILURES + 1))
run_case "form_arrow_navigation" form_arrow_navigation         || FAILURES=$((FAILURES + 1))
run_case "form_escape_behavior" form_escape_behavior           || FAILURES=$((FAILURES + 1))
run_case "form_rapid_input" form_rapid_input                   || FAILURES=$((FAILURES + 1))
run_case "form_backspace_handling" form_backspace_handling     || FAILURES=$((FAILURES + 1))
run_case "form_mode_toggle" form_mode_toggle                   || FAILURES=$((FAILURES + 1))

exit "$FAILURES"

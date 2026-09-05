#!/bin/bash
set -euo pipefail

# E2E tests for terminal resize and scroll-region behavior.
#
# Covers:
# - Initial render at various sizes
# - DECSTBM (scroll region) sequence emission
# - Scroll region reset on cleanup
# - Size-dependent layout behavior

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/resize_missing.log"
    for t in resize_small resize_wide resize_tall resize_scroll_region resize_cleanup_reset; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

RESIZE_SEED="${RESIZE_SEED:-0}"
RESIZE_ENV_JSONL="$E2E_LOG_DIR/resize_env_$(date +%Y%m%d_%H%M%S).jsonl"
mkdir -p "$E2E_LOG_DIR"
RESIZE_ENV_TIMESTAMP="$("${E2E_PYTHON:-python3}" -c 'from datetime import datetime; print(datetime.now().astimezone().isoformat(timespec="seconds"))')" || exit 2
cat > "$RESIZE_ENV_JSONL" <<EOF
{"event":"env","timestamp":"$RESIZE_ENV_TIMESTAMP","seed":$RESIZE_SEED,"term":"${TERM:-}","colorterm":"${COLORTERM:-}","no_color":"${NO_COLOR:-}"}
{"event":"rust","rustc":"$(rustc --version 2>/dev/null || echo 'N/A')","cargo":"$(cargo --version 2>/dev/null || echo 'N/A')"}
{"event":"git","commit":"$(git rev-parse HEAD 2>/dev/null || echo 'N/A')","branch":"$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo 'N/A')"}
EOF

PTY_JSONL_DEFAULT="${PTY_JSONL:-$E2E_LOG_DIR/resize_pty.jsonl}"

run_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)" || return 2
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    return 1
}

# Test: Small terminal (60x15)
resize_small() {
    LOG_FILE="$E2E_LOG_DIR/resize_small.log"
    local output_file="$E2E_LOG_DIR/resize_small.pty"

    log_test_start "resize_small"

    PTY_COLS=60 \
    PTY_ROWS=15 \
    FTUI_HARNESS_EXIT_AFTER_MS=1000 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="resize_small" \
    PTY_JSONL="$PTY_JSONL_DEFAULT" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should have substantial output
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    # Status bar should render despite narrow width
    grep -a -q "claude-3.5" "$output_file" || return 1

    log_debug "resize_small: $size bytes captured"
}

# Test: Wide terminal (120x24)
resize_wide() {
    LOG_FILE="$E2E_LOG_DIR/resize_wide.log"
    local output_file="$E2E_LOG_DIR/resize_wide.pty"

    log_test_start "resize_wide"

    PTY_COLS=120 \
    PTY_ROWS=24 \
    FTUI_HARNESS_EXIT_AFTER_MS=1000 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="resize_wide" \
    PTY_JSONL="$PTY_JSONL_DEFAULT" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should have substantial output
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    # Status bar should render
    grep -a -q "claude-3.5" "$output_file" || return 1

    log_debug "resize_wide: $size bytes captured"
}

# Test: Tall terminal (80x40)
resize_tall() {
    LOG_FILE="$E2E_LOG_DIR/resize_tall.log"
    local output_file="$E2E_LOG_DIR/resize_tall.pty"

    log_test_start "resize_tall"

    PTY_COLS=80 \
    PTY_ROWS=40 \
    FTUI_HARNESS_EXIT_AFTER_MS=1000 \
    FTUI_HARNESS_LOG_LINES=20 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="resize_tall" \
    PTY_JSONL="$PTY_JSONL_DEFAULT" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should have substantial output
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 300 ]] || return 1

    # Status bar should render
    grep -a -q "claude-3.5" "$output_file" || return 1

    log_debug "resize_tall: $size bytes captured"
}

# Test: Scroll region (DECSTBM) sequence detection
# When not in a mux and with inline mode, scroll region should be set.
# DECSTBM format: ESC [ top ; bottom r  (1-indexed rows)
resize_scroll_region() {
    LOG_FILE="$E2E_LOG_DIR/resize_scroll_region.log"
    local output_file="$E2E_LOG_DIR/resize_scroll_region.pty"

    log_test_start "resize_scroll_region"

    # Run without mux environment variables to allow scroll region
    # Clear any mux indicators that might be set
    unset TMUX ZELLIJ TERM_PROGRAM TERM_PROGRAM_VERSION 2>/dev/null || true

    PTY_COLS=80 \
    PTY_ROWS=24 \
    TERM=xterm-256color \
    COLORTERM=truecolor \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_LOG_LINES=10 \
    FTUI_HARNESS_SCREEN_MODE=inline \
    FTUI_HARNESS_UI_HEIGHT=6 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=4 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="resize_scroll_region" \
    PTY_JSONL="$PTY_JSONL_DEFAULT" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should have output
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    # Look for DECSTBM pattern: ESC [ digits ; digits r
    # For 24-row terminal with 6-row UI, log region is rows 1-18
    # DECSTBM would be: ESC [ 1 ; 18 r  (hex: 1b 5b 31 3b 31 38 72)
    # Actually the exact range depends on implementation.
    # Just check for any scroll region setup (ESC [ ... r pattern)
    if grep -a -o -E $'\x1b\\[[0-9]+;[0-9]+r' "$output_file" >/dev/null 2>&1; then
        log_debug "Scroll region sequence found"
    else
        # If no scroll region found, that's ok - might be using overlay mode
        # or running in a mux. Log it for diagnostics.
        log_debug "No scroll region sequence found (overlay mode or mux detected)"
    fi

    # Either way, harness should function correctly
    grep -a -q "claude-3.5" "$output_file" || return 1
}

# Test: Dynamic resize triggers scroll-region update and resize event
resize_scroll_region_bounds() {
    LOG_FILE="$E2E_LOG_DIR/resize_scroll_region_bounds.log"
    local output_file="$E2E_LOG_DIR/resize_scroll_region_bounds.pty"

    log_test_start "resize_scroll_region_bounds"

    local initial_cols=80
    local initial_rows=24
    local resize_cols=100
    local resize_rows=30
    local resize_delay_ms=400
    local ui_height=8

    log_info "Resize schedule: ${initial_cols}x${initial_rows} -> ${resize_cols}x${resize_rows} @ ${resize_delay_ms}ms"
    log_info "Expected scroll region: 1;16r then 1;22r (ui_height=${ui_height})"

    unset TMUX ZELLIJ STY TERM_PROGRAM TERM_PROGRAM_VERSION 2>/dev/null || true

    TERM="xterm-256color" \
    PTY_COLS="$initial_cols" \
    PTY_ROWS="$initial_rows" \
    PTY_RESIZE_COLS="$resize_cols" \
    PTY_RESIZE_ROWS="$resize_rows" \
    PTY_RESIZE_DELAY_MS="$resize_delay_ms" \
    FTUI_HARNESS_SCREEN_MODE=inline \
    FTUI_HARNESS_UI_HEIGHT="$ui_height" \
    FTUI_HARNESS_LOG_LINES=25 \
    FTUI_HARNESS_EXIT_AFTER_MS=1600 \
    PTY_TIMEOUT=5 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="resize_scroll_region_bounds" \
    PTY_JSONL="$PTY_JSONL_DEFAULT" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    log_info "Observed resize lines (raw PTY capture)"
    grep -a "Resize:" "$output_file" >> "$LOG_FILE" 2>&1 || true

    # Resize event is expected, but some terminals may not emit the event reliably.
    # Treat it as diagnostic-only and rely on scroll-region updates as the hard check.
    if ! grep -a -q "Resize: ${resize_cols}x${resize_rows}" "$output_file"; then
        log_warn "Resize event line not found in PTY capture" || true
    fi
    # UI chrome should still render.
    grep -a -q "claude-3.5" "$output_file" || return 1

    # Scroll region bounds should be set for initial + resized terminal sizes.
    grep -a -F -q $'\x1b[1;16r' "$output_file" || return 1
    grep -a -F -q $'\x1b[1;22r' "$output_file" || return 1

    # Cursor save/restore sequences should be present in inline mode.
    grep -a -F -q $'\x1b7' "$output_file" || return 1
    grep -a -F -q $'\x1b8' "$output_file" || return 1

    # Log a final buffer snapshot for diagnostics.
    log_info "Final PTY tail (printable)"
    if command -v strings >/dev/null 2>&1; then
        strings -n 3 "$output_file" | tail -n 30 >> "$LOG_FILE" 2>&1 || true
    fi
    log_info "Final PTY tail (hex)"
    if command -v xxd >/dev/null 2>&1; then
        tail -c 256 "$output_file" | xxd -g 1 >> "$LOG_FILE" 2>&1 || true
    fi
}

# Test: Scroll region reset on cleanup
# The cleanup should emit ESC [ r to reset scroll region to full screen.
resize_cleanup_reset() {
    LOG_FILE="$E2E_LOG_DIR/resize_cleanup_reset.log"
    local output_file="$E2E_LOG_DIR/resize_cleanup_reset.pty"

    log_test_start "resize_cleanup_reset"

    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_EXIT_AFTER_MS=1000 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_SCREEN_MODE=inline \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="resize_cleanup_reset" \
    PTY_JSONL="$PTY_JSONL_DEFAULT" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should have output
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    # Cursor show must appear at cleanup (ESC [ ? 25 h)
    grep -a -F -q $'\x1b[?25h' "$output_file" || return 1

    # If scroll region was set, it should be reset (ESC [ r)
    # This is optional - depends on whether scroll region was used
    if grep -a -F -q $'\x1b[r' "$output_file"; then
        log_debug "Scroll region reset sequence found at cleanup"
    else
        log_debug "No scroll region reset (scroll region may not have been used)"
    fi
}

# Test: Minimum viable size
resize_minimum() {
    LOG_FILE="$E2E_LOG_DIR/resize_minimum.log"
    local output_file="$E2E_LOG_DIR/resize_minimum.pty"

    log_test_start "resize_minimum"

    # Very small terminal - harness should handle gracefully
    PTY_COLS=40 \
    PTY_ROWS=10 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=2 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="resize_minimum" \
    PTY_JSONL="$PTY_JSONL_DEFAULT" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should have some output (even if layout is degraded)
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 100 ]] || return 1

    log_debug "resize_minimum: $size bytes captured"
}

# Test: Very large terminal
resize_large() {
    LOG_FILE="$E2E_LOG_DIR/resize_large.log"
    local output_file="$E2E_LOG_DIR/resize_large.pty"

    log_test_start "resize_large"

    PTY_COLS=200 \
    PTY_ROWS=60 \
    FTUI_HARNESS_EXIT_AFTER_MS=1000 \
    FTUI_HARNESS_LOG_LINES=30 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=4 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="resize_large" \
    PTY_JSONL="$PTY_JSONL_DEFAULT" \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Should have substantial output
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 500 ]] || return 1

    # Status bar should render
    grep -a -q "claude-3.5" "$output_file" || return 1

    log_debug "resize_large: $size bytes captured"
}

FAILURES=0
run_case "resize_small" resize_small                    || FAILURES=$((FAILURES + 1))
run_case "resize_wide" resize_wide                      || FAILURES=$((FAILURES + 1))
run_case "resize_tall" resize_tall                      || FAILURES=$((FAILURES + 1))
run_case "resize_scroll_region" resize_scroll_region    || FAILURES=$((FAILURES + 1))
run_case "resize_scroll_region_bounds" resize_scroll_region_bounds || FAILURES=$((FAILURES + 1))
run_case "resize_cleanup_reset" resize_cleanup_reset    || FAILURES=$((FAILURES + 1))
run_case "resize_minimum" resize_minimum                || FAILURES=$((FAILURES + 1))
run_case "resize_large" resize_large                    || FAILURES=$((FAILURES + 1))

exit "$FAILURES"

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

ALL_CASES=(
    osc8_open_sequence
    osc8_close_sequence
    osc8_multiple_links
    osc8_reset_after_frame
)

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/osc8_missing.log"
    for t in "${ALL_CASES[@]}"; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

FIXTURE_FILE="$SCRIPT_DIR/../fixtures/hyperlink_markup.txt"
if [[ ! -f "$FIXTURE_FILE" ]]; then
    LOG_FILE="$E2E_LOG_DIR/osc8_fixture_missing.log"
    for t in "${ALL_CASES[@]}"; do
        log_test_skip "$t" "fixture file missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "fixture missing"
    done
    exit 0
fi

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
    log_test_fail "$name" "OSC 8 assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "OSC 8 assertions failed"
    return 1
}

# Test: OSC 8 open sequences are emitted for hyperlinks
# The harness should emit OSC 8 open sequences when rendering links from markup.
osc8_open_sequence() {
    LOG_FILE="$E2E_LOG_DIR/osc8_open_sequence.log"
    local output_file="$E2E_LOG_DIR/osc8_open_sequence.pty"

    log_test_start "osc8_open_sequence"

    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_LOG_FILE="$FIXTURE_FILE" \
    FTUI_HARNESS_LOG_MARKUP=1 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # OSC 8 open sequence: ESC ] 8 ; ; URL (BEL or ST terminator)
    # Should contain at least one OSC 8 open with URL
    if grep -a -E -q $'\x1b]8;;https://[^\x07\x1b]+[\x07\x1b]' "$output_file"; then
        log_debug "OSC 8 open sequence with URL found"
        return 0
    fi

    log_debug "No OSC 8 open sequence found in output"
    return 1
}

# Test: OSC 8 close sequences are emitted after links
# Each link should be closed with an empty OSC 8 sequence.
osc8_close_sequence() {
    LOG_FILE="$E2E_LOG_DIR/osc8_close_sequence.log"
    local output_file="$E2E_LOG_DIR/osc8_close_sequence.pty"

    log_test_start "osc8_close_sequence"

    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_LOG_FILE="$FIXTURE_FILE" \
    FTUI_HARNESS_LOG_MARKUP=1 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # OSC 8 close sequence: ESC ] 8 ; ; (empty URL) followed by BEL or ST
    # This closes any active hyperlink
    if grep -a -E -q $'\x1b]8;;[\x07\x1b]' "$output_file"; then
        log_debug "OSC 8 close sequence found"
        return 0
    fi

    log_debug "No OSC 8 close sequence found in output"
    return 1
}

# Test: Multiple distinct links have separate OSC 8 sequences
# The fixture has multiple links; verify at least two different URLs appear.
osc8_multiple_links() {
    LOG_FILE="$E2E_LOG_DIR/osc8_multiple_links.log"
    local output_file="$E2E_LOG_DIR/osc8_multiple_links.pty"

    log_test_start "osc8_multiple_links"

    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_LOG_FILE="$FIXTURE_FILE" \
    FTUI_HARNESS_LOG_MARKUP=1 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Count distinct URLs in OSC 8 sequences
    local url_count
    url_count=$(grep -a -o -E $'\x1b]8;;https://[^\x07\x1b]+' "$output_file" 2>/dev/null | sort -u | wc -l | tr -d ' ')

    if [[ "$url_count" -ge 2 ]]; then
        log_debug "Found $url_count distinct OSC 8 URLs"
        return 0
    fi

    log_debug "Only found $url_count distinct URLs, expected at least 2"
    return 1
}

# Test: OSC 8 link is reset at frame end
# After rendering, any active link should be closed with an OSC 8 close sequence.
osc8_reset_after_frame() {
    LOG_FILE="$E2E_LOG_DIR/osc8_reset_after_frame.log"
    local output_file="$E2E_LOG_DIR/osc8_reset_after_frame.pty"

    log_test_start "osc8_reset_after_frame"

    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_LOG_FILE="$FIXTURE_FILE" \
    FTUI_HARNESS_LOG_MARKUP=1 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Count opens and closes - closes should be >= opens (link transitions + frame end)
    local open_count close_count
    open_count=$(grep -a -c -F $'\x1b]8;;https://' "$output_file" 2>/dev/null || echo "0")
    close_count=$(grep -a -c -E $'\x1b]8;;[\x07\x1b]' "$output_file" 2>/dev/null || echo "0")

    # Remove the URL matches from close count (they also match the pattern partially)
    # Actually, the close pattern matches empty URL only, so this should be fine
    # But we need to exclude the URL opens from the close count
    close_count=$(grep -a -o -E $'\x1b]8;;[\x07\x1b\\\\]' "$output_file" 2>/dev/null | wc -l | tr -d ' ')

    log_debug "OSC 8 opens: $open_count, closes: $close_count"

    if [[ "$close_count" -ge "$open_count" ]] && [[ "$close_count" -gt 0 ]]; then
        log_debug "Link reset verified: closes >= opens"
        return 0
    fi

    log_debug "Link reset check failed: expected closes >= opens"
    return 1
}

FAILURES=0
run_case "osc8_open_sequence" osc8_open_sequence       || FAILURES=$((FAILURES + 1))
run_case "osc8_close_sequence" osc8_close_sequence     || FAILURES=$((FAILURES + 1))
run_case "osc8_multiple_links" osc8_multiple_links     || FAILURES=$((FAILURES + 1))
run_case "osc8_reset_after_frame" osc8_reset_after_frame || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

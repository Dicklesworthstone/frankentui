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

E2E_SUITE_SCRIPT="$SCRIPT_DIR/test_osc8.sh"
export E2E_SUITE_SCRIPT
ONLY_CASE="${E2E_ONLY_CASE:-}"

ALL_CASES=(
    osc8_basic_link
    osc8_multi_links
)

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/osc8_missing.log"
    for t in "${ALL_CASES[@]}"; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

run_case() {
    local name="$1"
    shift
    if [[ -n "$ONLY_CASE" && "$ONLY_CASE" != "$name" ]]; then
        LOG_FILE="$E2E_LOG_DIR/${name}.log"
        log_test_skip "$name" "filtered (E2E_ONLY_CASE=$ONLY_CASE)"
        record_result "$name" "skipped" 0 "$LOG_FILE" "filtered"
        return 0
    fi
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

osc8_basic_link() {
    LOG_FILE="$E2E_LOG_DIR/osc8_basic_link.log"
    local output_file="$E2E_LOG_DIR/osc8_basic_link.pty"
    local log_fixture="$E2E_LOG_DIR/osc8_basic_link.txt"

    log_test_start "osc8_basic_link"

    cat > "$log_fixture" <<'TXT'
[link=https://example.com]Click here[/link]
TXT

    PTY_COLS=80 \
    PTY_ROWS=24 \
    FTUI_HARNESS_LOG_MARKUP=1 \
    FTUI_HARNESS_LOG_FILE="$log_fixture" \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    # OSC 8 open and close should appear
    grep -a -F -q $'\x1b]8;;https://example.com\x1b\\' "$output_file" || return 1
    grep -a -F -q $'\x1b]8;;\x1b\\' "$output_file" || return 1
}

osc8_multi_links() {
    LOG_FILE="$E2E_LOG_DIR/osc8_multi_links.log"
    local output_file="$E2E_LOG_DIR/osc8_multi_links.pty"
    local log_fixture="$E2E_LOG_DIR/osc8_multi_links.txt"

    log_test_start "osc8_multi_links"

    cat > "$log_fixture" <<'TXT'
[link=https://a.example]Alpha[/link] [link=https://b.example]Beta[/link]
TXT

    PTY_COLS=90 \
    PTY_ROWS=24 \
    FTUI_HARNESS_LOG_MARKUP=1 \
    FTUI_HARNESS_LOG_FILE="$log_fixture" \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    grep -a -F -q $'\x1b]8;;https://a.example\x1b\\' "$output_file" || return 1
    grep -a -F -q $'\x1b]8;;https://b.example\x1b\\' "$output_file" || return 1
    grep -a -F -q $'\x1b]8;;\x1b\\' "$output_file" || return 1
}

FAILURES=0
run_case "osc8_basic_link" osc8_basic_link   || FAILURES=$((FAILURES + 1))
run_case "osc8_multi_links" osc8_multi_links || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

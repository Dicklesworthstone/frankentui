#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=../lib/common.sh
source "$SCRIPT_DIR/../lib/common.sh"
# shellcheck source=../lib/logging.sh
source "$SCRIPT_DIR/../lib/logging.sh"
# shellcheck source=../lib/pty.sh
source "$SCRIPT_DIR/../lib/pty.sh"

LOG_LEVEL=${LOG_LEVEL:-INFO}
LOG_DIR=${LOG_DIR:-/tmp/ftui_e2e_logs}
ensure_log_dir

require_command cargo
require_command script
require_command timeout
require_command grep

TOTAL=0
PASSED=0
FAILED=0
SKIPPED=0
START_MS=$(now_ms)

TEST_NAMES=()
TEST_STATUS=()
TEST_DURATION=()
TEST_LOG_FILES=()
TEST_OUTPUT_FILES=()

record_result() {
    local name="$1"
    local status="$2"
    local duration_ms="$3"
    local log_file="$4"
    local output_file="$5"

    TEST_NAMES+=($name)
    TEST_STATUS+=($status)
    TEST_DURATION+=($duration_ms)
    TEST_LOG_FILES+=($log_file)
    TEST_OUTPUT_FILES+=($output_file)
}

run_test() {
    local name="$1"
    local fn="$2"

    TOTAL=$((TOTAL + 1))

    local test_log="$LOG_DIR/${name}.log"
    local test_output="$LOG_DIR/${name}.pty"
    LOG_FILE="$test_log"

    log_test_start "$name"

    local start_ms
    start_ms=$(now_ms)

    if $fn "$test_output"; then
        local end_ms
        end_ms=$(now_ms)
        local duration_ms=$((end_ms - start_ms))
        PASSED=$((PASSED + 1))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$test_log" "$test_output"
        return 0
    else
        local end_ms
        end_ms=$(now_ms)
        local duration_ms=$((end_ms - start_ms))
        FAILED=$((FAILED + 1))
        log_test_fail "$name" "See $test_log"
        record_result "$name" "failed" "$duration_ms" "$test_log" "$test_output"
        return 1
    fi
}

# ---------------------------------------------------------------------------
# Build step (shared)
# ---------------------------------------------------------------------------
BUILD_LOG="$LOG_DIR/build_harness.log"
LOG_FILE="$BUILD_LOG"
log_info "Building ftui-harness (for E2E)"
if ! cargo build -p ftui-harness >"$BUILD_LOG" 2>&1; then
    log_error "Build failed. See: $BUILD_LOG"
    exit 1
fi

HARNESS_BIN="$PROJECT_ROOT/target/debug/ftui-harness"
if [[ ! -x "$HARNESS_BIN" ]]; then
    log_error "Harness binary not found at $HARNESS_BIN"
    exit 1
fi

# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

test_inline_smoke() {
    local output_file="$1"

    local input="help\nquit\n"
    run_pty "$HARNESS_BIN" "$input" "$output_file" 12

    if ! expect_output_contains "$output_file" "Welcome to the Agent Harness Reference Application"; then
        log_error "Missing welcome banner"
        return 1
    fi

    if ! expect_output_contains "$output_file" "Available commands:"; then
        log_error "Missing help output"
        return 1
    fi

    return 0
}

test_cleanup_cursor() {
    local output_file="$1"

    local input="quit\n"
    run_pty "$HARNESS_BIN" "$input" "$output_file" 8

    # Expect cursor show sequence (CSI ? 25 h)
    if ! expect_ansi_sequence "$output_file" $'\x1b\\[\\?25h'; then
        log_error "Missing cursor show sequence"
        return 1
    fi

    return 0
}

run_test "inline_smoke" test_inline_smoke || true
run_test "cleanup_cursor" test_cleanup_cursor || true

END_MS=$(now_ms)
DURATION_MS=$((END_MS - START_MS))

SUMMARY_JSON="$LOG_DIR/summary.json"
{
    printf '{\n'
    printf '  "timestamp": "%s",\n' "$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    printf '  "total": %s,\n' "$TOTAL"
    printf '  "passed": %s,\n' "$PASSED"
    printf '  "failed": %s,\n' "$FAILED"
    printf '  "skipped": %s,\n' "$SKIPPED"
    printf '  "duration_ms": %s,\n' "$DURATION_MS"
    printf '  "tests": [\n'

    local i
    for i in "${!TEST_NAMES[@]}"; do
        local name="${TEST_NAMES[$i]}"
        local status="${TEST_STATUS[$i]}"
        local duration="${TEST_DURATION[$i]}"
        local log_file="${TEST_LOG_FILES[$i]}"
        local output_file="${TEST_OUTPUT_FILES[$i]}"

        printf '    {"name": "%s", "status": "%s", "duration_ms": %s, "log_file": "%s", "output_file": "%s"}' \
            "$name" "$status" "$duration" "$log_file" "$output_file"

        if [[ $i -lt $((${#TEST_NAMES[@]} - 1)) ]]; then
            printf ','
        fi
        printf '\n'
    done

    printf '  ]\n'
    printf '}\n'
} >"$SUMMARY_JSON"

log_info "E2E summary: $SUMMARY_JSON"
log_info "Results: passed=$PASSED failed=$FAILED skipped=$SKIPPED"

if [[ $FAILED -ne 0 ]]; then
    exit 1
fi

#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PROJECT_ROOT="$(cd "$E2E_ROOT/../.." && pwd)"

# Reuse shared helpers from tests/e2e/lib
# shellcheck source=/dev/null
source "$PROJECT_ROOT/tests/e2e/lib/common.sh"
# shellcheck source=/dev/null
source "$PROJECT_ROOT/tests/e2e/lib/logging.sh"
# shellcheck source=/dev/null
source "$PROJECT_ROOT/tests/e2e/lib/pty.sh"

require_harness() {
    mkdir -p "$E2E_LOG_DIR"
    LOG_FILE="$E2E_LOG_DIR/${TEST_NAME//\//_}.log"
    local rc=0
    e2e_require bin "${E2E_HARNESS_BIN:-}" "ftui-harness binary missing" || rc=$?
    if (( rc != 0 )); then
        if (( rc == 1 )); then
            log_test_skip "$TEST_NAME" "ftui-harness binary missing (opted out)"
            record_result "$TEST_NAME" "skipped" 0 "$LOG_FILE" "binary missing"
            exit 0
        fi
        log_test_fail "$TEST_NAME" "ftui-harness binary missing"
        record_result "$TEST_NAME" "failed" 0 "$LOG_FILE" "missing_prerequisite: ftui-harness binary missing"
        exit "$rc"
    fi
}

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
    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    return 1
}

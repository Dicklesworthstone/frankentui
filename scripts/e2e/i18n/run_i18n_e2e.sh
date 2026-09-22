#!/bin/bash
# i18n E2E Test Suite for FrankenTUI (bd-ic6i.6)
#
# Validates:
# 1. String localization (lookup, fallback, interpolation)
# 2. Pluralization (English, Russian, Arabic, French, CJK, Polish)
# 3. RTL layout (direction reversal, alignment mirroring)
# 4. BiDi text (reorder, segment mapping, bracket pairing)
# 5. Locale switching (runtime, persistence, determinism)
# 6. Performance budgets (lookup < 1μs, categorize < 100ns)
#
# JSONL Log Schema:
#   {"ts":"<utc>","step":"<name>","status":"<pass|fail|skip>","duration_ms":<N>,...}
#
# Usage:
#   ./scripts/e2e/i18n/run_i18n_e2e.sh [--verbose]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
VERBOSE="${1:-}"

# Logging
LOG_DIR="${E2E_LOG_DIR:-${PROJECT_ROOT}/target/e2e-logs/i18n}"
RESULTS_DIR="${E2E_RESULTS_DIR:-${LOG_DIR}/results}"
JSONL_FILE="${LOG_DIR}/i18n_e2e.jsonl"
mkdir -p "$LOG_DIR" "$RESULTS_DIR"

PASSED=0
FAILED=0
SKIPPED=0

# Milliseconds from the monotonic clock. Not `date +%s%3N`: BSD date prints
# "...3N", and the duration arithmetic then aborted the suite on macOS.
now_ms() {
    python3 -c 'import time; print(time.monotonic_ns() // 1_000_000)'
}

jsonl() {
    local step="$1"
    shift
    local fields="\"ts\":\"$(date -Iseconds)\",\"step\":\"$step\""
    while (( $# >= 2 )); do
        fields="${fields},\"$1\":\"$2\""
        shift 2
    done
    echo "{${fields}}" >> "$JSONL_FILE"
}

run_step() {
    local name="$1"
    local log_file="$2"
    shift 2
    local start_ms
    start_ms="$(now_ms)"

    echo -n "  [$name] ... "
    jsonl "$name" "event" "start"

    if "$@" > "$log_file" 2>&1; then
        local end_ms
        end_ms="$(now_ms)"
        local dur=$((end_ms - start_ms))
        echo "PASS (${dur}ms)"
        jsonl "$name" "status" "passed" "duration_ms" "$dur"
        PASSED=$((PASSED + 1))
        return 0
    else
        local end_ms
        end_ms="$(now_ms)"
        local dur=$((end_ms - start_ms))
        echo "FAIL (${dur}ms)"
        jsonl "$name" "status" "failed" "duration_ms" "$dur"
        FAILED=$((FAILED + 1))
        if [[ "$VERBOSE" == "--verbose" ]]; then
            echo "    --- output ---"
            tail -20 "$log_file" | sed 's/^/    /'
            echo "    --- end ---"
        fi
        return 1
    fi
}

echo "=========================================="
echo "  i18n E2E Test Suite (bd-ic6i.6)"
echo "=========================================="
echo ""

# Log environment
{
    echo "Environment Information"
    echo "Date: $(date -Iseconds)"
    echo "Rust version: $(rustc --version 2>/dev/null || echo 'N/A')"
    echo "Cargo version: $(cargo --version 2>/dev/null || echo 'N/A')"
    echo "Git: $(cd "$PROJECT_ROOT" && git log -1 --oneline 2>/dev/null || echo 'N/A')"
    echo "TERM: ${TERM:-unset}"
    echo "LANG: ${LANG:-unset}"
} > "$LOG_DIR/00_environment.log"

jsonl "environment" \
    "rust" "$(rustc --version 2>/dev/null | awk '{print $2}')" \
    "term" "${TERM:-unset}" \
    "lang" "${LANG:-unset}"

echo "Phase 1: Compilation & Linting"
echo "---"

run_step "cargo_check" "$LOG_DIR/01_check.log" \
    cargo check -p ftui-demo-showcase --tests || true

run_step "clippy" "$LOG_DIR/02_clippy.log" \
    cargo clippy -p ftui-demo-showcase --tests -- -D warnings || true

run_step "fmt_check" "$LOG_DIR/03_fmt.log" \
    cargo fmt -p ftui-demo-showcase --check || true

echo ""
echo "Phase 2: Unit Tests (i18n crate)"
echo "---"

run_step "i18n_unit_tests" "$LOG_DIR/04_i18n_unit.log" \
    cargo test -p ftui-i18n || true

echo ""
echo "Phase 3: E2E Integration Tests"
echo "---"

run_step "i18n_e2e_all" "$LOG_DIR/05_i18n_e2e.log" \
    cargo test -p ftui-demo-showcase --test i18n_e2e -- --nocapture || true

# Run individual test groups for granularity
run_step "string_localization" "$LOG_DIR/06_string.log" \
    cargo test -p ftui-demo-showcase --test i18n_e2e -- string_ --nocapture || true

run_step "pluralization" "$LOG_DIR/07_plural.log" \
    cargo test -p ftui-demo-showcase --test i18n_e2e -- plural_ --nocapture || true

run_step "rtl_layout" "$LOG_DIR/08_rtl.log" \
    cargo test -p ftui-demo-showcase --test i18n_e2e -- rtl_ --nocapture || true

run_step "bidi_text" "$LOG_DIR/09_bidi.log" \
    cargo test -p ftui-demo-showcase --test i18n_e2e -- bidi_ --nocapture || true

run_step "integration" "$LOG_DIR/10_integration.log" \
    cargo test -p ftui-demo-showcase --test i18n_e2e -- integration_ --nocapture || true

run_step "performance" "$LOG_DIR/11_perf.log" \
    cargo test -p ftui-demo-showcase --test i18n_e2e -- perf_ --nocapture || true

run_step "locale_context" "$LOG_DIR/11b_locale_ctx.log" \
    cargo test -p ftui-demo-showcase --test i18n_e2e -- locale_context_ --nocapture || true

echo ""
echo "Phase 4: i18n Demo Unit Tests"
echo "---"

run_step "i18n_demo_unit" "$LOG_DIR/12_demo_unit.log" \
    cargo test -p ftui-demo-showcase --lib -- i18n_demo --nocapture || true

echo ""
echo "=========================================="
TOTAL=$((PASSED + FAILED + SKIPPED))
echo "  Results: $PASSED/$TOTAL passed, $FAILED failed, $SKIPPED skipped"
echo "  Logs: $LOG_DIR"
echo "  JSONL: $JSONL_FILE"
echo "=========================================="

jsonl "summary" \
    "total" "$TOTAL" \
    "passed" "$PASSED" \
    "failed" "$FAILED" \
    "skipped" "$SKIPPED"

if [[ $FAILED -gt 0 ]]; then
    echo ""
    echo "FAILED — check logs for details"
    exit 1
fi

echo ""
echo "ALL PASSED"
exit 0

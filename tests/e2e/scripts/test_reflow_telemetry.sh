#!/bin/bash
set -euo pipefail

# E2E tests for resize coalescer telemetry and diagnostics (bd-1rz0.7)
#
# Validates:
# - JSONL decision logging format
# - Evidence ledger completeness
# - Deterministic checksums
# - Regime detection accuracy
# - Latency bounds compliance

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"

REFLOW_SEED="${REFLOW_SEED:-42}"
VOI_SEED="${VOI_SEED:-$REFLOW_SEED}"
REFLOW_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui-e2e}"
mkdir -p "$REFLOW_LOG_DIR"

REFLOW_ENV_JSONL="$REFLOW_LOG_DIR/reflow_env_$(date +%Y%m%d_%H%M%S).jsonl"
cat > "$REFLOW_ENV_JSONL" << EOF
{"event":"env","timestamp":"$(date -Iseconds)","seed":$REFLOW_SEED,"test":"reflow_telemetry"}
{"event":"rust","rustc":"$(rustc --version 2>/dev/null || echo 'N/A')","cargo":"$(cargo --version 2>/dev/null || echo 'N/A')"}
{"event":"capabilities","term":"${TERM:-}","colorterm":"${COLORTERM:-}","tmux":"${TMUX:-}","zellij":"${ZELLIJ:-}","kitty_window_id":"${KITTY_WINDOW_ID:-}","term_program":"${TERM_PROGRAM:-}"}
{"event":"voi_env","timestamp":"$(date -Iseconds)","seed":$VOI_SEED,"test":"voi_sampling"}
{"event":"git","commit":"$(git rev-parse HEAD 2>/dev/null || echo 'N/A')","branch":"$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo 'N/A')"}
EOF

compute_checksum() {
    local file="$1"
    if [[ -f "$file" ]]; then
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum "$file" | cut -d' ' -f1 | head -c 16
        elif command -v md5sum >/dev/null 2>&1; then
            md5sum "$file" | cut -d' ' -f1 | head -c 16
        else
            local size
            size=$(wc -c < "$file")
            printf "%08x%08x" "$size" "$(head -c 64 "$file" | cksum | cut -d' ' -f1)"
        fi
    else
        echo "0000000000000000"
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
        echo "{\"test\":\"$name\",\"status\":\"passed\",\"duration_ms\":$duration_ms}" >> "$REFLOW_ENV_JSONL"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)"
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "assertion failed"
    echo "{\"test\":\"$name\",\"status\":\"failed\",\"duration_ms\":$duration_ms}" >> "$REFLOW_ENV_JSONL"
    return 1
}

# Test: JSONL decision log format validation
test_jsonl_format() {
    log_test_start "reflow_jsonl_format"

    # Run the resize coalescer tests with logging enabled
    # The tests already verify JSONL format, this just confirms the output
    local output
    output=$(cargo test -p ftui-runtime resize_coalescer::tests::logging_jsonl_format -- --nocapture 2>&1 || true)

    # Check that the test passed
    if echo "$output" | grep -q "test.*ok"; then
        log_debug "JSONL format test passed"
        return 0
    fi

    log_debug "JSONL format test output: $output"
    return 1
}

# Test: Evidence ledger completeness
test_evidence_ledger() {
    log_test_start "reflow_evidence_ledger"

    # Run evidence_jsonl test
    local output
    output=$(cargo test -p ftui-runtime resize_coalescer::tests::evidence_jsonl_includes_summary -- --nocapture 2>&1 || true)

    if echo "$output" | grep -q "test.*ok"; then
        log_debug "Evidence ledger test passed"
        return 0
    fi

    return 1
}

# Test: Deterministic checksums
test_deterministic_checksums() {
    log_test_start "reflow_deterministic_checksums"

    # Run checksum stability test
    local output
    output=$(cargo test -p ftui-runtime resize_coalescer::tests::decision_checksum_is_stable -- --nocapture 2>&1 || true)

    if echo "$output" | grep -q "test.*ok"; then
        log_debug "Deterministic checksum test passed"
        return 0
    fi

    return 1
}

# Test: Regime detection - burst mode enters with rapid events
test_burst_detection() {
    log_test_start "reflow_burst_detection"

    local output
    output=$(cargo test -p ftui-runtime resize_coalescer::tests::burst_mode_detection -- --nocapture 2>&1 || true)

    if echo "$output" | grep -q "test.*ok"; then
        log_debug "Burst mode detection test passed"
        return 0
    fi

    return 1
}

# Test: Latency bounds - hard deadline is respected
test_latency_bounds() {
    log_test_start "reflow_latency_bounds"

    local output
    output=$(cargo test -p ftui-runtime resize_coalescer::tests::bounded_latency_invariant -- --nocapture 2>&1 || true)

    if echo "$output" | grep -q "test.*ok"; then
        log_debug "Latency bounds test passed"
        return 0
    fi

    return 1
}

# Test: Property tests - invariants hold under random inputs
test_property_invariants() {
    log_test_start "reflow_property_invariants"

    # Run all property tests
    local output
    output=$(cargo test -p ftui-runtime resize_coalescer::tests::property -- --nocapture 2>&1 || true)

    # Count passed tests
    local passed
    passed=$(echo "$output" | grep -c "test.*ok" || echo "0")

    if [[ "$passed" -ge 4 ]]; then
        log_debug "Property invariant tests passed: $passed"
        return 0
    fi

    log_debug "Property tests output: $output"
    return 1
}

# Test: Coalesce time tracking
test_coalesce_time() {
    log_test_start "reflow_coalesce_time"

    local output
    output=$(cargo test -p ftui-runtime resize_coalescer::tests::coalesce_time_tracked -- --nocapture 2>&1 || true)

    if echo "$output" | grep -q "test.*ok"; then
        log_debug "Coalesce time tracking test passed"
        return 0
    fi

    return 1
}

# Test: Latest-wins semantics
test_latest_wins() {
    log_test_start "reflow_latest_wins"

    local output
    output=$(cargo test -p ftui-runtime resize_coalescer::tests::latest_wins_semantics -- --nocapture 2>&1 || true)

    if echo "$output" | grep -q "test.*ok"; then
        log_debug "Latest-wins semantics test passed"
        return 0
    fi

    return 1
}

# Test: VOI sampling JSONL output (deterministic)
test_voi_sampling_policy() {
    log_test_start "reflow_voi_sampling"

    local output
    output=$(VOI_SEED="$VOI_SEED" cargo test -p ftui-runtime voi_sampling::tests::e2e_deterministic_jsonl -- --nocapture 2>&1 || true)

    if ! echo "$output" | grep -q "test.*ok"; then
        log_debug "VOI sampling test output: $output"
        return 1
    fi

    local jsonl_path="$REFLOW_LOG_DIR/voi_sampling_${VOI_SEED}_$(date +%Y%m%d_%H%M%S).jsonl"
    echo "$output" | grep '^{"event":"voi_' > "$jsonl_path" || true

    local checksum
    checksum=$(compute_checksum "$jsonl_path")
    echo "{\"event\":\"voi_sampling\",\"seed\":$VOI_SEED,\"jsonl\":\"$jsonl_path\",\"checksum\":\"sha256:$checksum\"}" >> "$REFLOW_ENV_JSONL"
    return 0
}

# Summary report
generate_summary() {
    local passed=$1
    local failed=$2
    local total=$((passed + failed))

    cat >> "$REFLOW_ENV_JSONL" << EOF
{"event":"summary","passed":$passed,"failed":$failed,"total":$total,"timestamp":"$(date -Iseconds)"}
EOF

    log_info "Reflow Telemetry E2E Summary: $passed/$total passed"
    if [[ $failed -gt 0 ]]; then
        log_warn "$failed tests failed"
    fi
}

# Run all tests
FAILURES=0
PASSES=0

run_case "reflow_jsonl_format" test_jsonl_format && PASSES=$((PASSES + 1)) || FAILURES=$((FAILURES + 1))
run_case "reflow_evidence_ledger" test_evidence_ledger && PASSES=$((PASSES + 1)) || FAILURES=$((FAILURES + 1))
run_case "reflow_deterministic_checksums" test_deterministic_checksums && PASSES=$((PASSES + 1)) || FAILURES=$((FAILURES + 1))
run_case "reflow_burst_detection" test_burst_detection && PASSES=$((PASSES + 1)) || FAILURES=$((FAILURES + 1))
run_case "reflow_latency_bounds" test_latency_bounds && PASSES=$((PASSES + 1)) || FAILURES=$((FAILURES + 1))
run_case "reflow_property_invariants" test_property_invariants && PASSES=$((PASSES + 1)) || FAILURES=$((FAILURES + 1))
run_case "reflow_coalesce_time" test_coalesce_time && PASSES=$((PASSES + 1)) || FAILURES=$((FAILURES + 1))
run_case "reflow_latest_wins" test_latest_wins && PASSES=$((PASSES + 1)) || FAILURES=$((FAILURES + 1))
run_case "reflow_voi_sampling" test_voi_sampling_policy && PASSES=$((PASSES + 1)) || FAILURES=$((FAILURES + 1))

generate_summary "$PASSES" "$FAILURES"

log_info "JSONL log written to: $REFLOW_ENV_JSONL"

exit "$FAILURES"

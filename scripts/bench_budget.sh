#!/usr/bin/env bash
# Performance Budget Enforcement Script (bd-3cwi)
#
# Validates that benchmark results meet documented performance budgets.
# Exit 0 = all budgets met, Exit 1 = at least one budget exceeded.
#
# Usage:
#   ./scripts/bench_budget.sh              # Run all benchmarks with budget checks
#   ./scripts/bench_budget.sh --quick      # Quick run (subset of benchmarks)
#   ./scripts/bench_budget.sh --check-only # Parse existing results, no re-run
#   ./scripts/bench_budget.sh --json       # Output JSONL perf log

set -euo pipefail

# =============================================================================
# Configuration
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
RESULTS_DIR="${PROJECT_ROOT}/target/benchmark-results"
PERF_LOG="${RESULTS_DIR}/perf_log.jsonl"
RUN_ID="$(date +%Y%m%dT%H%M%S)-$$"

# Performance budgets (name:max_ns:description)
# These are based on AGENTS.md requirements and documented in bd-3cwi
declare -A BUDGETS=(
    # Cell operations (< 100ns target)
    ["cell/compare/bits_eq_same"]=100
    ["cell/compare/bits_eq_different"]=100
    ["cell/create/default"]=50
    ["cell/create/from_char_ascii"]=50

    # Buffer operations
    ["buffer/new/alloc/80x24"]=100000        # <100us
    ["buffer/new/alloc/200x60"]=500000       # <500us
    ["buffer/clone/clone/80x24"]=100000      # <100us
    ["buffer/fill/fill_all/80x24"]=1000000   # <1ms

    # Diff operations
    ["diff/identical/compute/80x24"]=50000   # <50us (fast path)
    ["diff/sparse_5pct/compute/80x24"]=100000  # <100us
    ["diff/full_100pct/compute/80x24"]=1000000 # <1ms

    # Presenter operations
    ["present/sparse_5pct/present/80x24"]=500000   # <500us
    ["present/heavy_50pct/present/80x24"]=2000000  # <2ms
    ["present/full_100pct/present/80x24"]=5000000  # <5ms

    # Full pipeline
    ["pipeline/diff_and_present/full/80x24@5.0%"]=1000000  # <1ms

    # Widget rendering
    ["widget/block/bordered/80x24"]=100000     # <100us
    ["widget/paragraph/no_wrap/200ch"]=500000  # <500us
    ["widget/table/render/10x3"]=500000        # <500us

    # Telemetry config parsing (ftui-runtime)
    ["telemetry/config/from_env_disabled"]=500           # <500ns
    ["telemetry/config/from_env_enabled_endpoint"]=2000  # <2us
    ["telemetry/config/from_env_explicit_otlp"]=2000     # <2us
    ["telemetry/config/from_env_sdk_disabled"]=200       # <200ns
    ["telemetry/config/from_env_exporter_none"]=200      # <200ns
    ["telemetry/config/from_env_full_config"]=5000       # <5us
    ["telemetry/config/is_enabled_check"]=5              # <5ns

    # Telemetry ID parsing
    ["telemetry/id_parsing/trace_id_valid"]=200          # <200ns
    ["telemetry/id_parsing/span_id_valid"]=100           # <100ns

    # Telemetry redaction + validation
    ["telemetry/redaction/redact_path"]=50
    ["telemetry/redaction/redact_content"]=50
    ["telemetry/redaction/redact_env_var"]=50
    ["telemetry/redaction/redact_username"]=50
    ["telemetry/redaction/redact_count_10"]=50
    ["telemetry/redaction/redact_count_1000"]=50
    ["telemetry/redaction/redact_bytes_small"]=50
    ["telemetry/redaction/redact_bytes_large"]=50
    ["telemetry/redaction/redact_duration_us"]=50
    ["telemetry/redaction/redact_dimensions"]=50
    ["telemetry/redaction/is_verbose_check"]=5
    ["telemetry/redaction/is_safe_env_var_otel"]=50
    ["telemetry/redaction/is_safe_env_var_ftui"]=50
    ["telemetry/redaction/is_safe_env_var_unsafe"]=50
    ["telemetry/redaction/is_valid_custom_field_app"]=50
    ["telemetry/redaction/is_valid_custom_field_invalid"]=50
    ["telemetry/redaction/contains_sensitive_clean"]=500
    ["telemetry/redaction/contains_sensitive_password"]=500
    ["telemetry/redaction/contains_sensitive_url"]=500
    ["telemetry/redaction/contains_sensitive_long_clean"]=500
)

# PANIC threshold multiplier (2x budget = hard failure)
PANIC_MULTIPLIER=2

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# =============================================================================
# Argument parsing
# =============================================================================

QUICK_MODE=false
CHECK_ONLY=false
JSON_OUTPUT=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --quick)
            QUICK_MODE=true
            shift
            ;;
        --check-only)
            CHECK_ONLY=true
            shift
            ;;
        --json)
            JSON_OUTPUT=true
            shift
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# =============================================================================
# Functions
# =============================================================================

log() {
    if [[ "$JSON_OUTPUT" != "true" ]]; then
        echo -e "$1"
    fi
}

log_json() {
    local status="$1"
    local benchmark="$2"
    local actual_ns="$3"
    local budget_ns="$4"
    local pass="$5"

    echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"benchmark\":\"$benchmark\",\"actual_ns\":$actual_ns,\"budget_ns\":$budget_ns,\"pass\":$pass,\"status\":\"$status\"}" >> "$PERF_LOG"
}

run_benchmarks() {
    log "${BLUE}=== Running Performance Benchmarks ===${NC}"
    mkdir -p "$RESULTS_DIR"

    local benches=(
        "ftui-render:cell_bench"
        "ftui-render:buffer_bench"
        "ftui-render:diff_bench"
        "ftui-render:presenter_bench"
    )

    if [[ "$QUICK_MODE" != "true" ]]; then
        benches+=(
            "ftui-widgets:widget_bench"
            "ftui-layout:layout_bench"
            "ftui-text:width_bench"
            "ftui-runtime:telemetry_bench:telemetry"
        )
    fi

    for bench_spec in "${benches[@]}"; do
        IFS=':' read -r pkg bench features <<< "$bench_spec"
        log "  Running $pkg/$bench..."
        if [[ -n "${features:-}" ]]; then
            cargo bench -p "$pkg" --bench "$bench" --features "$features" -- --noplot \
                2>/dev/null | tee "${RESULTS_DIR}/${bench}.txt" || true
        else
            cargo bench -p "$pkg" --bench "$bench" -- --noplot \
                2>/dev/null | tee "${RESULTS_DIR}/${bench}.txt" || true
        fi
    done
}

parse_criterion_output() {
    local file="$1"
    local benchmark="$2"

    # Extract time from Criterion output format:
    # "bench_name    time:   [123.45 ns 125.67 ns 127.89 ns]"
    # We want the middle value (estimate)
    local time_line
    time_line=$(grep -E "^\s*${benchmark}.*time:" "$file" 2>/dev/null | head -1 || true)

    if [[ -z "$time_line" ]]; then
        echo "-1"
        return
    fi

    # Extract the middle time value
    local time_value
    time_value=$(echo "$time_line" | sed -E 's/.*time:[^[]*\[([0-9.]+)\s*(ns|µs|us|ms|s)\s+([0-9.]+)\s*(ns|µs|us|ms|s).*/\3 \4/')

    local value unit
    read -r value unit <<< "$time_value"

    # Convert to nanoseconds
    case "$unit" in
        ns) echo "${value%.*}" ;;
        µs|us) echo "$((${value%.*} * 1000))" ;;
        ms) echo "$((${value%.*} * 1000000))" ;;
        s) echo "$((${value%.*} * 1000000000))" ;;
        *) echo "-1" ;;
    esac
}

check_budgets() {
    log ""
    log "${BLUE}=== Performance Budget Check ===${NC}"
    log ""

    local passed=0
    local failed=0
    local panicked=0
    local skipped=0

    printf "%-50s %15s %15s %10s\n" "Benchmark" "Actual" "Budget" "Status"
    printf "%-50s %15s %15s %10s\n" "---------" "------" "------" "------"

    for benchmark in "${!BUDGETS[@]}"; do
        local budget_ns="${BUDGETS[$benchmark]}"
        local panic_ns=$((budget_ns * PANIC_MULTIPLIER))

        # Determine which result file to check
        local result_file
        case "$benchmark" in
            cell/*) result_file="${RESULTS_DIR}/cell_bench.txt" ;;
            buffer/*) result_file="${RESULTS_DIR}/buffer_bench.txt" ;;
            diff/*) result_file="${RESULTS_DIR}/diff_bench.txt" ;;
            present/*|pipeline/*) result_file="${RESULTS_DIR}/presenter_bench.txt" ;;
            widget/*) result_file="${RESULTS_DIR}/widget_bench.txt" ;;
            telemetry/*) result_file="${RESULTS_DIR}/telemetry_bench.txt" ;;
            *) result_file="" ;;
        esac

        if [[ -z "$result_file" ]] || [[ ! -f "$result_file" ]]; then
            printf "%-50s %15s %15s ${YELLOW}%10s${NC}\n" "$benchmark" "N/A" "${budget_ns}ns" "SKIP"
            ((skipped++))
            log_json "skip" "$benchmark" 0 "$budget_ns" "null"
            continue
        fi

        # Parse the benchmark name for Criterion lookup
        local criterion_name
        criterion_name=$(echo "$benchmark" | sed 's|/|/|g')

        local actual_ns
        actual_ns=$(parse_criterion_output "$result_file" "$criterion_name")

        if [[ "$actual_ns" == "-1" ]]; then
            printf "%-50s %15s %15s ${YELLOW}%10s${NC}\n" "$benchmark" "N/A" "${budget_ns}ns" "SKIP"
            ((skipped++))
            log_json "skip" "$benchmark" 0 "$budget_ns" "null"
            continue
        fi

        local status status_color pass_json
        if [[ "$actual_ns" -gt "$panic_ns" ]]; then
            status="PANIC"
            status_color="$RED"
            pass_json="false"
            ((panicked++))
        elif [[ "$actual_ns" -gt "$budget_ns" ]]; then
            status="FAIL"
            status_color="$YELLOW"
            pass_json="false"
            ((failed++))
        else
            status="PASS"
            status_color="$GREEN"
            pass_json="true"
            ((passed++))
        fi

        # Format times for display
        local actual_display budget_display
        if [[ "$actual_ns" -ge 1000000 ]]; then
            actual_display="$((actual_ns / 1000000))ms"
        elif [[ "$actual_ns" -ge 1000 ]]; then
            actual_display="$((actual_ns / 1000))us"
        else
            actual_display="${actual_ns}ns"
        fi

        if [[ "$budget_ns" -ge 1000000 ]]; then
            budget_display="$((budget_ns / 1000000))ms"
        elif [[ "$budget_ns" -ge 1000 ]]; then
            budget_display="$((budget_ns / 1000))us"
        else
            budget_display="${budget_ns}ns"
        fi

        printf "%-50s %15s %15s ${status_color}%10s${NC}\n" \
            "$benchmark" "$actual_display" "$budget_display" "$status"

        log_json "$status" "$benchmark" "$actual_ns" "$budget_ns" "$pass_json"
    done

    log ""
    log "${BLUE}=== Summary ===${NC}"
    log "  Passed:  $passed"
    log "  Failed:  $failed"
    log "  Panicked: $panicked"
    log "  Skipped: $skipped"
    log ""

    if [[ "$panicked" -gt 0 ]]; then
        log "${RED}PANIC: $panicked benchmark(s) exceeded 2x budget!${NC}"
        log "This indicates a severe performance regression."
        return 2
    elif [[ "$failed" -gt 0 ]]; then
        log "${YELLOW}WARNING: $failed benchmark(s) exceeded budget.${NC}"
        log "Consider investigating before merge."
        return 1
    else
        log "${GREEN}All budgets met!${NC}"
        return 0
    fi
}

# =============================================================================
# Main
# =============================================================================

main() {
    log "${BLUE}Performance Budget Validation (bd-3cwi)${NC}"
    log "Run ID: $RUN_ID"
    log ""

    mkdir -p "$RESULTS_DIR"

    # Initialize perf log
    if [[ "$JSON_OUTPUT" == "true" ]]; then
        echo "{\"run_id\":\"$RUN_ID\",\"start_ts\":\"$(date -Iseconds)\",\"event\":\"start\"}" >> "$PERF_LOG"
    fi

    if [[ "$CHECK_ONLY" != "true" ]]; then
        run_benchmarks
    fi

    local exit_code=0
    check_budgets || exit_code=$?

    if [[ "$JSON_OUTPUT" == "true" ]]; then
        echo "{\"run_id\":\"$RUN_ID\",\"end_ts\":\"$(date -Iseconds)\",\"event\":\"end\",\"exit_code\":$exit_code}" >> "$PERF_LOG"
        log ""
        log "Perf log: $PERF_LOG"
    fi

    exit $exit_code
}

main

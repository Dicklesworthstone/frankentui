#!/usr/bin/env bash
# Performance Regression Gate (bd-3fc.4)
#
# Compares criterion benchmark results against tests/baseline.json percentile
# budgets. Exits non-zero if any observed mean exceeds the baseline p99 by more
# than the configured threshold_pct.
#
# Usage:
#   ./scripts/perf_regression_gate.sh              # Run benchmarks + check
#   ./scripts/perf_regression_gate.sh --check-only # Parse existing results only
#   ./scripts/perf_regression_gate.sh --quick       # CI-friendly (fast sampling)
#   ./scripts/perf_regression_gate.sh --json        # Emit JSONL report
#   ./scripts/perf_regression_gate.sh --flamegraph  # Generate flamegraphs
#   ./scripts/perf_regression_gate.sh --update      # Update baseline with actuals

set -euo pipefail

# =============================================================================
# Configuration
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
BASELINE_FILE="${PROJECT_ROOT}/tests/baseline.json"
SLO_FILE="${PROJECT_ROOT}/slo.yaml"
RESULTS_DIR="${PROJECT_ROOT}/target/regression-gate"
REPORT_FILE="${RESULTS_DIR}/regression_report.jsonl"
RUN_ID="$(date +%Y%m%dT%H%M%S)-$$"

if command -v rch >/dev/null 2>&1; then
    CARGO=(rch exec -- cargo)
else
    CARGO=(cargo)
fi

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

# =============================================================================
# Argument parsing
# =============================================================================

QUICK_MODE=false
CHECK_ONLY=false
JSON_OUTPUT=false
FLAMEGRAPH=false
UPDATE_BASELINE=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --quick)        QUICK_MODE=true; shift ;;
        --check-only)   CHECK_ONLY=true; shift ;;
        --json)         JSON_OUTPUT=true; shift ;;
        --flamegraph)   FLAMEGRAPH=true; shift ;;
        --update)       UPDATE_BASELINE=true; shift ;;
        -h|--help)
            echo "Usage: $0 [--quick] [--check-only] [--json] [--flamegraph] [--update]"
            echo ""
            echo "  --quick       CI-friendly: fewer samples, faster run"
            echo "  --check-only  Parse existing criterion output without re-running"
            echo "  --json        Emit JSONL structured report to $RESULTS_DIR"
            echo "  --flamegraph  Generate flamegraphs per benchmark (requires cargo-flamegraph)"
            echo "  --update      Update baseline.json with observed actuals"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# =============================================================================
# Helpers
# =============================================================================

log() {
    if [[ "$JSON_OUTPUT" != "true" ]]; then
        echo -e "$1"
    fi
}

json_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

# Format nanoseconds for human display.
format_ns() {
    local ns="$1"
    if [[ "$ns" -ge 1000000 ]]; then
        printf "%.2fms" "$(echo "$ns / 1000000" | bc -l)"
    elif [[ "$ns" -ge 1000 ]]; then
        printf "%.2fus" "$(echo "$ns / 1000" | bc -l)"
    else
        printf "%dns" "$ns"
    fi
}

load_slo_threshold_pct() {
    [[ -f "$SLO_FILE" ]] || return 0
    awk -F: '
        /^[[:space:]]*#/ || NF < 2 { next }
        $1 ~ /^[[:space:]]*regression_threshold[[:space:]]*$/ {
            value = $2
            sub(/[[:space:]]*#.*/, "", value)
            gsub(/[[:space:]]/, "", value)
            if (value ~ /^[0-9]+([.][0-9]+)?$/) {
                printf "%.0f\n", value * 100
                exit
            }
        }
    ' "$SLO_FILE"
}

slo_metric_declared() {
    local metric="$1"
    [[ -n "$metric" ]] || return 1
    [[ -f "$SLO_FILE" ]] || return 1
    awk -v metric="$metric" '
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*metrics:[[:space:]]*$/ { in_metrics = 1; next }
        in_metrics && $0 ~ "^[[:space:]]+" metric ":[[:space:]]*$" { found = 1; exit }
        END { exit(found ? 0 : 1) }
    ' "$SLO_FILE"
}

# Parse criterion text output for a benchmark name.
# Returns: "<mean_ns> <ci_low_ns> <ci_high_ns>" or "-1 -1 -1" if not found.
parse_criterion_stats() {
    local file="$1"
    local benchmark="$2"

    awk -v b="$benchmark" '
        function trim(s) {
            sub(/^[[:space:]]+/, "", s)
            sub(/[[:space:]]+$/, "", s)
            return s
        }
        function to_ns(val, unit,    ns) {
            if (unit == "ps") ns = val / 1000.0
            else if (unit == "ns") ns = val
            else if (unit == "us" || unit == "µs") ns = val * 1000.0
            else if (unit == "ms") ns = val * 1000000.0
            else if (unit == "s") ns = val * 1000000000.0
            else ns = -1
            return ns
        }
        function parse_time_line(line,    m, low, mid, high, low_u, mid_u, high_u, low_ns, mid_ns, high_ns) {
            if (match(line, /\[([0-9.]+)[[:space:]]+([^[:space:]]+)[[:space:]]+([0-9.]+)[[:space:]]+([^[:space:]]+)[[:space:]]+([0-9.]+)[[:space:]]+([^[:space:]]+)\]/, m)) {
                low = m[1] + 0.0;  low_u = m[2]
                mid = m[3] + 0.0;  mid_u = m[4]
                high = m[5] + 0.0; high_u = m[6]
                low_ns = to_ns(low, low_u)
                mid_ns = to_ns(mid, mid_u)
                high_ns = to_ns(high, high_u)
                if (low_ns < 0 || mid_ns < 0 || high_ns < 0) return 0
                printf "%.0f %.0f %.0f\n", mid_ns, low_ns, high_ns
                printed = 1
                return 1
            }
            return 0
        }
        BEGIN { want_next_time = 0; printed = 0; }
        {
            t = trim($0)
            if (index(t, b) == 1) {
                rest = substr(t, length(b) + 1)
                if (rest ~ /^[[:space:]]+time:/) {
                    if (parse_time_line(t)) exit
                }
            }
            if (t == b) {
                want_next_time = 1
                next
            }
            if (want_next_time && $0 ~ /time:/) {
                if (parse_time_line($0)) exit
                want_next_time = 0
            }
        }
        END { if (!printed) print "-1 -1 -1" }
    ' "$file"
}

# =============================================================================
# Benchmark Execution
# =============================================================================

# Collect unique crate:bench pairs from baseline.json.
collect_bench_targets() {
    jq -r '
        to_entries[]
        | select(.key | startswith("_") | not)
        | select((.value.crate // "") != "")
        | select((.value.bench_file // "") != "")
        | select((.value.criterion_name // "") != "")
        | "\(.value.crate):\(.value.bench_file)"
    ' "$BASELINE_FILE" | sort -u
}

run_benchmarks() {
    log "${BLUE}=== Running Regression Gate Benchmarks (${RUN_ID}) ===${NC}"
    mkdir -p "$RESULTS_DIR"

    local targets
    targets=$(collect_bench_targets)

    local criterion_args=(-- --noplot)
    if [[ "$QUICK_MODE" == "true" ]]; then
        criterion_args=(-- --noplot --warm-up-time 0.5 --measurement-time 1 --sample-size 20)
    fi

    while IFS=: read -r pkg bench; do
        log "  ${BOLD}${pkg}/${bench}${NC} ..."

        local output_file="${RESULTS_DIR}/${bench}.txt"
        local stderr_file="${RESULTS_DIR}/${bench}.stderr.txt"

        if ! "${CARGO[@]}" bench -p "$pkg" --bench "$bench" "${criterion_args[@]}" \
                2>"$stderr_file" | tee "$output_file"; then
            log "${RED}  FAILED:${NC} ${pkg}/${bench}"
            log "  See: ${stderr_file}"
            tail -n 20 "$stderr_file" 2>/dev/null || true
            return 1
        fi

        # Optional flamegraph generation.
        if [[ "$FLAMEGRAPH" == "true" ]] && command -v cargo-flamegraph >/dev/null 2>&1; then
            log "  Generating flamegraph for ${bench}..."
            "${CARGO[@]}" flamegraph --bench "$bench" -p "$pkg" \
                -o "${RESULTS_DIR}/${bench}.svg" -- --bench 2>/dev/null || true
        fi
    done <<< "$targets"
}

# =============================================================================
# Regression Check
# =============================================================================

check_regression() {
    log ""
    log "${BLUE}=== Regression Gate Check ===${NC}"
    log ""

    if [[ ! -f "$BASELINE_FILE" ]]; then
        log "${RED}ERROR: Baseline file not found: ${BASELINE_FILE}${NC}"
        return 1
    fi

    local slo_threshold_pct
    slo_threshold_pct="$(load_slo_threshold_pct)"
    local slo_threshold_json="null"
    if [[ -n "$slo_threshold_pct" ]]; then
        slo_threshold_json="$slo_threshold_pct"
    fi

    local passed=0
    local failed=0
    local skipped=0
    local warned=0
    local total=0

    # Initialize JSONL report.
    if [[ "$JSON_OUTPUT" == "true" ]]; then
        : > "$REPORT_FILE"
        echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"event\":\"start\",\"baseline_file\":\"$(json_escape "$BASELINE_FILE")\",\"slo_file\":\"$(json_escape "$SLO_FILE")\",\"slo_threshold_pct\":$slo_threshold_json}" >> "$REPORT_FILE"
    fi

    # Table header.
    printf "${BOLD}%-25s %-40s %12s %12s %8s %8s %10s${NC}\n" \
        "Category" "Criterion Name" "Observed" "p99 Budget" "Delta%" "Thresh%" "Status"
    printf "%-25s %-40s %12s %12s %8s %8s %10s\n" \
        "$(printf '%.0s-' {1..25})" "$(printf '%.0s-' {1..40})" \
        "$(printf '%.0s-' {1..12})" "$(printf '%.0s-' {1..12})" \
        "$(printf '%.0s-' {1..8})" "$(printf '%.0s-' {1..8})" \
        "$(printf '%.0s-' {1..10})"

    # Iterate over baseline entries.
    local keys
    keys=$(jq -r 'to_entries[] | select(.key | startswith("_") | not) | .key' "$BASELINE_FILE")

    while IFS= read -r key; do
        ((total++))

        local criterion_name bench_file p99_ns threshold_pct description slo_metric
        criterion_name=$(jq -r --arg key "$key" '.[$key].criterion_name // ""' "$BASELINE_FILE")
        bench_file=$(jq -r --arg key "$key" '.[$key].bench_file // ""' "$BASELINE_FILE")
        p99_ns=$(jq -r --arg key "$key" '.[$key].p99_ns // 0' "$BASELINE_FILE")
        threshold_pct=$(jq -r --arg key "$key" '.[$key].threshold_pct // 0' "$BASELINE_FILE")
        description=$(jq -r --arg key "$key" '.[$key].description // ""' "$BASELINE_FILE")
        slo_metric=$(jq -r --arg key "$key" '.[$key].slo_metric // ""' "$BASELINE_FILE")

        if [[ -z "$criterion_name" || -z "$bench_file" ]]; then
            local category
            category=$(jq -r --arg key "$key" '.[$key].category // "baseline-only"' "$BASELINE_FILE")
            printf "%-25s %-40s %12s %12s %8s %8s ${YELLOW}%10s${NC}\n" \
                "$key" "$category" "N/A" "$(format_ns "$p99_ns")" "-" "-" "SKIP"
            ((skipped++))
            if [[ "$JSON_OUTPUT" == "true" ]]; then
                echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"category\":\"$key\",\"criterion_name\":null,\"slo_metric\":\"$(json_escape "$slo_metric")\",\"status\":\"skip\",\"reason\":\"non_criterion_baseline\"}" >> "$REPORT_FILE"
            fi
            continue
        fi

        if [[ -z "$slo_metric" ]]; then
            printf "%-25s %-40s %12s %12s %8s %8s ${RED}%10s${NC}\n" \
                "$key" "$criterion_name" "N/A" "$(format_ns "$p99_ns")" "-" "${threshold_pct}%" "SLO_CFG"
            ((failed++))
            if [[ "$JSON_OUTPUT" == "true" ]]; then
                echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"category\":\"$key\",\"criterion_name\":\"$criterion_name\",\"status\":\"fail\",\"reason\":\"missing_slo_metric\"}" >> "$REPORT_FILE"
            fi
            continue
        fi

        if ! slo_metric_declared "$slo_metric"; then
            printf "%-25s %-40s %12s %12s %8s %8s ${RED}%10s${NC}\n" \
                "$key" "$criterion_name" "N/A" "$(format_ns "$p99_ns")" "-" "${threshold_pct}%" "SLO_CFG"
            ((failed++))
            if [[ "$JSON_OUTPUT" == "true" ]]; then
                echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"category\":\"$key\",\"criterion_name\":\"$criterion_name\",\"slo_metric\":\"$(json_escape "$slo_metric")\",\"status\":\"fail\",\"reason\":\"slo_metric_not_found\"}" >> "$REPORT_FILE"
            fi
            continue
        fi

        local effective_threshold_pct
        effective_threshold_pct="$threshold_pct"
        if [[ -n "$slo_threshold_pct" && "$slo_threshold_pct" -lt "$effective_threshold_pct" ]]; then
            effective_threshold_pct="$slo_threshold_pct"
        fi

        # Find the result file.
        local result_file="${RESULTS_DIR}/${bench_file}.txt"
        if [[ ! -f "$result_file" ]] || [[ ! -s "$result_file" ]]; then
            # Also try the bench_budget.sh results directory.
            local alt_file="${PROJECT_ROOT}/target/benchmark-results/${bench_file}.txt"
            if [[ -f "$alt_file" ]] && [[ -s "$alt_file" ]]; then
                result_file="$alt_file"
            else
                printf "%-25s %-40s %12s %12s %8s %8s ${YELLOW}%10s${NC}\n" \
                    "$key" "$criterion_name" "N/A" "$(format_ns "$p99_ns")" "-" "${effective_threshold_pct}%" "SKIP"
                ((skipped++))
                if [[ "$JSON_OUTPUT" == "true" ]]; then
                    echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"category\":\"$key\",\"criterion_name\":\"$criterion_name\",\"slo_metric\":\"$(json_escape "$slo_metric")\",\"status\":\"skip\",\"reason\":\"no_results\",\"threshold_pct\":$threshold_pct,\"effective_threshold_pct\":$effective_threshold_pct,\"slo_threshold_pct\":$slo_threshold_json}" >> "$REPORT_FILE"
                fi
                continue
            fi
        fi

        # Parse criterion output.
        local mean_ns ci_low_ns ci_high_ns
        read -r mean_ns ci_low_ns ci_high_ns <<< "$(parse_criterion_stats "$result_file" "$criterion_name")"

        if [[ "$mean_ns" == "-1" ]]; then
            printf "%-25s %-40s %12s %12s %8s %8s ${YELLOW}%10s${NC}\n" \
                "$key" "$criterion_name" "N/A" "$(format_ns "$p99_ns")" "-" "${effective_threshold_pct}%" "SKIP"
            ((skipped++))
            if [[ "$JSON_OUTPUT" == "true" ]]; then
                echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"category\":\"$key\",\"criterion_name\":\"$criterion_name\",\"slo_metric\":\"$(json_escape "$slo_metric")\",\"status\":\"skip\",\"reason\":\"parse_failed\",\"threshold_pct\":$threshold_pct,\"effective_threshold_pct\":$effective_threshold_pct,\"slo_threshold_pct\":$slo_threshold_json}" >> "$REPORT_FILE"
            fi
            continue
        fi

        # Compute percentage delta from p99 baseline.
        local max_allowed_ns delta_pct status status_color
        max_allowed_ns=$(echo "$p99_ns * (100 + $effective_threshold_pct) / 100" | bc)

        if [[ "$p99_ns" -gt 0 ]]; then
            delta_pct=$(echo "scale=1; ($mean_ns - $p99_ns) * 100 / $p99_ns" | bc)
        else
            delta_pct="0"
        fi

        if [[ "$mean_ns" -gt "$max_allowed_ns" ]]; then
            status="REGRESS"
            status_color="$RED"
            ((failed++))
        elif [[ "$mean_ns" -gt "$p99_ns" ]]; then
            status="WARN"
            status_color="$YELLOW"
            ((warned++))
            ((passed++))
        else
            status="PASS"
            status_color="$GREEN"
            ((passed++))
        fi

        printf "%-25s %-40s %12s %12s %8s %8s ${status_color}%10s${NC}\n" \
            "$key" "$criterion_name" \
            "$(format_ns "$mean_ns")" "$(format_ns "$p99_ns")" \
            "${delta_pct}%" "${effective_threshold_pct}%" "$status"

        if [[ "$JSON_OUTPUT" == "true" ]]; then
            echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"category\":\"$key\",\"criterion_name\":\"$criterion_name\",\"slo_metric\":\"$(json_escape "$slo_metric")\",\"status\":\"$(echo "$status" | tr '[:upper:]' '[:lower:]')\",\"observed_ns\":$mean_ns,\"ci_low_ns\":$ci_low_ns,\"ci_high_ns\":$ci_high_ns,\"p99_baseline_ns\":$p99_ns,\"max_allowed_ns\":$max_allowed_ns,\"delta_pct\":$delta_pct,\"threshold_pct\":$threshold_pct,\"effective_threshold_pct\":$effective_threshold_pct,\"slo_threshold_pct\":$slo_threshold_json,\"description\":\"$(json_escape "$description")\"}" >> "$REPORT_FILE"
        fi

        # Log WARN for regression (per bead spec).
        if [[ "$status" == "REGRESS" ]]; then
            log "  ${RED}WARN:${NC} Regression detected in ${key}: observed $(format_ns "$mean_ns") exceeds p99 $(format_ns "$p99_ns") + ${effective_threshold_pct}% tolerance (SLO metric: ${slo_metric})"
        fi
    done <<< "$keys"

    # Summary.
    log ""
    log "${BLUE}=== Summary ===${NC}"
    log "  Total:      $total"
    log "  Passed:     $passed"
    log "  Regressions: $failed"
    log "  Warned:     $warned"
    log "  Skipped:    $skipped"

    if [[ "$JSON_OUTPUT" == "true" ]]; then
        echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"event\":\"summary\",\"total\":$total,\"passed\":$passed,\"failed\":$failed,\"warned\":$warned,\"skipped\":$skipped,\"slo_file\":\"$(json_escape "$SLO_FILE")\",\"slo_threshold_pct\":$slo_threshold_json}" >> "$REPORT_FILE"
        log ""
        log "Report: $REPORT_FILE"
    fi

    if [[ "$failed" -gt 0 ]]; then
        log ""
        log "${RED}REGRESSION DETECTED: ${failed} benchmark(s) exceeded baseline + threshold.${NC}"
        log "Review the results above and either fix the regression or update the baseline:"
        log "  ${BOLD}./scripts/perf_regression_gate.sh --update${NC}"
        return 1
    else
        log ""
        log "${GREEN}All benchmarks within regression threshold.${NC}"
        return 0
    fi
}

# =============================================================================
# Baseline Update
# =============================================================================

update_baseline() {
    log "${BLUE}=== Updating Baseline with Observed Values ===${NC}"

    if [[ ! -f "$BASELINE_FILE" ]]; then
        log "${RED}ERROR: Baseline file not found: ${BASELINE_FILE}${NC}"
        return 1
    fi

    local keys
    keys=$(jq -r 'to_entries[] | select(.key | startswith("_") | not) | .key' "$BASELINE_FILE")
    local updated=0

    while IFS= read -r key; do
        local criterion_name bench_file
        criterion_name=$(jq -r --arg key "$key" '.[$key].criterion_name // ""' "$BASELINE_FILE")
        bench_file=$(jq -r --arg key "$key" '.[$key].bench_file // ""' "$BASELINE_FILE")
        [[ -n "$criterion_name" && -n "$bench_file" ]] || continue

        local result_file="${RESULTS_DIR}/${bench_file}.txt"
        if [[ ! -f "$result_file" ]]; then
            result_file="${PROJECT_ROOT}/target/benchmark-results/${bench_file}.txt"
        fi
        [[ -f "$result_file" ]] || continue

        local mean_ns ci_low_ns ci_high_ns
        read -r mean_ns ci_low_ns ci_high_ns <<< "$(parse_criterion_stats "$result_file" "$criterion_name")"
        [[ "$mean_ns" == "-1" ]] && continue

        # Update p50 with observed mean. Set p95 = 2x mean, p99 = 4x mean, p999 = 10x mean.
        # These multipliers provide reasonable headroom for normal variance.
        local p50="$mean_ns"
        local p95=$(echo "$mean_ns * 2" | bc)
        local p99=$(echo "$mean_ns * 4" | bc)
        local p999=$(echo "$mean_ns * 10" | bc)

        # Update the baseline file in-place using jq.
        local tmp
        tmp=$(mktemp)
        jq --arg key "$key" \
           --argjson p50 "$p50" --argjson p95 "$p95" \
           --argjson p99 "$p99" --argjson p999 "$p999" \
           '.[$key].p50_ns = $p50 | .[$key].p95_ns = $p95 | .[$key].p99_ns = $p99 | .[$key].p999_ns = $p999 | ._updated = (now | strftime("%Y-%m-%d"))' \
           "$BASELINE_FILE" > "$tmp"
        mv "$tmp" "$BASELINE_FILE"
        ((updated++))

        log "  Updated ${key}: p50=$(format_ns "$p50") p95=$(format_ns "$p95") p99=$(format_ns "$p99") p999=$(format_ns "$p999")"
    done <<< "$keys"

    log ""
    log "${GREEN}Updated ${updated} baseline entries.${NC}"
}

# =============================================================================
# Main
# =============================================================================

main() {
    log "${BLUE}${BOLD}FrankenTUI Performance Regression Gate (bd-3fc.4)${NC}"
    log "Run ID: $RUN_ID"
    log "Baseline: $BASELINE_FILE"
    log ""

    mkdir -p "$RESULTS_DIR"

    if [[ "$CHECK_ONLY" != "true" ]]; then
        run_benchmarks || exit $?
    fi

    if [[ "$UPDATE_BASELINE" == "true" ]]; then
        update_baseline
        exit 0
    fi

    local exit_code=0
    check_regression || exit_code=$?

    exit $exit_code
}

main

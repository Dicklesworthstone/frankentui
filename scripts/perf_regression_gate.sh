#!/usr/bin/env bash
# Performance Regression Gate (bd-3fc.4)
#
# Compares Criterion central estimates against the configured baseline ceilings.
# Missing evidence fails. This is not a measurement of individual-operation tails.
#
# Usage:
#   ./scripts/perf_regression_gate.sh              # Run benchmarks + check
#   ./scripts/perf_regression_gate.sh --check-only # Parse existing results only
#   ./scripts/perf_regression_gate.sh --quick       # CI-friendly (fast sampling)
#   ./scripts/perf_regression_gate.sh --json        # Emit JSONL report
#   ./scripts/perf_regression_gate.sh --flamegraph  # Generate flamegraphs
#   ./scripts/perf_regression_gate.sh --update      # Refused: no raw tail samples

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

# The caller owns the pinned native DSR execution lane.
CARGO=(cargo)

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
            echo "  --update      Refused: Criterion estimates cannot establish tail percentiles"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Refuse before creating directories/logs or changing any baseline. The former
# mean*{2,4,10} updater fabricated tail statistics; no raw-tail input is supported.
if [[ "$UPDATE_BASELINE" == "true" ]]; then
    echo "ERROR: --update requires actual distribution inputs; Criterion central estimates cannot establish p50/p95/p99/p999. No files changed." >&2
    exit 2
fi

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
    awk -v ns="$ns" 'BEGIN {
        if (ns >= 1000000) printf "%.2fms", ns / 1000000
        else if (ns >= 1000) printf "%.2fus", ns / 1000
        else printf "%.3gns", ns
    }'
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
                printf "%.17g\n", value * 100
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
# Returns central/lower/upper estimates in ns; missing or invalid records fail.
parse_criterion_stats() {
    local file="$1"
    local benchmark="$2"

    python3 - "$file" "$benchmark" <<'PY'
import decimal
import math
import pathlib
import re
import sys

path, benchmark = sys.argv[1:]
number = r"(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?"
unit = r"(?:ps|ns|us|µs|μs|ms|s)"
value = rf"({number})\s+({unit})"
times = re.compile(rf"time:\s*\[\s*{value}\s+{value}\s+{value}\s*\]")
header = re.compile(re.escape(benchmark) + r"(?:[ \t]+(time:.*))?")
scales = {"ps": "0.001", "ns": "1", "us": "1000", "µs": "1000",
          "μs": "1000", "ms": "1000000", "s": "1000000000"}

try:
    records = []
    pending = False
    for line in pathlib.Path(path).read_text(encoding="utf-8").splitlines():
        text = line.strip()
        if not text:
            continue
        if pending:
            if not line[:1].isspace() or not text.startswith("time:"):
                raise ValueError("header has no adjacent timing record")
            records.append(text)
            pending = False
            continue
        match = header.fullmatch(text)
        if match:
            if match[1] is None:
                pending = True
            else:
                records.append(match[1])
    if pending or len(records) != 1:
        raise ValueError("expected exactly one complete timing record")
    match = times.fullmatch(records[0])
    if match is None:
        raise ValueError("malformed timing record")
    values = [decimal.Decimal(match[i]) * decimal.Decimal(scales[match[i + 1]])
              for i in (1, 3, 5)]
    if any(not math.isfinite(float(v)) or float(v) <= 0 or v > 2**53 - 1
           for v in values):
        raise ValueError("timing must be finite, positive and at most 2^53-1 ns")
    low, middle, high = values
    if not low <= middle <= high:
        raise ValueError("timing confidence bounds are not ordered")
    print(*(format(v, "f") for v in (middle, low, high)))
except (OSError, UnicodeError, ValueError, decimal.DecimalException) as error:
    print(f"{benchmark}: {error}", file=sys.stderr)
    sys.exit(1)
PY
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
            if ! "${CARGO[@]}" flamegraph --bench "$bench" -p "$pkg" \
                -o "${RESULTS_DIR}/${bench}.svg" -- --bench \
                2>"${RESULTS_DIR}/${bench}.flamegraph.stderr.txt"; then
                echo "ERROR: flamegraph failed; stderr retained at ${RESULTS_DIR}/${bench}.flamegraph.stderr.txt" >&2
                return 1
            fi
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
    # jq keeps the last duplicate key. Reject duplicates before it can discard
    # a configured obligation, including escaped-equivalent and nested keys.
    if ! python3 - "$BASELINE_FILE" <<'PY'
import json
import sys


def unique_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key!r}")
        result[key] = value
    return result


def reject_constant(value):
    raise ValueError(f"non-JSON numeric constant: {value}")


try:
    with open(sys.argv[1], encoding="utf-8") as source:
        json.load(source, object_pairs_hook=unique_keys, parse_constant=reject_constant)
except (OSError, ValueError) as error:
    print(f"ERROR: invalid baseline JSON: {error}", file=sys.stderr)
    sys.exit(1)
PY
    then
        return 3
    fi
    # Missing/null tolerance means zero; false and every other nonnumber fail.
    if ! jq -e '
        type == "object" and
        ([to_entries[] | select(.key | startswith("_") | not)] | length > 0) and
        all(to_entries[] | select(.key | startswith("_") | not);
            (.value | type == "object") and
            (.value.p99_ns | type == "number") and
            .value.p99_ns > 0 and .value.p99_ns <= 9007199254740991 and
            (.value.threshold_pct == null or (.value.threshold_pct | type == "number")) and
            (.value.threshold_pct // 0) >= 0 and (.value.threshold_pct // 0) <= 100)
    ' "$BASELINE_FILE" >/dev/null; then
        echo "ERROR: baseline must contain thresholds with positive finite ns ceilings and valid tolerances" >&2
        return 3
    fi

    local slo_threshold_pct
    if ! slo_threshold_pct=$(load_slo_threshold_pct); then
        echo "ERROR: failed to read SLO threshold" >&2
        return 3
    fi
    local slo_threshold_json="null"
    if [[ -n "$slo_threshold_pct" ]]; then
        slo_threshold_json="$slo_threshold_pct"
    fi

    local passed=0
    local failed=0
    local skipped=0
    local warned=0
    local total=0
    local incomplete=0

    # Initialize JSONL report.
    if [[ "$JSON_OUTPUT" == "true" ]]; then
        : > "$REPORT_FILE"
        echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"event\":\"start\",\"baseline_file\":\"$(json_escape "$BASELINE_FILE")\",\"slo_file\":\"$(json_escape "$SLO_FILE")\",\"slo_threshold_pct\":$slo_threshold_json}" >> "$REPORT_FILE"
    fi

    # Table header.
    printf "${BOLD}%-25s %-40s %12s %12s %8s %8s %10s${NC}\n" \
        "Category" "Criterion Name" "Estimate" "Ceiling" "Delta%" "Thresh%" "Status"
    printf "%-25s %-40s %12s %12s %8s %8s %10s\n" \
        "$(printf '%.0s-' {1..25})" "$(printf '%.0s-' {1..40})" \
        "$(printf '%.0s-' {1..12})" "$(printf '%.0s-' {1..12})" \
        "$(printf '%.0s-' {1..8})" "$(printf '%.0s-' {1..8})" \
        "$(printf '%.0s-' {1..10})"

    # Iterate over baseline entries.
    local keys
    if ! keys=$(jq -r 'to_entries[] | select(.key | startswith("_") | not) | .key' "$BASELINE_FILE"); then
        echo "ERROR: failed to enumerate required thresholds" >&2
        return 3
    fi

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
                "$key" "$category" "N/A" "$(format_ns "$p99_ns")" "-" "-" "INCOMPLETE"
            ((incomplete+=1))
            if [[ "$JSON_OUTPUT" == "true" ]]; then
                echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"category\":\"$key\",\"criterion_name\":null,\"slo_metric\":\"$(json_escape "$slo_metric")\",\"status\":\"incomplete\",\"reason\":\"unbound_required_threshold\"}" >> "$REPORT_FILE"
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
        if [[ -n "$slo_threshold_pct" ]]; then
            if ! effective_threshold_pct=$(awk -v configured="$threshold_pct" -v slo="$slo_threshold_pct" '
                BEGIN { printf "%.17g\n", configured < slo ? configured : slo }
            '); then
                echo "ERROR: failed to resolve SLO tolerance for $key" >&2
                return 3
            fi
        fi

        # Find the result file.
        local result_file="${RESULTS_DIR}/${bench_file}.txt"
        if [[ ! -f "$result_file" ]] || [[ ! -s "$result_file" ]]; then
            printf "%-25s %-40s %12s %12s %8s %8s ${RED}%10s${NC}\n" \
                "$key" "$criterion_name" "N/A" "$(format_ns "$p99_ns")" "-" "${effective_threshold_pct}%" "INCOMPLETE"
            ((incomplete+=1))
            if [[ "$JSON_OUTPUT" == "true" ]]; then
                echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"category\":\"$key\",\"criterion_name\":\"$criterion_name\",\"slo_metric\":\"$(json_escape "$slo_metric")\",\"status\":\"incomplete\",\"reason\":\"no_results\",\"threshold_pct\":$threshold_pct,\"effective_threshold_pct\":$effective_threshold_pct,\"slo_threshold_pct\":$slo_threshold_json}" >> "$REPORT_FILE"
            fi
            continue
        fi

        # Parse criterion output.
        local stats mean_ns ci_low_ns ci_high_ns
        if ! stats=$(parse_criterion_stats "$result_file" "$criterion_name"); then
            printf "%-25s %-40s %12s %12s %8s %8s ${RED}%10s${NC}\n" \
                "$key" "$criterion_name" "N/A" "$(format_ns "$p99_ns")" "-" "${effective_threshold_pct}%" "INCOMPLETE"
            ((incomplete+=1))
            if [[ "$JSON_OUTPUT" == "true" ]]; then
                echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"category\":\"$key\",\"criterion_name\":\"$criterion_name\",\"slo_metric\":\"$(json_escape "$slo_metric")\",\"status\":\"incomplete\",\"reason\":\"parse_failed\",\"threshold_pct\":$threshold_pct,\"effective_threshold_pct\":$effective_threshold_pct,\"slo_threshold_pct\":$slo_threshold_json}" >> "$REPORT_FILE"
            fi
            continue
        fi
        read -r mean_ns ci_low_ns ci_high_ns <<< "$stats"

        # Compare the observed central estimate without rounding away sub-ns
        # regressions. The configured field name does not make this a tail SLO.
        local comparison max_allowed_ns delta_pct status status_color
        if ! comparison=$(awk -v actual="$mean_ns" -v budget="$p99_ns" -v tolerance="$effective_threshold_pct" '
            BEGIN {
                limit = budget * (1 + tolerance / 100)
                status = actual > limit ? "REGRESS" : actual > budget ? "WARN" : "PASS"
                printf "%s %.17g %.17g\n", status, limit, (actual - budget) * 100 / budget
            }
        '); then
            echo "ERROR: regression comparison failed for $key" >&2
            return 3
        fi
        read -r status max_allowed_ns delta_pct <<< "$comparison"

        if [[ "$status" == "REGRESS" ]]; then
            status_color="$RED"
            ((failed++))
        elif [[ "$status" == "WARN" ]]; then
            status_color="$YELLOW"
            ((warned++))
            ((passed++))
        elif [[ "$status" == "PASS" ]]; then
            status_color="$GREEN"
            ((passed++))
        else
            echo "ERROR: invalid regression comparison for $key" >&2
            return 3
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
    log "  Incomplete: $incomplete"

    if [[ "$JSON_OUTPUT" == "true" ]]; then
        echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"event\":\"summary\",\"total\":$total,\"passed\":$passed,\"failed\":$failed,\"warned\":$warned,\"skipped\":$skipped,\"incomplete\":$incomplete,\"slo_file\":\"$(json_escape "$SLO_FILE")\",\"slo_threshold_pct\":$slo_threshold_json}" >> "$REPORT_FILE"
        log ""
        log "Report: $REPORT_FILE"
    fi

    if [[ "$total" -eq 0 || "$incomplete" -gt 0 || $((passed + failed)) -ne "$total" ]]; then
        log "${RED}INCOMPLETE: every configured threshold requires bound, valid evidence.${NC}"
        return 3
    elif [[ "$failed" -gt 0 ]]; then
        log ""
        log "${RED}REGRESSION DETECTED: ${failed} benchmark(s) exceeded baseline + threshold.${NC}"
        log "Review the retained results and fix the regression; estimates cannot justify rewriting tail baselines."
        return 1
    else
        log ""
        log "${GREEN}All configured central-estimate checks complete and within their ceilings.${NC}"
        log "This does not establish individual-operation p99 or runtime lifecycle SLOs."
        return 0
    fi
}

# =============================================================================
# Main
# =============================================================================

main() {
    log "${BLUE}${BOLD}FrankenTUI Performance Regression Gate (bd-3fc.4)${NC}"
    log "Run ID: $RUN_ID"
    log "Baseline: $BASELINE_FILE"
    log ""

    for dependency in python3 awk jq; do
        if ! command -v "$dependency" >/dev/null 2>&1; then
            echo "ERROR: required gate dependency not found: $dependency" >&2
            exit 3
        fi
    done

    mkdir -p "$RESULTS_DIR"

    if [[ "$CHECK_ONLY" != "true" ]]; then
        run_benchmarks || exit $?
    fi

    local exit_code=0
    check_regression || exit_code=$?

    exit $exit_code
}

main

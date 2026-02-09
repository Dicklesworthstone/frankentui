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
#   ./scripts/bench_budget.sh --json       # Output JSONL perf + confidence logs

set -euo pipefail

# =============================================================================
# Configuration
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
RESULTS_DIR="${PROJECT_ROOT}/target/benchmark-results"
PERF_LOG="${RESULTS_DIR}/perf_log.jsonl"
CONFIDENCE_LOG="${RESULTS_DIR}/perf_confidence.jsonl"
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
    ["pipeline/diff_and_present/full/80x24@5%"]=1000000  # <1ms

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

    # ---------------------------------------------------------------------
    # FrankenTerm core parser throughput (bd-lff4p.5.5)
    # ---------------------------------------------------------------------
    #
    # NOTE: These are intentionally *loose* budgets meant to catch only
    # significant regressions (multi-x slowdowns), not micro-noise.
    ["parser_throughput/feed_vec/build_log_v1"]=4000
    ["parser_throughput/feed_vec/dense_sgr_v1"]=3500
    ["parser_throughput/feed_vec/markdownish_v1"]=1500
    ["parser_throughput/feed_vec/unicode_heavy_v1"]=1000

    # Larger parser corpora (64 KiB streams)
    ["parser_throughput_large/feed_vec/sgr_64k_v1"]=1200000
    ["parser_throughput_large/feed_vec/cursor_64k_v1"]=1400000
    ["parser_throughput_large/feed_vec/utf8_64k_v1"]=1000000
    ["parser_throughput_large/feed_vec/ascii_64k_v1"]=1200000

    # Patch generation/apply costs
    ["patch_diff_apply/diff_dirty/2000_cells"]=2500
    ["patch_diff_apply/apply_forward_and_back/2000_cells"]=1000

    # End-to-end parser + apply path
    ["full_pipeline/parse_and_apply/sgr_64k_v1"]=4000000
    ["full_pipeline/parse_and_apply/cursor_64k_v1"]=5000000
    ["full_pipeline/parse_and_apply/utf8_64k_v1"]=3000000
    ["full_pipeline/parse_and_apply/ascii_64k_v1"]=3500000

    # Resize storm and scrollback footprint probes
    ["resize_storm/resize_with_scrollback/120x40_120x52"]=30000000
    ["resize_storm/resize_with_scrollback/80x24_200x60"]=30000000
    ["scrollback_memory/estimate_bytes_1k_120cols"]=100000

    # ---------------------------------------------------------------------
    # FrankenTerm web CPU-side frame-time harness (bd-lff4p.5.5)
    # ---------------------------------------------------------------------
    ["web/frame_harness_stats/sparse_5pct/80x24"]=20000
    ["web/frame_harness_stats/heavy_50pct/80x24"]=30000
    ["web/frame_harness_stats/sparse_5pct/120x40"]=40000
    ["web/frame_harness_stats/heavy_50pct/120x40"]=40000

    # Glyph atlas cache (bd-lff4p.2.4)
    ["web/glyph_atlas_cache/miss_insert_single"]=5000
    ["web/glyph_atlas_cache/hit_hot_path"]=250
    ["web/glyph_atlas_cache/eviction_cycle_3keys_budget2"]=1500
)

# PANIC threshold multiplier (2x budget = hard failure)
PANIC_MULTIPLIER=2

# Expected-loss matrix for confidence hints:
# - false positive: engineer time wasted investigating noise
# - false negative: shipping a real perf regression
LOSS_FALSE_POSITIVE=1
LOSS_FALSE_NEGATIVE=5

# Host metadata for confidence ledger provenance.
HOST_OS="$(uname -s 2>/dev/null || echo unknown)"
HOST_ARCH="$(uname -m 2>/dev/null || echo unknown)"
HOST_CPU_MODEL="$(
    awk -F: '/^model name[[:space:]]*:/ {sub(/^[[:space:]]+/, "", $2); print $2; exit}' /proc/cpuinfo 2>/dev/null ||
        sysctl -n machdep.cpu.brand_string 2>/dev/null ||
        echo unknown
)"
HOST_CORES_RAW="$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || echo "")"
if [[ "$HOST_CORES_RAW" =~ ^[0-9]+$ ]]; then
    HOST_CPU_CORES="$HOST_CORES_RAW"
else
    HOST_CPU_CORES="null"
fi

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
RERUN_ON_FAIL=false

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
        --rerun-on-fail)
            RERUN_ON_FAIL=true
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

json_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

log_json() {
    local status="$1"
    local benchmark="$2"
    local actual_ns="$3"
    local budget_ns="$4"
    local pass="$5"

    echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"benchmark\":\"$benchmark\",\"actual_ns\":$actual_ns,\"budget_ns\":$budget_ns,\"pass\":$pass,\"status\":\"$status\"}" >> "$PERF_LOG"
}

log_confidence_json() {
    local benchmark="$1"
    local status="$2"
    local actual_ns="$3"
    local budget_ns="$4"
    local ci_low_ns="$5"
    local ci_high_ns="$6"
    local sigma_ns="$7"
    local z_score="$8"
    local p_regression="$9"
    local e_value="${10}"
    local bayes_factor="${11}"
    local loss_block="${12}"
    local loss_allow="${13}"
    local decision="${14}"
    local hint="${15}"
    local ci_width_ns="${16}"
    local relative_ci_width="${17}"
    local variance_ns2="${18}"
    local os_json arch_json cpu_json
    os_json="$(json_escape "$HOST_OS")"
    arch_json="$(json_escape "$HOST_ARCH")"
    cpu_json="$(json_escape "$HOST_CPU_MODEL")"

    echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"benchmark\":\"$benchmark\",\"status\":\"$status\",\"actual_ns\":$actual_ns,\"budget_ns\":$budget_ns,\"ci_low_ns\":$ci_low_ns,\"ci_high_ns\":$ci_high_ns,\"ci_width_ns\":$ci_width_ns,\"relative_ci_width\":$relative_ci_width,\"sigma_ns\":$sigma_ns,\"variance_ns2\":$variance_ns2,\"z_score\":$z_score,\"posterior_prob_regression\":$p_regression,\"e_value\":$e_value,\"bayes_factor_10\":$bayes_factor,\"loss_block\":$loss_block,\"loss_allow\":$loss_allow,\"decision\":\"$decision\",\"confidence_hint\":\"$hint\",\"loss_matrix\":{\"false_positive\":$LOSS_FALSE_POSITIVE,\"false_negative\":$LOSS_FALSE_NEGATIVE},\"hardware\":{\"os\":\"$os_json\",\"arch\":\"$arch_json\",\"cpu_model\":\"$cpu_json\",\"cpu_cores\":$HOST_CPU_CORES}}" >> "$CONFIDENCE_LOG"
}

log_confidence_summary() {
    local passed="$1"
    local failed="$2"
    local panicked="$3"
    local skipped="$4"
    local likely_regression="$5"
    local likely_noise="$6"
    local uncertain="$7"
    local os_json arch_json cpu_json
    os_json="$(json_escape "$HOST_OS")"
    arch_json="$(json_escape "$HOST_ARCH")"
    cpu_json="$(json_escape "$HOST_CPU_MODEL")"

    echo "{\"run_id\":\"$RUN_ID\",\"ts\":\"$(date -Iseconds)\",\"event\":\"summary\",\"totals\":{\"passed\":$passed,\"failed\":$failed,\"panicked\":$panicked,\"skipped\":$skipped},\"confidence_hints\":{\"likely_regression\":$likely_regression,\"likely_noise\":$likely_noise,\"uncertain\":$uncertain},\"loss_matrix\":{\"false_positive\":$LOSS_FALSE_POSITIVE,\"false_negative\":$LOSS_FALSE_NEGATIVE},\"hardware\":{\"os\":\"$os_json\",\"arch\":\"$arch_json\",\"cpu_model\":\"$cpu_json\",\"cpu_cores\":$HOST_CPU_CORES}}" >> "$CONFIDENCE_LOG"
}

run_benchmarks() {
    log "${BLUE}=== Running Performance Benchmarks ===${NC}"
    mkdir -p "$RESULTS_DIR"
    # Avoid stale results from previous runs affecting budget checks.
    : > "${RESULTS_DIR}/cell_bench.txt"
    : > "${RESULTS_DIR}/buffer_bench.txt"
    : > "${RESULTS_DIR}/diff_bench.txt"
    : > "${RESULTS_DIR}/presenter_bench.txt"
    : > "${RESULTS_DIR}/widget_bench.txt"
    : > "${RESULTS_DIR}/telemetry_bench.txt"
    : > "${RESULTS_DIR}/parser_patch_bench.txt"
    : > "${RESULTS_DIR}/renderer_bench.txt"
    : > "${PERF_LOG}"
    : > "${CONFIDENCE_LOG}"

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
    else
        # Focused perf gates: parser throughput + web patch-pipeline frame-time harness.
        benches+=(
            "frankenterm-core:parser_patch_bench"
            "frankenterm-web:renderer_bench"
        )
    fi

    for bench_spec in "${benches[@]}"; do
        IFS=':' read -r pkg bench features <<< "$bench_spec"
        log "  Running $pkg/$bench..."

        # Default Criterion args.
        local bench_args=(-- --noplot)
        if [[ "$QUICK_MODE" == "true" ]]; then
            # CI-friendly: keep perf gates fast and stable.
            bench_args=(-- --noplot --warm-up-time 0.1 --measurement-time 0.1 --sample-size 10)
        fi
        local bench_filters=("")
        if [[ "$pkg" == "frankenterm-web" && "$bench" == "renderer_bench" && "$QUICK_MODE" == "true" ]]; then
            # Quick-mode still validates web frame-time harness and glyph-atlas cache gates.
            bench_filters=("frame_harness_stats" "glyph_atlas_cache")
        fi

        local append_mode=false
        for bench_filter in "${bench_filters[@]}"; do
            local run_args=("${bench_args[@]}")
            if [[ -n "$bench_filter" ]]; then
                run_args+=("$bench_filter")
                log "    Filter: $bench_filter"
            fi

            local stderr_file="${RESULTS_DIR}/${bench}${bench_filter:+.${bench_filter}}.stderr.txt"
            local tee_args=()
            if [[ "$append_mode" == "true" ]]; then
                tee_args=(-a)
            fi

            if [[ -n "${features:-}" ]]; then
                if ! cargo bench -p "$pkg" --bench "$bench" --features "$features" "${run_args[@]}" \
                    2>"$stderr_file" | tee "${tee_args[@]}" "${RESULTS_DIR}/${bench}.txt"; then
                    log "${RED}Benchmark failed:${NC} $pkg/$bench"
                    log "  stderr: $stderr_file"
                    tail -n 200 "$stderr_file" || true
                    return 1
                fi
            else
                if ! cargo bench -p "$pkg" --bench "$bench" "${run_args[@]}" \
                    2>"$stderr_file" | tee "${tee_args[@]}" "${RESULTS_DIR}/${bench}.txt"; then
                    log "${RED}Benchmark failed:${NC} $pkg/$bench"
                    log "  stderr: $stderr_file"
                    tail -n 200 "$stderr_file" || true
                    return 1
                fi
            fi

            append_mode=true
        done
    done
}

snapshot_results() {
    local suffix="$1"
    for name in \
        cell_bench buffer_bench diff_bench presenter_bench widget_bench telemetry_bench \
        parser_patch_bench renderer_bench
    do
        local src="${RESULTS_DIR}/${name}.txt"
        if [[ -f "$src" ]]; then
            cp -f "$src" "${RESULTS_DIR}/${name}.${suffix}.txt"
        fi
    done

    if [[ -f "$PERF_LOG" ]]; then
        cp -f "$PERF_LOG" "${RESULTS_DIR}/perf_log.${suffix}.jsonl"
    fi
    if [[ -f "$CONFIDENCE_LOG" ]]; then
        cp -f "$CONFIDENCE_LOG" "${RESULTS_DIR}/perf_confidence.${suffix}.jsonl"
    fi
}

parse_criterion_stats() {
    local file="$1"
    local benchmark="$2"

    # Criterion output has two common shapes:
    #
    # 1) Single-line:
    #    "bench/name    time:   [1.23 ns 1.45 ns 1.67 ns]"
    #
    # 2) Multi-line (often when throughput is enabled):
    #    "bench/name"
    #    "            time:   [1.23 us 1.45 us 1.67 us]"
    #
    # We parse the lower/middle/upper estimates and return integer nanoseconds
    # as: "<middle_ns> <low_ns> <high_ns>". "-1 -1 -1" means not found.
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
            # Extract lower/middle/upper estimates from "[a unit b unit c unit]".
            if (match(line, /\[([0-9.]+)[[:space:]]+([^[:space:]]+)[[:space:]]+([0-9.]+)[[:space:]]+([^[:space:]]+)[[:space:]]+([0-9.]+)[[:space:]]+([^[:space:]]+)\]/, m)) {
                low = m[1] + 0.0
                low_u = m[2]
                mid = m[3] + 0.0
                mid_u = m[4]
                high = m[5] + 0.0
                high_u = m[6]
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

            # One-line format: "<bench>  time: [..]"
            if (index(t, b) == 1) {
                rest = substr(t, length(b) + 1)
                if (rest ~ /^[[:space:]]+time:/) {
                    if (parse_time_line(t)) exit
                }
            }

            # Multi-line format: "<bench>" then later "time: [..]"
            if (t == b) {
                want_next_time = 1
                next
            }
            if (want_next_time && $0 ~ /time:/) {
                if (parse_time_line($0)) exit
                want_next_time = 0
            }
        }
        END {
            if (!printed) print "-1 -1 -1"
        }
    ' "$file"
}

compute_confidence_metrics() {
    local status="$1"
    local actual_ns="$2"
    local budget_ns="$3"
    local ci_low_ns="$4"
    local ci_high_ns="$5"

    awk -v status="$status" -v actual="$actual_ns" -v budget="$budget_ns" -v low="$ci_low_ns" -v high="$ci_high_ns" \
        -v loss_fp="$LOSS_FALSE_POSITIVE" -v loss_fn="$LOSS_FALSE_NEGATIVE" '
        function clamp(v, lo, hi) {
            if (v < lo) return lo
            if (v > hi) return hi
            return v
        }
        BEGIN {
            sigma = (high - low) / 3.92
            ci_width = high - low
            if (ci_width < 0) ci_width = 0
            relative_ci_width = (budget > 0) ? (ci_width / budget) : 0
            # Guard against unrealistically tiny CI widths in short criterion runs.
            min_sigma = budget * 0.01
            if (min_sigma < 1.0) min_sigma = 1.0
            if (sigma < min_sigma) sigma = min_sigma
            variance = sigma * sigma

            delta = actual - budget
            z = delta / sigma
            delta_reg = delta
            if (delta_reg < 0.0) delta_reg = 0.0

            # Logistic approximation of Normal CDF.
            logit_arg = clamp(-1.702 * z, -60.0, 60.0)
            p_reg = 1.0 / (1.0 + exp(logit_arg))

            # One-step e-value (nonnegative evidence for regression).
            z_reg = delta_reg / sigma
            lambda = z_reg
            if (lambda > 1.0) lambda = 1.0
            e_arg = clamp((lambda * z_reg) - ((lambda * lambda) / 2.0), -60.0, 60.0)
            e_value = exp(e_arg)

            # Bayes factor BF10 using Gaussian prior on regression delta.
            tau = budget * 0.05
            if (tau < sigma) tau = sigma
            bf_arg = (delta_reg * delta_reg * tau * tau) \
                / (2.0 * sigma * sigma * ((sigma * sigma) + (tau * tau)))
            bf_arg = clamp(bf_arg, -60.0, 60.0)
            bf10 = sqrt((sigma * sigma) / ((sigma * sigma) + (tau * tau))) \
                * exp(bf_arg)

            loss_block = (1.0 - p_reg) * loss_fp
            loss_allow = p_reg * loss_fn
            decision = (loss_block <= loss_allow) ? "block" : "allow"

            hint = "pass"
            if (status == "PANIC") {
                hint = "likely_regression"
            } else if (status == "FAIL") {
                hint = "uncertain"
                if (p_reg >= 0.95 || e_value >= 8.0 || bf10 >= 10.0) {
                    hint = "likely_regression"
                } else if (p_reg <= 0.50 && e_value <= 1.50 && bf10 <= 1.50) {
                    hint = "likely_noise"
                }
            }

            printf "%.3f %.3f %.6f %.6f %.6f %.6f %.6f %s %s %.3f %.6f %.3f\n",
                sigma, z, p_reg, e_value, bf10, loss_block, loss_allow, decision, hint, ci_width, relative_ci_width, variance
        }
    '
}

check_budgets() {
    log ""
    log "${BLUE}=== Performance Budget Check ===${NC}"
    log ""

    local passed=0
    local failed=0
    local panicked=0
    local skipped=0
    local likely_noise=0
    local likely_regression=0
    local uncertain=0

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
            parser_throughput/*|parser_throughput_large/*|patch_diff_apply/*|parser_action_mix/*|full_pipeline/*|resize_storm/*|scrollback_memory/*) result_file="${RESULTS_DIR}/parser_patch_bench.txt" ;;
            web/*) result_file="${RESULTS_DIR}/renderer_bench.txt" ;;
            *) result_file="" ;;
        esac

        if [[ -z "$result_file" ]] || [[ ! -f "$result_file" ]]; then
            printf "%-50s %15s %15s ${YELLOW}%10s${NC}\n" "$benchmark" "N/A" "${budget_ns}ns" "SKIP"
            ((skipped++))
            log_json "skip" "$benchmark" 0 "$budget_ns" "null"
            log_confidence_json "$benchmark" "skip" 0 "$budget_ns" "null" "null" "null" "null" "null" "null" "null" "null" "null" "allow" "insufficient_data" "null" "null" "null"
            continue
        fi

        # Parse the benchmark name for Criterion lookup
        local criterion_name
        criterion_name=$(echo "$benchmark" | sed 's|/|/|g')

        local actual_ns ci_low_ns ci_high_ns
        read -r actual_ns ci_low_ns ci_high_ns <<< "$(parse_criterion_stats "$result_file" "$criterion_name")"

        if [[ "$actual_ns" == "-1" ]]; then
            printf "%-50s %15s %15s ${YELLOW}%10s${NC}\n" "$benchmark" "N/A" "${budget_ns}ns" "SKIP"
            ((skipped++))
            log_json "skip" "$benchmark" 0 "$budget_ns" "null"
            log_confidence_json "$benchmark" "skip" 0 "$budget_ns" "null" "null" "null" "null" "null" "null" "null" "null" "null" "allow" "insufficient_data" "null" "null" "null"
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

        local sigma_ns z_score p_regression e_value bayes_factor loss_block loss_allow decision hint ci_width_ns relative_ci_width variance_ns2
        read -r sigma_ns z_score p_regression e_value bayes_factor loss_block loss_allow decision hint ci_width_ns relative_ci_width variance_ns2 <<< \
            "$(compute_confidence_metrics "$status" "$actual_ns" "$budget_ns" "$ci_low_ns" "$ci_high_ns")"

        case "$hint" in
            likely_noise) ((likely_noise++)) ;;
            likely_regression) ((likely_regression++)) ;;
            *) ((uncertain++)) ;;
        esac

        log_json "$status" "$benchmark" "$actual_ns" "$budget_ns" "$pass_json"
        log_confidence_json "$benchmark" "$status" "$actual_ns" "$budget_ns" \
            "$ci_low_ns" "$ci_high_ns" "$sigma_ns" "$z_score" "$p_regression" \
            "$e_value" "$bayes_factor" "$loss_block" "$loss_allow" "$decision" "$hint" \
            "$ci_width_ns" "$relative_ci_width" "$variance_ns2"
    done

    log ""
    log "${BLUE}=== Summary ===${NC}"
    log "  Passed:  $passed"
    log "  Failed:  $failed"
    log "  Panicked: $panicked"
    log "  Skipped: $skipped"
    log "  Confidence hints: likely_regression=$likely_regression likely_noise=$likely_noise uncertain=$uncertain"
    log ""
    log_confidence_summary "$passed" "$failed" "$panicked" "$skipped" "$likely_regression" "$likely_noise" "$uncertain"

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
    : > "$PERF_LOG"
    : > "$CONFIDENCE_LOG"

    # Initialize perf log
    if [[ "$JSON_OUTPUT" == "true" ]]; then
        echo "{\"run_id\":\"$RUN_ID\",\"start_ts\":\"$(date -Iseconds)\",\"event\":\"start\"}" >> "$PERF_LOG"
    fi

    if [[ "$CHECK_ONLY" != "true" ]]; then
        run_benchmarks
    fi

    local exit_code=0
    check_budgets || exit_code=$?

    if [[ "$exit_code" -eq 1 ]] && [[ "$RERUN_ON_FAIL" == "true" ]] && [[ "$CHECK_ONLY" != "true" ]]; then
        log ""
        log "${YELLOW}Budget exceeded; rerunning once to reduce false positives...${NC}"
        snapshot_results "run1"
        run_benchmarks
        exit_code=0
        check_budgets || exit_code=$?
        if [[ "$exit_code" -eq 0 ]]; then
            log ""
            log "${GREEN}Rerun passed. Treating initial failure as noise.${NC}"
            log "Saved first-run artifacts under: ${RESULTS_DIR}/*.run1.txt and perf_log.run1.jsonl"
        fi
    fi

    if [[ "$JSON_OUTPUT" == "true" ]]; then
        echo "{\"run_id\":\"$RUN_ID\",\"end_ts\":\"$(date -Iseconds)\",\"event\":\"end\",\"exit_code\":$exit_code}" >> "$PERF_LOG"
        log ""
        log "Perf log: $PERF_LOG"
        log "Confidence log: $CONFIDENCE_LOG"
    fi

    exit $exit_code
}

main

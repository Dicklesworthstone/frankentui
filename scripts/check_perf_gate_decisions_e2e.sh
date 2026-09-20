#!/usr/bin/env bash
# Drive scripts/perf_regression_gate.sh on fixture inputs and assert its
# verdicts (bd-g00-root-epic-ewths.31.5 item 2).
#
# The gate's decision logic is the thing standing between a real regression and
# a green run, and nothing exercised it. Every existing check feeds it whatever
# the machine measured, so a pass told you the machine was fast, not that the
# gate would have caught a slow one.
#
# These fixtures are synthetic: a hand-written baseline, a hand-written
# slo.yaml and hand-written criterion output. Nothing is benchmarked, nothing
# is timed, and the same inputs always produce the same verdict, so a failure
# here is a change in the gate's logic rather than a change in the weather.
#
# Usage:
#   scripts/check_perf_gate_decisions_e2e.sh [run-root]
#
# RUN_ROOT defaults under $TMPDIR and is NEVER deleted, by this script or
# anything it calls (Rule 1 in AGENTS.md). Each case gets its own directory, so
# a failing case leaves its exact inputs behind to look at.

set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="${ROOT_DIR}/scripts/perf_regression_gate.sh"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-${TMPDIR:-/tmp}/ftui_perf_gate_decisions/${TIMESTAMP_UTC}}"
RESULTS_TSV="${RUN_ROOT}/case_results.tsv"

mkdir -p "${RUN_ROOT}"
: > "${RESULTS_TSV}"

PASSED=0
FAILED=0

# The gate's own exit contract, from its verdict block:
#   0 = every configured threshold bound and within its ceiling
#   1 = at least one regression
#   3 = incomplete — a threshold with no bound, valid evidence
readonly EXIT_OK=0
readonly EXIT_REGRESSION=1
readonly EXIT_INCOMPLETE=3

# write_case <name> <observed_ns|-> <p99_ns> <threshold_pct> <slo_metric> <declare_slo>
# Builds a self-contained fixture tree and echoes its directory.
write_case() {
    local name="$1" observed="$2" p99="$3" threshold="$4" metric="$5" declare_slo="$6"
    local dir="${RUN_ROOT}/${name}"
    mkdir -p "${dir}/results"

    cat > "${dir}/baseline.json" <<JSON
{
  "_comment": "synthetic fixture for ${name}",
  "fixture_row": {
    "description": "fixture",
    "criterion_name": "fixture/group/compute/80x24",
    "bench_file": "fixture_bench",
    "crate": "ftui-render",
    "slo_metric": "${metric}",
    "p50_ns": 1,
    "p95_ns": 1,
    "p99_ns": ${p99},
    "p999_ns": 1,
    "threshold_pct": ${threshold}
  }
}
JSON

    if [[ "$declare_slo" == "yes" ]]; then
        cat > "${dir}/slo.yaml" <<YAML
metrics:
  ${metric}:
    metric_type: latency
    max_value: 1000000.0
    max_ratio: 1.30
    safe_mode_trigger: false
YAML
    else
        printf 'metrics:\n' > "${dir}/slo.yaml"
    fi

    # Criterion text: an id line, then an adjacent indented timing record.
    if [[ "$observed" != "-" ]]; then
        {
            printf 'fixture/group/compute/80x24\n'
            printf '                        time:   [%s ns %s ns %s ns]\n' \
                "$observed" "$observed" "$observed"
        } > "${dir}/results/fixture_bench.txt"
    fi
    echo "$dir"
}

# expect <name> <expected_exit> <dir>
expect() {
    local name="$1" expected="$2" dir="$3"
    PERF_GATE_BASELINE_FILE="${dir}/baseline.json" \
    PERF_GATE_SLO_FILE="${dir}/slo.yaml" \
    PERF_GATE_RESULTS_DIR="${dir}/results" \
        bash "$GATE" --check-only > "${dir}/gate.log" 2>&1
    local actual=$?
    if [[ "$actual" == "$expected" ]]; then
        PASSED=$((PASSED + 1))
        printf '%s\tpass\texpected=%s\tactual=%s\n' "$name" "$expected" "$actual" >> "${RESULTS_TSV}"
    else
        FAILED=$((FAILED + 1))
        printf '%s\tFAIL\texpected=%s\tactual=%s\n' "$name" "$expected" "$actual" >> "${RESULTS_TSV}"
        echo "FAIL ${name}: expected exit ${expected}, got ${actual}" >&2
        echo "  inputs and output: ${dir}" >&2
        tail -5 "${dir}/gate.log" >&2
    fi
}

# 1. Comfortably inside the budget.
expect "pass_within_budget" "$EXIT_OK" \
    "$(write_case pass_within_budget 900 1000 10 fixture_metric yes)"

# 2. Over the baseline but inside the tolerance. The gate calls this WARN and
#    counts it as passed; pinned because "slower than baseline" not failing is
#    a deliberate choice that looks like a bug from the outside.
expect "warn_over_baseline_within_tolerance" "$EXIT_OK" \
    "$(write_case warn_over_baseline_within_tolerance 1050 1000 10 fixture_metric yes)"

# 3. The boundary. limit = budget * (1 + tolerance/100) = 1100 exactly, and the
#    comparison is `actual > limit`, so 1100 is still not a regression.
expect "boundary_exactly_at_limit_is_not_a_regression" "$EXIT_OK" \
    "$(write_case boundary_exactly_at_limit_is_not_a_regression 1100 1000 10 fixture_metric yes)"

# 4. One nanosecond past the limit is.
expect "boundary_one_ns_past_limit_regresses" "$EXIT_REGRESSION" \
    "$(write_case boundary_one_ns_past_limit_regresses 1101 1000 10 fixture_metric yes)"

# 5. A real regression.
expect "regression_far_over_budget" "$EXIT_REGRESSION" \
    "$(write_case regression_far_over_budget 5000 1000 10 fixture_metric yes)"

# 6. No results file at all. Must be incomplete, never a pass: this is the case
#    where a bench silently stopped running.
expect "incomplete_when_results_file_absent" "$EXIT_INCOMPLETE" \
    "$(write_case incomplete_when_results_file_absent - 1000 10 fixture_metric yes)"

# 7. A results file that does not contain this id — the shape of bd-kfv6f,
#    where a baseline row names a benchmark criterion never emits.
incomplete_dir="$(write_case incomplete_when_id_absent_from_results 900 1000 10 fixture_metric yes)"
printf 'some/other/benchmark\n                        time:   [900 ns 900 ns 900 ns]\n' \
    > "${incomplete_dir}/results/fixture_bench.txt"
expect "incomplete_when_id_absent_from_results" "$EXIT_INCOMPLETE" "$incomplete_dir"

# 8. An SLO metric the SLO file does not declare. Fails rather than passing
#    quietly, so a budget cannot be enforced against a metric nobody defined.
expect "fails_when_slo_metric_undeclared" "$EXIT_REGRESSION" \
    "$(write_case fails_when_slo_metric_undeclared 900 1000 10 undeclared_metric no)"

echo
printf '{"kind":"summary","tool":"check_perf_gate_decisions_e2e","passed":%d,"failed":%d,"run_root":"%s"}\n' \
    "$PASSED" "$FAILED" "$RUN_ROOT"
echo "cases: ${RESULTS_TSV}"
[[ ${FAILED} -eq 0 ]]

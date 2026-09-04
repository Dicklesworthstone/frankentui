#!/bin/bash
set -euo pipefail

# Common helpers for E2E scripts.
#
# Usage:
#   source "$LIB_DIR/common.sh"
#   source "$LIB_DIR/logging.sh"   # optional JSONL helpers
#   e2e_fixture_init "my_suite"    # sets E2E_RUN_ID, E2E_SEED, E2E_TIME_STEP_MS
#   env_json="$(e2e_env_json)"     # JSON-friendly env snapshot
#   E2E_SELF_TEST=1 e2e_fixture_init "self_test"  # verify deterministic counters
#
# Conventions:
# - Deterministic mode uses E2E_DETERMINISTIC=1 (default) with E2E_SEED (default 0).
# - Non-deterministic mode uses a random seed unless E2E_SEED is explicitly set.
# - Exports FTUI_* seed/determinism vars for demo/harness/test binaries.
# - Exports STORM_SEED and E2E_CONTEXT_SEED for scripts that key off storm/context seeds.
# - E2E_SELF_TEST=1 runs a deterministic counter check (requires logging.sh).

E2E_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_ROOT="$(cd "$E2E_LIB_DIR/.." && pwd)"
PROJECT_ROOT="$(cd "$E2E_ROOT/../.." && pwd)"

require_cmd() {
    local cmd="$1"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "Missing required command: $cmd" >&2
        return 1
    fi
}

resolve_python() {
    if command -v python3 >/dev/null 2>&1; then
        echo "python3"
        return 0
    fi
    if command -v python >/dev/null 2>&1; then
        echo "python"
        return 0
    fi
    echo "" >&2
    return 1
}

E2E_PYTHON="${E2E_PYTHON:-}"
if [[ -z "$E2E_PYTHON" ]]; then
    E2E_PYTHON="$(resolve_python)" || true
fi

e2e_random_seed() {
    od -An -N4 -tu4 /dev/urandom 2>/dev/null | tr -d ' ' || date +%s
}

e2e_seed_init() {
    if [[ -z "${E2E_SEED:-}" ]]; then
        if [[ "${E2E_DETERMINISTIC:-1}" == "1" ]]; then
            E2E_SEED="${E2E_DEFAULT_SEED:-0}"
        else
            E2E_SEED="$(e2e_random_seed)"
        fi
    fi
    export E2E_SEED
    printf '%s' "$E2E_SEED"
}

e2e_run_id_init() {
    local prefix="${1:-run}"
    if [[ -z "${E2E_RUN_ID:-}" ]]; then
        if declare -f e2e_run_id >/dev/null 2>&1; then
            E2E_RUN_ID="$(e2e_run_id)"
        elif [[ "${E2E_DETERMINISTIC:-1}" == "1" ]]; then
            local seed="${E2E_SEED:-0}"
            local seq="${E2E_RUN_SEQ:-0}"
            seq=$((seq + 1))
            export E2E_RUN_SEQ="$seq"
            E2E_RUN_ID="${prefix}_det_${seed}_${seq}"
        else
            E2E_RUN_ID="${prefix}_$(date +%Y%m%d_%H%M%S)_$$"
        fi
    fi
    export E2E_RUN_ID
    printf '%s' "$E2E_RUN_ID"
}

e2e_tick_init() {
    local step="${1:-${E2E_TIME_STEP_MS:-}}"
    if [[ -n "$step" ]]; then
        export E2E_TIME_STEP_MS="$step"
    fi
    if [[ -n "${E2E_TIME_STEP_MS:-}" && -z "${FTUI_TEST_TIME_STEP_MS:-}" ]]; then
        export FTUI_TEST_TIME_STEP_MS="$E2E_TIME_STEP_MS"
    fi
}

e2e_export_deterministic_env() {
    local seed="${E2E_SEED:-0}"
    if [[ "${E2E_DETERMINISTIC:-1}" == "1" ]]; then
        export FTUI_TEST_DETERMINISTIC="${FTUI_TEST_DETERMINISTIC:-1}"
        export FTUI_DEMO_DETERMINISTIC="${FTUI_DEMO_DETERMINISTIC:-1}"
        export FTUI_SEED="${FTUI_SEED:-$seed}"
        export FTUI_HARNESS_SEED="${FTUI_HARNESS_SEED:-$seed}"
        export FTUI_DEMO_SEED="${FTUI_DEMO_SEED:-$seed}"
        export FTUI_TEST_SEED="${FTUI_TEST_SEED:-$seed}"
    fi
}

e2e_export_seed_env() {
    local seed="${E2E_SEED:-0}"
    export STORM_SEED="${STORM_SEED:-$seed}"
    export E2E_CONTEXT_SEED="${E2E_CONTEXT_SEED:-$seed}"
}

# The seed / time-step variables the deterministic PTY runs need, but which must
# NOT reach `cargo test`: unit tests and insta/screen snapshots carry their own
# default seeds (e.g. ftui-harness asserts 99/42; DeterminismLab is blessed at
# seed 7), so an inherited E2E seed makes them see the wrong value. Keep this
# list in one place so the widget-api and demo-showcase scripts cannot drift.
E2E_CARGO_TEST_STRIP_VARS=(
    FTUI_TEST_SEED FTUI_SEED FTUI_HARNESS_SEED FTUI_DEMO_SEED
    E2E_SEED E2E_CONTEXT_SEED FTUI_TEST_TIME_STEP_MS E2E_TIME_STEP_MS
)

# Run a `cargo test` invocation with the deterministic seed/time-step variables
# stripped, so the tests observe their own defaults. Usage:
#   e2e_cargo_test_env_guard cargo test -p foo -- --test-threads=4
e2e_cargo_test_env_guard() {
    local unset_args=()
    local var
    for var in "${E2E_CARGO_TEST_STRIP_VARS[@]}"; do
        unset_args+=(-u "$var")
    done
    env "${unset_args[@]}" "$@"
}

# Emit the JSONL evidence line that the cargo-test step ran hermetically. Call
# this immediately before the guarded step (outside run_step, so the line lands
# on the JSONL stream rather than in the step's captured log).
e2e_log_cargo_test_hermetic() {
    local joined
    joined="$(IFS=,; printf '%s' "${E2E_CARGO_TEST_STRIP_VARS[*]}")"
    jsonl_assert "cargo_test_env_hermetic" "pass" "unset=${joined}"
}

e2e_json_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

e2e_env_json() {
    local term="${TERM:-}"
    local colorterm="${COLORTERM:-}"
    local no_color="${NO_COLOR:-}"
    local tmux="${TMUX:-}"
    local zellij="${ZELLIJ:-}"
    local kitty="${KITTY_WINDOW_ID:-}"
    local ci="${CI:-}"
    local run_id="${E2E_RUN_ID:-}"
    local seed="${E2E_SEED:-}"
    local time_step="${E2E_TIME_STEP_MS:-}"
    local cols="${COLUMNS:-}"
    local rows="${LINES:-}"
    local deterministic="${E2E_DETERMINISTIC:-1}"

    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --arg term "$term" \
            --arg colorterm "$colorterm" \
            --arg no_color "$no_color" \
            --arg tmux "$tmux" \
            --arg zellij "$zellij" \
            --arg kitty "$kitty" \
            --arg ci "$ci" \
            --arg run_id "$run_id" \
            --arg seed "$seed" \
            --arg deterministic "$deterministic" \
            --arg time_step_ms "$time_step" \
            --arg columns "$cols" \
            --arg lines "$rows" \
            '{term:$term,colorterm:$colorterm,no_color:$no_color,tmux:$tmux,zellij:$zellij,kitty_window_id:$kitty,ci:$ci,run_id:$run_id,seed:$seed,deterministic:$deterministic,time_step_ms:$time_step_ms,columns:$columns,lines:$lines}'
        return 0
    fi

    printf '{"term":"%s","colorterm":"%s","no_color":"%s","tmux":"%s","zellij":"%s","kitty_window_id":"%s","ci":"%s","run_id":"%s","seed":"%s","deterministic":"%s","time_step_ms":"%s","columns":"%s","lines":"%s"}' \
        "$(e2e_json_escape "$term")" \
        "$(e2e_json_escape "$colorterm")" \
        "$(e2e_json_escape "$no_color")" \
        "$(e2e_json_escape "$tmux")" \
        "$(e2e_json_escape "$zellij")" \
        "$(e2e_json_escape "$kitty")" \
        "$(e2e_json_escape "$ci")" \
        "$(e2e_json_escape "$run_id")" \
        "$(e2e_json_escape "$seed")" \
        "$(e2e_json_escape "$deterministic")" \
        "$(e2e_json_escape "$time_step")" \
        "$(e2e_json_escape "$cols")" \
        "$(e2e_json_escape "$rows")"
}

e2e_fixture_self_test() {
    if ! declare -f e2e_determinism_self_test >/dev/null 2>&1; then
        if [[ -f "$E2E_LIB_DIR/logging.sh" ]]; then
            # shellcheck source=tests/e2e/lib/logging.sh
            source "$E2E_LIB_DIR/logging.sh"
        fi
    fi
    if declare -f e2e_determinism_self_test >/dev/null 2>&1; then
        e2e_determinism_self_test
    else
        echo "WARN: e2e_determinism_self_test not available (logging.sh not sourced)" >&2
    fi
}

e2e_fixture_init() {
    local prefix="${1:-run}"
    local seed_override="${2:-}"
    local tick_override="${3:-}"
    export E2E_DETERMINISTIC="${E2E_DETERMINISTIC:-1}"
    if [[ -n "$seed_override" ]]; then
        export E2E_SEED="$seed_override"
    fi
    e2e_seed_init >/dev/null
    e2e_run_id_init "$prefix" >/dev/null
    e2e_tick_init "$tick_override"
    e2e_export_seed_env
    e2e_export_deterministic_env
    export E2E_ENV_JSON="${E2E_ENV_JSON:-$(e2e_env_json)}"
    if [[ "${E2E_SELF_TEST:-0}" == "1" ]]; then
        e2e_fixture_self_test
    fi
}

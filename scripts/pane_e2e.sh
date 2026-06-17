#!/bin/bash
set -euo pipefail

# Pane validation E2E runner.
#
# Modes:
#   smoke  - fast terminal + web sanity checks
#   full   - smoke plus broader pane suites
#   stress - full plus repeated stress iterations
#
# This runner emits deterministic JSONL diagnostics and pane artifact bundles
# for failures, and uses rch for cargo-based checks when available.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"

# Preserve user-provided overrides before logging.sh assigns defaults.
PRESET_E2E_LOG_DIR="${E2E_LOG_DIR:-}"
PRESET_E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-}"
PRESET_E2E_JSONL_FILE="${E2E_JSONL_FILE:-}"
PRESET_LOG_FILE="${LOG_FILE:-}"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"

MODE="${PANE_E2E_MODE:-full}"
VERBOSE=false
TERMINAL_ONLY=false
WEB_ONLY=false
STRESS_ITERATIONS="${PANE_STRESS_ITERATIONS:-3}"
SEED_OVERRIDE="${PANE_E2E_SEED:-}"

usage() {
    cat <<USAGE
Usage: $0 [options]

Options:
  --mode <smoke|full|stress>   Runner mode (default: full)
  --stress-iterations <n>      Stress iterations for --mode stress (default: 3)
  --seed <n>                   Deterministic seed override
  --terminal-only              Run only terminal pane suites
  --web-only                   Run only web/wasm pane suites
  --verbose, -v                Stream command logs to stdout
  --help, -h                   Show this help
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --mode)
            MODE="${2:-}"
            shift 2
            ;;
        --stress-iterations)
            STRESS_ITERATIONS="${2:-}"
            shift 2
            ;;
        --seed)
            SEED_OVERRIDE="${2:-}"
            shift 2
            ;;
        --terminal-only)
            TERMINAL_ONLY=true
            shift
            ;;
        --web-only)
            WEB_ONLY=true
            shift
            ;;
        --verbose|-v)
            VERBOSE=true
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage
            exit 2
            ;;
    esac
done

case "$MODE" in
    smoke|full|stress)
        ;;
    *)
        echo "Invalid mode: $MODE (expected smoke|full|stress)" >&2
        exit 2
        ;;
esac

if [[ ! "$STRESS_ITERATIONS" =~ ^[0-9]+$ ]] || [[ "$STRESS_ITERATIONS" -lt 1 ]]; then
    echo "Invalid --stress-iterations value: $STRESS_ITERATIONS" >&2
    exit 2
fi

if $TERMINAL_ONLY && $WEB_ONLY; then
    echo "--terminal-only and --web-only are mutually exclusive" >&2
    exit 2
fi

RUN_TERMINAL=true
RUN_WEB=true
if $TERMINAL_ONLY; then
    RUN_WEB=false
fi
if $WEB_ONLY; then
    RUN_TERMINAL=false
fi

export E2E_DETERMINISTIC="${E2E_DETERMINISTIC:-1}"
export E2E_TIME_STEP_MS="${E2E_TIME_STEP_MS:-100}"
if [[ -n "$SEED_OVERRIDE" ]]; then
    export E2E_SEED="$SEED_OVERRIDE"
fi

e2e_fixture_init "pane_e2e" "${E2E_SEED:-}" "$E2E_TIME_STEP_MS"

RUN_ID="${E2E_RUN_ID:-$(e2e_run_id)}"
TIMESTAMP="$(e2e_log_stamp)"
E2E_LOG_DIR="${PRESET_E2E_LOG_DIR:-/tmp/ftui_pane_e2e_${RUN_ID}_${TIMESTAMP}}"
E2E_RESULTS_DIR="${PRESET_E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
E2E_JSONL_FILE="${PRESET_E2E_JSONL_FILE:-$E2E_LOG_DIR/pane_e2e.jsonl}"
LOG_FILE="${PRESET_LOG_FILE:-$E2E_LOG_DIR/pane_e2e.log}"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 --mode $MODE}"
E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"

export E2E_LOG_DIR E2E_RESULTS_DIR E2E_JSONL_FILE LOG_FILE E2E_RUN_CMD E2E_RUN_START_MS

mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
jsonl_init

PANE_TRACE_ROOT="$(jsonl_trace_id "pane-e2e")"
PANE_ARTIFACT_ROOT="$E2E_RESULTS_DIR/pane_artifacts/runner"
PANE_TRACEABILITY_MATRIX="${PANE_TRACEABILITY_MATRIX:-$PROJECT_ROOT/tests/e2e/pane_traceability_matrix.json}"
PANE_TRACEABILITY_STATUS="${PANE_TRACEABILITY_STATUS:-$E2E_RESULTS_DIR/pane_traceability_status.json}"
mkdir -p "$PANE_ARTIFACT_ROOT"

jsonl_assert "artifact_pane_e2e_log_dir" "pass" "path=$E2E_LOG_DIR"
jsonl_assert "artifact_pane_e2e_results_dir" "pass" "path=$E2E_RESULTS_DIR"
jsonl_assert "artifact_pane_e2e_artifact_root" "pass" "path=$PANE_ARTIFACT_ROOT"
jsonl_assert "artifact_pane_traceability_matrix" "pass" "path=$PANE_TRACEABILITY_MATRIX"

if command -v rch >/dev/null 2>&1; then
    CARGO_RUNNER=(rch exec -- cargo)
else
    CARGO_RUNNER=(cargo)
fi

FAILURES=0
TOTAL_STEPS=0

log_info "Pane E2E runner start"
log_info "  mode=$MODE run_id=$RUN_ID seed=${E2E_SEED:-0}"
log_info "  log_dir=$E2E_LOG_DIR"
log_info "  results_dir=$E2E_RESULTS_DIR"

hash_text() {
    local text="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        printf '%s' "$text" | sha256sum | awk '{print $1}'
        return 0
    fi
    if command -v shasum >/dev/null 2>&1; then
        printf '%s' "$text" | shasum -a 256 | awk '{print $1}'
        return 0
    fi
    printf 'hash-unavailable'
    return 0
}

emit_failure_bundle() {
    local step_id="$1"
    local trace_id="$2"
    local step_log="$3"
    local command_str="$4"
    local exit_code="$5"
    local duration_ms="$6"
    local lane="$7"

    local case_dir="$PANE_ARTIFACT_ROOT/$step_id"
    local snapshots_dir="$case_dir/snapshots"
    local replay_input="$case_dir/replay_input.txt"
    local manifest_path="$case_dir/manifest.json"
    local bundle_id="${RUN_ID}-${step_id}-bundle"

    mkdir -p "$snapshots_dir"

    cat > "$replay_input" <<EOF_REPLAY
step=$step_id
mode=$MODE
lane=$lane
seed=${E2E_SEED:-0}
command=$command_str
exit_code=$exit_code
duration_ms=$duration_ms
host=$(hostname 2>/dev/null || echo unknown)
EOF_REPLAY

    if [[ -f "$step_log" ]]; then
        cp "$step_log" "$snapshots_dir/step.log"
        tail -n 200 "$step_log" > "$snapshots_dir/step.tail.log" || true
    fi

    : > "$snapshots_dir/snapshots.index"
    for snap in step.log step.tail.log; do
        if [[ -f "$snapshots_dir/$snap" ]]; then
            printf '%s\n' "$snap" >> "$snapshots_dir/snapshots.index"
        fi
    done

    jsonl_pane_artifact_bundle \
        "$trace_id" \
        "$bundle_id" \
        "$manifest_path" \
        "$E2E_JSONL_FILE" \
        "$replay_input" \
        "$snapshots_dir" \
        "failed" \
        "step=$step_id lane=$lane exit_code=$exit_code"

    jsonl_assert "artifact_pane_e2e_manifest_${step_id}" "pass" "path=$manifest_path"
}

run_step() {
    local step_id="$1"
    local lane="$2"
    shift 2
    local cmd=("$@")

    local step_log="$E2E_LOG_DIR/${step_id}.log"
    local command_str="${cmd[*]}"

    TOTAL_STEPS=$((TOTAL_STEPS + 1))

    local cols=120
    local rows=40
    local mode_context="alt"
    if [[ "$lane" == "web" ]]; then
        cols=0
        rows=0
        mode_context="web"
    fi

    jsonl_set_context "$mode_context" "$cols" "$rows" "${E2E_SEED:-0}"
    jsonl_case_step_start "$step_id" "runner" "$lane" "$command_str"

    local start_ms
    start_ms="$(e2e_now_ms)"

    local exit_code=0
    if $VERBOSE; then
        if "${cmd[@]}" 2>&1 | tee "$step_log"; then
            exit_code=0
        else
            exit_code=$?
        fi
    else
        if "${cmd[@]}" > "$step_log" 2>&1; then
            exit_code=0
        else
            exit_code=$?
        fi
    fi

    local duration_ms=$(( $(e2e_now_ms) - start_ms ))
    local status="passed"
    local failure_code=""
    if [[ "$exit_code" -ne 0 ]]; then
        status="failed"
        failure_code="exit_${exit_code}"
    fi

    local trace_id="${PANE_TRACE_ROOT}-${step_id}"
    local pane_tree_hash
    pane_tree_hash="$(sha256_file "$step_log" 2>/dev/null || true)"
    if [[ -z "$pane_tree_hash" ]]; then
        pane_tree_hash="$(hash_text "${step_id}|missing-log")"
    fi
    local focus_state_hash
    focus_state_hash="$(hash_text "${MODE}|${lane}|${step_id}|focus")"
    local splitter_state_hash
    splitter_state_hash="$(hash_text "${step_id}|${duration_ms}|${exit_code}|splitter")"

    jsonl_pane_trace \
        "$trace_id" \
        "$lane" \
        "$pane_tree_hash" \
        "$focus_state_hash" \
        "$splitter_state_hash" \
        "$duration_ms" \
        "$status" \
        "step=${step_id} command=${command_str}" \
        "$failure_code"

    if [[ "$status" == "passed" ]]; then
        jsonl_case_step_end "$step_id" "runner" "passed" "$duration_ms" "$lane" "$command_str"
        log_info "PASS [$lane] $step_id (${duration_ms}ms)"
        return 0
    fi

    jsonl_case_step_end "$step_id" "runner" "failed" "$duration_ms" "$lane" "$command_str"
    emit_failure_bundle "$step_id" "$trace_id" "$step_log" "$command_str" "$exit_code" "$duration_ms" "$lane"
    log_error "FAIL [$lane] $step_id (exit=$exit_code, ${duration_ms}ms)"
    FAILURES=$((FAILURES + 1))
    return 1
}

run_cargo_test_step() {
    local step_id="$1"
    shift
    run_step "$step_id" "web" "${CARGO_RUNNER[@]}" test "$@"
}

run_traceability_step() {
    local traceability_cmd=(bash "$PROJECT_ROOT/tests/e2e/check_pane_traceability.sh" --matrix "$PANE_TRACEABILITY_MATRIX" --output "$PANE_TRACEABILITY_STATUS")
    local traceability_step="pane_traceability_matrix_check"

    if [[ "$MODE" == "smoke" ]]; then
        traceability_cmd+=(--warn-only)
        traceability_step="pane_traceability_matrix_smoke"
    fi

    run_step "$traceability_step" "terminal" "${traceability_cmd[@]}" || true

    if [[ -f "$PANE_TRACEABILITY_STATUS" ]]; then
        jsonl_assert "artifact_pane_traceability_status" "pass" "path=$PANE_TRACEABILITY_STATUS"
    else
        jsonl_assert "artifact_pane_traceability_status" "failed" "path=$PANE_TRACEABILITY_STATUS"
    fi
}

if $RUN_TERMINAL; then
    run_step "pane_terminal_layout_resize_smoke" "terminal" bash "$PROJECT_ROOT/tests/e2e/scripts/test_layout_composer_resize.sh" || true
    run_step "pane_layout_semantic_replay_harness" "terminal" "${CARGO_RUNNER[@]}" test -p ftui-layout --test pane_semantic_replay_harness -- --nocapture || true
    run_step "pane_terminal_splitter_drag_pty" "terminal" "${CARGO_RUNNER[@]}" test -p ftui-harness --test pane_splitter_drag_pty_e2e -- --test-threads=1 --nocapture || true
    run_step "pane_terminal_input_pty_smoke" "terminal" "${CARGO_RUNNER[@]}" test -p ftui-harness --test pane_input_pty_e2e pty_keyboard_plus_minus_resize -- --test-threads=1 --nocapture || true
    # Production terminal keyboard binding over PTY (bd-8e1oc): key -> PaneCommand
    # -> live tree, via PANE_HARNESS_INPUT=keymap. Smoke runs keyboard focus nav.
    run_step "pane_terminal_keymap_smoke" "terminal" "${CARGO_RUNNER[@]}" test -p ftui-harness --test pane_input_pty_e2e pty_keymap_tab_navigates_focus_to_next_pane -- --test-threads=1 --nocapture || true

    if [[ "$MODE" != "smoke" ]]; then
        run_step "pane_terminal_layout_resize_thrash" "terminal" env LAYOUT_RESIZE_PROFILE=thrash bash "$PROJECT_ROOT/tests/e2e/scripts/test_layout_composer_resize.sh" || true
        run_step "pane_terminal_action_timeline_full" "terminal" bash "$PROJECT_ROOT/tests/e2e/scripts/test_action_timeline.sh" || true
        run_step "pane_terminal_input_pty_full" "terminal" "${CARGO_RUNNER[@]}" test -p ftui-harness --test pane_input_pty_e2e -- --test-threads=1 --nocapture || true
    fi

    if [[ "$MODE" == "stress" ]]; then
        i=1
        while [[ "$i" -le "$STRESS_ITERATIONS" ]]; do
            run_step "pane_terminal_layout_resize_stress_${i}" "terminal" bash "$PROJECT_ROOT/tests/e2e/scripts/test_layout_composer_resize.sh" || true
            run_step "pane_terminal_input_pty_stress_${i}" "terminal" "${CARGO_RUNNER[@]}" test -p ftui-harness --test pane_input_pty_e2e pty_keyboard_resize_is_exact_and_mode_independent -- --test-threads=1 --nocapture || true
            run_step "pane_terminal_keymap_stress_${i}" "terminal" "${CARGO_RUNNER[@]}" test -p ftui-harness --test pane_input_pty_e2e pty_keymap_tab_navigates_focus_to_next_pane -- --test-threads=1 --nocapture || true
            i=$((i + 1))
        done
    fi
fi

if $RUN_WEB; then
    run_cargo_test_step "pane_web_pointer_smoke" -p ftui-web pointer_move_emits_motion_and_pressure_snap_profile -- --nocapture || true
    run_cargo_test_step "pane_web_runner_core_smoke" -p ftui-showcase-wasm runner_core_pane_pointer_lifecycle_emits_capture_commands -- --nocapture || true
    run_cargo_test_step "pane_web_runner_core_touch_yield_smoke" -p ftui-showcase-wasm runner_core_pane_touch_second_down_releases_capture_for_pinch_layer -- --nocapture || true
    run_cargo_test_step "pane_web_runner_core_context_loss_smoke" -p ftui-showcase-wasm runner_core_pane_context_lost_releases_active_capture -- --nocapture || true
    run_cargo_test_step "pane_web_runner_core_render_stall_smoke" -p ftui-showcase-wasm runner_core_pane_render_stalled_before_capture_ack_cancels_without_release -- --nocapture || true
    # Cohesive web E2E suite (bd-a46q1.4): drives the production pointer-capture
    # adapter + canonical coordinate normalizer end-to-end. Smoke runs the
    # headline DPR/zoom/viewport invariance (no geometry drift) scenario.
    run_cargo_test_step "pane_web_e2e_invariance_smoke" -p ftui-web --test pane_web_e2e web_drag_is_invariant_across_dpr_and_viewport -- --nocapture || true
    # Production browser keyboard binding E2E (bd-kxg62): keymap -> command ->
    # live tree, with roving-tabindex/ARIA + announcements. Smoke runs the
    # arrow-focus + roving-tabindex scenario.
    run_cargo_test_step "pane_web_e2e_kbd_smoke" -p ftui-web --test pane_web_e2e web_kbd_arrow_focus_traversal_keeps_roving_tabindex -- --nocapture || true

    if [[ "$MODE" != "smoke" ]]; then
        run_cargo_test_step "pane_web_pointer_full" -p ftui-web pointer_ -- --nocapture || true
        run_cargo_test_step "pane_web_runner_core_full" -p ftui-showcase-wasm runner_core_pane -- --nocapture || true
        run_cargo_test_step "pane_web_e2e_full" -p ftui-web --test pane_web_e2e -- --nocapture || true
    fi

    if [[ "$MODE" == "stress" ]]; then
        i=1
        while [[ "$i" -le "$STRESS_ITERATIONS" ]]; do
            run_cargo_test_step "pane_web_pointer_stress_${i}" -p ftui-web pointer_move_ -- --nocapture || true
            run_cargo_test_step "pane_web_e2e_replay_stress_${i}" -p ftui-web --test pane_web_e2e web_drag_semantic_trace_replays_deterministically -- --nocapture || true
            run_cargo_test_step "pane_web_e2e_kbd_stress_${i}" -p ftui-web --test pane_web_e2e web_kbd_command_stream_is_deterministic -- --nocapture || true
            i=$((i + 1))
        done
    fi
fi

# ---------------------------------------------------------------------------
# Cross-host parity diff runner (bd-a46q1.5): drives identical canonical pane
# gestures through BOTH production adapters in-process (terminal
# PaneTerminalAdapter + web PanePointerCaptureAdapter) and asserts zero
# unexplained topology/operation/lifecycle differences per the parity contract
# (docs/spec/pane-parity-contract-and-program.md §6). Inherently cross-host, so
# it only runs when neither --terminal-only nor --web-only was given.
# ---------------------------------------------------------------------------
if $RUN_TERMINAL && $RUN_WEB; then
    PARITY_ARTIFACT_DIR="$PANE_ARTIFACT_ROOT/cross_host_parity"
    mkdir -p "$PARITY_ARTIFACT_DIR"
    run_step "pane_cross_host_parity_smoke" "web" \
        env "PANE_PARITY_ARTIFACT_DIR=$PARITY_ARTIFACT_DIR" "${CARGO_RUNNER[@]}" \
        test -p ftui-web --test pane_cross_host_parity \
        pane_cross_host_parity_corpus_reports_zero_unexplained_diffs -- --nocapture || true

    if [[ "$MODE" != "smoke" ]]; then
        run_step "pane_cross_host_parity_full" "web" \
            env "PANE_PARITY_ARTIFACT_DIR=$PARITY_ARTIFACT_DIR" "${CARGO_RUNNER[@]}" \
            test -p ftui-web --test pane_cross_host_parity -- --nocapture || true
    fi

    if [[ "$MODE" == "stress" ]]; then
        i=1
        while [[ "$i" -le "$STRESS_ITERATIONS" ]]; do
            run_step "pane_cross_host_parity_stress_${i}" "web" \
                env "PANE_PARITY_ARTIFACT_DIR=$PARITY_ARTIFACT_DIR" "${CARGO_RUNNER[@]}" \
                test -p ftui-web --test pane_cross_host_parity \
                pane_cross_host_parity_is_deterministic -- --nocapture || true
            i=$((i + 1))
        done
    fi

    if [[ -f "$PARITY_ARTIFACT_DIR/parity_summary.json" ]]; then
        jsonl_assert "artifact_pane_cross_host_parity_summary" "pass" \
            "path=$PARITY_ARTIFACT_DIR/parity_summary.json"
    fi
fi

run_traceability_step

SUMMARY_JSON="$E2E_RESULTS_DIR/pane_e2e_summary.json"
if command -v jq >/dev/null 2>&1; then
    jq -nc \
        --arg run_id "$RUN_ID" \
        --arg mode "$MODE" \
        --argjson total_steps "$TOTAL_STEPS" \
        --argjson failures "$FAILURES" \
        --arg log_dir "$E2E_LOG_DIR" \
        --arg results_dir "$E2E_RESULTS_DIR" \
        --arg jsonl "$E2E_JSONL_FILE" \
        --arg seed "${E2E_SEED:-0}" \
        '{run_id:$run_id,mode:$mode,total_steps:$total_steps,failures:$failures,log_dir:$log_dir,results_dir:$results_dir,jsonl:$jsonl,seed:$seed}' \
        > "$SUMMARY_JSON"
else
    printf '{"run_id":"%s","mode":"%s","total_steps":%s,"failures":%s,"log_dir":"%s","results_dir":"%s","jsonl":"%s","seed":"%s"}\n' \
        "$RUN_ID" "$MODE" "$TOTAL_STEPS" "$FAILURES" "$E2E_LOG_DIR" "$E2E_RESULTS_DIR" "$E2E_JSONL_FILE" "${E2E_SEED:-0}" \
        > "$SUMMARY_JSON"
fi

jsonl_assert "artifact_pane_e2e_summary" "pass" "path=$SUMMARY_JSON"

RUN_DURATION_MS=$(( $(e2e_now_ms) - E2E_RUN_START_MS ))
if [[ "$FAILURES" -gt 0 ]]; then
    jsonl_run_end "failed" "$RUN_DURATION_MS" "$FAILURES"
    log_error "Pane E2E runner finished with failures=$FAILURES (mode=$MODE)"
    exit 1
fi

jsonl_run_end "complete" "$RUN_DURATION_MS" 0
log_info "Pane E2E runner completed successfully (mode=$MODE, steps=$TOTAL_STEPS)"
exit 0

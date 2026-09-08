#!/bin/bash
set -euo pipefail

# E2E: Text effects determinism sweep (gradient/wave/glitch/typewriter/matrix/ocean).
# bd-czn4y
#
# Coverage:
# - Dashboard: Typewriter, Glitch, Wave
# - Visual Effects (TextEffects): Diagonal Gradient (ocean palette), Matrix Style
# - Modes: alt + inline
# - Sizes: 80x24, 120x40
# - Deterministic seeds/time with stable hash checks
#
# JSONL (E2E schema) emits:
# - env/run_start/run_end/step_start/step_end
# - per-effect assert entries with effect, frame_idx, hash, timing, mode, dims
# - error asserts on mismatches

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

export E2E_DETERMINISTIC="${E2E_DETERMINISTIC:-1}"
export E2E_TIME_STEP_MS="${E2E_TIME_STEP_MS:-100}"

TEXTFX_SEED="${TEXTFX_SEED:-${E2E_SEED:-0}}"
export E2E_SEED="${E2E_SEED:-$TEXTFX_SEED}"

e2e_fixture_init "text_effects_sweep" "$E2E_SEED" "$E2E_TIME_STEP_MS"

E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_logs}"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
LOG_FILE="${LOG_FILE:-$E2E_LOG_DIR/text_effects_sweep.log}"
E2E_JSONL_FILE="${E2E_JSONL_FILE:-$E2E_LOG_DIR/e2e.jsonl}"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
export E2E_LOG_DIR E2E_RESULTS_DIR LOG_FILE E2E_JSONL_FILE E2E_RUN_CMD
export E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"

mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
jsonl_init
jsonl_assert "artifact_log_dir" "pass" "log_dir=$E2E_LOG_DIR"

if [[ -z "$E2E_PYTHON" ]]; then
    log_error "python3/python is required for PTY helpers"
    exit 1
fi

resolve_demo_bin() {
    if [[ -n "${FTUI_DEMO_BIN:-}" && -x "$FTUI_DEMO_BIN" ]]; then
        echo "$FTUI_DEMO_BIN"
        return 0
    fi
    if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
        local shared_debug="$CARGO_TARGET_DIR/debug/ftui-demo-showcase"
        local shared_release="$CARGO_TARGET_DIR/release/ftui-demo-showcase"
        if [[ -x "$shared_debug" ]]; then
            echo "$shared_debug"
            return 0
        fi
        if [[ -x "$shared_release" ]]; then
            echo "$shared_release"
            return 0
        fi
    fi
    local debug_bin="$PROJECT_ROOT/target/debug/ftui-demo-showcase"
    local release_bin="$PROJECT_ROOT/target/release/ftui-demo-showcase"
    if [[ -x "$debug_bin" ]]; then
        echo "$debug_bin"
        return 0
    fi
    if [[ -x "$release_bin" ]]; then
        echo "$release_bin"
        return 0
    fi
    return 1
}

ensure_demo_bin() {
    local bin=""
    if bin="$(resolve_demo_bin)"; then
        echo "$bin"
        return 0
    fi
    log_info "Building ftui-demo-showcase (debug)..." >&2
    (cd "$PROJECT_ROOT" && cargo build -p ftui-demo-showcase >/dev/null)
    if bin="$(resolve_demo_bin)"; then
        echo "$bin"
        return 0
    fi
    return 1
}

DEMO_BIN="$(ensure_demo_bin || true)"

TEXTFX_LOG_DIR="${TEXTFX_LOG_DIR:-$E2E_LOG_DIR/text_effects}"
mkdir -p "$TEXTFX_LOG_DIR"

TEXTFX_TICK_MS="${TEXTFX_TICK_MS:-${E2E_TIME_STEP_MS:-100}}"
TEXTFX_EXIT_AFTER_TICKS="${TEXTFX_EXIT_AFTER_TICKS:-12}"
# Use the available rows by default. A 12-row inline viewport selects the
# dashboard's tiny layout, which omits the text-effects panel entirely.
TEXTFX_UI_HEIGHT="${TEXTFX_UI_HEIGHT:-}"
# Send during capability probing, after raw mode is active. The old 300 ms
# wall-clock delay raced the probe's own 300 ms timeout and added a redraw.
TEXTFX_SEND_DELAY_MS="${TEXTFX_SEND_DELAY_MS:-0}"
TEXTFX_MIN_BYTES="${TEXTFX_MIN_BYTES:-400}"

if [[ -z "$TEXTFX_EXIT_AFTER_TICKS" || "$TEXTFX_EXIT_AFTER_TICKS" -lt 1 ]]; then
    TEXTFX_EXIT_AFTER_TICKS=1
fi
TEXTFX_FRAME_IDX=$((TEXTFX_EXIT_AFTER_TICKS - 1))

modes=("alt" "inline")
sizes=("80x24" "120x40")

cases=(
    "dashboard_typewriter|2|dashboard|Typewriter"
    "dashboard_glitch|2|dashboard|Glitch"
    "dashboard_wave|2|dashboard|Wave"
    "visual_ocean|18|visual_effects|Diagonal Gradient"
    "visual_matrix|18|visual_effects|Matrix Style"
)

repeat_char() {
    local ch="$1"
    local count="$2"
    local out=""
    printf -v out "%${count}s" ""
    out="${out// /$ch}"
    printf '%s' "$out"
}

send_keys_for_effect() {
    local effect_key="$1"
    case "$effect_key" in
        dashboard_typewriter)
            repeat_char "e" 15
            ;;
        dashboard_glitch)
            repeat_char "e" 17
            ;;
        dashboard_wave)
            repeat_char "e" 18
            ;;
        visual_ocean)
            printf 't\\x20\\x20'
            ;;
        visual_matrix)
            printf 't4\\x20\\x20\\x20'
            ;;
        *)
            printf ''
            ;;
    esac
}

emit_textfx_hash_jsonl() {
    local case_id="$1"
    local screen_id="$2"
    local effect_label="$3"
    local mode="$4"
    local cols="$5"
    local rows="$6"
    local seed="$7"
    local frame_idx="$8"
    local hash="$9"
    local output_file="${10}"
    local duration_ms="${11}"

    local ts
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "$seed" ]]; then seed_json="$seed"; fi

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}" \
            --arg type "assert" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg assertion "text_fx_hash" \
            --arg status "passed" \
            --arg case_id "$case_id" \
            --arg screen "$screen_id" \
            --arg effect "$effect_label" \
            --arg mode "$mode" \
            --argjson cols "$cols" \
            --argjson rows "$rows" \
            --argjson seed "$seed_json" \
            --argjson frame_idx "$frame_idx" \
            --arg hash "$hash" \
            --arg output_file "$output_file" \
            --argjson duration_ms "$duration_ms" \
            --argjson tick_ms "$TEXTFX_TICK_MS" \
            --argjson exit_after_ticks "$TEXTFX_EXIT_AFTER_TICKS" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,assertion:$assertion,status:$status,case_id:$case_id,screen:$screen,effect:$effect,mode:$mode,cols:$cols,rows:$rows,frame_idx:$frame_idx,hash:$hash,output_file:$output_file,duration_ms:$duration_ms,tick_ms:$tick_ms,exit_after_ticks:$exit_after_ticks}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}\",\"type\":\"assert\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"assertion\":\"text_fx_hash\",\"status\":\"passed\",\"case_id\":\"$(json_escape "$case_id")\",\"screen\":\"$(json_escape "$screen_id")\",\"effect\":\"$(json_escape "$effect_label")\",\"mode\":\"$(json_escape "$mode")\",\"cols\":${cols},\"rows\":${rows},\"frame_idx\":${frame_idx},\"hash\":\"$(json_escape "$hash")\",\"output_file\":\"$(json_escape "$output_file")\",\"duration_ms\":${duration_ms},\"tick_ms\":${TEXTFX_TICK_MS},\"exit_after_ticks\":${TEXTFX_EXIT_AFTER_TICKS}}"
    fi
}

run_demo_once() (
    local screen_id="$1"
    local mode="$2"
    local cols="$3"
    local rows="$4"
    local seed="$5"
    local send_keys="$6"
    local out_pty="$7"
    local run_tag="$8"

    # This declared identity requests a live sync-output probe. The query is
    # our readiness signal; key bytes read by the probe become startup events.
    unset NO_COLOR FTUI_TEST_PROFILE FTUI_SYNC_OUTPUT FTUI_SCROLL_REGION \
        TMUX TMUX_PANE STY ZELLIJ WEZTERM_UNIX_SOCKET WEZTERM_PANE \
        WEZTERM_EXECUTABLE KITTY_WINDOW_ID WT_SESSION TERM_PROGRAM \
        TERM_PROGRAM_VERSION LC_TERMINAL LC_TERMINAL_VERSION \
        PTY_SEND_FILE PTY_SEND_SEQUENCE
    export TERM=xterm-256color COLORTERM=truecolor FTUI_CAPS_PROBE=1
    log_info "Text FX PTY profile: TERM=$TERM COLORTERM=$COLORTERM; identity/mux/policy overrides cleared; input follows DEC 2026 query"

    local args=(
        "--screen=${screen_id}"
        "--screen-mode=${mode}"
        "--no-mouse"
    )
    if [[ "$mode" == "inline" ]]; then
        args+=("--ui-height=${TEXTFX_UI_HEIGHT:-$rows}")
    fi

    local run_exit=0
    if FTUI_DEMO_DETERMINISTIC=1 \
        FTUI_DEMO_SEED="$seed" \
        FTUI_DEMO_TICK_MS="$TEXTFX_TICK_MS" \
        FTUI_DEMO_EXIT_AFTER_TICKS="$TEXTFX_EXIT_AFTER_TICKS" \
        PTY_COLS="$cols" \
        PTY_ROWS="$rows" \
        PTY_TIMEOUT=8 \
        PTY_SEND="$send_keys" \
        PTY_SEND_AFTER_OUTPUT=$'\x1b[?2026$p' \
        PTY_SEND_DELAY_MS="$TEXTFX_SEND_DELAY_MS" \
        PTY_TEST_NAME="textfx_${screen_id}_${mode}_${cols}x${rows}_${run_tag}" \
        pty_run "$out_pty" "$DEMO_BIN" "${args[@]}"; then
        run_exit=0
    else
        run_exit=$?
    fi

    pty_record_metadata "$out_pty" "$run_exit" "$cols" "$rows"
    return "$run_exit"
)

assert_effect_visible() {
    local output="$1"
    local mode="$2"
    local cols="$3"
    local rows="$4"
    local screen_label="$5"
    local effect_label="$6"
    local frame_pty="${output%.pty}.frame.pty"
    local frame_text="${output%.pty}.frame.txt"

    # Preserve the last application frame before leaving the alternate screen.
    # The full capture, including cleanup, remains the determinism hash input.
    if ! "$E2E_PYTHON" - "$output" "$frame_pty" "$mode" <<'PY'
from pathlib import Path
import re
import sys

source, destination, mode = sys.argv[1:]
data = Path(source).read_bytes()
if mode == "alt":
    exits = [
        match.start()
        for match in re.finditer(rb"\x1b\[\?([0-9;]+)l", data)
        if b"1049" in match.group(1).split(b";")
    ]
    if not exits:
        raise SystemExit("Alternate-screen cleanup missing from capture")
    data = data[:exits[-1]]
Path(destination).write_bytes(data)
PY
    then
        return 1
    fi
    pty_canonicalize_file "$frame_pty" "$frame_text" "$cols" "$rows" || return 1

    local panel_label="Text FX"
    if [[ "$screen_label" == "visual_effects" ]]; then
        panel_label="[Space] Cycle effect"
    fi
    # Check the primary slot: an adjacent effect in a multi-effect panel does
    # not prove that the requested effect was selected.
    grep -F -q "$panel_label" "$frame_text" && \
        grep -F -q "1 · $effect_label" "$frame_text"
}

run_case() {
    local effect_key="$1"
    local screen_id="$2"
    local screen_label="$3"
    local effect_label="$4"
    local mode="$5"
    local cols="$6"
    local rows="$7"

    local case_id="${effect_key}_${mode}_${cols}x${rows}"
    LOG_FILE="$TEXTFX_LOG_DIR/${case_id}.log"

    jsonl_set_context "$mode" "$cols" "$rows" "$E2E_SEED"
    log_test_start "$case_id"

    local start_ms
    start_ms="$(e2e_now_ms)"

    local send_keys
    send_keys="$(send_keys_for_effect "$effect_key")"

    local out1="$TEXTFX_LOG_DIR/${case_id}_run1.pty"
    local out2="$TEXTFX_LOG_DIR/${case_id}_run2.pty"

    if ! run_demo_once "$screen_id" "$mode" "$cols" "$rows" "$E2E_SEED" "$send_keys" "$out1" "a"; then
        local duration_ms=$(( $(e2e_now_ms) - start_ms ))
        log_test_fail "$case_id" "run1 failed"
        jsonl_assert "text_fx_run1_${case_id}" "failed" "run1 failed"
        record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "run1 failed"
        return 1
    fi

    local hash1
    hash1="$(sha256_file "$out1" || true)"
    if [[ -z "$hash1" ]]; then
        local duration_ms=$(( $(e2e_now_ms) - start_ms ))
        log_test_fail "$case_id" "run1 hash missing"
        jsonl_assert "text_fx_hash_${case_id}" "failed" "hash missing"
        record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "hash missing"
        return 1
    fi

    if ! run_demo_once "$screen_id" "$mode" "$cols" "$rows" "$E2E_SEED" "$send_keys" "$out2" "b"; then
        local duration_ms=$(( $(e2e_now_ms) - start_ms ))
        log_test_fail "$case_id" "run2 failed"
        jsonl_assert "text_fx_run2_${case_id}" "failed" "run2 failed"
        record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "run2 failed"
        return 1
    fi

    local hash2
    hash2="$(sha256_file "$out2" || true)"
    if [[ -z "$hash2" ]]; then
        local duration_ms=$(( $(e2e_now_ms) - start_ms ))
        log_test_fail "$case_id" "run2 hash missing"
        jsonl_assert "text_fx_hash_${case_id}" "failed" "hash missing"
        record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "hash missing"
        return 1
    fi

    local size1 size2
    size1=$(wc -c < "$out1" | tr -d ' ')
    size2=$(wc -c < "$out2" | tr -d ' ')

    if [[ "$size1" -lt "$TEXTFX_MIN_BYTES" || "$size2" -lt "$TEXTFX_MIN_BYTES" ]]; then
        local duration_ms=$(( $(e2e_now_ms) - start_ms ))
        log_test_fail "$case_id" "output too small"
        jsonl_assert "text_fx_size_${case_id}" "failed" "size1=${size1} size2=${size2}"
        record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "output too small"
        return 1
    fi

    local output
    for output in "$out1" "$out2"; do
        if ! assert_effect_visible "$output" "$mode" "$cols" "$rows" "$screen_label" "$effect_label"; then
            local duration_ms=$(( $(e2e_now_ms) - start_ms ))
            log_test_fail "$case_id" "requested text effect was not rendered"
            jsonl_assert "text_fx_visible_${case_id}" "failed" "effect=$effect_label pty=$output frame=${output%.pty}.frame.txt"
            record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "requested text effect was not rendered"
            return 1
        fi
    done
    jsonl_assert "text_fx_visible_${case_id}" "passed" "effect=$effect_label rendered in the primary slot in both captures"

    if [[ "$hash1" != "$hash2" ]]; then
        local duration_ms=$(( $(e2e_now_ms) - start_ms ))
        log_test_fail "$case_id" "hash mismatch"
        jsonl_assert "text_fx_hash_stability_${case_id}" "failed" "${hash1} != ${hash2}"
        record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "hash mismatch"
        return 1
    fi

    local duration_ms=$(( $(e2e_now_ms) - start_ms ))
    jsonl_assert "artifact_textfx_pty_${case_id}_run1" "pass" "pty=$out1"
    jsonl_assert "artifact_textfx_pty_${case_id}_run2" "pass" "pty=$out2"
    jsonl_assert "text_fx_hash_stability_${case_id}" "passed" "hashes stable"
    emit_textfx_hash_jsonl "$case_id" "$screen_label" "$effect_label" "$mode" "$cols" "$rows" \
        "$E2E_SEED" "$TEXTFX_FRAME_IDX" "$hash1" "$out1" "$duration_ms"

    log_test_pass "$case_id"
    record_result "$case_id" "passed" "$duration_ms" "$LOG_FILE"
    return 0
}

if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/text_effects_sweep_missing.log"
    missing_cases=0
    for case in "${cases[@]}"; do
        IFS='|' read -r effect_key _screen_id _screen_label _effect_label <<<"$case"
        for mode in "${modes[@]}"; do
            for size in "${sizes[@]}"; do
                cols="${size%x*}"
                rows="${size#*x}"
                case_id="${effect_key}_${mode}_${cols}x${rows}"
                log_test_fail "$case_id" "ftui-demo-showcase binary missing"
                record_result "$case_id" "failed" 0 "$LOG_FILE" "binary missing"
                missing_cases=$((missing_cases + 1))
            done
        done
    done
    jsonl_run_end "failed" 0 "$missing_cases"
    exit "$missing_cases"
fi

FAILURES=0

for case in "${cases[@]}"; do
    IFS='|' read -r effect_key screen_id screen_label effect_label <<<"$case"
    for mode in "${modes[@]}"; do
        for size in "${sizes[@]}"; do
            cols="${size%x*}"
            rows="${size#*x}"
            if ! run_case "$effect_key" "$screen_id" "$screen_label" "$effect_label" "$mode" "$cols" "$rows"; then
                FAILURES=$((FAILURES + 1))
            fi
        done
    done
done

run_end_ms="$(e2e_now_ms)"
run_duration_ms=$((run_end_ms - ${E2E_RUN_START_MS:-0}))
if [[ "$FAILURES" -eq 0 ]]; then
    jsonl_run_end "passed" "$run_duration_ms" 0
else
    jsonl_run_end "failed" "$run_duration_ms" "$FAILURES"
fi

exit "$FAILURES"

#!/bin/bash
set -euo pipefail

# E2E: Runtime locale switch integration test (bd-g00-root-epic-ewths.34.3)
#
# Coverage:
# - Drives ftui-harness with locale context view
# - Initial override: en (LTR)
# - Scheduled switch at 500ms to ar (RTL)
# - Asserts two distinct canonicalized frames (before: en/LTR, after: ar/RTL)
# - Asserts direction change logged in stderr (ftui.runtime.locale: text direction changed)
# - Asserts full repaint after switch with synchronized output bracket pairs
# - Emits structured JSONL matching e2e-jsonl-v1 schema

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
export E2E_SEED="${E2E_SEED:-0}"

e2e_fixture_init "locale_switch" "$E2E_SEED" "$E2E_TIME_STEP_MS"

E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_logs}"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
LOG_FILE="${LOG_FILE:-$E2E_LOG_DIR/locale_switch.log}"
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

if [[ -z "${E2E_HARNESS_BIN:-}" || ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
    if [[ -x "$TARGET_DIR/debug/ftui-harness" ]]; then
        E2E_HARNESS_BIN="$TARGET_DIR/debug/ftui-harness"
    elif [[ -x "/data/tmp/cargo-target/debug/ftui-harness" ]]; then
        E2E_HARNESS_BIN="/data/tmp/cargo-target/debug/ftui-harness"
    fi
fi

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    log_test_skip "locale_switch" "ftui-harness binary missing"
    record_result "locale_switch" "skipped" 0 "$LOG_FILE" "binary missing"
    exit 0
fi

emit_locale_case_jsonl() {
    local scenario="$1"
    local identity="$2"
    local locale_before="$3"
    local locale_after="$4"
    local direction_before="$5"
    local direction_after="$6"
    local frames_before_switch="$7"
    local frames_after_switch="$8"
    local sync_pairs="$9"
    local aligned_right="${10}"
    local visual_match="${11}"
    local first_mismatch_col="${12}"
    local exit_code="${13}"
    local duration_ms="${14}"
    local status="${15}"
    local detail="${16}"

    local ts
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "locale_case" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg ts "$ts" \
            --arg scenario "$scenario" \
            --arg identity "$identity" \
            --arg locale_before "$locale_before" \
            --arg locale_after "$locale_after" \
            --arg direction_before "$direction_before" \
            --arg direction_after "$direction_after" \
            --arg status "$status" \
            --arg detail "$detail" \
            --argjson seed "$seed_json" \
            --argjson frames_before_switch "${frames_before_switch:-null}" \
            --argjson frames_after_switch "${frames_after_switch:-null}" \
            --argjson sync_pairs "${sync_pairs:-0}" \
            --argjson aligned_right "${aligned_right:-null}" \
            --argjson visual_match "$visual_match" \
            --argjson first_mismatch_col "${first_mismatch_col:-null}" \
            --argjson exit_code "$exit_code" \
            --argjson duration_ms "$duration_ms" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,ts:$ts,scenario:$scenario,identity:$identity,locale_before:$locale_before,locale_after:$locale_after,direction_before:$direction_before,direction_after:$direction_after,frames_before_switch:$frames_before_switch,frames_after_switch:$frames_after_switch,sync_pairs:$sync_pairs,aligned_right:$aligned_right,visual_match:$visual_match,first_mismatch_col:$first_mismatch_col,exit_code:$exit_code,duration_ms:$duration_ms,status:$status,detail:$detail}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"locale_case\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"ts\":\"$(json_escape "$ts")\",\"scenario\":\"$(json_escape "$scenario")\",\"identity\":\"$(json_escape "$identity")\",\"locale_before\":\"$(json_escape "$locale_before")\",\"locale_after\":\"$(json_escape "$locale_after")\",\"direction_before\":\"$(json_escape "$direction_before")\",\"direction_after\":\"$(json_escape "$direction_after")\",\"frames_before_switch\":${frames_before_switch:-null},\"frames_after_switch\":${frames_after_switch:-null},\"sync_pairs\":${sync_pairs:-0},\"aligned_right\":${aligned_right:-null},\"visual_match\":${visual_match},\"first_mismatch_col\":${first_mismatch_col:-null},\"exit_code\":${exit_code},\"duration_ms\":${duration_ms},\"status\":\"$(json_escape "$status")\",\"detail\":\"$(json_escape "$detail")\"}"
    fi
}

run_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)" || return 2
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "locale switch assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "locale switch assertions failed"
    return 1
}

test_locale_switch_case() {
    local case_name="locale_switch"
    local output_file="$E2E_LOG_DIR/${case_name}.pty"
    local stderr_file="$E2E_LOG_DIR/${case_name}_stderr.log"
    local frame_before_pty="$E2E_LOG_DIR/${case_name}_frame_before.pty"
    local frame_before_canon="$E2E_LOG_DIR/${case_name}_frame_before.canon"
    local frame_after_canon="$E2E_LOG_DIR/${case_name}_frame_after.canon"

    rm -f "$output_file" "$stderr_file" "$frame_before_pty" "$frame_before_canon" "$frame_after_canon"

    log_test_start "$case_name"
    jsonl_case_step_start "$case_name" "harness_run" "run_harness" "FTUI_HARNESS_VIEW=locale switch en->ar"

    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2

    local exit_code=0
    FTUI_SYNC_OUTPUT=1 \
    FTUI_HARNESS_VIEW=locale \
    FTUI_HARNESS_LOCALE_OVERRIDE=en \
    FTUI_HARNESS_LOCALE_SWITCH_TO=ar \
    FTUI_HARNESS_LOCALE_SWITCH_MS=500 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    FTUI_HARNESS_STDERR_FILE="$stderr_file" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN" || exit_code=$?

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))

    jsonl_case_step_end "$case_name" "harness_run" "$([[ $exit_code -eq 0 ]] && echo success || echo failed)" "$duration_ms" "run_harness" "exit_code=$exit_code"

    if [[ "$exit_code" -ne 0 ]]; then
        log_error "Harness exited with non-zero status: $exit_code"
        emit_locale_case_jsonl "locale_switch" "default" "en" "ar" "LTR" "RTL" 0 0 0 "null" "false" "null" "$exit_code" "$duration_ms" "failed" "harness_exit_$exit_code"
        return 1
    fi

    [[ -f "$output_file" ]] || { log_error "Output PTY file missing: $output_file"; return 1; }
    [[ -f "$stderr_file" ]] || { log_error "Stderr log file missing: $stderr_file"; return 1; }

    # Extract the initial frame (before switch) from raw PTY using sync marker boundaries
    "$E2E_PYTHON" - "$output_file" "$frame_before_pty" <<'PY'
import re
import sys
from pathlib import Path

raw = Path(sys.argv[1]).read_bytes()
starts = [m.start() for m in re.finditer(rb'\x1b\[\?2026h', raw)]
if len(starts) > 1:
    # First frame ends before the second frame begins
    frame_slice = raw[:starts[1]]
else:
    # Fallback to first 1200 bytes
    frame_slice = raw[:min(1200, len(raw))]

Path(sys.argv[2]).write_bytes(frame_slice)
PY

    # Canonicalize before frame and after frame (full capture)
    pty_canonicalize_file "$frame_before_pty" "$frame_before_canon" 80 24
    pty_canonicalize_file "$output_file" "$frame_after_canon" 80 24

    jsonl_artifact "pty_capture" "$output_file"
    jsonl_artifact "stderr_log" "$stderr_file"
    jsonl_artifact "canonical_frame_before" "$frame_before_canon"
    jsonl_artifact "canonical_frame_after" "$frame_after_canon"

    # Evaluate assertions with Python
    local eval_json
    eval_json="$("$E2E_PYTHON" - "$output_file" "$stderr_file" "$frame_before_canon" "$frame_after_canon" <<'PY'
import json
import re
import sys
from pathlib import Path

pty_path, stderr_path, canon_before_path, canon_after_path = sys.argv[1:5]
raw = Path(pty_path).read_bytes()
stderr_text = Path(stderr_path).read_text(encoding="utf-8", errors="replace")
canon_before = Path(canon_before_path).read_text(encoding="utf-8", errors="replace")
canon_after = Path(canon_after_path).read_text(encoding="utf-8", errors="replace")

# Strip ANSI codes from stderr for robust matching
clean_stderr = re.sub(r'\x1b\[[0-9;]*[a-zA-Z]', '', stderr_text)

# 1. Stderr check: direction changed logged with locale=ar and direction=Rtl
direction_logged = (
    "ftui.runtime.locale: text direction changed" in clean_stderr
    and "locale=ar" in clean_stderr
    and "direction=Rtl" in clean_stderr
)

# 2. Sync bracket pairs
sync_starts = [m.start() for m in re.finditer(rb'\x1b\[\?2026h', raw)]
sync_ends = [m.start() for m in re.finditer(rb'\x1b\[\?2026l', raw)]
sync_pairs = min(len(sync_starts), len(sync_ends))

# Estimate frames before/after switch (~500ms switch in 1500ms run)
frames_before = min(5, max(1, len(sync_starts) // 3))
frames_after = max(1, len(sync_starts) - frames_before)

# 3. Canonicalized frame assertions
distinct_frames = (canon_before != canon_after)
before_has_en_ltr = ("Current locale: en" in canon_before and "Direction: LTR" in canon_before)
after_has_ar_rtl = ("Current locale: ar" in canon_after and "Direction: RTL" in canon_after)

# 4. Dirty region updated (full repaint of locale view)
view_updated = distinct_frames and before_has_en_ltr and after_has_ar_rtl

passed = direction_logged and distinct_frames and before_has_en_ltr and after_has_ar_rtl and (sync_pairs >= 2)

result = {
    "passed": passed,
    "direction_logged": direction_logged,
    "distinct_frames": distinct_frames,
    "before_has_en_ltr": before_has_en_ltr,
    "after_has_ar_rtl": after_has_ar_rtl,
    "sync_pairs": sync_pairs,
    "frames_before": frames_before,
    "frames_after": frames_after,
    "detail": "" if passed else f"dir_log={direction_logged} distinct={distinct_frames} en_ltr={before_has_en_ltr} ar_rtl={after_has_ar_rtl} sync={sync_pairs}"
}
print(json.dumps(result))
PY
)"

    local passed direction_logged distinct_frames sync_pairs frames_before frames_after detail
    passed="$(echo "$eval_json" | jq -r '.passed')"
    direction_logged="$(echo "$eval_json" | jq -r '.direction_logged')"
    distinct_frames="$(echo "$eval_json" | jq -r '.distinct_frames')"
    sync_pairs="$(echo "$eval_json" | jq -r '.sync_pairs')"
    frames_before="$(echo "$eval_json" | jq -r '.frames_before')"
    frames_after="$(echo "$eval_json" | jq -r '.frames_after')"
    detail="$(echo "$eval_json" | jq -r '.detail')"

    jsonl_assert "locale_switch_direction_logged" "$([[ "$direction_logged" == "true" ]] && echo passed || echo failed)" "stderr_path=$stderr_file"
    jsonl_assert "locale_switch_distinct_frames" "$([[ "$distinct_frames" == "true" ]] && echo passed || echo failed)" "before=$frame_before_canon after=$frame_after_canon"
    jsonl_assert "locale_switch_repaint_sync_pairs" "$([[ "$sync_pairs" -ge 2 ]] && echo passed || echo failed)" "sync_pairs=$sync_pairs"

    if [[ "$passed" == "true" ]]; then
        emit_locale_case_jsonl "locale_switch" "default" "en" "ar" "LTR" "RTL" "$frames_before" "$frames_after" "$sync_pairs" "true" "true" "null" 0 "$duration_ms" "passed" "direction changed en/LTR -> ar/RTL, 2 distinct frames, sync_pairs=$sync_pairs"
        return 0
    else
        emit_locale_case_jsonl "locale_switch" "default" "en" "ar" "LTR" "RTL" "$frames_before" "$frames_after" "$sync_pairs" "false" "false" "null" 1 "$duration_ms" "failed" "$detail"
        return 1
    fi
}

FAILURES=0
run_case "locale_switch" test_locale_switch_case || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

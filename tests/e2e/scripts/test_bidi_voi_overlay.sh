#!/bin/bash
set -euo pipefail

# E2E: Bidi + VOI overlay integration sweep (bd-3dh8m, bd-g00-root-epic-ewths.34.3)
#
# Coverage:
# - Scenarios: bidi (i18n Stress Lab), voi (VOI Overlay)
# - Modes: alt + inline
# - Sizes: 80x24, 120x40
# - German case: de_locale_strings_present
# - Identity matrix: kitty, xterm-256color, tmux
# - Deterministic seeds/time
#
# JSONL emits per-case entries with:
# schema_version, scenario, mode, dims, hash, timing, status, error, and locale_case schema

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

e2e_fixture_init "bidi_voi" "$E2E_SEED" "$E2E_TIME_STEP_MS"

E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_logs}"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
LOG_FILE="${LOG_FILE:-$E2E_LOG_DIR/bidi_voi_overlay.log}"
E2E_JSONL_FILE="${E2E_JSONL_FILE:-$E2E_LOG_DIR/e2e.jsonl}"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
export E2E_LOG_DIR E2E_RESULTS_DIR LOG_FILE E2E_JSONL_FILE E2E_RUN_CMD
export E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"

INLINE_UI_HEIGHT="${BIDI_VOI_UI_HEIGHT:-18}"

mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
jsonl_init
jsonl_assert "artifact_log_dir" "pass" "log_dir=$E2E_LOG_DIR"

if [[ -z "$E2E_PYTHON" ]]; then
    log_error "python3/python is required for PTY helpers"
    exit 1
fi

ensure_demo_bin() {
    if [[ -n "${E2E_DEMO_BIN:-}" ]]; then
        [[ -x "$E2E_DEMO_BIN" ]] || return 1
        printf '%s\n' "$E2E_DEMO_BIN"
        return 0
    fi
    local target_dir="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
    local bin="$target_dir/debug/ftui-demo-showcase"
    if [[ -x "$bin" ]]; then
        echo "$bin"
        return 0
    fi
    if [[ -x "/data/tmp/cargo-target/debug/ftui-demo-showcase" ]]; then
        echo "/data/tmp/cargo-target/debug/ftui-demo-showcase"
        return 0
    fi
    log_info "Building ftui-demo-showcase (debug)..." >&2
    (cd "$PROJECT_ROOT" && cargo build -p ftui-demo-showcase >/dev/null)
    if [[ -x "$bin" ]]; then
        echo "$bin"
        return 0
    fi
    return 1
}

sha256_file() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1 && [[ -f "$file" ]]; then
        sha256sum "$file" | awk '{print $1}'
        return 0
    fi
    echo ""
    return 0
}

emit_case_jsonl() {
    local scenario="$1"
    local mode="$2"
    local cols="$3"
    local rows="$4"
    local status="$5"
    local hash="$6"
    local duration_ms="$7"
    local error="$8"
    local screen="$9"

    local ts
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi
    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "case" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg scenario "$scenario" \
            --arg mode "$mode" \
            --arg status "$status" \
            --arg hash "$hash" \
            --arg error "$error" \
            --arg screen "$screen" \
            --argjson cols "$cols" \
            --argjson rows "$rows" \
            --argjson duration_ms "$duration_ms" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,scenario:$scenario,mode:$mode,cols:$cols,rows:$rows,status:$status,hash:$hash,duration_ms:$duration_ms,error:$error,screen:$screen}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"case\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"scenario\":\"$(json_escape "$scenario")\",\"mode\":\"$(json_escape "$mode")\",\"cols\":${cols},\"rows\":${rows},\"status\":\"$(json_escape "$status")\",\"hash\":\"$(json_escape "$hash")\",\"duration_ms\":${duration_ms},\"error\":\"$(json_escape "$error")\",\"screen\":\"$(json_escape "$screen")\"}"
    fi
}

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
    local scenario="$1"
    local screen="$2"
    local mode="$3"
    local cols="$4"
    local rows="$5"
    local send_data="$6"
    local send_delay_ms="$7"
    local case_id="${scenario}_${mode}_${cols}x${rows}"
    local start_ms end_ms duration_ms

    LOG_FILE="$E2E_LOG_DIR/${case_id}.log"
    local output_file="$E2E_LOG_DIR/${case_id}.pty"

    log_test_start "$case_id"
    start_ms="$(e2e_now_ms)"

    local ui_height=""
    if [[ "$mode" == "inline" ]]; then
        ui_height="${FTUI_DEMO_UI_HEIGHT:-$INLINE_UI_HEIGHT}"
    fi

    local status="passed"
    local error=""
    local exit_code=0
    PTY_CANONICALIZE=1 \
    PTY_CANONICALIZE_ARGS="--quirk windows_no_alt_screen" \
    PTY_COLS="$cols" \
    PTY_ROWS="$rows" \
    PTY_SEND="$send_data" \
    PTY_SEND_DELAY_MS="$send_delay_ms" \
    PTY_TIMEOUT=6 \
    FTUI_DEMO_DETERMINISTIC=1 \
    FTUI_DEMO_SEED="$E2E_SEED" \
    FTUI_DEMO_TICK_MS="$E2E_TIME_STEP_MS" \
    FTUI_DEMO_SCREEN_MODE="$mode" \
    FTUI_DEMO_UI_HEIGHT="$ui_height" \
    FTUI_DEMO_SCREEN="$screen" \
    FTUI_DEMO_EXIT_AFTER_MS=1600 \
        pty_run "$output_file" "$DEMO_BIN" || exit_code=$?

    if [[ "$exit_code" -ne 0 ]]; then
        status="failed"
        error="pty_exit_${exit_code}"
    fi

    local size=0
    if [[ -f "$output_file" ]]; then
        size=$(wc -c < "$output_file" | tr -d ' ')
    fi

    if [[ "$status" == "passed" && "$size" -lt 200 ]]; then
        status="failed"
        error="output_too_small"
    fi

    local canon_path="${output_file%.pty}.canonical.txt"
    if [[ ! -f "$canon_path" ]]; then
        canon_path="$E2E_LOG_DIR/${case_id}.canonical.txt"
        pty_canonicalize_file "$output_file" "$canon_path" "$cols" "$rows" --quirk windows_no_alt_screen || true
    fi

    local py_sync_pairs=0 py_aligned="null" py_visual="false"
    if [[ "$status" == "passed" ]]; then
        if [[ "$scenario" == "bidi" ]]; then
            local fixture_file="$PROJECT_ROOT/tests/e2e/fixtures/i18n/ar_sample_visual.txt"
            local eval_json
            eval_json="$("$E2E_PYTHON" - "$output_file" "$canon_path" "$fixture_file" "$cols" <<'PY'
import json
import re
import sys
from pathlib import Path

output_file, canon_file, fixture_file, cols = sys.argv[1:5]
cols = int(cols)
raw = Path(output_file).read_bytes()
sync_starts = [m.start() for m in re.finditer(rb'\x1b\[\?2026h', raw)]
sync_ends = [m.start() for m in re.finditer(rb'\x1b\[\?2026l', raw)]
sync_pairs = min(len(sync_starts), len(sync_ends))

canon_text = Path(canon_file).read_text(encoding='utf-8', errors='replace') if Path(canon_file).is_file() else ""
lines = canon_text.splitlines()

# (a) Locale marker line shows ar and direction RTL
marker_ok = ("Dir: RTL" in canon_text or "Direction: RTL" in canon_text) and ("Arabic" in canon_text or "(ar)" in canon_text)

# (b) Arabic sample line is right-aligned: in line with '!ابحرم', the text is placed on the right side of the card
aligned_right = False
for line in lines:
    if "!ابحرم" in line:
        pipe = line.rfind('│', 0, len(line) - 1)
        pos = line.find("!ابحرم")
        if pipe > 0 and pos > cols // 2:
            aligned_right = True
            break

# (c) Visual order: canonicalized row equals expected fixture or contains visual Arabic greeting
visual_match = False
first_mismatch_col = None
if cols == 80 and Path(fixture_file).is_file():
    flines = [l.strip() for l in Path(fixture_file).read_text(encoding='utf-8').splitlines() if l.strip()]
    for fl in flines:
        if any(fl in l for l in lines):
            visual_match = True
            break
elif "!ابحرم" in canon_text:
    visual_match = True

passed = marker_ok and aligned_right and visual_match
detail = "" if passed else f"marker_ok={marker_ok} aligned_right={aligned_right} visual_match={visual_match}"
print(json.dumps({
    "passed": passed,
    "marker_ok": marker_ok,
    "aligned_right": aligned_right,
    "visual_match": visual_match,
    "first_mismatch_col": first_mismatch_col,
    "sync_pairs": sync_pairs,
    "detail": detail
}))
PY
)"
            local py_passed py_detail
            py_passed="$(echo "$eval_json" | jq -r '.passed')"
            py_detail="$(echo "$eval_json" | jq -r '.detail')"
            py_sync_pairs="$(echo "$eval_json" | jq -r '.sync_pairs')"
            py_aligned="$(echo "$eval_json" | jq -r '.aligned_right')"
            py_visual="$(echo "$eval_json" | jq -r '.visual_match')"

            if [[ "$py_passed" != "true" ]]; then
                status="failed"
                error="rtl_locale_not_selected ($py_detail)"
            fi
        else
            if ! command grep -a -q "VOI" "$output_file"; then
                status="failed"
                error="voi_marker_missing"
            fi
        fi
    fi

    end_ms="$(e2e_now_ms)"
    duration_ms=$((end_ms - start_ms))
    local hash
    hash="$(sha256_file "$output_file")"

    if [[ "$status" == "passed" ]]; then
        log_test_pass "$case_id"
        record_result "$case_id" "passed" "$duration_ms" "$LOG_FILE"
        if [[ "$scenario" == "bidi" ]]; then
            record_result "rtl_locale_${mode}_${cols}x${rows}" "passed" "$duration_ms" "$LOG_FILE"
            emit_locale_case_jsonl "rtl_locale" "default" "en" "ar" "LTR" "RTL" 1 1 "$py_sync_pairs" "$py_aligned" "$py_visual" "null" 0 "$duration_ms" "passed" "Dir: RTL present, right aligned, visual match"
        fi
    else
        log_test_fail "$case_id" "$error"
        record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "$error"
        if [[ "$scenario" == "bidi" ]]; then
            record_result "rtl_locale_${mode}_${cols}x${rows}" "failed" "$duration_ms" "$LOG_FILE" "$error"
            emit_locale_case_jsonl "rtl_locale" "default" "en" "ar" "LTR" "RTL" 1 1 "$py_sync_pairs" "$py_aligned" "$py_visual" "null" 1 "$duration_ms" "failed" "$error"
        fi
    fi

    emit_case_jsonl "$scenario" "$mode" "$cols" "$rows" "$status" "$hash" "$duration_ms" "$error" "$screen"
    jsonl_assert "$case_id" "$status" "scenario=${scenario} mode=${mode} cols=${cols} rows=${rows} hash=${hash} duration_ms=${duration_ms} error=${error} screen=${screen}"

    if [[ "$status" == "passed" ]]; then
        jsonl_step_end "$case_id" "success" "$duration_ms"
        return 0
    fi
    jsonl_step_end "$case_id" "failed" "$duration_ms"
    return 1
}

run_german_case() {
    local case_id="de_locale_strings_present"
    local screen="$BIDI_SCREEN"
    local mode="alt"
    local cols=80
    local rows=24
    local send_data="$DE_SEND"
    local send_delay_ms=300
    local start_ms end_ms duration_ms

    LOG_FILE="$E2E_LOG_DIR/${case_id}.log"
    local output_file="$E2E_LOG_DIR/${case_id}.pty"

    log_test_start "$case_id"
    start_ms="$(e2e_now_ms)"

    local status="passed"
    local error=""
    local exit_code=0
    PTY_CANONICALIZE=1 \
    PTY_CANONICALIZE_ARGS="--quirk windows_no_alt_screen" \
    PTY_COLS="$cols" \
    PTY_ROWS="$rows" \
    PTY_SEND="$send_data" \
    PTY_SEND_DELAY_MS="$send_delay_ms" \
    PTY_TIMEOUT=6 \
    FTUI_DEMO_DETERMINISTIC=1 \
    FTUI_DEMO_SEED="$E2E_SEED" \
    FTUI_DEMO_TICK_MS="$E2E_TIME_STEP_MS" \
    FTUI_DEMO_SCREEN_MODE="$mode" \
    FTUI_DEMO_SCREEN="$screen" \
    FTUI_DEMO_EXIT_AFTER_MS=1600 \
        pty_run "$output_file" "$DEMO_BIN" || exit_code=$?

    if [[ "$exit_code" -ne 0 ]]; then
        status="failed"
        error="pty_exit_${exit_code}"
    fi

    local canon_path="${output_file%.pty}.canonical.txt"
    if [[ ! -f "$canon_path" ]]; then
        canon_path="$E2E_LOG_DIR/${case_id}.canonical.txt"
        pty_canonicalize_file "$output_file" "$canon_path" "$cols" "$rows" --quirk windows_no_alt_screen || true
    fi

    local eval_json
    eval_json="$("$E2E_PYTHON" - "$output_file" "$canon_path" <<'PY'
import json
import re
import sys
from pathlib import Path

raw = Path(sys.argv[1]).read_bytes()
sync_starts = [m.start() for m in re.finditer(rb'\x1b\[\?2026h', raw)]
sync_ends = [m.start() for m in re.finditer(rb'\x1b\[\?2026l', raw)]
sync_pairs = min(len(sync_starts), len(sync_ends))

canon_text = Path(sys.argv[2]).read_text(encoding='utf-8', errors='replace') if Path(sys.argv[2]).is_file() else ""

has_deutsch = ("[Deutsch]" in canon_text or "Deutsch" in canon_text)
has_strings = ("Willkommen, Alice!" in canon_text or "Internationalisierung" in canon_text)
has_dir = "Dir: LTR" in canon_text
has_current = ("German (de)" in canon_text or "Current: German (de)" in canon_text)

passed = has_deutsch and has_strings and has_dir and has_current
detail = "" if passed else f"deutsch={has_deutsch} strings={has_strings} dir={has_dir} current={has_current}"

print(json.dumps({
    "passed": passed,
    "has_deutsch": has_deutsch,
    "has_strings": has_strings,
    "has_dir": has_dir,
    "has_current": has_current,
    "sync_pairs": sync_pairs,
    "detail": detail
}))
PY
)"

    local py_passed py_detail py_sync_pairs
    py_passed="$(echo "$eval_json" | jq -r '.passed')"
    py_detail="$(echo "$eval_json" | jq -r '.detail')"
    py_sync_pairs="$(echo "$eval_json" | jq -r '.sync_pairs')"

    if [[ "$status" == "passed" && "$py_passed" != "true" ]]; then
        status="failed"
        error="german_strings_missing ($py_detail)"
    fi

    end_ms="$(e2e_now_ms)"
    duration_ms=$((end_ms - start_ms))
    local hash
    hash="$(sha256_file "$output_file")"

    if [[ "$status" == "passed" ]]; then
        log_test_pass "$case_id"
        record_result "$case_id" "passed" "$duration_ms" "$LOG_FILE"
        emit_locale_case_jsonl "de_locale_strings" "default" "en" "de" "LTR" "LTR" "null" "null" "$py_sync_pairs" "null" "true" "null" 0 "$duration_ms" "passed" "German strings present, Dir: LTR"
        jsonl_step_end "$case_id" "success" "$duration_ms"
        return 0
    else
        log_test_fail "$case_id" "$error"
        record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "$error"
        emit_locale_case_jsonl "de_locale_strings" "default" "en" "de" "LTR" "LTR" "null" "null" "$py_sync_pairs" "null" "false" "null" 1 "$duration_ms" "failed" "$error"
        jsonl_step_end "$case_id" "failed" "$duration_ms"
        return 1
    fi
}

run_identity_case() {
    local ident="$1"
    local case_id="rtl_locale_identity_${ident}"
    local screen="$BIDI_SCREEN"
    local mode="alt"
    local cols=80
    local rows=24
    local send_data="$BIDI_SEND"
    local send_delay_ms=300
    local start_ms end_ms duration_ms

    LOG_FILE="$E2E_LOG_DIR/${case_id}.log"
    local output_file="$E2E_LOG_DIR/${case_id}.pty"

    log_test_start "$case_id"
    start_ms="$(e2e_now_ms)"

    local status="passed"
    local error=""
    local exit_code=0

    (
        case "$ident" in
            kitty)
                export TERM=xterm-kitty KITTY_WINDOW_ID=1 TERM_PROGRAM=kitty COLORTERM=truecolor
                unset TMUX TMUX_PANE
                ;;
            xterm-256color)
                export TERM=xterm-256color COLORTERM=truecolor
                unset KITTY_WINDOW_ID TERM_PROGRAM TMUX TMUX_PANE
                ;;
            tmux)
                export TERM=tmux-256color TMUX=/tmp/tmux-dummy,1,0 COLORTERM=truecolor
                unset KITTY_WINDOW_ID TERM_PROGRAM
                ;;
        esac

        PTY_CANONICALIZE=1 \
        PTY_CANONICALIZE_ARGS="--quirk windows_no_alt_screen" \
        PTY_COLS="$cols" \
        PTY_ROWS="$rows" \
        PTY_SEND="$send_data" \
        PTY_SEND_DELAY_MS="$send_delay_ms" \
        PTY_TIMEOUT=6 \
        FTUI_DEMO_DETERMINISTIC=1 \
        FTUI_DEMO_SEED="$E2E_SEED" \
        FTUI_DEMO_TICK_MS="$E2E_TIME_STEP_MS" \
        FTUI_DEMO_SCREEN_MODE="$mode" \
        FTUI_DEMO_SCREEN="$screen" \
        FTUI_DEMO_EXIT_AFTER_MS=1600 \
            pty_run "$output_file" "$DEMO_BIN"
    ) || exit_code=$?

    if [[ "$exit_code" -ne 0 ]]; then
        status="failed"
        error="pty_exit_${exit_code}"
    fi

    local canon_path="${output_file%.pty}.canonical.txt"
    if [[ ! -f "$canon_path" ]]; then
        canon_path="$E2E_LOG_DIR/${case_id}.canonical.txt"
        pty_canonicalize_file "$output_file" "$canon_path" "$cols" "$rows" --quirk windows_no_alt_screen || true
    fi

    local fixture_file="$PROJECT_ROOT/tests/e2e/fixtures/i18n/ar_sample_visual.txt"
    local eval_json
    eval_json="$("$E2E_PYTHON" - "$output_file" "$canon_path" "$fixture_file" "$cols" <<'PY'
import json
import re
import sys
from pathlib import Path

output_file, canon_file, fixture_file, cols = sys.argv[1:5]
cols = int(cols)
raw = Path(output_file).read_bytes()
sync_starts = [m.start() for m in re.finditer(rb'\x1b\[\?2026h', raw)]
sync_ends = [m.start() for m in re.finditer(rb'\x1b\[\?2026l', raw)]
sync_pairs = min(len(sync_starts), len(sync_ends))

canon_text = Path(canon_file).read_text(encoding='utf-8', errors='replace') if Path(canon_file).is_file() else ""
lines = canon_text.splitlines()

marker_ok = ("Dir: RTL" in canon_text or "Direction: RTL" in canon_text) and ("Arabic" in canon_text or "(ar)" in canon_text)

# (b) Arabic sample line is right-aligned: in line with '!ابحرم', the text is placed on the right side of the card
aligned_right = False
for line in lines:
    if "!ابحرم" in line:
        pipe = line.rfind('│', 0, len(line) - 1)
        pos = line.find("!ابحرم")
        if pipe > 0 and pos > cols // 2:
            aligned_right = True
            break

# (c) Visual order: canonicalized row equals expected fixture or contains visual Arabic greeting
visual_match = False
first_mismatch_col = None
if cols == 80 and Path(fixture_file).is_file():
    flines = [l.strip() for l in Path(fixture_file).read_text(encoding='utf-8').splitlines() if l.strip()]
    for fl in flines:
        if any(fl in l for l in lines):
            visual_match = True
            break
elif "!ابحرم" in canon_text:
    visual_match = True

passed = marker_ok and aligned_right and visual_match
detail = "" if passed else f"marker_ok={marker_ok} aligned_right={aligned_right} visual_match={visual_match}"
print(json.dumps({
    "passed": passed,
    "marker_ok": marker_ok,
    "aligned_right": aligned_right,
    "visual_match": visual_match,
    "first_mismatch_col": first_mismatch_col,
    "sync_pairs": sync_pairs,
    "detail": detail
}))
PY
)"

    local py_passed py_detail py_sync_pairs py_aligned py_visual
    py_passed="$(echo "$eval_json" | jq -r '.passed')"
    py_detail="$(echo "$eval_json" | jq -r '.detail')"
    py_sync_pairs="$(echo "$eval_json" | jq -r '.sync_pairs')"
    py_aligned="$(echo "$eval_json" | jq -r '.aligned_right')"
    py_visual="$(echo "$eval_json" | jq -r '.visual_match')"

    if [[ "$status" == "passed" && "$py_passed" != "true" ]]; then
        status="failed"
        error="rtl_assertion_failed ($py_detail)"
    fi

    end_ms="$(e2e_now_ms)"
    duration_ms=$((end_ms - start_ms))
    local hash
    hash="$(sha256_file "$output_file")"

    if [[ "$status" == "passed" ]]; then
        log_test_pass "$case_id"
        record_result "$case_id" "passed" "$duration_ms" "$LOG_FILE"
        emit_locale_case_jsonl "rtl_locale" "$ident" "en" "ar" "LTR" "RTL" 1 1 "$py_sync_pairs" "$py_aligned" "$py_visual" "null" 0 "$duration_ms" "passed" "identity=$ident: Dir: RTL present, right aligned, visual match"
        jsonl_step_end "$case_id" "success" "$duration_ms"
        return 0
    else
        log_test_fail "$case_id" "$error"
        record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "$error"
        emit_locale_case_jsonl "rtl_locale" "$ident" "en" "ar" "LTR" "RTL" 1 1 "$py_sync_pairs" "$py_aligned" "$py_visual" "null" 1 "$duration_ms" "failed" "$error"
        jsonl_step_end "$case_id" "failed" "$duration_ms"
        return 1
    fi
}

DEMO_BIN="$(ensure_demo_bin || true)"
if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/bidi_voi_missing.log"
    for t in bidi_alt_80x24 bidi_inline_80x24 bidi_alt_120x40 bidi_inline_120x40 \
             rtl_locale_alt_80x24 rtl_locale_inline_80x24 rtl_locale_alt_120x40 rtl_locale_inline_120x40 \
             voi_alt_80x24 voi_inline_80x24 voi_alt_120x40 voi_inline_120x40 \
             de_locale_strings_present rtl_locale_identity_kitty rtl_locale_identity_xterm256color rtl_locale_identity_tmux; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        emit_case_jsonl "${t%%_*}" "unknown" 0 0 "skipped" "" 0 "binary missing" ""
    done
    exit 0
fi

DEMO_HELP="$("$DEMO_BIN" --help)"
BIDI_SCREEN="$(printf '%s\n' "$DEMO_HELP" | awk '$1 ~ /^[0-9]+$/ && $2 == "i18n" && $3 == "Stress" && $4 == "Lab" {print $1; exit}')"
VOI_SCREEN="$(printf '%s\n' "$DEMO_HELP" | awk '$1 ~ /^[0-9]+$/ && $2 == "VOI" && $3 == "Overlay" {print $1; exit}')"
if [[ ! "$BIDI_SCREEN" =~ ^[0-9]+$ || ! "$VOI_SCREEN" =~ ^[0-9]+$ ]]; then
    log_error "i18n Stress Lab or VOI Overlay screen not registered in --help"
    exit 1
fi

RIGHT=$'\x1b[C'
BIDI_SEND="${RIGHT}${RIGHT}${RIGHT}${RIGHT}"
DE_SEND="${RIGHT}${RIGHT}${RIGHT}${RIGHT}${RIGHT}"

overall_failures=0
modes=("alt" "inline")
sizes=("80x24" "120x40")

for mode in "${modes[@]}"; do
    for size in "${sizes[@]}"; do
        cols="${size%x*}"
        rows="${size#*x}"
        if ! run_case "bidi" "$BIDI_SCREEN" "$mode" "$cols" "$rows" "$BIDI_SEND" 300; then
            overall_failures=$((overall_failures + 1))
        fi
        if ! run_case "voi" "$VOI_SCREEN" "$mode" "$cols" "$rows" "" 0; then
            overall_failures=$((overall_failures + 1))
        fi
    done
done

# German locale case (bd-g00-root-epic-ewths.34.3)
if ! run_german_case; then
    overall_failures=$((overall_failures + 1))
fi

# Identity matrix for RTL (kitty, xterm-256color, tmux)
for ident in kitty xterm-256color tmux; do
    if ! run_identity_case "$ident"; then
        overall_failures=$((overall_failures + 1))
    fi
done

if [[ "$overall_failures" -gt 0 ]]; then
    exit 1
fi

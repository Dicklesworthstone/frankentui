#!/bin/bash
# Keybinding Chords E2E PTY Tests (bd-g00-root-epic-ewths.20.5)
#
# Exercises multi-key chord dispatch in ftui-demo-showcase:
# - chord_gg_scrolls_to_top: `g g` within timeout scrolls LogSearch to top
# - chord_gg_times_out: `g` ... 1300ms ... `g` expires without scrolling to top, then `g g` succeeds
# - single_key_not_blocked_by_pending_chord: `g` then `j` within 100ms flushes `g` and dispatches `j`
# - chord_ctrl_x_ctrl_s_saves: `Ctrl+x Ctrl+s` in AdvancedTextEditor sets `Saved` in status bar
# - keybind_regressions: verifies the 8 harness keybinding cases in test_keybinding.sh pass

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

e2e_fixture_init "keybinding_chords"

require_tools "${E2E_PYTHON:-python3}" jq grep sha256sum || exit 2

LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_keychord_${E2E_RUN_ID}}"
RESULTS_DIR="${E2E_RESULTS_DIR:-$LOG_DIR/results}"
LOG_FILE="${LOG_FILE:-$LOG_DIR/keybinding_chords.log}"
E2E_JSONL_FILE="${E2E_JSONL_FILE:-$LOG_DIR/keybinding_chords.e2e.jsonl}"
export E2E_LOG_DIR="$LOG_DIR"
export E2E_RESULTS_DIR="$RESULTS_DIR"
export LOG_FILE E2E_JSONL_FILE

mkdir -p "$LOG_DIR" "$RESULTS_DIR"
jsonl_init

TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
FTUI_DEMO_BIN="${FTUI_DEMO_BIN:-$TARGET_DIR/debug/ftui-demo-showcase}"
E2E_HARNESS_BIN="${E2E_HARNESS_BIN:-$TARGET_DIR/debug/ftui-harness}"
PTY_CANONICALIZE_BIN="${PTY_CANONICALIZE_BIN:-$TARGET_DIR/debug/pty_canonicalize}"
export FTUI_DEMO_BIN E2E_HARNESS_BIN PTY_CANONICALIZE_BIN

e2e_require bin "$FTUI_DEMO_BIN" "ftui-demo-showcase binary required"
e2e_require bin "$E2E_HARNESS_BIN" "ftui-harness binary required"

resolve_screen_index() {
    local slug="$1"
    local list_json
    list_json="$("$FTUI_DEMO_BIN" --list-screens)"
    "${E2E_PYTHON:-python3}" -c 'import json, sys; slugs = json.loads(sys.argv[1]); print(slugs.index(sys.argv[2]) + 1)' "$list_json" "$slug"
}

LOG_SEARCH_SCREEN="$(resolve_screen_index "log_search")"
EDITOR_SCREEN="$(resolve_screen_index "advanced_text_editor")"

run_chord_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2
    jsonl_step_start "$name"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)" || return 2
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        jsonl_assert "${name}_passed" "pass" "duration_ms=${duration_ms}"
        jsonl_step_end "$name" "passed" "$duration_ms"
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "chord assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "chord assertion failed"
    jsonl_assert "${name}_passed" "fail" "duration_ms=${duration_ms}"
    jsonl_step_end "$name" "failed" "$duration_ms"
    return 1
}

canonicalize_alt_screen() {
    local raw_pty="$1"
    local canonical_out="$2"
    local cols="${3:-80}"
    local rows="${4:-24}"
    local alt_pty="${raw_pty%.pty}.alt.pty"
    "${E2E_PYTHON:-python3}" -c '
import sys
data = open(sys.argv[1], "rb").read().replace(b"\x1b[?1049l", b"")
open(sys.argv[2], "wb").write(data)
' "$raw_pty" "$alt_pty"
    pty_canonicalize_file "$alt_pty" "$canonical_out" "$cols" "$rows"
}

# Case 1: `g g` scrolls LogSearch to top
chord_gg_scrolls_to_top() {
    local output_file="$LOG_DIR/chord_gg_scrolls_to_top_${E2E_RUN_ID}.pty"
    local canonical_file="$LOG_DIR/chord_gg_scrolls_to_top_${E2E_RUN_ID}.txt"
    local key_jsonl="$LOG_DIR/chord_gg_scrolls_to_top_${E2E_RUN_ID}.keys.jsonl"

    log_test_start "chord_gg_scrolls_to_top"

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_SEND_AFTER_OUTPUT=$'\e[?1049h' \
    FTUI_CAPS_PROBE=0 \
    PTY_SEND_SEQUENCE='[{"delay_ms":150,"text":" "},{"delay_ms":300,"text":"G"},{"delay_ms":500,"text":"g"},{"delay_ms":650,"text":"g"}]' \
    FTUI_DEMO_SCREEN="$LOG_SEARCH_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=1800 \
    FTUI_DEMO_KEY_JSONL="$key_jsonl" \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$FTUI_DEMO_BIN" || return $?

    canonicalize_alt_screen "$output_file" "$canonical_file" 80 24

    # First log line ([     0]  INFO server::http) must be visible at top of viewport
    grep -a -q "0\]  INFO server::http" "$canonical_file" || return 1

    # Exactly one `g g` scroll_to_top dispatch in FTUI_DEMO_KEY_JSONL
    [[ -f "$key_jsonl" ]] || return 1
    local gg_count
    gg_count=$(jq -s '[.[] | select(.event=="key_dispatch" and .outcome=="dispatched" and .chord=="g g" and .action=="scroll_to_top")] | length' "$key_jsonl")
    [[ "$gg_count" -eq 1 ]] || return 1
}

# Case 2: `g` ... 1300ms ... `g` expires without scrolling to top, then `g g` succeeds
chord_gg_times_out() {
    local output_file="$LOG_DIR/chord_gg_times_out_${E2E_RUN_ID}.pty"
    local canonical_file="$LOG_DIR/chord_gg_times_out_${E2E_RUN_ID}.txt"
    local key_jsonl="$LOG_DIR/chord_gg_times_out_${E2E_RUN_ID}.keys.jsonl"

    log_test_start "chord_gg_times_out"

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_SEND_AFTER_OUTPUT=$'\e[?1049h' \
    FTUI_CAPS_PROBE=0 \
    PTY_SEND_SEQUENCE='[{"delay_ms":150,"text":" "},{"delay_ms":300,"text":"G"},{"delay_ms":450,"text":"g"},{"delay_ms":1750,"text":"g"},{"delay_ms":3050,"text":"g"},{"delay_ms":3200,"text":"g"}]' \
    FTUI_DEMO_SCREEN="$LOG_SEARCH_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=4200 \
    FTUI_DEMO_KEY_JSONL="$key_jsonl" \
    PTY_TIMEOUT=8 \
        pty_run "$output_file" "$FTUI_DEMO_BIN" || return $?

    canonicalize_alt_screen "$output_file" "$canonical_file" 80 24
    grep -a -q "0\]  INFO server::http" "$canonical_file" || return 1

    [[ -f "$key_jsonl" ]] || return 1

    local pending_count expired_count gg_count
    pending_count=$(jq -s '[.[] | select(.event=="key_dispatch" and .outcome=="pending" and .chord=="g")] | length' "$key_jsonl")
    expired_count=$(jq -s '[.[] | select(.event=="key_dispatch" and .outcome=="expired" and .chord=="g")] | length' "$key_jsonl")
    gg_count=$(jq -s '[.[] | select(.event=="key_dispatch" and .outcome=="dispatched" and .chord=="g g" and .action=="scroll_to_top")] | length' "$key_jsonl")

    # Three `g` prefixes entered pending (450ms, 1750ms, 3050ms), the first two expired, and only the third pair dispatched `g g`.
    [[ "$pending_count" -ge 3 ]] || return 1
    [[ "$expired_count" -ge 2 ]] || return 1
    [[ "$gg_count" -eq 1 ]] || return 1
}

# Case 3: `g` then `j` within 100ms flushes pending `g` and dispatches `j`
single_key_not_blocked_by_pending_chord() {
    local output_file="$LOG_DIR/single_key_not_blocked_by_pending_chord_${E2E_RUN_ID}.pty"
    local canonical_file="$LOG_DIR/single_key_not_blocked_by_pending_chord_${E2E_RUN_ID}.txt"
    local key_jsonl="$LOG_DIR/single_key_not_blocked_by_pending_chord_${E2E_RUN_ID}.keys.jsonl"

    log_test_start "single_key_not_blocked_by_pending_chord"

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_SEND_AFTER_OUTPUT=$'\e[?1049h' \
    FTUI_CAPS_PROBE=0 \
    PTY_SEND_SEQUENCE='[{"delay_ms":150,"text":" "},{"delay_ms":300,"text":"g"},{"delay_ms":420,"text":"g"},{"delay_ms":600,"text":"g"},{"delay_ms":680,"text":"j"}]' \
    FTUI_DEMO_SCREEN="$LOG_SEARCH_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=1600 \
    FTUI_DEMO_KEY_JSONL="$key_jsonl" \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$FTUI_DEMO_BIN" || return $?

    canonicalize_alt_screen "$output_file" "$canonical_file" 80 24
    grep -a -q "1\]  INFO db::pool" "$canonical_file" || return 1

    [[ -f "$key_jsonl" ]] || return 1

    local expired_count j_count
    expired_count=$(jq -s '[.[] | select(.event=="key_dispatch" and .outcome=="expired" and .chord=="g")] | length' "$key_jsonl")
    j_count=$(jq -s '[.[] | select(.event=="key_dispatch" and .outcome=="dispatched" and .chord=="j" and .action=="scroll_down")] | length' "$key_jsonl")

    [[ "$expired_count" -ge 1 ]] || return 1
    [[ "$j_count" -eq 1 ]] || return 1
}

# Case 4: `Ctrl+x Ctrl+s` in AdvancedTextEditor sets `Saved` in status bar
chord_ctrl_x_ctrl_s_saves() {
    local output_file="$LOG_DIR/chord_ctrl_x_ctrl_s_saves_${E2E_RUN_ID}.pty"
    local canonical_file="$LOG_DIR/chord_ctrl_x_ctrl_s_saves_${E2E_RUN_ID}.txt"
    local key_jsonl="$LOG_DIR/chord_ctrl_x_ctrl_s_saves_${E2E_RUN_ID}.keys.jsonl"

    log_test_start "chord_ctrl_x_ctrl_s_saves"

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_SEND_AFTER_OUTPUT=$'\e[?1049h' \
    FTUI_CAPS_PROBE=0 \
    PTY_SEND_SEQUENCE='[{"delay_ms":150,"text":"hello"},{"delay_ms":350,"text":"\u0018"},{"delay_ms":500,"text":"\u0013"}]' \
    FTUI_DEMO_SCREEN="$EDITOR_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=1600 \
    FTUI_DEMO_KEY_JSONL="$key_jsonl" \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$FTUI_DEMO_BIN" || return $?

    canonicalize_alt_screen "$output_file" "$canonical_file" 80 24

    grep -a -q "Saved" "$canonical_file" || return 1

    [[ -f "$key_jsonl" ]] || return 1
    local save_count
    save_count=$(jq -s '[.[] | select(.event=="key_dispatch" and .outcome=="dispatched" and .chord=="Ctrl+x Ctrl+s" and .action=="save")] | length' "$key_jsonl")
    [[ "$save_count" -eq 1 ]] || return 1
}

# Case 5: Re-run test_keybinding.sh regressions
keybind_regressions() {
    log_test_start "keybind_regressions"
    "$SCRIPT_DIR/test_keybinding.sh"
}

FAILURES=0
run_chord_case "chord_gg_scrolls_to_top" chord_gg_scrolls_to_top || FAILURES=$((FAILURES + 1))
run_chord_case "chord_gg_times_out" chord_gg_times_out || FAILURES=$((FAILURES + 1))
run_chord_case "single_key_not_blocked_by_pending_chord" single_key_not_blocked_by_pending_chord || FAILURES=$((FAILURES + 1))
run_chord_case "chord_ctrl_x_ctrl_s_saves" chord_ctrl_x_ctrl_s_saves || FAILURES=$((FAILURES + 1))
run_chord_case "keybind_regressions" keybind_regressions || FAILURES=$((FAILURES + 1))

if [[ "$FAILURES" -eq 0 ]]; then
    E2E_JSONL_VALIDATE_MODE=strict jsonl_run_end "passed" 0 0
else
    E2E_JSONL_VALIDATE_MODE=strict jsonl_run_end "failed" "$FAILURES" "$FAILURES"
fi

"${E2E_PYTHON:-python3}" "$LIB_DIR/validate_jsonl.py" "$E2E_JSONL_FILE" --schema "$LIB_DIR/e2e_jsonl_schema.json" --strict

exit "$FAILURES"

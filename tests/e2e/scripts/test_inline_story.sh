#!/bin/bash
set -euo pipefail

# E2E: Inline mode story + scrollback preservation (bd-2232w)
#
# Coverage:
# - Mode: inline
# - Sizes: 80x24, 120x40, 200x50
# - Verifies scrollback preservation, cursor save/restore, UI height handling
# - Emits JSONL steps + assertions with hashes for UI/log regions

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

e2e_fixture_init "inline_story" "$E2E_SEED" "$E2E_TIME_STEP_MS"

E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_logs}"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
LOG_FILE="${LOG_FILE:-$E2E_LOG_DIR/inline_story.log}"
E2E_JSONL_FILE="${E2E_JSONL_FILE:-$E2E_LOG_DIR/e2e.jsonl}"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
export E2E_LOG_DIR E2E_RESULTS_DIR LOG_FILE E2E_JSONL_FILE E2E_RUN_CMD
export E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"

mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
jsonl_init

if [[ -z "$E2E_PYTHON" ]]; then
    log_error "python3/python is required for PTY helpers"
    exit 1
fi

ensure_demo_bin() {
    local target_dir="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
    local bin="$target_dir/debug/ftui-demo-showcase"
    if [[ -x "$bin" ]]; then
        echo "$bin"
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

build_prelog_cmd() {
    local prefix="$1"
    local lines="$2"
    local demo_bin="$3"
    cat <<CMD
for i in \$(seq 1 ${lines}); do
  printf '${prefix}%03d\n' "\$i"
done
exec "${demo_bin}"
CMD
}

hash_region() {
    local file="$1"
    local head_lines="$2"
    if [[ "$head_lines" -le 0 ]]; then
        echo ""
        return 0
    fi
    if ! command -v sha256sum >/dev/null 2>&1; then
        echo ""
        return 0
    fi
    head -n "$head_lines" "$file" | sha256sum | awk '{print $1}'
}

hash_tail_region() {
    local file="$1"
    local tail_lines="$2"
    if [[ "$tail_lines" -le 0 ]]; then
        echo ""
        return 0
    fi
    if ! command -v sha256sum >/dev/null 2>&1; then
        echo ""
        return 0
    fi
    tail -n "$tail_lines" "$file" | sha256sum | awk '{print $1}'
}

run_case() {
    local cols="$1"
    local rows="$2"
    local ui_height="$3"
    local case_id="inline_story_${cols}x${rows}_ui${ui_height}"
    local prefix="SB_${cols}x${rows}_ui${ui_height}_"
    local pre_lines=$((rows + 4))
    local expected_visible=$((rows - ui_height))
    local expected_last=$((pre_lines - ui_height))
    local expected_hidden=$((expected_last + 1))

    LOG_FILE="$E2E_LOG_DIR/${case_id}.log"
    local output_file="$E2E_LOG_DIR/${case_id}.pty"

    log_test_start "$case_id"
    jsonl_set_context "inline" "$cols" "$rows" "$E2E_SEED"
    jsonl_case_step_start "$case_id" "scrollback_preserve" "pty_run" "story_step=scrollback_preserve ui_height=${ui_height} scrollback_lines=${expected_visible} scrollback_line_count=${expected_visible} pre_lines=${pre_lines}"

    local cmd
    cmd="$(build_prelog_cmd "$prefix" "$pre_lines" "$DEMO_BIN")"

    local start_ms end_ms duration_ms
    start_ms="$(e2e_now_ms)"

    local exit_code=0
    PTY_COLS="$cols" \
    PTY_ROWS="$rows" \
    PTY_TIMEOUT=8 \
    PTY_CANONICALIZE=1 \
    PTY_TEST_NAME="$case_id" \
    PTY_SEND="t" \
    PTY_SEND_DELAY_MS=300 \
    FTUI_DEMO_DETERMINISTIC=1 \
    FTUI_DEMO_SEED="$E2E_SEED" \
    FTUI_DEMO_TICK_MS="$E2E_TIME_STEP_MS" \
    FTUI_DEMO_SCREEN_MODE="inline" \
    FTUI_DEMO_UI_HEIGHT="$ui_height" \
    FTUI_DEMO_SCREEN="$INLINE_STORY_SCREEN" \
    FTUI_DEMO_EXIT_AFTER_MS=1800 \
        pty_run "$output_file" bash -lc "$cmd" || exit_code=$?

    end_ms="$(e2e_now_ms)"
    duration_ms=$((end_ms - start_ms))

    local status="passed"
    local error=""

    if [[ "$exit_code" -ne 0 ]]; then
        status="failed"
        error="pty_exit_${exit_code}"
    fi

    local size=0
    if [[ -f "$output_file" ]]; then
        size=$(wc -c < "$output_file" | tr -d ' ')
    fi
    if [[ "$status" == "passed" && "$size" -lt 400 ]]; then
        status="failed"
        error="output_too_small"
    fi

    local canonical_file="${PTY_CANONICAL_FILE:-${output_file%.pty}.canonical.txt}"
    if [[ "$status" == "passed" && ! -f "$canonical_file" ]]; then
        status="failed"
        error="canonical_missing"
    fi

    if [[ "$status" == "passed" ]]; then
        if ! grep -a -q "INLINE MODE - SCROLLBACK PRESERVED" "$output_file"; then
            status="failed"
            error="inline_bar_missing"
        fi
    fi

    if [[ "$status" == "passed" ]]; then
        if ! grep -a -q "Anchor: BOTTOM" "$output_file"; then
            status="failed"
            error="anchor_bottom_missing"
        fi
    fi

    if [[ "$status" == "passed" ]]; then
        if ! grep -a -F -q $'\x1b7' "$output_file"; then
            status="failed"
            error="cursor_save_missing"
        fi
    fi

    if [[ "$status" == "passed" ]]; then
        if ! grep -a -F -q $'\x1b8' "$output_file"; then
            status="failed"
            error="cursor_restore_missing"
        fi
    fi

    local scrollback_visible=0
    if [[ "$status" == "passed" ]]; then
        scrollback_visible=$(grep -c "^${prefix}" "$canonical_file" || true)
        if [[ "$scrollback_visible" -ne "$expected_visible" ]]; then
            status="failed"
            error="scrollback_count_${scrollback_visible}_expected_${expected_visible}"
        fi
    fi

    if [[ "$status" == "passed" ]]; then
        if ! grep -q "${prefix}$(printf "%03d" "$expected_last")" "$canonical_file"; then
            status="failed"
            error="scrollback_last_missing"
        fi
    fi

    if [[ "$status" == "passed" ]]; then
        if grep -q "${prefix}$(printf "%03d" "$expected_hidden")" "$canonical_file"; then
            status="failed"
            error="ui_height_overlap"
        fi
    fi

    local log_hash=""
    local ui_hash=""
    if [[ "$status" == "passed" ]]; then
        log_hash="$(hash_region "$canonical_file" "$expected_visible" || true)"
        ui_hash="$(hash_tail_region "$canonical_file" "$ui_height" || true)"
    fi

    if [[ "$status" == "passed" ]]; then
        log_test_pass "$case_id"
        record_result "$case_id" "passed" "$duration_ms" "$LOG_FILE"
    else
        log_test_fail "$case_id" "$error"
        record_result "$case_id" "failed" "$duration_ms" "$LOG_FILE" "$error"
    fi

    jsonl_case_step_end "$case_id" "scrollback_preserve" "$status" "$duration_ms" "pty_run" "story_step=scrollback_preserve ui_height=${ui_height} scrollback_lines=${expected_visible} scrollback_line_count=${expected_visible} scrollback_visible=${scrollback_visible} log_hash=${log_hash} ui_hash=${ui_hash}"
    jsonl_assert "${case_id}_hashes" "$status" "log_hash=${log_hash} ui_hash=${ui_hash}"

    if [[ "$status" == "passed" ]]; then
        return 0
    fi
    return 1
}

DEMO_BIN="$(ensure_demo_bin || true)"
if [[ -z "$DEMO_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/inline_story_missing.log"
    for t in inline_story_80x24 inline_story_120x40 inline_story_200x50; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

export PTY_CANONICALIZE=1
CANON_BIN="$(resolve_canonicalize_bin || true)"
if [[ -z "$CANON_BIN" ]]; then
    LOG_FILE="$E2E_LOG_DIR/inline_story_missing.log"
    for t in inline_story_80x24 inline_story_120x40 inline_story_200x50; do
        log_test_skip "$t" "pty_canonicalize binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "pty_canonicalize missing"
    done
    exit 0
fi
export PTY_CANONICALIZE_BIN="$CANON_BIN"

# Screen list is 1-indexed; InlineModeStory is index 33 (0-based) → 34 here.
INLINE_STORY_SCREEN="${INLINE_STORY_SCREEN:-34}"
INLINE_STORY_UI_HEIGHT="${INLINE_STORY_UI_HEIGHT:-12}"

FAILURES=0
run_case 80 24 "$INLINE_STORY_UI_HEIGHT" || FAILURES=$((FAILURES + 1))
run_case 120 40 "$INLINE_STORY_UI_HEIGHT" || FAILURES=$((FAILURES + 1))
run_case 200 50 "$INLINE_STORY_UI_HEIGHT" || FAILURES=$((FAILURES + 1))

if [[ "$FAILURES" -gt 0 ]]; then
    exit 1
fi

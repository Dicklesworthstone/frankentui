#!/bin/bash
set -euo pipefail

# E2E tests for UI Inspector overlay (bd-17h9.3, bd-17h9.7)
#
# Test scenarios:
# - Smoke render at multiple sizes (120x40, 80x24, 40x10)
# - Validate inspector panel text and labels are present
# - Mode display verification
# - Edge cases: small terminal, minimal output
#
# JSONL Schema v2 (stable):
# {
#   "schema_version": "2.0.0",
#   "run_id": "<unique identifier>",
#   "case": "<test case name>",
#   "status": "passed|failed|skipped",
#   "reason": "<failure/skip reason>",
#   "duration_ms": <total duration>,
#   "timings": {
#     "setup_ms": <setup phase>,
#     "execute_ms": <pty execution>,
#     "assert_ms": <assertion checking>
#   },
#   "output_bytes": <pty output size>,
#   "output_sha256": "<checksum>",
#   "seed": "<deterministic seed>",
#   "view": "widget-inspector",
#   "cols": <terminal width>,
#   "rows": <terminal height>,
#   "input_trace": "<input sent to pty>",
#   "assertions": ["<assertion1>", ...],
#   "capabilities": {...},
#   "env": {
#     "term": "<TERM>",
#     "colorterm": "<COLORTERM>",
#     "no_color": "<NO_COLOR>"
#   }
# }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

SCHEMA_VERSION="2.0.0"
JSONL_FILE="$E2E_RESULTS_DIR/ui_inspector.jsonl"
RUN_ID="ui_inspector_$(date +%Y%m%d_%H%M%S)_$$"
SEED="${FTUI_HARNESS_SEED:-0}"
FTUI_HARNESS_SCREEN_MODE="${FTUI_HARNESS_SCREEN_MODE:-inline}"
FTUI_HARNESS_UI_HEIGHT="${FTUI_HARNESS_UI_HEIGHT:-10}"
FTUI_HARNESS_AUTO_UI_HEIGHT="${FTUI_HARNESS_AUTO_UI_HEIGHT:-0}"
FTUI_HARNESS_INPUT_MODE="${FTUI_HARNESS_INPUT_MODE:-runtime}"
FTUI_HARNESS_ENABLE_MOUSE="${FTUI_HARNESS_ENABLE_MOUSE:-0}"
FTUI_HARNESS_ENABLE_FOCUS="${FTUI_HARNESS_ENABLE_FOCUS:-0}"
FTUI_HARNESS_ENABLE_KITTY_KEYBOARD="${FTUI_HARNESS_ENABLE_KITTY_KEYBOARD:-0}"
FTUI_HARNESS_LOG_KEYS="${FTUI_HARNESS_LOG_KEYS:-0}"

# Accumulated test data for JSONL
declare -A TIMING_DATA
declare -a ASSERTIONS_PASSED

jsonl_log() {
    local line="$1"
    mkdir -p "$E2E_RESULTS_DIR"
    printf '%s\n' "$line" >> "$JSONL_FILE"
}

bool_json() {
    case "${1:-}" in
        1|true|TRUE|True|yes|YES|on|ON)
            echo "true"
            ;;
        *)
            echo "false"
            ;;
    esac
}

capabilities_json() {
    printf '{"screen_mode":"%s","input_mode":"%s","ui_height":%s,"auto_ui_height":%s,"mouse":%s,"focus":%s,"kitty_keyboard":%s,"log_keys":%s}' \
        "$FTUI_HARNESS_SCREEN_MODE" \
        "$FTUI_HARNESS_INPUT_MODE" \
        "$FTUI_HARNESS_UI_HEIGHT" \
        "$(bool_json "$FTUI_HARNESS_AUTO_UI_HEIGHT")" \
        "$(bool_json "$FTUI_HARNESS_ENABLE_MOUSE")" \
        "$(bool_json "$FTUI_HARNESS_ENABLE_FOCUS")" \
        "$(bool_json "$FTUI_HARNESS_ENABLE_KITTY_KEYBOARD")" \
        "$(bool_json "$FTUI_HARNESS_LOG_KEYS")"
}

env_json() {
    printf '{"term":"%s","colorterm":"%s","no_color":"%s"}' \
        "${TERM:-}" "${COLORTERM:-}" "${NO_COLOR:-}"
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

escape_json_string() {
    local str="$1"
    # Escape backslashes, quotes, and control characters
    printf '%s' "$str" | sed 's/\\/\\\\/g; s/"/\\"/g; s/\t/\\t/g; s/\r/\\r/g; s/\n/\\n/g'
}

assertions_json() {
    local result="["
    local first=1
    for a in "${ASSERTIONS_PASSED[@]}"; do
        if [[ "$first" -eq 1 ]]; then
            first=0
        else
            result+=","
        fi
        result+="\"$(escape_json_string "$a")\""
    done
    result+="]"
    echo "$result"
}

# Skip all tests if binary missing
if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/ui_inspector_missing.log"
    for t in ui_inspector_120x40 ui_inspector_80x24 ui_inspector_40x10; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
        jsonl_log "{\"schema_version\":\"$SCHEMA_VERSION\",\"run_id\":\"$RUN_ID\",\"case\":\"$t\",\"status\":\"skipped\",\"reason\":\"binary missing\",\"seed\":\"$SEED\",\"capabilities\":$(capabilities_json),\"env\":$(env_json)}"
    done
    exit 0
fi

# Enhanced run_case with verbose timing
run_case() {
    local name="$1"
    local cols="$2"
    local rows="$3"
    shift 3

    local setup_start_ms execute_start_ms assert_start_ms end_ms
    # BSD date lacks %N; elapsed timing uses the real monotonic clock.
    setup_start_ms="$(e2e_monotonic_ms)" || return 2

    LOG_FILE="$E2E_LOG_DIR/${name}.log"
    local output_file="$E2E_LOG_DIR/${name}.pty"

    # Reset assertions array
    ASSERTIONS_PASSED=()

    log_test_start "$name"

    execute_start_ms="$(e2e_monotonic_ms)" || return 2
    local setup_ms=$((execute_start_ms - setup_start_ms))

    # Capture input trace if any
    local input_trace="${PTY_SEND:-}"

    if "$@"; then
        assert_start_ms="$(e2e_monotonic_ms)" || return 2
        local execute_ms=$((assert_start_ms - execute_start_ms))

        end_ms="$(e2e_monotonic_ms)" || return 2
        local assert_ms=$((end_ms - assert_start_ms))
        local duration_ms=$((end_ms - setup_start_ms))

        local size output_sha
        size=$(wc -c < "$output_file" | tr -d ' ')
        output_sha="$(sha256_file "$output_file")"

        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        jsonl_log "{\"schema_version\":\"$SCHEMA_VERSION\",\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"passed\",\"duration_ms\":$duration_ms,\"timings\":{\"setup_ms\":$setup_ms,\"execute_ms\":$execute_ms,\"assert_ms\":$assert_ms},\"output_bytes\":$size,\"output_sha256\":\"$output_sha\",\"seed\":\"$SEED\",\"view\":\"widget-inspector\",\"cols\":$cols,\"rows\":$rows,\"input_trace\":\"$(escape_json_string "$input_trace")\",\"assertions\":$(assertions_json),\"capabilities\":$(capabilities_json),\"env\":$(env_json)}"
        return 0
    fi

    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - setup_start_ms))
    local output_sha
    output_sha="$(sha256_file "$output_file")"

    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    jsonl_log "{\"schema_version\":\"$SCHEMA_VERSION\",\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"status\":\"failed\",\"duration_ms\":$duration_ms,\"output_sha256\":\"$output_sha\",\"seed\":\"$SEED\",\"view\":\"widget-inspector\",\"cols\":$cols,\"rows\":$rows,\"input_trace\":\"$(escape_json_string "$input_trace")\",\"assertions\":$(assertions_json),\"capabilities\":$(capabilities_json),\"env\":$(env_json)}"
    return 1
}

# Assertion helper that tracks what was checked
assert_file_min_size() {
    local file="$1"
    local min_bytes="$2"
    local size
    size=$(wc -c < "$file" | tr -d ' ')
    if [[ "$size" -ge "$min_bytes" ]]; then
        ASSERTIONS_PASSED+=("file_min_size($min_bytes):passed")
        return 0
    fi
    ASSERTIONS_PASSED+=("file_min_size($min_bytes):failed(got=$size)")
    return 1
}

assert_contains() {
    local file="$1"
    local pattern="$2"
    if grep -a -q "$pattern" "$file"; then
        ASSERTIONS_PASSED+=("contains($pattern):passed")
        return 0
    fi
    ASSERTIONS_PASSED+=("contains($pattern):failed")
    return 1
}

# ============================================================================
# Test Cases
# ============================================================================

# Standard smoke test at a given terminal size
ui_inspector_smoke() {
    local name="$1"
    local cols="$2"
    local rows="$3"
    local output_file="$E2E_LOG_DIR/${name}.pty"

    PTY_COLS="$cols" \
    PTY_ROWS="$rows" \
    FTUI_HARNESS_SCREEN_MODE="$FTUI_HARNESS_SCREEN_MODE" \
    FTUI_HARNESS_UI_HEIGHT="$FTUI_HARNESS_UI_HEIGHT" \
    FTUI_HARNESS_AUTO_UI_HEIGHT="$FTUI_HARNESS_AUTO_UI_HEIGHT" \
    FTUI_HARNESS_INPUT_MODE="$FTUI_HARNESS_INPUT_MODE" \
    FTUI_HARNESS_VIEW="widget-inspector" \
    FTUI_HARNESS_ENABLE_MOUSE="$FTUI_HARNESS_ENABLE_MOUSE" \
    FTUI_HARNESS_ENABLE_FOCUS="$FTUI_HARNESS_ENABLE_FOCUS" \
    FTUI_HARNESS_ENABLE_KITTY_KEYBOARD="$FTUI_HARNESS_ENABLE_KITTY_KEYBOARD" \
    FTUI_HARNESS_LOG_KEYS="$FTUI_HARNESS_LOG_KEYS" \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_SEED="$SEED" \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Assertions with tracking
    assert_file_min_size "$output_file" 300 || return 1
    assert_contains "$output_file" "Inspector" || return 1
    assert_contains "$output_file" "Region:" || return 1
    assert_contains "$output_file" "LogPanel" || return 1
}

# Small terminal edge case - should render without crash
ui_inspector_small_terminal() {
    local name="$1"
    local cols="$2"
    local rows="$3"
    local output_file="$E2E_LOG_DIR/${name}.pty"

    PTY_COLS="$cols" \
    PTY_ROWS="$rows" \
    FTUI_HARNESS_SCREEN_MODE="$FTUI_HARNESS_SCREEN_MODE" \
    FTUI_HARNESS_UI_HEIGHT="$((rows > 5 ? 5 : rows - 1))" \
    FTUI_HARNESS_VIEW="widget-inspector" \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_SEED="$SEED" \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # Minimal assertions - just verify it produced output and didn't crash
    assert_file_min_size "$output_file" 50 || return 1
}

# Mode display verification - check "Full" mode indicator
ui_inspector_mode_display() {
    local name="$1"
    local cols="$2"
    local rows="$3"
    local output_file="$E2E_LOG_DIR/${name}.pty"

    PTY_COLS="$cols" \
    PTY_ROWS="$rows" \
    FTUI_HARNESS_VIEW="widget-inspector" \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1000 \
    FTUI_HARNESS_SEED="$SEED" \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    assert_file_min_size "$output_file" 200 || return 1
    # Check that mode indicator is present
    assert_contains "$output_file" "Mode:" || return 1
}

# Widget bounds verification - check bound indicators are rendered
ui_inspector_widget_bounds() {
    local name="$1"
    local cols="$2"
    local rows="$3"
    local output_file="$E2E_LOG_DIR/${name}.pty"

    PTY_COLS="$cols" \
    PTY_ROWS="$rows" \
    FTUI_HARNESS_VIEW="widget-inspector" \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_SEED="$SEED" \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    assert_file_min_size "$output_file" 300 || return 1
    # Widget labels should be rendered with brackets
    assert_contains "$output_file" "\[" || return 1
}

# ============================================================================
# Run Test Suite
# ============================================================================

FAILURES=0

# Standard smoke tests at multiple sizes
run_case "ui_inspector_120x40" 120 40 ui_inspector_smoke "ui_inspector_120x40" 120 40 || FAILURES=$((FAILURES + 1))
run_case "ui_inspector_80x24" 80 24 ui_inspector_smoke "ui_inspector_80x24" 80 24 || FAILURES=$((FAILURES + 1))

# Small terminal edge case
run_case "ui_inspector_40x10" 40 10 ui_inspector_small_terminal "ui_inspector_40x10" 40 10 || FAILURES=$((FAILURES + 1))

# Mode display verification
run_case "ui_inspector_mode_display" 100 30 ui_inspector_mode_display "ui_inspector_mode_display" 100 30 || FAILURES=$((FAILURES + 1))

# Widget bounds verification
run_case "ui_inspector_widget_bounds" 100 30 ui_inspector_widget_bounds "ui_inspector_widget_bounds" 100 30 || FAILURES=$((FAILURES + 1))

# ============================================================================
# Summary
# ============================================================================

TOTAL_TESTS=5
PASSED=$((TOTAL_TESTS - FAILURES))

jsonl_log "{\"schema_version\":\"$SCHEMA_VERSION\",\"run_id\":\"$RUN_ID\",\"type\":\"summary\",\"total\":$TOTAL_TESTS,\"passed\":$PASSED,\"failed\":$FAILURES,\"timestamp\":\"$(date -Iseconds)\"}"

if [[ "$FAILURES" -gt 0 ]]; then
    echo "E2E UI Inspector: $PASSED/$TOTAL_TESTS passed, $FAILURES failed"
    exit 1
fi

echo "E2E UI Inspector: $PASSED/$TOTAL_TESTS passed"
exit 0

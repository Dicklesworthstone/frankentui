#!/bin/bash
set -euo pipefail

# E2E tests for Modal/Dialog system (bd-39vx.6)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"

JSONL_FILE="$E2E_RESULTS_DIR/modal_dialog.jsonl"
RUN_ID="modal_dialog_$(date +%Y%m%d_%H%M%S)_$$"
SEED="${MODAL_DIALOG_SEED:-42}"
DETERMINISTIC="${MODAL_DIALOG_DETERMINISTIC:-1}"

jsonl_log() {
    local line="$1"
    mkdir -p "$E2E_RESULTS_DIR"
    printf '%s\n' "$line" >> "$JSONL_FILE"
}

compute_checksum() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1 && [[ -f "$file" ]]; then
        sha256sum "$file" | awk '{print $1}'
    else
        echo "unavailable"
    fi
}

detect_capabilities() {
    local term_val="${TERM:-}"
    local colorterm="${COLORTERM:-}"
    if [[ -n "$colorterm" ]]; then
        echo "truecolor"
    elif [[ "$term_val" == *"256color"* ]]; then
        echo "256color"
    else
        echo "basic"
    fi
}

log_run_env() {
    local term_val="${TERM:-}"
    local colorterm="${COLORTERM:-}"
    local no_color="${NO_COLOR:-}"
    local tmux="${TMUX:-}"
    local zellij="${ZELLIJ:-}"
    local kitty="${KITTY_WINDOW_ID:-}"
    local capabilities
    capabilities="$(detect_capabilities)"
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"event\":\"env\",\"timestamp\":\"$(date -Iseconds)\",\"seed\":\"$SEED\",\"term\":\"$term_val\",\"colorterm\":\"$colorterm\",\"no_color\":\"$no_color\",\"tmux\":\"$tmux\",\"zellij\":\"$zellij\",\"kitty_window_id\":\"$kitty\",\"capabilities\":\"$capabilities\"}"
}

run_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(e2e_monotonic_ms)"

    LOG_FILE="$E2E_LOG_DIR/${name}.log"
    local output_file="$E2E_LOG_DIR/${name}.out"

    log_test_start "$name"
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"event\":\"start\"}"

    if (cd "$PROJECT_ROOT" && "$@" > "$output_file" 2>&1); then
        local end_ms
        end_ms="$(e2e_monotonic_ms)"
        local duration_ms=$((end_ms - start_ms))
        local checksum
        checksum="$(compute_checksum "$output_file")"
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"seed\":\"$SEED\",\"timings\":{\"start_ms\":$start_ms,\"end_ms\":$end_ms,\"duration_ms\":$duration_ms},\"checksums\":{\"output\":\"$checksum\"},\"outcome\":{\"status\":\"passed\"}}"
        if [[ -s "$output_file" ]]; then
            grep -E '^\{.*"step":' "$output_file" >> "$JSONL_FILE" || true
        fi
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)"
    local duration_ms=$((end_ms - start_ms))
    local checksum
    checksum="$(compute_checksum "$output_file")"
    log_test_fail "$name" "test failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "test failed"
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$name\",\"seed\":\"$SEED\",\"timings\":{\"start_ms\":$start_ms,\"end_ms\":$end_ms,\"duration_ms\":$duration_ms},\"checksums\":{\"output\":\"$checksum\"},\"outcome\":{\"status\":\"failed\",\"reason\":\"test failed\"}}"
    if [[ -s "$output_file" ]]; then
        grep -E '^\{.*"step":' "$output_file" >> "$JSONL_FILE" || true
    fi
    return 1
}

main() {
    mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"

    export FTUI_SEED="$SEED"
    if [[ "$DETERMINISTIC" == "1" ]]; then
        export RUST_TEST_THREADS=1
    fi
    log_run_env

    run_case "modal_dialog_e2e" \
        cargo test -p ftui-demo-showcase --test modal_e2e -- --nocapture
}

main "$@"

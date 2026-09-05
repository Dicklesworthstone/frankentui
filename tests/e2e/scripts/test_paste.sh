#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

E2E_SUITE_SCRIPT="$SCRIPT_DIR/test_paste.sh"
export E2E_SUITE_SCRIPT
export PTY_CANONICALIZE=1
ONLY_CASE="${E2E_ONLY_CASE:-}"
FIXTURE_DIR="$E2E_ROOT/fixtures"

ALL_CASES=(
    paste_basic
    paste_multiline
    paste_large
    paste_unicode
    paste_embedded_escape
    paste_dos_limit
)

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/paste_missing.log"
    for t in "${ALL_CASES[@]}"; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

run_case() {
    local name="$1"
    shift
    if [[ -n "$ONLY_CASE" && "$ONLY_CASE" != "$name" ]]; then
        LOG_FILE="$E2E_LOG_DIR/${name}.log"
        log_test_skip "$name" "filtered (E2E_ONLY_CASE=$ONLY_CASE)"
        record_result "$name" "skipped" 0 "$LOG_FILE" "filtered"
        return 0
    fi
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
    log_test_fail "$name" "paste assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "paste assertions failed"
    return 1
}

paste_basic() {
    LOG_FILE="$E2E_LOG_DIR/paste_basic.log"
    local output_file="$E2E_LOG_DIR/paste_basic.pty"

    log_test_start "paste_basic"
    PTY_TEST_NAME="paste_basic"

    PTY_SEND=$'\x1b[200~hello paste\x1b[201~' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Paste: hello paste" "$canonical_file" || return 1
}

paste_multiline() {
    LOG_FILE="$E2E_LOG_DIR/paste_multiline.log"
    local output_file="$E2E_LOG_DIR/paste_multiline.pty"

    log_test_start "paste_multiline"
    PTY_TEST_NAME="paste_multiline"

    PTY_SEND=$'\x1b[200~line_one\nline_two\nline_three\x1b[201~' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Paste: line_one" "$canonical_file" || return 1
    grep -a -q "line_two" "$canonical_file" || return 1
    grep -a -q "line_three" "$canonical_file" || return 1
}

paste_large() {
    LOG_FILE="$E2E_LOG_DIR/paste_large.log"
    local output_file="$E2E_LOG_DIR/paste_large.pty"

    log_test_start "paste_large"
    PTY_TEST_NAME="paste_large"

    local payload
    payload="$(printf 'a%.0s' {1..4096})"

    PTY_SEND=$'\x1b[200~'"$payload"$'\x1b[201~' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Paste:" "$canonical_file" || return 1
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 2000 ]] || return 1
}

paste_unicode() {
    LOG_FILE="$E2E_LOG_DIR/paste_unicode.log"
    local output_file="$E2E_LOG_DIR/paste_unicode.pty"
    local fixture="$FIXTURE_DIR/paste_unicode.txt"

    log_test_start "paste_unicode"
    PTY_TEST_NAME="paste_unicode"

    if [[ ! -f "$fixture" ]]; then
        log_error "Missing fixture: $fixture"
        return 1
    fi

    local payload
    payload="$(cat "$fixture")"

    PTY_SEND=$'\x1b[200~'"$payload"$'\x1b[201~' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Paste: こんにちは" "$canonical_file" || return 1
    grep -a -q "café" "$canonical_file" || return 1
}

paste_embedded_escape() {
    LOG_FILE="$E2E_LOG_DIR/paste_embedded_escape.log"
    local output_file="$E2E_LOG_DIR/paste_embedded_escape.pty"

    log_test_start "paste_embedded_escape"
    PTY_TEST_NAME="paste_embedded_escape"

    local payload
    payload=$'alpha\x1b[31mbeta\x1b[0m gamma'

    PTY_SEND=$'\x1b[200~'"$payload"$'\x1b[201~' \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Paste: alpha" "$canonical_file" || return 1
    grep -a -q "beta" "$canonical_file" || return 1
    grep -a -q "gamma" "$canonical_file" || return 1
}

paste_dos_limit() {
    LOG_FILE="$E2E_LOG_DIR/paste_dos_limit.log"
    local output_file="$E2E_LOG_DIR/paste_dos_limit.pty"
    local payload_file="$E2E_LOG_DIR/paste_dos_payload.bin"

    log_test_start "paste_dos_limit"
    PTY_TEST_NAME="paste_dos_limit"

    if [[ -z "${E2E_PYTHON:-}" ]]; then
        log_test_fail "paste_dos_limit" "E2E_PYTHON missing"
        return 1
    fi

    "$E2E_PYTHON" - "$payload_file" <<'PY'
import sys

path = sys.argv[1]
max_len = 1024 * 1024
prefix = "PREFIX-"
tail_marker = "TAIL-"
marker_len = 64

if len(prefix) + marker_len >= max_len:
    raise SystemExit("Marker setup exceeds max length")

marker = tail_marker + ("Z" * (marker_len - len(tail_marker)))
fill_len = (max_len - len(prefix) - marker_len) + 1
content = prefix + ("A" * fill_len) + marker

if len(content) <= max_len:
    raise SystemExit("Payload does not exceed MAX_PASTE_LEN")

with open(path, "wb") as handle:
    handle.write(b"\x1b[200~")
    handle.write(content.encode("ascii"))
    handle.write(b"\x1b[201~")
PY

    PTY_SEND_FILE="$payload_file" \
    PTY_SEND_DELAY_MS=300 \
    FTUI_HARNESS_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    local canonical_file="${PTY_CANONICAL_FILE:-$output_file}"
    grep -a -q "Paste: TAIL-" "$canonical_file" || return 1
    if grep -a -q "PREFIX-" "$canonical_file"; then
        return 1
    fi
}

FAILURES=0
run_case "paste_basic" paste_basic         || FAILURES=$((FAILURES + 1))
run_case "paste_multiline" paste_multiline || FAILURES=$((FAILURES + 1))
run_case "paste_large" paste_large         || FAILURES=$((FAILURES + 1))
run_case "paste_unicode" paste_unicode     || FAILURES=$((FAILURES + 1))
run_case "paste_embedded_escape" paste_embedded_escape || FAILURES=$((FAILURES + 1))
run_case "paste_dos_limit" paste_dos_limit || FAILURES=$((FAILURES + 1))
exit "$FAILURES"

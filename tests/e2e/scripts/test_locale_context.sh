#!/bin/bash
# Locale Context Provider E2E Test Suite for FrankenTUI
# bd-ic6i.2: Locale switching + overrides with JSONL logging
#
# Validates:
# 1. System locale detection (LC_ALL/LANG)
# 2. Base locale override via FTUI_HARNESS_LOCALE
# 3. Scoped locale overrides via FTUI_HARNESS_LOCALE_OVERRIDE
# 4. Locale switch triggers re-render
#
# JSONL Log Schema:
#   {"ts":"<utc>","test":"<name>","output_size":<bytes>,"checksum":"<sha>",...}

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

# Invariants (Alien Artifact):
# 1. Base locale reflects system locale when no override is set.
# 2. Scoped overrides do not mutate the base locale.
# 3. Locale switch triggers a new render with updated locale.
#
# Failure Modes:
# | Scenario                     | Expected Behavior                  |
# |-----------------------------|------------------------------------|
# | Empty locale env             | Falls back to "en"                 |
# | Invalid locale token         | Normalized or falls back to "en"   |
# | Switch target missing         | No switch, no crash                |

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/locale_context_missing.log"
    for t in locale_system_detection locale_override locale_switch; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

compute_checksum() {
    local file="$1"
    if [[ -f "$file" ]]; then
        sha256sum "$file" | cut -d' ' -f1 | head -c 16
    else
        echo "no_file"
    fi
}

run_case() {
    local name="$1"
    shift
    local start_ms
    # BSD date lacks %N; elapsed timing uses the real monotonic clock.
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
    log_test_fail "$name" "assertion failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "assertion failed"
    return 1
}

locale_system_detection() {
    LOG_FILE="$E2E_LOG_DIR/locale_system_detection.log"
    local output_file="$E2E_LOG_DIR/locale_system_detection.pty"

    log_test_start "locale_system_detection"

    LC_ALL="es_ES.UTF-8" \
    FTUI_HARNESS_VIEW=locale \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q "Base locale: es-ES" "$output_file" || return 1
    grep -a -q "System locale: es-ES" "$output_file" || return 1

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    local checksum
    checksum=$(compute_checksum "$output_file")
    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"locale_system_detection\",\"output_size\":$size,\"checksum\":\"$checksum\",\"locale\":\"es-ES\"}" >> "$LOG_FILE"
}

locale_override() {
    LOG_FILE="$E2E_LOG_DIR/locale_override.log"
    local output_file="$E2E_LOG_DIR/locale_override.pty"

    log_test_start "locale_override"

    FTUI_HARNESS_VIEW=locale \
    FTUI_HARNESS_LOCALE=en \
    FTUI_HARNESS_LOCALE_OVERRIDE=fr \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q "Base locale: en" "$output_file" || return 1
    grep -a -q "Current locale: fr" "$output_file" || return 1
    grep -a -q "Override: fr" "$output_file" || return 1

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    local checksum
    checksum=$(compute_checksum "$output_file")
    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"locale_override\",\"output_size\":$size,\"checksum\":\"$checksum\",\"base\":\"en\",\"override\":\"fr\"}" >> "$LOG_FILE"
}

locale_switch() {
    LOG_FILE="$E2E_LOG_DIR/locale_switch.log"
    local output_file="$E2E_LOG_DIR/locale_switch.pty"

    log_test_start "locale_switch"

    FTUI_HARNESS_VIEW=locale \
    FTUI_HARNESS_LOCALE=en \
    FTUI_HARNESS_LOCALE_SWITCH_TO=de \
    FTUI_HARNESS_LOCALE_SWITCH_MS=200 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_EXIT_AFTER_MS=900 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    grep -a -q "Locale switch -> de" "$output_file" || return 1
    grep -a -q "Current locale: de" "$output_file" || return 1

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    local checksum
    checksum=$(compute_checksum "$output_file")
    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"locale_switch\",\"output_size\":$size,\"checksum\":\"$checksum\",\"base\":\"en\",\"target\":\"de\"}" >> "$LOG_FILE"
}

run_case "locale_system_detection" locale_system_detection
run_case "locale_override" locale_override
run_case "locale_switch" locale_switch

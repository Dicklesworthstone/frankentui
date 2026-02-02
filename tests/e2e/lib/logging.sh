#!/usr/bin/env bash
set -euo pipefail

LOG_LEVEL=${LOG_LEVEL:-INFO}
LOG_DIR=${LOG_DIR:-/tmp/ftui_e2e_logs}
LOG_FILE=${LOG_FILE:-"$LOG_DIR/ftui_e2e.log"}

log() {
    local level="$1"
    shift
    local timestamp
    timestamp=$(date +"%Y-%m-%d %H:%M:%S.%3N")
    local caller="${BASH_SOURCE[2]}:${BASH_LINENO[1]}"
    printf '[%s] [%s] [%s] %s\n' "$timestamp" "$level" "$caller" "$*" | tee -a "$LOG_FILE"
}

log_debug() {
    if [[ "$LOG_LEVEL" == "DEBUG" ]]; then
        log "DEBUG" "$@"
    fi
}

log_info() {
    log "INFO" "$@"
}

log_warn() {
    log "WARN" "$@"
}

log_error() {
    log "ERROR" "$@"
}

log_test_start() {
    local test_name="$1"
    log_info "=========================================="
    log_info "STARTING TEST: $test_name"
    log_info "=========================================="
}

log_test_pass() {
    local test_name="$1"
    log_info "PASS: $test_name"
}

log_test_fail() {
    local test_name="$1"
    local reason="$2"
    log_error "FAIL: $test_name"
    log_error "Reason: $reason"
    log_error "Log file: $LOG_FILE"
}

log_hex_dump() {
    local label="$1"
    local data="$2"
    if command -v xxd >/dev/null 2>&1; then
        log_debug "$label (hex): $(printf '%s' "$data" | xxd -p | tr -d '\n')"
    else
        log_debug "$label (hex): xxd not available"
    fi
}

#!/bin/bash
set -euo pipefail

LOG_LEVEL="${LOG_LEVEL:-INFO}"
E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_logs}"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-/tmp/ftui_e2e_results}"
LOG_FILE="${LOG_FILE:-$E2E_LOG_DIR/e2e.log}"

log() {
    local level="$1"
    shift
    local ts
    ts="$(date +"%Y-%m-%d %H:%M:%S.%3N")"
    echo "[$ts] [$level] $*" | tee -a "$LOG_FILE"
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
    local name="$1"
    log_info "========================================"
    log_info "STARTING TEST: $name"
    log_info "========================================"
}

log_test_pass() {
    local name="$1"
    log_info "PASS: $name"
}

log_test_fail() {
    local name="$1"
    local reason="$2"
    log_error "FAIL: $name"
    log_error "  Reason: $reason"
    log_error "  Log file: $LOG_FILE"
}

log_test_skip() {
    local name="$1"
    local reason="$2"
    log_warn "SKIP: $name"
    log_warn "  Reason: $reason"
}

record_result() {
    local name="$1"
    local status="$2"
    local duration_ms="$3"
    local log_file="$4"
    local error_msg="${5:-}"

    mkdir -p "$E2E_RESULTS_DIR"

    local result_file
    result_file="$E2E_RESULTS_DIR/${name}_$(date +%s%N)_$$.json"

    if command -v jq >/dev/null 2>&1; then
        if [[ -n "$error_msg" ]]; then
            jq -n \
                --arg name "$name" \
                --arg status "$status" \
                --argjson duration_ms "$duration_ms" \
                --arg log_file "$log_file" \
                --arg error "$error_msg" \
                '{name:$name,status:$status,duration_ms:$duration_ms,log_file:$log_file,error:$error}' \
                > "$result_file"
        else
            jq -n \
                --arg name "$name" \
                --arg status "$status" \
                --argjson duration_ms "$duration_ms" \
                --arg log_file "$log_file" \
                '{name:$name,status:$status,duration_ms:$duration_ms,log_file:$log_file}' \
                > "$result_file"
        fi
    else
        local safe_error
        safe_error="$(printf '%s' "$error_msg" | sed 's/"/\\"/g')"
        if [[ -n "$safe_error" ]]; then
            printf '{"name":"%s","status":"%s","duration_ms":%s,"log_file":"%s","error":"%s"}\n' \
                "$name" "$status" "$duration_ms" "$log_file" "$safe_error" \
                > "$result_file"
        else
            printf '{"name":"%s","status":"%s","duration_ms":%s,"log_file":"%s"}\n' \
                "$name" "$status" "$duration_ms" "$log_file" \
                > "$result_file"
        fi
    fi
}

finalize_summary() {
    local summary_file="$1"
    local end_ms
    end_ms="$(date +%s%3N)"
    local start_ms="${E2E_RUN_START_MS:-$end_ms}"
    local duration_ms=$((end_ms - start_ms))

    if command -v jq >/dev/null 2>&1; then
        jq -s \
            --arg timestamp "$(date -Iseconds)" \
            --argjson duration_ms "$duration_ms" \
            '{
                timestamp: $timestamp,
                total: length,
                passed: (map(select(.status=="passed")) | length),
                failed: (map(select(.status=="failed")) | length),
                skipped: (map(select(.status=="skipped")) | length),
                duration_ms: $duration_ms,
                tests: .
            }' \
            "$E2E_RESULTS_DIR"/*.json > "$summary_file"
    else
        local total
        total=$(ls -1 "$E2E_RESULTS_DIR"/*.json 2>/dev/null | wc -l | tr -d ' ')
        cat > "$summary_file" <<EOF_SUM
{"timestamp":"$(date -Iseconds)","total":${total},"passed":0,"failed":0,"skipped":0,"duration_ms":${duration_ms},"tests":[]}
EOF_SUM
    fi
}

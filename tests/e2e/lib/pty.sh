#!/usr/bin/env bash
set -euo pipefail

run_pty() {
    local cmd="$1"
    local input="${2-}"
    local output_file="$3"
    local timeout_seconds="${4:-10}"

    if [[ -n "$input" ]]; then
        printf '%b' "$input" | timeout "${timeout_seconds}s" script -q -c "$cmd" "$output_file" >/dev/null
    else
        timeout "${timeout_seconds}s" script -q -c "$cmd" "$output_file" >/dev/null
    fi
}

expect_output_contains() {
    local output_file="$1"
    local needle="$2"

    if ! grep -a -q "$needle" "$output_file"; then
        return 1
    fi
}

expect_ansi_sequence() {
    local output_file="$1"
    local pattern="$2"

    if ! grep -a -P -q "$pattern" "$output_file"; then
        return 1
    fi
}

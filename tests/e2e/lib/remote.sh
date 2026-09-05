#!/bin/bash
set -euo pipefail

# Remote session helpers for E2E scripts.
#
# Starts the frankenterm_ws_bridge binary and provides functions
# for managing its lifecycle during scripted remote terminal sessions.
#
# Usage:
#   source "$LIB_DIR/remote.sh"
#   remote_start [--cols N] [--rows N] [--cmd path] [--port N]
#   remote_wait_ready
#   remote_stop
#
# Depends on: common.sh (for PROJECT_ROOT)

REMOTE_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REMOTE_PROJECT_ROOT="${PROJECT_ROOT:-$(cd "$REMOTE_LIB_DIR/../.." && pwd)}"

# --- Configuration (override via env) ---
REMOTE_PORT="${REMOTE_PORT:-9231}"
REMOTE_COLS="${REMOTE_COLS:-120}"
REMOTE_ROWS="${REMOTE_ROWS:-40}"
REMOTE_CMD="${REMOTE_CMD:-/bin/sh}"
REMOTE_TERM="${REMOTE_TERM:-xterm-256color}"
REMOTE_BRIDGE_PID=""
REMOTE_TELEMETRY_FILE=""
REMOTE_WS_CLIENT="${REMOTE_LIB_DIR}/ws_client.py"
REMOTE_TARGET_DIR="${REMOTE_TARGET_DIR:-$REMOTE_PROJECT_ROOT/target/remote-e2e}"
REMOTE_ALLOW_LOCAL_CARGO_FALLBACK="${REMOTE_ALLOW_LOCAL_CARGO_FALLBACK:-0}"

# Run cargo build commands via rch when available (falls back to local cargo).
remote_run_cargo_build() {
    if command -v rch >/dev/null 2>&1; then
        rch exec -- cargo "$@"
        return
    fi
    cargo "$@"
}

# Build locally (opt-in fallback when remote artifact retrieval does not
# materialize binaries needed for local execution, such as ws_bridge).
remote_run_local_cargo_build() {
    (
        cd "$REMOTE_PROJECT_ROOT"
        cargo "$@"
    )
}

# Build the ws_bridge binary if not already built.
remote_build_bridge() {
    local bin_path
    bin_path="$(remote_bridge_path)"
    if [[ -x "$bin_path" ]]; then
        return 0
    fi
    echo "[remote] Building frankenterm_ws_bridge..." >&2
    if remote_run_cargo_build build -p ftui-pty --bin frankenterm_ws_bridge --release \
        --target-dir "$REMOTE_TARGET_DIR" 2>&1 | tail -3 >&2; then
        if [[ -x "$bin_path" ]]; then
            return 0
        fi
        echo "[remote] WARN: remote build succeeded but bridge binary is not present locally at $bin_path" >&2
    fi

    if [[ "$REMOTE_ALLOW_LOCAL_CARGO_FALLBACK" != "1" ]]; then
        echo "[remote] ERROR: bridge binary missing after rch build: $bin_path" >&2
        echo "[remote] Hint: set REMOTE_ALLOW_LOCAL_CARGO_FALLBACK=1 to allow local cargo fallback." >&2
        return 1
    fi

    # Some environments have rustup configured without cargo for nightly.
    # Fall back to stable so remote E2E fixtures can still run.
    echo "[remote] retrying build via local cargo (opt-in fallback)..." >&2
    if remote_run_local_cargo_build build -p ftui-pty --bin frankenterm_ws_bridge --release \
        --target-dir "$REMOTE_TARGET_DIR" 2>&1 | tail -3 >&2; then
        if [[ -x "$bin_path" ]]; then
            return 0
        fi
    fi

    echo "[remote] local nightly build failed; retrying local stable toolchain..." >&2
    if remote_run_local_cargo_build +stable build -p ftui-pty --bin frankenterm_ws_bridge --release \
        --target-dir "$REMOTE_TARGET_DIR" 2>&1 | tail -3 >&2; then
        if [[ -x "$bin_path" ]]; then
            return 0
        fi
    fi

    echo "[remote] ERROR: bridge binary missing after fallback attempts: $bin_path" >&2
    return 1
}

# Return path to the ws_bridge binary.
remote_bridge_path() {
    printf '%s' "$REMOTE_TARGET_DIR/release/frankenterm_ws_bridge"
}

# Start the ws_bridge server.
# Usage: remote_start [--cols N] [--rows N] [--cmd path] [--port N]
remote_start() {
    local cols="$REMOTE_COLS"
    local rows="$REMOTE_ROWS"
    local cmd="$REMOTE_CMD"
    local port="$REMOTE_PORT"
    local telemetry_dir="${REMOTE_TELEMETRY_DIR:-$E2E_LOG_DIR}"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --cols) cols="$2"; shift 2 ;;
            --rows) rows="$2"; shift 2 ;;
            --cmd) cmd="$2"; shift 2 ;;
            --port) port="$2"; shift 2 ;;
            *) shift ;;
        esac
    done

    if ! remote_build_bridge; then
        return 1
    fi

    mkdir -p "$telemetry_dir"
    REMOTE_TELEMETRY_FILE="$telemetry_dir/ws_bridge_telemetry.jsonl"

    local bridge_bin
    bridge_bin="$(remote_bridge_path)"

    "$bridge_bin" \
        --bind "127.0.0.1:${port}" \
        --cols "$cols" \
        --rows "$rows" \
        --cmd "$cmd" \
        --term "$REMOTE_TERM" \
        --telemetry "$REMOTE_TELEMETRY_FILE" \
        --accept-once &
    REMOTE_BRIDGE_PID=$!

    # Update port for client usage.
    REMOTE_PORT="$port"
}

# Wait until the bridge is listening (max 10s).
# Inspects the listen state without making a TCP connection,
# since the bridge in --accept-once mode would consume the connection.
remote_wait_ready() {
    local port="${REMOTE_PORT:-9231}"
    local probe=(ss -tln "sport = :${port}")
    if [[ "$OSTYPE" == darwin* ]]; then
        probe=(lsof -nP -a -p "$REMOTE_BRIDGE_PID" -iTCP:"$port" -sTCP:LISTEN)
    fi
    require_tools "${probe[0]}" grep sleep || return 2
    local max_wait=100  # 100 * 100ms = 10s
    local i=0
    while :; do
        # A stale listener must not hide the requested bridge's exit.
        if [[ -n "$REMOTE_BRIDGE_PID" ]] && ! kill -0 "$REMOTE_BRIDGE_PID" 2>/dev/null; then
            echo "[remote] ERROR: bridge process died (PID=$REMOTE_BRIDGE_PID)" >&2
            return 1
        fi
        if "${probe[@]}" 2>/dev/null | command grep -q "LISTEN"; then
            return 0
        fi
        i=$((i + 1))
        if [[ $i -ge $max_wait ]]; then
            echo "[remote] ERROR: bridge not listening on port $port after 10s" >&2
            return 1
        fi
        sleep 0.1
    done
}

# Stop the bridge process.
remote_stop() {
    if [[ -n "$REMOTE_BRIDGE_PID" ]]; then
        kill "$REMOTE_BRIDGE_PID" 2>/dev/null || true
        wait "$REMOTE_BRIDGE_PID" 2>/dev/null || true
        REMOTE_BRIDGE_PID=""
    fi
}

# Run the Python WebSocket client with a scenario.
# Usage: remote_run_scenario <scenario_json_path> [extra_args...]
remote_run_scenario() {
    local scenario_path="$1"; shift
    local port="${REMOTE_PORT:-9231}"
    local python="${E2E_PYTHON:-python3}"

    "$python" "$REMOTE_WS_CLIENT" \
        --url "ws://127.0.0.1:${port}" \
        --scenario "$scenario_path" \
        "$@"
}

# Cleanup trap for scripts.
remote_cleanup() {
    remote_stop
}

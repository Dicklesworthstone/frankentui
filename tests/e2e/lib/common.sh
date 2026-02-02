#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

require_command() {
    local cmd="$1"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "Missing required command: $cmd" >&2
        return 1
    fi
}

ensure_log_dir() {
    mkdir -p "$LOG_DIR"
}

now_ms() {
    python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
}

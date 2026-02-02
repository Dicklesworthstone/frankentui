#!/bin/bash
set -euo pipefail

E2E_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT_ROOT="$(cd "$E2E_ROOT/../.." && pwd)"

require_cmd() {
    local cmd="$1"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "Missing required command: $cmd" >&2
        return 1
    fi
}

resolve_python() {
    if command -v python3 >/dev/null 2>&1; then
        echo "python3"
        return 0
    fi
    if command -v python >/dev/null 2>&1; then
        echo "python"
        return 0
    fi
    echo "" >&2
    return 1
}

E2E_PYTHON="${E2E_PYTHON:-}"
if [[ -z "$E2E_PYTHON" ]]; then
    E2E_PYTHON="$(resolve_python)" || true
fi

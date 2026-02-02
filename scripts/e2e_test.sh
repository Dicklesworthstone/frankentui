#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

VERBOSE=false
QUICK=false

for arg in "$@"; do
    case "$arg" in
        --verbose|-v)
            VERBOSE=true
            ;;
        --quick|-q)
            QUICK=true
            ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--quick]"
            echo ""
            echo "Options:"
            echo "  --verbose, -v   Show detailed output"
            echo "  --quick, -q     Reserved for future quick mode"
            exit 0
            ;;
    esac
    shift || true
done

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
LOG_DIR=${LOG_DIR:-/tmp/ftui_e2e_${TIMESTAMP}}
LOG_LEVEL=${LOG_LEVEL:-INFO}

if $VERBOSE; then
    LOG_LEVEL=DEBUG
fi

export LOG_DIR
export LOG_LEVEL

if $QUICK; then
    export FTUI_E2E_QUICK=1
fi

echo "=============================================="
echo "  FrankenTUI E2E Test Suite"
echo "=============================================="
echo "Project root: $PROJECT_ROOT"
echo "Log directory: $LOG_DIR"
echo "Started at: $(date -Iseconds)"

"$PROJECT_ROOT/tests/e2e/scripts/run_all.sh"

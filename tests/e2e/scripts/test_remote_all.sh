#!/bin/bash
set -euo pipefail

# Master runner for all remote terminal E2E tests (bd-lff4p.2.17)
#
# Runs all remote session scenarios sequentially and reports results.
#
# Usage:
#   ./test_remote_all.sh
#   E2E_SEED=42 ./test_remote_all.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TESTS=(
    test_remote_resize_storm
    test_remote_paste
    test_remote_selection_copy
    test_remote_search
    test_remote_unicode
    test_remote_osc8_links
    test_remote_scrollback
)

PASSED=0
FAILED=0
ERRORS=()

echo "=========================================="
echo "  Remote Terminal E2E Test Suite"
echo "  $(date -Iseconds)"
echo "=========================================="
echo ""

for test_name in "${TESTS[@]}"; do
    script="$SCRIPT_DIR/${test_name}.sh"
    if [[ ! -x "$script" ]]; then
        echo "[SKIP] $test_name (not found or not executable)"
        continue
    fi

    echo "--- Running: $test_name ---"
    if bash "$script" 2>&1; then
        PASSED=$((PASSED + 1))
    else
        FAILED=$((FAILED + 1))
        ERRORS+=("$test_name")
    fi
    echo ""
done

echo "=========================================="
echo "  Results: $PASSED passed, $FAILED failed"
echo "=========================================="

if [[ $FAILED -gt 0 ]]; then
    echo "  Failed tests:"
    for err in "${ERRORS[@]}"; do
        echo "    - $err"
    done
    exit 1
fi

echo "  All remote E2E tests passed."

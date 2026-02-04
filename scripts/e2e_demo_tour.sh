#!/usr/bin/env bash
# E2E test for Guided Tour Mode (bd-iuvb.1)
#
# Generates JSONL logs with:
# - run_id, step_id, screen_id, duration_ms, seed, size, mode, caps_profile
# - action, outcome, checksum (if present)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOG_DIR="${PROJECT_ROOT}/target/e2e-logs"
TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
RUN_ID="tour_${TIMESTAMP}"
LOG_FILE="${LOG_DIR}/guided_tour_${TIMESTAMP}.jsonl"
STDOUT_LOG="${LOG_DIR}/guided_tour_${TIMESTAMP}.log"

mkdir -p "$LOG_DIR"

# -----------------------------------------------------------------------
# Environment info
# -----------------------------------------------------------------------

echo '=== Guided Tour E2E (bd-iuvb.1) ==='
echo "Date: $(date -Iseconds)"
echo "Log: $LOG_FILE"
echo

cat > "$LOG_FILE" <<EOF_ENV
{"type":"env","timestamp":"$(date -Iseconds)","rust_version":"$(rustc --version 2>/dev/null || echo 'unknown')","platform":"$(uname -s)","arch":"$(uname -m)","run_id":"${RUN_ID}"}
EOF_ENV

# -----------------------------------------------------------------------
# Build
# -----------------------------------------------------------------------

echo "Building ftui-demo-showcase (debug)..."
if cargo build -p ftui-demo-showcase > "$STDOUT_LOG" 2>&1; then
    echo '{"type":"build","status":"success","target":"ftui-demo-showcase"}' >> "$LOG_FILE"
else
    echo '{"type":"build","status":"failed","target":"ftui-demo-showcase"}' >> "$LOG_FILE"
    echo "FAIL: Build failed (see $STDOUT_LOG)"
    exit 1
fi

# -----------------------------------------------------------------------
# Run guided tour
# -----------------------------------------------------------------------

echo "Running guided tour..."

CMD=(
    cargo run -p ftui-demo-showcase --
    --tour
    --tour-speed=1.0
    --tour-start-step=1
    --exit-after-ms=7000
)

ENV_VARS=(
    "FTUI_TOUR_REPORT_PATH=$LOG_FILE"
    "FTUI_TOUR_RUN_ID=$RUN_ID"
    "FTUI_TOUR_SEED=0"
    "FTUI_TOUR_CAPS_PROFILE=${TERM:-unknown}"
    "FTUI_DEMO_SCREEN_MODE=alt"
)

if command -v timeout >/dev/null 2>&1; then
    if env "${ENV_VARS[@]}" timeout 12s "${CMD[@]}" >> "$STDOUT_LOG" 2>&1; then
        echo '{"type":"run","status":"success","mode":"alt"}' >> "$LOG_FILE"
    else
        echo '{"type":"run","status":"failed","mode":"alt"}' >> "$LOG_FILE"
        echo "FAIL: Run failed (see $STDOUT_LOG)"
        exit 1
    fi
else
    if env "${ENV_VARS[@]}" "${CMD[@]}" >> "$STDOUT_LOG" 2>&1; then
        echo '{"type":"run","status":"success","mode":"alt"}' >> "$LOG_FILE"
    else
        echo '{"type":"run","status":"failed","mode":"alt"}' >> "$LOG_FILE"
        echo "FAIL: Run failed (see $STDOUT_LOG)"
        exit 1
    fi
fi

# -----------------------------------------------------------------------
# Verify JSONL output
# -----------------------------------------------------------------------

if ! grep -q '"event":"tour"' "$LOG_FILE"; then
    echo "FAIL: No tour JSONL entries found"
    exit 1
fi

if ! grep -q '"action":"start"' "$LOG_FILE"; then
    echo "FAIL: Missing tour start log entry"
    exit 1
fi

echo '{"type":"summary","status":"pass"}' >> "$LOG_FILE"

echo "PASS: Guided tour logs captured at $LOG_FILE"

echo
exit 0

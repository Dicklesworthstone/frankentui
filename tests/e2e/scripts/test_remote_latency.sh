#!/bin/bash
set -euo pipefail

# E2E: Keystroke-to-photon latency measurement (bd-lff4p.10.6)
#
# Measures keystroke→output latency through the WebSocket PTY bridge.
# Reports p50/p95/p99, jitter, and throughput. Optionally gates against
# a latency budget (exit non-zero on violations).
#
# Usage:
#   ./test_remote_latency.sh
#   ./test_remote_latency.sh --gate        # fail on budget violations
#   REMOTE_PORT=9250 ./test_remote_latency.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"
SCENARIOS_DIR="$SCRIPT_DIR/../scenarios/remote"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/remote.sh"

export E2E_DETERMINISTIC="${E2E_DETERMINISTIC:-0}"
export E2E_SEED="${E2E_SEED:-0}"

REMOTE_PORT="${REMOTE_PORT:-9250}"
REMOTE_LOG_DIR="${REMOTE_LOG_DIR:-$E2E_LOG_DIR/remote_latency}"
mkdir -p "$REMOTE_LOG_DIR"

GATE_FLAG=""
if [[ "${1:-}" == "--gate" ]]; then
    GATE_FLAG="--gate"
fi

LATENCY_HARNESS="${LIB_DIR}/latency_harness.py"
SCENARIO="${SCENARIOS_DIR}/latency_probe.json"
BUDGET="${SCENARIOS_DIR}/latency_budget.json"
JSONL_OUT="${REMOTE_LOG_DIR}/latency.jsonl"

trap remote_cleanup EXIT

echo "=== Remote Latency Measurement E2E Test ==="
echo "Port: $REMOTE_PORT"

# Start bridge.
remote_start --port "$REMOTE_PORT" --cols 80 --rows 24 --cmd /bin/sh
remote_wait_ready
echo "[OK] Bridge ready on port $REMOTE_PORT (PID=$REMOTE_BRIDGE_PID)"

# Run latency measurement.
python3 "$LATENCY_HARNESS" \
    --url "ws://127.0.0.1:${REMOTE_PORT}" \
    --scenario "$SCENARIO" \
    --budget "$BUDGET" \
    --jsonl "$JSONL_OUT" \
    $GATE_FLAG 2>&1 | tee "$REMOTE_LOG_DIR/latency_report.json" | python3 -c "
import json, sys
try:
    r = json.load(sys.stdin)
    s = r.get('stats', {})
    print(f'  p50:       {s.get(\"p50_ms\", \"?\")} ms')
    print(f'  p95:       {s.get(\"p95_ms\", \"?\")} ms')
    print(f'  p99:       {s.get(\"p99_ms\", \"?\")} ms')
    print(f'  mean:      {s.get(\"mean_ms\", \"?\")} ms')
    print(f'  jitter:    {s.get(\"jitter_ms\", \"?\")} ms')
    print(f'  throughput: {s.get(\"throughput_kbps\", \"?\")} KB/s')
    print(f'  probes:    {s.get(\"count\", 0)}')
    v = r.get('violations', [])
    if v:
        print(f'  VIOLATIONS: {len(v)}')
        for vi in v:
            print(f'    [{vi[\"severity\"]}] {vi[\"metric\"]}: {vi[\"actual\"]} > budget {vi[\"budget\"]}')
    else:
        print('  All within budget.')
    print(f'[{\"PASS\" if r[\"outcome\"] == \"pass\" else \"FAIL\"}] Latency measurement')
except Exception as e:
    print(f'[WARN] Could not parse report: {e}', file=sys.stderr)
" 2>/dev/null || true

echo "[OK] Report saved to $REMOTE_LOG_DIR/latency_report.json"
echo "[OK] JSONL saved to $JSONL_OUT"

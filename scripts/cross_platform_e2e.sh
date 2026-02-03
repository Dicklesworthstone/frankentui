#!/usr/bin/env bash
set -euo pipefail

LOG_DIR="${LOG_DIR:-/tmp/ftui_cross_platform_e2e_$(date +%Y%m%d_%H%M%S)}"
LOG_JSONL="$LOG_DIR/cross_platform_e2e.jsonl"
mkdir -p "$LOG_DIR"

log_json() {
  local event="$1"
  local message="$2"
  python3 - "$event" "$message" <<'PY' >> "$LOG_JSONL"
import json
import sys
import time

event = sys.argv[1]
message = sys.argv[2]
print(json.dumps({
    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "event": event,
    "message": message,
}))
PY
}

run_step() {
  local name="$1"
  shift
  log_json "step_start" "$name"
  if "$@"; then
    log_json "step_end" "$name: ok"
  else
    local status=$?
    log_json "step_end" "$name: failed ($status)"
    return "$status"
  fi
}

log_json "env" "platform=$(uname -s) term=${TERM:-unknown} shell=${SHELL:-unknown}"

run_step "build_release" cargo build -p ftui-demo-showcase --release
run_step "test_core" cargo test -p ftui-core -- --nocapture
run_step "test_render" cargo test -p ftui-render -- --nocapture
run_step "test_showcase_snapshots" cargo test -p ftui-demo-showcase --test screen_snapshots

log_json "summary" "logs=$LOG_JSONL"

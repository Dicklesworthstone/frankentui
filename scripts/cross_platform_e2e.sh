#!/usr/bin/env bash
set -euo pipefail

LOG_DIR="${LOG_DIR:-/tmp/ftui_cross_platform_e2e_$(date +%Y%m%d_%H%M%S)}"
LOG_JSONL="$LOG_DIR/cross_platform_e2e.jsonl"
mkdir -p "$LOG_DIR"

log_json() {
  local event="$1"
  local message="$2"
  local python_bin="${PYTHON_BIN:-}"
  if [[ -z "$python_bin" ]]; then
    if command -v python3 >/dev/null 2>&1; then
      python_bin="python3"
    elif command -v python >/dev/null 2>&1; then
      python_bin="python"
    else
      echo "python or python3 is required for JSONL logging" >&2
      return 1
    fi
  fi
  "$python_bin" - "$event" "$message" <<'PY' >> "$LOG_JSONL"
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

MODE="default"
if [[ "${1:-}" == "--windows-smoke" ]]; then
  MODE="windows-smoke"
  shift
fi

SMOKE_JSONL="${SMOKE_JSONL:-$LOG_DIR/windows-smoke.jsonl}"

run_smoke_binary() {
  local label="$1"
  shift
  local out_file="$LOG_DIR/${label}.out"
  local err_file="$LOG_DIR/${label}.err"
  local start_ms
  start_ms=$(python3 -c 'import time; print(int(time.time() * 1000))')
  set +e
  FTUI_EXIT_AFTER_MS=300 RUST_LOG=info,ftui.runtime=debug "$@" >"$out_file" 2>"$err_file"
  local rc=$?
  set -e
  local end_ms
  end_ms=$(python3 -c 'import time; print(int(time.time() * 1000))')
  local elapsed_ms=$((end_ms - start_ms))

  if grep -q "panicked at" "$err_file" 2>/dev/null; then
    echo "FAIL: $label panicked (see $err_file)" >&2
    return 1
  fi

  local outcome=""
  local cursor_restore="false"
  if python3 - "$out_file" <<'PY'
import pathlib, sys
sys.exit(0 if b"\x1b[?25h" in pathlib.Path(sys.argv[1]).read_bytes() else 1)
PY
  then
    cursor_restore="true"
  fi

  if [[ "$rc" -eq 0 && "$cursor_restore" == "true" ]]; then
    outcome="exit_after_ms"
  elif [[ "$rc" -ne 0 ]] && grep -qi "not a console" "$err_file" 2>/dev/null; then
    outcome="not_a_console"
  else
    echo "FAIL: $label unexpected exit rc=$rc cursor_restore=$cursor_restore (see $err_file)" >&2
    return 1
  fi

  python3 - "$label" "$outcome" "$rc" "$elapsed_ms" "$cursor_restore" "$SMOKE_JSONL" <<'PY'
import json, pathlib, sys
label, outcome, rc, elapsed_ms, cursor_restore, out_path = sys.argv[1:7]
record = {
    "binary": label,
    "outcome": outcome,
    "exit_code": int(rc),
    "elapsed_ms": int(elapsed_ms),
    "cursor_restore": cursor_restore == "true",
}
with pathlib.Path(out_path).open("a", encoding="utf-8") as fh:
    fh.write(json.dumps(record) + "\n")
PY
}

log_json "env" "platform=$(uname -s) mode=$MODE term=${TERM:-unknown} shell=${SHELL:-unknown}"

if [[ "$MODE" == "windows-smoke" ]]; then
  TARGET_DIR="${CARGO_TARGET_DIR:-target}"
  run_step "test_ftui_tty" env RCH_CARGO_WRAPPER_BYPASS=1 cargo test -p ftui-tty --lib
  run_step "test_default_backend" env RCH_CARGO_WRAPPER_BYPASS=1 cargo test -p ftui --test default_backend
  run_step "test_wt_session_caps" env RCH_CARGO_WRAPPER_BYPASS=1 cargo test -p ftui-core --lib wt_session_profile_matches_windows_doc
  run_step "test_crossterm_key_map" env RCH_CARGO_WRAPPER_BYPASS=1 cargo test -p ftui-core --lib crossterm_key_event_maps_to_ftui_key
  run_step "test_simulator_resize" env RCH_CARGO_WRAPPER_BYPASS=1 cargo test -p ftui-runtime --lib simulator_resize_reflows_view
  run_step "test_exit_after_env" env RCH_CARGO_WRAPPER_BYPASS=1 cargo test -p ftui-runtime --lib exit_after_env_parses_ms_and_rejects_garbage
  run_step "test_not_a_console" env RCH_CARGO_WRAPPER_BYPASS=1 cargo test -p ftui-runtime --lib with_config_reports_not_a_console_cleanly
  if command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    run_step "win_gnu_clippy" env RCH_CARGO_WRAPPER_BYPASS=1 cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings
    run_step "win_gnu_runtime_native_feature" env RCH_CARGO_WRAPPER_BYPASS=1 cargo check -p ftui-runtime --features native-backend --target x86_64-pc-windows-gnu
    run_step "win_gnu_tests_no_run" env RCH_CARGO_WRAPPER_BYPASS=1 cargo test -p ftui-tty -p ftui --test default_backend --target x86_64-pc-windows-gnu --no-run
    run_step "win_gnu_examples" env RCH_CARGO_WRAPPER_BYPASS=1 cargo build -p ftui --examples --target x86_64-pc-windows-gnu
  fi
  run_step "build_smoke_binaries" env RCH_CARGO_WRAPPER_BYPASS=1 cargo build -p ftui --example minimal_inline --example agent_shell -p ftui-demo-showcase --bin ftui-demo-showcase
  run_step "smoke_minimal_inline" run_smoke_binary "minimal_inline" "$TARGET_DIR/debug/examples/minimal_inline"
  run_step "smoke_showcase" run_smoke_binary "ftui-demo-showcase" "$TARGET_DIR/debug/ftui-demo-showcase" --screen=1
  if [[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* || "$(uname -s)" == CYGWIN* || "${OS:-}" == "Windows_NT" ]]; then
    run_step "smoke_agent_shell" run_smoke_binary "agent_shell" "$TARGET_DIR/debug/examples/agent_shell" -- cmd.exe /C echo hi
  else
    run_step "smoke_agent_shell" run_smoke_binary "agent_shell" "$TARGET_DIR/debug/examples/agent_shell" -- cat
    if command -v setsid >/dev/null 2>&1; then
      run_step "smoke_not_a_console_crossterm" run_smoke_binary "ftui-demo-showcase-no-tty" setsid env FTUI_DEMO_BACKEND=crossterm "$TARGET_DIR/debug/ftui-demo-showcase" --screen=1 </dev/null
    fi
  fi
  log_json "summary" "logs=$LOG_JSONL smoke_jsonl=$SMOKE_JSONL"
  exit 0
fi

run_step "build_release" cargo build -p ftui-demo-showcase --release
run_step "test_core" cargo test -p ftui-core -- --nocapture
run_step "test_render" cargo test -p ftui-render -- --nocapture
run_step "test_showcase_snapshots" cargo test -p ftui-demo-showcase --test screen_snapshots

log_json "summary" "logs=$LOG_JSONL"

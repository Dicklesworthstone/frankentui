#!/usr/bin/env bash
# E2E test for Guided Tour Mode (bd-iuvb.1, bd-9o94q)
#
# Generates JSONL logs with:
# - run_id, step_id, screen_id, duration_ms, seed, size, mode, caps_profile
# - action, outcome, checksum, speed, paused

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"

LOG_DIR="${PROJECT_ROOT}/target/e2e-logs"
e2e_fixture_init "tour"
RUN_ID="${E2E_RUN_ID}"
TIMESTAMP="$(e2e_log_stamp)"
LOG_FILE="${LOG_DIR}/guided_tour_${RUN_ID}_${TIMESTAMP}.jsonl"
STDOUT_LOG="${LOG_DIR}/guided_tour_${TIMESTAMP}.log"

mkdir -p "$LOG_DIR"
export E2E_LOG_DIR="$LOG_DIR"
export E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$LOG_DIR/results}"
export E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
export E2E_JSONL_FILE="$LOG_FILE"
mkdir -p "$E2E_RESULTS_DIR"
jsonl_init

# -----------------------------------------------------------------------
# Environment info
# -----------------------------------------------------------------------

echo '=== Guided Tour E2E (bd-iuvb.1) ==='
echo "Date: $(e2e_timestamp)"
echo "Log: $LOG_FILE"
echo

# -----------------------------------------------------------------------
# Build
# -----------------------------------------------------------------------

echo "Building ftui-demo-showcase (debug)..."
build_start_ms="$(e2e_now_ms)"
jsonl_step_start "build"
if cargo build -p ftui-demo-showcase > "$STDOUT_LOG" 2>&1; then
    build_duration_ms=$(( $(e2e_now_ms) - build_start_ms ))
    jsonl_step_end "build" "success" "$build_duration_ms"
else
    build_duration_ms=$(( $(e2e_now_ms) - build_start_ms ))
    jsonl_step_end "build" "failed" "$build_duration_ms"
    echo "FAIL: Build failed (see $STDOUT_LOG)"
    jsonl_run_end "failed" "$build_duration_ms" 1
    exit 1
fi

# -----------------------------------------------------------------------
# Guided tour cases
# -----------------------------------------------------------------------

TOUR_MODES=("alt" "inline")
TOUR_SIZES=("80 24" "120 40")
INLINE_UI_HEIGHT="${FTUI_DEMO_UI_HEIGHT:-12}"

validate_tour_jsonl() {
    local log_file="$1"
    local case_id="$2"
    if [[ -z "${E2E_PYTHON:-}" ]]; then
        echo "FAIL: python not available for JSONL validation"
        exit 1
    fi
    "$E2E_PYTHON" - "$log_file" "$case_id" <<'PY'
import json
import sys

path = sys.argv[1]
case_id = sys.argv[2]
required_actions = {"start", "pause", "resume", "next", "prev", "speed_up", "speed_down", "exit"}
seen = set()
missing = []
bad_checksum = False
bad_paused = False
non_null_checksums = 0

with open(path, "r", encoding="utf-8") as handle:
    for line in handle:
        line = line.strip()
        if not line:
            continue
        try:
            data = json.loads(line)
        except json.JSONDecodeError:
            continue
        if data.get("event") != "tour":
            continue
        action = data.get("action")
        if action:
            seen.add(action)
        for field in ("step_index", "screen_id", "width", "height", "speed", "paused", "checksum"):
            if field not in data:
                missing.append((action or "unknown", field))
        checksum = data.get("checksum")
        if checksum is None:
            if action not in ("start", "exit"):
                bad_checksum = True
        else:
            non_null_checksums += 1
        paused = data.get("paused")
        if paused is not None and not isinstance(paused, bool):
            bad_paused = True

missing_actions = sorted(required_actions - seen)
if missing_actions:
    print(f"[{case_id}] missing actions: {missing_actions}")
    sys.exit(1)
if missing:
    print(f"[{case_id}] missing fields: {missing[:3]}")
    sys.exit(1)
if non_null_checksums == 0:
    print(f"[{case_id}] no checksum values recorded")
    sys.exit(1)
if bad_checksum:
    print(f"[{case_id}] checksum missing/null in tour logs")
    sys.exit(1)
if bad_paused:
    print(f"[{case_id}] paused not boolean in tour logs")
    sys.exit(1)
PY
}

run_guided_tour_case() {
    local mode="$1"
    local cols="$2"
    local rows="$3"
    local ui_height="$4"
    local case_id="${mode}_${cols}x${rows}"
    local run_id="${RUN_ID}_${case_id}"
    local tour_log="${LOG_DIR}/guided_tour_${run_id}_${TIMESTAMP}.jsonl"
    local stdout_log="${LOG_DIR}/guided_tour_${case_id}_${TIMESTAMP}.log"
    local exit_after_ms=5000
    local timeout_s=12

    local key_script="sleep 0.6; printf ' '; sleep 0.3; printf 'n'; sleep 0.3; printf 'p'; sleep 0.3; printf ' '; sleep 0.3; printf '+'; sleep 0.3; printf '-'; sleep 0.3; printf '\\e'"

    local cmd=(
        cargo run -p ftui-demo-showcase --
        --tour
        --tour-speed=1.0
        --tour-start-step=1
        --exit-after-ms=${exit_after_ms}
    )

    local env_vars=(
        "COLUMNS=${cols}"
        "LINES=${rows}"
        "FTUI_TOUR_REPORT_PATH=${tour_log}"
        "FTUI_TOUR_RUN_ID=${run_id}"
        "FTUI_TOUR_SEED=${E2E_SEED:-0}"
        "FTUI_TOUR_CAPS_PROFILE=${TERM:-unknown}"
        "FTUI_DEMO_SCREEN_MODE=${mode}"
        "FTUI_DEMO_EXIT_AFTER_MS=${exit_after_ms}"
    )
    if [[ "$mode" == "inline" ]]; then
        env_vars+=("FTUI_DEMO_UI_HEIGHT=${ui_height}")
    fi

    jsonl_set_context "$mode" "$cols" "$rows" "${E2E_SEED:-0}"
    jsonl_case_step_start "guided_tour" "$case_id" "run" "mode=${mode} cols=${cols} rows=${rows}"

    local run_start_ms
    run_start_ms="$(e2e_now_ms)"
    local run_status="failed"
    local full_cmd="stty rows ${rows} cols ${cols} 2>/dev/null; (${key_script} > /dev/tty) & env ${env_vars[*]}"
    if command -v timeout >/dev/null 2>&1; then
        full_cmd+=" timeout ${timeout_s}"
    fi
    full_cmd+=" ${cmd[*]}"

    if eval "$full_cmd" >> "$stdout_log" 2>&1; then
        run_status="success"
    fi

    local run_duration_ms=$(( $(e2e_now_ms) - run_start_ms ))
    jsonl_case_step_end "guided_tour" "$case_id" "$run_status" "$run_duration_ms" "run" "mode=${mode}"
    if [[ "$run_status" != "success" ]]; then
        echo "FAIL: Guided tour run failed for ${case_id} (see $stdout_log)"
        jsonl_run_end "failed" "$run_duration_ms" 1
        exit 1
    fi

    jsonl_case_step_start "guided_tour" "$case_id" "validate" "log=${tour_log}"
    validate_tour_jsonl "$tour_log" "$case_id"
    jsonl_case_step_end "guided_tour" "$case_id" "success" 0 "validate" "log=${tour_log}"
}

echo "Running guided tour cases..."
run_start_ms="$(e2e_now_ms)"
jsonl_step_start "guided_tour"

for mode in "${TOUR_MODES[@]}"; do
    for size in "${TOUR_SIZES[@]}"; do
        read -r cols rows <<< "$size"
        run_guided_tour_case "$mode" "$cols" "$rows" "$INLINE_UI_HEIGHT"
    done
done

run_duration_ms=$(( $(e2e_now_ms) - run_start_ms ))
jsonl_step_end "guided_tour" "success" "$run_duration_ms"
jsonl_run_end "success" "$run_duration_ms" 0

echo "PASS: Guided tour logs captured at $LOG_DIR"

echo
exit 0

#!/usr/bin/env bash
# Visual FX Harness E2E (bd-1qyn3)
#
# Runs VFX harness in alt + inline modes at 80x24 and 120x40 for:
# - sampling (coverage via shared sampler; uses metaballs harness)
# - metaballs
# - plasma
#
# Produces per-frame JSONL with schema_version/mode/dims/seed/hash fields
# and fails fast on schema/hash mismatches.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"
TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
DEMO_BIN="$TARGET_DIR/release/ftui-demo-showcase"
# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"

E2E_VFX_SEED="${E2E_VFX_SEED:-42}"
E2E_VFX_FRAMES="${E2E_VFX_FRAMES:-6}"
E2E_VFX_FPS_FRAMES="${E2E_VFX_FPS_FRAMES:-12}"
E2E_VFX_TICK_MS="${E2E_VFX_TICK_MS:-16}"
E2E_INLINE_UI_HEIGHT="${E2E_INLINE_UI_HEIGHT:-8}"
E2E_VFX_INCLUDE_FPS="${E2E_VFX_INCLUDE_FPS:-1}"

if [[ -n "${E2E_VFX_LABELS:-}" ]]; then
    labels="${E2E_VFX_LABELS// /,}"
    IFS=',' read -r -a EFFECT_LABELS <<< "$labels"
else
    EFFECT_LABELS=("sampling" "metaballs" "plasma")
    if [[ "${E2E_VFX_INCLUDE_FPS:-1}" != "0" ]]; then
        EFFECT_LABELS+=("doom" "quake")
    fi
fi

if [[ -n "${E2E_VFX_MODES:-}" ]]; then
    modes="${E2E_VFX_MODES// /,}"
    IFS=',' read -r -a MODES <<< "$modes"
else
    MODES=("alt" "inline")
fi

if [[ -n "${E2E_VFX_SIZES:-}" ]]; then
    sizes="${E2E_VFX_SIZES// /,}"
    IFS=',' read -r -a SIZES <<< "$sizes"
else
    SIZES=("80x24" "120x40")
fi

if [[ -z "${E2E_PYTHON:-}" ]]; then
    echo "E2E_PYTHON is not set (python3/python not found)" >&2
    exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "WARN: jq not found; JSONL validation will rely on python only" >&2
fi

effect_for_label() {
    local label="$1"
    case "$label" in
        sampling) echo "metaballs" ;;
        doom) echo "doom-e1m1" ;;
        quake) echo "quake-e1m1" ;;
        *) echo "$label" ;;
    esac
}

parse_size() {
    local size="$1"
    local cols="${size%x*}"
    local rows="${size#*x}"
    printf '%s %s\n' "$cols" "$rows"
}

build_release() {
    local step_name="build_release"
    LOG_FILE="$E2E_LOG_DIR/${step_name}.log"
    log_test_start "$step_name"
    local start_ms
    start_ms="$(e2e_now_ms)"

    if cargo build -p ftui-demo-showcase --release >"$LOG_FILE" 2>&1; then
        local duration_ms=$(( $(e2e_now_ms) - start_ms ))
        if [[ ! -x "$DEMO_BIN" ]]; then
            log_test_fail "$step_name" "binary not found at $DEMO_BIN"
            record_result "$step_name" "failed" "$duration_ms" "$LOG_FILE" "binary not found at $DEMO_BIN"
            finalize_summary "$E2E_RESULTS_DIR/summary.json"
            exit 1
        fi
        log_test_pass "$step_name"
        record_result "$step_name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    fi

    local duration_ms=$(( $(e2e_now_ms) - start_ms ))
    log_test_fail "$step_name" "build failed"
    record_result "$step_name" "failed" "$duration_ms" "$LOG_FILE" "build failed"
    finalize_summary "$E2E_RESULTS_DIR/summary.json"
    exit 1
}

validate_and_enrich() {
    local raw_jsonl="$1"
    local out_jsonl="$2"
    local label="$3"
    local effect_raw_expected="$4"
    local mode="$5"
    local cols="$6"
    local rows="$7"
    local tick_ms="$8"
    local frames_expected="$9"
    local seed_expected="${10}"
    local case_id="${11}"
    local golden_file="${12:-}"

    "$E2E_PYTHON" - "$raw_jsonl" "$out_jsonl" "$label" "$effect_raw_expected" "$mode" \
        "$cols" "$rows" "$tick_ms" "$frames_expected" "$seed_expected" "$case_id" \
        "${golden_file:-}" <<'PY'
import json
import os
import sys
from datetime import datetime

raw_path = sys.argv[1]
out_path = sys.argv[2]
label = sys.argv[3]
effect_expected = sys.argv[4]
mode = sys.argv[5]
cols = int(sys.argv[6])
rows = int(sys.argv[7])
tick_ms = int(sys.argv[8])
frames_expected = int(sys.argv[9])
seed_expected = int(sys.argv[10])
case_id = sys.argv[11]
golden_path = sys.argv[12] if len(sys.argv) > 12 and sys.argv[12] else ""

schema_version = "vfx-jsonl-v1"
errors = []
frames = []
inputs = []
start = None

if not os.path.exists(raw_path):
    errors.append(f"raw JSONL missing: {raw_path}")
else:
    with open(raw_path, "r", encoding="utf-8") as handle:
        for idx, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except Exception as exc:
                errors.append(f"line {idx}: json parse error: {exc}")
                continue
            event = obj.get("event")
            if event == "vfx_harness_start":
                start = obj
                continue
            if event == "vfx_input":
                inputs.append(obj)
                continue
            if event == "vfx_frame":
                frames.append(obj)

required = ["timestamp", "run_id", "hash_key", "effect", "frame_idx", "hash", "time", "cols", "rows", "tick_ms", "seed"]
for idx, frame in enumerate(frames, 1):
    missing = [k for k in required if k not in frame]
    if missing:
        errors.append(f"frame {idx} missing keys {missing}")
        continue
    if frame.get("effect") != effect_expected:
        errors.append(f"frame {idx} effect mismatch: {frame.get('effect')} expected {effect_expected}")
    if frame.get("cols") != cols or frame.get("rows") != rows:
        errors.append(f"frame {idx} dims mismatch: {frame.get('cols')}x{frame.get('rows')} expected {cols}x{rows}")
    if frame.get("tick_ms") != tick_ms:
        errors.append(f"frame {idx} tick_ms mismatch: {frame.get('tick_ms')} expected {tick_ms}")
    if frame.get("seed") != seed_expected:
        errors.append(f"frame {idx} seed mismatch: {frame.get('seed')} expected {seed_expected}")

input_markers = {}
input_required = ["timestamp", "run_id", "effect", "frame_idx", "action", "cols", "rows", "tick_ms", "seed"]
for idx, entry in enumerate(inputs, 1):
    missing = [k for k in input_required if k not in entry]
    if missing:
        errors.append(f"input {idx} missing keys {missing}")
        continue
    if entry.get("effect") != effect_expected:
        errors.append(f"input {idx} effect mismatch: {entry.get('effect')} expected {effect_expected}")
    if entry.get("cols") != cols or entry.get("rows") != rows:
        errors.append(f"input {idx} dims mismatch: {entry.get('cols')}x{entry.get('rows')} expected {cols}x{rows}")
    if entry.get("tick_ms") != tick_ms:
        errors.append(f"input {idx} tick_ms mismatch: {entry.get('tick_ms')} expected {tick_ms}")
    if entry.get("seed") != seed_expected:
        errors.append(f"input {idx} seed mismatch: {entry.get('seed')} expected {seed_expected}")
    frame_idx = entry.get("frame_idx")
    action = entry.get("action")
    if not isinstance(frame_idx, int):
        errors.append(f"input {idx} frame_idx invalid: {frame_idx}")
        continue
    if not isinstance(action, str):
        errors.append(f"input {idx} action invalid: {action}")
        continue
    input_markers.setdefault(frame_idx, []).append(action)

if not frames:
    errors.append("no vfx_frame entries found")

if frames_expected > 0 and len(frames) != frames_expected:
    errors.append(f"frame count mismatch: got {len(frames)} expected {frames_expected}")

fps_case = label in ("doom", "quake")
if fps_case and not inputs:
    errors.append(f"no vfx_input entries found for fps effect {label}")

run_id = None
hash_key = None
if frames:
    run_id = frames[0].get("run_id")
    hash_key = frames[0].get("hash_key")
if start and not run_id:
    run_id = start.get("run_id")
if not run_id:
    run_id = case_id
if not hash_key:
    hash_key = ""

os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)

with open(out_path, "w", encoding="utf-8") as out:
    timestamp = None
    if start:
        timestamp = start.get("timestamp")
    if not timestamp and frames:
        timestamp = frames[0].get("timestamp")
    if not timestamp:
        timestamp = datetime.utcnow().isoformat() + "Z"
    fps_estimate = round(1000.0 / tick_ms, 3) if tick_ms > 0 else 0.0

    start_record = {
        "schema_version": schema_version,
        "type": "vfx_start",
        "timestamp": timestamp,
        "run_id": run_id,
        "case_id": case_id,
        "effect": label,
        "effect_raw": effect_expected,
        "mode": mode,
        "cols": cols,
        "rows": rows,
        "tick_ms": tick_ms,
        "seed": seed_expected,
        "hash_key": hash_key,
        "frames": len(frames),
        "fps_estimate": fps_estimate,
    }
    out.write(json.dumps(start_record, separators=(",", ":")) + "\n")

    for frame in frames:
        frame_idx = frame.get("frame_idx")
        record = {
            "schema_version": schema_version,
            "type": "vfx_frame",
            "timestamp": frame.get("timestamp"),
            "run_id": run_id,
            "case_id": case_id,
            "effect": label,
            "effect_raw": frame.get("effect"),
            "frame_idx": frame_idx,
            "hash": frame.get("hash"),
            "time": frame.get("time"),
            "action": "frame",
            "mode": mode,
            "cols": frame.get("cols"),
            "rows": frame.get("rows"),
            "tick_ms": frame.get("tick_ms"),
            "seed": frame.get("seed"),
            "hash_key": frame.get("hash_key"),
            "fps_estimate": fps_estimate,
            "input_markers": input_markers.get(frame_idx, []),
        }
        out.write(json.dumps(record, separators=(",", ":")) + "\n")

    for entry in inputs:
        record = {
            "schema_version": schema_version,
            "type": "vfx_input",
            "timestamp": entry.get("timestamp"),
            "run_id": run_id,
            "case_id": case_id,
            "effect": label,
            "effect_raw": entry.get("effect"),
            "frame_idx": entry.get("frame_idx"),
            "action": entry.get("action"),
            "mode": mode,
            "cols": entry.get("cols"),
            "rows": entry.get("rows"),
            "tick_ms": entry.get("tick_ms"),
            "seed": entry.get("seed"),
            "hash_key": entry.get("hash_key"),
            "fps_estimate": fps_estimate,
        }
        out.write(json.dumps(record, separators=(",", ":")) + "\n")

    if golden_path and os.path.exists(golden_path):
        expected = []
        with open(golden_path, "r", encoding="utf-8") as gf:
            for line in gf:
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                expected.append(line)
        actual = [f"{frame['frame_idx']:03}:{frame['hash']:016x}" for frame in frames if isinstance(frame.get("frame_idx"), int) and isinstance(frame.get("hash"), int)]
        if not expected:
            errors.append(f"golden file empty: {golden_path}")
        elif len(actual) < len(expected):
            errors.append(f"golden frame count mismatch: expected {len(expected)} got {len(actual)}")
        else:
            for idx, exp in enumerate(expected):
                if exp != actual[idx]:
                    errors.append(f"golden mismatch at frame {idx+1}: expected {exp} got {actual[idx]}")
                    break

    if errors:
        for msg in errors:
            err_record = {
                "schema_version": schema_version,
                "type": "error",
                "timestamp": timestamp,
                "run_id": run_id,
                "case_id": case_id,
                "effect": label,
                "mode": mode,
                "message": msg,
            }
            out.write(json.dumps(err_record, separators=(",", ":")) + "\n")

if errors:
    for msg in errors:
        print(f"ERROR: {msg}", file=sys.stderr)
    sys.exit(1)
PY
}

run_vfx_case() {
    local label="$1"
    local mode="$2"
    local cols="$3"
    local rows="$4"

    local effect
    effect="$(effect_for_label "$label")"
    local case_id="${label}_${mode}_${cols}x${rows}"
    local raw_jsonl="$E2E_LOG_DIR/${case_id}_raw.jsonl"
    local out_jsonl="$E2E_LOG_DIR/${case_id}.jsonl"
    local run_id="${E2E_RUN_ID}_${case_id}"
    local name="vfx_${case_id}"
    LOG_FILE="$E2E_LOG_DIR/${case_id}.log"

    log_test_start "$name"
    local start_ms
    start_ms="$(e2e_now_ms)"

    local frames="$E2E_VFX_FRAMES"
    if [[ "$label" == "doom" || "$label" == "quake" ]]; then
        frames="$E2E_VFX_FPS_FRAMES"
    fi

    local ui_arg="--ui-height=${E2E_INLINE_UI_HEIGHT}"
    if [[ "$mode" == "alt" ]]; then
        ui_arg="--ui-height=${E2E_INLINE_UI_HEIGHT}"
    fi

    if ! "$DEMO_BIN" \
        --screen-mode="$mode" \
        "$ui_arg" \
        --vfx-harness \
        --vfx-effect="$effect" \
        --vfx-tick-ms="$E2E_VFX_TICK_MS" \
        --vfx-frames="$frames" \
        --vfx-cols="$cols" \
        --vfx-rows="$rows" \
        --vfx-seed="$E2E_SEED" \
        --vfx-jsonl="$raw_jsonl" \
        --vfx-run-id="$run_id" \
        >"$LOG_FILE" 2>&1; then
        local duration_ms=$(( $(e2e_now_ms) - start_ms ))
        log_test_fail "$name" "harness failed"
        record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "harness failed"
        finalize_summary "$E2E_RESULTS_DIR/summary.json"
        exit 1
    fi

    local golden_file=""
    if [[ "$mode" == "alt" && "$label" != "sampling" ]]; then
        golden_file="$PROJECT_ROOT/crates/ftui-demo-showcase/tests/golden/vfx_${effect}_${cols}x${rows}_${E2E_VFX_TICK_MS}ms_seed${E2E_SEED}.checksums"
        if [[ ! -f "$golden_file" ]]; then
            golden_file=""
        fi
    fi

    if ! validate_and_enrich "$raw_jsonl" "$out_jsonl" "$label" "$effect" "$mode" "$cols" "$rows" \
        "$E2E_VFX_TICK_MS" "$frames" "$E2E_SEED" "$case_id" "$golden_file"; then
        local duration_ms=$(( $(e2e_now_ms) - start_ms ))
        log_test_fail "$name" "validation failed"
        record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "validation failed"
        finalize_summary "$E2E_RESULTS_DIR/summary.json"
        exit 1
    fi

    jsonl_assert "artifact_vfx_raw_jsonl" "pass" "vfx_raw_jsonl=$raw_jsonl"
    jsonl_assert "artifact_vfx_jsonl" "pass" "vfx_jsonl=$out_jsonl"

    local duration_ms=$(( $(e2e_now_ms) - start_ms ))
    log_test_pass "$name"
    record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
}

main() {
    e2e_fixture_init "vfx_harness" "$E2E_VFX_SEED"

    E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui-vfx-e2e-${E2E_RUN_ID}}"
    E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
    E2E_JSONL_FILE="${E2E_JSONL_FILE:-$E2E_LOG_DIR/visual_fx_e2e.jsonl}"
    E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
    E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"
    export E2E_LOG_DIR E2E_RESULTS_DIR E2E_JSONL_FILE E2E_RUN_CMD E2E_RUN_START_MS

    mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
    jsonl_init
    jsonl_assert "artifact_log_dir" "pass" "log_dir=$E2E_LOG_DIR"

    build_release

    for label in "${EFFECT_LABELS[@]}"; do
        for mode in "${MODES[@]}"; do
            for size in "${SIZES[@]}"; do
                read -r cols rows < <(parse_size "$size")
                run_vfx_case "$label" "$mode" "$cols" "$rows"
            done
        done
    done

    finalize_summary "$E2E_RESULTS_DIR/summary.json"
}

main "$@"

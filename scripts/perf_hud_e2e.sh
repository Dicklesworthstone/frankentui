#!/usr/bin/env bash
# Performance HUD + Degradation Tiers E2E (bd-3bzos)
#
# Validates:
# - Performance HUD screen renders in alt + inline modes (80x24, 120x40).
# - Degradation tier labels match expected FPS tier for deterministic tick_ms.
# - JSONL logs include tier info, budget metrics, hashes, dims, mode, seed.
# - Optional HUD overlay toggle emits hud_toggle JSONL event.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

if ! declare -f e2e_timestamp >/dev/null 2>&1; then
    e2e_timestamp() { date -Iseconds; }
fi
if ! declare -f e2e_log_stamp >/dev/null 2>&1; then
    e2e_log_stamp() { date +%Y%m%d_%H%M%S; }
fi

if [[ -z "${E2E_PYTHON:-}" ]]; then
    echo "E2E_PYTHON is not set (python3/python not found)" >&2
    exit 1
fi

VERBOSE=false
QUICK=false
for arg in "$@"; do
    case "$arg" in
        --verbose|-v) VERBOSE=true ;;
        --quick|-q) QUICK=true ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--quick]"
            exit 0
            ;;
    esac
done

e2e_fixture_init "perf_hud"
TIMESTAMP="$(e2e_log_stamp)"
LOG_DIR="${LOG_DIR:-/tmp/ftui_perf_hud_e2e_${E2E_RUN_ID}_${TIMESTAMP}}"
E2E_LOG_DIR="$LOG_DIR"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$LOG_DIR/results}"
E2E_JSONL_FILE="$LOG_DIR/perf_hud_e2e.jsonl"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"
export E2E_LOG_DIR E2E_RESULTS_DIR E2E_JSONL_FILE E2E_RUN_CMD E2E_RUN_START_MS
mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
jsonl_init

TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
DEMO_BIN="$TARGET_DIR/debug/ftui-demo-showcase"
export CARGO_TARGET_DIR="$TARGET_DIR"

PTY_CANONICALIZE_BIN="${PTY_CANONICALIZE_BIN:-$TARGET_DIR/debug/pty_canonicalize}"
if [[ ! -x "$PTY_CANONICALIZE_BIN" && -x "$TARGET_DIR/release/pty_canonicalize" ]]; then
    PTY_CANONICALIZE_BIN="$TARGET_DIR/release/pty_canonicalize"
fi

if ! $QUICK; then
    log_info "Building ftui-demo-showcase (debug)..."
    if $VERBOSE; then
        cargo build -p ftui-demo-showcase 2>&1 | tee "$LOG_DIR/build.log"
    else
        cargo build -p ftui-demo-showcase > "$LOG_DIR/build.log" 2>&1
    fi
fi

if [[ ! -x "$PTY_CANONICALIZE_BIN" ]]; then
    log_info "Building pty_canonicalize helper..."
    if $VERBOSE; then
        cargo build -p ftui-pty --bin pty_canonicalize 2>&1 | tee "$LOG_DIR/pty_canonicalize_build.log"
    else
        cargo build -p ftui-pty --bin pty_canonicalize > "$LOG_DIR/pty_canonicalize_build.log" 2>&1
    fi
fi

if [[ -x "$PTY_CANONICALIZE_BIN" ]]; then
    export PTY_CANONICALIZE=1
    export PTY_CANONICALIZE_BIN
else
    log_warn "pty_canonicalize not available; falling back to raw PTY output"
    export PTY_CANONICALIZE=0
    unset PTY_CANONICALIZE_BIN
fi

if [[ ! -x "$DEMO_BIN" ]]; then
    log_error "Demo binary not found at $DEMO_BIN"
    jsonl_run_end "failed" "$(( $(e2e_now_ms) - E2E_RUN_START_MS ))" 1
    exit 1
fi

# The tier labels live on the Performance HUD screen. A hardcoded 30 had
# drifted onto Theme Studio as screens were added.
PERF_HUD_SCREEN="$(e2e_demo_screen "$DEMO_BIN" performance_hud)"

MODES=("alt" "inline")
SIZES=("80x24" "120x40")
INLINE_UI_HEIGHT="${INLINE_UI_HEIGHT:-12}"
EXIT_AFTER_TICKS="${EXIT_AFTER_TICKS:-10}"

# tier_label:tick_ms:views_per_tick:expected_text
TIERS=(
    "full:16:1.00:Full Fidelity"
    "reduced:40:1.00:Reduced (no FX)"
    "minimal:100:1.00:Minimal"
    "safety:250:0.10:SAFETY MODE"
)

parse_size() {
    local size="$1"
    local cols="${size%x*}"
    local rows="${size#*x}"
    printf '%s %s\n' "$cols" "$rows"
}

expected_fps() {
    local tick_ms="$1"
    if [[ "$tick_ms" -le 0 ]]; then
        echo "0"
        return 0
    fi
    "$E2E_PYTHON" - <<PY "$tick_ms"
import sys
ms = float(sys.argv[1])
print(f"{1000.0 / ms:.2f}")
PY
}

strip_and_find() {
    local raw_file="$1"
    local canonical_file="$2"
    local expected="$3"
    local want_toggle="$4"
    "$E2E_PYTHON" - "$raw_file" "$canonical_file" "$expected" "$want_toggle" <<'PY'
import os
import re
import sys

raw_path = sys.argv[1]
canonical_path = sys.argv[2]
expected = sys.argv[3]
want_toggle = sys.argv[4] == "1"

def read_text(path):
    if not path:
        return None
    try:
        return open(path, "rb").read().decode("utf-8", errors="ignore")
    except Exception:
        return None

raw_data = read_text(raw_path) or ""
canonical_data = None
if canonical_path and os.path.exists(canonical_path):
    canonical_data = read_text(canonical_path)
if raw_data is None and canonical_data is None:
    print("0 0")
    sys.exit(0)

# Strip ANSI CSI/OSC sequences
ansi_csi = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")
ansi_osc = re.compile(r"\x1b\][^\x07]*\x07")
def normalize(text):
    text = ansi_csi.sub("", text)
    text = ansi_osc.sub("", text)
    return text

found_label = 0
if canonical_data is not None:
    if expected in normalize(canonical_data):
        found_label = 1
if found_label == 0 and raw_data:
    if expected in normalize(raw_data):
        found_label = 1
found_toggle = 0
if want_toggle:
    found_toggle = 1 if '"event":"tick_stats"' in raw_data else 0

print(f"{found_label} {found_toggle}")
PY
}

run_case() {
    local mode="$1"
    local cols="$2"
    local rows="$3"
    local tier_label="$4"
    local tick_ms="$5"
    local views_per_tick="$6"
    local expected_text="$7"
    local send_toggle="$8"

    local case_id="perf_hud_${mode}_${cols}x${rows}_${tier_label}"
    local out_pty="$LOG_DIR/${case_id}.pty"
    local canonical_file="$LOG_DIR/${case_id}.canonical.txt"
    local run_log="$LOG_DIR/${case_id}.log"

    jsonl_set_context "$mode" "$cols" "$rows" "${E2E_SEED:-0}"
    jsonl_case_step_start "$case_id" "run" "launch" "tick_ms=${tick_ms} tier=${tier_label} vpt=${views_per_tick}"

    local start_ms
    start_ms="$(e2e_now_ms)"

    local exit_code=0
    local ui_height="$INLINE_UI_HEIGHT"
    local exit_after_ticks="$EXIT_AFTER_TICKS"
    if [[ -n "$send_toggle" ]]; then
        exit_after_ticks=60
    fi
    if [[ "$mode" == "inline" ]]; then
        ui_height="$rows"
    fi

    if FTUI_DEMO_DETERMINISTIC=1 \
        FTUI_DEMO_SEED="${E2E_SEED:-0}" \
        FTUI_DEMO_RUN_ID="${E2E_RUN_ID}_${case_id}" \
        FTUI_DEMO_SCREEN="$PERF_HUD_SCREEN" \
        FTUI_DEMO_SCREEN_MODE="$mode" \
        FTUI_DEMO_UI_HEIGHT="$ui_height" \
        FTUI_DEMO_TICK_MS="$tick_ms" \
        FTUI_DEMO_EXIT_AFTER_TICKS="$exit_after_ticks" \
        FTUI_DEMO_PERF_HUD_VIEWS_PER_TICK="$views_per_tick" \
        FTUI_PERF_HUD_JSONL=1 \
        PTY_COLS="$cols" \
        PTY_ROWS="$rows" \
        PTY_TIMEOUT=8 \
        PTY_SEND="${send_toggle}" \
        PTY_SEND_DELAY_MS=200 \
        PTY_TEST_NAME="$case_id" \
        pty_run "$out_pty" "$DEMO_BIN" > "$run_log" 2>&1; then
        exit_code=0
    else
        exit_code=$?
    fi

    local end_ms
    end_ms="$(e2e_now_ms)"
    local duration_ms=$((end_ms - start_ms))

    export PTY_CANONICAL_FILE=""
    if pty_canonicalize_file "$out_pty" "$canonical_file" "$cols" "$rows"; then
        export PTY_CANONICAL_FILE="$canonical_file"
        jsonl_artifact "pty_canonical" "$canonical_file" "present"
    fi
    pty_record_metadata "$out_pty" "$exit_code" "$cols" "$rows"
    jsonl_artifact "pty_output" "$out_pty" "present"

    local hash_key
    hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "${E2E_SEED:-0}")"
    local hash
    hash="$(sha256_file "$out_pty" 2>/dev/null || true)"
    if [[ -z "$hash" ]]; then
        hash="missing"
    fi

    local found_label found_toggle
    read -r found_label found_toggle < <(strip_and_find "$out_pty" "$PTY_CANONICAL_FILE" "$expected_text" "$([[ -n "$send_toggle" ]] && echo 1 || echo 0)")

    local status="pass"
    if [[ "$exit_code" -ne 0 ]]; then
        status="fail"
    fi
    if [[ "$found_label" -ne 1 ]]; then
        status="fail"
    fi
    if [[ -n "$send_toggle" && "$found_toggle" -ne 1 ]]; then
        status="fail"
    fi

    local fps_est
    fps_est="$(expected_fps "$tick_ms")"
    local budget_ms="16.67"

    jsonl_assert "perf_hud_${case_id}" "$status" \
        "tier=${tier_label} expected=\"${expected_text}\" found=${found_label} toggle=${found_toggle} tick_ms=${tick_ms} vpt=${views_per_tick} fps_est=${fps_est} budget_ms=${budget_ms} mode=${mode} cols=${cols} rows=${rows} seed=${E2E_SEED:-0} hash_key=${hash_key} hash=${hash} exit=${exit_code} duration_ms=${duration_ms}"

    jsonl_case_step_end "$case_id" "run" "$status" "$duration_ms" "complete" "tier=${tier_label}"

    if [[ "$status" != "pass" ]]; then
        log_warn "Case ${case_id} failed; see ${run_log}"
        return 1
    fi
    return 0
}

failures=0

for mode in "${MODES[@]}"; do
    for size in "${SIZES[@]}"; do
        read -r cols rows < <(parse_size "$size")
        prev_tier=""
        for tier in "${TIERS[@]}"; do
            IFS=':' read -r tier_label tick_ms views_per_tick expected_text <<< "$tier"
            local_toggle=""
            # Toggle HUD overlay on the very first case to capture hud_toggle JSONL.
            if [[ -z "$prev_tier" && "$mode" == "alt" && "$size" == "80x24" ]]; then
                local_toggle='\x10'
            fi
            if ! run_case "$mode" "$cols" "$rows" "$tier_label" "$tick_ms" "$views_per_tick" "$expected_text" "$local_toggle"; then
                failures=$((failures + 1))
            fi
            prev_tier="$tier_label"
        done
    done
done

if [[ "$failures" -gt 0 ]]; then
    jsonl_run_end "failed" "$(( $(e2e_now_ms) - E2E_RUN_START_MS ))" "$failures"
    exit 1
fi

jsonl_run_end "passed" "$(( $(e2e_now_ms) - E2E_RUN_START_MS ))" 0
exit 0

#!/bin/bash
set -euo pipefail

# E2E: Doom/Quake braille VFX harness (input + render hashes).
# bd-vmuax
#
# Coverage:
# - Effects: doom-e1m1, quake-e1m1
# - Modes: alt + inline
# - Sizes: 80x24, 120x40
# - Deterministic seeds/time with stable hashes + input markers
#
# Delegates to scripts/demo_visual_fx_e2e.sh with effect filter.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"

export E2E_DETERMINISTIC="${E2E_DETERMINISTIC:-1}"
export E2E_TIME_STEP_MS="${E2E_TIME_STEP_MS:-100}"

VFX_SEED="${VFX_SEED:-${E2E_SEED:-0}}"
export E2E_SEED="${E2E_SEED:-$VFX_SEED}"

e2e_fixture_init "vfx_fps" "$E2E_SEED" "$E2E_TIME_STEP_MS"

BASE_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_logs}"
E2E_LOG_DIR="${BASE_LOG_DIR}/visual_fx_fps"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
LOG_FILE="${LOG_FILE:-$E2E_LOG_DIR/visual_fx_fps.log}"
E2E_JSONL_FILE="${E2E_JSONL_FILE:-$BASE_LOG_DIR/e2e.jsonl}"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
export E2E_LOG_DIR E2E_RESULTS_DIR LOG_FILE E2E_JSONL_FILE E2E_RUN_CMD
export E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"

mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
jsonl_init
jsonl_assert "artifact_log_dir" "pass" "log_dir=$E2E_LOG_DIR"

export E2E_VFX_LABELS="doom,quake"
export E2E_VFX_INCLUDE_FPS=1
export E2E_VFX_FPS_FRAMES="${E2E_VFX_FPS_FRAMES:-12}"
export E2E_VFX_TICK_MS="${E2E_VFX_TICK_MS:-16}"
export E2E_VFX_SEED="${E2E_VFX_SEED:-$E2E_SEED}"
export E2E_VFX_MODES="${E2E_VFX_MODES:-alt,inline}"
export E2E_VFX_SIZES="${E2E_VFX_SIZES:-80x24,120x40}"

"$PROJECT_ROOT/scripts/demo_visual_fx_e2e.sh" >"$LOG_FILE" 2>&1

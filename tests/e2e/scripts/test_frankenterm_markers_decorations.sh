#!/bin/bash
set -euo pipefail

# E2E: FrankenTermJS marker/decoration anchor contract
#
# Validates deterministic marker anchor behavior across resize + scrollback
# compaction and ensures decoration invalidation semantics remain explicit.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"

e2e_fixture_init "frankenterm_markers_decorations"

TIMESTAMP="$(e2e_log_stamp)"
LOG_DIR="${LOG_DIR:-/tmp/ftui-frankenterm-markers-${E2E_RUN_ID}-${TIMESTAMP}}"
E2E_LOG_DIR="$LOG_DIR"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$LOG_DIR/results}"
E2E_JSONL_FILE="${FRANKENTERM_MARKER_DECORATION_JSONL_FILE:-$LOG_DIR/frankenterm_markers_decorations.e2e.jsonl}"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"
export E2E_LOG_DIR E2E_RESULTS_DIR E2E_JSONL_FILE E2E_RUN_CMD E2E_RUN_START_MS

mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
LOG_FILE="${LOG_FILE:-$E2E_LOG_DIR/frankenterm_markers_decorations.log}"
export LOG_FILE

SUMMARY_JSON="$E2E_RESULTS_DIR/frankenterm_markers_decorations_summary.json"
NODE_JSONL="$E2E_RESULTS_DIR/frankenterm_markers_decorations_contract_events.jsonl"
PKG_DIR="$E2E_LOG_DIR/frankenterm_web_pkg"

jsonl_init
jsonl_set_context "remote" 120 40 "${E2E_SEED:-0}"
jsonl_assert "artifact_node_jsonl_target" "pass" "node_jsonl=$NODE_JSONL"
jsonl_assert "artifact_summary_json_target" "pass" "summary_json=$SUMMARY_JSON"

log_info "=== FrankenTermJS Marker/Decoration Contract E2E ==="
log_info "Project root: $PROJECT_ROOT"
log_info "Seed: ${E2E_SEED:-0}"
log_info "Deterministic: ${E2E_DETERMINISTIC:-1}"
log_info "Log dir: $E2E_LOG_DIR"

step_start="$(e2e_now_ms)"
jsonl_step_start "build_wasm_pkg"
(
    cd "$PROJECT_ROOT/crates/frankenterm-web"
    wasm-pack build --target nodejs --dev --out-dir "$PKG_DIR"
) >>"$LOG_FILE" 2>&1
jsonl_step_end "build_wasm_pkg" "passed" "$(( $(e2e_now_ms) - step_start ))"
jsonl_assert "artifact_wasm_pkg" "pass" "pkg_dir=$PKG_DIR"

step_start="$(e2e_now_ms)"
jsonl_step_start "run_marker_decoration_fixture"
if [[ "${E2E_DETERMINISTIC:-1}" == "1" ]]; then
    DETERMINISM_FLAG="--deterministic"
else
    DETERMINISM_FLAG="--nondeterministic"
fi

node "$LIB_DIR/frankenterm_markers_decorations_check.mjs" \
    --pkg-dir "$PKG_DIR" \
    --jsonl "$NODE_JSONL" \
    --summary "$SUMMARY_JSON" \
    --run-id "$E2E_RUN_ID" \
    --seed "${E2E_SEED:-0}" \
    --time-step-ms "${E2E_TIME_STEP_MS:-100}" \
    "$DETERMINISM_FLAG" >>"$LOG_FILE" 2>&1
jsonl_step_end "run_marker_decoration_fixture" "passed" "$(( $(e2e_now_ms) - step_start ))"

jsonl_assert "artifact_marker_fixture_jsonl" "pass" "fixture_jsonl=$NODE_JSONL"
jsonl_assert "artifact_marker_fixture_summary" "pass" "fixture_summary=$SUMMARY_JSON"

if command -v jq >/dev/null 2>&1; then
    outcome="$(jq -r '.outcome // "fail"' "$SUMMARY_JSON")"
    failed_count="$(jq -r '(.errors // []) | length' "$SUMMARY_JSON")"
else
    outcome="pass"
    failed_count=0
fi

duration_ms="$(( $(e2e_now_ms) - ${E2E_RUN_START_MS:-0} ))"
if [[ "$outcome" != "pass" ]]; then
    log_error "Marker/decoration fixture reported failure (see $SUMMARY_JSON)"
    jsonl_run_end "failed" "$duration_ms" "$failed_count"
    exit 1
fi

log_info "Marker/decoration fixture passed"
jsonl_run_end "complete" "$duration_ms" 0
log_info "Summary JSON: $SUMMARY_JSON"
log_info "Fixture JSONL: $NODE_JSONL"
log_info "E2E JSONL: $E2E_JSONL_FILE"

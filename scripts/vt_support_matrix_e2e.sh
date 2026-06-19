#!/usr/bin/env bash
set -euo pipefail

RUN_ROOT="${1:-/tmp/frankentui_vt_support_matrix}"
META_DIR="${RUN_ROOT}/meta"
LOG_DIR="${RUN_ROOT}/logs"
mkdir -p "${META_DIR}" "${LOG_DIR}"

export FTUI_VT_CONFORMANCE_JSONL="${META_DIR}/vt_support_matrix_results.jsonl"
export FTUI_VT_CONFORMANCE_SUMMARY_JSON="${META_DIR}/vt_support_matrix_summary.json"

echo "[vt_support_matrix_e2e] run_root=${RUN_ROOT}"
echo "[vt_support_matrix_e2e] jsonl=${FTUI_VT_CONFORMANCE_JSONL}"
echo "[vt_support_matrix_e2e] summary=${FTUI_VT_CONFORMANCE_SUMMARY_JSON}"

# Offload to the remote build fleet when available (local dev); fall back to a
# bare local build in CI, where `rch` is not installed.
if command -v rch >/dev/null 2>&1; then
  rch exec -- cargo test -p ftui-pty --test vt_support_matrix_runner -- --nocapture \
    | tee "${LOG_DIR}/vt_support_matrix_test.log"
else
  cargo test -p ftui-pty --test vt_support_matrix_runner -- --nocapture \
    | tee "${LOG_DIR}/vt_support_matrix_test.log"
fi

echo "[vt_support_matrix_e2e] complete"

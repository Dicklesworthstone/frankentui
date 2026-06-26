#!/usr/bin/env bash
#
# End-to-end alien-uplift validation pipeline (bd-3bxhj.10.11).
#
# Runs the `doctor_frankentui alien-uplift` command, which executes the
# cross-kernel alien-uplift checks (shadow-run, concolic/metamorphic discovery,
# CEGIS synthesis fallback, controller-interference guards, posterior/expected-
# loss decision traces, formal-guarantee checks, and quality-bar validation) and
# materializes a JSONL evidence ledger with artifact-graph IDs and replay
# commands. This wrapper validates the artifact contract and fails closed,
# making it suitable as a release-candidate promotion gate.
#
# Usage:
#   ./scripts/doctor_frankentui_alien_uplift_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/alien_uplift/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[alien-uplift-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

echo "[alien-uplift-e2e] run-root: ${RUN_ROOT}"
echo "[alien-uplift-e2e] building + running doctor_frankentui alien-uplift ..."

# The command exits non-zero if the pipeline gate fails; capture so we can still
# inspect the materialized artifacts for triage.
PIPELINE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json alien-uplift \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || PIPELINE_EXIT=$?

fail() {
  echo "[alien-uplift-e2e] FAIL: $1" >&2
  exit 1
}

# ── Artifact existence ──────────────────────────────────────────────────────
[ -s "${LEDGER}" ]   || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ]  || fail "pipeline summary missing: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing: ${MANIFEST}"

# ── Ledger contract: every line carries artifact-graph IDs + a replay command ─
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.check_id        | type == "string" and (. | length) > 0) and
    (.kernel          | type == "string" and (. | length) > 0) and
    (.equation_id     | type == "string" and (. | length) > 0) and
    (.policy_id       | type == "string" and (. | length) > 0) and
    (.budget_state    | type == "object") and
    (.posterior_hash  | type == "string" and (. | length) > 0) and
    (.guarantee_status| type == "string" and (. | length) > 0) and
    (.proof_hash      | type == "string" and (. | length) > 0) and
    (.witness_hash    | type == "string" and (. | length) > 0) and
    (.replay_command  | type == "string" and (contains(.check_id)))
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the record contract"
done <"${LEDGER}"
echo "[alien-uplift-e2e] ledger lines validated: ${LINE_NO}"

# ── Summary gate: green-path + red-path completeness ────────────────────────
jq -e '.all_checks_passed == true'  "${SUMMARY}" >/dev/null || fail "all_checks_passed != true"
jq -e '.red_path_complete == true'  "${SUMMARY}" >/dev/null || fail "red_path_complete != true"
jq -e '.pipeline_passed == true'    "${SUMMARY}" >/dev/null || fail "pipeline_passed != true"
jq -e '.failed == 0'                "${SUMMARY}" >/dev/null || fail "summary reports failed checks"

# Every kernel must carry at least one passing red-path fixture (AC#2).
jq -e '.red_path_coverage | all(.red_path_checks >= 1 and .passed == true)' \
  "${SUMMARY}" >/dev/null || fail "a kernel is missing a passing red-path fixture"

KERNELS="$(jq -r '.kernels_covered' "${SUMMARY}")"
RED_PATHS="$(jq -r '.red_path_checks' "${SUMMARY}")"
TOTAL="$(jq -r '.total_checks' "${SUMMARY}")"
echo "[alien-uplift-e2e] kernels=${KERNELS} total_checks=${TOTAL} red_path_checks=${RED_PATHS}"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
echo "[alien-uplift-e2e] manifest integrity verified"

# The CLI gate itself must have passed on the green path.
[ "${PIPELINE_EXIT}" -eq 0 ] || fail "alien-uplift command exited ${PIPELINE_EXIT}"

echo "[alien-uplift-e2e] PASS — alien-uplift validation pipeline gate green"
exit 0

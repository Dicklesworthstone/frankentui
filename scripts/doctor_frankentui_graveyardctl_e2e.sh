#!/usr/bin/env bash
#
# End-to-end graveyardctl executable-workflow gate (bd-3bxhj.10.32).
#
# Runs the `doctor_frankentui graveyardctl` command, which makes alien-graveyard
# planning executable as a deterministic route -> rank -> contract -> verify
# pipeline (index/score/pick/scaffold/verify command wrappers). The default
# (all-stage) view applies the active-entry verify CI gate: every actively-
# implemented alien entry must pass the recommendation-contract linter and the
# failure-mode risk gate, otherwise the command fails closed.
#
# It materializes a JSONL evidence ledger whose every line carries entry_id,
# verify_result, missing_artifacts, a remediation command set, run_id, the
# stage, and a reproduction command. This wrapper validates the artifact
# contract, determinism, and the green gate, making it suitable as a release
# governance gate.
#
# Usage:
#   ./scripts/doctor_frankentui_graveyardctl_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/graveyardctl/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[graveyardctl-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

echo "[graveyardctl-e2e] run-root: ${RUN_ROOT}"
echo "[graveyardctl-e2e] building + running doctor_frankentui graveyardctl ..."

# The command exits non-zero if the verify gate fails; capture so we can still
# inspect the materialized artifacts for triage.
GATE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json graveyardctl \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" --stage all \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

fail() {
  echo "[graveyardctl-e2e] FAIL: $1" >&2
  exit 1
}

# ── Artifact existence ──────────────────────────────────────────────────────
[ -s "${LEDGER}" ]   || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ]  || fail "pipeline summary missing: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing: ${MANIFEST}"

# ── Ledger contract: every line carries all AC3-mandated fields ─────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id               | type == "string" and (. | length) > 0) and
    (.stage                | type == "string" and (. | length) > 0) and
    (.entry_id             | type == "string" and (. | length) > 0) and
    (.card_id              | type == "string" and (. | length) > 0) and
    (.action               | type == "string" and (. | length) > 0) and
    (.verify_result        | type == "string" and (. | length) > 0) and
    (.missing_artifacts    | type == "array") and
    (.remediation          | type == "array") and
    (.detail               | type == "string" and (. | length) > 0) and
    (.reproduction_command | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the record contract"
done <"${LEDGER}"
echo "[graveyardctl-e2e] ledger lines validated: ${LINE_NO}"

# ── Coverage: all five workflow stages appear ───────────────────────────────
STAGES="$(jq -r '.stage' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${STAGES}" -eq 5 ] || fail "expected 5 workflow stages, got ${STAGES}"

# ── Verify gate (AC1): every active entry passed verify on the green path ────
NON_PASS="$(jq -r 'select(.stage == "verify" and .verify_result != "pass") | .entry_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${NON_PASS}" -eq 0 ] || fail "green corpus has ${NON_PASS} non-passing verify entries"

# ── Summary gate ────────────────────────────────────────────────────────────
jq -e '.gate_applies == true'             "${SUMMARY}" >/dev/null || fail "gate_applies != true"
jq -e '.gate_passes == true'              "${SUMMARY}" >/dev/null || fail "gate_passes != true"
jq -e '.required_fields_complete == true' "${SUMMARY}" >/dev/null || fail "required_fields_complete != true"
jq -e '.verify_total >= 1'                "${SUMMARY}" >/dev/null || fail "expected >= 1 verify entry"
jq -e '.verify_pass == .verify_total'     "${SUMMARY}" >/dev/null || fail "not all verify entries passed"
jq -e '.verify_incomplete == 0'           "${SUMMARY}" >/dev/null || fail "unexpected incomplete entries"
jq -e '.verify_inconsistent == 0'         "${SUMMARY}" >/dev/null || fail "unexpected inconsistent entries"

ACTIVE="$(jq -r '.active_entries' "${SUMMARY}")"
TOTAL="$(jq -r '.total_ledger_lines' "${SUMMARY}")"
VPASS="$(jq -r '.verify_pass' "${SUMMARY}")"
VTOTAL="$(jq -r '.verify_total' "${SUMMARY}")"
echo "[graveyardctl-e2e] active=${ACTIVE} ledger_lines=${TOTAL} verify=${VPASS}/${VTOTAL}"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
echo "[graveyardctl-e2e] manifest integrity verified"

# ── Determinism (AC2): a second run yields a byte-identical ledger ──────────
RUN2_DIR="${RUN_ROOT}/green2"
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json graveyardctl \
  --run-root "${RUN_ROOT}" --run-name "green2" --stage all \
  >"${RUN_ROOT}/cli_stdout2.json" 2>"${RUN_ROOT}/cli_stderr2.log" || fail "second run failed"
if ! diff -q "${LEDGER}" "${RUN2_DIR}/evidence_ledger.jsonl" >/dev/null; then
  fail "ledger is not deterministic across runs"
fi
echo "[graveyardctl-e2e] determinism verified (byte-identical ledger)"

# The CLI gate itself must have passed on the green path.
[ "${GATE_EXIT}" -eq 0 ] || fail "graveyardctl command exited ${GATE_EXIT}"

echo "[graveyardctl-e2e] PASS — graveyardctl workflow gate green"
exit 0

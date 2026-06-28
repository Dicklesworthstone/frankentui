#!/usr/bin/env bash
#
# End-to-end graveyard-verify gate (bd-3bxhj.10.16).
#
# Runs the `doctor_frankentui graveyard-verify` command, which enforces alien-
# governance completeness (recommendation-contract fields, formal-guarantee
# status, galaxy-brain explainability), reproducibility, and fallback integrity
# (budget-exhaustion and assumption-violation conservative fallbacks) before
# release-candidate promotion. It materializes a JSONL evidence ledger whose
# every line carries run_id, claim_id, policy_id, trace_id, fallback_reason,
# comparator verdict, guarantee_status, and a reproduction command. This wrapper
# validates the artifact contract and fails closed, making it suitable as a
# release-candidate promotion gate.
#
# Usage:
#   ./scripts/doctor_frankentui_graveyard_verify_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/graveyard_verify/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[graveyard-verify-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

echo "[graveyard-verify-e2e] run-root: ${RUN_ROOT}"
echo "[graveyard-verify-e2e] building + running doctor_frankentui graveyard-verify ..."

# The command exits non-zero if the gate fails; capture so we can still inspect
# the materialized artifacts for triage.
GATE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json graveyard-verify \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

fail() {
  echo "[graveyard-verify-e2e] FAIL: $1" >&2
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
    (.scenario_id          | type == "string" and (. | length) > 0) and
    (.scenario_kind        | type == "string" and (. | length) > 0) and
    (.claim_id             | type == "string" and (. | length) > 0) and
    (.policy_id            | type == "string" and (. | length) > 0) and
    (.trace_id             | type == "string" and (. | length) > 0) and
    (.dimension            | type == "string" and (. | length) > 0) and
    (.clause_id            | type == "string" and (. | length) > 0) and
    (.outcome              | type == "string" and (. | length) > 0) and
    (.guarantee_status     | type == "string" and (. | length) > 0) and
    (.comparator_verdict   | type == "string" and (. | length) > 0) and
    (.fallback_reason      | type == "string" and (. | length) > 0) and
    (.reproduction_command | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the record contract"
done <"${LEDGER}"
echo "[graveyard-verify-e2e] ledger lines validated: ${LINE_NO}"

# ── Coverage: all four scenario kinds + all seven dimensions appear ─────────
SCENARIO_KINDS="$(jq -r '.scenario_kind' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
DIMENSIONS="$(jq -r '.dimension' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${SCENARIO_KINDS}" -eq 4 ] || fail "expected 4 scenario kinds, got ${SCENARIO_KINDS}"
[ "${DIMENSIONS}" -eq 7 ]     || fail "expected 7 verification dimensions, got ${DIMENSIONS}"

# ── Fallback integrity (AC2): budget + assumption fallbacks fired ───────────
jq -e 'select(.dimension == "budget_fallback" and .outcome == "fallback_triggered")' \
  "${LEDGER}" >/dev/null || fail "budget-exhaustion fallback not triggered"
jq -e 'select(.dimension == "assumption_fallback" and .outcome == "fallback_triggered")' \
  "${LEDGER}" >/dev/null || fail "assumption-violation fallback not triggered"

# ── Completeness (AC1): incomplete contract is blocked ──────────────────────
jq -e 'select(.scenario_kind == "incomplete_contract" and .outcome == "defect_detected")' \
  "${LEDGER}" >/dev/null || fail "incomplete contract was not blocked"

# ── Summary gate ────────────────────────────────────────────────────────────
jq -e '.gate_passes == true'              "${SUMMARY}" >/dev/null || fail "gate_passes != true"
jq -e '.all_expectations_met == true'     "${SUMMARY}" >/dev/null || fail "all_expectations_met != true"
jq -e '.required_fields_complete == true' "${SUMMARY}" >/dev/null || fail "required_fields_complete != true"
jq -e '.scenarios_meeting_expectation == .total_scenarios' \
  "${SUMMARY}" >/dev/null || fail "not all scenarios met expectation"
jq -e '.defects_detected >= 1'    "${SUMMARY}" >/dev/null || fail "expected >= 1 defect detected"
jq -e '.fallbacks_triggered >= 2' "${SUMMARY}" >/dev/null || fail "expected >= 2 fallbacks triggered"

SCN="$(jq -r '.total_scenarios' "${SUMMARY}")"
TOTAL="$(jq -r '.total_checks' "${SUMMARY}")"
DEFECTS="$(jq -r '.defects_detected' "${SUMMARY}")"
FALLBACKS="$(jq -r '.fallbacks_triggered' "${SUMMARY}")"
echo "[graveyard-verify-e2e] scenarios=${SCN} total_checks=${TOTAL} defects=${DEFECTS} fallbacks=${FALLBACKS}"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
echo "[graveyard-verify-e2e] manifest integrity verified"

# The CLI gate itself must have passed on the green path.
[ "${GATE_EXIT}" -eq 0 ] || fail "graveyard-verify command exited ${GATE_EXIT}"

echo "[graveyard-verify-e2e] PASS — graveyard-verify gate green"
exit 0

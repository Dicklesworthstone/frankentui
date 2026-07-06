#!/usr/bin/env bash
# E2E gate for the release-candidate go/no-go decision (bd-3bxhj.9.9).
#
# Drives the doctor_frankentui `release-candidate` subcommand end-to-end and
# asserts: the per-section operational-trace contract (stage_span /
# sub_report_id / sub_evidence_checksum / policy_verdict / hotspot + score
# context / reproduction_command), full eight-section coverage, the three
# fail-closed RC clauses (rollback readiness, behavior-regression proofs,
# drift resolution), manifest SHA-256 integrity, and byte-identical
# determinism across a second run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/release_candidate/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "SKIP-BLOCKER: required command '$1' not found" >&2
    exit 2
  }
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

GATE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json release-candidate \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

[ -s "${LEDGER}" ] || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ] || fail "pipeline summary missing or empty: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing or empty: ${MANIFEST}"

# ── AC2: every section line carries the full operational trace ───────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id                 | type == "string" and (. | length) > 0) and
    (.section                | type == "string" and (. | length) > 0) and
    (.stage_span             | type == "string" and (. | length) > 0) and
    (.sub_report_id          | type == "string" and (. | length) > 0 and . != "n/a") and
    (.sub_evidence_checksum  | type == "string" and (. | length) > 0 and . != "n/a") and
    (.policy_verdict         | type == "string" and (. | length) > 0) and
    (.hotspot_context        | type == "string" and (. | length) > 0) and
    (.score_context          | type == "string" and (. | length) > 0) and
    (.gate_passed            | type == "boolean") and
    (.detail                 | type == "string" and (. | length) > 0) and
    (.reproduction_command   | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the operational-trace contract"
done <"${LEDGER}"

# ── AC1: all eight sections executed and passed ──────────────────────────────
SECTIONS="$(jq -r '.section' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${SECTIONS}" -eq 8 ] || fail "expected 8 distinct sections, saw ${SECTIONS}"
FAILED_SECTIONS="$(jq -r 'select(.gate_passed == false) | .section' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${FAILED_SECTIONS}" -eq 0 ] || fail "${FAILED_SECTIONS} section(s) failed their gate"

for section in operator_certification formal_assurance graveyard_verify graveyard_chain \
  deep_assurance chaos_drill optimization_gauntlet multi_round_drill; do
  jq -e --arg s "${section}" 'select(.section == $s and .policy_verdict == "pass")' \
    "${LEDGER}" | grep -q . || fail "section ${section} missing or not passing"
done

# The optimization section must carry hotspot/score context (AC2).
jq -e 'select(.section == "optimization_gauntlet")
  | (.hotspot_context != "n/a") and (.score_context != "n/a")' "${LEDGER}" \
  | grep -q true || fail "optimization section missing hotspot/score context"

# ── AC3: the three RC clauses hold fail-closed ───────────────────────────────
jq -e '
  .gate_passes == true and
  .required_fields_complete == true and
  .all_sections_passed == true and
  .rollback_readiness == true and
  .behavior_regression_proofs == true and
  .drift_resolved == true and
  .total_sections == 8
' "${SUMMARY}" >/dev/null || fail "RC summary clauses do not hold"

# ── Manifest SHA-256 integrity ───────────────────────────────────────────────
while IFS=$'\t' read -r fname declared; do
  [ -f "${RUN_DIR}/${fname}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${RUN_DIR}/${fname}" | awk '{print $1}')"
  [ "${actual}" = "${declared}" ] || fail "sha256 mismatch for ${fname}"
done < <(jq -r '.artifacts[] | [.file, .sha256] | @tsv' "${MANIFEST}")

# ── Determinism: a second run must produce a byte-identical ledger ───────────
SECOND_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json release-candidate \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}2" \
  >"${RUN_ROOT}/cli_stdout_2.json" 2>"${RUN_ROOT}/cli_stderr_2.log" || SECOND_EXIT=$?
[ "${SECOND_EXIT}" -eq 0 ] || fail "second release-candidate run exited ${SECOND_EXIT}"
diff -q "${LEDGER}" "${RUN_ROOT}/${RUN_NAME}2/evidence_ledger.jsonl" >/dev/null \
  || fail "evidence ledger is not byte-identical across runs"

[ "${GATE_EXIT}" -eq 0 ] || fail "release-candidate CLI exited ${GATE_EXIT} (NO-GO)"

echo "PASS: release-candidate E2E (${SECTIONS} sections GO, run ${RUN_DIR})"
exit 0

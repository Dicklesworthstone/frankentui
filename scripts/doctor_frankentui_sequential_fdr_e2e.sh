#!/usr/bin/env bash
#
# End-to-end sequential multiple-testing controller gate (bd-3bxhj.10.39).
#
# Runs the `doctor_frankentui sequential-fdr` command, which controls the false
# discovery rate over many migration gate families using e-BH (Wang & Ramdas,
# 2022) as the primary controller (FDR <= alpha under arbitrary dependence) plus
# a per-family Foster-Stine alpha-investing wealth process as a secondary online
# governor. It is a deterministic evalue -> ebh -> invest -> govern pipeline.
#
# Null orientation: each test is a claim that the migration is SAFE on a
# dimension; rejecting H0 (= "unsafe") means CERTIFYING the dimension safe; a
# large e-value is strong safety evidence. Conservative == non-permissive: when a
# family's wealth is exhausted or its evidence is adversarial, the test is
# WITHHELD (never silently certified).
#
# The default (all-stage) view applies the FDR gate: every decision clause must
# be consistent with its arithmetic, the e-BH certified count must equal k*, the
# final certified set must be a subset of the e-BH set, every conservative event
# must be surfaced (never silent), and the fairness transfers must conserve total
# wealth. The command fails closed on a malformed test, an inconsistent clause, a
# silent conservative event, a non-monotone certification, an e-BH count
# mismatch, or a wealth-conservation violation.
#
# Usage:
#   ./scripts/doctor_frankentui_sequential_fdr_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/sequential_fdr/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[sequential-fdr-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

echo "[sequential-fdr-e2e] run-root: ${RUN_ROOT}"
echo "[sequential-fdr-e2e] building + running doctor_frankentui sequential-fdr ..."

# The command exits non-zero if the gate fails; capture so we can still inspect
# the materialized artifacts for triage.
GATE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json sequential-fdr \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" --stage all \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

fail() {
  echo "[sequential-fdr-e2e] FAIL: $1" >&2
  exit 1
}

# ── Artifact existence ──────────────────────────────────────────────────────
[ -s "${LEDGER}" ]   || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ]  || fail "pipeline summary missing: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing: ${MANIFEST}"

# ── Ledger contract: every line carries the AC4 fields ──────────────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id               | type == "string" and (. | length) > 0) and
    (.stage                | type == "string" and (. | length) > 0) and
    (.test_id              | type == "string" and (. | length) > 0) and
    (.family               | type == "string" and (. | length) > 0) and
    (.decision             | type == "string" and (. | length) > 0) and
    (.conservative_reason  | type == "string" and (. | length) > 0) and
    (.evalue               | type == "string" and (. | length) > 0) and
    (.rejection_threshold  | type == "string" and (. | length) > 0) and
    (.wealth_before        | type == "string" and (. | length) > 0) and
    (.wealth_after         | type == "string" and (. | length) > 0) and
    (.clause_consistent    | type == "boolean") and
    (.detail               | type == "string" and (. | length) > 0) and
    (.remediation          | type == "array") and
    (.reproduction_command | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the record contract"
done <"${LEDGER}"
echo "[sequential-fdr-e2e] ledger lines validated: ${LINE_NO}"

# ── Coverage: all four pipeline stages appear ───────────────────────────────
STAGES="$(jq -r '.stage' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${STAGES}" -eq 4 ] || fail "expected 4 pipeline stages, got ${STAGES}"

# ── AC4: every decision clause is consistent with its arithmetic ────────────
BAD_CLAUSE="$(jq -r 'select(.clause_consistent == false) | .test_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${BAD_CLAUSE}" -eq 0 ] || fail "found ${BAD_CLAUSE} inconsistent decision clauses"

# ── e-BH self-check: certify iff evalue >= recorded threshold ───────────────
EBH_BAD="$(jq -r 'select(.stage == "ebh") | select((.decision == "certify") != ((.evalue | tonumber) >= (.rejection_threshold | tonumber))) | .test_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${EBH_BAD}" -eq 0 ] || fail "found ${EBH_BAD} e-BH lines whose decision disagrees with the threshold"

# ── AC3: every conservative event surfaces a non-empty remediation ──────────
BAD_CONSERVATIVE="$(jq -r 'select(.conservative_reason != "not_conservative" and (.remediation | length) == 0) | .test_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${BAD_CONSERVATIVE}" -eq 0 ] || fail "found ${BAD_CONSERVATIVE} silent conservative events"

# ── Summary gate ────────────────────────────────────────────────────────────
jq -e '.gate_applies == true'                 "${SUMMARY}" >/dev/null || fail "gate_applies != true"
jq -e '.gate_passes == true'                  "${SUMMARY}" >/dev/null || fail "gate_passes != true"
jq -e '.required_fields_complete == true'     "${SUMMARY}" >/dev/null || fail "required_fields_complete != true"
jq -e '.clauses_consistent == true'           "${SUMMARY}" >/dev/null || fail "clauses_consistent != true"
jq -e '.conservative_integrity_ok == true'    "${SUMMARY}" >/dev/null || fail "conservative_integrity_ok != true"
jq -e '.fdr_monotone_ok == true'              "${SUMMARY}" >/dev/null || fail "fdr_monotone_ok != true"
jq -e '.ebh_count_matches == true'            "${SUMMARY}" >/dev/null || fail "ebh_count_matches != true"
jq -e '.wealth_conserved == true'             "${SUMMARY}" >/dev/null || fail "wealth_conserved != true"
jq -e '.invalid == 0'                         "${SUMMARY}" >/dev/null || fail "unexpected invalid tests"
jq -e '.ebh_certified >= 1'                   "${SUMMARY}" >/dev/null || fail "expected >= 1 e-BH certification"
# The authoritative set must be a subset of the FDR-controlled e-BH set.
jq -e '.final_certified <= .ebh_certified'    "${SUMMARY}" >/dev/null || fail "final_certified exceeds ebh_certified"
# Every conservative event must be surfaced on a govern line.
jq -e '.conservative_events == .conservative_surfaced' "${SUMMARY}" >/dev/null || fail "a conservative event went unsurfaced"

KSTAR="$(jq -r '.ebh_k_star' "${SUMMARY}")"
EBH="$(jq -r '.ebh_certified' "${SUMMARY}")"
FINAL="$(jq -r '.final_certified' "${SUMMARY}")"
echo "[sequential-fdr-e2e] ebh_k_star=${KSTAR} ebh_certified=${EBH} final_certified=${FINAL}"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
echo "[sequential-fdr-e2e] manifest integrity verified"

# ── Determinism (AC2): a second run yields a byte-identical ledger ──────────
RUN2_DIR="${RUN_ROOT}/green2"
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json sequential-fdr \
  --run-root "${RUN_ROOT}" --run-name "green2" --stage all \
  >"${RUN_ROOT}/cli_stdout2.json" 2>"${RUN_ROOT}/cli_stderr2.log" || fail "second run failed"
if ! diff -q "${LEDGER}" "${RUN2_DIR}/evidence_ledger.jsonl" >/dev/null; then
  fail "ledger is not deterministic across runs"
fi
echo "[sequential-fdr-e2e] determinism verified (byte-identical ledger)"

# The CLI gate itself must have passed on the green path.
[ "${GATE_EXIT}" -eq 0 ] || fail "sequential-fdr command exited ${GATE_EXIT}"

echo "[sequential-fdr-e2e] PASS — sequential FDR controller gate green"
exit 0

#!/usr/bin/env bash
# E2E gate for the alien-artifact deep-assurance gauntlet (bd-3bxhj.10.45).
#
# Drives the doctor_frankentui `deep-assurance` subcommand end-to-end and
# asserts: the per-line evidence-pack record contract (posterior / wealth /
# guarantee / counterfactual / degradation / UX records + failure_signature +
# reproduction_command), full scenario + family coverage, per-scenario
# expected safe-action paths, red-path machine signatures, record-pack
# completeness, the fail-closed summary gate, manifest SHA-256 integrity,
# and byte-identical determinism across a second run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/deep_assurance/${TIMESTAMP_UTC}}"
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
  --machine json deep-assurance \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

[ -s "${LEDGER}" ] || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ] || fail "pipeline summary missing or empty: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing or empty: ${MANIFEST}"

# ── Per-line evidence-pack record contract ───────────────────────────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id                | type == "string" and (. | length) > 0) and
    (.scenario_id           | type == "string" and (. | length) > 0) and
    (.family                | type == "string" and (. | length) > 0) and
    (.kernel                | type == "string" and (. | length) > 0) and
    (.phase                 | type == "string" and (. | length) > 0) and
    (.posterior_record      | type == "string" and (. | length) > 0) and
    (.wealth_record         | type == "string" and (. | length) > 0) and
    (.guarantee_record      | type == "string" and (. | length) > 0) and
    (.counterfactual_record | type == "string" and (. | length) > 0) and
    (.degradation_record    | type == "string" and (. | length) > 0) and
    (.ux_record             | type == "string" and (. | length) > 0) and
    (.action_path           | type == "string" and (. | length) > 0) and
    (.expected_path         | type == "string" and (. | length) > 0) and
    (.safe                  | type == "boolean") and
    (.failure_signature     | type == "string" and (. | length) > 0) and
    (.detail                | type == "string" and (. | length) > 0) and
    (.reproduction_command  | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the record contract"
done <"${LEDGER}"

# ── Coverage: 12 scenarios across 6 families ─────────────────────────────────
SCENARIOS="$(jq -r '.scenario_id' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${SCENARIOS}" -eq 12 ] || fail "expected 12 distinct scenarios, saw ${SCENARIOS}"
FAMILIES="$(jq -r '.family' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${FAMILIES}" -eq 6 ] || fail "expected 6 distinct families, saw ${FAMILIES}"

# ── Safety: every phase safe, every path as expected ─────────────────────────
UNSAFE="$(jq -r 'select(.safe == false) | .scenario_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${UNSAFE}" -eq 0 ] || fail "${UNSAFE} unsafe ledger line(s)"
OFF_PATH="$(jq -r 'select(.action_path != .expected_path) | .scenario_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${OFF_PATH}" -eq 0 ] || fail "${OFF_PATH} line(s) off their expected path"

# ── Red paths carry machine signatures ───────────────────────────────────────
for scenario in fusion_sparse_drift fdr_adversarial_stopping counterfactual_unsat \
  degradation_campaign guarantee_mid_run_fault ux_adversarial_stress; do
  jq -e --arg s "${scenario}" \
    'select(.scenario_id == $s and .failure_signature != "none")' \
    "${LEDGER}" | grep -q . \
    || fail "red scenario ${scenario} carries no machine failure signature"
done

# ── Record-pack completeness: all six record types present ──────────────────
for record in posterior_record wealth_record guarantee_record \
  counterfactual_record degradation_record ux_record; do
  jq -e --arg r "${record}" 'select(.[$r] != "n/a")' "${LEDGER}" | grep -q . \
    || fail "evidence pack is missing ${record} entries"
done

# ── Summary gate booleans (fail-closed) ──────────────────────────────────────
jq -e '
  .gate_passes == true and
  .required_fields_complete == true and
  .all_safe == true and
  .red_paths_covered == true and
  .green_anchors == true and
  .record_pack_complete == true and
  .families_covered == 6 and
  .total_scenarios == 12
' "${SUMMARY}" >/dev/null || fail "pipeline summary gate booleans do not hold"

# ── Manifest SHA-256 integrity ───────────────────────────────────────────────
while IFS=$'\t' read -r fname declared; do
  [ -f "${RUN_DIR}/${fname}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${RUN_DIR}/${fname}" | awk '{print $1}')"
  [ "${actual}" = "${declared}" ] || fail "sha256 mismatch for ${fname}"
done < <(jq -r '.artifacts[] | [.file, .sha256] | @tsv' "${MANIFEST}")

# ── Determinism: a second run must produce a byte-identical ledger ───────────
SECOND_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json deep-assurance \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}2" \
  >"${RUN_ROOT}/cli_stdout_2.json" 2>"${RUN_ROOT}/cli_stderr_2.log" || SECOND_EXIT=$?
[ "${SECOND_EXIT}" -eq 0 ] || fail "second deep-assurance run exited ${SECOND_EXIT}"
diff -q "${LEDGER}" "${RUN_ROOT}/${RUN_NAME}2/evidence_ledger.jsonl" >/dev/null \
  || fail "evidence ledger is not byte-identical across runs"

[ "${GATE_EXIT}" -eq 0 ] || fail "deep-assurance CLI exited ${GATE_EXIT}"

echo "PASS: deep-assurance E2E (${SCENARIOS} scenarios, ${FAMILIES} families, run ${RUN_DIR})"
exit 0

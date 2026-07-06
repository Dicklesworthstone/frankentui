#!/usr/bin/env bash
# E2E gate for the graveyard-executable gauntlet (bd-3bxhj.10.37).
#
# Drives the doctor_frankentui `graveyard-gauntlet` subcommand end-to-end and
# asserts: the AC3 per-line ledger field contract (stage_id / route_id /
# ranking_hash / contract_id / verify_verdict / demo_id /
# release_policy_verdict / reproduction_command + violated_clause + triage
# fields), full scenario + family coverage, per-scenario terminal
# stage/outcome/cluster expectations, the fail-closed summary gate, manifest
# SHA-256 integrity, and byte-identical determinism across a second run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/graveyard_gauntlet/${TIMESTAMP_UTC}}"
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
  --machine json graveyard-gauntlet \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

[ -s "${LEDGER}" ] || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ] || fail "pipeline summary missing or empty: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing or empty: ${MANIFEST}"

# ── AC3: every ledger line carries the mandated machine-actionable fields ────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id                 | type == "string" and (. | length) > 0) and
    (.scenario_id            | type == "string" and (. | length) > 0) and
    (.family                 | type == "string" and (. | length) > 0) and
    (.stage_id               | type == "string" and (. | length) > 0) and
    (.route_id               | type == "string" and (. | length) > 0) and
    (.ranking_hash           | type == "string" and (. | length) > 0) and
    (.contract_id            | type == "string" and (. | length) > 0) and
    (.verify_verdict         | type == "string" and (. | length) > 0) and
    (.demo_id                | type == "string" and (. | length) > 0) and
    (.release_policy_verdict | type == "string" and (. | length) > 0) and
    (.stage_outcome          | type == "string" and (. | length) > 0) and
    (.gate_passed            | type == "boolean") and
    (.is_terminal_stage      | type == "boolean") and
    (.violated_clause        | type == "string" and (. | length) > 0) and
    (.triage_cluster         | type == "string" and (. | length) > 0) and
    (.detail                 | type == "string" and (. | length) > 0) and
    (.reproduction_command   | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the AC3 record contract"
done <"${LEDGER}"

# ── Coverage: 11 scenarios across 5 families ─────────────────────────────────
SCENARIOS="$(jq -r '.scenario_id' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${SCENARIOS}" -eq 11 ] || fail "expected 11 distinct scenarios, saw ${SCENARIOS}"
FAMILIES="$(jq -r '.family' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${FAMILIES}" -eq 5 ] || fail "expected 5 distinct families, saw ${FAMILIES}"

# ── Per-scenario terminal expectations (AC2: explicit machine clauses) ───────
assert_terminal() {
  local scenario="$1" stage="$2" outcome="$3" cluster="$4"
  jq -e --arg s "${scenario}" --arg st "${stage}" --arg o "${outcome}" --arg c "${cluster}" \
    'select(.scenario_id == $s and .is_terminal_stage == true)
     | (.stage == $st) and (.stage_outcome == $o) and (.triage_cluster == $c)' \
    "${LEDGER}" | grep -q true \
    || fail "scenario ${scenario} did not terminate at ${stage}/${outcome}/${cluster}"
}

assert_terminal green_promotion release release_pass none
assert_terminal malformed_header contract contract_blocked metadata_gap
assert_terminal incomplete_contract contract contract_blocked contract_artifact_gap
assert_terminal inconsistent_contract verify verify_inconsistent risk_posture
assert_terminal unsafe_combination contract composition_blocked composition_hazard
assert_terminal missing_interference_evidence contract composition_blocked composition_hazard
assert_terminal demo_divergence demo demo_divergent demo_reproducibility
assert_terminal claim_linkage_break demo linkage_broken linkage_drift
assert_terminal multi_lever_violation release release_rejected_lever lever_governance
assert_terminal rollback_clause_violation release release_rejected_rollback lever_governance
assert_terminal release_policy_hold release release_hold release_policy_hold

# Every red terminal line names a violated clause and a triage hint.
RED_WITHOUT_CLAUSE="$(jq -r 'select(.is_terminal_stage == true and .gate_passed == false)
  | select(.violated_clause == "none" or .violated_clause == "" or .triage_hint == "")
  | .scenario_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${RED_WITHOUT_CLAUSE}" -eq 0 ] \
  || fail "${RED_WITHOUT_CLAUSE} red terminal line(s) missing violated_clause/triage_hint"

# The green anchor walks all six chain stages.
GREEN_STAGES="$(jq -r 'select(.scenario_id == "green_promotion") | .stage' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${GREEN_STAGES}" -eq 6 ] || fail "green_promotion emitted ${GREEN_STAGES} stage lines, want 6"

# ── Summary gate booleans (fail-closed) ──────────────────────────────────────
jq -e '
  .gate_passes == true and
  .required_fields_complete == true and
  .all_expectations_met == true and
  .red_paths_covered == true and
  .green_anchor_promoted == true and
  .triage_actionable == true and
  .families_covered == 5 and
  .total_scenarios == 11
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
  --machine json graveyard-gauntlet \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}2" \
  >"${RUN_ROOT}/cli_stdout_2.json" 2>"${RUN_ROOT}/cli_stderr_2.log" || SECOND_EXIT=$?
[ "${SECOND_EXIT}" -eq 0 ] || fail "second gauntlet run exited ${SECOND_EXIT}"
diff -q "${LEDGER}" "${RUN_ROOT}/${RUN_NAME}2/evidence_ledger.jsonl" >/dev/null \
  || fail "evidence ledger is not byte-identical across runs"

[ "${GATE_EXIT}" -eq 0 ] || fail "graveyard-gauntlet CLI exited ${GATE_EXIT}"

echo "PASS: graveyard-gauntlet E2E (${SCENARIOS} scenarios, ${FAMILIES} families, run ${RUN_DIR})"
exit 0

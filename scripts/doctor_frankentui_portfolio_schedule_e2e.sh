#!/usr/bin/env bash
#
# End-to-end expected-loss portfolio scheduler gate (bd-3bxhj.10.18).
#
# Runs the `doctor_frankentui portfolio-schedule` command, which chooses among
# candidate alien primitives (math families: symbolic / search / probabilistic /
# formal-analysis) per migration milestone to minimize expected loss while
# maintaining branch diversity, budget safety, and formal-guarantee
# compatibility. It is a deterministic score -> select -> diversify -> govern
# pipeline that reuses the decision-loss policy engine for the per-candidate
# expected loss.
#
# Conservative == non-permissive: when a selection is uncertain (high posterior
# variance / VOI with a thin loss margin), when a drift monitor fires, or when
# the budget would be exceeded, the scheduler falls back to the minimax-safe,
# guarantee-applicable primitive (or DEFERS) and SURFACES it with remediation —
# never silently shipping the aggressive lever.
#
# The default (all-stage) view applies the portfolio gate: every decision clause
# must be consistent with its arithmetic, every pre-governance selection must be
# the feasible argmin, every committed selection must clear its quality bar, no
# family may breach the diversity cap, the committed cost must be within budget,
# every safety event must be surfaced (never silent), and the governor must only
# downgrade to a safer (<= worst-case) selection. The command fails closed on any
# violation.
#
# Usage:
#   ./scripts/doctor_frankentui_portfolio_schedule_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/portfolio_scheduler/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[portfolio-schedule-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

echo "[portfolio-schedule-e2e] run-root: ${RUN_ROOT}"
echo "[portfolio-schedule-e2e] building + running doctor_frankentui portfolio-schedule ..."

# The command exits non-zero if the gate fails; capture so we can still inspect
# the materialized artifacts for triage.
GATE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json portfolio-schedule \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" --stage all \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

fail() {
  echo "[portfolio-schedule-e2e] FAIL: $1" >&2
  exit 1
}

# ── Artifact existence ──────────────────────────────────────────────────────
[ -s "${LEDGER}" ]   || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ]  || fail "pipeline summary missing: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing: ${MANIFEST}"

# ── Ledger contract: every line carries the AC1/AC4 fields ──────────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id               | type == "string" and (. | length) > 0) and
    (.stage                | type == "string" and (. | length) > 0) and
    (.milestone_id         | type == "string" and (. | length) > 0) and
    (.primitive_id         | type == "string" and (. | length) > 0) and
    (.family               | type == "string" and (. | length) > 0) and
    (.decision             | type == "string" and (. | length) > 0) and
    (.safety_trigger       | type == "string" and (. | length) > 0) and
    (.selected_action      | type == "string" and (. | length) > 0) and
    (.posterior_mean       | type == "string" and (. | length) > 0) and
    (.posterior_variance   | type == "string" and (. | length) > 0) and
    (.voi                  | type == "string" and (. | length) > 0) and
    (.expected_loss        | type == "string" and (. | length) > 0) and
    (.worst_case_loss      | type == "string" and (. | length) > 0) and
    (.clause_consistent    | type == "boolean") and
    (.detail               | type == "string" and (. | length) > 0) and
    (.remediation          | type == "array") and
    (.reproduction_command | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the record contract"
done <"${LEDGER}"
echo "[portfolio-schedule-e2e] ledger lines validated: ${LINE_NO}"

# ── Coverage: all four pipeline stages appear ───────────────────────────────
STAGES="$(jq -r '.stage' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${STAGES}" -eq 4 ] || fail "expected 4 pipeline stages, got ${STAGES}"

# ── AC4: every decision clause is consistent with its arithmetic ────────────
BAD_CLAUSE="$(jq -r 'select(.clause_consistent == false) | .primitive_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${BAD_CLAUSE}" -eq 0 ] || fail "found ${BAD_CLAUSE} inconsistent decision clauses"

# ── AC1 self-check: the selected winner has the minimum feasible expected loss ─
# For every milestone, no NOT-SELECTED candidate may undercut the SELECTED one.
SELECT_BAD="$(jq -s '
  [ .[] | select(.stage == "select") ]
  | group_by(.milestone_id)
  | map(
      (map(select(.decision == "select")) | .[0].expected_loss | if . == null then 1e18 else tonumber end) as $win
      | map(select(.decision == "not_selected") | (.expected_loss | tonumber))
      | map(select(. < $win - 1e-9))
      | length
    )
  | add // 0
' "${LEDGER}")"
[ "${SELECT_BAD}" -eq 0 ] || fail "found ${SELECT_BAD} milestones where a non-selected candidate undercut the winner"

# ── AC3: every safety-mode (conservative) event surfaces non-empty remediation ─
BAD_CONSERVATIVE="$(jq -r 'select(.decision == "conservative" and (.remediation | length) == 0) | .milestone_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${BAD_CONSERVATIVE}" -eq 0 ] || fail "found ${BAD_CONSERVATIVE} silent conservative events"

# ── AC2: every diversity violation surfaces non-empty remediation ────────────
BAD_DIVERSITY="$(jq -r 'select(.decision == "diversity_violation" and (.remediation | length) == 0) | .primitive_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${BAD_DIVERSITY}" -eq 0 ] || fail "found ${BAD_DIVERSITY} silent diversity violations"

# ── Summary gate ────────────────────────────────────────────────────────────
jq -e '.gate_applies == true'              "${SUMMARY}" >/dev/null || fail "gate_applies != true"
jq -e '.gate_passes == true'               "${SUMMARY}" >/dev/null || fail "gate_passes != true"
jq -e '.required_fields_complete == true'  "${SUMMARY}" >/dev/null || fail "required_fields_complete != true"
jq -e '.clauses_consistent == true'        "${SUMMARY}" >/dev/null || fail "clauses_consistent != true"
jq -e '.selection_optimal_ok == true'      "${SUMMARY}" >/dev/null || fail "selection_optimal_ok != true"
jq -e '.quality_bar_ok == true'            "${SUMMARY}" >/dev/null || fail "quality_bar_ok != true"
jq -e '.diversity_ok == true'              "${SUMMARY}" >/dev/null || fail "diversity_ok != true"
jq -e '.diversity_integrity_ok == true'    "${SUMMARY}" >/dev/null || fail "diversity_integrity_ok != true"
jq -e '.budget_safe == true'               "${SUMMARY}" >/dev/null || fail "budget_safe != true"
jq -e '.conservative_integrity_ok == true' "${SUMMARY}" >/dev/null || fail "conservative_integrity_ok != true"
jq -e '.safety_monotone_ok == true'        "${SUMMARY}" >/dev/null || fail "safety_monotone_ok != true"
jq -e '.invalid == 0'                      "${SUMMARY}" >/dev/null || fail "unexpected invalid candidates"
jq -e '.diversity_violations == 0'         "${SUMMARY}" >/dev/null || fail "unexpected diversity violations"
jq -e '.final_selected >= 1'               "${SUMMARY}" >/dev/null || fail "expected >= 1 committed selection"
# Every safety event must be surfaced on a govern line.
jq -e '.conservative_events == .conservative_surfaced' "${SUMMARY}" >/dev/null || fail "a safety event went unsurfaced"

MS="$(jq -r '.total_milestones' "${SUMMARY}")"
SEL="$(jq -r '.final_selected' "${SUMMARY}")"
COST="$(jq -r '.committed_cost' "${SUMMARY}")"
echo "[portfolio-schedule-e2e] milestones=${MS} final_selected=${SEL} committed_cost=${COST}"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
echo "[portfolio-schedule-e2e] manifest integrity verified"

# ── Determinism (AC4): a second run yields a byte-identical ledger ──────────
RUN2_DIR="${RUN_ROOT}/green2"
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json portfolio-schedule \
  --run-root "${RUN_ROOT}" --run-name "green2" --stage all \
  >"${RUN_ROOT}/cli_stdout2.json" 2>"${RUN_ROOT}/cli_stderr2.log" || fail "second run failed"
if ! diff -q "${LEDGER}" "${RUN2_DIR}/evidence_ledger.jsonl" >/dev/null; then
  fail "ledger is not deterministic across runs"
fi
echo "[portfolio-schedule-e2e] determinism verified (byte-identical ledger)"

# The CLI gate itself must have passed on the green path.
[ "${GATE_EXIT}" -eq 0 ] || fail "portfolio-schedule command exited ${GATE_EXIT}"

echo "[portfolio-schedule-e2e] PASS — expected-loss portfolio scheduler gate green"
exit 0

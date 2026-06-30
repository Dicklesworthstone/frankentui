#!/usr/bin/env bash
#
# End-to-end optimization gauntlet validation gate (bd-3bxhj.8.21).
#
# Runs the `doctor_frankentui optimization-gauntlet` command, which executes the
# full extreme-optimization loop —
# baseline -> profile -> score -> one-lever -> isomorphism -> reprofile -> rollback
# drill — across positive, red, and drift scenarios, and proves the loop both
# promotes a safe optimization and refuses every unsafe one:
#
#   - positive path: a measurable improvement with unchanged golden outputs promotes;
#   - red paths: low score, multi-lever, checksum mismatch, unstable profiling, and a
#     post-change regression are each refused (rejected or rolled back);
#   - drift path: a post-change hotspot shift reorders the next candidates, still
#     promoting.
#
# The gate fails closed if any scenario's decision is unsafe, any stage line is
# missing a field, the positive path fails to promote, a red path is not refused,
# the rollback does not restore the incumbent, or the drift path does not reorder.
#
# Usage:
#   ./scripts/doctor_frankentui_optimization_gauntlet_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/optimization_gauntlet/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[optimization-gauntlet-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

fail() {
  echo "[optimization-gauntlet-e2e] FAIL: $1" >&2
  exit 1
}

echo "[optimization-gauntlet-e2e] run-root: ${RUN_ROOT}"
echo "[optimization-gauntlet-e2e] building + running doctor_frankentui optimization-gauntlet ..."

GATE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json optimization-gauntlet \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

# ── Artifact existence ──────────────────────────────────────────────────────
[ -s "${LEDGER}" ]   || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ]  || fail "pipeline summary missing: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing: ${MANIFEST}"

# ── AC1: every line carries the mandated per-stage fields ───────────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id              | type == "string" and (. | length) > 0) and
    (.scenario_id         | type == "string" and (. | length) > 0) and
    (.stage               | type == "string" and (. | length) > 0) and
    (.stage_index         | type == "number") and
    (.kernel              | type == "string" and (. | length) > 0) and
    (.policy_id           | type == "string" and (. | length) > 0) and
    (.subject_id          | type == "string" and (. | length) > 0) and
    (.upstream_stage      | type == "string" and (. | length) > 0) and
    (.hotspot_id          | type == "string" and (. | length) > 0) and
    (.proof_id            | type == "string" and (. | length) > 0) and
    (.score_terms         | type == "array" and (. | length) > 0) and
    (.stage_outcome       | type == "string" and (. | length) > 0) and
    (.gate_passed         | type == "boolean") and
    (.is_terminal_stage   | type == "boolean") and
    (.baseline_restored   | type == "boolean") and
    (.bottleneck_shifted  | type == "boolean") and
    (.backlog_reordered   | type == "boolean") and
    (.scenario_decision   | type == "string" and (. | length) > 0) and
    (.expected_decision   | type == "string" and (. | length) > 0) and
    (.decision_safe       | type == "boolean") and
    (.reproduction_command| type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the AC1 stage-record contract"
done <"${LEDGER}"
echo "[optimization-gauntlet-e2e] stage ledger lines validated: ${LINE_NO}"

# ── Coverage: all seven scenarios + all seven loop stages appear ────────────
SCENARIOS="$(jq -r '.scenario_id' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${SCENARIOS}" -eq 7 ] || fail "expected 7 scenarios, got ${SCENARIOS}"
for st in baseline profile score one_lever isomorphism reprofile rollback; do
  n="$(jq -r --arg s "${st}" 'select(.stage == $s) | .scenario_id' "${LEDGER}" | wc -l | tr -d ' ')"
  [ "${n}" -ge 1 ] || fail "loop stage '${st}' never appears"
done

# ── No unsafe decisions anywhere ────────────────────────────────────────────
UNSAFE="$(jq -r 'select(.decision_safe == false) | .scenario_id' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${UNSAFE}" -eq 0 ] || fail "found ${UNSAFE} scenarios with an unsafe decision"

# ── Per-scenario terminal decision (the decision pseudo-stage) ──────────────
assert_decision() {
  local id="$1" want="$2" got
  got="$(jq -r --arg id "${id}" 'select(.scenario_id == $id and .stage == "decision") | .scenario_decision' "${LEDGER}")"
  [ "${got}" = "${want}" ] || fail "scenario ${id}: decision '${got}' != '${want}'"
}
assert_decision "positive_promote"    "promote"
assert_decision "low_score_reject"    "reject_low_score"
assert_decision "multi_lever_reject"  "reject_multi_lever"
assert_decision "checksum_mismatch"   "reject_isomorphism"
assert_decision "unstable_profiling"  "reject_unstable"
assert_decision "regression_rollback" "rollback"
assert_decision "hotspot_drift"       "promote"

# Rollback must restore the incumbent; drift must reorder the backlog.
jq -e 'select(.scenario_id == "regression_rollback" and .stage == "rollback") | .stage_outcome == "rollback_triggered" and .baseline_restored == true' "${LEDGER}" >/dev/null \
  || fail "regression_rollback did not roll back + restore the incumbent"
jq -e 'select(.scenario_id == "hotspot_drift" and .stage == "reprofile") | .bottleneck_shifted == true and .backlog_reordered == true' "${LEDGER}" >/dev/null \
  || fail "hotspot_drift did not shift the bottleneck + reorder the backlog"

# ── Summary gate ────────────────────────────────────────────────────────────
jq -e '.gate_passes == true'              "${SUMMARY}" >/dev/null || fail "gate_passes != true"
jq -e '.required_fields_complete == true' "${SUMMARY}" >/dev/null || fail "required_fields_complete != true"
jq -e '.all_decisions_safe == true'       "${SUMMARY}" >/dev/null || fail "all_decisions_safe != true"
jq -e '.all_loop_stages_present == true'  "${SUMMARY}" >/dev/null || fail "all_loop_stages_present != true"
jq -e '.positive_promoted == true'        "${SUMMARY}" >/dev/null || fail "positive_promoted != true"
jq -e '.red_paths_covered == true'        "${SUMMARY}" >/dev/null || fail "red_paths_covered != true"
jq -e '.rollback_restored == true'        "${SUMMARY}" >/dev/null || fail "rollback_restored != true"
jq -e '.drift_reordered == true'          "${SUMMARY}" >/dev/null || fail "drift_reordered != true"
jq -e '.total_scenarios == 7'             "${SUMMARY}" >/dev/null || fail "expected 7 total scenarios"
echo "[optimization-gauntlet-e2e] summary gate validated"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
echo "[optimization-gauntlet-e2e] manifest integrity verified"

# ── Determinism: a second run yields a byte-identical ledger ────────────────
RUN2_DIR="${RUN_ROOT}/green2"
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json optimization-gauntlet \
  --run-root "${RUN_ROOT}" --run-name "green2" \
  >"${RUN_ROOT}/cli_stdout2.json" 2>"${RUN_ROOT}/cli_stderr2.log" || fail "second run failed"
if ! diff -q "${LEDGER}" "${RUN2_DIR}/evidence_ledger.jsonl" >/dev/null; then
  fail "ledger is not deterministic across runs"
fi
echo "[optimization-gauntlet-e2e] determinism verified (byte-identical ledger)"

# The CLI gate itself must have passed on the green path.
[ "${GATE_EXIT}" -eq 0 ] || fail "optimization-gauntlet command exited ${GATE_EXIT}"

echo "[optimization-gauntlet-e2e] PASS — optimization gauntlet gate green"
exit 0

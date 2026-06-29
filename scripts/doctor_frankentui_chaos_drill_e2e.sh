#!/usr/bin/env bash
#
# End-to-end reverse-round chaos drill + portfolio-fallback validation gate
# (bd-3bxhj.10.20).
#
# Runs the `doctor_frankentui chaos-drill` command, which stress-tests the three
# alien-governance decision kernels under adversarial conditions and proves they
# DEGRADE SAFELY (never silently promoting an unsafe change):
#
#   - reverse-round one-lever governance: a multi-lever merge without an override
#     is BLOCKED; a behavior-changing (non-isomorphic) lever is BLOCKED; a
#     percentile-regression drift triggers an automatic ROLLBACK that restores the
#     incumbent baseline;
#   - portfolio scheduler: budget exhaustion DEFERS, an uncertainty spike and a
#     drift signal force a CONSERVATIVE fallback;
#   - guarantee layer: a conformal coverage (calibration) failure recommends a
#     conservative FALLBACK; an optional-stopping stream HOLDS (anytime-valid, no
#     false discovery).
#
# A baseline (unperturbed) scenario proves the drill still PROMOTES a safe change,
# so the gate is not vacuously green by always blocking.
#
# The gate fails closed if any scenario did not degrade safely, any ledger line is
# missing a mandated field, the budget/calibration/uncertainty red paths are not
# all covered, or the performance-drift rollback did not restore the baseline.
#
# Usage:
#   ./scripts/doctor_frankentui_chaos_drill_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/chaos_drill/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[chaos-drill-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

echo "[chaos-drill-e2e] run-root: ${RUN_ROOT}"
echo "[chaos-drill-e2e] building + running doctor_frankentui chaos-drill ..."

GATE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json chaos-drill \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

fail() {
  echo "[chaos-drill-e2e] FAIL: $1" >&2
  exit 1
}

# ── Artifact existence ──────────────────────────────────────────────────────
[ -s "${LEDGER}" ]   || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ]  || fail "pipeline summary missing: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing: ${MANIFEST}"

# ── AC1: every line carries the mandated fields ─────────────────────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id               | type == "string" and (. | length) > 0) and
    (.chaos_scenario_id    | type == "string" and (. | length) > 0) and
    (.kernel               | type == "string" and (. | length) > 0) and
    (.policy_id            | type == "string" and (. | length) > 0) and
    (.claim_id             | type == "string" and (. | length) > 0) and
    (.action_path          | type == "string" and (. | length) > 0) and
    (.expected_path        | type == "string" and (. | length) > 0) and
    (.guarantee_status     | type == "string" and (. | length) > 0) and
    (.fallback_reason      | type == "string" and (. | length) > 0) and
    (.rollback_verdict     | type == "string" and (. | length) > 0) and
    (.safe_degradation_ok  | type == "boolean") and
    (.baseline_restored    | type == "boolean") and
    (.detail               | type == "string" and (. | length) > 0) and
    (.reproduction_command | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the AC1 record contract"
done <"${LEDGER}"
echo "[chaos-drill-e2e] ledger lines validated: ${LINE_NO}"

# ── Coverage: all nine scenarios + all three kernels appear ─────────────────
SCENARIOS="$(jq -r '.chaos_scenario_id' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${SCENARIOS}" -eq 9 ] || fail "expected 9 chaos scenarios, got ${SCENARIOS}"
KERNELS="$(jq -r '.kernel' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${KERNELS}" -eq 3 ] || fail "expected 3 kernels, got ${KERNELS}"

# ── Safe degradation: every scenario's observed path matches its expectation ─
UNSAFE="$(jq -r 'select(.safe_degradation_ok == false) | .chaos_scenario_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${UNSAFE}" -eq 0 ] || fail "found ${UNSAFE} scenarios that did not degrade safely"
PATH_BAD="$(jq -r 'select(.action_path != .expected_path) | .chaos_scenario_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${PATH_BAD}" -eq 0 ] || fail "found ${PATH_BAD} scenarios whose action_path != expected_path"

# ── Per-scenario assertions (the AC2 red paths + reverse-round guarantees) ───
assert_path() {
  local id="$1" want="$2"
  local got
  got="$(jq -r --arg id "${id}" 'select(.chaos_scenario_id == $id) | .action_path' "${LEDGER}")"
  [ "${got}" = "${want}" ] || fail "scenario ${id}: action_path '${got}' != '${want}'"
}
assert_path "multi_lever_merge"       "blocked"
assert_path "contradictory_evidence"  "blocked"
assert_path "performance_drift"       "rollback"
assert_path "budget_exhaustion"       "defer"
assert_path "uncertainty_spike"       "conservative"
assert_path "portfolio_drift"         "conservative"
assert_path "calibration_failure"     "fallback"
assert_path "optional_stopping"       "holds"
assert_path "baseline_promote"        "promote"

# Rollback must restore the incumbent baseline and record the rollback verdict.
jq -e 'select(.chaos_scenario_id == "performance_drift") | .baseline_restored == true' "${LEDGER}" >/dev/null \
  || fail "performance_drift did not restore the incumbent baseline"
jq -e 'select(.chaos_scenario_id == "performance_drift") | .rollback_verdict == "rollback_triggered"' "${LEDGER}" >/dev/null \
  || fail "performance_drift did not trigger an automatic rollback"
# The guarantee layer must surface a fallback under calibration failure.
jq -e 'select(.chaos_scenario_id == "calibration_failure") | .guarantee_status == "fallback"' "${LEDGER}" >/dev/null \
  || fail "calibration_failure did not surface a guarantee fallback"
# Optional stopping must hold (anytime-valid, no false discovery).
jq -e 'select(.chaos_scenario_id == "optional_stopping") | .guarantee_status == "holds"' "${LEDGER}" >/dev/null \
  || fail "optional_stopping raised a false discovery"

# ── Summary gate ────────────────────────────────────────────────────────────
jq -e '.gate_passes == true'              "${SUMMARY}" >/dev/null || fail "gate_passes != true"
jq -e '.required_fields_complete == true' "${SUMMARY}" >/dev/null || fail "required_fields_complete != true"
jq -e '.all_safe == true'                 "${SUMMARY}" >/dev/null || fail "all_safe != true"
jq -e '.red_paths_covered == true'        "${SUMMARY}" >/dev/null || fail "red_paths_covered != true"
jq -e '.rollback_restored == true'        "${SUMMARY}" >/dev/null || fail "rollback_restored != true"
jq -e '.unsafe_scenarios == 0'            "${SUMMARY}" >/dev/null || fail "unexpected unsafe scenarios"
jq -e '.total_scenarios == 9'             "${SUMMARY}" >/dev/null || fail "expected 9 total scenarios"

SAFE="$(jq -r '.safe_scenarios' "${SUMMARY}")"
echo "[chaos-drill-e2e] safe_scenarios=${SAFE}/9 kernels=${KERNELS}"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
echo "[chaos-drill-e2e] manifest integrity verified"

# ── Determinism: a second run yields a byte-identical ledger ────────────────
RUN2_DIR="${RUN_ROOT}/green2"
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json chaos-drill \
  --run-root "${RUN_ROOT}" --run-name "green2" \
  >"${RUN_ROOT}/cli_stdout2.json" 2>"${RUN_ROOT}/cli_stderr2.log" || fail "second run failed"
if ! diff -q "${LEDGER}" "${RUN2_DIR}/evidence_ledger.jsonl" >/dev/null; then
  fail "ledger is not deterministic across runs"
fi
echo "[chaos-drill-e2e] determinism verified (byte-identical ledger)"

# The CLI gate itself must have passed on the green path.
[ "${GATE_EXIT}" -eq 0 ] || fail "chaos-drill command exited ${GATE_EXIT}"

echo "[chaos-drill-e2e] PASS — reverse-round chaos drill gate green"
exit 0

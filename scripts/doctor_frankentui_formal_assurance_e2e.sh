#!/usr/bin/env bash
#
# End-to-end formal-assurance gauntlet gate (bd-3bxhj.10.28).
#
# Runs the `doctor_frankentui formal-assurance` command, which proves the
# alien-artifact stack stays safe and auditable under sequential + adversarial
# conditions across four assurance areas:
#
#   - optional stopping: an e-process holds (no false discovery) on a
#     null-consistent stream but rejects a genuine degradation stream;
#   - coverage: conformal coverage holds when calibrated, falls back on a coverage
#     breakdown, and surfaces an explicit assumption violation when calibration is
#     insufficient;
#   - drift: a steady regime stays normal while a degrading regime auto-transitions
#     to rollback;
#   - explainability: a galaxy-brain card reconstructs the decision deterministically.
#
# The gate fails closed if any scenario did not reach its expected safe path, any
# ledger line is missing a mandated field, an assurance area is uncovered, or a
# calibration-breakdown / assumption-violation red path did not trigger a
# conservative fallback.
#
# Usage:
#   ./scripts/doctor_frankentui_formal_assurance_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/formal_assurance/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[formal-assurance-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

fail() {
  echo "[formal-assurance-e2e] FAIL: $1" >&2
  exit 1
}

echo "[formal-assurance-e2e] run-root: ${RUN_ROOT}"
echo "[formal-assurance-e2e] building + running doctor_frankentui formal-assurance ..."

GATE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json formal-assurance \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

# ── Artifact existence ──────────────────────────────────────────────────────
[ -s "${LEDGER}" ]   || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ]  || fail "pipeline summary missing: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing: ${MANIFEST}"

# ── AC1: every line carries the mandated fields ─────────────────────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id                | type == "string" and (. | length) > 0) and
    (.scenario_id           | type == "string" and (. | length) > 0) and
    (.area                  | type == "string" and (. | length) > 0) and
    (.kernel                | type == "string" and (. | length) > 0) and
    (.action_path           | type == "string" and (. | length) > 0) and
    (.expected_path         | type == "string" and (. | length) > 0) and
    (.safe                  | type == "boolean") and
    (.guarantee_status      | type == "string" and (. | length) > 0) and
    (.guarantee_assumptions | type == "array") and
    (.bound_terms           | type == "array" and (. | length) > 0) and
    (.decision_trace        | type == "string" and (. | length) > 0) and
    (.fallback_triggered    | type == "boolean") and
    (.detail                | type == "string" and (. | length) > 0) and
    (.reproduction_command  | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the AC1 record contract"
done <"${LEDGER}"
echo "[formal-assurance-e2e] ledger lines validated: ${LINE_NO}"

# ── Coverage: all 8 scenarios + all 4 assurance areas appear ────────────────
SCENARIOS="$(jq -r '.scenario_id' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${SCENARIOS}" -eq 8 ] || fail "expected 8 scenarios, got ${SCENARIOS}"
AREAS="$(jq -r '.area' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${AREAS}" -eq 4 ] || fail "expected 4 assurance areas, got ${AREAS}"

# ── Safety: observed path matches expectation everywhere ────────────────────
UNSAFE="$(jq -r 'select(.safe == false) | .scenario_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${UNSAFE}" -eq 0 ] || fail "found ${UNSAFE} scenarios that did not reach their expected path"
PATH_BAD="$(jq -r 'select(.action_path != .expected_path) | .scenario_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${PATH_BAD}" -eq 0 ] || fail "found ${PATH_BAD} scenarios whose action_path != expected_path"

# ── Per-scenario assertions (green + red paths) ─────────────────────────────
assert_path() {
  local id="$1" want="$2" got
  got="$(jq -r --arg id "${id}" 'select(.scenario_id == $id) | .action_path' "${LEDGER}")"
  [ "${got}" = "${want}" ] || fail "scenario ${id}: action_path '${got}' != '${want}'"
}
assert_path "optional_stopping_holds"   "holds"
assert_path "optional_stopping_detects" "rejected"
assert_path "coverage_holds"            "coverage_holds"
assert_path "coverage_breakdown"        "fallback"
assert_path "assumption_violation"      "fallback"
assert_path "drift_steady"              "normal"
assert_path "drift_incident"            "rollback"
assert_path "explainability_replay"     "reconstructed"

# ── AC2: calibration-breakdown + assumption-violation must trigger a fallback ─
for id in coverage_breakdown assumption_violation; do
  jq -e --arg id "${id}" 'select(.scenario_id == $id) | .fallback_triggered == true' "${LEDGER}" >/dev/null \
    || fail "${id} did not trigger a conservative fallback"
done
# The assumption-violation scenario must record an explicit assumption.
jq -e 'select(.scenario_id == "assumption_violation") | (.guarantee_assumptions | length) > 0' "${LEDGER}" >/dev/null \
  || fail "assumption_violation recorded no guarantee assumption"
# The incident must roll back; explainability must reconstruct deterministically.
jq -e 'select(.scenario_id == "drift_incident") | .fallback_triggered == true' "${LEDGER}" >/dev/null \
  || fail "drift_incident did not transition"
jq -e 'select(.scenario_id == "explainability_replay") | (.decision_trace | length) > 0' "${LEDGER}" >/dev/null \
  || fail "explainability_replay produced no decision trace"

# ── Summary gate ────────────────────────────────────────────────────────────
jq -e '.gate_passes == true'              "${SUMMARY}" >/dev/null || fail "gate_passes != true"
jq -e '.required_fields_complete == true' "${SUMMARY}" >/dev/null || fail "required_fields_complete != true"
jq -e '.all_safe == true'                 "${SUMMARY}" >/dev/null || fail "all_safe != true"
jq -e '.red_paths_covered == true'        "${SUMMARY}" >/dev/null || fail "red_paths_covered != true"
jq -e '.fallback_integrity == true'       "${SUMMARY}" >/dev/null || fail "fallback_integrity != true"
jq -e '.green_anchors == true'            "${SUMMARY}" >/dev/null || fail "green_anchors != true"
jq -e '.areas_covered == 4'               "${SUMMARY}" >/dev/null || fail "expected 4 areas covered"
jq -e '.total_scenarios == 8'             "${SUMMARY}" >/dev/null || fail "expected 8 scenarios"
echo "[formal-assurance-e2e] summary gate validated"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
echo "[formal-assurance-e2e] manifest integrity verified"

# ── Determinism: a second run yields a byte-identical ledger ────────────────
RUN2_DIR="${RUN_ROOT}/green2"
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json formal-assurance \
  --run-root "${RUN_ROOT}" --run-name "green2" \
  >"${RUN_ROOT}/cli_stdout2.json" 2>"${RUN_ROOT}/cli_stderr2.log" || fail "second run failed"
if ! diff -q "${LEDGER}" "${RUN2_DIR}/evidence_ledger.jsonl" >/dev/null; then
  fail "ledger is not deterministic across runs"
fi
echo "[formal-assurance-e2e] determinism verified (byte-identical ledger)"

# The CLI gate itself must have passed on the green path.
[ "${GATE_EXIT}" -eq 0 ] || fail "formal-assurance command exited ${GATE_EXIT}"

echo "[formal-assurance-e2e] PASS — formal-assurance gauntlet gate green"
exit 0

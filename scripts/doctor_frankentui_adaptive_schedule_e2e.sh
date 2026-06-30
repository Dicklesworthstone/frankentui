#!/usr/bin/env bash
#
# End-to-end adaptive-scheduling + drift-alert validation gate (bd-3bxhj.8.13).
#
# Runs the `doctor_frankentui adaptive-schedule` command, which contrasts the
# adaptive value-of-information schedule against the static round-robin baseline
# on identical datasets, screens each dataset for drift, and re-ranks the
# optimization backlog with a re-profile round:
#
#   - adaptive vs static: each dataset reports the adaptive and static
#     confidence-gain-per-compute plus the percentile/memory tradeoff;
#   - drift timeline + allocation decisions + hotspot movement + reprioritization
#     are recorded per dataset;
#   - the confidence-per-compute gate fails closed if the adaptive schedule
#     regresses below the static baseline on any dataset, or if any
#     optimization-policy invariant is violated.
#
# A skewed dataset proves adaptive *strictly* beats static (improvement_ratio > 1)
# and a drifting dataset proves the drift timeline is populated, so the gate is
# not vacuously green.
#
# Usage:
#   ./scripts/doctor_frankentui_adaptive_schedule_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/adaptive_schedule/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[adaptive-schedule-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

echo "[adaptive-schedule-e2e] run-root: ${RUN_ROOT}"
echo "[adaptive-schedule-e2e] building + running doctor_frankentui adaptive-schedule ..."

GATE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json adaptive-schedule \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

fail() {
  echo "[adaptive-schedule-e2e] FAIL: $1" >&2
  exit 1
}

# ── Artifact existence ──────────────────────────────────────────────────────
[ -s "${LEDGER}" ]   || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ]  || fail "pipeline summary missing: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing: ${MANIFEST}"

# ── AC1/AC2: every line carries the mandated fields ─────────────────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id                            | type == "string" and (. | length) > 0) and
    (.dataset_id                        | type == "string" and (. | length) > 0) and
    (.total_candidates                  | type == "number") and
    (.scheduled_count                   | type == "number") and
    (.adaptive_confidence_per_compute   | type == "string" and (. | length) > 0) and
    (.static_confidence_per_compute     | type == "string" and (. | length) > 0) and
    (.improvement_ratio                 | type == "string" and (. | length) > 0) and
    (.adaptive_at_least_static          | type == "boolean") and
    (.drift_event_count                 | type == "number") and
    (.drift_timeline                    | type == "array") and
    (.bottleneck_before                 | type == "string" and (. | length) > 0) and
    (.bottleneck_after                  | type == "string" and (. | length) > 0) and
    (.bottleneck_shifted                | type == "boolean") and
    (.reprioritized                     | type == "boolean") and
    (.p95_delta                         | type == "string" and (. | length) > 0) and
    (.p99_delta                         | type == "string" and (. | length) > 0) and
    (.memory_delta                      | type == "string" and (. | length) > 0) and
    (.policy_invariant_ok               | type == "boolean") and
    (.detail                            | type == "string" and (. | length) > 0) and
    (.reproduction_command              | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the record contract"
done <"${LEDGER}"
echo "[adaptive-schedule-e2e] ledger lines validated: ${LINE_NO}"

# ── Coverage: all three datasets appear ─────────────────────────────────────
DATASETS="$(jq -r '.dataset_id' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${DATASETS}" -eq 3 ] || fail "expected 3 datasets, got ${DATASETS}"

# ── AC3: adaptive must never regress confidence-per-compute on any dataset ──
REGRESSED="$(jq -r 'select(.adaptive_at_least_static == false) | .dataset_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${REGRESSED}" -eq 0 ] || fail "found ${REGRESSED} datasets where adaptive regressed confidence-per-compute"
POLICY_BAD="$(jq -r 'select(.policy_invariant_ok == false) | .dataset_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${POLICY_BAD}" -eq 0 ] || fail "found ${POLICY_BAD} datasets violating the optimization-policy invariant"

# ── Non-vacuous: skewed adaptive strictly beats static; drifting has a timeline ─
SKEWED_RATIO="$(jq -r 'select(.dataset_id == "ds.skewed") | .improvement_ratio' "${LEDGER}")"
awk -v r="${SKEWED_RATIO}" 'BEGIN { exit !(r > 1.0) }' \
  || fail "ds.skewed improvement_ratio ${SKEWED_RATIO} should exceed 1.0"
jq -e 'select(.dataset_id == "ds.skewed") | .reprioritized == true' "${LEDGER}" >/dev/null \
  || fail "ds.skewed did not reprioritize on its hotspot shift"
DRIFT_EVENTS="$(jq -r 'select(.dataset_id == "ds.drifting") | .drift_event_count' "${LEDGER}")"
[ "${DRIFT_EVENTS}" -gt 0 ] || fail "ds.drifting drift timeline is empty"
jq -e 'select(.dataset_id == "ds.drifting") | (.drift_timeline | length) > 0' "${LEDGER}" >/dev/null \
  || fail "ds.drifting drift_timeline array is empty"

# ── Summary gate ────────────────────────────────────────────────────────────
jq -e '.gate_passes == true'                  "${SUMMARY}" >/dev/null || fail "gate_passes != true"
jq -e '.required_fields_complete == true'     "${SUMMARY}" >/dev/null || fail "required_fields_complete != true"
jq -e '.all_adaptive_at_least_static == true' "${SUMMARY}" >/dev/null || fail "all_adaptive_at_least_static != true"
jq -e '.all_policy_invariants_ok == true'     "${SUMMARY}" >/dev/null || fail "all_policy_invariants_ok != true"
jq -e '.drift_exercised == true'              "${SUMMARY}" >/dev/null || fail "drift_exercised != true"
jq -e '.reprioritization_exercised == true'   "${SUMMARY}" >/dev/null || fail "reprioritization_exercised != true"
jq -e '.improvement_demonstrated == true'     "${SUMMARY}" >/dev/null || fail "improvement_demonstrated != true"
jq -e '.total_datasets == 3'                  "${SUMMARY}" >/dev/null || fail "expected 3 total datasets"
echo "[adaptive-schedule-e2e] summary gate validated"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
echo "[adaptive-schedule-e2e] manifest integrity verified"

# ── Determinism: a second run yields a byte-identical ledger ────────────────
RUN2_DIR="${RUN_ROOT}/green2"
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json adaptive-schedule \
  --run-root "${RUN_ROOT}" --run-name "green2" \
  >"${RUN_ROOT}/cli_stdout2.json" 2>"${RUN_ROOT}/cli_stderr2.log" || fail "second run failed"
if ! diff -q "${LEDGER}" "${RUN2_DIR}/evidence_ledger.jsonl" >/dev/null; then
  fail "ledger is not deterministic across runs"
fi
echo "[adaptive-schedule-e2e] determinism verified (byte-identical ledger)"

# The CLI gate itself must have passed on the green path.
[ "${GATE_EXIT}" -eq 0 ] || fail "adaptive-schedule command exited ${GATE_EXIT}"

echo "[adaptive-schedule-e2e] PASS — adaptive scheduling + drift-alert gate green"
exit 0

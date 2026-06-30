#!/usr/bin/env bash
#
# End-to-end nightly stress validation gate (bd-3bxhj.8.9).
#
# Runs the `doctor_frankentui nightly-stress` command, which validates the full
# optimization-aware evaluation flow over a mixed corpus with robust per-fixture
# logging, resume/retry, and behavior-preservation proofs:
#
#   - per-fixture lifecycle JSONL: stage timeline + timings, hotspot table, score
#     decision, and a terminal outcome class;
#   - resume/retry: a second run with --resume skips the fixtures the prior run
#     completed (matching the determinism fingerprint), re-using their lineage;
#   - artifact completeness + replay readiness + behavior-preservation proof: every
#     optimized path carries an isomorphism proof, and a behavior regression is
#     detected and refused promotion.
#
# The mixed corpus exercises optimized wins, a below-threshold fixture, and a
# behavior regression, so the gate is not vacuously green.
#
# Usage:
#   ./scripts/doctor_frankentui_nightly_stress_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/nightly_stress/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"
CHECKPOINT="${RUN_DIR}/checkpoint.json"
LEDGER_RUN1="${RUN_ROOT}/ledger_run1.jsonl"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[nightly-stress-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

fail() {
  echo "[nightly-stress-e2e] FAIL: $1" >&2
  exit 1
}

run_cli() {
  # $1 = run-name, $2 = extra flag (e.g. --resume or empty)
  local name="$1" extra="${2:-}"
  cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
    --machine json nightly-stress \
    --run-root "${RUN_ROOT}" --run-name "${name}" ${extra} \
    >"${RUN_ROOT}/cli_${name}.json" 2>"${RUN_ROOT}/cli_${name}.log"
}

echo "[nightly-stress-e2e] run-root: ${RUN_ROOT}"
echo "[nightly-stress-e2e] building + running doctor_frankentui nightly-stress (fresh) ..."

GATE_EXIT=0
run_cli "${RUN_NAME}" "" || GATE_EXIT=$?

# ── Artifact existence ──────────────────────────────────────────────────────
[ -s "${LEDGER}" ]     || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ]    || fail "pipeline summary missing: ${SUMMARY}"
[ -s "${MANIFEST}" ]   || fail "artifact manifest missing: ${MANIFEST}"
[ -s "${CHECKPOINT}" ] || fail "checkpoint missing: ${CHECKPOINT}"
cp "${LEDGER}" "${LEDGER_RUN1}"

# ── AC2: every line carries the mandated lifecycle fields ───────────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id              | type == "string" and (. | length) > 0) and
    (.fixture_id          | type == "string" and (. | length) > 0) and
    (.stage_id            | type == "string" and (. | length) > 0) and
    (.resumed             | type == "boolean") and
    (.baseline_id         | type == "string" and (. | length) > 0) and
    (.profile_id          | type == "string" and (. | length) > 0) and
    (.stage_timings       | type == "array" and (. | length) > 0) and
    (.hotspot_table       | type == "array") and
    (.score_activated     | type == "boolean") and
    (.score_value         | type == "string" and (. | length) > 0) and
    (.score_decision      | type == "string" and (. | length) > 0) and
    (.outcome_class       | type == "string" and (. | length) > 0) and
    (.proof_id            | type == "string" and (. | length) > 0) and
    (.proof_available     | type == "boolean") and
    (.behavior_preserved  | type == "boolean") and
    (.replay_ready        | type == "boolean") and
    (.rollback_ready      | type == "boolean") and
    (.reproduction_command| type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the lifecycle record contract"
done <"${LEDGER}"
echo "[nightly-stress-e2e] lifecycle ledger lines validated: ${LINE_NO}"

# ── Coverage: the three forward outcome classes appear (not vacuous) ────────
for cls in optimized below_threshold behavior_regression; do
  n="$(jq -r --arg c "${cls}" 'select(.outcome_class == $c) | .fixture_id' "${LEDGER}" | wc -l | tr -d ' ')"
  [ "${n}" -ge 1 ] || fail "outcome class '${cls}' was not exercised"
done

# ── AC3: every optimized path carries a proof and is replay-ready ───────────
BAD_OPT="$(jq -r 'select(.outcome_class == "optimized") | select(.proof_available == false or .behavior_preserved == false or .replay_ready == false) | .fixture_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${BAD_OPT}" -eq 0 ] || fail "found ${BAD_OPT} optimized fixtures lacking a proof / replay readiness"

# ── AC3: a behavior regression is detected and refused promotion ────────────
BAD_REG="$(jq -r 'select(.outcome_class == "behavior_regression") | select(.behavior_preserved == true or .replay_ready == true) | .fixture_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${BAD_REG}" -eq 0 ] || fail "a behavior regression was silently promoted"
jq -e 'select(.outcome_class == "behavior_regression") | .proof_available == true' "${LEDGER}" >/dev/null \
  || fail "behavior regression did not record its failing proof"

# ── Summary gate ────────────────────────────────────────────────────────────
jq -e '.gate_passes == true'                   "${SUMMARY}" >/dev/null || fail "gate_passes != true"
jq -e '.required_fields_complete == true'      "${SUMMARY}" >/dev/null || fail "required_fields_complete != true"
jq -e '.lineage_complete == true'              "${SUMMARY}" >/dev/null || fail "lineage_complete != true"
jq -e '.optimized_proven == true'              "${SUMMARY}" >/dev/null || fail "optimized_proven != true"
jq -e '.no_silent_regression == true'          "${SUMMARY}" >/dev/null || fail "no_silent_regression != true"
jq -e '.determinism_metadata_present == true'  "${SUMMARY}" >/dev/null || fail "determinism_metadata_present != true"
jq -e '(.determinism_fingerprint | length) > 0' "${SUMMARY}" >/dev/null || fail "determinism_fingerprint missing"
echo "[nightly-stress-e2e] summary gate validated"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
jq -e '.resumed_from_checkpoint == false' "${MANIFEST}" >/dev/null || fail "fresh run claims a resume"
echo "[nightly-stress-e2e] manifest integrity verified"

# ── AC1: resume/retry — a second run skips completed fixtures, preserving lineage ─
echo "[nightly-stress-e2e] running resume pass ..."
run_cli "${RUN_NAME}" "--resume" || fail "resume run failed"
RESUMED_TOTAL="$(jq -r '.total_fixtures' "${SUMMARY}")"
RESUMED_COUNT="$(jq -r '.resumed' "${SUMMARY}")"
[ "${RESUMED_COUNT}" = "${RESUMED_TOTAL}" ] \
  || fail "resume skipped ${RESUMED_COUNT}/${RESUMED_TOTAL} fixtures (expected all)"
jq -e '.gate_passes == true' "${SUMMARY}" >/dev/null || fail "resume gate did not pass"
jq -e '.resumed_from_checkpoint == true' "${MANIFEST}" >/dev/null || fail "resume run did not consume the checkpoint"
# Lineage preserved across resume: baseline/profile ids unchanged per fixture.
while IFS= read -r fid; do
  b1="$(jq -r --arg f "${fid}" 'select(.fixture_id == $f) | .baseline_id' "${LEDGER_RUN1}")"
  b2="$(jq -r --arg f "${fid}" 'select(.fixture_id == $f) | .baseline_id' "${LEDGER}")"
  [ "${b1}" = "${b2}" ] || fail "fixture ${fid} lineage changed across resume (${b1} != ${b2})"
done < <(jq -r '.fixture_id' "${LEDGER_RUN1}")
echo "[nightly-stress-e2e] resume/retry verified (all fixtures resumed, lineage preserved)"

# ── Determinism: an independent fresh run yields a byte-identical ledger ────
run_cli "green_det" "" || fail "determinism run failed"
if ! diff -q "${LEDGER_RUN1}" "${RUN_ROOT}/green_det/evidence_ledger.jsonl" >/dev/null; then
  fail "ledger is not deterministic across fresh runs"
fi
echo "[nightly-stress-e2e] determinism verified (byte-identical ledger)"

# The CLI gate itself must have passed on the fresh path.
[ "${GATE_EXIT}" -eq 0 ] || fail "nightly-stress command exited ${GATE_EXIT}"

echo "[nightly-stress-e2e] PASS — nightly stress gate green"
exit 0

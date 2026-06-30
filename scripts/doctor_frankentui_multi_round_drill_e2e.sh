#!/usr/bin/env bash
#
# End-to-end multi-round optimization drill gate (bd-3bxhj.8.24).
#
# Runs the `doctor_frankentui multi-round-drill` command, which demonstrates
# correct progression across the three optimization tiers with proof + rollback
# discipline preserved (one round per tier):
#
#   - Round 1 (low-hanging): measurable gain + preserved golden outputs;
#   - Round 2 (algorithmic): eligible only once Round 1 is exhausted;
#   - Round 3 (exotic): heightened safety + a rollback rehearsal, eligible only
#     once Round 2 is exhausted.
#
# Every round records its tier eligibility + transition evidence (prior tiers
# exhausted), baseline/profile/proof artifacts, and a post-change re-profile delta.
# The gate fails closed on an invalid tier jump, a missing proof, an unmeasured
# gain, or a rollback-readiness failure.
#
# Usage:
#   ./scripts/doctor_frankentui_multi_round_drill_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/multi_round_drill/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[multi-round-drill-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

fail() {
  echo "[multi-round-drill-e2e] FAIL: $1" >&2
  exit 1
}

echo "[multi-round-drill-e2e] run-root: ${RUN_ROOT}"
echo "[multi-round-drill-e2e] building + running doctor_frankentui multi-round-drill ..."

GATE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json multi-round-drill \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

# ── Artifact existence ──────────────────────────────────────────────────────
[ -s "${LEDGER}" ]   || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ]  || fail "pipeline summary missing: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing: ${MANIFEST}"

# ── AC1/AC2: every round line carries the mandated fields ───────────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id                | type == "string" and (. | length) > 0) and
    (.round_number          | type == "number") and
    (.tier                  | type == "string" and (. | length) > 0) and
    (.eligibility           | type == "string" and (. | length) > 0) and
    (.prior_tiers_exhausted | type == "boolean") and
    (.active_tier_matched   | type == "boolean") and
    (.baseline_id           | type == "string" and (. | length) > 0) and
    (.baseline_clean        | type == "boolean") and
    (.profile_id            | type == "string" and (. | length) > 0) and
    (.hotspot_id            | type == "string" and (. | length) > 0) and
    (.lever_id              | type == "string" and (. | length) > 0) and
    (.lever_activated       | type == "boolean") and
    (.score                 | type == "string" and (. | length) > 0) and
    (.proof_id              | type == "string" and (. | length) > 0) and
    (.behavior_preserved    | type == "boolean") and
    (.p99_before            | type == "string" and (. | length) > 0) and
    (.p99_after             | type == "string" and (. | length) > 0) and
    (.gain_pct              | type == "string" and (. | length) > 0) and
    (.reprofile_continued   | type == "boolean") and
    (.rollback_ready        | type == "boolean") and
    (.rollback_rehearsed    | type == "boolean") and
    (.outcome               | type == "string" and (. | length) > 0) and
    (.reproduction_command  | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the round-record contract"
done <"${LEDGER}"
echo "[multi-round-drill-e2e] round ledger lines validated: ${LINE_NO}"

# ── Coverage: exactly three rounds, one per tier ────────────────────────────
ROUNDS="$(jq -s 'length' "${LEDGER}")"
[ "${ROUNDS}" -eq 3 ] || fail "expected 3 rounds, got ${ROUNDS}"

assert_round() {
  # $1 round_number, $2 tier
  local n="$1" tier="$2" got
  got="$(jq -r --argjson n "${n}" 'select(.round_number == $n) | .tier' "${LEDGER}")"
  [ "${got}" = "${tier}" ] || fail "round ${n}: tier '${got}' != '${tier}'"
}
assert_round 1 round1
assert_round 2 round2
assert_round 3 round3

# ── AC1: tier eligibility + transition evidence ─────────────────────────────
INELIGIBLE="$(jq -r 'select(.eligibility != "eligible") | .round_number' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${INELIGIBLE}" -eq 0 ] || fail "found ${INELIGIBLE} rounds that were not eligible"
UNMATCHED="$(jq -r 'select(.active_tier_matched == false) | .round_number' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${UNMATCHED}" -eq 0 ] || fail "found ${UNMATCHED} rounds whose active tier did not match"
# Later rounds must have their prior tiers exhausted (no invalid jumps).
for n in 2 3; do
  jq -e --argjson n "${n}" 'select(.round_number == $n) | .prior_tiers_exhausted == true' "${LEDGER}" >/dev/null \
    || fail "round ${n} proceeded without exhausting prior tiers (invalid jump)"
done

# ── AC2: per-round artifacts + measured gain ────────────────────────────────
for n in 1 2 3; do
  jq -e --argjson n "${n}" 'select(.round_number == $n) | (.baseline_clean == true) and (.behavior_preserved == true) and (.lever_activated == true) and (.reprofile_continued == true)' "${LEDGER}" >/dev/null \
    || fail "round ${n} missing a baseline/proof/lever/reprofile artifact"
  gain="$(jq -r --argjson n "${n}" 'select(.round_number == $n) | .gain_pct' "${LEDGER}")"
  awk -v g="${gain}" 'BEGIN { exit !(g > 0) }' || fail "round ${n} measured no gain (${gain})"
done

# ── AC3: Round 3 rehearses a ready rollback ─────────────────────────────────
jq -e 'select(.round_number == 3) | .rollback_ready == true and .rollback_rehearsed == true' "${LEDGER}" >/dev/null \
  || fail "round 3 did not rehearse a ready rollback"
jq -e 'select(.round_number == 3) | .outcome == "promoted_with_rollback_rehearsal"' "${LEDGER}" >/dev/null \
  || fail "round 3 outcome is not a rollback rehearsal"

# ── Summary gate ────────────────────────────────────────────────────────────
jq -e '.gate_passes == true'              "${SUMMARY}" >/dev/null || fail "gate_passes != true"
jq -e '.tier_progression_valid == true'   "${SUMMARY}" >/dev/null || fail "tier_progression_valid != true"
jq -e '.artifacts_complete == true'       "${SUMMARY}" >/dev/null || fail "artifacts_complete != true"
jq -e '.proofs_present == true'           "${SUMMARY}" >/dev/null || fail "proofs_present != true"
jq -e '.gains_measured == true'          "${SUMMARY}" >/dev/null || fail "gains_measured != true"
jq -e '.rollback_rehearsed == true'      "${SUMMARY}" >/dev/null || fail "rollback_rehearsed != true"
jq -e '.total_rounds == 3'               "${SUMMARY}" >/dev/null || fail "expected 3 rounds"
echo "[multi-round-drill-e2e] summary gate validated"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
echo "[multi-round-drill-e2e] manifest integrity verified"

# ── Determinism: a second run yields a byte-identical ledger ────────────────
RUN2_DIR="${RUN_ROOT}/green2"
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json multi-round-drill \
  --run-root "${RUN_ROOT}" --run-name "green2" \
  >"${RUN_ROOT}/cli_stdout2.json" 2>"${RUN_ROOT}/cli_stderr2.log" || fail "second run failed"
if ! diff -q "${LEDGER}" "${RUN2_DIR}/evidence_ledger.jsonl" >/dev/null; then
  fail "ledger is not deterministic across runs"
fi
echo "[multi-round-drill-e2e] determinism verified (byte-identical ledger)"

# The CLI gate itself must have passed on the green path.
[ "${GATE_EXIT}" -eq 0 ] || fail "multi-round-drill command exited ${GATE_EXIT}"

echo "[multi-round-drill-e2e] PASS — multi-round optimization drill gate green"
exit 0

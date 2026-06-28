#!/usr/bin/env bash
#
# End-to-end value-of-information probe-planner gate (bd-3bxhj.10.24).
#
# Runs the `doctor_frankentui voi-plan` command, which chooses diagnostic probes
# using formal Value-of-Information economics as a deterministic
# estimate -> schedule -> allocate -> account pipeline. The default (all-stage)
# view applies the adaptive-evidence gate: every selected probe must satisfy the
# net-value rule (VOI*value_scale - cost >= 0), every skip carries an explicit
# reason, the adaptive test-budget allocator must schedule runs for the selected
# probes, and any required conservative fallback must be surfaced (never silent).
# The command fails closed on a malformed candidate, an unjustified selection,
# or a silently dropped fallback.
#
# It materializes a JSONL evidence ledger whose every line carries voi, cost,
# net_value, claim_id, policy_id, a decision, run_id, the stage, and a
# reproduction command, plus a voi_cards.json artifact (the VoiCardInput forward
# contract consumed by the galaxy-brain card layer). This wrapper validates the
# artifact contract, the explicit VOI/cost/net terms, determinism, and the green
# gate, making it suitable as a release governance gate.
#
# Usage:
#   ./scripts/doctor_frankentui_voi_plan_e2e.sh [RUN_ROOT]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/voi_plan/${TIMESTAMP_UTC}}"
RUN_NAME="green"
RUN_DIR="${RUN_ROOT}/${RUN_NAME}"
LEDGER="${RUN_DIR}/evidence_ledger.jsonl"
SUMMARY="${RUN_DIR}/pipeline_summary.json"
MANIFEST="${RUN_DIR}/artifact_manifest.json"
CARDS="${RUN_DIR}/voi_cards.json"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[voi-plan-e2e] missing required command: $1" >&2
    exit 2
  fi
}

require_command cargo
require_command jq

mkdir -p "${RUN_ROOT}"

echo "[voi-plan-e2e] run-root: ${RUN_ROOT}"
echo "[voi-plan-e2e] building + running doctor_frankentui voi-plan ..."

# The command exits non-zero if the gate fails; capture so we can still inspect
# the materialized artifacts for triage.
GATE_EXIT=0
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json voi-plan \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" --stage all \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

fail() {
  echo "[voi-plan-e2e] FAIL: $1" >&2
  exit 1
}

# ── Artifact existence ──────────────────────────────────────────────────────
[ -s "${LEDGER}" ]   || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ]  || fail "pipeline summary missing: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing: ${MANIFEST}"
[ -s "${CARDS}" ]    || fail "voi cards artifact missing: ${CARDS}"

# ── Ledger contract: every line carries the AC1/AC3 fields ──────────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id               | type == "string" and (. | length) > 0) and
    (.stage                | type == "string" and (. | length) > 0) and
    (.probe_id             | type == "string" and (. | length) > 0) and
    (.claim_id             | type == "string" and (. | length) > 0) and
    (.policy_id            | type == "string" and (. | length) > 0) and
    (.decision             | type == "string" and (. | length) > 0) and
    (.voi                  | type == "string" and (. | length) > 0) and
    (.cost                 | type == "string" and (. | length) > 0) and
    (.net_value            | type == "string" and (. | length) > 0) and
    (.detail               | type == "string" and (. | length) > 0) and
    (.remediation          | type == "array") and
    (.reproduction_command | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the record contract"
done <"${LEDGER}"
echo "[voi-plan-e2e] ledger lines validated: ${LINE_NO}"

# ── Coverage: all four pipeline stages appear ───────────────────────────────
STAGES="$(jq -r '.stage' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${STAGES}" -eq 4 ] || fail "expected 4 pipeline stages, got ${STAGES}"

# ── AC1: every selected probe satisfies the net-value rule ──────────────────
BAD_SELECT="$(jq -r 'select(.stage == "schedule" and .decision == "select" and ((.net_value | tonumber) < 0)) | .probe_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${BAD_SELECT}" -eq 0 ] || fail "found ${BAD_SELECT} selected probes with net_value < 0"

# ── AC3: every skip carries an explicit reason ──────────────────────────────
BAD_SKIP="$(jq -r 'select(.stage == "schedule" and .decision == "skip" and .skip_reason == "not_skipped") | .probe_id' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${BAD_SKIP}" -eq 0 ] || fail "found ${BAD_SKIP} skipped probes with no reason"

# ── Summary gate ────────────────────────────────────────────────────────────
jq -e '.gate_applies == true'             "${SUMMARY}" >/dev/null || fail "gate_applies != true"
jq -e '.gate_passes == true'              "${SUMMARY}" >/dev/null || fail "gate_passes != true"
jq -e '.required_fields_complete == true' "${SUMMARY}" >/dev/null || fail "required_fields_complete != true"
jq -e '.all_selections_justified == true' "${SUMMARY}" >/dev/null || fail "all_selections_justified != true"
jq -e '.fallback_integrity_ok == true'    "${SUMMARY}" >/dev/null || fail "fallback_integrity_ok != true"
jq -e '.invalid == 0'                     "${SUMMARY}" >/dev/null || fail "unexpected invalid candidates"
jq -e '.selected >= 1'                    "${SUMMARY}" >/dev/null || fail "expected >= 1 selected probe"
# ── AC2: the adaptive test-budget allocator scheduled runs for selected probes
jq -e '.total_runs_allocated >= 1'              "${SUMMARY}" >/dev/null || fail "allocator scheduled no runs"
jq -e '.allocation_voi_at_least_baseline == true' "${SUMMARY}" >/dev/null || fail "voi allocation underperformed round-robin"

SELECTED="$(jq -r '.selected' "${SUMMARY}")"
SKIPPED="$(jq -r '.skipped' "${SUMMARY}")"
RUNS="$(jq -r '.total_runs_allocated' "${SUMMARY}")"
NET="$(jq -r '.total_net_value' "${SUMMARY}")"
echo "[voi-plan-e2e] selected=${SELECTED} skipped=${SKIPPED} runs_allocated=${RUNS} net_value=${NET}"

# ── VoiCardInput forward contract: net = voi*value_scale - cost ─────────────
CARD_VIOLATIONS="$(jq '[.[] | ((.net_value) - (.voi * .value_scale - .cost)) | if (. < 0) then -. else . end | select(. > 1e-6)] | length' "${CARDS}")"
[ "${CARD_VIOLATIONS}" -eq 0 ] || fail "voi_cards violate net = voi*value_scale - cost (${CARD_VIOLATIONS} cards)"
echo "[voi-plan-e2e] voi card forward contract verified"

# ── Manifest integrity: declared sha256 matches the file on disk ────────────
jq -c '.artifacts[]' "${MANIFEST}" | while IFS= read -r artifact; do
  fname="$(echo "${artifact}" | jq -r '.file')"
  declared="$(echo "${artifact}" | jq -r '.sha256')"
  fpath="${RUN_DIR}/${fname}"
  [ -f "${fpath}" ] || fail "manifest artifact missing on disk: ${fname}"
  actual="$(sha256sum "${fpath}" | awk '{print $1}')"
  [ "${declared}" = "${actual}" ] || fail "checksum mismatch for ${fname}"
done
echo "[voi-plan-e2e] manifest integrity verified"

# ── Determinism (AC2): a second run yields a byte-identical ledger ──────────
RUN2_DIR="${RUN_ROOT}/green2"
cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -p doctor_frankentui -- \
  --machine json voi-plan \
  --run-root "${RUN_ROOT}" --run-name "green2" --stage all \
  >"${RUN_ROOT}/cli_stdout2.json" 2>"${RUN_ROOT}/cli_stderr2.log" || fail "second run failed"
if ! diff -q "${LEDGER}" "${RUN2_DIR}/evidence_ledger.jsonl" >/dev/null; then
  fail "ledger is not deterministic across runs"
fi
echo "[voi-plan-e2e] determinism verified (byte-identical ledger)"

# The CLI gate itself must have passed on the green path.
[ "${GATE_EXIT}" -eq 0 ] || fail "voi-plan command exited ${GATE_EXIT}"

echo "[voi-plan-e2e] PASS — voi probe planner gate green"
exit 0

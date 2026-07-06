#!/usr/bin/env bash
# E2E gate for the operator workflow scripts (bd-3bxhj.7.9).
#
# Drives the doctor_frankentui `operator-workflows` subcommand headlessly and
# asserts: the per-span session-log contract (command_span /
# operator_decision / artifact_refs / galaxy_card_ids / recovery guidance +
# reproduction_command), full six-workflow coverage with deterministic
# fixture ordering, red-path diagnostics (failure triage + tamper refusal),
# the fail-closed summary gate, manifest SHA-256 integrity, and
# byte-identical determinism across a second run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-/tmp/doctor_frankentui/operator_workflows/${TIMESTAMP_UTC}}"
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
  --machine json operator-workflows \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}" \
  >"${RUN_ROOT}/cli_stdout.json" 2>"${RUN_ROOT}/cli_stderr.log" || GATE_EXIT=$?

[ -s "${LEDGER}" ] || fail "evidence ledger missing or empty: ${LEDGER}"
[ -s "${SUMMARY}" ] || fail "pipeline summary missing or empty: ${SUMMARY}"
[ -s "${MANIFEST}" ] || fail "artifact manifest missing or empty: ${MANIFEST}"

# ── AC2: every span logs the mandated session facts ──────────────────────────
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO + 1))
  echo "${line}" | jq -e '
    (.run_id               | type == "string" and (. | length) > 0) and
    (.workflow             | type == "string" and (. | length) > 0) and
    (.span_index           | type == "number") and
    (.command_span         | type == "string" and (. | length) > 0) and
    (.operator_decision    | type == "string" and (. | length) > 0) and
    (.artifact_refs        | type == "array" and (. | length) > 0) and
    (.galaxy_card_ids      | type == "array" and (. | length) > 0) and
    (.outcome              | type == "string" and (. | length) > 0) and
    (.red_path             | type == "boolean") and
    (.recovery_guidance    | type == "string") and
    (.detail               | type == "string" and (. | length) > 0) and
    (.reproduction_command | type == "string" and (. | length) > 0)
  ' >/dev/null || fail "ledger line ${LINE_NO} violates the session-log contract"
done <"${LEDGER}"

# ── AC1: six workflows in deterministic order ────────────────────────────────
WORKFLOWS="$(jq -r '.workflow' "${LEDGER}" | sort -u | wc -l | tr -d ' ')"
[ "${WORKFLOWS}" -eq 6 ] || fail "expected 6 distinct workflows, saw ${WORKFLOWS}"

assert_outcome() {
  local workflow="$1" outcome="$2"
  jq -e --arg w "${workflow}" --arg o "${outcome}" \
    'select(.workflow == $w and .outcome == $o)' "${LEDGER}" | grep -q . \
    || fail "workflow ${workflow} did not reach outcome ${outcome}"
}

assert_outcome dry_run dry_run_reviewed
assert_outcome full_migration migration_certified
assert_outcome failure_triage failure_triaged
assert_outcome remediation_rerun remediation_verified
assert_outcome certification_signoff signoff_recorded
assert_outcome certification_signoff signoff_refused_tamper_detected
assert_outcome explainability_audit explainability_reconstructed

# ── AC3: red paths surface diagnostics + recovery guidance ───────────────────
RED_WITHOUT_GUIDANCE="$(jq -r 'select(.red_path == true and (.recovery_guidance | length) == 0)
  | .workflow' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${RED_WITHOUT_GUIDANCE}" -eq 0 ] \
  || fail "${RED_WITHOUT_GUIDANCE} red-path span(s) missing recovery guidance"
RED_COUNT="$(jq -r 'select(.red_path == true) | .workflow' "${LEDGER}" | wc -l | tr -d ' ')"
[ "${RED_COUNT}" -eq 2 ] || fail "expected 2 red-path spans, saw ${RED_COUNT}"

# The audit workflow must log real galaxy-card ids.
jq -e 'select(.workflow == "explainability_audit")
  | .galaxy_card_ids | map(select(. != "n/a")) | length >= 4' "${LEDGER}" \
  | grep -q true || fail "explainability audit did not log galaxy-card ids"

# ── Summary gate booleans (fail-closed) ──────────────────────────────────────
jq -e '
  .gate_passes == true and
  .required_fields_complete == true and
  .all_workflows_expected == true and
  .red_paths_covered == true and
  .decisions_logged == true and
  .audit_cards_logged == true and
  .total_workflows == 6
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
  --machine json operator-workflows \
  --run-root "${RUN_ROOT}" --run-name "${RUN_NAME}2" \
  >"${RUN_ROOT}/cli_stdout_2.json" 2>"${RUN_ROOT}/cli_stderr_2.log" || SECOND_EXIT=$?
[ "${SECOND_EXIT}" -eq 0 ] || fail "second operator-workflows run exited ${SECOND_EXIT}"
diff -q "${LEDGER}" "${RUN_ROOT}/${RUN_NAME}2/evidence_ledger.jsonl" >/dev/null \
  || fail "evidence ledger is not byte-identical across runs"

[ "${GATE_EXIT}" -eq 0 ] || fail "operator-workflows CLI exited ${GATE_EXIT}"

echo "PASS: operator-workflows E2E (${WORKFLOWS} workflows, ${RED_COUNT} red paths, run ${RUN_DIR})"
exit 0

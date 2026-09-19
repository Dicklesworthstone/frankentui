#!/usr/bin/env bash
# Prove `.beads/policy.yaml` is enforced by the installed `br`, in a throwaway
# beads project rather than this repo's tracker (bd-g00-root-epic-ewths.7.2).
#
# `scripts/check_close_evidence.py --self-test` covers the audit's own rules
# against a retained fixture. It cannot cover the half that matters most: that
# `br` actually reads this policy file and refuses the closes it describes. A
# `br` upgrade, a schema rename or a typo in the YAML would all leave the audit
# green while every close sailed through unchecked.
#
# Usage:
#   scripts/check_close_policy_e2e.sh [run-root]
#
# RUN_ROOT defaults under $TMPDIR and is NEVER deleted, by this script or
# anything it calls -- Rule 1 in AGENTS.md covers files the tooling created
# itself, and the run is the evidence. Each invocation gets its own timestamped
# directory, so repeated runs accumulate rather than overwrite.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ROOT="${1:-${TMPDIR:-/tmp}/ftui_close_policy/${TIMESTAMP_UTC}}"
PROJECT_DIR="${RUN_ROOT}/project"
META_DIR="${RUN_ROOT}/meta"
RESULTS_TSV="${META_DIR}/case_results.tsv"
SUMMARY_JSON="${META_DIR}/summary.json"

mkdir -p "${PROJECT_DIR}" "${META_DIR}"

if ! command -v br >/dev/null 2>&1; then
  echo '{"kind":"error","status":"failed","detail":"br not on PATH"}' >&2
  exit 1
fi

PASSED=0
FAILED=0
: > "${RESULTS_TSV}"

# record <case> <expected> <actual> <note>
record() {
  local name="$1" expected="$2" actual="$3" note="${4:-}"
  if [[ "${expected}" == "${actual}" ]]; then
    PASSED=$((PASSED + 1))
    printf '%s\tpass\texpected=%s\tactual=%s\t%s\n' "${name}" "${expected}" "${actual}" "${note}" \
      >> "${RESULTS_TSV}"
  else
    FAILED=$((FAILED + 1))
    printf '%s\tFAIL\texpected=%s\tactual=%s\t%s\n' "${name}" "${expected}" "${actual}" "${note}" \
      >> "${RESULTS_TSV}"
    echo "FAIL ${name}: expected ${expected}, got ${actual} ${note}" >&2
  fi
}

# Outcome of a close attempt: "accepted" or "rejected". Deliberately does not
# inspect the exit code alone -- a `br` that stopped reading the policy would
# still exit 0, and that is precisely the regression this is here to catch.
close_outcome() {
  if br --db "${DB}" close "$1" --reason "$2" >/dev/null 2>"${META_DIR}/last_error.txt"; then
    echo accepted
  else
    echo rejected
  fi
}

cd "${PROJECT_DIR}"
br init --prefix fx >"${META_DIR}/init.log" 2>&1 || true
DB="$(ls "${PROJECT_DIR}"/.beads/*.db 2>/dev/null | head -1 || true)"
if [[ -z "${DB}" ]]; then
  echo '{"kind":"error","status":"failed","detail":"br init produced no database"}' >&2
  exit 1
fi

# The policy under test is this repo's, copied verbatim. Editing a copy here
# would test a fiction.
cp "${ROOT_DIR}/.beads/policy.yaml" "${PROJECT_DIR}/.beads/policy.yaml"

new_bead() {
  br --db "${DB}" create --title "$1" --type task --priority 3 --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d[0] if isinstance(d,list) else d)["id"])'
}

# 1. The disease itself.
ID="$(new_bead 'policy case: bare completed')"
record "rejects_bare_completed" rejected "$(close_outcome "${ID}" 'Completed')"

# 2. Long enough, but nothing a reader can follow.
ID="$(new_bead 'policy case: untyped prose')"
LONG_PROSE="This reason is comfortably past the eighty character minimum but carries no structured reference of any kind."
record "rejects_untyped_prose" rejected "$(close_outcome "${ID}" "${LONG_PROSE}")"

# 3. A reference alone is not an explanation.
ID="$(new_bead 'policy case: bare reference')"
record "rejects_bare_reference" rejected "$(close_outcome "${ID}" 'commit:ab07291f')"

# 4. A kind that is not in required_kinds must not satisfy the gate, or the
#    list is decorative.
ID="$(new_bead 'policy case: unlisted kind')"
UNLISTED="Reworked the presenter seam so the terminal writer is owned in exactly one place. banana:12345"
record "rejects_unlisted_kind" rejected "$(close_outcome "${ID}" "${UNLISTED}")"

# 5. What a good close looks like.
ID="$(new_bead 'policy case: evidence bearing')"
GOOD="Scroll runs now flush one event per notch instead of collapsing to one, so magnitude survives. commit:509710e0"
record "accepts_evidence_bearing" accepted "$(close_outcome "${ID}" "${GOOD}")"

# 6. A hyphenated built-in kind is still a kind.
ID="$(new_bead 'policy case: hyphenated kind')"
HYPHEN="Owner decided to keep the module quarantined rather than wire it into the showcase. agent-mail:1577"
record "accepts_hyphenated_kind" accepted "$(close_outcome "${ID}" "${HYPHEN}")"

# 7. The escape hatch still opens, and requires its own reason.
ID="$(new_bead 'policy case: bypass')"
if br --db "${DB}" close "${ID}" --reason done --bypass-policy \
     --bypass-reason 'e2e: proving the hatch opens' >/dev/null 2>&1; then
  record "bypass_opens_with_reason" accepted accepted
else
  record "bypass_opens_with_reason" accepted rejected
fi

# 8. ...but not without one.
ID="$(new_bead 'policy case: bypass without reason')"
if br --db "${DB}" close "${ID}" --reason done --bypass-policy >/dev/null 2>&1; then
  record "bypass_requires_reason" rejected accepted
else
  record "bypass_requires_reason" rejected rejected
fi

STATUS=passed
[[ ${FAILED} -gt 0 ]] && STATUS=failed

python3 - "$SUMMARY_JSON" "$PASSED" "$FAILED" "$STATUS" "$RUN_ROOT" "$RESULTS_TSV" <<'PY'
import json, sys
path, passed, failed, status, run_root, results = sys.argv[1:7]
json.dump(dict(kind="summary", tool="check_close_policy_e2e", status=status,
               passed=int(passed), failed=int(failed),
               run_root=run_root, case_results=results),
          open(path, "w"), indent=2)
PY

cat "${SUMMARY_JSON}"
echo
echo "cases: ${RESULTS_TSV}"
[[ ${FAILED} -eq 0 ]]

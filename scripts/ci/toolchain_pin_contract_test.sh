#!/usr/bin/env bash
# Verification suite for the FrankenTUI rust-toolchain.toml pin contract
# (bd-g00-root-epic-ewths.6.19, .6.20, .6.29, .6.30).
#
# Retains all isolated scratch fixtures under TMPDIR; never deletes files.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
CONTRACT="${SCRIPT_DIR}/toolchain_pin_contract.sh"
PARSER_TEST="${SCRIPT_DIR}/parse_toolchain_pin_test.sh"
CI_YML="${ROOT_DIR}/.github/workflows/ci.yml"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/ftui-toolchain-pin-contract.XXXXXX")"

pass_count=0
pass() {
    pass_count=$((pass_count + 1))
    echo "PASS [$pass_count]: $1"
}
fail() {
    echo "FAIL: $1" >&2
    exit 1
}

# 1. Run all 18 parser unit test cases
bash "${PARSER_TEST}" > "${SCRATCH}/parser_test.stdout" 2> "${SCRATCH}/parser_test.stderr" \
    || { cat "${SCRATCH}/parser_test.stdout" "${SCRATCH}/parser_test.stderr" >&2; fail "parse_toolchain_pin_test.sh failed"; }
grep -Fq "18 parser cases passed" "${SCRATCH}/parser_test.stdout" \
    || fail "expected 18 parser cases passed"
pass "parser_unit_test_suite_18_cases"

# 2. Positive live workspace pin contract
LIVE_JSON="${SCRATCH}/live_receipt.json"
env -u RUSTUP_TOOLCHAIN bash "${CONTRACT}" --repo-root "${ROOT_DIR}" --check-git-lockfile --out-json "${LIVE_JSON}" \
    > "${SCRATCH}/live.stdout" 2> "${SCRATCH}/live.stderr" \
    || { cat "${SCRATCH}/live.stdout" "${SCRATCH}/live.stderr" >&2; fail "live workspace contract check failed"; }
grep -Fq '"status": "passed"' "${LIVE_JSON}" || fail "live receipt missing passed status"
grep -Fq '"channel": "nightly-2026-08-31"' "${LIVE_JSON}" || fail "live receipt channel mismatch"
LIVE_CACHE_KEY="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["cache_key"])' "${LIVE_JSON}")"
[[ -n "${LIVE_CACHE_KEY}" ]] || fail "empty live cache_key"
pass "positive_live_workspace_contract"

# 3. Negative: missing components = ["rustfmt", "clippy"]
NO_COMP_TOML="${SCRATCH}/no_components.toml"
printf '[toolchain]\nchannel = "nightly-2026-08-31"\ncomponents = ["rustfmt"]\n' > "${NO_COMP_TOML}"
rc=0
env -u RUSTUP_TOOLCHAIN bash "${CONTRACT}" --toolchain-file "${NO_COMP_TOML}" --lockfile "${ROOT_DIR}/Cargo.lock" \
    > "${SCRATCH}/no_comp.stdout" 2> "${SCRATCH}/no_comp.stderr" || rc=$?
[[ "${rc}" -ne 0 ]] || fail "missing clippy component unexpectedly succeeded"
grep -Fq "toolchain.components must include 'rustfmt' and 'clippy'" "${SCRATCH}/no_comp.stderr" \
    || { cat "${SCRATCH}/no_comp.stderr" >&2; fail "unexpected diagnostic for missing components"; }
pass "negative_missing_components_rejected"

# 4. Negative: floating channel = "nightly"
FLOAT_TOML="${SCRATCH}/floating.toml"
printf '[toolchain]\nchannel = "nightly"\ncomponents = ["rustfmt", "clippy"]\n' > "${FLOAT_TOML}"
rc=0
env -u RUSTUP_TOOLCHAIN bash "${CONTRACT}" --toolchain-file "${FLOAT_TOML}" --lockfile "${ROOT_DIR}/Cargo.lock" \
    > "${SCRATCH}/float.stdout" 2> "${SCRATCH}/float.stderr" || rc=$?
[[ "${rc}" -ne 0 ]] || fail "floating channel unexpectedly succeeded"
grep -Fq "floating channel not allowed" "${SCRATCH}/float.stderr" \
    || { cat "${SCRATCH}/float.stderr" >&2; fail "unexpected diagnostic for floating channel"; }
pass "negative_floating_channel_rejected"

# 5. Negative: hostile RUSTUP_TOOLCHAIN environment override
rc=0
RUSTUP_TOOLCHAIN="stable" bash "${CONTRACT}" --repo-root "${ROOT_DIR}" \
    > "${SCRATCH}/env_override.stdout" 2> "${SCRATCH}/env_override.stderr" || rc=$?
[[ "${rc}" -ne 0 ]] || fail "conflicting RUSTUP_TOOLCHAIN=stable unexpectedly succeeded"
grep -Fq "conflicting RUSTUP_TOOLCHAIN override" "${SCRATCH}/env_override.stderr" \
    || { cat "${SCRATCH}/env_override.stderr" >&2; fail "unexpected diagnostic for RUSTUP_TOOLCHAIN override"; }
pass "negative_env_override_rejected"

# 6. Negative: unavailable dated nightly pin fails explicitly (never falls back to default)
UNAVAIL_TOML="${SCRATCH}/unavailable_pin.toml"
printf '[toolchain]\nchannel = "nightly-2099-01-01"\ncomponents = ["rustfmt", "clippy"]\n' > "${UNAVAIL_TOML}"
rc=0
env -u RUSTUP_TOOLCHAIN bash "${CONTRACT}" --toolchain-file "${UNAVAIL_TOML}" --lockfile "${ROOT_DIR}/Cargo.lock" \
    > "${SCRATCH}/unavail.stdout" 2> "${SCRATCH}/unavail.stderr" || rc=$?
[[ "${rc}" -ne 0 ]] || fail "unavailable dated nightly pin unexpectedly succeeded"
grep -Fq "pinned toolchain nightly-2099-01-01 is not installed" "${SCRATCH}/unavail.stderr" \
    || { cat "${SCRATCH}/unavail.stderr" >&2; fail "unexpected diagnostic for unavailable toolchain"; }
pass "negative_unavailable_dated_pin_rejected"

# 7. Cache identity mutation: modifying Cargo.lock in an isolated fixture changes cache_key
MUT_LOCK="${SCRATCH}/Cargo.mutated.lock"
cp "${ROOT_DIR}/Cargo.lock" "${MUT_LOCK}"
printf '\n# synthetic lockfile mutation for cache-key sensitivity test\n' >> "${MUT_LOCK}"
MUT_JSON="${SCRATCH}/mutated_lock_receipt.json"
env -u RUSTUP_TOOLCHAIN bash "${CONTRACT}" --toolchain-file "${ROOT_DIR}/rust-toolchain.toml" \
    --lockfile "${MUT_LOCK}" --out-json "${MUT_JSON}" \
    > "${SCRATCH}/mut_lock.stdout" 2> "${SCRATCH}/mut_lock.stderr" \
    || fail "contract check with mutated lockfile fixture (without --check-git-lockfile) failed"
MUT_CACHE_KEY="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["cache_key"])' "${MUT_JSON}")"
[[ "${MUT_CACHE_KEY}" != "${LIVE_CACHE_KEY}" ]] \
    || fail "cache_key did not change when Cargo.lock changed (${MUT_CACHE_KEY} == ${LIVE_CACHE_KEY})"
pass "cache_key_sensitive_to_lockfile_identity"

# 8. Negative: dirty Cargo.lock in isolated git repository fails --check-git-lockfile
GIT_FIXTURE="${SCRATCH}/git_fixture"
mkdir -p "${GIT_FIXTURE}"
git -C "${GIT_FIXTURE}" init -q
git -C "${GIT_FIXTURE}" config user.email "test@frankentui.local"
git -C "${GIT_FIXTURE}" config user.name "FrankenTUI Test"
cp "${ROOT_DIR}/rust-toolchain.toml" "${GIT_FIXTURE}/rust-toolchain.toml"
cp "${ROOT_DIR}/Cargo.lock" "${GIT_FIXTURE}/Cargo.lock"
git -C "${GIT_FIXTURE}" add rust-toolchain.toml Cargo.lock
git -C "${GIT_FIXTURE}" commit -q -m "initial fixture"
printf '\n# uncommitted drift\n' >> "${GIT_FIXTURE}/Cargo.lock"
rc=0
env -u RUSTUP_TOOLCHAIN bash "${CONTRACT}" --repo-root "${GIT_FIXTURE}" --check-git-lockfile \
    > "${SCRATCH}/dirty_lock.stdout" 2> "${SCRATCH}/dirty_lock.stderr" || rc=$?
[[ "${rc}" -ne 0 ]] || fail "dirty Cargo.lock unexpectedly passed --check-git-lockfile"
grep -Fq "Cargo.lock has uncommitted modifications" "${SCRATCH}/dirty_lock.stderr" \
    || { cat "${SCRATCH}/dirty_lock.stderr" >&2; fail "unexpected diagnostic for dirty Cargo.lock"; }
pass "negative_dirty_git_lockfile_rejected"

# 9. Static check: .github/workflows/ci.yml has toolchain-pin-check and zero MSRV claims
if grep -iq "msrv" "${CI_YML}"; then
    fail ".github/workflows/ci.yml still contains MSRV string"
fi
grep -Fq "toolchain-pin-check:" "${CI_YML}" \
    || fail ".github/workflows/ci.yml missing toolchain-pin-check job"
pass "static_ci_yml_toolchain_pin_check_without_msrv"

echo "${pass_count} toolchain-pin contract checks passed; diagnostics retained in ${SCRATCH}"

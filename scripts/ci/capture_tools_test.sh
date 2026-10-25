#!/usr/bin/env bash
# Verification suite for pinned capture dependencies (VHS 0.10.0 + ttyd 1.7.7)
# and doctor_frankentui snapshot/artifact validation (bd-g00-root-epic-ewths.6.13,
# bd-g00-root-epic-ewths.6.14).
#
# Retains all isolated scratch directories under TMPDIR; never deletes files.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INSTALLER="${ROOT_DIR}/scripts/doctor_frankentui_install_capture_tools.sh"
ACTION_YML="${ROOT_DIR}/.github/actions/install-vhs/action.yml"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/ftui-capture-tools-test.XXXXXX")"

EXPECTED_VHS_VER="0.10.0"
EXPECTED_VHS_ARCHIVE_SHA="b552c3870aca101dcafe533cfef32dceb7b783400ad32642e728775c9f125407"
EXPECTED_VHS_BIN_SHA="7f5c56994afd9beeb207d1bb2e7e5757005639781fa8ed1ff4b7038c640bc949"
EXPECTED_TTYD_VER="1.7.7"
EXPECTED_TTYD_BIN_SHA="8a217c968aba172e0dbf3f34447218dc015bc4d5e59bf51db2f2cd12b7be4f55"

pass_count=0
pass() {
    pass_count=$((pass_count + 1))
    echo "PASS [$pass_count]: $1"
}
fail() {
    echo "FAIL: $1" >&2
    exit 1
}

# 1. Verify .github/actions/install-vhs/action.yml pins VHS 0.10.0 + SHA-256
grep -Fq "default: \"${EXPECTED_VHS_VER}\"" "${ACTION_YML}" \
    || fail "install-vhs action.yml missing default version ${EXPECTED_VHS_VER}"
grep -Fq "default: \"${EXPECTED_VHS_ARCHIVE_SHA}\"" "${ACTION_YML}" \
    || fail "install-vhs action.yml missing default sha256 ${EXPECTED_VHS_ARCHIVE_SHA}"
pass "install_vhs_action_pins_version_and_sha256"

# 2. Positive install + idempotent re-run via doctor_frankentui_install_capture_tools.sh
DEFAULT_PREFIX="${XDG_DATA_HOME:-${HOME}/.local/share}/doctor-frankentui/capture-tools/vhs-${EXPECTED_VHS_VER}-ttyd-${EXPECTED_TTYD_VER}"
DEFAULT_CACHE="${DEFAULT_PREFIX}/downloads"
bash "${INSTALLER}" > "${SCRATCH}/install_pos1.stdout" 2> "${SCRATCH}/install_pos1.stderr" \
    || { cat "${SCRATCH}/install_pos1.stdout" "${SCRATCH}/install_pos1.stderr" >&2; fail "positive capture tools install failed"; }
grep -Fq "vhs version v${EXPECTED_VHS_VER}" "${SCRATCH}/install_pos1.stdout" \
    || fail "unexpected vhs version output"
grep -Fq "ttyd version ${EXPECTED_TTYD_VER}" "${SCRATCH}/install_pos1.stdout" \
    || fail "unexpected ttyd version output"
printf '%s  %s\n' "${EXPECTED_VHS_BIN_SHA}" "${DEFAULT_PREFIX}/bin/vhs" | sha256sum --check --strict - >/dev/null
printf '%s  %s\n' "${EXPECTED_TTYD_BIN_SHA}" "${DEFAULT_PREFIX}/bin/ttyd" | sha256sum --check --strict - >/dev/null
pass "positive_install_and_binary_checksums"

# Re-run against the same prefix must succeed idempotently and preserve hashes
bash "${INSTALLER}" > "${SCRATCH}/install_pos2.stdout" 2> "${SCRATCH}/install_pos2.stderr" \
    || fail "idempotent re-install failed"
printf '%s  %s\n' "${EXPECTED_VHS_BIN_SHA}" "${DEFAULT_PREFIX}/bin/vhs" | sha256sum --check --strict - >/dev/null
printf '%s  %s\n' "${EXPECTED_TTYD_BIN_SHA}" "${DEFAULT_PREFIX}/bin/ttyd" | sha256sum --check --strict - >/dev/null
pass "idempotent_reinstall_preserves_hashes"

# 3. Negative control: corrupted VHS tarball checksum rejected
BAD_VHS_DIR="${SCRATCH}/neg_bad_vhs"
mkdir -p "${BAD_VHS_DIR}/cache"
printf 'corrupted-vhs-tarball-payload\n' > "${BAD_VHS_DIR}/cache/vhs_${EXPECTED_VHS_VER}_Linux_x86_64.tar.gz"
cp "${DEFAULT_CACHE}/ttyd.${EXPECTED_TTYD_VER}.x86_64" "${BAD_VHS_DIR}/cache/ttyd.${EXPECTED_TTYD_VER}.x86_64"
neg_vhs_rc=0
bash "${INSTALLER}" --prefix "${BAD_VHS_DIR}/prefix" --cache-dir "${BAD_VHS_DIR}/cache" \
    > "${BAD_VHS_DIR}/stdout" 2> "${BAD_VHS_DIR}/stderr" || neg_vhs_rc=$?
[[ "${neg_vhs_rc}" -ne 0 ]] || fail "corrupted VHS tarball unexpectedly succeeded"
grep -Fq "WARNING: 1 computed checksum did NOT match" "${BAD_VHS_DIR}/stderr" \
    || { cat "${BAD_VHS_DIR}/stderr" >&2; fail "corrupted VHS stderr missing sha256sum mismatch diagnostic"; }
[[ ! -e "${BAD_VHS_DIR}/prefix/bin/vhs" ]] || fail "corrupted VHS tarball must not install binary"
pass "negative_corrupted_vhs_checksum_rejected"

# 4. Negative control: corrupted ttyd binary checksum rejected
BAD_TTYD_DIR="${SCRATCH}/neg_bad_ttyd"
mkdir -p "${BAD_TTYD_DIR}/cache"
cp "${DEFAULT_CACHE}/vhs_${EXPECTED_VHS_VER}_Linux_x86_64.tar.gz" "${BAD_TTYD_DIR}/cache/vhs_${EXPECTED_VHS_VER}_Linux_x86_64.tar.gz"
printf 'corrupted-ttyd-binary-payload\n' > "${BAD_TTYD_DIR}/cache/ttyd.${EXPECTED_TTYD_VER}.x86_64"
neg_ttyd_rc=0
bash "${INSTALLER}" --prefix "${BAD_TTYD_DIR}/prefix" --cache-dir "${BAD_TTYD_DIR}/cache" \
    > "${BAD_TTYD_DIR}/stdout" 2> "${BAD_TTYD_DIR}/stderr" || neg_ttyd_rc=$?
[[ "${neg_ttyd_rc}" -ne 0 ]] || fail "corrupted ttyd binary unexpectedly succeeded"
grep -Fq "WARNING: 1 computed checksum did NOT match" "${BAD_TTYD_DIR}/stderr" \
    || { cat "${BAD_TTYD_DIR}/stderr" >&2; fail "corrupted ttyd stderr missing sha256sum mismatch diagnostic"; }
[[ ! -e "${BAD_TTYD_DIR}/prefix/bin/ttyd" ]] || fail "corrupted ttyd must not install binary"
pass "negative_corrupted_ttyd_checksum_rejected"

# 5. Negative control: conflicting existing installation refused
CONFLICT_DIR="${SCRATCH}/neg_conflict"
mkdir -p "${CONFLICT_DIR}/prefix/bin"
printf '#!/usr/bin/env bash\necho fake-vhs\n' > "${CONFLICT_DIR}/prefix/bin/vhs"
chmod 0755 "${CONFLICT_DIR}/prefix/bin/vhs"
conflict_rc=0
bash "${INSTALLER}" --prefix "${CONFLICT_DIR}/prefix" --cache-dir "${DEFAULT_CACHE}" \
    > "${CONFLICT_DIR}/stdout" 2> "${CONFLICT_DIR}/stderr" || conflict_rc=$?
[[ "${conflict_rc}" -ne 0 ]] || fail "conflicting existing vhs binary unexpectedly succeeded"
grep -Fq "refusing to replace existing installation" "${CONFLICT_DIR}/stderr" \
    || { cat "${CONFLICT_DIR}/stderr" >&2; fail "missing conflicting installation diagnostic"; }
pass "negative_conflicting_existing_binary_refused"

# 6. FFmpeg PNG decoder positive & negative verification + live doctor snapshot check
if command -v ffmpeg >/dev/null 2>&1; then
    VALID_PNG="${SCRATCH}/valid_decoder_probe.png"
    CORRUPT_PNG="${SCRATCH}/corrupt_decoder_probe.png"
    ffmpeg -nostdin -v error -f lavfi -i "color=c=navy:s=64x32:d=0.1" -frames:v 1 -y "${VALID_PNG}"
    ffmpeg -nostdin -v error -xerror -i "${VALID_PNG}" -f null - \
        || fail "ffmpeg failed to decode valid probe PNG"
    printf '\x89PNG\r\n\x1a\ncorrupt-payload' > "${CORRUPT_PNG}"
    corrupt_png_rc=0
    ffmpeg -nostdin -v error -xerror -i "${CORRUPT_PNG}" -f null - 2> "${SCRATCH}/corrupt_png.stderr" || corrupt_png_rc=$?
    [[ "${corrupt_png_rc}" -ne 0 ]] || fail "ffmpeg unexpectedly accepted corrupt PNG"
    pass "ffmpeg_png_decoder_positive_and_negative_controls"
fi

DOCTOR_ROOT="${DOCTOR_FTUI_ARTIFACT_ROOT:-/tmp/doctor_frankentui_ci}"
if [[ -f "${DOCTOR_ROOT}/happy/doctor/doctor_full_run/snapshot.png" ]]; then
    SNAP="${DOCTOR_ROOT}/happy/doctor/doctor_full_run/snapshot.png"
    [[ -s "${SNAP}" ]] || fail "doctor snapshot.png is empty: ${SNAP}"
    if command -v ffmpeg >/dev/null 2>&1; then
        ffmpeg -nostdin -v error -xerror -i "${SNAP}" -f null - \
            || fail "doctor snapshot.png failed ffmpeg decode: ${SNAP}"
    fi
    python3 -B "${ROOT_DIR}/scripts/doctor_frankentui_validate_jsonl.py" \
        --bundle-root "${DOCTOR_ROOT}" --write-artifact-map > "${SCRATCH}/bundle_validate.json"
    grep -Fq '"status": "passed"' "${SCRATCH}/bundle_validate.json" \
        || { cat "${SCRATCH}/bundle_validate.json" >&2; fail "doctor bundle validation did not pass"; }
    pass "live_doctor_snapshot_and_13_artifact_bundle_verified"
fi

echo "${pass_count} capture-tool verification checks passed; diagnostics retained in ${SCRATCH}"

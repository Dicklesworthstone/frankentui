#!/usr/bin/env bash
# Enforce the FrankenTUI rust-toolchain.toml pin contract for DSR and CI
# (bd-g00-root-epic-ewths.6.19, .6.20, .6.29, .6.30).
#
# Verifies:
# 1. Dated nightly channel via scripts/ci/parse_toolchain_pin.sh.
# 2. Required toolchain components (rustfmt, clippy) in rust-toolchain.toml.
# 3. No conflicting RUSTUP_TOOLCHAIN env override or rustup directory override.
# 4. Pinned toolchain is installed and active rustc -vV / cargo -V match
#    rustc +<channel> -vV / cargo +<channel> -V without PATH shadowing.
# 5. Cargo.lock identity (SHA-256) and optional git diff --exit-code check.
# 6. Composite cache identity (channel + host + rustc -vV hash + Cargo.lock hash)
#    and crate-wide #![feature(...)] inventory.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
PARSER="${SCRIPT_DIR}/parse_toolchain_pin.sh"

TOOLCHAIN_FILE=""
LOCKFILE=""
REPO_ROOT="${ROOT_DIR}"
CHECK_GIT_LOCKFILE=0
OUT_JSON=""

while (( $# )); do
    case "$1" in
        --toolchain-file)
            TOOLCHAIN_FILE="$2"
            shift 2
            ;;
        --lockfile)
            LOCKFILE="$2"
            shift 2
            ;;
        --repo-root)
            REPO_ROOT="$2"
            shift 2
            ;;
        --check-git-lockfile)
            CHECK_GIT_LOCKFILE=1
            shift
            ;;
        --out-json)
            OUT_JSON="$2"
            shift 2
            ;;
        *)
            echo "usage: $0 [--toolchain-file PATH] [--lockfile PATH] [--repo-root DIR] [--check-git-lockfile] [--out-json PATH]" >&2
            exit 2
            ;;
    esac
done

TOOLCHAIN_FILE="${TOOLCHAIN_FILE:-${REPO_ROOT}/rust-toolchain.toml}"
LOCKFILE="${LOCKFILE:-${REPO_ROOT}/Cargo.lock}"

# 1. Parse dated nightly channel
CHANNEL="$(bash "${PARSER}" "${TOOLCHAIN_FILE}")"

# 2. Verify components = ["rustfmt", "clippy"] in rust-toolchain.toml
"${PYTHON_BIN:-python3}" - "${TOOLCHAIN_FILE}" <<'PY'
import pathlib
import sys
import tomllib

path = pathlib.Path(sys.argv[1])
doc = tomllib.loads(path.read_text(encoding="utf-8"))
components = doc.get("toolchain", {}).get("components")
if not isinstance(components, list) or not {"rustfmt", "clippy"}.issubset(set(components)):
    print(
        f"{path}: toolchain.components must include 'rustfmt' and 'clippy' (got {components!r})",
        file=sys.stderr,
    )
    sys.exit(1)
PY

# 3. Detect conflicting environment or directory overrides
if [[ -n "${RUSTUP_TOOLCHAIN:-}" && "${RUSTUP_TOOLCHAIN}" != "${CHANNEL}" && "${RUSTUP_TOOLCHAIN}" != "${CHANNEL}-"* ]]; then
    echo "conflicting RUSTUP_TOOLCHAIN override: expected ${CHANNEL}, got ${RUSTUP_TOOLCHAIN}" >&2
    exit 1
fi

override_line="$(cd "${REPO_ROOT}" && rustup override list 2>/dev/null | awk -v dir="${REPO_ROOT}" '$1 == dir {print $2}' || true)"
if [[ -n "${override_line}" && "${override_line}" != "${CHANNEL}" && "${override_line}" != "${CHANNEL}-"* ]]; then
    echo "conflicting rustup directory override for ${REPO_ROOT}: expected ${CHANNEL}, got ${override_line}" >&2
    exit 1
fi

# 4. Verify the pinned toolchain is installed and matches active compiler/Cargo
export RCH_CARGO_WRAPPER_BYPASS=1
if ! plus_rustc_vv="$(rustc +"${CHANNEL}" -vV 2>&1)"; then
    echo "pinned toolchain ${CHANNEL} is not installed on this host: ${plus_rustc_vv}" >&2
    exit 1
fi
plus_cargo_v="$(cargo +"${CHANNEL}" -V 2>&1)"
host_triple="$(printf '%s\n' "${plus_rustc_vv}" | sed -n 's/^host: //p')"
if [[ -z "${host_triple}" ]]; then
    echo "failed to extract host triple from rustc +${CHANNEL} -vV" >&2
    exit 1
fi

active_toolchain="$(cd "${REPO_ROOT}" && RUSTUP_TOOLCHAIN="${CHANNEL}" rustup show active-toolchain)"
active_token="${active_toolchain%% *}"
if [[ "${active_token}" != "${CHANNEL}-${host_triple}" ]]; then
    echo "active toolchain mismatch: expected ${CHANNEL}-${host_triple}, got ${active_toolchain}" >&2
    exit 1
fi

env_rustc_vv="$(cd "${REPO_ROOT}" && RUSTUP_TOOLCHAIN="${CHANNEL}" rustc -vV)"
env_cargo_v="$(cd "${REPO_ROOT}" && RUSTUP_TOOLCHAIN="${CHANNEL}" cargo -V)"
if [[ "${env_rustc_vv}" != "${plus_rustc_vv}" || "${env_cargo_v}" != "${plus_cargo_v}" ]]; then
    echo "active compiler or Cargo differs from rustc/cargo +${CHANNEL}" >&2
    exit 1
fi

# 5. Lockfile verification and composite cache identity
if [[ ! -f "${LOCKFILE}" ]]; then
    echo "missing lockfile: ${LOCKFILE}" >&2
    exit 1
fi
if [[ "${CHECK_GIT_LOCKFILE}" -eq 1 ]]; then
    if ! git -C "${REPO_ROOT}" diff --exit-code -- "${LOCKFILE}" >/dev/null; then
        echo "Cargo.lock has uncommitted modifications in ${REPO_ROOT}" >&2
        exit 1
    fi
fi

toolchain_sha="$(sha256sum "${TOOLCHAIN_FILE}" | awk '{print $1}')"
lockfile_sha="$(sha256sum "${LOCKFILE}" | awk '{print $1}')"
compiler_sha="$(printf '%s\n%s\n' "${env_rustc_vv}" "${env_cargo_v}" | sha256sum | awk '{print $1}')"
cache_key="${CHANNEL}-${host_triple}-${compiler_sha:0:12}-${lockfile_sha:0:12}"

# 6. Nightly feature inventory
feature_lines=()
if [[ -d "${REPO_ROOT}/crates" ]]; then
    while IFS= read -r line; do
        [[ -n "${line}" ]] && feature_lines+=("${line}")
    done < <(grep -rn --include='*.rs' '#!\[feature(' "${REPO_ROOT}/crates" 2>/dev/null | sort || true)
fi

rustc_short="$(printf '%s\n' "${env_rustc_vv}" | head -n 1)"
echo "channel: ${CHANNEL}"
echo "host: ${host_triple}"
echo "active_toolchain: ${active_toolchain}"
echo "rustc: ${rustc_short}"
echo "cargo: ${env_cargo_v}"
echo "toolchain_file_sha256: ${toolchain_sha}"
echo "lockfile_sha256: ${lockfile_sha}"
echo "compiler_identity_sha256: ${compiler_sha}"
echo "cache_key: ${cache_key}"
echo "nightly_feature_count: ${#feature_lines[@]}"

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
        echo "### Toolchain Pin Check"
        echo "- **channel**: \`${CHANNEL}\`"
        echo "- **active_toolchain**: \`${active_toolchain}\`"
        echo "- **rustc**: \`${rustc_short}\`"
        echo "- **cargo**: \`${env_cargo_v}\`"
        echo "- **lockfile_sha256**: \`${lockfile_sha}\`"
        echo "- **cache_key**: \`${cache_key}\`"
        echo "- **nightly_feature_count**: \`${#feature_lines[@]}\`"
    } >> "${GITHUB_STEP_SUMMARY}"
fi

if [[ -n "${OUT_JSON}" ]]; then
    "${PYTHON_BIN:-python3}" - "${OUT_JSON}" "${CHANNEL}" "${host_triple}" "${active_toolchain}" \
        "${rustc_short}" "${env_cargo_v}" "${toolchain_sha}" "${lockfile_sha}" "${compiler_sha}" \
        "${cache_key}" "${#feature_lines[@]}" <<'PY'
import json
import pathlib
import sys

out = pathlib.Path(sys.argv[1])
out.parent.mkdir(parents=True, exist_ok=True)
payload = {
    "status": "passed",
    "channel": sys.argv[2],
    "host": sys.argv[3],
    "active_toolchain": sys.argv[4],
    "rustc": sys.argv[5],
    "cargo": sys.argv[6],
    "toolchain_file_sha256": sys.argv[7],
    "lockfile_sha256": sys.argv[8],
    "compiler_identity_sha256": sys.argv[9],
    "cache_key": sys.argv[10],
    "nightly_feature_count": int(sys.argv[11]),
}
out.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
PY
fi

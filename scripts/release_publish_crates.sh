#!/usr/bin/env bash
# Run on a native DSR host with the repository's pinned Rust toolchain.
# Registry inventory, candidate dry runs, and publication are distinct results.
# All logs, registry responses, and Cargo package archives are retained.
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
    echo "Usage: $0 VERSION NEW_OUTPUT_DIR [--dry-run|--registry-only|--publish]" >&2
    exit 2
fi
version="${1#v}"
out="$2"
mode="${3:---dry-run}"
case "$mode" in
    --dry-run|--registry-only|--publish) ;;
    *) echo "Unknown mode: $mode" >&2; exit 2 ;;
esac
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Expected a stable X.Y.Z version' >&2; exit 2; }
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
out="$(python3 -I -c 'from pathlib import Path; import sys; print(Path(sys.argv[1]).resolve())' "$out")"
case "$out/" in
    "$root/"*) echo 'Output must be outside the source checkout' >&2; exit 2 ;;
esac
[[ ! -e "$out" ]] || { echo "Output directory already exists: $out" >&2; exit 2; }
mkdir "$out"
export CARGO_CACHE_AUTO_CLEAN_FREQUENCY=never
export CARGO_HTTP_USER_AGENT='OpenAI File Downloader, XaiImageApiFetch/1.0'
source_commit="$(git rev-parse HEAD)"
git status --porcelain=v1 --untracked-files=normal > "$out/source-status.txt"
if [[ -s "$out/source-status.txt" ]]; then
    echo "Use a clean, isolated candidate checkout; see $out/source-status.txt" >&2
    exit 1
fi

python3 -I - "$root" "$version" > "$out/plan.json" <<'PY'
import json
import sys
import tomllib
from pathlib import Path

root = Path(sys.argv[1])
version = sys.argv[2]
workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
assert workspace["package"]["version"] == version, "workspace version mismatch"
packages = {}
for member in workspace["members"]:
    manifest = tomllib.loads((root / member / "Cargo.toml").read_text())
    package = manifest["package"]
    if package.get("publish") is False:
        continue
    actual = package["version"]
    if actual == {"workspace": True}:
        actual = workspace["package"]["version"]
    assert actual == version, f"version mismatch: {package['name']}"
    packages[package["name"]] = manifest

dependencies = {}
for name, manifest in packages.items():
    tables = [manifest] + list(manifest.get("target", {}).values())
    internal = set()
    for table in tables:
        for kind in ("dependencies", "build-dependencies", "dev-dependencies"):
            for alias, dependency in table.get(kind, {}).items():
                if not isinstance(dependency, dict):
                    continue
                # Cargo strips path-only development dependencies when packaging.
                # Versioned ones remain in the normalized manifest and lockfile.
                if kind == "dev-dependencies" and "version" not in dependency:
                    continue
                target = dependency.get("package", alias)
                if target in packages:
                    assert dependency.get("version") == version, f"dependency version mismatch: {name} -> {target}"
                    internal.add(target)
    dependencies[name] = internal

order = []
while len(order) < len(packages):
    ready = sorted(name for name, deps in dependencies.items() if name not in order and deps <= set(order))
    assert ready, "cycle in publishable dependencies"
    order.extend(ready)
assert order, "no publishable crates"
print(json.dumps({"version": version, "crates": order}, indent=2))
PY

pin="$(python3 -I -c 'import tomllib; print(tomllib.load(open("rust-toolchain.toml", "rb"))["toolchain"]["channel"])')"
export RUSTUP_TOOLCHAIN="$pin"
rustc -Vv > "$out/rustc.txt"
cargo --version > "$out/cargo.txt"
sha256sum Cargo.lock > "$out/cargo-lock.sha256"
target_dir="$(cargo metadata --locked --no-deps --format-version=1 | jq -er '.target_directory')"
results="$out/release_log.jsonl"
touch "$results"

record() {
    local crate="$1" status="$2" detail="$3" checksum="${4:-}"
    jq -nc --arg crate "$crate" --arg version "$version" --arg status "$status" \
        --arg detail "$detail" --arg source_commit "$source_commit" --arg toolchain "$pin" \
        --arg checksum "$checksum" --argjson elapsed_s "$((SECONDS - started))" \
        '{crate:$crate,version:$version,status:$status,detail:$detail,source_commit:$source_commit,toolchain:$toolchain,checksum:$checksum,elapsed_s:$elapsed_s}' >> "$results"
    printf '%s | %s | %s | %s\n' "$crate" "$version" "$status" "$detail"
}

query_registry() {
    local crate="$1" response="$2" code
    code="$(curl -sS -L --retry 3 --connect-timeout 15 --max-time 60 \
        -A "$CARGO_HTTP_USER_AGENT" -o "$response" -w '%{http_code}' \
        "https://crates.io/api/v1/crates/$crate/$version")" || return 1
    case "$code" in
        200)
            jq -e --arg crate "$crate" --arg version "$version" \
                '.version | .crate == $crate and .num == $version and .yanked == false and (.checksum | test("^[0-9a-f]{64}$"))' \
                "$response" > /dev/null || return 1
            return 0 ;;
        404) return 4 ;;
        *) echo "Registry returned HTTP $code for $crate" >&2; return 1 ;;
    esac
}

verify_archive() {
    local crate="$1" response="$2" archive="$target_dir/package/$1-$version.crate"
    [[ -f "$archive" ]] || { echo "Missing candidate archive: $archive" >&2; return 1; }
    local expected actual
    expected="$(jq -er '.version.checksum' "$response")"
    actual="$(sha256sum "$archive" | cut -d ' ' -f 1)"
    [[ "$actual" == "$expected" ]] || { echo "Registry archive differs from candidate: $crate" >&2; return 1; }
    python3 -I - "$archive" "$crate" "$version" "$source_commit" <<'PY'
import json
import sys
import tarfile
import tomllib

archive, crate, version, commit = sys.argv[1:]
with tarfile.open(archive, "r:gz") as package:
    prefix = f"{crate}-{version}/"
    vcs = json.load(package.extractfile(prefix + ".cargo_vcs_info.json"))
    manifest = tomllib.loads(package.extractfile(prefix + "Cargo.toml").read().decode())
assert vcs["git"]["sha1"] == commit, "candidate archive comes from a different commit"
assert vcs["git"].get("dirty", False) is False, "candidate archive contains uncommitted source"
assert vcs["path_in_vcs"] == f"crates/{crate}", "candidate archive has the wrong source path"
assert manifest["package"]["name"] == crate, "candidate archive name mismatch"
assert manifest["package"]["version"] == version, "candidate archive version mismatch"
PY
}

while IFS= read -r crate; do
    started=$SECONDS
    if [[ "$(git rev-parse HEAD)" != "$source_commit" ]] || [[ -n "$(git status --porcelain=v1 --untracked-files=normal)" ]]; then
        record "$crate" failed 'Source changed after release planning'
        exit 1
    fi
    response="$out/$crate.registry-before.json"
    registry_status=0
    query_registry "$crate" "$response" || registry_status=$?
    if [[ "$registry_status" != 0 && "$registry_status" != 4 ]]; then
        record "$crate" failed 'Registry identity query failed'
        exit 1
    fi
    if [[ "$mode" == --registry-only ]]; then
        if [[ "$registry_status" == 0 ]]; then
            record "$crate" registry-present 'Registry inventory only; candidate bytes not verified' "$(jq -r '.version.checksum' "$response")"
            continue
        fi
        record "$crate" failed 'Version is absent from registry'
        exit 1
    fi
    if [[ "$mode" == --publish && "$registry_status" == 0 ]]; then
        if ! verify_archive "$crate" "$response"; then
            record "$crate" failed 'Existing version does not have a matching retained candidate archive'
            exit 1
        fi
        record "$crate" already-published 'Retained candidate archive matches registry checksum' "$(jq -r '.version.checksum' "$response")"
        continue
    fi
    args=(publish --locked --registry crates-io -p "$crate")
    [[ "$mode" == --dry-run ]] && args+=(--dry-run)
    if ! cargo "${args[@]}" > "$out/$crate.cargo.log" 2>&1; then
        record "$crate" failed "Cargo failed; see $crate.cargo.log (unpublished sibling dependencies can block dry runs)"
        exit 1
    fi
    if [[ "$mode" == --dry-run ]]; then
        record "$crate" dry-run-ok 'Cargo package verification passed; nothing published'
        continue
    fi
    observed=false
    for attempt in {1..24}; do
        response="$out/$crate.registry-after-$attempt.json"
        registry_status=0
        query_registry "$crate" "$response" || registry_status=$?
        if [[ "$registry_status" == 0 ]]; then
            observed=true
            break
        fi
        [[ "$registry_status" == 4 ]] || break
        sleep 5
    done
    if [[ "$observed" != true ]] || ! verify_archive "$crate" "$response"; then
        record "$crate" failed 'Publication was not confirmed with the candidate archive checksum'
        exit 1
    fi
    record "$crate" published 'Registry checksum matches the actual Cargo archive' "$(jq -r '.version.checksum' "$response")"
done < <(jq -r '.crates[]' "$out/plan.json")

sha256sum --check "$out/cargo-lock.sha256"
if [[ "$(git rev-parse HEAD)" != "$source_commit" ]] || ! git diff --quiet HEAD --; then
    echo 'Source changed during release' >&2
    exit 1
fi
git status --porcelain=v1 --untracked-files=normal > "$out/source-status-after.txt"
cmp "$out/source-status.txt" "$out/source-status-after.txt"
jq -e -s --argjson expected "$(jq '.crates | length' "$out/plan.json")" \
    'length == $expected and all(.status != "failed")' "$results" > /dev/null

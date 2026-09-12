#!/usr/bin/env bash
# Build a complete browser showcase inside a native DSR job.
# The first-party renderer comes from its last source change before removal
# from this workspace. Its source, dependency lock and license are retained.
# No source manifest edits, wasm-pack cleanup, or reuse of stale pkg files.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
if [[ $# != 1 || $1 != /* ]]; then
  fail 'usage: bash build-wasm.sh /absolute/NEW_OUTPUT_DIR (run through DSR)'
fi
output=$1
[[ ! -e "$output" && ! -L "$output" ]] || fail "output already exists: $output"
output=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$output")
case "$output/" in "$SCRIPT_DIR/"*) fail 'output must be outside the checkout' ;; esac
[[ -z ${FRANKENTERM_WEB_CRATE_DIR:-} ]] || fail 'unverified renderer overrides are unsupported'
for tool in cargo rustc wasm-bindgen python3 curl tar; do
  command -v "$tool" >/dev/null || fail "missing tool: $tool"
done
[[ -f Cargo.lock ]] || fail 'Cargo.lock is required; retain the DSR candidate lock'
[[ -f crates/ftui-showcase-wasm/renderer.lock ]] || fail 'renderer dependency lock is missing'

# The current repository pin governs BOTH builds, including the archived tree.
export RUSTUP_TOOLCHAIN
RUSTUP_TOOLCHAIN=$(python3 -c 'import tomllib; print(tomllib.load(open("rust-toolchain.toml", "rb"))["toolchain"]["channel"])')
[[ "$RUSTUP_TOOLCHAIN" == nightly-????-??-?? ]] || fail 'toolchain must be pinned by date'
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS
export CARGO_CACHE_AUTO_CLEAN_FREQUENCY=never
export CARGO_BUILD_FINGERPRINT=content
export CARGO_HTTP_USER_AGENT='OpenAI File Downloader, XaiImageApiFetch/1.0'

renderer_revision=88b402b8be9c70a4405895d4172e449940cab2fe
renderer_archive_sha256=46e23154a20e22672465e406f5f79f5b9e800198c8f655e073d0e79ad0d05306
mkdir "$output"
output=$(cd "$output" && pwd)
mkdir "$output/renderer-source" "$output/site" "$output/site/pkg" "$output/site/assets" "$output/site/fonts"
rustc -Vv > "$output/toolchain.txt"
wasm-bindgen --version > "$output/wasm-bindgen.txt"

# Require the CLI schema to match both dependency locks before compilation.
python3 - "$output/wasm-bindgen.txt" <<'PY'
import pathlib, sys, tomllib
cli = pathlib.Path(sys.argv[1]).read_text().strip().split()[-1]
for name in ('Cargo.lock', 'crates/ftui-showcase-wasm/renderer.lock'):
    packages = tomllib.loads(pathlib.Path(name).read_text())['package']
    versions = {p['version'] for p in packages if p['name'] == 'wasm-bindgen'}
    if versions != {cli}:
        raise SystemExit(f'{name}: wasm-bindgen CLI {cli} does not match {sorted(versions)}')
PY

curl --fail --location --user-agent "$CARGO_HTTP_USER_AGENT" \
  --output "$output/renderer.tar.gz" \
  "https://codeload.github.com/Dicklesworthstone/frankentui/tar.gz/$renderer_revision"
python3 - "$output/renderer.tar.gz" "$renderer_archive_sha256" <<'PY'
import hashlib, pathlib, sys
actual = hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest()
if actual != sys.argv[2]:
    raise SystemExit(f'renderer source checksum mismatch: {actual}')
PY
tar -xz --strip-components=1 -f "$output/renderer.tar.gz" -C "$output/renderer-source"
[[ ! -e "$output/renderer-source/Cargo.lock" ]] || fail 'unexpected lock in pinned source archive'
cp crates/ftui-showcase-wasm/renderer.lock "$output/renderer-source/Cargo.lock"

build_package() {
  local source=$1 package=$2 out_name=$3 target_dir
  # The packages have distinct output names; cache selection is owned by DSR.
  target_dir=${CARGO_TARGET_DIR:-$output/target}
  [[ "$target_dir" == /* ]] || fail 'CARGO_TARGET_DIR must be absolute'
  (
    cd "$source"
    cargo -Zchecksum-freshness --config 'profile.release.package.ftui-extras.opt-level="z"' \
      build --locked --release --lib --target wasm32-unknown-unknown \
      --target-dir "$target_dir" -p "$package"
  )
  wasm-bindgen --target web --out-name "$out_name" --out-dir "$output/site/pkg" \
    "$target_dir/wasm32-unknown-unknown/release/${package//-/_}.wasm"
}
build_package "$output/renderer-source" frankenterm-web FrankenTerm
build_package "$SCRIPT_DIR" ftui-showcase-wasm ftui_showcase_wasm
cp crates/ftui-showcase-wasm/frankentui_showcase_demo.html "$output/site/index.html"
cp crates/ftui-demo-showcase/data/shakespeare.txt crates/ftui-demo-showcase/data/sqlite3.c "$output/site/assets/"
cp fonts/pragmasevka-nf-subset.woff2 "$output/site/fonts/"
cp "$output/renderer-source/LICENSE" "$output/site/RENDERER-LICENSE"
cp LICENSE "$output/site/LICENSE"

# This manifest is consumed by the host's integrity loader before it executes
# either module. It also binds the package to exact source/dependency inputs.
python3 - "$output" "$renderer_revision" "$renderer_archive_sha256" <<'PY'
import hashlib, json, os, pathlib, sys
root = pathlib.Path(sys.argv[1])
digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
files = {p.name: digest(p) for p in sorted((root / 'site/pkg').iterdir()) if p.suffix in ('.js', '.wasm')}
inputs = {p.as_posix(): digest(p) for p in sorted(pathlib.Path('crates').rglob('*'))
          if p.is_file() and p.suffix in ('.rs', '.toml')}
for name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config.toml', 'build-wasm.sh',
             'crates/ftui-showcase-wasm/frankentui_showcase_demo.html',
             'crates/ftui-showcase-wasm/renderer.lock', 'fonts/pragmasevka-nf-subset.woff2',
             'crates/ftui-demo-showcase/data/shakespeare.txt', 'crates/ftui-demo-showcase/data/sqlite3.c'):
    inputs[name] = digest(pathlib.Path(name))
source_bytes = json.dumps(inputs, sort_keys=True, separators=(',', ':')).encode()
with (root / 'source-inputs.json').open('xb') as out:
    out.write(source_bytes)
manifest = {
    'schema': 'ftui-browser-package-v1', 'toolchain': os.environ['RUSTUP_TOOLCHAIN'],
    'renderer': {'revision': sys.argv[2], 'archive_sha256': sys.argv[3],
                 'lock_sha256': digest(pathlib.Path('crates/ftui-showcase-wasm/renderer.lock')),
                 'license': 'MIT License (with OpenAI/Anthropic Rider)'},
    'source_inputs_sha256': hashlib.sha256(source_bytes).hexdigest(),
    'runner_lock_sha256': digest(pathlib.Path('Cargo.lock')), 'files': files,
}
with (root / 'site/pkg/manifest.json').open('x') as out:
    json.dump(manifest, out, indent=2)
    out.write('\n')
for name, sha in files.items():
    print(f'{sha}  pkg/{name}')
PY
printf 'Serve the completed bundle: python3 -m http.server --directory %q 8080\n' "$output/site"

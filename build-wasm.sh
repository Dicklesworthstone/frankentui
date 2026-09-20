#!/usr/bin/env bash
# Build a complete browser showcase inside a native DSR job.
# The first-party renderer comes from its last source change before removal
# from this workspace. Its source, dependency lock and license are retained.
# No source manifest edits, wasm-pack cleanup, or reuse of stale pkg files.
set -euo pipefail

fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

check_artifact() {
  command -v node >/dev/null || fail 'missing tool: node (required for artifact validation)'
  node --input-type=module - "$1" "$2" <<'JS'
import {readFileSync, realpathSync, statSync} from 'node:fs';
import {createHash} from 'node:crypto';

const [artifact, limit] = process.argv.slice(2);
try {
  if (!/^[0-9]+$/.test(limit) || BigInt(limit) === 0n) {
    throw new Error('MAX_BYTES must be a positive decimal integer');
  }
  const budget = BigInt(limit);
  const path = realpathSync(artifact);
  const stat = statSync(path);
  if (!stat.isFile()) throw new Error(`artifact is not a regular file: ${path}`);
  if (stat.size === 0) throw new Error(`artifact is empty: ${path}`);
  if (BigInt(stat.size) > budget) {
    throw new Error(`artifact exceeds budget: ${stat.size} bytes > ${budget} bytes (${path})`);
  }
  const data = readFileSync(path);
  if (data.length === 0) throw new Error(`artifact is empty: ${path}`);
  if (BigInt(data.length) > budget) {
    throw new Error(`artifact exceeds budget: ${data.length} bytes > ${budget} bytes (${path})`);
  }
  // Compilation validates the entire binary without instantiating or running it.
  const exports = WebAssembly.Module.exports(new WebAssembly.Module(data));
  // wasm.rs: class names are lowercased; method js_name spelling is retained.
  // The raw cdylib appends a per-module 16-hex suffix to every wasm-bindgen
  // export (e.g. showcaserunner_new_c999be23f7b1c7cd); the transformed artifact
  // keeps the bare name. Both spellings are accepted, anchored.
  const required = [
    'showcaserunner_new',
    'showcaserunner_init',
    'showcaserunner_step',
    'showcaserunner_pushEncodedInput',
    'showcaserunner_takeFlatPatches',
  ];
  const functions = new Set(exports.filter(entry => entry.kind === 'function').map(entry => entry.name));
  const missing = required.filter(name =>
    ![...functions].some(exported => new RegExp(`^${name}(_[0-9a-f]{16})?$`).test(exported)));
  if (missing.length) throw new Error(`missing ShowcaseRunner function exports: ${missing.join(', ')}`);
  console.log(JSON.stringify({
    event: 'wasm_artifact_check',
    crate: 'ftui-showcase-wasm',
    artifact: path,
    bytes: data.length,
    budget: budget.toString(),
    target: 'wasm32-unknown-unknown',
    profile: 'release',
    sha256: createHash('sha256').update(data).digest('hex'),
    exports,
  }));
} catch (error) {
  console.error(`ERROR: WASM artifact check (${artifact}): ${error.message}`);
  process.exitCode = 1;
}
JS
}

if [[ ${1:-} == --check-artifact ]]; then
  [[ $# == 3 ]] || fail 'usage: bash build-wasm.sh --check-artifact WASM MAX_BYTES'
  check_artifact "$2" "$3"
  exit 0
fi
if [[ $# != 1 || ( $1 != --check-features && $1 != /* ) ]]; then
  fail 'usage: bash build-wasm.sh /absolute/NEW_OUTPUT_DIR | --check-features | --check-artifact WASM MAX_BYTES (run builds through DSR)'
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Keep browser-supported native defaults explicit. This catches the observed
# 43/45-screen build when screen-mermaid was omitted from the WASM dependency.
# Retire this check if both packages consume one shared feature declaration.
python3 -B - <<'PY'
import tomllib
from pathlib import Path
native = tomllib.loads(Path('crates/ftui-demo-showcase/Cargo.toml').read_text())
web = tomllib.loads(Path('crates/ftui-showcase-wasm/Cargo.toml').read_text())
dependency = web['dependencies']['ftui-demo-showcase']
backends = {'native-backend', 'crossterm-compat'}
expected = set(native['features']['default']) - backends
actual = set(dependency.get('features', []))
if dependency.get('default-features', True):
    raise SystemExit('browser feature parity: disable native defaults explicitly')
missing = expected - actual
forbidden = actual & (backends | {'default'})
if missing or forbidden:
    raise SystemExit(f'browser feature parity: missing={sorted(missing)}, native-only={sorted(forbidden)}')
print(f'browser feature parity: {len(expected)} required features present ({", ".join(sorted(expected))})')
PY
if [[ $1 == --check-features ]]; then
  exit 0
fi
output=$1
[[ ! -e "$output" && ! -L "$output" ]] || fail "output already exists: $output"
output=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$output")
case "$output/" in "$SCRIPT_DIR/"*) fail 'output must be outside the checkout' ;; esac
[[ -z ${FRANKENTERM_WEB_CRATE_DIR:-} ]] || fail 'unverified renderer overrides are unsupported'
for tool in cargo rustc wasm-bindgen python3 curl tar node; do
  command -v "$tool" >/dev/null || fail "missing tool: $tool"
done
[[ -f Cargo.lock ]] || fail 'Cargo.lock is required; retain the DSR candidate lock'
[[ -f crates/ftui-showcase-wasm/renderer.lock ]] || fail 'renderer dependency lock is missing'
[[ -f crates/ftui-showcase-wasm/accessibility.mjs ]] || fail 'browser accessibility bridge is missing'

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
mkdir "$output/renderer-source" "$output/site" "$output/site/pkg" "$output/site/assets" "$output/site/fonts" "$output/raw"
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
  local source=$1 package=$2 out_name=$3 target_dir wasm
  # The packages have distinct output names; cache selection is owned by DSR.
  target_dir=${CARGO_TARGET_DIR:-$output/target}
  [[ "$target_dir" == /* ]] || fail 'CARGO_TARGET_DIR must be absolute'
  (
    cd "$source"
    cargo -Zchecksum-freshness --config 'profile.release.package.ftui-extras.opt-level="z"' \
      build --locked --release --lib --target wasm32-unknown-unknown \
      --target-dir "$target_dir" -p "$package"
  )
  wasm="$target_dir/wasm32-unknown-unknown/release/${package//-/_}.wasm"
  if [[ "$package" == ftui-showcase-wasm ]]; then
    # Keep the exact cdylib checked by the budget gate, before bindgen rewrites it.
    cp "$wasm" "$output/raw/ftui_showcase_wasm.wasm"
    wasm="$output/raw/ftui_showcase_wasm.wasm"
    # Advisory baseline: 5,143,294 bytes measured with this recipe; ceil(1.25x).
    check_artifact "$wasm" "${FTUI_WASM_MAX_BYTES-6429118}" \
      > "$output/raw/ftui_showcase_wasm.observation.json"
  fi
  wasm-bindgen --target web --out-name "$out_name" --out-dir "$output/site/pkg" \
    "$wasm"
}
build_package "$output/renderer-source" frankenterm-web FrankenTerm
build_package "$SCRIPT_DIR" ftui-showcase-wasm ftui_showcase_wasm

# Package the import-free bridge inside the same verified module as the runner.
# The host imports checked bytes through a Blob URL: a relative snippet import
# would both fail resolution and escape the existing per-file integrity check.
# Append only to newly generated output, never rewrite generated methods or
# source files. The subclass preserves the exported constructor's API.
printf '\n' >> "$output/site/pkg/ftui_showcase_wasm.js"
cat crates/ftui-showcase-wasm/accessibility.mjs >> "$output/site/pkg/ftui_showcase_wasm.js"
printf '\nShowcaseRunner = withShowcaseAccessibility(ShowcaseRunner);\n' \
  >> "$output/site/pkg/ftui_showcase_wasm.js"

# Run the native binary with its own default feature graph, separately from
# the WASM graph. Compare names AND order, not a hard-coded expected count.
cargo -Zchecksum-freshness run --locked -p ftui-demo-showcase -- --list-screens \
  > "$output/native-screen-slugs.json"
node --input-type=module - "$output" <<'JS'
import assert from 'node:assert/strict';
import {readFile, writeFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
const root = process.argv[2];
const pkg = `${root}/site/pkg`;
const {initSync, ShowcaseRunner} = await import(pathToFileURL(`${pkg}/ftui_showcase_wasm.js`));
initSync({module: await readFile(`${pkg}/ftui_showcase_wasm_bg.wasm`)});
const runner = new ShowcaseRunner(80, 24);
const actual = runner.screenSlugs();
const expected = JSON.parse(await readFile(`${root}/native-screen-slugs.json`, 'utf8'));
assert.ok(expected.length > 0, 'native screen registry must not be empty');
assert.equal(new Set(expected).size, expected.length, 'native slugs must be unique');
assert.deepEqual(actual, expected, 'native/WASM ordered screen registries differ');
await writeFile(`${root}/wasm-screen-slugs.json`, JSON.stringify(actual) + '\n', {flag: 'wx'});
console.log(`native/WASM registry parity: ${actual.length} ordered screen slugs match`);
// Exercise the real compiled exports, not only the JavaScript adapter. Node
// has no reserved DOM proxy, so explicit configuration chooses manual delivery.
runner.setAccessibilityEnabled(true);
runner.init();
const first = JSON.parse(runner.takeAccessibilityUpdateJson());
assert.equal(first.schema_version, 1);
assert.equal(first.enabled, true);
assert.equal(first.frame_id, '0');
assert.ok(first.lines.length > 0 && first.lines.length <= 128);
assert.ok(first.announcements.length <= 8);
const drained = JSON.parse(runner.takeAccessibilityUpdateJson());
assert.deepEqual(drained.lines, first.lines);
assert.equal(drained.announcements.length, 0);
assert.equal(drained.dropped_count, 0);
runner.destroy();
assert.equal(JSON.parse(runner.takeAccessibilityUpdateJson()).enabled, false);
runner.free();
console.log('compiled browser accessibility transport: init, bounded mirror, drain, cleanup passed');
JS
cp crates/ftui-showcase-wasm/frankentui_showcase_demo.html "$output/site/index.html"
cp crates/ftui-demo-showcase/data/shakespeare.txt crates/ftui-demo-showcase/data/sqlite3.c \
  crates/ftui-demo-showcase/data/evidence.jsonl "$output/site/assets/"
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
             'crates/ftui-showcase-wasm/accessibility.mjs',
             'crates/ftui-showcase-wasm/renderer.lock', 'fonts/pragmasevka-nf-subset.woff2',
             'crates/ftui-demo-showcase/data/shakespeare.txt', 'crates/ftui-demo-showcase/data/sqlite3.c',
             'crates/ftui-demo-showcase/data/evidence.jsonl'):
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
observation = json.loads((root / 'raw/ftui_showcase_wasm.observation.json').read_text())
observation.update(
    source_inputs_sha256=manifest['source_inputs_sha256'],
    runner_lock_sha256=manifest['runner_lock_sha256'],
    compiler=(root / 'toolchain.txt').read_text(),
    wasm_bindgen=(root / 'wasm-bindgen.txt').read_text().strip(),
)
with (root / 'raw/ftui_showcase_wasm.provenance.jsonl').open('x') as out:
    out.write(json.dumps(observation) + '\n')
for name, sha in files.items():
    print(f'{sha}  pkg/{name}')
PY
printf 'Serve the completed bundle: python3 -m http.server --directory %q 8080\n' "$output/site"
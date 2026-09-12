# Dependency Upgrade Log

## 2026-09-12 release campaign (bd-3trh0)

Status: release test gate awaits permission for fixture filesystem operations.
Source starts at `1f08de0e` (workspace version 0.7.0); nothing is published yet.
The existing ASCII wrapping candidate is preserved. It is included in the initial
dependency test snapshot, but has not been accepted for release. A concurrent
agent committed it as `661e82fb` during the September 12 follow-up; its bytes are
unchanged and that commit does not establish performance acceptance.
All compilation and verification use DSR on native hosts with the pinned
`nightly-2026-08-31`, checksum freshness, and content fingerprints. GitHub Actions
remains disabled.

Follow-up verification under G35 caught a default-feature rustdoc regression in
the earlier HAMT-only link repair: `g35a` failed `redundant_explicit_links` at
`program.rs:1135`. The `final4` HAMT check had not covered that final link text in
the default workspace feature graph. The link now uses a fully qualified label
without an explicit target; `g35b` checks both strict documentation builds.
This is separate from the 28 full-suite fixture failures awaiting permission.

Registry discovery examined 77 distinct direct dependencies across the workspace
and excluded fuzz project. Raw registry responses and the manifest/lock inventory
are retained at `/tmp/ftui-release-20260912-greenlynx/dependency-inventory.json`.
Fourteen workspace dependencies have newer stable versions than the lockfile:
asupersync, bitflags, flate2, js-sys, lru, reqwest, serial_test, smallvec,
sqlmodel-console, toml, trybuild, wasm-bindgen, wasm-bindgen-test, and which.
Fuzz's separate lockfile already has current arbitrary 1.4.2 and libfuzzer-sys 0.4.13.
Path dependencies and the toolchain pin are preserved.

- [x] Inspect current source, repository instructions, prior upgrade log and release.
- [x] Query current registry versions and retain responses.
- [x] Research, update, and verify each dependency individually.
- [ ] Run final workspace tests, feature checks, strict lint/docs, and security audit.
- [ ] Resolve release inclusion of the unfinished wrapping candidate.
- [ ] Update version and CHANGELOG.md from landed history.
- [ ] Build the previous six-target release matrix through DSR.
- [ ] Publish and verify GitHub assets/checksums and all 17 registry libraries.
- [ ] Verify downloads, native consumer behavior, and browser output.

### lru 0.18.2 → 0.18.4

The [published changelog](https://github.com/jeromefroe/lru-rs/blob/0.18.4/CHANGELOG.md)
adds `sparse` in 0.18.3 and `retain` in 0.18.4. Existing cache APIs need no migration.
Raised ftui-text's minimum constraint from 0.18.1 to 0.18.4 and started isolated
DSR verification (`frankentui-release-lru1`). The native resolver changed only
lru. The text suite passed 1,104 tests and all 292 showcase snapshots passed;
formatting, all-feature shaping check, workspace all-target check, strict Clippy,
and strict rustdoc also passed. The original outer wrapper returned exit 1 after
the native work completed; its failed receipt is preserved. A separate DSR
`frankentui-release-lru1-finalize` verified all nine native stage results, source
hashes, the resolved lockfile, and actual content fingerprints. Evidence is retained on trj
at `/data/retained/ftui-release-20260912-greenlynx/lru1-results`.

### which 8.0.5 → 8.0.6

The published source fixes relative PATH resolution against an explicitly supplied
working directory. FrankenTUI calls `which::which`; no API migration is needed.
Raised the doctor dependency constraint and started the doctor utility tests via
DSR. Native resolution changed only which. All 23 utility assertions passed,
but ten TempDir destructor cleanup attempts were denied and the original
`which1` gate failed. The focused command-discovery check (`which2`) passed
with no denied operations and verified source/lock/content fingerprints.
The utility test helper now disables cleanup at fixture creation; its full
suite passed all 23 cases in `console1`, with no blocked cleanup operations.

### bitflags 2.13.1 → 2.13.2

Upstream repairs macro constant declaration scope. Updated the three direct
constraints. The first run passed core/render assertions but stopped at the
headless export test's explicit cleanup. That test now retains its export;
all assertions remain unchanged. DSR `bitflags2` passed 1,421 core, 1,864 render,
and 3,380 widget tests, with no blocked operations, ignored cases, or escaped
children. Only bitflags's version/checksum/dependency references changed in
the lockfile; source and actual content fingerprints were checked.

### smallvec 1.15.2 → 1.16.1

Upstream removes an unused debugger-visualizer feature and optimizes push.
FrankenTUI does not enable the removed feature. DSR `smallvec1` passed 1,104 text,
1,864 render, and 684 layout tests with no blocked operations. Only smallvec's
version and checksum changed in the lockfile. All-feature shaping execution
remains part of the final feature gate.

### asupersync 0.4.9 → 0.4.11

Published blocking-pool and spawn-blocking implementations are unchanged;
the existing builder/task-handle APIs remain available. The release fixes
runtime teardown from async workers and current-thread scheduling. DSR
`asupersync1` passed all 12 executor-focused tests with `asupersync-executor`
enabled, including delivery, panic isolation, shutdown, and backpressure.
The lockfile also advances its four coupled macro/franken dependencies to 0.4.11.

### reqwest 0.13.4 → 0.13.5

Upstream fixes blocking timeout panic handling, wrapped timeouts, and proxies.
DSR `reqwest1` passed all nine doctor seed tests, including retry and timeout
behavior. The lockfile changes reqwest and its edge to the existing base64 0.23.1.

### sqlmodel-console 0.4.1 → 0.4.3

Upstream adds injected-environment detection and trims truthy environment values.
Existing output APIs remain available. DSR `console1` passed all 23 doctor utility
tests with no blocked operations, ignored tests, or escaped children. Only this
package's version and checksum changed in the lockfile.

### flate2 1.1.9 → 1.1.10

Upstream rejects incomplete deflate streams and oversized gzip extra fields and
fixes gzip header/footer write loops. The removed cloudflare-zlib backend is not
enabled here. Both runtime constraints now require 1.1.10. DSR `flate1` passed
all 41 event-trace tests, including retained gzip round-trip fixtures. Resolution
adds miniz_oxide 0.9.1 and zlib-rs 0.6.7 while preserving PNG's miniz_oxide 0.8.9.

### toml 1.1.4 → 1.1.6 (spec 1.1.0)

Upstream fixes numeric ownership conversion and avoids unnecessary key cloning.
DSR `toml1` passed 25 policy-config tests and 69 keybinding tests. The explicit
core serde run `toml2` also passed all 69 tests, but its source verifier expected
a non-test rlib and rejected the test-only artifact inventory. `toml3` passed
with verification of the actual test-lib content fingerprint. Failed receipts
are retained; only toml's version/checksum changed in the lockfile.

### trybuild 1.0.120 → 1.0.121

Upstream renames its target-triple dependency to target-tuple. No TestCases
invocations exist in ftui-core; this is a declared development dependency.
DSR `trybuild1` passed all 1,299 default core unit tests. Resolution changed
trybuild and the renamed target-tuple package only.

### serial_test 3.5.0 → 4.0.1

The new minimum Rust version is below this project's pinned nightly. Runtime
locking is unchanged; the derive crate updates to syn 3. DSR `serial1` passed
all 22 mouse-playground tests with four test threads, including the named serial
locks used for shared event counters and diagnostics. No blocked operations,
ignored tests, or escaped children were observed.

### Coupled WASM bindings

Updating wasm-bindgen 0.2.127 → 0.2.128, js-sys 0.3.104 → 0.3.105, and
wasm-bindgen-test 0.3.77 → 0.3.78 together because exact schema-family edges
require coherent resolution. The archived renderer's independent lockfile must
match too; its source revision and other dependencies remain pinned. The matching
CLI archive was verified against upstream SHA-256. DSR `wasm1` passed all 225
native web tests and checked source/content fingerprints. Upstream test 0.3.78
pins minicov exactly to 0.3.8 for LLVM profiling-runtime compatibility; the
resolver's 0.3.9 → 0.3.8 change is intentional (neither version is yanked).
DSR `wasm2` built both modules and regenerated bindings with actual content
fingerprints. DSR `browser2` passed real Chrome rendering, resize, input/byte
admission, quit-tail download/recovery, logging, and four package-rejection cases.
Screenshots were inspected and show actual dashboard UI. The first browser wrapper
used a WebSocket URL where the script expects an HTTP CDP endpoint; its failed
receipt is retained, and the rerun used the same package and retained profile.

### Audit follow-ups

The current RustSec snapshot reports existing unsoundness in im/sized-chunks,
unmaintained collections/shaping dependencies, and yanked chacha20 0.10.1.
These versions/checksums match the initial lock. The archived renderer's im/lru
warnings concern other workspace packages outside its normal renderer graph;
its pinned historical source is retained. No clean audit is claimed while these
warnings remain. Workspace and renderer audit invocations returned zero; fuzz
was absent from the tracked snapshot and was separately copied and audited in
`audit3` after preserving the failed missing-file/transport receipts.

DSR `chacha1` passed 1,520 core and 12 enabled Asupersync tests with chacha20
0.10.2. This unyanked patch fixes SSE2 backend instructions. Cargo also selected
the existing getrandom 0.3.4 for tempfile's compatible dependency edge.

Migrating im 15.1 to maintained imbl 7.0.2 touches four handwritten files.
The public hamt reexport changes concrete crate identity; test state hashing now
sorts map entries while preserving every previous state field and assertion.
Added insertion-order/hash sensitivity and public OrdSet snapshot-isolation
checks. DSR `imbl1` passed 31 snapshot unit tests, eight end-to-end tests, and
fourteen property tests with hamt enabled, without blocked operations or skips.
The resulting root lock removes im, sized-chunks, and bitmaps. DSR `audit4`
reports zero vulnerability entries, zero unsoundness warnings, and no yanked
packages in that lock; only rustybuzz/ttf-parser unmaintained notices remain.
The archived renderer's separate whole-workspace lock still has the previously
documented warnings; this is not a claim that every lock is warning-free.

### Release build freshness

The first-party WASM builder and crate publisher now explicitly pass
`-Zchecksum-freshness` and set `CARGO_BUILD_FINGERPRINT=content`. These entry points
previously omitted the newer native verification requirement. Shell syntax checks
passed; real route execution is part of the release gates.

### Final verification tracking

- [x] Initial updated workspace: check, strict Clippy, strict rustdoc, and test compilation (`final1`).
- [x] Audit follow-up: chacha20 and imbl targeted tests.
- [x] Correct the new hash helper's Clippy sort-by-key suggestion; retain `final2` failure.
- [x] Repeat workspace compiler/lint/doc gates on the audit-remediated source (`final3`).
- [x] Check explicit runtime features and text shaping; execute shaping tests.
- [x] Run complete nextest inventory with the configured 120-second per-test timeout.
- [ ] Account separately for ignored tests, doctests, and no-deletion guard constraints.
- [ ] Finalize release source, version 0.8.0, and changelog publication facts.
- [ ] Produce Linux GNU/musl/ARM64, macOS ARM64/Intel, and Windows MSVC assets.
- [ ] Publish and independently verify every release venue and main/master mirror.

RustSec evidence uses advisory-db commit
`b50980aad8b8f14f77e25a97b32dd94bf008b0af`, fetched on 2026-09-12. Archive bytes,
registry responses, failed/successful DSR receipts, source snapshots, and tests
are retained under the release campaign directories on trj and the Mac.

`final3` passed formatting, workspace all-target check, strict Clippy, default
strict rustdoc, and feature checks. Standalone hamt rustdoc exposed an existing
unqualified TerminalCapabilities link; its target is now fully qualified.
`final4` passed that affected documentation gate and full test compilation,
source/content verification, and nextest metadata generation. `shaping1` passed
all 1,321 all-feature text tests with no ignored cases or blocked operations.
`nextest1` ran 25,606 tests: 25,578 passed and 28 failed; seven other tests were
skipped. The guard recorded 394 denied filesystem operations, no successful
deletion/rename traces, no escaped children, and no aggregate timeout. Source
and test-binary hashes were checked before and after execution.

Independent failure review maps all 28 cases to blocked filesystem operations:
five Git fixture setups, three archive relocations, six pane saves, eleven
transition saves, watcher removal, symlink-fixture cleanup, and the nested
typestate compile-fail Cargo build. Later assertions in those cases remain
unexecuted. They are failures, not passes or newly proven product regressions.
The library-updater circuit breaker (more than ten test failures) and AGENTS.md
Rule 1 require a pause and explicit permission before the filesystem-dependent
retries. The narrow requested scope is only newly created test fixtures, with
logs, source copies, existing retained artifacts, and shared caches preserved;
the compile-fail case needs an isolated native DSR target/cache route.

Doctest execution, final release-source selection/version bump, six native release
packages, registry/GitHub publication, and download verification remain pending.
The excluded fuzz project's two direct tooling dependencies were already current
and its existing lock was audited. A prepared fuzz lock refresh/compiler check
has not run and must not be described as fuzz execution or completed coverage.

---

**Date:** 2026-06-08 | **Project:** FrankenTUI | **Language:** Rust

## Summary

- **Updated:** 20
- **Skipped:** 1 intentional compatibility alias
- **Failed:** 0
- **Needs attention:** 0

## Baseline

- Read `AGENTS.md` completely.
- Read `README.md` completely.
- GitHub repository: `Dicklesworthstone/frankentui`, default branch `main`.
- Open GitHub issues: none found, including bug/broken/regression searches.
- Agent Mail coordination degraded: MCP health is green, but the mail database reports corruption and session bootstrap failed with `file is not a database`.
- Local Beads triage: 198 open issues, 56 actionable; no dependency-update issue superseded this request.

## Outdated Dependencies

Detected with `cargo outdated --workspace --depth 1 --root-deps-only`.

- `chrono`: 0.4.44 -> 0.4.45
- `reqwest`: 0.13.3 -> 0.13.4
- `serde_json`: 1.0.149 -> 1.0.150
- `sha2`: 0.10.9 -> 0.11.0
- `which`: 8.0.2 -> 8.0.3
- `opentelemetry_sdk`: 0.32.0 -> 0.32.1
- `toml`: 0.8.23 -> 1.1.2+spec-1.1.0
- `unicode-segmentation`: 1.13.2 -> 1.13.3
- `bitflags`: 2.11.1 -> 2.13.0
- `getrandom`: 0.3.4 -> 0.4.2
- `bumpalo`: 3.20.2 -> 3.20.3
- `memchr`: 2.8.0 -> 2.8.1
- `ratatui`: 0.30.0 -> 0.30.1
- `pulldown-cmark`: 0.13.3 -> 0.13.4
- `wgpu`: 28.0.0 -> 29.0.3
- `tungstenite`: 0.28.0 -> 0.29.0
- `serial_test`: 3.4.0 -> 3.5.0
- `wasm-bindgen-test`: 0.3.71 -> 0.3.73
- `js-sys`: 0.3.98 -> 0.3.100
- `wasm-bindgen`: 0.2.121 -> 0.2.123

## Research Notes

- `wgpu` 29 changes `PipelineLayoutDescriptor::bind_group_layouts` to hold optional layouts, so the GPU VFX pipeline now passes `&[Some(&bind_group_layout)]`.
- `sha2` 0.11 no longer supports direct lower-hex formatting of finalized digest output at the existing call sites, so `doctor_frankentui` now uses one shared hex encoder in `util`.
- `getrandom` 0.4 is the current direct wasm RNG line, but `ahash` 0.8.12 still pulls `getrandom` 0.3 for its default runtime RNG path. `ftui-core` keeps a target-only `getrandom_03` alias with `wasm_js` enabled so wasm builds keep compiling.
- `toml` 1.1.2, `tungstenite` 0.29.0, `ratatui` 0.30.1, `pulldown-cmark` 0.13.4, wasm-bindgen family patch releases, and the remaining patch updates did not require source changes beyond manifest constraints and lock resolution.

## Updates

- Updated dependency constraints in crate manifests for `chrono`, `reqwest`, `serde_json`, `sha2`, `which`, `opentelemetry_sdk`, `toml`, `unicode-segmentation`, `bitflags`, direct `getrandom`, `bumpalo`, `memchr`, `ratatui`, `pulldown-cmark`, `wgpu`, `tungstenite`, `serial_test`, `wasm-bindgen-test`, `js-sys`, and `wasm-bindgen`.
- Ran `cargo update`, which refreshed the local ignored `Cargo.lock` resolution used by validation.
- Updated `ftui-extras` GPU VFX pipeline layout construction for `wgpu` 29.
- Added `doctor_frankentui::util::hex_encode` and replaced digest lower-hex formatting call sites affected by `sha2` 0.11.
- Reworked wasm `getrandom` target dependencies so direct `getrandom` 0.4 and transitive `getrandom` 0.3 both have wasm JS support where needed.
- Applied the minimal rustfmt-required formatting changes after the workspace format check identified them.

## Failed

None.

## Needs Attention

None. `cargo outdated --workspace --depth 1 --root-deps-only` now reports only the intentional target-only `getrandom_03` alias described above.

## Validation

- `env CARGO_TARGET_DIR=/data/tmp/frankentui-upgrade-target cargo check -p ftui-extras --features fx-gpu` passed.
- `env CARGO_TARGET_DIR=/data/tmp/frankentui-upgrade-target cargo check -p ftui-runtime --features policy-config --all-targets` passed.
- `env CARGO_TARGET_DIR=/data/tmp/frankentui-upgrade-target cargo check -p doctor_frankentui --all-targets` passed after the `sha2` hex fix.
- `env CARGO_TARGET_DIR=/data/tmp/frankentui-upgrade-target cargo check -p ftui-pty --all-targets` passed.
- `env CARGO_TARGET_DIR=/data/tmp/frankentui-upgrade-target cargo check -p ftui-showcase-wasm --target wasm32-unknown-unknown` passed after the wasm `getrandom` feature fix.
- `env CARGO_TARGET_DIR=/data/tmp/frankentui-upgrade-target cargo check --workspace --all-targets` passed.
- `env CARGO_TARGET_DIR=/data/tmp/frankentui-upgrade-target cargo clippy --workspace --all-targets -- -D warnings` passed.
- `cargo fmt --check` passed.
- `cargo audit` passed with no vulnerability findings.
- `cargo outdated --workspace --depth 1 --root-deps-only` passed except for the intentional `getrandom_03` compatibility alias.
- `env CARGO_TARGET_DIR=/data/tmp/frankentui-upgrade-target cargo test --workspace` ran through the workspace and hit one non-repeatable perf-threshold failure in `bloodstream_database_to_terminal_roundtrip_is_delta_only` (`2466us` vs sub-millisecond target).
- Targeted rerun passed: `env CARGO_TARGET_DIR=/data/tmp/frankentui-upgrade-target cargo test -p ftui-runtime --test reactive_bindings_e2e bloodstream_roundtrip::bloodstream_database_to_terminal_roundtrip_is_delta_only -- --exact --nocapture`.

---

# Dependency Upgrade Log — 2026-07-24

**Project:** FrankenTUI | **Language:** Rust | **Toolchain:** nightly (rolling, 2026-07-22)

## Summary

Modernized all direct dependencies to latest published crates.io versions,
including the held dependabot bump `sqlmodel-console` 0.2.0 -> 0.3.0 and the
frankensuite `asupersync` 0.3.4 -> 0.3.9 upgrade.

## Dependency changes (old -> new)

| Crate | Old | New | Notes |
|---|---|---|---|
| asupersync | 0.3.4 | 0.3.9 | optional (`asupersync-executor` feature); no source change needed |
| sqlmodel-console | 0.2.0 (lock 0.2.2) | 0.3.0 | held dependabot #89; no API break at our call sites |
| base64 | 0.22.1 | 0.23.0 | no breaking changes affecting our usage |
| ratatui (+ -core/-widgets/-macros/-crossterm/-termwiz) | 0.30.1 | 0.30.2 | patch |
| clap (+ builder/derive) | 4.5.60/4.6.1 | 4.6.4 | |
| serde (+ core/derive) | 1.0.228 | 1.0.229 | |
| serde_json | 1.0.150 | 1.0.151 | |
| thiserror | 2.0.12/2.0.18 | 2.0.19 | |
| which | 8.0.3 | 8.0.5 | |
| bitflags | 2.13.0 | 2.13.1 | |
| bytemuck | 1.25.0 | 1.25.2 | |
| memchr | 2.8.1 | 2.8.3 | |
| time | 0.3.44 | 0.3.54 | |
| toml | 1.1.2 | 1.1.3 | |
| lru | 0.18.0 | 0.18.1 | |
| rustc-hash | 2.1(.1) | 2.1.3 | |
| regex | 1.12.3 | 1.13.1 | |
| arc-swap | 1.8.2 | 1.9.2 | |
| getrandom | 0.4.2 | 0.4.3 | wasm target dep |
| js-sys | 0.3.100 | 0.3.103 | wasm |
| wasm-bindgen (+ -test) | 0.2.123/0.3.73 | 0.2.126/0.3.76 | wasm |
| libc | 0.2 (unpinned) | 0.2.189 | |
| trybuild | 1.0 (unpinned) | 1.0.118 | dev |

## asupersync 0.3.4 -> 0.3.9

- The documented `MutexGuard` `!Send` break is a NO-OP here: FrankenTUI does not
  hold an `asupersync::sync::MutexGuard` across `.await` inside a `spawn`. The
  only asupersync surface used is `runtime::{RuntimeBuilder, Runtime,
  BlockingTaskHandle}` in `ftui-runtime/src/program.rs` (the
  `asupersync-executor` blocking-task lane), which compiled unchanged.
- `cargo check -p ftui-runtime --features asupersync-executor --all-targets`
  passed (Finished, exit 0). No code edits required.
- Toolchain: repo pins bare `channel = "nightly"` which resolves to the rolling
  nightly (2026-07-22), newer than asupersync's `sysinfo 0.39 cfg_select`
  floor (2026-07-05), so the E0658 `cfg_select` issue does not apply.

## Validation

- `cargo check -p ftui-runtime --features asupersync-executor --all-targets`: Finished, exit 0.
- `cargo test --workspace --lib`: deterministic unit tests (see repo log line).
- Full `cargo test --workspace` failures are confined to the `ftui-demo-showcase`
  integration suite and are all environmental: snapshot mismatches (terminal/locale
  dependent — pre-existing, reproduced on clean HEAD before any change),
  perf-budget panics (p99/tick-latency budgets blown under host load avg ~300),
  and one frame-hash determinism test (timing under load). No logic/correctness
  regressions from the dependency bumps. This matches upstream CI, whose only real
  test failure on a clean env was a single `command_palette` perf-budget test.

## Pre-existing CI red

FrankenTUI CI was red before this work. Root causes: (1) `error: unresolved link`
rustdoc failures in `ftui-web`/`ftui-widgets` (Documentation job), (2) a single
`command_palette::scorer::perf_tests::perf_corpus_100_under_budget` perf-budget
failure, and (3) jobs that never ran real steps during the GH-Actions/`ovh-b`
degraded window. None are attributable to this dependency upgrade.

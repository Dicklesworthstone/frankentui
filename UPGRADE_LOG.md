# Dependency Upgrade Log

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

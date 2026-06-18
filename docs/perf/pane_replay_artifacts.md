# Symbolized Pane Replay Artifacts — Contract (bd-1pvzq.1)

> Status: **implemented · self-validating · CI-enforced**
>
> Producer: `scripts/pane_profile.sh` (+ `crates/ftui-layout/benches/pane_profile_harness.rs`)
> Index tool: `scripts/pane_replay_artifacts.py` (`emit` / `validate` / `selftest`)
> CI job: `.github/workflows/ci.yml` → `pane-perf-artifacts`

This document is the **stable artifact contract** for the pane performance lane.
The later perf gates (`bd-1pvzq.2` latency envelopes, `bd-1pvzq.3` golden
replay-oracle + differential certification, `bd-1pvzq.5` E2E soak/rollback) all
read the same bundle, so the layout and schema below are the interface they
depend on.

## Why "symbolized replay"?

A pane perf regression is only actionable if you can do two things at once:

1. **Replay** the exact pane operation history that produced a number, and
   re-derive the deterministic result state — so you can tell a *real* behaviour
   change from measurement noise.
2. **Symbolize** a CPU profile of the *exact binary* that produced that number
   back to source — so you can see *where* the time went.

Historically these lived in different places (`pane_profile_harness` manifest
vs. `symbol_metadata.txt` vs. ad-hoc `perf record`). A "symbolized pane replay
artifact" couples them under one checksummed, schema-versioned index so a
downstream gate can trust that the replay evidence and the symbolization
provenance describe **one coherent run**.

## Bundle layout

Produced under the `--out-dir` of `pane_profile.sh` (default
`target/pane-profiling/bd-1y0ph`, CI uses `target/pane-profiling/ci`):

```
<out-dir>/
├── replay_artifact_index.json      # the contract: checksummed index (this doc)
├── pane_core_profile_harness/
│   ├── manifest.json               # REPLAY evidence: state hashes + diagnostics
│   ├── baseline_snapshot.json      # canonical pane tree before the history
│   ├── final_snapshot.json         # canonical pane tree after the history
│   └── run.log                      # verbose per-iteration log
├── symbol_metadata.txt             # SYMBOLIZATION provenance per bench binary
├── executed-binaries/              # exact bench binaries when materialized locally
├── layout_bench.txt                # pane/core/* Criterion output
├── pane_terminal_bench.txt         # pane/terminal/* Criterion output
├── pane_pointer_bench.txt          # pane/web_pointer/* Criterion output
└── README.txt                      # human index of the above
```

## Index schema (`replay_artifact_index.json`)

`schema = "ftui.pane.replay_artifact_index"`, `schema_version = 1`.

| Top-level key   | Meaning |
|-----------------|---------|
| `schema` / `schema_version` | Contract identity; bump the version on any breaking change. |
| `bead`          | `bd-1pvzq.1` provenance. |
| `runner`        | `local` \| `rch` \| `ci` — where the benches actually executed. |
| `mode`          | `{test_mode, perf_stat, stack_reports}` flags of the run. |
| `out_dir`       | Bundle root (absolute). |
| `replay`        | Replay evidence block (below). |
| `symbolization` | Symbolization provenance block (below). |
| `artifacts`     | Flat checksummed manifest of **every** file in the bundle. |

### `replay`

Lifted from `pane_core_profile_harness/manifest.json` plus file references:

- `manifest` — `{path, sha256, size_bytes}` for the harness manifest.
- `snapshots` — `[{path, sha256, size_bytes}, …]` (baseline + final).
- `run_log` — `{path, sha256, size_bytes}`.
- `scenario`, `leaf_count`, `operations_per_iteration`, `iterations`,
  `warmup_iterations` — scenario shape.
- `baseline_hash`, `final_hash`, `aggregate_hash` — **deterministic replay
  state hashes**. These are the golden values a later gate compares against.
  The harness itself asserts `replay() == applied tree` every iteration, so the
  hashes are replay-verified, not merely observed.
- `ns_per_iteration`, `checkpoint_interval`, `checkpoint_count`,
  `checkpoint_hit`, `replay_start_idx`, `replay_depth` — replay-cost telemetry.
- `allocation_diagnostics`, `retention_diagnostics` — carried verbatim so gates
  reason about allocation/retention budgets without re-reading the manifest.

`validate` cross-checks that the index's `baseline_hash` / `final_hash` /
`aggregate_hash` / `scenario` **still agree with the manifest file** — the index
can never silently drift from the evidence it summarizes.

### `symbolization`

- `metadata` — `{path, sha256, size_bytes}` for `symbol_metadata.txt`.
- `binaries` — one entry per executed bench binary (labels:
  `pane_profile_harness`, `layout_bench`, `pane_terminal_bench`,
  `pane_pointer_bench`), each carrying:
  `executed_path`, `binary_source`, `exact_binary_status`, `build_id`,
  `debug_info`, `stripped`, `addr2line_ready`, `symbolization_ready`,
  `binary_sha256`.
- `all_symbolization_ready` — true iff every binary is symbolization-ready.
- `expected_labels` — the labels the contract requires to be present.

`symbolization_ready` is true iff the binary has a GNU **build-id** (to match a
profile to this exact binary) **and** is addr2line-ready (`.debug_info` +
`.debug_line` present and not stripped). The `[profile.bench]` profile in the
workspace `Cargo.toml` sets `debug = true` and `strip = false`, so bench
binaries are symbolizable by construction both locally and in CI.

## Producing the bundle

### Local

```bash
./scripts/pane_profile.sh --test         # fast; criterion benches in --test mode
./scripts/pane_profile.sh                # full measured run
./scripts/pane_profile.sh --stack-reports  # + perf record/report symbolized stacks
```

`pane_profile.sh` always emits `replay_artifact_index.json` and runs a **lenient
self-validation** (structure + replay + checksums). Lenient mode lets local
`rch` runs pass even when the exact remote bench binary could not be fetched
(`symbolization_ready=false` is recorded, not fatal).

### rch nuance

When `rch` offloads the build, the bench binaries execute remotely. The runner
preserves them via `scp` into `executed-binaries/` when possible; if that fails,
the symbol metadata records `exact_binary_status=missing` and
`symbolization_ready=false`. The replay manifest is still reconstructed from the
harness's stdout markers, so the **replay** half of the contract holds even when
the **symbolization** half degrades. This is why local self-validation is
lenient and CI is strict.

### CI

The `pane-perf-artifacts` job runs `pane_profile.sh --test` (no `rch`, binaries
local), then runs the validator **strictly**:

```bash
python3 scripts/pane_replay_artifacts.py validate \
  --index target/pane-profiling/ci/replay_artifact_index.json \
  --require-symbolization --json
```

The bundle is uploaded as the `pane-replay-artifacts` GitHub artifact
(30-day retention).

## Validating the contract

```bash
# Structure + replay + checksums (always enforced):
python3 scripts/pane_replay_artifacts.py validate --index <out-dir>/replay_artifact_index.json

# Additionally require every binary be symbolization-ready (CI / local non-rch):
python3 scripts/pane_replay_artifacts.py validate --index … --require-symbolization

# Logic regression test (no cargo build needed):
python3 scripts/pane_replay_artifacts.py selftest
```

`validate` fails (non-zero) when any of these silently degrade
(bd-1pvzq.1 AC2):

- the schema / schema_version changes unexpectedly,
- a referenced file is missing or its checksum no longer matches,
- the replay summary drifts from the manifest,
- an expected bench-binary label is absent,
- a required per-binary field is missing,
- (strict) any binary is not symbolization-ready or lacks a hex build-id,
- the core replay + symbolization files are absent from the checksummed manifest.

## How later gates reuse this

| Gate | Consumes |
|------|----------|
| `bd-1pvzq.2` latency envelopes | `replay.ns_per_iteration`, `replay.allocation_diagnostics`, `replay.retention_diagnostics` as the measured envelope inputs; `replay.*_hash` to confirm the envelope was measured on the canonical history. |
| `bd-1pvzq.3` golden replay-oracle + differential certification | `replay.baseline_hash` / `final_hash` / `aggregate_hash` + the snapshots as the golden oracle; `symbolization.binaries[*].build_id` to attribute any differential failure to a specific binary. |
| `bd-1pvzq.5` E2E soak / rollback | the whole bundle as the per-run evidence pack, validated with `--require-symbolization`. |

Keeping all of them on this one index means a single producer
(`pane_profile.sh`) and a single validator (`pane_replay_artifacts.py`) — no
gate re-discovers how to collect or symbolize pane perf evidence.

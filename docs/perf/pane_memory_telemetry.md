# Pane Memory Telemetry — Allocation & Retention (bd-25wj7.1)

> Status: **instrumented · baselines captured**
>
> Library: `crates/ftui-layout/src/pane_memory.rs` (cross-strategy model)
> Persistent model: `crates/ftui-layout/src/pane_persistent.rs` → `PaneVersionStore::retention`
> Artifact bench: `crates/ftui-layout/benches/pane_memory_telemetry.rs`
> Tests: `pane_memory::tests` (6) + `pane_persistent::tests::retention_*` (3)

## TL;DR

Pane history can be retained three ways, each buying undo/redo at a different
memory price. This bead turns that trade space into **measured, attributable,
deterministic telemetry** so later retention/pruning work optimizes against
concrete numbers instead of intuition.

| Strategy | What it stores | Navigation | Modeled retained (resize_storm, 512 ops) |
|----------|----------------|------------|-------------------------------------------|
| **Baseline** | one live `PaneTree`, no history | n/a | **15.7 KB** |
| **Checkpointed** (`PaneInteractionTimeline`) | baseline + periodic snapshots + op log + per-entry hashes; replay | replay from nearest checkpoint | **575.6 KB** (36.6× baseline) |
| **Persistent** (`PaneVersionStore`) | `Arc`-shared version roots (path-copy) | **O(1)** index move | **1907.7 KB** (3.31× checkpointed) |

Two headline findings, consistent across both workloads:

1. **Node structs dominate every strategy.** The op log and state hashes are a
   small minority of the checkpointed footprint (45 KB op log + 8 KB hashes out
   of 576 KB). The memory problem is fundamentally *retained tree nodes*, so
   that is where pruning (`bd-25wj7.2`) and structural sharing (`bd-25wj7.3`)
   must aim.
2. **History is 1–2 orders of magnitude over the live tree** (24–37× baseline),
   and the persistent path trades ~3× the checkpointed memory for O(1)
   navigation — exactly the trade the bounded-window adoption
   ([`pane_persistent_versioning.md`](pane_persistent_versioning.md)) is meant
   to close.

## What is measured

Telemetry has two halves, split by where each can be honestly captured:

- **Transient allocations** (build-time churn) — captured by the bench with
  `stats_alloc`: allocation count + bytes allocated while each strategy builds
  the full operation history. Allocator-dependent, so it lives in the bench, not
  the library.
- **Retained-state economics** (what stays resident) — the pure, deterministic
  [`pane_memory_comparison`] in the library. Every strategy is decomposed into
  the **same retained-state classes** so the dominant driver is explicit:

  | Class | Meaning |
  |-------|---------|
  | `node_struct_bytes` | retained tree node structs (records / `Arc` nodes) |
  | `leaf_payload_bytes` | leaf surface-key bytes |
  | `extension_payload_bytes` | node + leaf extension-map bytes |
  | `operation_payload_bytes` | op-log entry structs + operation heap payloads (checkpointed only) |
  | `state_hash_overhead_bytes` | per-entry before/after `u64` hashes (checkpointed only) |
  | `container_and_metadata_bytes` | container struct + version/checkpoint metadata (remainder) |

  The per-class fields sum **exactly** to `total_retained_bytes` (asserted in
  `every_footprint_decomposition_is_faithful`); the model mirrors the canonical
  timeline's `size_of`-plus-measured-payload methodology so the strategies are
  byte-comparable.

The byte model is a conservative shallow estimate, not allocator truth — but it
is *reproducible*: identical inputs yield byte-identical reports
(`comparison_is_deterministic`), which is what makes it usable as a CI
regression baseline. The bench also emits the allocator-measured
`net_retained_bytes_measured` alongside the model so the two corroborate each
other (e.g. checkpointed resize_storm: 576 KB modeled vs 604 KB measured).

## Measured baselines

Reproduce with:

```bash
cargo bench -p ftui-layout --bench pane_memory_telemetry -- --out /tmp/pane_memory.json
```

### `resize_storm` — 64 leaves, 512 pure `SetSplitRatio` ops

| Strategy | transient allocs | transient bytes | modeled retained | dominant driver |
|----------|------------------|-----------------|------------------|-----------------|
| baseline | 76 373 | 20.4 MB | 15.7 KB | node structs |
| checkpointed | 3 801 | 1.17 MB | 575.6 KB | node structs |
| persistent | 15 840 | 1.97 MB | 1907.7 KB | node structs |

`persistent/checkpointed = 3.31×`, `checkpointed/baseline = 36.55×`. Checkpointed
breakdown: 505 KB nodes · 14 KB leaf · 45 KB op log · 8 KB hashes · 2.7 KB meta.

### `mixed_session` — 32 leaves, 384 split/close/swap/move/ratio ops

| Strategy | transient allocs | transient bytes | modeled retained | dominant driver |
|----------|------------------|-----------------|------------------|-----------------|
| baseline | 33 625 | 8.31 MB | 12.2 KB | node structs |
| checkpointed | 19 215 | 4.91 MB | 294.5 KB | node structs |
| persistent | 17 551 | 4.64 MB | 878.5 KB | node structs |

`persistent/checkpointed = 2.98×`, `checkpointed/baseline = 24.16×`.

### Reading the transient vs retained split

The two axes tell different stories and both matter:

- **Baseline churns the most transient memory but retains almost nothing.** The
  conservative in-place mutation path re-validates the whole tree per op (76 K
  allocs / 20 MB over the storm) yet ends holding one ~16 KB tree. This is the
  *transient churn* problem `bd-25wj7` was originally about.
- **Checkpointed is transient-frugal but retention-heavy relative to baseline.**
  Few allocations (it appends to a log and snapshots sparsely) but 24–37× the
  resident bytes, deferring reconstruction cost to replay.
- **Persistent sits between on transient and highest on retained**, because it
  materializes every version — the price of O(1) navigation, and the number the
  bounded-window policy must bring down.

## How downstream beads use this

- **`bd-25wj7.2` (bounded retention / pruning):** the `persistent` and
  `checkpointed` retained totals are the concrete numbers a window/pruning policy
  optimizes against. Because node structs dominate, capping retained
  versions/snapshots is the highest-leverage lever; payloads and hashes are
  noise. The persistent store already exposes `with_max_versions`, and
  `PaneVersionStore::retention` re-measures a bounded store directly.
- **`bd-25wj7.3` (churn reduction via structural sharing):** target the baseline
  transient-allocation counts (the in-place mutation path), and use the
  persistent sharing ratio as the model for hot-path small-object reuse.

## Files

- `crates/ftui-layout/src/pane_memory.rs` — `PaneMemoryStrategy`,
  `PaneMemoryDriver`, `PaneMemoryStrategyFootprint`, `PaneMemoryComparison`,
  `pane_memory_comparison`.
- `crates/ftui-layout/src/pane_persistent.rs` — `PaneVersionStore::retention` →
  `PaneVersionRetention` (the persistent retained-memory byte model).
- `crates/ftui-layout/benches/pane_memory_telemetry.rs` — artifact producer
  (transient + modeled), JSON manifest via `--out`.

# Persistent / Versioned PaneTree — Adoption Spike (bd-1k7ek.5)

> Status: **prototype complete · recommendation = Conditional GO (bounded-window)**
>
> Code: `crates/ftui-layout/src/pane_persistent.rs`
> Proof: `crates/ftui-layout/tests/pane_persistent_equivalence.rs`
> Benchmark: `crates/ftui-layout/benches/pane_persistent_bench.rs`

## TL;DR

The pane lane's guiding thesis is that the biggest remaining wins come from
**asymptotic replay elimination**, not instruction-level churn. This spike builds
a persistent (structurally shared) pane tree and measures it head-to-head against
the production checkpointed [`PaneInteractionTimeline`] replay path.

**Verdict: Conditional GO.** Adopt a **bounded-window** persistent version store
for the active undo/redo history. At a realistic 64-version window it is:

- **~4.6–6× faster to apply** (path-copy vs. clone + whole-tree validate + periodic snapshot),
- **O(1) to navigate** (undo/redo/scrub) versus the timeline's replay + snapshot-restore,
- and **uses 0.46–0.90× the memory** of the checkpointed timeline — i.e. *less* memory while being dramatically faster.

The **unbounded** form trades **~3.4× more memory** for the same speed, so the
GO is gated on a bounded-retention policy ([bd-25wj7.2]) and a per-context
execution-policy selector ([bd-1k7ek.6]). Those are already modeled as
dependents of this bead.

## Background: the replay problem

`PaneTree` stores nodes in a flat `BTreeMap<PaneId, PaneNodeRecord>` with explicit
parent pointers — excellent for `O(1)` id lookup, but **not naturally persistent**:
cloning the whole map to snapshot a version is `O(nodes)`. The timeline therefore
reconstructs any historical state by **replaying** operations forward from the
nearest checkpoint:

```
undo() / redo()  →  rebuild()  →  restore nearest checkpoint snapshot
                                   (clone + from_snapshot ⇒ whole-tree validation, O(nodes))
                                +  replay forward up to `checkpoint_interval` ops
```

Every single navigation step pays a full `O(nodes)` snapshot-restore (which
re-validates the entire tree) plus up to `checkpoint_interval − 1` operation
replays. The default interval is 16, so worst-case replay depth is 15 — but the
snapshot-restore is the dominant cost for non-trivial trees.

## Design

`VersionedPaneTree` represents the tree as an immutable tree of
`Arc<PersistentNode>`. A structural change produces a **new root** by
*path-copying*: only the nodes on the root→target path are re-allocated; every
off-path subtree is reused via an `Arc` clone (a refcount bump). A
`PaneVersionStore` keeps a `Vec` of version roots and a cursor, so undo/redo are
pure index moves — no clone, no validation, no replay.

Two design choices make this work:

1. **Parent-free nodes.** A node that knew its parent could not be shared between
   two versions (or two positions). Parents are reconstructed only when flattening
   back to the canonical tree (`to_pane_tree`). This is the property that *enables*
   sharing; it is also the one ergonomic cost (see below).
2. **Canonical bridge.** `from_pane_tree` / `to_pane_tree` convert losslessly to
   the production `PaneTree`, and `to_pane_tree` runs the same whole-tree
   validator. This is the differential-oracle hook and the always-available
   rollback path.

### Operation coverage

| Operation | Strategy | Notes |
|-----------|----------|-------|
| `SetSplitRatio` | **path-copy** | the hot drag-resize path; `O(depth)` new nodes |
| `SplitLeaf`     | **path-copy** | id-allocating insertion (split id, then leaf id — same order as baseline) |
| `CloseNode`     | **path-copy** | sibling promotion = replace parent split with the other child |
| `SwapNodes`     | **path-copy** | two-path rebuild; off-path subtrees stay `Arc`-identical |
| `MoveSubtree`   | rebuild fallback | flatten → conservative baseline → rebuild (rare; no sharing) |
| `NormalizeRatios` | rebuild fallback | touches all splits anyway; no sharing to preserve |

The rebuild fallback guarantees **total** semantic parity for the differential
oracle while keeping the prototype small. `MoveSubtree`/`NormalizeRatios` are the
rarest operations and do not change the asymptotic conclusion.

## Equivalence proof

`tests/pane_persistent_equivalence.rs` proves the prototype is observationally
equivalent to the canonical `PaneTree` + `PaneInteractionTimeline`:

- **Apply parity** — 48 seeds × 40 ops, all six families: after every op the
  persistent current version flattens to the same `state_hash` and `next_id` as
  the canonical tree.
- **Navigation parity** — 24 seeds: replay-free `O(1)` undo/redo over the version
  store reproduces the exact hash sequence the checkpointed-replay timeline
  produces at every cursor (undo all the way down, redo all the way back).
- **Reject parity** — missing-node, set-ratio-on-leaf, split-a-split, close-root,
  swap-with-ancestor: both engines reject.
- **Structural sharing** — after a deep `SetSplitRatio`, an off-path subtree is the
  same physical `Arc` allocation (`Arc::ptr_eq`), and the mutated root path is a
  fresh allocation.
- **Determinism** — identical seeds yield identical version-hash sequences.
- **Rollback** — the persistent tree always flattens to a *valid* canonical tree
  identical to replaying the operation log.

Plus 11 in-module unit tests for the path-copy primitives.

## Benchmark results

`benches/pane_persistent_bench.rs` (release, `stats_alloc` for retained bytes).
Numbers below are representative; absolute timings vary run-to-run on shared
build hosts, but the **ratios are stable**.

### resize_storm — 63 leaves (125 nodes), 512 `SetSplitRatio` ops

| Metric | Checkpointed timeline | Persistent (unbounded) | Persistent (window=64) |
|--------|----------------------:|-----------------------:|-----------------------:|
| Apply (total) | ~13–22 ms | **~2.1–4.9 ms** | — |
| Memory retained | 0.60 MB | 2.05 MB (**3.44×**) | **0.275 MB (0.46×)** |
| Structural sharing | n/a | **73.8%** (16,797 distinct / 64,125 logical) | 72.0% (2,237 distinct) |
| Per-step undo/redo | ~105–130 µs | **~0 µs pure · ~34–50 µs +flatten** | same |
| Scrub (64 scattered jumps) | ~1.4–1.7 s | **~3–4 ms (≈450×)** | same |

### resize_storm — 255 leaves (509 nodes), 512 ops

| Metric | Checkpointed | Persistent (unbounded) | Persistent (window=64) |
|--------|-------------:|-----------------------:|-----------------------:|
| Apply (total) | ~43–60 ms | **~6–7 ms** | — |
| Memory retained | 2.26 MB | 8.2 MB (3.63×) | **1.05 MB (0.47×)** |
| Sharing | n/a | 73.8% | 73.3% |
| Scrub (64 jumps) | ~6–8 s | **~15–32 ms (≈250–400×)** | same |

### mixed_session — 32 leaves, 256 mixed ops

| Metric | Checkpointed | Persistent (unbounded) | Persistent (window=64) |
|--------|-------------:|-----------------------:|-----------------------:|
| Apply (total) | ~4–7 ms | **~2–4 ms** | — |
| Memory retained | 0.159 MB | 0.555 MB (3.49×) | **0.144 MB (0.90×)** |
| Sharing | n/a | 69.7% | 69.2% |
| Scrub (64 jumps) | ~0.4–0.7 s | **~1.5–2.9 ms (≈255×)** | same |

### Live `PaneExecutionEngine` wrapper benchmarks (`--live`, 240×80 viewport, 100% feasible solve)

The initial spike tables above measure the standalone `PaneVersionStore` against
`PaneInteractionTimeline`. When wired through the live [`PaneExecutionEngine`]
wrapper (which maintains canonical `PaneTree` + `PaneInteractionTimeline` parity,
enforces `PaneRetentionPolicy`, and solves layout + renders preview rects on a
`240×80` viewport), `benches/pane_persistent_bench.rs -- --live` also benchmarks:

- **`balanced_resize_storm`** (63 and 255 leaves built via breadth-first
  alternating `Horizontal`/`Vertical` splits, depth 6 and 8, `solve_success=512,
  solve_error=0`),
- **`skewed_feasible_resize_storm`** (255 leaves built as a depth-254 chain
  biasing splits 3:1 `Horizontal:Vertical` so minimum dimensions `192×64` fit
  inside `240×80`, `solve_success=512, solve_error=0`),
- **`balanced_mixed_session`** (64 leaves, 256 viewport-validated mixed
  operations across all six families, `solve_success=256, solve_error=0`).

Representative `retained_edits=64` wrapper timings (`engine_apply` p50 / p95 / p99):

| Scenario | Checkpointed | Persistent | Conservative | Adaptive |
|----------|-------------:|-----------:|-------------:|---------:|
| `balanced_resize_storm` (63 leaves) | **160 / 218 / 243 µs** | 265 / 307 / 339 µs | 183 / 252 / 270 µs | 266 / 326 / 380 µs |
| `balanced_resize_storm` (255 leaves) | **773 / 961 / 1,057 µs** | 1,139 / 1,249 / 1,473 µs | 943 / 1,143 / 1,232 µs | 1,139 / 1,237 / 1,502 µs |
| `skewed_feasible_resize_storm` (255 leaves) | **777 / 986 / 1,081 µs** | 2,238 / 2,557 / 3,143 µs | 963 / 1,165 / 1,249 µs | 2,202 / 2,321 / 2,466 µs |
| `balanced_mixed_session` (64 leaves) | **209 / 277 / 298 µs** | 812 / 985 / 1,291 µs | 237 / 350 / 393 µs | **209 / 277 / 341 µs** |

Why `Checkpointed` wins end-to-end inside `PaneExecutionEngine`: the live engine
maintains canonical `PaneTree` and `PaneInteractionTimeline` state alongside the
optional `PaneVersionStore` and flattens `VersionedPaneTree -> PaneTree` on
navigation so all downstream callers (`solve_layout`, snapshot export, invariant
validation) operate on the canonical `PaneTree`. In the `balanced_mixed_session`
workload, `Adaptive` automatically detects non-local structural operations and
falls back to `Checkpointed`, matching `Checkpointed` latency (`209 µs` p50 vs
`812 µs` for forced `Persistent`).

## Complexity

| | Checkpointed timeline | Persistent version store |
|--|----------------------|--------------------------|
| Apply | `O(nodes)` clone + validate, periodic `O(nodes)` snapshot | **`O(depth)`** new nodes (path-copy) |
| Undo / redo (one step) | `O(nodes)` restore + `O(replay_depth)` replay | **`O(1)`** cursor move (`O(nodes)` flatten when materializing canonical `PaneTree`) |
| Seek to version *k* | `O(distance × per-step)` (no seek API) | **`O(1)`** |
| Memory (V versions, N nodes) | `N + (V/interval)·N` snapshots + compact op log | `N + Σ O(depth)` shared nodes (bounded by window) |

The persistent path-copy *search* is `O(nodes)` in this prototype (no id→path
index); a production version would carry a positional index for `O(depth)`
location. The headline win — `O(1)` navigation — does not depend on that.

## Memory analysis & the bounded-window insight

The unbounded store retains **every** version, so for 512 ops it holds ~3.4× the
bytes of a timeline that keeps only a baseline + sparse checkpoints + a compact
op log. That is the real cost the bead asked us to quantify.

But undo history is **always bounded** in practice. Capping the store at a
64-version window:

- drops retained memory to **0.46–0.90×** of the checkpointed timeline,
- preserves **~72%** structural sharing (consecutive versions still share),
- and keeps navigation **`O(1)`** within the window.

So the standalone bounded store beats the standalone checkpointed timeline on
both speed and memory, while inside the dual-tracked `PaneExecutionEngine`
wrapper, `Checkpointed` avoids dual-substrate maintenance overhead.

## Debugging ergonomics & rollback posture

- **Rollback is trivial and always available.** `to_pane_tree()` flattens any
  version to the validated canonical structure; the execution-policy selector
  ([`PaneExecutionEngine`]) can switch between `Persistent`, `Checkpointed`,
  `Conservative`, and `Adaptive` at any cursor position (including mid-history
  with a live redo tail) and rejects invalid/baseline migrations atomically.
- **Inspection cost.** Persistent nodes are parent-free, so "who is my parent?"
  requires a flatten (or a top-down walk). Tooling that introspects parents
  (invariant reports, repair) already operates on `PaneTreeSnapshot`, so it keeps
  working unchanged via `to_snapshot()`. Direct interactive debugging of a raw
  `Arc<PersistentNode>` is slightly less convenient than the flat map — an
  acceptable cost given the always-available canonical view.
- **Determinism is preserved.** Version hashes, solved layout rects, `Frame`
  buffer cells, and `Presenter` ANSI outputs are byte-identical across all four
  live policies under equal retention budgets (verified by
  `pane_full_showcase_render_and_ansi_present_parity_across_policies_and_rollback`
  in `ftui-demo-showcase`).

## Recommendation & Live Adoption Decision (`bd-g00-root-epic-ewths.44`)

1. **Keep `Checkpointed` as the default strategy in `PaneExecutionEngine::new`**
   because the live wrapper maintains canonical `PaneTree` + `PaneInteractionTimeline`
   in all history modes and `Checkpointed` achieves the lowest end-to-end apply/undo
   latency across both 63-leaf and 255-leaf feasible layouts (`balanced_resize_storm`,
   `skewed_feasible_resize_storm`, and `balanced_mixed_session`).
2. **Expose `Persistent`, `Conservative`, and `Adaptive` via `PaneExecutionEngine::set_policy`**
   (and interactive `'e'` policy cycling in `LayoutLab`), guarded by bounded
   retention (`PaneRetentionPolicy`) and runtime assumption monitoring
   (`LocalityFraction`, `LatencyEnvelope`, `RetentionPressure`).
3. **Retain `Checkpointed` as the certified oracle and automatic fallback target**
   whenever `Adaptive` or `Persistent` observes non-local structural churn,
   latency envelope violations, or retention pressure.

## Prototype limitations (tracked, not normalized away)

- `MoveSubtree` / `NormalizeRatios` use the rebuild fallback (no sharing). A full
  adoption should implement native path-copy for `MoveSubtree`; `NormalizeRatios`
  rebuilds the whole tree by nature.
- Path-copy location is `O(nodes)` without a positional index.
- Pruning beyond the window discards undo reach into pruned versions (same
  semantics as the timeline's entry limit) — a deliberate, documented trade-off,
  not a silent gap.

[`PaneInteractionTimeline`]: ../../crates/ftui-layout/src/pane.rs
[`PaneExecutionEngine`]: ../../crates/ftui-layout/src/pane_execution.rs
[bd-1k7ek.6]: https://example.invalid/bd-1k7ek.6
[bd-25wj7.2]: https://example.invalid/bd-25wj7.2
[bd-25wj7.3]: https://example.invalid/bd-25wj7.3
[bd-1pvzq.3]: https://example.invalid/bd-1pvzq.3
[bd-1pvzq.4]: https://example.invalid/bd-1pvzq.4

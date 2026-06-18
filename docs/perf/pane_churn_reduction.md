# Pane-Core Churn Reduction — Compact Touched-Node Storage (bd-25wj7.3)

> Status: **measured · implemented · equivalence-proven**
>
> Code: `crates/ftui-layout/src/pane.rs` (`PaneOperationOutcome`/`PaneOperationError`,
> `validate_local_closure`, `apply_set_split_ratio_atomic`, `apply_operation_generic`)
> Evidence: `benches/pane_memory_telemetry.rs` (`fast_path_resize`)
> Proof: `pane::tests::touched_nodes_compact_storage_holds_correct_content` +
> `tests/pane_operation_family_equivalence.rs` (differential oracle)

## TL;DR

The `SetSplitRatio` fast path is the pane resize hot path (a live drag-resize
fires one per motion sample). The memory telemetry (bd-25wj7.1) flagged per-op
allocation churn there. This bead removes it entirely with one grounded lever —
**compact touched-node storage** — making the resize hot path **allocation-free**:

| `fast_path_resize` (512 `SetSplitRatio` ops on a plain tree) | Before | After |
|--------------------------------------------------------------|-------:|------:|
| total allocations | 1536 | **0** |
| allocations per op | 3.00 | **0.00** |
| bytes allocated | 61 440 | **0** |

No behavior changed: the certified differential oracle
(`pane_operation_family_equivalence`, fast vs. conservative over 48×40 histories)
and all 640 lib tests pass.

## The measured hotspot

Each `apply_set_split_ratio_atomic` call (the fast path) allocated three times:

1. `validate_after_operation(kind, &BTreeSet::from([split_id]))` — a heap
   `BTreeSet` built purely to hand the validator a single node id (the B-tree
   allocation accounted for two of the three).
2. `PaneOperationOutcome { touched_nodes: vec![split_id], .. }` — a 1-element
   heap `Vec`.

The validator (`validate_local_closure`) only *iterates* `touched`; it never
needs ordering or dedup. So the `BTreeSet` was pure overhead.

The dominant churn elsewhere (the conservative path's whole-tree `self.clone()`,
~20 MB over a resize storm in the baseline measurement) is **out of scope** here:
that is a representation/strategy shift owned by the persistence work
(bd-1k7ek.5) and gated by the bounded-retention policy (bd-25wj7.2). This bead
takes only the grounded, local lever the profile demanded.

## The lever

Two changes, both "compact touched-node storage":

1. **Validators take `&[PaneId]`** instead of `&BTreeSet<PaneId>`
   (`validate_local_closure`, `validate_after_operation`,
   `validate_after_operation_with_mode`). The fast path now passes a stack slice
   `&[split_id]` — zero allocation. The generic clone path collects its touched
   `BTreeSet` into the outcome's `SmallVec` once and passes that (it coerces to a
   slice), reusing it for validation and the outcome.
2. **`PaneOperationOutcome::touched_nodes` / `PaneOperationError::touched_nodes`
   become `SmallVec<[PaneId; 4]>`** — inline for the common small case (resize
   touches one node; most structural ops a handful), so building an outcome on
   the hot path no longer heap-allocates. `PaneOperationJournalEntry`
   (serialized) keeps its `Vec`; the SmallVec is converted at that boundary.

`SmallVec<[PaneId; 4]>` is a bounded, well-understood representation (4 inline,
heap spill beyond) — no opaque or unbounded storage policy is introduced, and the
change does not touch the bounded-retention policy or make rollback/debugging
harder (the public field is still a contiguous `PaneId` sequence).

## Why it stays correct

- `validate_local_closure` iterates `touched` identically whether it is a
  `BTreeSet` or a slice; the fast path always passes exactly the touched split.
- The generic and replay paths still build a `BTreeSet` for the in-place mutators
  (which need dedup), and collect to the compact `SmallVec` only for the outcome
  and validation. Error paths preserve their prior reported node sets.
- The differential oracle proves the fast and conservative paths accept/reject
  identically and produce identical trees; the new representation test locks the
  touched-node *content* across the structural, fast, and rejected paths.

## Reproduce

```bash
cargo bench -p ftui-layout --bench pane_memory_telemetry   # see fast_path_resize=0.00/op
cargo test  -p ftui-layout                                 # 640 lib + differential suites
```

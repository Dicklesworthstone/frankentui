# Pane Operation Families — Certified Fast Paths & Local-Closure Validation

**Beads:** `bd-1k7ek.3` (certified operation-family fast paths), `bd-1k7ek.4`
(incremental invariant-closure validation)
**Surface:** `crates/ftui-layout/src/pane.rs`
**Proof:** `crates/ftui-layout/tests/pane_operation_family_equivalence.rs`

---

## Why this exists

The profiling capture in `docs/profiling/bd-1y0ph-pane-hotspot-capture.md` ranked
**pane-core structural validation + replay internals** as the #1 optimization
lane. The dominant evidence was allocation-heavy table growth, VEB/layout-order
work, and validation/search traffic on the replay-heavy surface — costs that also
show up under terminal drag.

Drag interactions are overwhelmingly *local ratio changes* (`SetSplitRatio`): a
splitter moves and one split node's ratio is updated. Paying full-transaction
machinery — clone the entire tree, then revalidate the entire tree — for a
bounded one-node edit is wasteful. This document defines the operation-family
classification that lets local edits take a cheaper path **without weakening any
correctness guarantee**, and the proof that the cheaper path is observationally
equivalent to the conservative baseline.

---

## The classifier

Every operation is classified into a [`PaneOperationFamily`]:

| Family       | Operation kinds                                                          | Reach                                        |
| ------------ | ----------------------------------------------------------------------- | -------------------------------------------- |
| `Local`      | `SetSplitRatio`                                                         | One split node + its parent/child closure    |
| `Structural` | `SplitLeaf`, `CloseNode`, `MoveSubtree`, `SwapNodes`, `NormalizeRatios` | Potentially arbitrary regions of the tree    |

The family is the **single source of truth** for both the execution path and the
validation strategy, so the "escalation decision" for any operation is fully
recoverable from its `PaneOperationKind` (and therefore from the `kind` field
already recorded on `PaneOperationOutcome` / `PaneOperationError`):

```text
family == Local      => in-place atomic apply + local-closure validation
family == Structural => clone working tree + whole-tree validation
```

`PaneOperationKind::family()` and `PaneOperation::family()` expose the
classification publicly for logging and telemetry.

---

## The two paths

### Adaptive (default) — `PaneTree::apply_operation`

* `Local` (`SetSplitRatio`): `apply_set_split_ratio_atomic` mutates the target
  split's ratio **in place** and validates only the touched closure
  (`validate_local_closure`). On validation failure it restores the previous
  ratio, preserving the "no mutation on error" contract.
* `Structural`: clones a working tree, applies the mutation, runs the **whole-tree**
  validator (`validate`), and swaps the clone in only on success.

### Conservative (forced baseline) — `PaneTree::apply_operation_conservative`

Always clones a working tree and runs the whole-tree validator **regardless of
family** (`PaneValidationMode::AlwaysFull`). This is:

1. the easy-to-force conservative validator for diagnosis, rollback, and rollout
   (bd-1k7ek.4 acceptance criterion 4), and
2. the **differential oracle** the `Local` fast path is proven equivalent to.

Internally the mode is threaded through `validate_after_operation_with_mode`;
`Adaptive` defers to the family, `AlwaysFull` pins `FullTree`. The mode is *not*
stored on `PaneTree` (which derives `PartialEq`/`Eq`), so it never perturbs tree
equality, `state_hash`, or snapshot serialization.

---

## Isomorphism rationale

The fast path is sound because two equivalences hold for the `Local` family.

### 1. Application equivalence (in-place ≡ clone-then-mutate)

`SetSplitRatio` touches exactly one field: `split.ratio` on a single node. The
in-place path and the clone-based path execute the *same* mutation
(`apply_set_split_ratio` and `apply_set_split_ratio_atomic` construct identical
`PaneOperationFailure` variants and the identical post-state). The only
observable contract is "on error, the tree is unchanged":

* `MissingNode` / `ParentNotSplit`: detected before any field write, so neither
  path mutates.
* validation failure: the in-place path explicitly restores `previous_ratio`;
  the clone-based path simply discards the working clone.

Both therefore leave `self` byte-identical to its pre-call state on rejection,
and byte-identical to the conservative result on acceptance — including the
returned `before_hash` / `after_hash` / `touched_nodes`.

### 2. Validation equivalence (local closure ≡ whole tree, for the Local family)

Whole-tree validation checks, for every node: constraint validity, root/parent
consistency, split-child wellformedness, and ratio positivity. On a tree that was
valid *before* the operation, `SetSplitRatio` changes **only** one split's ratio.
No parent pointer, child set, leaf, or topology changes. Therefore the *only*
invariant that can transition from satisfied to violated is the ratio positivity
of the touched split — which `validate_local_closure` checks for exactly that
node. Every other whole-tree check is evaluating structure the operation did not
touch, and so cannot newly fail.

Hence, for `Local` operations on a previously-valid tree, the local-closure
verdict equals the whole-tree verdict. When that precondition is *not* met (a
mutation family whose reach is not provably bounded), the classifier routes to
`Structural` → whole-tree validation, i.e. it **escalates conservatively** rather
than smuggling optimism into correctness.

> A corruption *outside* the touched closure is, by construction, invisible to
> local-closure validation — that is the whole point of locality. The unit test
> `always_full_validation_mode_catches_corruption_outside_touched_closure`
> demonstrates this and confirms `AlwaysFull` still catches it, which is why the
> conservative override exists.

---

## Empirical evidence

### Differential proofs (`tests/pane_operation_family_equivalence.rs`)

* `adaptive_and_conservative_paths_agree_across_random_histories` — 48 seeds × 40
  ops in lockstep across **all** families; identical outcomes and snapshots at
  every step.
* `set_split_ratio_fast_path_matches_conservative_baseline_for_every_split` — 64
  seeds, every split, 4 ratios each; fast path == conservative baseline in both
  the returned result and the resulting tree, and accepted mutations keep the
  tree globally valid.
* `set_split_ratio_counterexamples_reject_identically_on_both_paths` — adversarial
  fixtures (missing node, non-split target) reject with identical reasons and an
  unchanged tree.

Unit tests (`crates/ftui-layout/src/pane.rs`): `operation_family_classifier_*`,
`validation_strategy_is_derived_from_operation_family`,
`always_full_validation_mode_catches_corruption_outside_touched_closure`.

### Benchmark delta (`pane/core/apply_operation`)

Measured on a 32-leaf tree (63 nodes), Criterion median:

| Path                            | Median   | vs conservative |
| ------------------------------- | -------- | --------------- |
| `set_split_ratio_fast`          | ~11.6 µs | **~2.05× faster** |
| `set_split_ratio_conservative`  | ~23.8 µs | baseline        |

The win comes from skipping the 63-node working-tree clone and the whole-tree
validation pass in favor of an in-place field write plus a touched-closure check.

Reproduce:

```bash
cargo bench -p ftui-layout --bench layout_bench -- \
  'pane/core/apply_operation/set_split_ratio' \
  --warm-up-time 0.3 --measurement-time 1.0 --sample-size 30
```

---

## Extending the Local family

To certify a new `Local` operation:

1. Prove its reach is bounded to a touched closure that `validate_local_closure`
   covers (application equivalence + validation equivalence above).
2. Add it to `PaneOperationKind::family()`'s `Local` arm.
3. Give it an in-place atomic apply path with rollback-on-error.
4. Extend the differential corpus in
   `tests/pane_operation_family_equivalence.rs` so the fast path is proven
   identical to `apply_operation_conservative`.

If any step is uncertain, leave the kind `Structural`. Conservative escalation is
always sound; an unproven fast path is not.

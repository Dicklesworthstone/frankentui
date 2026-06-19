# Migration Guide — Flex/Grid → Panes, and Persisted-Workspace Versioning

For maintainers moving an existing FrankenTUI screen onto the pane workspace
system, plus the versioning/rollback playbook for persisted workspaces. Aligns
with the project's modernization philosophy: **no compatibility shims — modernize
directly** (see [`docs/migration-map.md`](../migration-map.md)).

Companion docs: [Pane 101](../guides/pane-101.md),
[cookbook](../cookbook/panes.md),
[stability contract](../api/pane-stability-contract.md).

---

## 1. Should this screen use panes at all?

Panes add an interaction model (user-driven split/resize/dock/persist). If you
do not need that, the `Flex`/`Grid` solvers are simpler, cheaper, and have no
gesture state to manage.

```
Does the USER reshape the layout at runtime
(drag a splitter, split/close regions, dock, persist their arrangement)?
│
├── No  → keep ftui::layout::Flex / Grid. Done.
│
└── Yes → use ftui::pane.
          │
          ├── Fixed set of regions, user only drags the dividers?
          │     → a PaneTree built once + drag-resize adapter.
          │
          └── User adds/removes/reorders regions and you save the layout?
                → full PaneTree + PaneInteractionTimeline + VersionedPaneTree.
```

Do **not** wrap `Flex` in a pane shim or keep both in parallel "just in case."
Pick one per screen and modernize the call sites directly.

---

## 2. Pattern mapping (Flex → pane)

| Legacy `Flex`/`Grid` pattern | Pane equivalent |
|------------------------------|-----------------|
| `Flex::horizontal([Constraint::Percentage(30), Percentage(70)]).split(area)` | `PaneTree::singleton(..)` + one `SplitLeaf { axis: Horizontal, ratio: 3/10, .. }`, then `tree.solve_layout(area)` |
| `Flex::vertical([..])` | `SplitLeaf { axis: Vertical, .. }` |
| `Constraint::Ratio(n, d)` | `PaneSplitRatio::new(n, d)` on the split |
| `Constraint::Fixed(k)` / `Min`/`Max` | `PaneConstraints` (min/max per pane); ratios still drive division |
| Nested `Flex` inside a chunk | nested `SplitLeaf` on that leaf |
| Indexing `rects[i]` | `layout.iter()` / `layout.rect(pane_id)` keyed by `PaneId` |
| Recompute layout each frame | `tree.solve_layout(area)` each frame (pure, deterministic) |
| Hand-rolled "drag the divider" code | feed events to the host adapter → `tree.operations_for_transition(..)` |
| Hand-rolled undo of layout edits | `PaneInteractionTimeline` |
| Serializing your own layout struct | `VersionedPaneTree` (carries the schema version) |

### Before

```rust,ignore
let cols = Flex::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)])
    .split(area);
render_sidebar(frame, cols[0]);
render_main(frame, cols[1]);
```

### After

```rust,ignore
use ftui::pane::prelude::*;
use ftui::pane::{PaneLeaf, PanePlacement, PaneSplitRatio, PaneNodeKind};

// built once (e.g. in init), not per frame:
let mut tree = PaneTree::singleton("sidebar");
let root = tree.nodes().find(|n| matches!(n.kind, PaneNodeKind::Leaf(_)))
    .map(|n| n.id).unwrap();
tree.apply_operation(1, PaneOperation::SplitLeaf {
    target: root,
    axis: SplitAxis::Horizontal,
    ratio: PaneSplitRatio::new(3, 10).unwrap(),
    placement: PanePlacement::ExistingFirst,
    new_leaf: PaneLeaf::new("main"),
}).unwrap();

// per frame:
for (id, rect) in tree.solve_layout(area).iter() {
    if let Some(PaneNodeKind::Leaf(leaf)) = tree.node(id).map(|n| &n.kind) {
        match leaf.surface_key.as_str() {
            "sidebar" => render_sidebar(frame, rect),
            "main"    => render_main(frame, rect),
            _ => {}
        }
    }
}
```

The pane version costs more than `Flex` for a static 30/70 split — only adopt
it when the user controls the split.

---

## 3. Changed assumptions (read before you port)

- **Identity is a `PaneId`, not an index.** Anything that referenced `rects[0]`
  must key off a stable `PaneId` or `surface_key`. Indices are not stable across
  splits/closes.
- **The tree is the source of truth.** Do not cache rectangles across frames;
  re-solve. Caching is the renderer's job (the kernel already diffs).
- **Mutations go through operations.** Never mutate the tree by hand; use
  `PaneOperation` so invariants, undo, and parity all hold.
- **Min/max are constraints, not guarantees of pixels.** Under extreme
  shrinkage the solver honors `PaneConstraints` deterministically; verify with
  `tree.validate()`.
- **Determinism is contractual.** `solve_layout` and operation application are
  deterministic; if you introduce nondeterminism (time, RNG) in content, keep
  it out of the layout inputs.

---

## 4. Persisted-workspace versioning

Persisted layouts are versioned by `PANE_TREE_SCHEMA_VERSION` (currently `1`).
The rules:

1. **Always store the schema version with the snapshot.**
   `VersionedPaneTree::from_pane_tree(&tree)` embeds it.
2. **Refuse what you do not understand.** Loading a snapshot whose version your
   build does not support returns a typed `PersistentApplyError` — never a
   silently corrupted tree.
3. **Choose reconciliation explicitly** via `PersistentApplyStrategy` (replace /
   merge / validate-only) when applying a loaded snapshot to a live tree.
4. **Bound history deterministically** with `PaneVersionRetention`; the current
   head is never pruned.

### When `PANE_TREE_SCHEMA_VERSION` bumps

A bump means the persisted shape changed incompatibly. The migration path:

1. Read the old snapshot with a build that still understands version `N`.
2. Re-serialize through `VersionedPaneTree` on a build at version `N+1` (the
   upgrade is applied on load when supported).
3. If a field cannot be auto-mapped, surface it as a typed
   `PersistentApplyError` and fall back to a fresh default workspace rather than
   guessing — never partially apply.

The migration test corpus (`bd-14edr`) exercises round-trips; extend it when you
change the schema.

---

## 5. Rollback / fallback during integration

- **Behind a flag.** Ship the pane version of a screen behind a runtime toggle;
  keep the `Flex` path importable in the same build until the pane path passes
  the [release gates](../../docs/pane-release-gate-policy.md). Remove the old
  path once green — do not keep it forever.
- **Corrupt/unknown snapshot → default workspace.** On `PersistentApplyError`,
  log it and start from `PaneTree::singleton(..)`; never crash the screen.
- **Parity divergence → terminal is canonical.** If terminal and web disagree
  (the parity runner emits a JSON diff artifact), the terminal semantics are
  authoritative; fix the web adapter, do not fork the model.

---

## 6. Known hazards & mitigations

| Hazard | Symptom | Mitigation |
|--------|---------|-----------|
| Index-based rendering left in place | wrong content after a split/close | key rendering off `PaneId`/`surface_key`, not order |
| Rectangle cached across frames | stale layout after resize | re-solve every frame |
| Direct tree mutation | invariant/undo/parity break | only mutate via `PaneOperation` |
| Hand-rolled drag math | terminal/web divergence | use the host adapter + `operations_for_transition` |
| Loading an old snapshot blindly | corruption / panic | check schema version, handle `PersistentApplyError` |
| Unbounded version history | memory growth in long sessions | set `PaneVersionRetention` budgets |

---

## 7. Operator troubleshooting (artifact bundles)

When a migrated screen misbehaves in CI or a soak run, the pane gates emit
structured artifacts — start there:

- **Parity divergence:** the cross-host parity runner writes a JSON diff
  (op/hash/cursor/strategy + first-divergence). Source:
  `crates/ftui-web/tests/pane_cross_host_parity.rs`.
- **Soak/rollback:** `scripts/pane_soak_rollback.sh` writes operator-grade JSONL
  (per-round strategy, assumption violations, rollback events, state hashes) to
  `target/pane-soak/`. See [`docs/perf/pane_soak_rollback.md`](../perf/pane_soak_rollback.md).
- **Replay/perf:** `scripts/pane_replay_artifacts.py` produces a checksummed
  symbolized replay index; certify against the golden oracle. See
  [`docs/perf/pane_replay_artifacts.md`](../perf/pane_replay_artifacts.md).

The full incident playbook is in the
[operational runbook](../../docs/pane-operational-runbook.md).
</content>

# Pane 101 — Getting Started with Pane Workspaces

A guided introduction to the FrankenTUI pane workspace system: split trees,
structural operations, interactive resize, persistence, and keyboard control.
Everything here uses the stable surface at `ftui::pane`
(see the [stability contract](../api/pane-stability-contract.md)).

> **When do I need panes?** Reach for panes when the *user* controls the
> layout — split, drag-to-resize, dock, collapse, reorder, persist. For
> static or app-controlled layouts, the `Flex`/`Grid` solvers in
> `ftui::layout` are simpler and cheaper. See the
> [migration guide](../migration/flex-to-pane-and-versioning.md) for the
> decision tree.

---

## 1. The mental model

A pane workspace is a **binary split tree**:

```
            Split(Horizontal, ratio=1/2)
            ├── Leaf "editor"
            └── Split(Vertical, ratio=2/3)
                ├── Leaf "terminal"
                └── Leaf "log"
```

- **Leaves** hold your content (identified by a stable `surface_key` string).
- **Splits** divide space along an axis (`Horizontal` or `Vertical`) at a
  ratio.
- Every node has a `PaneId`. The tree is always acyclic and structurally
  valid — `tree.validate()` proves it after any mutation.

You change the tree in two ways:

1. **Structural operations** (`PaneOperation`) — discrete, undoable edits:
   split a leaf, close a node, move/swap subtrees, set a ratio.
2. **Semantic interaction events** (`PaneSemanticInputEvent`) — the
   host-agnostic pointer/keyboard stream that drives *live* drag-resize via the
   `PaneDragResizeMachine`. The terminal and web hosts feed the same events, so
   behavior is identical across hosts.

---

## 2. Build a tree and solve a layout

```rust
use ftui::pane::prelude::*;
use ftui::core::geometry::Rect;

// One full-screen pane to start.
let tree = PaneTree::singleton("root");

// Solve the layout for an 80x24 viewport -> rectangles per pane id.
let area = Rect::new(0, 0, 80, 24);
let layout = tree.solve_layout(area);
for (id, rect) in layout.iter() {
    // render your content for `id` into `rect`
    let _ = (id, rect);
}
```

`solve_layout` is pure and deterministic: the same tree and area always produce
the same rectangles.

---

## 3. Split a pane

Operations are applied with a monotonically increasing operation id (used for
the timeline/undo history):

```rust
use ftui::pane::prelude::*;
use ftui::pane::{PaneLeaf, PanePlacement, PaneSplitRatio, PaneNodeKind};

let mut tree = PaneTree::singleton("root");

// The id of the only leaf.
let root_leaf = tree
    .nodes()
    .find(|n| matches!(n.kind, PaneNodeKind::Leaf(_)))
    .map(|n| n.id)
    .expect("singleton has a leaf");

// Split it left/right, 50/50, putting the new pane on the right.
let outcome = tree.apply_operation(
    1,
    PaneOperation::SplitLeaf {
        target: root_leaf,
        axis: SplitAxis::Horizontal,
        ratio: PaneSplitRatio::new(1, 2).expect("valid ratio"),
        placement: PanePlacement::ExistingFirst,
        new_leaf: PaneLeaf::new("inspector"),
    },
);
assert!(outcome.is_ok());
debug_assert!(tree.validate().is_ok());
```

Other operations: `PaneOperation::CloseNode { target }`,
`MoveSubtree { source, target, axis, ratio, placement }`,
`SwapNodes { first, second }`, `SetSplitRatio { split, ratio }`, and
`NormalizeRatios`.

---

## 4. Interactive drag-to-resize

You usually do not build `SetSplitRatio` operations by hand during a drag.
Instead you feed pointer events to a `PaneDragResizeMachine` (terminal: via
`ftui-runtime`'s `PaneTerminalAdapter`; web: via `ftui-web`'s
`PanePointerCaptureAdapter`). The machine recognizes the gesture, and the tree
turns the resulting transition into operations via
`operations_for_transition(...)`. This is the path the demo screens use; the
[cookbook](../cookbook/panes.md#recipe-add-drag-to-resize-to-a-screen) has the
end-to-end wiring for both hosts.

The key property: a given semantic event stream produces a deterministic
sequence of operations, regardless of host. That is what the cross-host parity
runner verifies.

---

## 5. Intelligence modes

`PaneLayoutIntelligenceMode` offers preset arrangements — `Focus`, `Compare`,
`Monitor`, `Compact` — that the Dashboard and Layout Lab demos cycle through
with right-click or a keybinding. They are layout presets over the same tree,
not a separate model.

---

## 6. Persist a workspace

`VersionedPaneTree` snapshots a tree for save/restore across sessions, carrying
the `PANE_TREE_SCHEMA_VERSION` so old snapshots are rejected (never silently
corrupted) rather than misread:

```rust
use ftui::pane::prelude::*;
use ftui::pane::PANE_TREE_SCHEMA_VERSION;

let tree = PaneTree::singleton("root");
let snapshot = VersionedPaneTree::from_pane_tree(&tree);
assert_eq!(PANE_TREE_SCHEMA_VERSION, 1);
// Persist `snapshot` via a `PaneVersionStore`; bounded history via
// `PaneVersionRetention`. See the cookbook for the full round-trip.
```

---

## 7. Keyboard control (terminal)

With the `runtime` feature, `ftui::pane::keyboard` gives you a
`PaneKeyboardController` that maps `KeyEvent`s to `PaneCommand`s (focus
navigation, split, close, maximize, resize), renders a focus ring, and emits
accessibility announcements:

```rust,ignore
use ftui::pane::keyboard::{PaneKeyboardController, pane_keyboard_hints};

let mut kb = PaneKeyboardController::new(Some(active_pane_id));
let outcome = kb.handle_key(&key_event, &mut tree);
for hint in pane_keyboard_hints() {
    // show `hint` in your help bar
}
```

The web host binds keys through `ftui-web::pane_keyboard` (browser-safe: it
refuses Ctrl/Super so it never hijacks browser shortcuts) and exposes a roving
`tabindex` + ARIA accessibility tree.

---

## 8. Where to go next

- [Pane cookbook](../cookbook/panes.md) — task-oriented recipes.
- [Stability contract](../api/pane-stability-contract.md) — what is supported,
  schema versions, and the deprecation policy.
- [Migration guide](../migration/flex-to-pane-and-versioning.md) — Flex/Grid →
  panes, and persisted-workspace versioning.
- [Parity contract](../spec/pane-parity-contract-and-program.md) — the
  terminal/web behavior guarantee.
- Run `cargo run -p ftui-demo-showcase` and open **Layout Lab**, **Dashboard**,
  or **Widget Gallery** to see panes live.
</content>

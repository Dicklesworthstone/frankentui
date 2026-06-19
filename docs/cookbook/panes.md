# Pane Cookbook

Task-oriented recipes for the pane workspace system. Each recipe targets the
stable surface at `ftui::pane` (see the
[stability contract](../api/pane-stability-contract.md)). New to panes? Start
with [Pane 101](../guides/pane-101.md).

Recipes:
- [Build a multi-pane workspace from scratch](#recipe-build-a-multi-pane-workspace)
- [Add drag-to-resize to a screen](#recipe-add-drag-to-resize-to-a-screen)
- [Undo / redo with the interaction timeline](#recipe-undo--redo)
- [Persist and restore a workspace](#recipe-persist-and-restore-a-workspace)
- [Wire keyboard control + a focus ring](#recipe-keyboard-control)
- [Prove terminal/web parity for a custom scenario](#recipe-prove-parity)
- [Tune snap/dock pressure](#recipe-tune-snapdock)

---

## Recipe: Build a multi-pane workspace

Compose a workspace by applying `PaneOperation`s to a singleton. Operation ids
are monotonic and feed the undo timeline.

```rust
use ftui::pane::prelude::*;
use ftui::pane::{PaneLeaf, PanePlacement, PaneSplitRatio, PaneNodeKind};

fn leaf_id(tree: &PaneTree, key_hint_index: usize) -> PaneId {
    tree.nodes()
        .filter(|n| matches!(n.kind, PaneNodeKind::Leaf(_)))
        .nth(key_hint_index)
        .map(|n| n.id)
        .expect("leaf exists")
}

let mut tree = PaneTree::singleton("editor");
let half = PaneSplitRatio::new(1, 2).expect("valid");

// Split editor | sidebar.
let editor = leaf_id(&tree, 0);
tree.apply_operation(1, PaneOperation::SplitLeaf {
    target: editor,
    axis: SplitAxis::Horizontal,
    ratio: half,
    placement: PanePlacement::ExistingFirst,
    new_leaf: PaneLeaf::new("sidebar"),
}).expect("split ok");

// Split the sidebar into sidebar / terminal (top/bottom).
let sidebar = leaf_id(&tree, 1);
tree.apply_operation(2, PaneOperation::SplitLeaf {
    target: sidebar,
    axis: SplitAxis::Vertical,
    ratio: PaneSplitRatio::new(2, 3).expect("valid"),
    placement: PanePlacement::ExistingFirst,
    new_leaf: PaneLeaf::new("terminal"),
}).expect("split ok");

debug_assert!(tree.validate().is_ok());
```

Render by solving the layout each frame:

```rust,ignore
use ftui::pane::PaneNodeKind;

for (id, rect) in tree.solve_layout(frame.area()).iter() {
    if let Some(PaneNodeKind::Leaf(leaf)) = tree.node(id).map(|n| &n.kind) {
        match leaf.surface_key.as_str() {
            "editor"   => render_editor(frame, rect),
            "terminal" => render_terminal(frame, rect),
            _ => {}
        }
    }
}
```

---

## Recipe: Add drag-to-resize to a screen

Both hosts feed the **same** semantic gesture pipeline; only the adapter that
produces it differs. The canonical wiring lives in the demo screens
(`crates/ftui-demo-showcase/src/screens/layout_lab.rs`) and is exercised by
`crates/ftui-harness/tests/pane_splitter_drag_pty_e2e.rs` (terminal) and
`crates/ftui-web/tests/pane_web_e2e.rs` (web).

**Terminal** — own a `PaneTerminalAdapter`, translate raw events, then turn the
resulting transition into operations:

```rust,ignore
use ftui::pane::prelude::*;
use ftui::pane::PanePressureSnapProfile;
use ftui_runtime::{PaneTerminalAdapter, PaneTerminalAdapterConfig};

// once, at construction:
let mut adapter = PaneTerminalAdapter::new(PaneTerminalAdapterConfig::default())?;

// per input event (e.g. inside update()):
let dispatch = adapter.translate(&event, target_hint);
if let Some(transition) = dispatch.transition() {
    let layout = tree.solve_layout(area);
    for op in tree.operations_for_transition(
        transition, &layout, PanePressureSnapProfile::neutral(),
    ) {
        tree.apply_operation(next_op_id(), op).ok();
    }
}
```

**Web** — the `PanePointerCaptureAdapter` + `PaneCoordinateNormalizer` in
`ftui-web` normalize browser pointer/touch events (DPR, zoom, viewport origin)
into the identical `PaneSemanticInputEvent` stream, so the
`operations_for_transition` half is the same.

The guarantee: an equivalent gesture produces an equivalent operation sequence
on both hosts — verified by the cross-host parity runner.

---

## Recipe: Undo / redo

The `PaneInteractionTimeline` records applied operations so you can step back
and forward, and replay deterministically. Use it instead of hand-rolling an
undo stack — it preserves the structural-validity invariant at every step and
supports checkpoint-based retention for long sessions.

See `PaneInteractionTimeline`, `PaneInteractionTimelineEntry`, and the
checkpoint integration test
`crates/ftui-layout/tests/pane_checkpoint_integration.rs` for the supported
API and the replay-determinism guarantees.

---

## Recipe: Persist and restore a workspace

```rust
use ftui::pane::prelude::*;
use ftui::pane::PANE_TREE_SCHEMA_VERSION;

// Snapshot for save.
let tree = PaneTree::singleton("root");
let snapshot = VersionedPaneTree::from_pane_tree(&tree);

// `snapshot` is serializable and carries the schema version; persist it with
// your storage backend. A `PaneVersionStore` keeps bounded history via
// `PaneVersionRetention` (it never prunes the current head).
assert_eq!(PANE_TREE_SCHEMA_VERSION, 1);
```

On load, a snapshot whose schema version your build does not understand is
rejected with a typed `PersistentApplyError` rather than silently
misinterpreted. When `PANE_TREE_SCHEMA_VERSION` bumps, follow the migration
path in the
[migration guide](../migration/flex-to-pane-and-versioning.md#persisted-workspace-versioning).

---

## Recipe: Keyboard control

With the `runtime` feature, drive panes entirely from the keyboard:

```rust,ignore
use ftui::pane::keyboard::{PaneKeyboardController, pane_keyboard_hints};

let mut kb = PaneKeyboardController::new(Some(active_id));

// in update():
let outcome = kb.handle_key(&key_event, &mut tree);
if let Some(announcement) = kb.take_announcement() {
    // forward to your live region / status line for screen readers
}

// in view(): draw the focus ring + hints
let ring = kb.focus_ring(&theme);
ftui_runtime::pane_keymap::render_pane_focus_ring(frame, focused_rect, &ring);
for hint in pane_keyboard_hints() { /* render help bar */ }
```

Bindings (terminal): `Tab`/`Shift+Tab` cycle focus, `Ctrl+Arrow` directional
focus, `Alt+s` split, `Alt+w` close, `Alt+z` maximize, arrows/`+`/`-` resize.
The web host uses `ftui-web::pane_keyboard`, which refuses `Ctrl`/`Super` so it
never collides with browser shortcuts, and maintains a roving `tabindex` + ARIA
tree.

---

## Recipe: Prove parity

To assert a custom scenario behaves identically on terminal and web, add it to
the canonical corpus consumed by
`crates/ftui-web/tests/pane_cross_host_parity.rs`. The runner drives the same
host-neutral gesture through both adapters in-process and compares topology
hash, applied-operation sequence, and settle state, normalizing intentional
host differences. A divergence emits a structured JSON diff artifact for
triage. The contract is specified in
[`docs/spec/pane-parity-contract-and-program.md`](../spec/pane-parity-contract-and-program.md).

---

## Recipe: Tune snap/dock

`PaneSnapTuning` and `PanePressureSnapProfile` control magnetic snapping during
drags (snap step, hysteresis, pressure derived from drag speed). Stronger
pressure → crisper snapping to dock zones (`PaneDockZone`); weaker → smoother
free resize. The Dashboard demo exposes scroll-wheel field tuning so you can
feel the effect interactively. Keep hysteresis non-zero to avoid single-cell
jitter at zone boundaries.

---

## See also

- [Pane 101](../guides/pane-101.md) — guided introduction.
- [Stability contract](../api/pane-stability-contract.md) — supported surface.
- [Migration guide](../migration/flex-to-pane-and-versioning.md) — Flex/Grid → panes.
- `docs/perf/pane_*.md` — performance/observability internals (advanced tier).
</content>

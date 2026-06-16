#![forbid(unsafe_code)]

//! Live pane PTY harness (bd-x9lqw acceptance #3, extended for bd-a46q1.3).
//!
//! This binary runs the **real** terminal runtime ([`ftui_runtime::App`] /
//! `Program`) with a single root split and drives the production pane interaction
//! paths end-to-end over a genuine PTY. It started life proving splitter *drag*
//! resize and now also exercises the other terminal-input modalities the
//! `PaneTerminalAdapter` supports, so the PTY E2E suite can validate them against
//! the production code path rather than an in-process stub:
//!
//! ```text
//! crossterm input bytes (over a real PTY)
//!   -> ftui_core::Event (Mouse / Key / Scroll)
//!   -> PaneTerminalAdapter::translate{_with_handles}   (terminal hit-testing)
//!   -> PaneDragResizeTransition
//!   -> PaneTree::operations_for_transition             (fixed-pressure bridge)
//!   -> PaneTree::apply_operation                       (live tree mutation)
//! ```
//!
//! Covered terminal-input paths (all genuine `PaneTerminalAdapter` contracts):
//!
//! - **Pointer drag** resize: SGR mouse down/drag/up over the splitter handle.
//! - **Keyboard** resize: arrow keys / `+` / `-` (with `Shift` = 5x step). The
//!   host (this harness) resolves the splitter target from the rendered handles
//!   and feeds it through the documented `translate(event, target_hint)` contract
//!   — exactly how a focus-aware host wires keyboard resize.
//! - **Wheel** nudge resize: SGR scroll over the splitter handle.
//! - **Escape** recovery: cancels an armed/active pointer interaction.
//!
//! Plus harness **affordance** keys for the structural operations the production
//! terminal adapter does *not yet* bind to input (that input binding is tracked
//! by bd-21pbi.2). These drive the operation -> render -> teardown stack over a
//! real PTY so we still get end-to-end coverage of split/close/swap behaviour:
//!
//! - `s` -> split the left leaf, `c` -> close the right leaf, `w` -> swap leaves.
//!
//! After the runtime tears down and the terminal is restored, the harness prints
//! a single deterministic, greppable result marker to stdout:
//!
//! ```text
//! PANE_RESULT mode=alt initial_bps=5000 final_bps=8000 applied_ops=3 \
//!     down_resolved=true committed=true tree_valid=true node_count=3 \
//!     first_leaf=left canceled=false
//! ```
//!
//! Determinism note: the realistic resize path derives snap *pressure* from
//! pointer motion (and therefore from wall-clock `Instant`). To keep the
//! reported ratio byte-for-byte reproducible across runs and identical between
//! screen modes, this harness applies transitions with a FIXED neutral pressure
//! profile -- exactly like the `apply_dispatch_fixed` helper in the
//! `ftui-runtime` adapter tests. Keyboard and wheel nudges step by a fixed
//! `PANE_SNAP_DEFAULT_STEP_BPS` per unit/line and are geometry-independent, so
//! their reported ratios are exact and identical across screen modes.
//!
//! ## Production keyboard-binding mode (bd-8e1oc)
//!
//! With `PANE_HARNESS_INPUT=keymap`, `Event::Key` is instead routed through the
//! **production** terminal keyboard binding
//! (`ftui_runtime::pane_keymap::PaneKeyboardController` — key -> `PaneCommand`
//! -> resolve -> apply, bd-21pbi.2), not the affordance/adapter paths. The
//! controller starts focused on the left leaf; the scripted test sends a key
//! sequence then `q`, and the marker additionally reports `active_pane=<name>`
//! (keyboard focus navigation) and `maximized=<bool>`. This proves split / close
//! / move / swap / focus-nav via the real input binding end-to-end over a PTY.
//!
//! ## Environment
//!
//! - `PANE_HARNESS_SCREEN_MODE` -- `inline` | `alt` (default `alt`).
//! - `PANE_HARNESS_INPUT` -- `adapter` (default) | `keymap` (production keyboard).
//! - `PANE_HARNESS_AXIS` -- root split axis: `horizontal` (default) | `vertical`.
//! - `PANE_HARNESS_UI_HEIGHT`   -- inline UI height in rows (default `12`).
//! - `PANE_HARNESS_EXIT_AFTER_MS` -- safety auto-quit if no gesture arrives
//!   (default `4000`). The harness normally exits as soon as a drag commits, a
//!   structural affordance is applied, or an interaction is canceled.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_layout::{
    PANE_TREE_SCHEMA_VERSION, PaneCommandEffect, PaneId, PaneLayout, PaneLeaf, PaneNodeKind,
    PaneNodeRecord, PaneOperation, PanePlacement, PanePressureSnapProfile, PaneSplit,
    PaneSplitRatio, PaneTree, PaneTreeSnapshot, SplitAxis,
};
use ftui_render::frame::Frame;
use ftui_runtime::pane_keymap::{PaneKeyOutcome, PaneKeyboardController};
use ftui_runtime::{
    App, Cmd, Every, Model, PaneTerminalAdapter, PaneTerminalAdapterConfig, PaneTerminalDispatch,
    PaneTerminalLifecyclePhase, ScreenMode, Subscription, UiAnchor, pane_terminal_splitter_handles,
};
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;

/// Root split node id.
const ROOT: u64 = 1;
/// Left (first) leaf id.
const LEFT: u64 = 2;
/// Right (second) leaf id.
const RIGHT: u64 = 3;

/// Splitter hit-test thickness (cells). Deliberately generous so a click aimed
/// at the boundary column lands inside the handle regardless of any +/-1
/// coordinate-convention differences between SGR and the layout grid.
const HIT_THICKNESS: u16 = 7;

/// Fixed, timing-independent pressure-snap profile (matches the `ftui-runtime`
/// adapter tests' `fixed_neutral_pressure`). Using a constant pressure makes the
/// reported ratio a deterministic function of the pointer position alone.
const FIXED_PRESSURE: PanePressureSnapProfile = PanePressureSnapProfile {
    strength_bps: 5_000,
    hysteresis_bps: 100,
};

fn pane_id(raw: u64) -> PaneId {
    PaneId::new(raw).expect("non-zero pane id")
}

/// Build a single root split (`left | right` or `left / right`) at a 1:1 ratio.
fn root_split_tree(axis: SplitAxis) -> PaneTree {
    let snapshot = PaneTreeSnapshot {
        schema_version: PANE_TREE_SCHEMA_VERSION,
        root: pane_id(ROOT),
        next_id: pane_id(4),
        nodes: vec![
            PaneNodeRecord::split(
                pane_id(ROOT),
                None,
                PaneSplit {
                    axis,
                    ratio: PaneSplitRatio::new(1, 1).expect("valid ratio"),
                    first: pane_id(LEFT),
                    second: pane_id(RIGHT),
                },
            ),
            PaneNodeRecord::leaf(pane_id(LEFT), Some(pane_id(ROOT)), PaneLeaf::new("left")),
            PaneNodeRecord::leaf(pane_id(RIGHT), Some(pane_id(ROOT)), PaneLeaf::new("right")),
        ],
        extensions: BTreeMap::new(),
    };
    PaneTree::from_snapshot(snapshot).expect("valid root split tree")
}

/// First-child share (basis points) of the root split's ratio, or `None` when the
/// root is no longer a split node (e.g. after a close promotes a sibling to root).
fn root_first_share_bps(tree: &PaneTree) -> Option<u32> {
    match &tree.node(pane_id(ROOT))?.kind {
        PaneNodeKind::Split(node) => Some(
            node.ratio.numerator() * 10_000 / (node.ratio.numerator() + node.ratio.denominator()),
        ),
        PaneNodeKind::Leaf(_) => None,
    }
}

/// Surface key of the root split's first child *if* it is a leaf, else `-`.
/// Lets a test observe leaf ordering (so a swap is provably a swap, not a no-op).
fn root_first_leaf_name(tree: &PaneTree) -> String {
    let Some(root) = tree.node(pane_id(ROOT)) else {
        return "-".to_string();
    };
    let PaneNodeKind::Split(split) = &root.kind else {
        return "-".to_string();
    };
    match tree.node(split.first).map(|node| &node.kind) {
        Some(PaneNodeKind::Leaf(leaf)) => leaf.surface_key.clone(),
        _ => "-".to_string(),
    }
}

/// Surface key of a pane node if it is a leaf, else `-`.
fn leaf_name(tree: &PaneTree, id: PaneId) -> String {
    match tree.node(id).map(|node| &node.kind) {
        Some(PaneNodeKind::Leaf(leaf)) => leaf.surface_key.clone(),
        _ => "-".to_string(),
    }
}

/// Mutable state shared between the model and `main` so the result can be
/// emitted *after* the terminal is restored. Also carries the most recent
/// rendered pane area for hit-testing (set from `view`, read from `update`).
#[derive(Clone)]
struct Shared {
    mode: String,
    area: Rect,
    initial_bps: u32,
    final_bps: u32,
    applied_ops: u64,
    down_resolved: bool,
    committed: bool,
    canceled: bool,
    tree_valid: bool,
    node_count: usize,
    first_leaf: String,
    active_pane: String,
    maximized: bool,
}

struct Harness {
    tree: PaneTree,
    adapter: PaneTerminalAdapter,
    /// Production keyboard binding controller, present only in `keymap` input
    /// mode (`PANE_HARNESS_INPUT=keymap`). When present, `Event::Key` is routed
    /// through the real `ftui_runtime::pane_keymap` path instead of the adapter
    /// resize / affordance paths.
    keyboard: Option<PaneKeyboardController>,
    op_seed: u64,
    ticks_remaining: u32,
    shared: Arc<Mutex<Shared>>,
}

enum Msg {
    Input(Event),
    Tick,
    Quit,
}

impl From<Event> for Msg {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Char('q'),
                kind: KeyEventKind::Press,
                ..
            }) => Msg::Quit,
            other => Msg::Input(other),
        }
    }
}

impl Harness {
    fn with_shared<R>(&self, f: impl FnOnce(&mut Shared) -> R) -> Option<R> {
        self.shared.lock().ok().map(|mut guard| f(&mut guard))
    }

    /// Run one raw terminal event through the full production resize path.
    fn handle_event(&mut self, event: &Event) -> Cmd<Msg> {
        let area = self
            .with_shared(|s| s.area)
            .unwrap_or_else(|| Rect::new(0, 0, 80, 24));
        let Ok(layout) = self.tree.solve_layout(area) else {
            return Cmd::none();
        };

        // Production keyboard-binding mode (bd-8e1oc): route `Event::Key` through
        // the real `ftui_runtime::pane_keymap` controller — the same key ->
        // PaneCommand -> resolve -> apply path a focus-aware terminal host uses.
        // The scripted test sends a key sequence then `q`; the marker reports the
        // final focus + structural state. This is NOT the affordance-key path.
        if let Some(mut keyboard) = self.keyboard.take() {
            if let Event::Key(key) = event
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            {
                let out = keyboard.handle_key(key, &mut self.tree, &layout);
                let applied = match &out {
                    PaneKeyOutcome::Handled { resolution, .. } => match &resolution.effect {
                        PaneCommandEffect::Structural(ops) => {
                            u64::try_from(ops.len()).unwrap_or_default()
                        }
                        _ => 0,
                    },
                    _ => 0,
                };
                let active = keyboard
                    .active()
                    .map_or_else(|| "-".to_string(), |id| leaf_name(&self.tree, id));
                let maximized = keyboard.maximized().is_some();
                if applied > 0 {
                    self.with_shared(|s| s.applied_ops += applied);
                }
                self.with_shared(|s| {
                    s.active_pane = active;
                    s.maximized = maximized;
                });
                self.record_state(false);
            }
            self.keyboard = Some(keyboard);
            return Cmd::none();
        }

        let handles = pane_terminal_splitter_handles(&self.tree, &layout, HIT_THICKNESS);

        match event {
            Event::Mouse(mouse) => {
                // Faithful terminal path: the adapter resolves the splitter target
                // from the hit-test handles on press/scroll and reuses the armed
                // target on drag/release.
                let dispatch = self.adapter.translate_with_handles(event, &handles);

                if matches!(mouse.kind, MouseEventKind::Down(_))
                    && self.adapter.active_pointer_id().is_some()
                {
                    self.with_shared(|s| s.down_resolved = true);
                }

                self.apply_dispatch(&dispatch, &layout);

                let committed = matches!(mouse.kind, MouseEventKind::Up(_))
                    && dispatch.primary_transition.is_some();
                self.record_state(committed);

                if committed {
                    // Gesture finished: quit promptly so the test reads a stable result.
                    Cmd::quit()
                } else {
                    Cmd::none()
                }
            }
            Event::Key(key) => {
                // Harness affordances for the structural operations the production
                // terminal adapter does not yet bind to input (bd-21pbi.2). These
                // exercise operation -> render -> teardown over a real PTY.
                if let Some(op) = affordance_operation(key) {
                    if self.tree.apply_operation(self.op_seed, op).is_ok() {
                        self.op_seed += 1;
                        self.with_shared(|s| s.applied_ops += 1);
                    }
                    self.record_state(false);
                    return Cmd::quit();
                }

                // Resize / Escape via the real adapter. The host resolves the
                // splitter target (here, the single root splitter) and supplies it
                // through the documented `translate(event, target_hint)` contract.
                let target = handles.first().map(|handle| handle.target);
                let dispatch = self.adapter.translate(event, target);
                let canceled = dispatch.log.phase == PaneTerminalLifecyclePhase::KeyCancel;
                self.apply_dispatch(&dispatch, &layout);
                if canceled {
                    self.with_shared(|s| s.canceled = true);
                }
                self.record_state(false);

                if canceled {
                    // Interaction canceled: quit so the test reads a stable result
                    // without depending on the safety auto-quit timer.
                    Cmd::quit()
                } else {
                    Cmd::none()
                }
            }
            _ => Cmd::none(),
        }
    }

    /// Realize a dispatched transition into live pane operations (fixed pressure).
    fn apply_dispatch(&mut self, dispatch: &PaneTerminalDispatch, layout: &PaneLayout) {
        let Some(transition) = dispatch.primary_transition.as_ref() else {
            return;
        };
        let ops = self
            .tree
            .operations_for_transition(transition, layout, FIXED_PRESSURE);
        for op in ops {
            if self.tree.apply_operation(self.op_seed, op).is_ok() {
                self.op_seed += 1;
                self.with_shared(|s| s.applied_ops += 1);
            }
        }
    }

    /// Snapshot the post-operation tree state into the shared result.
    fn record_state(&mut self, committed: bool) {
        let bps = root_first_share_bps(&self.tree);
        let node_count = self.tree.nodes().count();
        let first_leaf = root_first_leaf_name(&self.tree);
        let valid = self.tree.validate().is_ok();
        self.with_shared(|s| {
            if let Some(bps) = bps {
                s.final_bps = bps;
            }
            s.node_count = node_count;
            s.first_leaf = first_leaf;
            s.tree_valid = valid;
            if committed {
                s.committed = true;
            }
        });
    }
}

/// Map a harness affordance key to a structural pane operation, or `None` for
/// keys that should flow to the resize/cancel adapter path instead.
fn affordance_operation(key: &KeyEvent) -> Option<PaneOperation> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    match c {
        's' => Some(PaneOperation::SplitLeaf {
            target: pane_id(LEFT),
            axis: SplitAxis::Vertical,
            ratio: PaneSplitRatio::new(1, 1).expect("valid ratio"),
            placement: PanePlacement::ExistingFirst,
            new_leaf: PaneLeaf::new("split"),
        }),
        'c' => Some(PaneOperation::CloseNode {
            target: pane_id(RIGHT),
        }),
        'w' => Some(PaneOperation::SwapNodes {
            first: pane_id(LEFT),
            second: pane_id(RIGHT),
        }),
        _ => None,
    }
}

impl Model for Harness {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Quit => Cmd::quit(),
            Msg::Tick => {
                self.ticks_remaining = self.ticks_remaining.saturating_sub(1);
                if self.ticks_remaining == 0 {
                    Cmd::quit()
                } else {
                    Cmd::none()
                }
            }
            Msg::Input(event) => self.handle_event(&event),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        self.with_shared(|s| s.area = area);

        let Ok(layout) = self.tree.solve_layout(area) else {
            return;
        };
        for record in self.tree.nodes() {
            if let PaneNodeKind::Leaf(leaf) = &record.kind
                && let Some(rect) = layout.rect(record.id)
            {
                Block::new()
                    .borders(Borders::ALL)
                    .title(leaf.surface_key.as_str())
                    .render(rect, frame);
            }
        }
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Msg>>> {
        // Safety auto-quit heartbeat: fires Msg::Tick every 100ms so the harness
        // never hangs if the scripted gesture is never delivered.
        vec![Box::new(Every::new(Duration::from_millis(100), || {
            Msg::Tick
        }))]
    }
}

fn env_u16(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(default)
}

fn main() -> std::io::Result<()> {
    let mode = std::env::var("PANE_HARNESS_SCREEN_MODE").unwrap_or_else(|_| "alt".to_string());
    let axis = match std::env::var("PANE_HARNESS_AXIS").as_deref() {
        Ok("vertical") => SplitAxis::Vertical,
        _ => SplitAxis::Horizontal,
    };
    let ui_height = env_u16("PANE_HARNESS_UI_HEIGHT", 12).max(4);
    let exit_after_ms = u32::from(env_u16("PANE_HARNESS_EXIT_AFTER_MS", 4_000)).max(200);
    // Production keyboard-binding input mode (bd-8e1oc).
    let keymap_mode = std::env::var("PANE_HARNESS_INPUT").as_deref() == Ok("keymap");

    let (screen_mode, anchor) = if mode == "inline" {
        // Anchor the inline UI at the TOP so the rendered pane area's cell
        // coordinates coincide with the terminal's mouse coordinates (the
        // runtime does not translate mouse coords for inline mode).
        (ScreenMode::Inline { ui_height }, UiAnchor::Top)
    } else {
        (ScreenMode::AltScreen, UiAnchor::Top)
    };

    let tree = root_split_tree(axis);
    let initial_bps = root_first_share_bps(&tree).expect("root split has a first share");
    let node_count = tree.nodes().count();
    let first_leaf = root_first_leaf_name(&tree);
    // In keymap mode the controller starts focused on the left leaf.
    let keyboard = keymap_mode.then(|| PaneKeyboardController::new(Some(pane_id(LEFT))));
    let initial_active = keyboard
        .as_ref()
        .and_then(PaneKeyboardController::active)
        .map_or_else(|| "-".to_string(), |id| leaf_name(&tree, id));
    let shared = Arc::new(Mutex::new(Shared {
        mode: mode.clone(),
        area: Rect::new(0, 0, 80, 24),
        initial_bps,
        final_bps: initial_bps,
        applied_ops: 0,
        down_resolved: false,
        committed: false,
        canceled: false,
        tree_valid: true,
        node_count,
        first_leaf,
        active_pane: initial_active,
        maximized: false,
    }));

    let adapter = PaneTerminalAdapter::new(PaneTerminalAdapterConfig::default())
        .expect("valid pane terminal adapter config");

    let model = Harness {
        tree,
        adapter,
        keyboard,
        op_seed: 1,
        ticks_remaining: exit_after_ms.div_ceil(100).max(1),
        shared: Arc::clone(&shared),
    };

    let run_result = App::new(model)
        .screen_mode(screen_mode)
        .anchor(anchor)
        .with_mouse()
        .run();

    // Terminal is restored here. Emit the deterministic, greppable result line.
    let snap = shared.lock().expect("shared state lock").clone();
    println!(
        "PANE_RESULT mode={} initial_bps={} final_bps={} applied_ops={} down_resolved={} committed={} tree_valid={} node_count={} first_leaf={} canceled={} active_pane={} maximized={}",
        snap.mode,
        snap.initial_bps,
        snap.final_bps,
        snap.applied_ops,
        snap.down_resolved,
        snap.committed,
        snap.tree_valid,
        snap.node_count,
        snap.first_leaf,
        snap.canceled,
        snap.active_pane,
        snap.maximized,
    );
    let _ = std::io::stdout().flush();

    run_result
}

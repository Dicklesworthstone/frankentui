#![forbid(unsafe_code)]

//! Live pane-splitter PTY harness (bd-x9lqw acceptance #3).
//!
//! This binary runs the **real** terminal runtime ([`ftui_runtime::App`] /
//! `Program`) with a single root horizontal split and drives the production
//! pane-resize path end-to-end:
//!
//! ```text
//! crossterm SGR mouse bytes (over a real PTY)
//!   -> ftui_core::Event::Mouse
//!   -> PaneTerminalAdapter::translate_with_handles  (terminal hit-testing)
//!   -> PaneDragResizeTransition
//!   -> PaneTree::operations_for_transition           (fixed-pressure bridge)
//!   -> PaneTree::apply_operation                     (live tree mutation)
//! ```
//!
//! It exists so a PTY-level E2E test can send genuine SGR mouse sequences and
//! prove the live split ratio changes in **both** inline and alt-screen modes
//! (the one DoD item the in-process integration tests in `ftui-runtime` could
//! not cover). After the runtime tears down and the terminal is restored, the
//! harness prints a single deterministic, greppable result marker to stdout:
//!
//! ```text
//! PANE_RESULT mode=alt initial_bps=5000 final_bps=8000 applied_ops=3 \
//!     down_resolved=true committed=true tree_valid=true
//! ```
//!
//! Determinism note: the realistic resize path derives snap *pressure* from
//! pointer motion (and therefore from wall-clock `Instant`). To keep the
//! reported ratio byte-for-byte reproducible across runs and identical between
//! screen modes, this harness applies transitions with a FIXED neutral pressure
//! profile -- exactly like the `apply_dispatch_fixed` helper in the
//! `ftui-runtime` adapter tests.
//!
//! ## Environment
//!
//! - `PANE_HARNESS_SCREEN_MODE` -- `inline` | `alt` (default `alt`).
//! - `PANE_HARNESS_UI_HEIGHT`   -- inline UI height in rows (default `12`).
//! - `PANE_HARNESS_EXIT_AFTER_MS` -- safety auto-quit if no gesture arrives
//!   (default `4000`). The harness normally exits as soon as a drag commits.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_layout::{
    PANE_TREE_SCHEMA_VERSION, PaneId, PaneLeaf, PaneNodeKind, PaneNodeRecord,
    PanePressureSnapProfile, PaneSplit, PaneSplitRatio, PaneTree, PaneTreeSnapshot, SplitAxis,
};
use ftui_render::frame::Frame;
use ftui_runtime::{
    App, Cmd, Every, Model, PaneTerminalAdapter, PaneTerminalAdapterConfig, ScreenMode,
    Subscription, UiAnchor, pane_terminal_splitter_handles,
};
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;

/// Root horizontal split node id.
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

/// Build a single root horizontal split (`left | right`) at a 1:1 ratio.
fn root_split_tree() -> PaneTree {
    let snapshot = PaneTreeSnapshot {
        schema_version: PANE_TREE_SCHEMA_VERSION,
        root: pane_id(ROOT),
        next_id: pane_id(4),
        nodes: vec![
            PaneNodeRecord::split(
                pane_id(ROOT),
                None,
                PaneSplit {
                    axis: SplitAxis::Horizontal,
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

/// First-child share (basis points) of the root split's ratio.
fn root_first_share_bps(tree: &PaneTree) -> u32 {
    match &tree.node(pane_id(ROOT)).expect("root node present").kind {
        PaneNodeKind::Split(node) => {
            node.ratio.numerator() * 10_000 / (node.ratio.numerator() + node.ratio.denominator())
        }
        other => panic!("root must be a split node, got {other:?}"),
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
    tree_valid: bool,
}

struct Harness {
    tree: PaneTree,
    adapter: PaneTerminalAdapter,
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
        let Event::Mouse(mouse) = event else {
            return Cmd::none();
        };

        let area = self
            .with_shared(|s| s.area)
            .unwrap_or_else(|| Rect::new(0, 0, 80, 24));
        let Ok(layout) = self.tree.solve_layout(area) else {
            return Cmd::none();
        };
        let handles = pane_terminal_splitter_handles(&self.tree, &layout, HIT_THICKNESS);

        // Faithful terminal path: the adapter resolves the splitter target from
        // the hit-test handles on press and reuses the armed target on
        // drag/release.
        let dispatch = self.adapter.translate_with_handles(event, &handles);

        if matches!(mouse.kind, MouseEventKind::Down(_))
            && self.adapter.active_pointer_id().is_some()
        {
            self.with_shared(|s| s.down_resolved = true);
        }

        if let Some(transition) = dispatch.primary_transition.as_ref() {
            let ops = self
                .tree
                .operations_for_transition(transition, &layout, FIXED_PRESSURE);
            for op in ops {
                if self.tree.apply_operation(self.op_seed, op).is_ok() {
                    self.op_seed += 1;
                    self.with_shared(|s| s.applied_ops += 1);
                }
            }
        }

        let committed =
            matches!(mouse.kind, MouseEventKind::Up(_)) && dispatch.primary_transition.is_some();

        let bps = root_first_share_bps(&self.tree);
        let valid = self.tree.validate().is_ok();
        self.with_shared(|s| {
            s.final_bps = bps;
            s.tree_valid = valid;
            if committed {
                s.committed = true;
            }
        });

        if committed {
            // Gesture finished: quit promptly so the test reads a stable result.
            Cmd::quit()
        } else {
            Cmd::none()
        }
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
        if let Some(left) = layout.rect(pane_id(LEFT)) {
            Block::new()
                .borders(Borders::ALL)
                .title("left")
                .render(left, frame);
        }
        if let Some(right) = layout.rect(pane_id(RIGHT)) {
            Block::new()
                .borders(Borders::ALL)
                .title("right")
                .render(right, frame);
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
    let ui_height = env_u16("PANE_HARNESS_UI_HEIGHT", 12).max(4);
    let exit_after_ms = u32::from(env_u16("PANE_HARNESS_EXIT_AFTER_MS", 4_000)).max(200);

    let (screen_mode, anchor) = if mode == "inline" {
        // Anchor the inline UI at the TOP so the rendered pane area's cell
        // coordinates coincide with the terminal's mouse coordinates (the
        // runtime does not translate mouse coords for inline mode).
        (ScreenMode::Inline { ui_height }, UiAnchor::Top)
    } else {
        (ScreenMode::AltScreen, UiAnchor::Top)
    };

    let tree = root_split_tree();
    let initial_bps = root_first_share_bps(&tree);
    let shared = Arc::new(Mutex::new(Shared {
        mode: mode.clone(),
        area: Rect::new(0, 0, 80, 24),
        initial_bps,
        final_bps: initial_bps,
        applied_ops: 0,
        down_resolved: false,
        committed: false,
        tree_valid: true,
    }));

    let adapter = PaneTerminalAdapter::new(PaneTerminalAdapterConfig::default())
        .expect("valid pane terminal adapter config");

    let model = Harness {
        tree,
        adapter,
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
        "PANE_RESULT mode={} initial_bps={} final_bps={} applied_ops={} down_resolved={} committed={} tree_valid={}",
        snap.mode,
        snap.initial_bps,
        snap.final_bps,
        snap.applied_ops,
        snap.down_resolved,
        snap.committed,
        snap.tree_valid,
    );
    let _ = std::io::stdout().flush();

    run_result
}

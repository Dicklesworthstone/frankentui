//! Cross-host pane parity diff runner (bd-a46q1.5).
//!
//! This is the cross-host capstone of the pane validation pyramid. The terminal
//! PTY E2E suite (`ftui-harness/tests/pane_input_pty_e2e.rs`, bd-a46q1.3) proves
//! the terminal adapter in isolation; the web E2E suite
//! (`ftui-web/tests/pane_web_e2e.rs`, bd-a46q1.4) proves the web adapter in
//! isolation. This runner proves they agree: it drives **canonical, host-neutral
//! gestures** through *both production adapters in-process* —
//! `PaneTerminalAdapter` (ftui-runtime) and `PanePointerCaptureAdapter`
//! (ftui-web) — applies the resulting transitions through the *single shared*
//! `PaneTree::operations_for_transition` bridge, and diffs the normalized
//! outcomes.
//!
//! Why ftui-web owns this test
//! ---------------------------
//! `ftui-web` is the only crate that already depends on **both** hosts: it
//! depends on `ftui-runtime` (terminal adapter lives there) and *is* the web
//! adapter. So this is the single linkable point where the two production input
//! paths can be exercised side by side without a PTY subprocess or a browser.
//!
//! Parity contract (normative: `docs/spec/pane-parity-contract-and-program.md`)
//! ----------------------------------------------------------------------------
//! Per §6, for equivalent event traces the two hosts MUST agree on (1) the final
//! topology hash (`PaneTree::state_hash`), (2) the applied operation sequence
//! (`PaneOperationKind` stream), and (3) lifecycle settle (machine returns to
//! `Idle`, plus the commit/cancel flags). We additionally assert the observable
//! resize result (first-child basis points) since it is the user-facing effect
//! of the topology.
//!
//! Intentional, contract-sanctioned host differences (§4) that are therefore
//! **excluded** from the parity diff:
//!   * browser pointer-capture lifecycle (`Acquire`/`Release` commands) — a
//!     web-only host quirk with no terminal analogue,
//!   * the cancel *reason* (`EscapeKey` on terminal vs `PointerCancel` on web) —
//!     the host cancellation mechanism differs by construction; only the
//!     *effect* (canceled + tree unchanged) must match,
//!   * host-only fields on semantic events (pointer id, absolute pixel
//!     position) — normalized away by [`normalize_event`].
//!
//! Determinism: every gesture is integer cell-space with fixed, cell-centered
//! coordinates and a fixed neutral pressure profile, so the whole corpus is
//! byte-for-byte reproducible (no wall-clock, no float, no RNG). Each scenario
//! prints one greppable `PANE_PARITY_TRACE ...` line, and — when
//! `PANE_PARITY_ARTIFACT_DIR` is set by the runner script — writes a
//! machine-readable JSON diff artifact per scenario plus a corpus summary, so a
//! CI failure is triageable without an ad-hoc rerun.
//!
//! Pass/fail: a scenario is `match` iff its difference list is empty;
//! `pane_cross_host_parity_corpus_reports_zero_unexplained_diffs` fails (with
//! the full per-host outcome printed) if any scenario diverges.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ftui_core::event::{Event, MouseButton, MouseEvent, MouseEventKind};
use ftui_layout::{
    PANE_TREE_SCHEMA_VERSION, PaneDragResizeState, PaneId, PaneLeaf, PaneModifierSnapshot,
    PaneNodeKind, PaneNodeRecord, PaneOperation, PanePointerButton, PanePointerPosition,
    PanePressureSnapProfile, PaneResizeTarget, PaneSemanticInputEvent, PaneSemanticInputEventKind,
    PaneSplit, PaneSplitRatio, PaneTree, PaneTreeSnapshot, Rect, SplitAxis,
};
use ftui_runtime::{PaneTerminalAdapter, PaneTerminalAdapterConfig};
use ftui_web::pane_pointer_capture::{PanePointerCaptureAdapter, PanePointerCaptureConfig};

/// Neutral, pressure-independent snap profile. Identical to the terminal and
/// web E2E suites so resize math is host- and pressure-invariant.
const NEUTRAL: PanePressureSnapProfile = PanePressureSnapProfile {
    strength_bps: 5_000,
    hysteresis_bps: 100,
};

/// Canonical drag viewport (cells). Mirrors the per-host E2E suites.
const VIEW_W: u16 = 50;
const VIEW_H: u16 = 20;

fn pane_id(raw: u64) -> PaneId {
    PaneId::new(raw).expect("test pane id must be non-zero")
}

fn pos(x: i32, y: i32) -> PanePointerPosition {
    PanePointerPosition::new(x, y)
}

fn modifiers() -> PaneModifierSnapshot {
    PaneModifierSnapshot::none()
}

/// A two-leaf root split spanning the whole viewport, so a single layout solve
/// targets the root splitter precisely. Identical seed tree on both hosts.
fn root_split_tree(axis: SplitAxis) -> PaneTree {
    let root = pane_id(1);
    let first = pane_id(2);
    let second = pane_id(3);
    let snapshot = PaneTreeSnapshot {
        schema_version: PANE_TREE_SCHEMA_VERSION,
        root,
        next_id: pane_id(4),
        nodes: vec![
            PaneNodeRecord::split(
                root,
                None,
                PaneSplit {
                    axis,
                    ratio: PaneSplitRatio::new(1, 1).expect("valid 1:1 ratio"),
                    first,
                    second,
                },
            ),
            PaneNodeRecord::leaf(first, Some(root), PaneLeaf::new("first")),
            PaneNodeRecord::leaf(second, Some(root), PaneLeaf::new("second")),
        ],
        extensions: BTreeMap::new(),
    };
    PaneTree::from_snapshot(snapshot).expect("valid root split tree")
}

fn resize_target(split: PaneId, axis: SplitAxis) -> PaneResizeTarget {
    PaneResizeTarget {
        split_id: split,
        axis,
    }
}

/// First-child share of the root split in basis points (0..=10_000).
fn first_share_bps(tree: &PaneTree, split: PaneId) -> u32 {
    match &tree.node(split).expect("split node present").kind {
        PaneNodeKind::Split(node) => {
            node.ratio.numerator() * 10_000 / (node.ratio.numerator() + node.ratio.denominator())
        }
        PaneNodeKind::Leaf(_) => panic!("expected split node"),
    }
}

/// Project a semantic event onto a host-agnostic descriptor: keep the kind and
/// the resize target (split id + axis), drop host-only fields (pointer id,
/// absolute position, cancel reason) per the parity contract §4.
fn normalize_event(event: &PaneSemanticInputEvent) -> String {
    fn tgt(t: &PaneResizeTarget) -> String {
        let axis = match t.axis {
            SplitAxis::Horizontal => "H",
            SplitAxis::Vertical => "V",
        };
        format!("{}#{}", axis, t.split_id.get())
    }
    match &event.kind {
        PaneSemanticInputEventKind::PointerDown { target, .. } => {
            format!("PointerDown({})", tgt(target))
        }
        PaneSemanticInputEventKind::PointerMove { target, .. } => {
            format!("PointerMove({})", tgt(target))
        }
        PaneSemanticInputEventKind::PointerUp { target, .. } => {
            format!("PointerUp({})", tgt(target))
        }
        PaneSemanticInputEventKind::WheelNudge { target, lines } => {
            format!("WheelNudge({},{lines})", tgt(target))
        }
        PaneSemanticInputEventKind::KeyboardResize {
            target,
            direction,
            units,
        } => format!("KeyboardResize({},{direction:?},{units})", tgt(target)),
        // Cancel reason is host-specific (EscapeKey vs PointerCancel) and is
        // deliberately normalized away: only the cancellation *effect* is part
        // of the parity contract.
        PaneSemanticInputEventKind::Cancel { .. } => "Cancel".to_string(),
        PaneSemanticInputEventKind::Blur { .. } => "Blur".to_string(),
    }
}

fn op_kind_name(op: &PaneOperation) -> String {
    format!("{:?}", op.kind())
}

/// Normalized, host-agnostic outcome of running one gesture through one host.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostOutcome {
    host: &'static str,
    /// Normalized semantic event stream (host-only fields stripped).
    semantic: Vec<String>,
    /// Applied `PaneOperationKind` stream (the contract §6 "operation sequence").
    ops: Vec<String>,
    /// Final topology hash (contract §6 "final topology hash").
    tree_hash: u64,
    /// Observable resize result: first-child share in basis points.
    first_share_bps: u32,
    committed: bool,
    canceled: bool,
    machine_idle: bool,
}

/// Host-neutral canonical gesture: a pointer drag (optionally cancelled).
#[derive(Debug, Clone)]
struct Gesture {
    axis: SplitAxis,
    /// Pointer-down cell (on the splitter handle).
    down: PanePointerPosition,
    /// Intermediate drag waypoints (all forwarded as move/drag).
    drags: Vec<PanePointerPosition>,
    /// Release cell (pointer-up). Ignored when `cancel` is true.
    up: PanePointerPosition,
    /// When true, the host cancels mid-gesture instead of releasing.
    cancel: bool,
}

/// Drive a gesture through the **terminal** production adapter.
fn run_terminal(gesture: &Gesture) -> HostOutcome {
    let mut tree = root_split_tree(gesture.axis);
    let layout = tree
        .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
        .expect("layout solves");
    let split = pane_id(1);
    let target = resize_target(split, gesture.axis);

    // coalesce distance 1 so the terminal forwards every whole-cell move,
    // matching the web adapter's per-move forwarding (parity isolation).
    let config = PaneTerminalAdapterConfig {
        drag_update_coalesce_distance: 1,
        ..PaneTerminalAdapterConfig::default()
    };
    let mut adapter = PaneTerminalAdapter::new(config).expect("valid terminal adapter");

    let mut semantic = Vec::new();
    let mut ops = Vec::new();
    let mut seed = 1_000u64;
    let mut committed = false;
    let mut canceled = false;

    let apply = |adapter: &mut PaneTerminalAdapter,
                 tree: &mut PaneTree,
                 event: &Event,
                 hint: Option<PaneResizeTarget>,
                 semantic: &mut Vec<String>,
                 ops: &mut Vec<String>,
                 seed: &mut u64| {
        let dispatch = adapter.translate(event, hint);
        if let Some(ev) = dispatch.primary_event.as_ref() {
            semantic.push(normalize_event(ev));
        }
        if let Some(transition) = dispatch.primary_transition.as_ref() {
            for op in tree.operations_for_transition(transition, &layout, NEUTRAL) {
                ops.push(op_kind_name(&op));
                tree.apply_operation(*seed, op).expect("operation applies");
                *seed += 1;
            }
        }
    };

    // Pointer down (host hit-testing supplies the target on press).
    let down = Event::Mouse(MouseEvent::new(
        MouseEventKind::Down(MouseButton::Left),
        gesture.down.x as u16,
        gesture.down.y as u16,
    ));
    apply(
        &mut adapter,
        &mut tree,
        &down,
        Some(target),
        &mut semantic,
        &mut ops,
        &mut seed,
    );

    for d in &gesture.drags {
        let drag = Event::Mouse(MouseEvent::new(
            MouseEventKind::Drag(MouseButton::Left),
            d.x as u16,
            d.y as u16,
        ));
        apply(
            &mut adapter,
            &mut tree,
            &drag,
            None,
            &mut semantic,
            &mut ops,
            &mut seed,
        );
    }

    if gesture.cancel {
        // Terminal cancellation: the Escape key.
        let esc = Event::Key(ftui_core::event::KeyEvent::new(
            ftui_core::event::KeyCode::Escape,
        ));
        let dispatch = adapter.translate(&esc, None);
        if let Some(ev) = dispatch.primary_event.as_ref() {
            semantic.push(normalize_event(ev));
            canceled = matches!(ev.kind, PaneSemanticInputEventKind::Cancel { .. });
        }
        if let Some(transition) = dispatch.primary_transition.as_ref() {
            for op in tree.operations_for_transition(transition, &layout, NEUTRAL) {
                ops.push(op_kind_name(&op));
                tree.apply_operation(seed, op).expect("operation applies");
                seed += 1;
            }
        }
    } else {
        let up = Event::Mouse(MouseEvent::new(
            MouseEventKind::Up(MouseButton::Left),
            gesture.up.x as u16,
            gesture.up.y as u16,
        ));
        let dispatch = adapter.translate(&up, None);
        if let Some(ev) = dispatch.primary_event.as_ref() {
            semantic.push(normalize_event(ev));
            committed = matches!(adapter.machine_state(), PaneDragResizeState::Idle);
        }
        if let Some(transition) = dispatch.primary_transition.as_ref() {
            for op in tree.operations_for_transition(transition, &layout, NEUTRAL) {
                ops.push(op_kind_name(&op));
                tree.apply_operation(seed, op).expect("operation applies");
                seed += 1;
            }
        }
    }

    HostOutcome {
        host: "terminal",
        semantic,
        ops,
        tree_hash: tree.state_hash(),
        first_share_bps: first_share_bps(&tree, split),
        committed,
        canceled,
        machine_idle: matches!(adapter.machine_state(), PaneDragResizeState::Idle),
    }
}

/// Drive a gesture through the **web** production adapter.
fn run_web(gesture: &Gesture) -> HostOutcome {
    let mut tree = root_split_tree(gesture.axis);
    let layout = tree
        .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
        .expect("layout solves");
    let split = pane_id(1);
    let target = resize_target(split, gesture.axis);

    let mut adapter = PanePointerCaptureAdapter::new(PanePointerCaptureConfig::default())
        .expect("valid web adapter");

    let mut semantic = Vec::new();
    let mut ops = Vec::new();
    let mut seed = 1_000u64;
    let mut committed = false;
    let mut canceled = false;
    let pointer_id = 1u32;

    let apply = |tree: &mut PaneTree,
                 dispatch: &ftui_web::pane_pointer_capture::PanePointerDispatch,
                 semantic: &mut Vec<String>,
                 ops: &mut Vec<String>,
                 seed: &mut u64| {
        if let Some(ev) = dispatch.semantic_event.as_ref() {
            semantic.push(normalize_event(ev));
        }
        if let Some(transition) = dispatch.transition.as_ref() {
            for op in tree.operations_for_transition(transition, &layout, NEUTRAL) {
                ops.push(op_kind_name(&op));
                tree.apply_operation(*seed, op).expect("operation applies");
                *seed += 1;
            }
        }
    };

    let down = adapter.pointer_down(
        target,
        pointer_id,
        PanePointerButton::Primary,
        gesture.down,
        modifiers(),
    );
    apply(&mut tree, &down, &mut semantic, &mut ops, &mut seed);
    // Host confirms the browser granted pointer capture (web-only lifecycle).
    let _ = adapter.capture_acquired(pointer_id);

    for d in &gesture.drags {
        let moved = adapter.pointer_move(pointer_id, *d, modifiers());
        apply(&mut tree, &moved, &mut semantic, &mut ops, &mut seed);
    }

    if gesture.cancel {
        let dispatch = adapter.pointer_cancel(Some(pointer_id));
        if let Some(ev) = dispatch.semantic_event.as_ref() {
            semantic.push(normalize_event(ev));
            canceled = matches!(ev.kind, PaneSemanticInputEventKind::Cancel { .. });
        }
        if let Some(transition) = dispatch.transition.as_ref() {
            for op in tree.operations_for_transition(transition, &layout, NEUTRAL) {
                ops.push(op_kind_name(&op));
                tree.apply_operation(seed, op).expect("operation applies");
                seed += 1;
            }
        }
    } else {
        let up = adapter.pointer_up(
            pointer_id,
            PanePointerButton::Primary,
            gesture.up,
            modifiers(),
        );
        if up.semantic_event.is_some() {
            committed = matches!(adapter.machine_state(), PaneDragResizeState::Idle);
        }
        apply(&mut tree, &up, &mut semantic, &mut ops, &mut seed);
    }

    HostOutcome {
        host: "web",
        semantic,
        ops,
        tree_hash: tree.state_hash(),
        first_share_bps: first_share_bps(&tree, split),
        committed,
        canceled,
        machine_idle: matches!(adapter.machine_state(), PaneDragResizeState::Idle),
    }
}

/// A parity report for one scenario: the two host outcomes and any differences
/// that violate the parity contract.
#[derive(Debug, Clone)]
struct ParityReport {
    scenario: &'static str,
    terminal: HostOutcome,
    web: HostOutcome,
    differences: Vec<String>,
}

impl ParityReport {
    fn matched(&self) -> bool {
        self.differences.is_empty()
    }

    fn verdict(&self) -> &'static str {
        if self.matched() { "match" } else { "diverged" }
    }
}

/// Diff two host outcomes against the parity contract §6 (plus the observable
/// resize result). Host-only concerns are already normalized out upstream.
fn diff(scenario: &'static str, terminal: HostOutcome, web: HostOutcome) -> ParityReport {
    let mut differences = Vec::new();
    if terminal.tree_hash != web.tree_hash {
        differences.push(format!(
            "final topology hash: terminal={} web={}",
            terminal.tree_hash, web.tree_hash
        ));
    }
    if terminal.first_share_bps != web.first_share_bps {
        differences.push(format!(
            "first-child share bps: terminal={} web={}",
            terminal.first_share_bps, web.first_share_bps
        ));
    }
    if terminal.ops != web.ops {
        differences.push(format!(
            "operation sequence: terminal={:?} web={:?}",
            terminal.ops, web.ops
        ));
    }
    if terminal.semantic != web.semantic {
        differences.push(format!(
            "semantic event stream: terminal={:?} web={:?}",
            terminal.semantic, web.semantic
        ));
    }
    if terminal.committed != web.committed {
        differences.push(format!(
            "committed flag: terminal={} web={}",
            terminal.committed, web.committed
        ));
    }
    if terminal.canceled != web.canceled {
        differences.push(format!(
            "canceled flag: terminal={} web={}",
            terminal.canceled, web.canceled
        ));
    }
    if terminal.machine_idle != web.machine_idle {
        differences.push(format!(
            "machine idle: terminal={} web={}",
            terminal.machine_idle, web.machine_idle
        ));
    }
    ParityReport {
        scenario,
        terminal,
        web,
        differences,
    }
}

/// The canonical cross-host parity corpus.
fn corpus() -> Vec<Gesture> {
    vec![
        // Horizontal splitter dragged right: first child grows.
        Gesture {
            axis: SplitAxis::Horizontal,
            down: pos(25, 10),
            drags: vec![pos(30, 10), pos(34, 10)],
            up: pos(38, 10),
            cancel: false,
        },
        // Horizontal splitter dragged left: first child shrinks.
        Gesture {
            axis: SplitAxis::Horizontal,
            down: pos(25, 10),
            drags: vec![pos(20, 10), pos(16, 10)],
            up: pos(12, 10),
            cancel: false,
        },
        // Vertical splitter dragged down: first (top) child grows.
        Gesture {
            axis: SplitAxis::Vertical,
            down: pos(25, 10),
            drags: vec![pos(25, 12), pos(25, 14)],
            up: pos(25, 15),
            cancel: false,
        },
        // Cancel before crossing the drag threshold: tree stays at baseline.
        Gesture {
            axis: SplitAxis::Horizontal,
            down: pos(25, 10),
            drags: vec![],
            up: pos(25, 10),
            cancel: true,
        },
        // Marginal one-cell gesture that never crosses the drag threshold but
        // still commits a small adjustment on release: both hosts must agree on
        // exactly the same marginal outcome (this is where threshold/hysteresis
        // edge handling tends to drift between adapters).
        Gesture {
            axis: SplitAxis::Horizontal,
            down: pos(25, 10),
            drags: vec![pos(26, 10)],
            up: pos(26, 10),
            cancel: false,
        },
    ]
}

fn scenario_name(index: usize, gesture: &Gesture) -> &'static str {
    // Stable, human-readable names aligned with `corpus()` ordering.
    const NAMES: [&str; 5] = [
        "horizontal_drag_grow",
        "horizontal_drag_shrink",
        "vertical_drag_grow",
        "cancel_before_commit",
        "marginal_nudge",
    ];
    let _ = gesture;
    NAMES[index]
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn json_str_array(items: &[String]) -> String {
    let mut out = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "\"{}\"", json_escape(item));
    }
    out.push(']');
    out
}

fn host_json(outcome: &HostOutcome) -> String {
    format!(
        "{{\"host\":\"{}\",\"tree_hash\":{},\"first_share_bps\":{},\"committed\":{},\"canceled\":{},\"machine_idle\":{},\"ops\":{},\"semantic\":{}}}",
        outcome.host,
        outcome.tree_hash,
        outcome.first_share_bps,
        outcome.committed,
        outcome.canceled,
        outcome.machine_idle,
        json_str_array(&outcome.ops),
        json_str_array(&outcome.semantic),
    )
}

fn report_json(report: &ParityReport) -> String {
    format!(
        "{{\"schema_version\":\"pane-parity-v1\",\"scenario\":\"{}\",\"verdict\":\"{}\",\"terminal\":{},\"web\":{},\"differences\":{}}}",
        report.scenario,
        report.verdict(),
        host_json(&report.terminal),
        host_json(&report.web),
        json_str_array(&report.differences),
    )
}

/// Emit one greppable trace line and, when `PANE_PARITY_ARTIFACT_DIR` is set,
/// a per-scenario JSON artifact for CI / human triage.
fn emit(report: &ParityReport) {
    println!(
        "PANE_PARITY_TRACE scenario={} verdict={} term_hash={} web_hash={} \
         term_bps={} web_bps={} ops={} diffs={}",
        report.scenario,
        report.verdict(),
        report.terminal.tree_hash,
        report.web.tree_hash,
        report.terminal.first_share_bps,
        report.web.first_share_bps,
        report.terminal.ops.len(),
        report.differences.len(),
    );
    if let Ok(dir) = std::env::var("PANE_PARITY_ARTIFACT_DIR")
        && !dir.is_empty()
    {
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{dir}/parity_{}.json", report.scenario);
        let _ = std::fs::write(path, report_json(report));
    }
}

/// Run the full corpus through both hosts, emitting evidence per scenario.
fn run_corpus() -> Vec<ParityReport> {
    let mut reports = Vec::new();
    for (index, gesture) in corpus().iter().enumerate() {
        let name = scenario_name(index, gesture);
        let terminal = run_terminal(gesture);
        let web = run_web(gesture);
        let report = diff(name, terminal, web);
        emit(&report);
        reports.push(report);
    }
    reports
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The headline parity gate: every canonical scenario must agree across hosts.
#[test]
fn pane_cross_host_parity_corpus_reports_zero_unexplained_diffs() {
    let reports = run_corpus();

    // Write a corpus-level summary artifact when requested.
    if let Ok(dir) = std::env::var("PANE_PARITY_ARTIFACT_DIR")
        && !dir.is_empty()
    {
        let _ = std::fs::create_dir_all(&dir);
        let matched = reports.iter().filter(|r| r.matched()).count();
        let bodies: Vec<String> = reports.iter().map(report_json).collect();
        let summary = format!(
            "{{\"schema_version\":\"pane-parity-summary-v1\",\"total\":{},\"matched\":{},\"diverged\":{},\"reports\":[{}]}}",
            reports.len(),
            matched,
            reports.len() - matched,
            bodies.join(",")
        );
        let _ = std::fs::write(format!("{dir}/parity_summary.json"), summary);
    }

    let diverged: Vec<&ParityReport> = reports.iter().filter(|r| !r.matched()).collect();
    assert!(
        diverged.is_empty(),
        "cross-host pane parity violations:\n{}",
        diverged
            .iter()
            .map(|r| format!(
                "  [{}] {:?}\n    terminal={:?}\n    web={:?}",
                r.scenario, r.differences, r.terminal, r.web
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Every scenario must actually exercise both hosts (guards against a corpus
/// that silently degenerates to empty outcomes and "passes" vacuously).
#[test]
fn pane_cross_host_parity_corpus_is_non_trivial() {
    let reports = run_corpus();
    assert_eq!(reports.len(), 5, "corpus size changed unexpectedly");
    for report in &reports {
        assert!(
            !report.terminal.semantic.is_empty() && !report.web.semantic.is_empty(),
            "[{}] both hosts must emit semantic events",
            report.scenario
        );
        assert!(
            report.terminal.machine_idle && report.web.machine_idle,
            "[{}] both hosts must settle to idle",
            report.scenario
        );
    }
    // A committing drag must actually move the splitter on both hosts.
    let grow = &reports[0];
    assert!(
        grow.terminal.first_share_bps > 5_000 && grow.web.first_share_bps > 5_000,
        "rightward drag must grow the first child on both hosts"
    );
    // A cancel must leave the tree at baseline (1:1 = 5000 bps) on both hosts.
    let cancel = &reports[3];
    assert!(cancel.terminal.canceled && cancel.web.canceled);
    assert_eq!(cancel.terminal.first_share_bps, 5_000);
    assert_eq!(cancel.web.first_share_bps, 5_000);
}

/// The runner is fully deterministic: two corpus runs are byte-identical.
#[test]
fn pane_cross_host_parity_is_deterministic() {
    let first = run_corpus();
    let second = run_corpus();
    assert_eq!(first.len(), second.len());
    for (a, b) in first.iter().zip(second.iter()) {
        assert_eq!(a.scenario, b.scenario);
        assert_eq!(
            a.terminal, b.terminal,
            "[{}] terminal not deterministic",
            a.scenario
        );
        assert_eq!(a.web, b.web, "[{}] web not deterministic", a.scenario);
    }
}

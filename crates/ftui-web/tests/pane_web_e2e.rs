//! Web pane interaction E2E suite (bd-a46q1.4).
//!
//! This is the browser/wasm-host counterpart to the terminal PTY E2E suite
//! (`ftui-harness/tests/pane_input_pty_e2e.rs`, bd-a46q1.3). It drives the
//! **production** web pointer-capture adapter (`PanePointerCaptureAdapter`)
//! together with the **canonical coordinate normalizer**
//! (`PaneCoordinateNormalizer`, delivered by bd-2eqqy) end-to-end, the same way
//! a real JS host wires DOM pointer/touch events into the shared pane engine.
//!
//! Coverage (everything reachable through the production web input path today):
//!   * pointer drag -> live `PaneTree` ratio mutation (horizontal + vertical),
//!   * single-touch drag parity + multi-touch yield to the pinch/scroll layer,
//!   * DPR / zoom / viewport-origin / cell-size INVARIANCE (no geometry drift),
//!   * the full interruption / focus-recovery matrix (pointercancel, leave,
//!     blur, visibility-hidden/tab-switch, lost-pointer-capture, context-loss,
//!     render-stall),
//!   * deterministic replay of the emitted semantic stream (the validation
//!     "spine" the parent epic bd-a46q1 mandates),
//!   * keyboard resize at the reachable semantic/bridge layer.
//!
//! Honesty boundary
//! ----------------
//! "Keyboard resizing" in the browser has two layers:
//!   (a) the host-agnostic semantic -> operation -> tree bridge
//!       (`KeyboardResize` event -> `PaneDragResizeMachine` ->
//!       `operations_for_transition`), which IS implemented and exercised here
//!       by `web_keyboard_resize_semantic_event_drives_tree_via_bridge`, and
//!   (b) the browser `KeyboardEvent` -> semantic translation plus roving
//!       tabindex and ARIA splitter/pane semantics, which is GREENFIELD and
//!       owned by bd-21pbi.3 (web keyboard bindings), itself blocked by
//!       bd-21pbi.1 (canonical keyboard command model + focus graph).
//! This suite therefore covers (a) and deliberately does NOT fabricate (b).
//! Production-binding browser-key E2E coverage is filed as a follow-up bead
//! (gated on bd-21pbi.3), exactly mirroring how the terminal suite deferred its
//! production-input remainder to bd-8e1oc.
//!
//! Determinism: every scenario operates in integer cell space with fixed,
//! cell-centered coordinate sampling, so it is byte-for-byte reproducible and
//! free of wall-clock or floating-point dependence. Each scenario prints a
//! single structured `PANE_WEB_TRACE ...` line so the runner script
//! (`scripts/pane_e2e.sh`) captures greppable, hashable observability evidence
//! per step.

use std::collections::BTreeMap;

use ftui_layout::{
    PANE_SNAP_DEFAULT_STEP_BPS, PANE_TREE_SCHEMA_VERSION, PaneCancelReason,
    PaneCoordinateNormalizer, PaneCoordinateRoundingPolicy, PaneDragResizeMachine,
    PaneDragResizeState, PaneInputCoordinate, PaneLayout, PaneLeaf, PaneModifierSnapshot,
    PaneNodeKind, PaneNodeRecord, PaneOperation, PanePointerButton, PanePointerPosition,
    PanePressureSnapProfile, PaneResizeDirection, PaneResizeTarget, PaneScaleFactor,
    PaneSemanticInputEvent, PaneSemanticInputEventKind, PaneSemanticInputTrace, PaneSplit,
    PaneSplitRatio, PaneTree, PaneTreeSnapshot, Rect, SplitAxis,
};
use ftui_web::pane_pointer_capture::{
    PanePointerCaptureAdapter, PanePointerCaptureCommand, PanePointerDispatch,
};

/// Neutral snap profile: matches the terminal harness so resize math is
/// identical across hosts and independent of derived pointer pressure.
const NEUTRAL: PanePressureSnapProfile = PanePressureSnapProfile {
    strength_bps: 5_000,
    hysteresis_bps: 100,
};

/// The canonical drag viewport (cells). Mirrors the in-crate adapter test.
const VIEW_W: u16 = 50;
const VIEW_H: u16 = 20;

fn pane_id(raw: u64) -> ftui_layout::PaneId {
    ftui_layout::PaneId::new(raw).expect("test pane id must be non-zero")
}

fn pos(x: i32, y: i32) -> PanePointerPosition {
    PanePointerPosition::new(x, y)
}

fn modifiers() -> PaneModifierSnapshot {
    PaneModifierSnapshot::none()
}

/// A simple two-leaf root split. The root split spans the whole viewport
/// regardless of ratio, so a single layout solve targets it precisely.
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

fn resize_target(split: ftui_layout::PaneId, axis: SplitAxis) -> PaneResizeTarget {
    PaneResizeTarget {
        split_id: split,
        axis,
    }
}

/// First-child share of the root split in basis points (0..=10_000).
fn first_share_bps(tree: &PaneTree, split: ftui_layout::PaneId) -> u32 {
    match &tree.node(split).expect("split node present").kind {
        PaneNodeKind::Split(node) => {
            node.ratio.numerator() * 10_000 / (node.ratio.numerator() + node.ratio.denominator())
        }
        PaneNodeKind::Leaf(_) => panic!("expected split node"),
    }
}

fn adapter() -> PanePointerCaptureAdapter {
    PanePointerCaptureAdapter::default()
}

/// Convert a dispatch's geometry-bearing transition into live tree operations,
/// applying each with a deterministic monotonic seed. Returns ops applied.
fn apply_dispatch(
    tree: &mut PaneTree,
    layout: &PaneLayout,
    dispatch: &PanePointerDispatch,
    seed: &mut u64,
) -> usize {
    let Some(transition) = dispatch.transition.as_ref() else {
        return 0;
    };
    let pressure = dispatch.pressure_snap_profile().unwrap_or(NEUTRAL);
    let ops: Vec<PaneOperation> = tree.operations_for_transition(transition, layout, pressure);
    let applied = ops.len();
    for op in ops {
        tree.apply_operation(*seed, op).expect("operation applies");
        *seed += 1;
    }
    applied
}

/// Emit one structured observability line per scenario (consumed by the runner
/// script's per-step log hashing).
#[allow(clippy::too_many_arguments)]
fn emit_trace(
    scenario: &str,
    profile: &str,
    initial_bps: u32,
    final_bps: u32,
    applied_ops: usize,
    committed: bool,
    canceled: bool,
    machine_idle: bool,
) {
    println!(
        "PANE_WEB_TRACE scenario={scenario} profile={profile} initial_bps={initial_bps} \
         final_bps={final_bps} applied_ops={applied_ops} committed={committed} \
         canceled={canceled} machine_idle={machine_idle}"
    );
}

/// Returns true when a dispatch released the host pointer capture.
fn released(dispatch: &PanePointerDispatch) -> bool {
    matches!(
        dispatch.capture_command,
        Some(PanePointerCaptureCommand::Release { .. })
    )
}

/// Returns true when the dispatch's semantic event is a cancel with the given
/// reason.
fn canceled_with(dispatch: &PanePointerDispatch, reason: PaneCancelReason) -> bool {
    matches!(
        dispatch.semantic_event.as_ref().map(|e| &e.kind),
        Some(PaneSemanticInputEventKind::Cancel { reason: r, .. }) if *r == reason
    )
}

// ---------------------------------------------------------------------------
// A. Pointer drag -> live tree mutation
// ---------------------------------------------------------------------------

/// Drive a full host pointer-capture lifecycle (down -> acquire -> move* -> up)
/// for a horizontal root split and assert the first child grows, the capture is
/// acquired then released, and the machine returns to idle.
#[test]
fn web_pointer_drag_grows_first_child() {
    let mut tree = root_split_tree(SplitAxis::Horizontal);
    let layout = tree
        .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
        .expect("layout solves");
    let split = pane_id(1);
    let target = resize_target(split, SplitAxis::Horizontal);
    let before = first_share_bps(&tree, split);

    let mut adapter = adapter();
    let mut seed = 4_000u64;
    let mut applied = 0usize;

    let down = adapter.pointer_down(
        target,
        7,
        PanePointerButton::Primary,
        pos(25, 10),
        modifiers(),
    );
    assert!(
        matches!(
            down.capture_command,
            Some(PanePointerCaptureCommand::Acquire { pointer_id: 7 })
        ),
        "pointer-down must request browser pointer capture"
    );
    applied += apply_dispatch(&mut tree, &layout, &down, &mut seed);

    // Host confirms setPointerCapture() succeeded.
    let _ = adapter.capture_acquired(7);

    for x in [30, 34, 38] {
        let moved = adapter.pointer_move(7, pos(x, 10), modifiers());
        applied += apply_dispatch(&mut tree, &layout, &moved, &mut seed);
    }

    let up = adapter.pointer_up(7, PanePointerButton::Primary, pos(38, 10), modifiers());
    assert!(
        released(&up),
        "pointer-up must release browser pointer capture"
    );
    applied += apply_dispatch(&mut tree, &layout, &up, &mut seed);

    let after = first_share_bps(&tree, split);
    assert!(applied > 0, "drag must yield geometry operations");
    assert!(
        after > before,
        "rightward drag should grow first child: after={after} before={before}"
    );
    assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    assert_eq!(adapter.active_pointer_id(), None);
    emit_trace(
        "pointer_drag_grow",
        "horizontal",
        before,
        after,
        applied,
        true,
        false,
        true,
    );
}

/// A leftward drag shrinks the first child (direction symmetry).
#[test]
fn web_pointer_drag_left_shrinks_first_child() {
    let mut tree = root_split_tree(SplitAxis::Horizontal);
    let layout = tree
        .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
        .expect("layout solves");
    let split = pane_id(1);
    let target = resize_target(split, SplitAxis::Horizontal);
    let before = first_share_bps(&tree, split);

    let mut adapter = adapter();
    let mut seed = 5_000u64;
    let mut applied = 0usize;

    let down = adapter.pointer_down(
        target,
        3,
        PanePointerButton::Primary,
        pos(25, 10),
        modifiers(),
    );
    applied += apply_dispatch(&mut tree, &layout, &down, &mut seed);
    let _ = adapter.capture_acquired(3);
    for x in [20, 16, 12] {
        let moved = adapter.pointer_move(3, pos(x, 10), modifiers());
        applied += apply_dispatch(&mut tree, &layout, &moved, &mut seed);
    }
    let up = adapter.pointer_up(3, PanePointerButton::Primary, pos(12, 10), modifiers());
    applied += apply_dispatch(&mut tree, &layout, &up, &mut seed);

    let after = first_share_bps(&tree, split);
    assert!(applied > 0, "drag must yield geometry operations");
    assert!(
        after < before,
        "leftward drag should shrink first child: after={after} before={before}"
    );
    assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    emit_trace(
        "pointer_drag_shrink",
        "horizontal",
        before,
        after,
        applied,
        true,
        false,
        true,
    );
}

/// A vertical root split resizes along the Y axis.
#[test]
fn web_pointer_drag_vertical_axis() {
    let mut tree = root_split_tree(SplitAxis::Vertical);
    let layout = tree
        .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
        .expect("layout solves");
    let split = pane_id(1);
    let target = resize_target(split, SplitAxis::Vertical);
    let before = first_share_bps(&tree, split);

    let mut adapter = adapter();
    let mut seed = 6_000u64;
    let mut applied = 0usize;

    let down = adapter.pointer_down(
        target,
        9,
        PanePointerButton::Primary,
        pos(25, 10),
        modifiers(),
    );
    applied += apply_dispatch(&mut tree, &layout, &down, &mut seed);
    let _ = adapter.capture_acquired(9);
    for y in [12, 14, 16] {
        let moved = adapter.pointer_move(9, pos(25, y), modifiers());
        applied += apply_dispatch(&mut tree, &layout, &moved, &mut seed);
    }
    let up = adapter.pointer_up(9, PanePointerButton::Primary, pos(25, 16), modifiers());
    applied += apply_dispatch(&mut tree, &layout, &up, &mut seed);

    let after = first_share_bps(&tree, split);
    assert!(applied > 0, "vertical drag must yield geometry operations");
    assert!(
        after > before,
        "downward drag should grow first (top) child: after={after} before={before}"
    );
    assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    emit_trace(
        "pointer_drag_vertical",
        "vertical",
        before,
        after,
        applied,
        true,
        false,
        true,
    );
}

// ---------------------------------------------------------------------------
// B. Touch
// ---------------------------------------------------------------------------

/// A single-touch drag (active_touch_points == 1) follows the primary-button
/// path and produces the same final ratio as the equivalent mouse drag.
#[test]
fn web_single_touch_drag_matches_mouse_drag() {
    // Mouse reference.
    let mouse_final = {
        let mut tree = root_split_tree(SplitAxis::Horizontal);
        let layout = tree
            .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
            .expect("layout solves");
        let target = resize_target(pane_id(1), SplitAxis::Horizontal);
        let mut adapter = adapter();
        let mut seed = 7_000u64;
        let d = adapter.pointer_down(
            target,
            1,
            PanePointerButton::Primary,
            pos(25, 10),
            modifiers(),
        );
        apply_dispatch(&mut tree, &layout, &d, &mut seed);
        let _ = adapter.capture_acquired(1);
        for x in [30, 34, 38] {
            let m = adapter.pointer_move(1, pos(x, 10), modifiers());
            apply_dispatch(&mut tree, &layout, &m, &mut seed);
        }
        let u = adapter.pointer_up(1, PanePointerButton::Primary, pos(38, 10), modifiers());
        apply_dispatch(&mut tree, &layout, &u, &mut seed);
        first_share_bps(&tree, pane_id(1))
    };

    // Touch path: identical coordinate stream via touch_pointer_down.
    let mut tree = root_split_tree(SplitAxis::Horizontal);
    let layout = tree
        .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
        .expect("layout solves");
    let split = pane_id(1);
    let target = resize_target(split, SplitAxis::Horizontal);
    let before = first_share_bps(&tree, split);
    let mut adapter = adapter();
    let mut seed = 7_500u64;
    let mut applied = 0usize;

    let down = adapter.touch_pointer_down(target, 1, pos(25, 10), 1, modifiers());
    applied += apply_dispatch(&mut tree, &layout, &down, &mut seed);
    let _ = adapter.capture_acquired(1);
    for x in [30, 34, 38] {
        let m = adapter.pointer_move(1, pos(x, 10), modifiers());
        applied += apply_dispatch(&mut tree, &layout, &m, &mut seed);
    }
    let u = adapter.pointer_up(1, PanePointerButton::Primary, pos(38, 10), modifiers());
    applied += apply_dispatch(&mut tree, &layout, &u, &mut seed);

    let touch_final = first_share_bps(&tree, split);
    assert_eq!(
        touch_final, mouse_final,
        "single-touch drag must match mouse drag exactly"
    );
    assert!(applied > 0);
    emit_trace(
        "touch_drag_parity",
        "single_touch",
        before,
        touch_final,
        applied,
        true,
        false,
        true,
    );
}

/// A second concurrent touch yields the pane gesture to the host's native
/// pinch/scroll layer: the active capture is canceled and released, no tree
/// mutation occurs, and the machine returns to idle.
#[test]
fn web_second_touch_yields_to_pinch_layer() {
    let mut tree = root_split_tree(SplitAxis::Horizontal);
    let layout = tree
        .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
        .expect("layout solves");
    let split = pane_id(1);
    let target = resize_target(split, SplitAxis::Horizontal);
    let before = first_share_bps(&tree, split);
    let mut adapter = adapter();
    let mut seed = 8_000u64;

    let down = adapter.touch_pointer_down(target, 1, pos(25, 10), 1, modifiers());
    let mut applied = apply_dispatch(&mut tree, &layout, &down, &mut seed);
    let _ = adapter.capture_acquired(1);

    // Second finger lands -> multi-touch arbitration cancels the pane gesture.
    let second = adapter.touch_pointer_down(target, 2, pos(40, 14), 2, modifiers());
    applied += apply_dispatch(&mut tree, &layout, &second, &mut seed);

    assert!(
        canceled_with(&second, PaneCancelReason::PointerCancel),
        "second touch must cancel the pane gesture for the pinch layer"
    );
    assert!(
        released(&second),
        "second touch must release pointer capture"
    );
    let after = first_share_bps(&tree, split);
    assert_eq!(after, before, "yielding to pinch must not mutate the tree");
    assert_eq!(applied, 0, "no geometry ops when gesture never committed");
    assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    assert_eq!(adapter.active_pointer_id(), None);
    emit_trace(
        "touch_multi_yield",
        "multi_touch",
        before,
        after,
        applied,
        false,
        true,
        true,
    );
}

// ---------------------------------------------------------------------------
// C. DPR / zoom / viewport invariance (no geometry drift)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum InputKind {
    Css,
    Device,
}

/// A device-independent viewport profile: how the host expresses the same
/// logical (cell-space) splitter location as raw browser coordinates.
struct ViewportProfile {
    name: &'static str,
    normalizer: PaneCoordinateNormalizer,
    kind: InputKind,
}

/// Project a canonical cell coordinate into a profile's raw input coordinate,
/// sampling the CENTER of the cell so the inverse normalization is exact (free
/// of boundary-tie rounding). This is the forward of `normalize()`.
fn project(profile: &ViewportProfile, cell: PanePointerPosition) -> PaneInputCoordinate {
    let n = &profile.normalizer;
    let w = i64::from(n.cell_width_css);
    let h = i64::from(n.cell_height_css);
    // Cell center in pre-zoom, viewport-local CSS pixels.
    let local_css_x = i64::from(cell.x) * w + w / 2;
    let local_css_y = i64::from(cell.y) * h + h / 2;
    // Apply zoom (CSS pixels are zoom-scaled), then viewport origin offset.
    let zoom_num = i64::from(n.zoom.numerator());
    let zoom_den = i64::from(n.zoom.denominator());
    let abs_css_x = local_css_x * zoom_num / zoom_den + i64::from(n.viewport_origin_css.x);
    let abs_css_y = local_css_y * zoom_num / zoom_den + i64::from(n.viewport_origin_css.y);
    match profile.kind {
        InputKind::Css => PaneInputCoordinate::CssPixels {
            position: pos(abs_css_x as i32, abs_css_y as i32),
        },
        InputKind::Device => {
            let dpr_num = i64::from(n.dpr.numerator());
            let dpr_den = i64::from(n.dpr.denominator());
            PaneInputCoordinate::DevicePixels {
                position: pos(
                    (abs_css_x * dpr_num / dpr_den) as i32,
                    (abs_css_y * dpr_num / dpr_den) as i32,
                ),
            }
        }
    }
}

/// Normalize a canonical cell through a profile and return the recovered
/// viewport-local cell. Asserts the round-trip is lossless (the drift guard).
fn normalize_cell(profile: &ViewportProfile, cell: PanePointerPosition) -> PanePointerPosition {
    let input = project(profile, cell);
    let normalized = profile
        .normalizer
        .normalize(input)
        .expect("coordinate normalizes");
    assert_eq!(
        normalized.local_cell, cell,
        "profile {} drifted: cell {:?} -> {:?}",
        profile.name, cell, normalized.local_cell
    );
    normalized.local_cell
}

fn scale(num: u32, den: u32) -> PaneScaleFactor {
    PaneScaleFactor::new(num, den).expect("valid scale factor")
}

fn invariance_profiles() -> Vec<ViewportProfile> {
    let origin0 = pos(0, 0);
    let cells0 = pos(0, 0);
    let rounding = PaneCoordinateRoundingPolicy::TowardNegativeInfinity;
    vec![
        ViewportProfile {
            name: "css_1x",
            normalizer: PaneCoordinateNormalizer::new(
                origin0,
                cells0,
                10,
                10,
                PaneScaleFactor::ONE,
                PaneScaleFactor::ONE,
                rounding,
            )
            .unwrap(),
            kind: InputKind::Css,
        },
        ViewportProfile {
            name: "device_2x",
            normalizer: PaneCoordinateNormalizer::new(
                origin0,
                cells0,
                10,
                10,
                scale(2, 1),
                PaneScaleFactor::ONE,
                rounding,
            )
            .unwrap(),
            kind: InputKind::Device,
        },
        ViewportProfile {
            name: "device_4x",
            normalizer: PaneCoordinateNormalizer::new(
                origin0,
                cells0,
                10,
                10,
                scale(4, 1),
                PaneScaleFactor::ONE,
                rounding,
            )
            .unwrap(),
            kind: InputKind::Device,
        },
        ViewportProfile {
            name: "css_origin_offset",
            normalizer: PaneCoordinateNormalizer::new(
                pos(120, 48),
                pos(3, 1),
                10,
                10,
                PaneScaleFactor::ONE,
                PaneScaleFactor::ONE,
                rounding,
            )
            .unwrap(),
            kind: InputKind::Css,
        },
        ViewportProfile {
            name: "device_2x_cell_8x16",
            normalizer: PaneCoordinateNormalizer::new(
                origin0,
                cells0,
                8,
                16,
                scale(2, 1),
                PaneScaleFactor::ONE,
                rounding,
            )
            .unwrap(),
            kind: InputKind::Device,
        },
    ]
}

/// The headline "no drift" proof: the same logical (cell-space) drag, expressed
/// as raw browser coordinates under a matrix of DPR / viewport-origin / cell-size
/// profiles, normalizes back to identical cells and therefore produces a
/// byte-identical final pane ratio across every profile.
#[test]
fn web_drag_is_invariant_across_dpr_and_viewport() {
    // Canonical drag, defined once in cell space (matches the mouse drag above).
    let down_cell = pos(25, 10);
    let move_cells = [pos(30, 10), pos(34, 10), pos(38, 10)];

    let mut results: Vec<(&'static str, u32)> = Vec::new();
    let mut initial = 0u32;

    for profile in invariance_profiles() {
        let mut tree = root_split_tree(SplitAxis::Horizontal);
        let layout = tree
            .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
            .expect("layout solves");
        let split = pane_id(1);
        let target = resize_target(split, SplitAxis::Horizontal);
        initial = first_share_bps(&tree, split);
        let mut adapter = adapter();
        let mut seed = 11_000u64;
        let mut applied = 0usize;

        let d = adapter.pointer_down(
            target,
            5,
            PanePointerButton::Primary,
            normalize_cell(&profile, down_cell),
            modifiers(),
        );
        applied += apply_dispatch(&mut tree, &layout, &d, &mut seed);
        let _ = adapter.capture_acquired(5);
        let mut last = down_cell;
        for &c in &move_cells {
            last = normalize_cell(&profile, c);
            let m = adapter.pointer_move(5, last, modifiers());
            applied += apply_dispatch(&mut tree, &layout, &m, &mut seed);
        }
        let u = adapter.pointer_up(5, PanePointerButton::Primary, last, modifiers());
        applied += apply_dispatch(&mut tree, &layout, &u, &mut seed);

        let after = first_share_bps(&tree, split);
        assert!(applied > 0, "{} produced no ops", profile.name);
        emit_trace(
            "viewport_invariance",
            profile.name,
            initial,
            after,
            applied,
            true,
            false,
            true,
        );
        results.push((profile.name, after));
    }

    let baseline = results[0].1;
    assert!(baseline > initial, "drag must grow the first child");
    for (name, final_bps) in &results {
        assert_eq!(
            *final_bps, baseline,
            "geometry drift across viewport profiles: {name} -> {final_bps} != baseline {baseline}"
        );
    }
}

/// Focused round-trip: the same logical cell expressed as CSS pixels,
/// device pixels at 2x and 3x, and a zoomed CSS frame all normalize to the
/// same cell. Complements the end-to-end drift test with exact zoom coverage.
#[test]
fn web_coordinate_normalizer_round_trips_dpr_and_zoom() {
    let rounding = PaneCoordinateRoundingPolicy::TowardNegativeInfinity;
    // cell_width=12 makes zoom=3/2 exact for cell-center sampling (6 px center).
    let profiles = [
        ViewportProfile {
            name: "css_identity",
            normalizer: PaneCoordinateNormalizer::new(
                pos(0, 0),
                pos(0, 0),
                12,
                12,
                PaneScaleFactor::ONE,
                PaneScaleFactor::ONE,
                rounding,
            )
            .unwrap(),
            kind: InputKind::Css,
        },
        ViewportProfile {
            name: "device_2x",
            normalizer: PaneCoordinateNormalizer::new(
                pos(0, 0),
                pos(0, 0),
                12,
                12,
                scale(2, 1),
                PaneScaleFactor::ONE,
                rounding,
            )
            .unwrap(),
            kind: InputKind::Device,
        },
        ViewportProfile {
            name: "device_3x",
            normalizer: PaneCoordinateNormalizer::new(
                pos(0, 0),
                pos(0, 0),
                12,
                12,
                scale(3, 1),
                PaneScaleFactor::ONE,
                rounding,
            )
            .unwrap(),
            kind: InputKind::Device,
        },
        ViewportProfile {
            name: "css_zoom_3_2",
            normalizer: PaneCoordinateNormalizer::new(
                pos(0, 0),
                pos(0, 0),
                12,
                12,
                PaneScaleFactor::ONE,
                scale(3, 2),
                rounding,
            )
            .unwrap(),
            kind: InputKind::Css,
        },
    ];

    for cell in [pos(0, 0), pos(7, 3), pos(25, 10), pos(49, 19)] {
        for profile in &profiles {
            // `normalize_cell` asserts the local_cell round-trips exactly.
            let recovered = normalize_cell(profile, cell);
            assert_eq!(recovered, cell);
        }
    }
}

// ---------------------------------------------------------------------------
// D. Interruption / focus-recovery matrix
// ---------------------------------------------------------------------------

/// Helper: arm a drag (down + acquire) on a fresh horizontal split, returning
/// the adapter primed for an interruption. No moves are applied, so the tree is
/// still at its initial ratio.
fn primed_drag(pointer_id: u32) -> (PaneTree, PaneLayout, PanePointerCaptureAdapter, u32) {
    let mut tree = root_split_tree(SplitAxis::Horizontal);
    let layout = tree
        .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
        .expect("layout solves");
    let split = pane_id(1);
    let target = resize_target(split, SplitAxis::Horizontal);
    let before = first_share_bps(&tree, split);
    let mut adapter = adapter();
    let mut seed = 20_000u64;
    let down = adapter.pointer_down(
        target,
        pointer_id,
        PanePointerButton::Primary,
        pos(25, 10),
        modifiers(),
    );
    let applied = apply_dispatch(&mut tree, &layout, &down, &mut seed);
    assert_eq!(applied, 0, "armed-only state must not mutate the tree yet");
    (tree, layout, adapter, before)
}

#[test]
fn web_pointer_cancel_aborts_drag_cleanly() {
    let (tree, _layout, mut adapter, before) = primed_drag(7);
    let _ = adapter.capture_acquired(7);
    let cancel = adapter.pointer_cancel(Some(7));
    assert!(canceled_with(&cancel, PaneCancelReason::PointerCancel));
    assert!(released(&cancel), "pointercancel must release capture");
    assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    assert_eq!(adapter.active_pointer_id(), None);
    let after = first_share_bps(&tree, pane_id(1));
    assert_eq!(after, before);
    emit_trace(
        "interrupt_cancel",
        "pointercancel",
        before,
        after,
        0,
        false,
        true,
        true,
    );
}

#[test]
fn web_blur_releases_active_capture() {
    let (tree, _layout, mut adapter, before) = primed_drag(7);
    let _ = adapter.capture_acquired(7);
    let blur = adapter.blur();
    // The blur path forwards a dedicated `Blur` semantic (not a `Cancel`) whose
    // transition cancels the active gesture.
    assert!(
        matches!(
            blur.semantic_event.as_ref().map(|e| &e.kind),
            Some(PaneSemanticInputEventKind::Blur { .. })
        ),
        "blur must forward a Blur semantic event"
    );
    assert!(released(&blur), "blur must release capture");
    assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    assert_eq!(adapter.active_pointer_id(), None);
    let after = first_share_bps(&tree, pane_id(1));
    assert_eq!(after, before);
    emit_trace(
        "interrupt_blur",
        "blur",
        before,
        after,
        0,
        false,
        true,
        true,
    );
}

#[test]
fn web_visibility_hidden_cancels_on_tab_switch() {
    let (tree, _layout, mut adapter, before) = primed_drag(7);
    let _ = adapter.capture_acquired(7);
    let hidden = adapter.visibility_hidden();
    assert!(canceled_with(&hidden, PaneCancelReason::FocusLost));
    assert!(released(&hidden), "tab-switch must release capture");
    assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    let after = first_share_bps(&tree, pane_id(1));
    assert_eq!(after, before);
    emit_trace(
        "interrupt_tab_switch",
        "visibility_hidden",
        before,
        after,
        0,
        false,
        true,
        true,
    );
}

#[test]
fn web_lost_pointer_capture_does_not_double_release() {
    let (tree, _layout, mut adapter, before) = primed_drag(7);
    let _ = adapter.capture_acquired(7);
    let lost = adapter.lost_pointer_capture(7);
    assert!(canceled_with(&lost, PaneCancelReason::PointerCancel));
    assert!(
        !released(&lost),
        "lostpointercapture must NOT re-release capture the browser already took"
    );
    assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    assert_eq!(adapter.active_pointer_id(), None);
    let after = first_share_bps(&tree, pane_id(1));
    assert_eq!(after, before);
    emit_trace(
        "interrupt_lost_capture",
        "lost_pointer_capture",
        before,
        after,
        0,
        false,
        true,
        true,
    );
}

#[test]
fn web_context_lost_cancels_active_gesture() {
    let (tree, _layout, mut adapter, before) = primed_drag(7);
    let _ = adapter.capture_acquired(7);
    let lost = adapter.context_lost();
    assert!(canceled_with(&lost, PaneCancelReason::ContextLost));
    assert!(released(&lost));
    assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    let after = first_share_bps(&tree, pane_id(1));
    assert_eq!(after, before);
    emit_trace(
        "interrupt_context_lost",
        "context_lost",
        before,
        after,
        0,
        false,
        true,
        true,
    );
}

#[test]
fn web_render_stall_before_capture_ack_cancels_without_release() {
    // No capture_acquired() -> capture still merely Requested.
    let (tree, _layout, mut adapter, before) = primed_drag(7);
    let stalled = adapter.render_stalled();
    assert!(canceled_with(&stalled, PaneCancelReason::RenderStalled));
    assert!(
        !released(&stalled),
        "no capture was acquired, so nothing to release"
    );
    assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    let after = first_share_bps(&tree, pane_id(1));
    assert_eq!(after, before);
    emit_trace(
        "interrupt_render_stall",
        "render_stalled",
        before,
        after,
        0,
        false,
        true,
        true,
    );
}

#[test]
fn web_pointer_leave_before_capture_ack_cancels() {
    // Capture requested but not yet acknowledged: leaving the element cancels.
    let (tree, _layout, mut adapter, before) = primed_drag(7);
    let leave = adapter.pointer_leave(7);
    assert!(canceled_with(&leave, PaneCancelReason::PointerCancel));
    assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    assert_eq!(adapter.active_pointer_id(), None);
    let after = first_share_bps(&tree, pane_id(1));
    assert_eq!(after, before);
    emit_trace(
        "interrupt_leave_pre_ack",
        "pointer_leave",
        before,
        after,
        0,
        false,
        true,
        true,
    );
}

#[test]
fn web_pointer_leave_after_capture_ack_is_ignored() {
    // Once capture is acknowledged, leaving the element must NOT cancel the drag
    // (the browser keeps routing moves to the captured target).
    let (tree, _layout, mut adapter, before) = primed_drag(7);
    let _ = adapter.capture_acquired(7);
    let leave = adapter.pointer_leave(7);
    assert!(
        leave.semantic_event.is_none() && leave.transition.is_none(),
        "leave after capture ack must be ignored"
    );
    assert_eq!(
        adapter.active_pointer_id(),
        Some(7),
        "captured gesture stays active across leave"
    );
    let after = first_share_bps(&tree, pane_id(1));
    assert_eq!(after, before);
    emit_trace(
        "interrupt_leave_post_ack",
        "pointer_leave",
        before,
        after,
        0,
        false,
        false,
        false,
    );
}

// ---------------------------------------------------------------------------
// E. Deterministic replay spine
// ---------------------------------------------------------------------------

/// The web input stream emitted by a drag is captured into a canonical semantic
/// trace and replayed through two independent fresh machines. Both replays must
/// produce identical transitions, final state, and checksum — the determinism
/// guarantee the parent epic (bd-a46q1) treats as the validation spine, and the
/// same trace/replay contract the terminal lane exercises via
/// `pane_semantic_replay_harness`.
#[test]
fn web_drag_semantic_trace_replays_deterministically() {
    let mut tree = root_split_tree(SplitAxis::Horizontal);
    let layout = tree
        .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
        .expect("layout solves");
    let target = resize_target(pane_id(1), SplitAxis::Horizontal);
    let mut adapter = adapter();
    let mut seed = 30_000u64;
    let mut events: Vec<PaneSemanticInputEvent> = Vec::new();

    let mut collect = |dispatch: &PanePointerDispatch| {
        if let Some(event) = dispatch.semantic_event.clone() {
            events.push(event);
        }
    };

    let down = adapter.pointer_down(
        target,
        4,
        PanePointerButton::Primary,
        pos(25, 10),
        modifiers(),
    );
    collect(&down);
    apply_dispatch(&mut tree, &layout, &down, &mut seed);
    let _ = adapter.capture_acquired(4);
    for x in [30, 34, 38] {
        let m = adapter.pointer_move(4, pos(x, 10), modifiers());
        collect(&m);
        apply_dispatch(&mut tree, &layout, &m, &mut seed);
    }
    let up = adapter.pointer_up(4, PanePointerButton::Primary, pos(38, 10), modifiers());
    collect(&up);
    apply_dispatch(&mut tree, &layout, &up, &mut seed);

    assert!(
        events.len() >= 3,
        "drag should emit a non-trivial semantic stream, got {}",
        events.len()
    );

    let trace = PaneSemanticInputTrace::new(0, 0, "web-e2e", events).expect("valid semantic trace");
    trace.validate().expect("trace self-validates");

    let mut machine_a = PaneDragResizeMachine::default();
    let mut machine_b = PaneDragResizeMachine::default();
    let outcome_a = trace.replay(&mut machine_a).expect("replay A");
    let outcome_b = trace.replay(&mut machine_b).expect("replay B");

    assert_eq!(
        outcome_a.trace_checksum, outcome_b.trace_checksum,
        "replayed checksums diverged"
    );
    assert_eq!(
        outcome_a.final_state, outcome_b.final_state,
        "replayed final state diverged"
    );
    assert_eq!(
        outcome_a.transitions, outcome_b.transitions,
        "replayed transition stream diverged"
    );
    assert_eq!(
        outcome_a.trace_checksum,
        trace.recompute_checksum(),
        "trace checksum is unstable"
    );
    println!(
        "PANE_WEB_TRACE scenario=replay_determinism profile=semantic_trace events={} \
         checksum={} transitions={} final_idle={}",
        trace.events.len(),
        outcome_a.trace_checksum,
        outcome_a.transitions.len(),
        matches!(outcome_a.final_state, PaneDragResizeState::Idle),
    );
}

// ---------------------------------------------------------------------------
// F. Keyboard resize (reachable semantic/bridge layer only — see honesty note)
// ---------------------------------------------------------------------------

/// Drive a canonical `KeyboardResize` SEMANTIC event through the shared
/// machine + layout bridge — the identical host-agnostic path a future web
/// keyboard binding (bd-21pbi.3) will feed once the canonical command model
/// (bd-21pbi.1) lands. This proves the reachable downstream half on the web
/// host's own data types; the browser `KeyboardEvent` -> semantic translation
/// plus roving tabindex / ARIA remain bd-21pbi.3 scope and are covered by a
/// filed follow-up bead.
#[test]
fn web_keyboard_resize_semantic_event_drives_tree_via_bridge() {
    let mut tree = root_split_tree(SplitAxis::Horizontal);
    let layout = tree
        .solve_layout(Rect::new(0, 0, VIEW_W, VIEW_H))
        .expect("layout solves");
    let split = pane_id(1);
    let target = resize_target(split, SplitAxis::Horizontal);
    let before = first_share_bps(&tree, split);

    // Increase by 3 units -> +3 * PANE_SNAP_DEFAULT_STEP_BPS, applied to a 1:1
    // split (5000 bps), via a fresh machine just like a host keyboard binding.
    let mut machine = PaneDragResizeMachine::default();
    let grow = PaneSemanticInputEvent::new(
        1,
        PaneSemanticInputEventKind::KeyboardResize {
            target,
            direction: PaneResizeDirection::Increase,
            units: 3,
        },
    );
    let transition = machine.apply_event(&grow).expect("keyboard event applies");
    let ops = tree.operations_for_transition(&transition, &layout, NEUTRAL);
    assert!(!ops.is_empty(), "keyboard resize must yield operations");
    let mut seed = 40_000u64;
    for op in ops {
        tree.apply_operation(seed, op).expect("operation applies");
        seed += 1;
    }
    let after_grow = first_share_bps(&tree, split);
    let expected = before + 3 * u32::from(PANE_SNAP_DEFAULT_STEP_BPS);
    assert_eq!(
        after_grow, expected,
        "increase-by-3 should nudge first share by 3 steps: {after_grow} != {expected}"
    );

    // Decrease by 1 unit -> -1 step.
    let shrink = PaneSemanticInputEvent::new(
        2,
        PaneSemanticInputEventKind::KeyboardResize {
            target,
            direction: PaneResizeDirection::Decrease,
            units: 1,
        },
    );
    let transition = machine
        .apply_event(&shrink)
        .expect("keyboard event applies");
    let ops = tree.operations_for_transition(&transition, &layout, NEUTRAL);
    for op in ops {
        tree.apply_operation(seed, op).expect("operation applies");
        seed += 1;
    }
    let after_shrink = first_share_bps(&tree, split);
    assert_eq!(
        after_shrink,
        after_grow - u32::from(PANE_SNAP_DEFAULT_STEP_BPS),
        "decrease-by-1 should reverse one step"
    );
    emit_trace(
        "keyboard_bridge",
        "semantic",
        before,
        after_shrink,
        2,
        true,
        false,
        true,
    );
}

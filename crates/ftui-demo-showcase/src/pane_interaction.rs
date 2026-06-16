#![forbid(unsafe_code)]

//! Shared pane interaction semantic primitives.
//!
//! This module intentionally contains host-agnostic behavior policy and
//! preview-state shaping used by both terminal and web adapters.

use ftui_layout::{
    PANE_SNAP_DEFAULT_HYSTERESIS_BPS, PANE_SNAP_DEFAULT_STEP_BPS, PaneDockPreview, PaneDockZone,
    PaneId, PaneInertialThrow, PaneInteractionTimeline, PaneInteractionTimelineError, PaneLayout,
    PaneLeaf, PaneMotionVector, PaneNodeKind, PaneOperation, PanePlacement, PanePointerPosition,
    PanePressureSnapProfile, PaneResizeGrip, PaneResizeTarget, PaneSelectionState, PaneSplitRatio,
    PaneTree, Rect, SplitAxis,
};

pub const PANE_MAGNETIC_FIELD_MIN_CELLS: f64 = 3.5;
pub const PANE_MAGNETIC_FIELD_MAX_CELLS: f64 = 11.0;
pub const DOCK_PREVIEW_CANDIDATE_LIMIT: usize = 3;
pub const LIVE_REFLOW_THRESHOLD_MIN_BPS: u16 = 3_600;
pub const LIVE_REFLOW_THRESHOLD_MAX_BPS: u16 = 8_200;
pub const LIVE_REFLOW_SWITCH_ADVANTAGE_MIN_BPS: u16 = 450;
pub const LIVE_REFLOW_SWITCH_ADVANTAGE_MAX_BPS: u16 = 1_650;
pub const DEFAULT_SPRING_BLEND_BPS: u16 = 3_500;
const FNV64_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV64_PRIME: u64 = 0x100000001b3;

/// Live preview metadata consumed by host renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PanePreviewState {
    pub source: Option<PaneId>,
    pub target: Option<PaneId>,
    pub zone: Option<PaneDockZone>,
    pub ghost_rect: Option<Rect>,
    pub dock_strength_bps: u16,
    pub motion_speed_cps: u16,
    pub alt_one_target: Option<PaneId>,
    pub alt_one_zone: Option<PaneDockZone>,
    pub alt_one_ghost_rect: Option<Rect>,
    pub alt_one_strength_bps: u16,
    pub alt_two_target: Option<PaneId>,
    pub alt_two_zone: Option<PaneDockZone>,
    pub alt_two_ghost_rect: Option<Rect>,
    pub alt_two_strength_bps: u16,
    pub selection_bounds: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneSplitterVisualState {
    Idle,
    Hover,
    Active,
}

/// Host-agnostic splitter/handle primitive derived from solved pane layouts.
///
/// Both terminal and web hosts can consume this shape data directly so handle
/// geometry and visual states stay deterministic across renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneSplitterPrimitive {
    pub split_id: PaneId,
    pub target: PaneResizeTarget,
    pub axis: SplitAxis,
    pub rail_rect: Rect,
    pub handle_rect: Rect,
    pub state: PaneSplitterVisualState,
}

/// Lightweight timeline status for host HUD updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneTimelineStatus {
    pub cursor: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneGestureMode {
    Move,
    Resize(PaneResizeGrip),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivePaneGesture {
    pub pointer_id: u32,
    pub leaf: PaneId,
    pub mode: PaneGestureMode,
    pub coalesce_after_operation_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneAutoPointerDownContext {
    pub target: PaneResizeTarget,
    pub leaf: PaneId,
    pub mode: PaneGestureMode,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneMovePolicy {
    pub projected_pointer: PanePointerPosition,
    pub inertial_throw: Option<PaneInertialThrow>,
    pub magnetic_field_cells: f64,
    pub switch_advantage_bps: u16,
    pub live_reflow_threshold_bps: u16,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneMovePolicyInput {
    pub pointer: PanePointerPosition,
    pub projected_position: Option<PanePointerPosition>,
    pub motion: PaneMotionVector,
    pub pressure: PanePressureSnapProfile,
    pub release_inertial_throw: Option<PaneInertialThrow>,
    pub committed: bool,
    pub viewport: Rect,
    pub baseline_magnetic_field_cells: f64,
}

pub struct PaneGestureArmState<'a> {
    pub active_gesture: &'a mut Option<ActivePaneGesture>,
    pub gesture_timeline_cursor_start: &'a mut Option<usize>,
    pub live_reflow_signature: &'a mut Option<u64>,
    pub preview_state: &'a mut PanePreviewState,
}

pub struct PaneTimelineApplyState<'a> {
    pub layout_tree: &'a mut PaneTree,
    pub timeline: &'a mut PaneInteractionTimeline,
    pub next_operation_id: &'a mut u64,
    pub workspace_generation: &'a mut u64,
}

pub struct PaneLiveReflowState<'a> {
    pub layout_tree: &'a mut PaneTree,
    pub timeline: &'a mut PaneInteractionTimeline,
    pub next_operation_id: &'a mut u64,
    pub workspace_generation: &'a mut u64,
    pub live_reflow_signature: &'a mut Option<u64>,
}

pub struct PaneDragSemanticsContext<'a> {
    pub layout_tree: &'a mut PaneTree,
    pub timeline: &'a mut PaneInteractionTimeline,
    pub next_operation_id: &'a mut u64,
    pub workspace_generation: &'a mut u64,
    pub selection: &'a PaneSelectionState,
    pub preview_state: &'a mut PanePreviewState,
    pub live_reflow_signature: &'a mut Option<u64>,
    pub viewport: Rect,
    pub baseline_magnetic_field_cells: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneDragSemanticsInput {
    pub sequence: u64,
    pub active: ActivePaneGesture,
    pub pointer: PanePointerPosition,
    pub pressure: PanePressureSnapProfile,
    pub motion: PaneMotionVector,
    pub projected_position: Option<PanePointerPosition>,
    pub inertial_throw: Option<PaneInertialThrow>,
    pub committed: bool,
}

#[must_use]
pub fn adaptive_magnetic_field_cells(
    baseline_cells: f64,
    motion: PaneMotionVector,
    pressure: PanePressureSnapProfile,
) -> f64 {
    let speed_factor = (motion.speed / 90.0).clamp(0.0, 1.0);
    let confidence = (f64::from(pressure.strength_bps) / 10_000.0).clamp(0.0, 1.0);
    let noise_penalty = (f64::from(motion.direction_changes) / 10.0).clamp(0.0, 1.0);
    (baseline_cells + speed_factor * 3.5 + confidence * 1.8 - noise_penalty * 1.6)
        .clamp(PANE_MAGNETIC_FIELD_MIN_CELLS, PANE_MAGNETIC_FIELD_MAX_CELLS)
}

#[must_use]
pub fn pane_move_policy(input: PaneMovePolicyInput) -> PaneMovePolicy {
    let projected_pointer = if input.committed {
        edge_fling_projection(
            input.projected_position.unwrap_or(input.pointer),
            input.motion,
            input.viewport,
        )
    } else {
        input.pointer
    };
    let inertial_throw = if input.committed {
        input.release_inertial_throw
    } else if input.motion.speed > 18.0 {
        Some(PaneInertialThrow::from_motion(input.motion))
    } else {
        None
    };
    let magnetic_field_cells = adaptive_magnetic_field_cells(
        input.baseline_magnetic_field_cells,
        input.motion,
        input.pressure,
    );
    let switch_advantage_bps = dynamic_preview_switch_advantage_bps(input.motion, input.pressure);
    let live_reflow_threshold_bps = dynamic_live_reflow_threshold_bps(input.motion, input.pressure);
    PaneMovePolicy {
        projected_pointer,
        inertial_throw,
        magnetic_field_cells,
        switch_advantage_bps,
        live_reflow_threshold_bps,
    }
}

#[must_use]
pub fn dynamic_live_reflow_threshold_bps(
    motion: PaneMotionVector,
    pressure: PanePressureSnapProfile,
) -> u16 {
    let speed_factor = (motion.speed / 95.0).clamp(0.0, 1.0).powf(0.78);
    let confidence = (f64::from(pressure.strength_bps) / 10_000.0)
        .clamp(0.0, 1.0)
        .powf(0.9);
    let noise_penalty = (f64::from(motion.direction_changes) / 10.0).clamp(0.0, 1.0);
    let threshold = 7_100.0 - speed_factor * 1_850.0 - confidence * 1_050.0 + noise_penalty * 900.0;
    threshold.round().clamp(
        f64::from(LIVE_REFLOW_THRESHOLD_MIN_BPS),
        f64::from(LIVE_REFLOW_THRESHOLD_MAX_BPS),
    ) as u16
}

#[must_use]
pub fn dynamic_preview_switch_advantage_bps(
    motion: PaneMotionVector,
    pressure: PanePressureSnapProfile,
) -> u16 {
    let speed_factor = (motion.speed / 95.0).clamp(0.0, 1.0);
    let confidence = (f64::from(pressure.strength_bps) / 10_000.0).clamp(0.0, 1.0);
    let noise_penalty = (f64::from(motion.direction_changes) / 10.0).clamp(0.0, 1.0);
    let advantage =
        1_250.0 - speed_factor * 500.0 + noise_penalty * 420.0 + (1.0 - confidence) * 260.0;
    advantage.round().clamp(
        f64::from(LIVE_REFLOW_SWITCH_ADVANTAGE_MIN_BPS),
        f64::from(LIVE_REFLOW_SWITCH_ADVANTAGE_MAX_BPS),
    ) as u16
}

#[must_use]
pub fn adaptive_dock_strength_bps(
    score: f64,
    motion: PaneMotionVector,
    pressure: PanePressureSnapProfile,
    committed: bool,
) -> u16 {
    if score <= 0.0 {
        return 0;
    }
    let base = score.clamp(0.0, 1.0) * 10_000.0;
    let speed = (motion.speed / 110.0).clamp(0.0, 1.0);
    let confidence = (f64::from(pressure.strength_bps) / 10_000.0).clamp(0.0, 1.0);
    let noise_penalty = (f64::from(motion.direction_changes) / 10.0).clamp(0.0, 1.0);
    let abs_dx = f64::from(motion.delta_x).abs();
    let abs_dy = f64::from(motion.delta_y).abs();
    let dominance = if (abs_dx + abs_dy) <= f64::EPSILON {
        0.0
    } else {
        (abs_dx - abs_dy).abs() / (abs_dx + abs_dy)
    };
    let assist =
        speed * 0.16 + confidence * 0.10 + dominance * 0.08 + if committed { 0.06 } else { 0.0 };
    let precision_penalty = (1.0 - speed) * (1.0 - confidence) * 0.12 + noise_penalty * 0.18;
    (base * (1.0 + assist - precision_penalty))
        .round()
        .clamp(0.0, 10_000.0) as u16
}

/// Adaptive splitter rail thickness (cell units) for host-agnostic rendering.
///
/// Larger canvases/terminals get a 2-cell rail for better pointer clarity.
#[must_use]
pub fn pane_splitter_rail_thickness(viewport: Rect) -> u16 {
    if viewport.width >= 90 && viewport.height >= 24 {
        2
    } else {
        1
    }
}

/// Deterministic handle length along the splitter major axis.
#[must_use]
pub fn pane_splitter_handle_span(split_rect: Rect, axis: SplitAxis) -> u16 {
    let major = match axis {
        SplitAxis::Horizontal => split_rect.height,
        SplitAxis::Vertical => split_rect.width,
    };
    match major {
        0..=6 => 1,
        7..=11 => 2,
        12..=18 => 3,
        19..=26 => 4,
        _ => 5,
    }
}

/// Build splitter/handle primitives with Idle/Hover/Active state.
///
/// This is intentionally renderer-independent: hosts decide *how* to draw,
/// while this function decides *what* to draw.
#[must_use]
pub fn collect_splitter_primitives(
    tree: &PaneTree,
    layout: &PaneLayout,
    viewport: Rect,
    hover_pointer: Option<PanePointerPosition>,
    active_gesture: Option<ActivePaneGesture>,
) -> Vec<PaneSplitterPrimitive> {
    let rail_thickness = pane_splitter_rail_thickness(viewport);
    let active_split = active_resize_split_id(tree, active_gesture);
    let mut primitives = Vec::new();

    for node in tree.nodes() {
        let PaneNodeKind::Split(split) = &node.kind else {
            continue;
        };
        let Some(split_rect) = layout.rect(node.id) else {
            continue;
        };
        let Some(first_rect) = layout.rect(split.first) else {
            continue;
        };
        let Some(second_rect) = layout.rect(split.second) else {
            continue;
        };
        if split_rect.is_empty() || first_rect.is_empty() || second_rect.is_empty() {
            continue;
        }

        let (mut rail, mut handle) = match split.axis {
            SplitAxis::Horizontal => {
                let divider_x = second_rect.x;
                let x = divider_x.saturating_sub(rail_thickness);
                let rail = Rect::new(x, split_rect.y, rail_thickness, split_rect.height);
                let handle_h = pane_splitter_handle_span(split_rect, split.axis)
                    .min(rail.height)
                    .max(1);
                let handle_y = rail.y + rail.height.saturating_sub(handle_h) / 2;
                let handle = Rect::new(rail.x, handle_y, rail.width.max(1), handle_h);
                (rail, handle)
            }
            SplitAxis::Vertical => {
                let divider_y = second_rect.y;
                let y = divider_y.saturating_sub(rail_thickness);
                let rail = Rect::new(split_rect.x, y, split_rect.width, rail_thickness);
                let handle_w = pane_splitter_handle_span(split_rect, split.axis)
                    .min(rail.width)
                    .max(1);
                let handle_x = rail.x + rail.width.saturating_sub(handle_w) / 2;
                let handle = Rect::new(handle_x, rail.y, handle_w, rail.height.max(1));
                (rail, handle)
            }
        };

        rail = rail.intersection(&viewport);
        handle = handle.intersection(&rail);
        if rail.is_empty() || handle.is_empty() {
            continue;
        }

        let mut state = PaneSplitterVisualState::Idle;
        if hover_pointer.is_some_and(|pointer| pointer_hits_rect(pointer, rail)) {
            state = PaneSplitterVisualState::Hover;
        }
        if active_split == Some(node.id) {
            state = PaneSplitterVisualState::Active;
        }

        primitives.push(PaneSplitterPrimitive {
            split_id: node.id,
            target: PaneResizeTarget {
                split_id: node.id,
                axis: split.axis,
            },
            axis: split.axis,
            rail_rect: rail,
            handle_rect: handle,
            state,
        });
    }

    primitives
}

/// Z-order for pane resize-overlay layers, painted back-to-front (bd-j1mo9).
///
/// Hosts paint lower `z_index` first so badges are never occluded by the ghost
/// boundary or snap guides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PaneOverlayLayer {
    /// Faint guide line marking the snap target the divider would settle on.
    SnapGuide,
    /// The proposed (ghost) divider position the resize would commit.
    GhostBoundary,
    /// Live size badges (topmost, never occluded).
    SizeBadge,
}

impl PaneOverlayLayer {
    /// Deterministic paint-order index (lower paints first / underneath).
    #[must_use]
    pub const fn z_index(self) -> u8 {
        match self {
            Self::SnapGuide => 0,
            Self::GhostBoundary => 1,
            Self::SizeBadge => 2,
        }
    }
}

/// A snap-guide line marking the snap target a live resize would settle on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneSnapGuide {
    /// One-cell-thick line rect spanning the split's cross axis.
    pub line: Rect,
    /// Split axis the guide belongs to.
    pub axis: SplitAxis,
    /// First-child share at this guide, in basis points (0..=10_000).
    pub share_bps: u16,
}

/// The proposed (ghost) divider position during a live resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneGhostBoundary {
    /// One-cell-thick rail rect at the proposed divider position.
    pub rail: Rect,
    /// Split axis the boundary belongs to.
    pub axis: SplitAxis,
    /// First-child share at the ghost position, in basis points.
    pub share_bps: u16,
    /// Whether the ghost is currently resting on a snap target.
    pub snapped: bool,
}

/// A live size badge for one pane affected by a resize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSizeBadge {
    /// The pane the badge describes.
    pub pane: PaneId,
    /// The pane's proposed rect (where the badge anchors).
    pub anchor: Rect,
    /// Deterministic label, e.g. `"62% 50x20"` (percentage + cell dimensions).
    pub label: String,
}

/// What to render in the resize overlay (the optional guide/badge toggle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneResizeOverlayConfig {
    /// Render the snap-target guide line.
    pub show_snap_guides: bool,
    /// Render live size badges.
    pub show_size_badges: bool,
    /// Snap granularity in basis points (matches `PaneSnapTuning::step_bps`).
    pub snap_step_bps: u16,
    /// Window (bps) within which the ghost is considered resting on a snap
    /// target (matches `PaneSnapTuning::hysteresis_bps`).
    pub snap_hysteresis_bps: u16,
}

impl Default for PaneResizeOverlayConfig {
    fn default() -> Self {
        Self {
            show_snap_guides: true,
            show_size_badges: true,
            snap_step_bps: PANE_SNAP_DEFAULT_STEP_BPS,
            snap_hysteresis_bps: PANE_SNAP_DEFAULT_HYSTERESIS_BPS,
        }
    }
}

/// Host-agnostic visual feedback for one active splitter resize (bd-j1mo9):
/// the proposed ghost boundary, the snap-target guide, and live size badges.
///
/// This is shape + label data only (no colors): hosts paint it using the
/// contrast-safe `ftui_style::PaneAffordanceTheme` (e.g. `splitter_active` for
/// the ghost, `snap` for guides) in the [`PaneOverlayLayer`] z-order. The
/// overlay is static geometry — it carries no animation — so it is inherently
/// reduced-motion safe and renders identically regardless of motion settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneResizeOverlay {
    /// The split being resized.
    pub split_id: PaneId,
    /// The split axis.
    pub axis: SplitAxis,
    /// The proposed divider position.
    pub ghost: PaneGhostBoundary,
    /// Snap-target guides (empty when disabled).
    pub snap_guides: Vec<PaneSnapGuide>,
    /// Live size badges (empty when disabled).
    pub badges: Vec<PaneSizeBadge>,
}

/// Compute proposed first/second child rects and the divider coordinate for a
/// split at `share_bps` (the cross-axis coordinate: x for a horizontal split's
/// vertical divider, y for a vertical split's horizontal divider).
fn proposed_split_rects(split_rect: Rect, axis: SplitAxis, share_bps: u16) -> (Rect, Rect, u16) {
    match axis {
        SplitAxis::Horizontal => {
            let w = split_rect.width;
            let first_w = ((u32::from(w) * u32::from(share_bps)) / 10_000) as u16;
            let divider = split_rect.x.saturating_add(first_w);
            let first = Rect::new(split_rect.x, split_rect.y, first_w, split_rect.height);
            let second = Rect::new(
                divider,
                split_rect.y,
                w.saturating_sub(first_w),
                split_rect.height,
            );
            (first, second, divider)
        }
        SplitAxis::Vertical => {
            let h = split_rect.height;
            let first_h = ((u32::from(h) * u32::from(share_bps)) / 10_000) as u16;
            let divider = split_rect.y.saturating_add(first_h);
            let first = Rect::new(split_rect.x, split_rect.y, split_rect.width, first_h);
            let second = Rect::new(
                split_rect.x,
                divider,
                split_rect.width,
                h.saturating_sub(first_h),
            );
            (first, second, divider)
        }
    }
}

/// Round `share_bps` to the nearest snap step, clamped to a valid interior share.
fn nearest_snap_bps(share_bps: u16, step_bps: u16) -> u16 {
    let step = u32::from(step_bps.max(1));
    let s = u32::from(share_bps);
    let snapped = ((s + step / 2) / step) * step;
    snapped.clamp(1, 9_999) as u16
}

/// Deterministic size-badge label: rounded percentage plus cell dimensions.
fn size_badge_label(share_bps: u16, rect: Rect) -> String {
    let pct = (u32::from(share_bps) + 50) / 100;
    format!("{pct}% {}x{}", rect.width, rect.height)
}

/// Build the resize-overlay feedback for a live splitter drag at
/// `proposed_share_bps` (the first-child share the drag is currently proposing).
///
/// Returns `None` if the target is not a live split on `axis`, or the split has
/// no solved rect. The overlay is a pure function of `(tree, layout, target,
/// share, config)`, so repeated builds for the same drag sample are byte
/// identical — feedback stays stable under rapid drag.
#[must_use]
pub fn build_resize_overlay(
    tree: &PaneTree,
    layout: &PaneLayout,
    target: PaneResizeTarget,
    proposed_share_bps: u16,
    config: PaneResizeOverlayConfig,
) -> Option<PaneResizeOverlay> {
    let node = tree.node(target.split_id)?;
    let PaneNodeKind::Split(split) = &node.kind else {
        return None;
    };
    if split.axis != target.axis {
        return None;
    }
    let split_rect = layout.rect(target.split_id)?;
    if split_rect.is_empty() {
        return None;
    }

    let share = proposed_share_bps.clamp(1, 9_999);
    let (first_rect, second_rect, divider) = proposed_split_rects(split_rect, split.axis, share);

    let rail = match split.axis {
        SplitAxis::Horizontal => Rect::new(divider, split_rect.y, 1, split_rect.height),
        SplitAxis::Vertical => Rect::new(split_rect.x, divider, split_rect.width, 1),
    };

    let step = config.snap_step_bps.max(1);
    let snapped_bps = nearest_snap_bps(share, step);
    // On a snap target only when within the hysteresis window of it (so a freely
    // dragged divider between grid points reads as un-snapped).
    let snapped = share.abs_diff(snapped_bps) <= config.snap_hysteresis_bps;

    let ghost = PaneGhostBoundary {
        rail,
        axis: split.axis,
        share_bps: share,
        snapped,
    };

    let mut snap_guides = Vec::new();
    if config.show_snap_guides {
        let (_, _, guide_pos) = proposed_split_rects(split_rect, split.axis, snapped_bps);
        let line = match split.axis {
            SplitAxis::Horizontal => Rect::new(guide_pos, split_rect.y, 1, split_rect.height),
            SplitAxis::Vertical => Rect::new(split_rect.x, guide_pos, split_rect.width, 1),
        };
        snap_guides.push(PaneSnapGuide {
            line,
            axis: split.axis,
            share_bps: snapped_bps,
        });
    }

    let mut badges = Vec::new();
    if config.show_size_badges {
        badges.push(PaneSizeBadge {
            pane: split.first,
            anchor: first_rect,
            label: size_badge_label(share, first_rect),
        });
        badges.push(PaneSizeBadge {
            pane: split.second,
            anchor: second_rect,
            label: size_badge_label(10_000 - share, second_rect),
        });
    }

    Some(PaneResizeOverlay {
        split_id: target.split_id,
        axis: split.axis,
        ghost,
        snap_guides,
        badges,
    })
}

/// Pointer input modality, which sets the minimum comfortable hit-target size
/// for pane affordances (bd-c2z7c). Touch needs a larger target than a mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneInputModality {
    /// A precise pointer (mouse / trackpad).
    Mouse,
    /// A coarse pointer (touch / pen), needing a larger target per WCAG 2.5.5.
    Touch,
}

impl PaneInputModality {
    /// Minimum comfortable thickness (cells) on the handle's minor axis — the
    /// thin dimension the user must land on.
    #[must_use]
    pub const fn min_target_thickness(self) -> u16 {
        match self {
            Self::Mouse => 1,
            Self::Touch => 2,
        }
    }

    /// Minimum comfortable span (cells) on the handle's major axis.
    #[must_use]
    pub const fn min_target_span(self) -> u16 {
        match self {
            Self::Mouse => 1,
            Self::Touch => 3,
        }
    }
}

/// A target-size adequacy report for one splitter handle and input modality
/// (bd-c2z7c). Used by the discoverability pass to flag handles that are hard to
/// grab, especially under touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneTargetReport {
    /// The modality the handle was judged against.
    pub modality: PaneInputModality,
    /// Minimum required thickness (minor axis), cells.
    pub required_thickness: u16,
    /// Minimum required span (major axis), cells.
    pub required_span: u16,
    /// Measured thickness — the handle's minor axis (`min(w, h)`).
    pub measured_thickness: u16,
    /// Measured span — the handle's major axis (`max(w, h)`).
    pub measured_span: u16,
    /// Whether the handle meets both minimums for this modality.
    pub passes: bool,
}

/// Judge whether a splitter handle is a comfortable target for `modality`.
#[must_use]
pub fn pane_handle_target_report(handle: Rect, modality: PaneInputModality) -> PaneTargetReport {
    let measured_thickness = handle.width.min(handle.height);
    let measured_span = handle.width.max(handle.height);
    let required_thickness = modality.min_target_thickness();
    let required_span = modality.min_target_span();
    PaneTargetReport {
        modality,
        required_thickness,
        required_span,
        measured_thickness,
        measured_span,
        passes: measured_thickness >= required_thickness && measured_span >= required_span,
    }
}

/// The recommended handle thickness for `modality`, honoring the large-target
/// accessibility preference (bd-21pbi.5). This is the remediation a host applies
/// when [`pane_handle_target_report`] fails: grow the rail/handle to this size.
#[must_use]
pub fn pane_recommended_handle_thickness(
    modality: PaneInputModality,
    prefs: ftui_layout::PaneAccessibilityPreferences,
) -> u16 {
    prefs.enlarge_target(modality.min_target_thickness())
}

/// The recommended handle span for `modality`, honoring the large-target
/// accessibility preference.
#[must_use]
pub fn pane_recommended_handle_span(
    modality: PaneInputModality,
    prefs: ftui_layout::PaneAccessibilityPreferences,
) -> u16 {
    prefs.enlarge_target(modality.min_target_span())
}

/// One discoverable cue for a pointer-driven pane action (bd-c2z7c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneActionHint {
    /// The visible UI cue a first-time user can act on.
    pub cue: &'static str,
    /// What the action does.
    pub action: &'static str,
    /// The input gesture that triggers it.
    pub gesture: &'static str,
}

/// First-run discoverability hints for pointer pane interactions, in display
/// order. Surfaced as an onboarding affordance so resize/split/maximize/mode
/// actions are discoverable without prior instruction.
#[must_use]
pub fn pane_first_run_hints() -> &'static [PaneActionHint] {
    &[
        PaneActionHint {
            cue: "drag the splitter handle",
            action: "resize the panes",
            gesture: "drag",
        },
        PaneActionHint {
            cue: "double-click a splitter",
            action: "reset the split to 50/50",
            gesture: "double-click",
        },
        PaneActionHint {
            cue: "double-click a pane",
            action: "maximize / restore the pane",
            gesture: "double-click",
        },
        PaneActionHint {
            cue: "right-click a pane",
            action: "cycle layout intelligence modes",
            gesture: "right-click",
        },
        PaneActionHint {
            cue: "scroll over a splitter",
            action: "nudge the split ratio",
            gesture: "scroll",
        },
    ]
}

/// An empty-/single-pane onboarding hint, shown when there is nothing to resize
/// yet so the user discovers how to create panes. Returns `None` once the
/// workspace has more than one pane.
#[must_use]
pub fn pane_empty_state_hint(tree: &PaneTree) -> Option<PaneActionHint> {
    let leaves = tree
        .nodes()
        .filter(|node| matches!(node.kind, PaneNodeKind::Leaf(_)))
        .count();
    (leaves <= 1).then_some(PaneActionHint {
        cue: "press Alt+s / Alt+v (or the split button)",
        action: "split this pane to create a workspace",
        gesture: "key / click",
    })
}

#[must_use]
pub fn edge_fling_projection(
    pointer: PanePointerPosition,
    motion: PaneMotionVector,
    viewport: Rect,
) -> PanePointerPosition {
    if motion.speed < 34.0 {
        return pointer;
    }
    let boost_cells = ((motion.speed - 28.0) * 0.13).round().clamp(2.0, 26.0);
    let margin_x = (f64::from(viewport.width) * 0.14).clamp(2.0, 18.0);
    let margin_y = (f64::from(viewport.height) * 0.18).clamp(2.0, 14.0);
    let left = f64::from(viewport.x);
    let right = f64::from(viewport.x.saturating_add(viewport.width.saturating_sub(1)));
    let top = f64::from(viewport.y);
    let bottom = f64::from(viewport.y.saturating_add(viewport.height.saturating_sub(1)));
    let px = f64::from(pointer.x);
    let py = f64::from(pointer.y);

    let mut out_x = f64::from(pointer.x);
    let mut out_y = f64::from(pointer.y);
    if f64::from(motion.delta_x) < 0.0 && px <= left + margin_x {
        out_x -= boost_cells;
    } else if f64::from(motion.delta_x) > 0.0 && px >= right - margin_x {
        out_x += boost_cells;
    }
    if f64::from(motion.delta_y) < 0.0 && py <= top + margin_y {
        out_y -= boost_cells;
    } else if f64::from(motion.delta_y) > 0.0 && py >= bottom - margin_y {
        out_y += boost_cells;
    }
    PanePointerPosition::new(round_f64_to_i32(out_x), round_f64_to_i32(out_y))
}

#[must_use]
pub fn blend_preview_ghost_rect(
    previous: Option<Rect>,
    target: Rect,
    pressure: PanePressureSnapProfile,
    dock_strength_bps: u16,
) -> Rect {
    let Some(previous) = previous else {
        return target;
    };
    let blend = (u32::from(2_000_u16)
        + u32::from(pressure.strength_bps / 2)
        + u32::from(dock_strength_bps / 4))
    .clamp(2_000, 9_200) as u16;
    blend_rect(previous, target, blend)
}

#[must_use]
pub fn build_preview_state_from_candidates(
    source: PaneId,
    primary: PaneDockPreview,
    primary_ghost_rect: Rect,
    dock_strength_bps: u16,
    motion_speed_cps: u16,
    ranked: &[PaneDockPreview],
    selection_bounds: Option<Rect>,
) -> PanePreviewState {
    let mut alternatives = ranked
        .iter()
        .copied()
        .filter(|candidate| candidate.target != primary.target || candidate.zone != primary.zone);
    let alt_one = alternatives.next();
    let alt_two = alternatives.next();
    PanePreviewState {
        source: Some(source),
        target: Some(primary.target),
        zone: Some(primary.zone),
        ghost_rect: Some(primary_ghost_rect),
        dock_strength_bps,
        motion_speed_cps,
        alt_one_target: alt_one.map(|candidate| candidate.target),
        alt_one_zone: alt_one.map(|candidate| candidate.zone),
        alt_one_ghost_rect: alt_one.map(|candidate| candidate.ghost_rect),
        alt_one_strength_bps: alt_one
            .map(|candidate| score_to_strength_bps(candidate.score))
            .unwrap_or(0),
        alt_two_target: alt_two.map(|candidate| candidate.target),
        alt_two_zone: alt_two.map(|candidate| candidate.zone),
        alt_two_ghost_rect: alt_two.map(|candidate| candidate.ghost_rect),
        alt_two_strength_bps: alt_two
            .map(|candidate| score_to_strength_bps(candidate.score))
            .unwrap_or(0),
        selection_bounds,
    }
}

#[must_use]
pub fn default_pane_layout_tree() -> PaneTree {
    let mut tree = PaneTree::singleton("pane-1");
    let ratio = PaneSplitRatio::new(1, 1).expect("1:1 split ratio must be valid");
    let root_leaf = tree.root();
    let _ = tree
        .apply_operation(
            1,
            PaneOperation::SplitLeaf {
                target: root_leaf,
                axis: SplitAxis::Horizontal,
                ratio,
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("pane-2"),
            },
        )
        .expect("default layout root split should succeed");

    if let Some(left_leaf) = leaf_id_for_key(&tree, "pane-1") {
        let _ = tree
            .apply_operation(
                2,
                PaneOperation::SplitLeaf {
                    target: left_leaf,
                    axis: SplitAxis::Vertical,
                    ratio,
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new("pane-3"),
                },
            )
            .expect("default layout left split should succeed");
    }

    if let Some(right_leaf) = leaf_id_for_key(&tree, "pane-2") {
        let _ = tree
            .apply_operation(
                3,
                PaneOperation::SplitLeaf {
                    target: right_leaf,
                    axis: SplitAxis::Vertical,
                    ratio,
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new("pane-4"),
                },
            )
            .expect("default layout right split should succeed");
    }

    tree
}

#[must_use]
pub fn nearest_axis_split_for_node(
    layout_tree: &PaneTree,
    node: PaneId,
    axis: SplitAxis,
) -> Option<PaneId> {
    let mut cursor = Some(node);
    while let Some(node_id) = cursor {
        let parent_id = layout_tree.node(node_id)?.parent?;
        let parent = layout_tree.node(parent_id)?;
        if let PaneNodeKind::Split(split) = &parent.kind
            && split.axis == axis
        {
            return Some(parent_id);
        }
        cursor = Some(parent_id);
    }
    None
}

#[must_use]
pub fn nearest_split_for_node(layout_tree: &PaneTree, node: PaneId) -> Option<PaneId> {
    let mut cursor = Some(node);
    while let Some(node_id) = cursor {
        let parent_id = layout_tree.node(node_id)?.parent?;
        let parent = layout_tree.node(parent_id)?;
        if matches!(parent.kind, PaneNodeKind::Split(_)) {
            return Some(parent_id);
        }
        cursor = Some(parent_id);
    }
    None
}

#[must_use]
pub fn pointer_down_context_at(
    layout_tree: &PaneTree,
    viewport: Rect,
    pointer: PanePointerPosition,
    edge_grip_inset_cells: f64,
) -> Option<PaneAutoPointerDownContext> {
    let layout = layout_tree.solve_layout(viewport).ok()?;
    let px = pointer.x;
    let py = pointer.y;
    let leaf = layout_tree.nodes().find_map(|node| {
        if !matches!(node.kind, PaneNodeKind::Leaf(_)) {
            return None;
        }
        let rect = layout.rect(node.id)?;
        let rx = i32::from(rect.x);
        let ry = i32::from(rect.y);
        let rw = i32::from(rect.width);
        let rh = i32::from(rect.height);
        if px >= rx && py >= ry && px < rx.saturating_add(rw) && py < ry.saturating_add(rh) {
            Some(node.id)
        } else {
            None
        }
    })?;

    let grip = layout.classify_resize_grip(leaf, pointer, edge_grip_inset_cells);
    let mode = grip.map_or(PaneGestureMode::Move, PaneGestureMode::Resize);
    let axis = grip.map_or(SplitAxis::Horizontal, grip_primary_axis);
    let split_id = nearest_axis_split_for_node(layout_tree, leaf, axis)
        .or_else(|| nearest_split_for_node(layout_tree, leaf))
        .unwrap_or(layout_tree.root());

    Some(PaneAutoPointerDownContext {
        target: PaneResizeTarget { split_id, axis },
        leaf,
        mode,
    })
}

pub fn update_selection_for_pointer_down(
    selection: &mut PaneSelectionState,
    leaf: PaneId,
    shift: bool,
) {
    if shift {
        selection.shift_toggle(leaf);
        if selection.anchor.is_none() {
            selection.anchor = Some(leaf);
        }
        return;
    }

    if !(selection.selected.contains(&leaf) && selection.selected.len() > 1) {
        set_single_selection(selection, leaf);
    }
}

#[must_use]
pub fn allow_preview_switch(
    preview_state: PanePreviewState,
    next_target: PaneId,
    next_zone: PaneDockZone,
    next_strength_bps: u16,
    switch_advantage_bps: u16,
) -> bool {
    if preview_state.target.is_none() || preview_state.zone.is_none() {
        return true;
    }
    if preview_state.target == Some(next_target) && preview_state.zone == Some(next_zone) {
        return true;
    }
    preview_state.dock_strength_bps <= next_strength_bps.saturating_add(switch_advantage_bps)
}

#[must_use]
pub fn ratio_to_bps(ratio: PaneSplitRatio) -> u16 {
    let numerator = ratio.numerator() as f64;
    let denominator = ratio.denominator() as f64;
    let total = (numerator + denominator).max(1.0);
    ((numerator / total) * 10_000.0).round().clamp(1.0, 9_999.0) as u16
}

#[must_use]
pub fn blend_bps(current: u16, target: u16, blend_factor_bps: u16) -> u16 {
    let blend = u32::from(blend_factor_bps.clamp(1, 10_000));
    let current = i32::from(current);
    let target = i32::from(target);
    let delta = target.saturating_sub(current);
    let blended = current + ((delta * i32::try_from(blend).unwrap_or(10_000)) / 10_000);
    blended.clamp(1, 9_999) as u16
}

#[must_use]
pub fn pane_operations_signature(operations: &[PaneOperation]) -> u64 {
    let mut hash = FNV64_OFFSET_BASIS;
    for operation in operations {
        let debug = format!("{operation:?}");
        hash = fnv1a64_extend(hash, debug.as_bytes());
        hash = fnv1a64_extend(hash, b"|");
    }
    hash
}

pub fn arm_active_gesture(
    state: PaneGestureArmState<'_>,
    timeline_cursor: usize,
    coalesce_after_operation_id: u64,
    pointer_id: u32,
    leaf: PaneId,
    mode: PaneGestureMode,
) {
    let PaneGestureArmState {
        active_gesture,
        gesture_timeline_cursor_start,
        live_reflow_signature,
        preview_state,
    } = state;
    *active_gesture = Some(ActivePaneGesture {
        pointer_id,
        leaf,
        mode,
        coalesce_after_operation_id,
    });
    *gesture_timeline_cursor_start = Some(timeline_cursor);
    *live_reflow_signature = None;
    *preview_state = PanePreviewState::default();
}

#[must_use]
pub fn apply_operations_with_timeline(
    state: PaneTimelineApplyState<'_>,
    sequence: u64,
    operations: &[PaneOperation],
    pressure: PanePressureSnapProfile,
    spring_blend: bool,
) -> usize {
    apply_operations_with_timeline_recording(
        state,
        sequence,
        operations,
        pressure,
        spring_blend,
        None,
    )
}

#[must_use]
pub fn apply_resize_operations_with_timeline(
    state: PaneTimelineApplyState<'_>,
    sequence: u64,
    operations: &[PaneOperation],
    coalesce_after_operation_id: u64,
    pressure: PanePressureSnapProfile,
    spring_blend: bool,
) -> usize {
    apply_operations_with_timeline_recording(
        state,
        sequence,
        operations,
        pressure,
        spring_blend,
        Some(coalesce_after_operation_id),
    )
}

#[must_use]
fn apply_operations_with_timeline_recording(
    state: PaneTimelineApplyState<'_>,
    sequence: u64,
    operations: &[PaneOperation],
    pressure: PanePressureSnapProfile,
    spring_blend: bool,
    coalesce_resize_deltas_after_operation_id: Option<u64>,
) -> usize {
    let PaneTimelineApplyState {
        layout_tree,
        timeline,
        next_operation_id,
        workspace_generation,
    } = state;
    let mut applied = 0usize;
    for operation in operations {
        let operation = spring_adjust_split_ratio_operation(
            layout_tree,
            operation.clone(),
            pressure,
            spring_blend,
        );
        let operation_id = *next_operation_id;
        *next_operation_id = next_operation_id.saturating_add(1);
        let result =
            if let Some(coalesce_after_operation_id) = coalesce_resize_deltas_after_operation_id {
                timeline.apply_and_record_coalesced_resize_delta(
                    layout_tree,
                    sequence,
                    operation_id,
                    operation,
                    coalesce_after_operation_id,
                )
            } else {
                timeline.apply_and_record(layout_tree, sequence, operation_id, operation)
            };
        if result.is_ok() {
            applied = applied.saturating_add(1);
        }
    }
    if applied > 0 {
        *workspace_generation = workspace_generation.saturating_add(1);
    }
    applied
}

#[must_use]
pub fn apply_live_reflow_if_needed(
    state: PaneLiveReflowState<'_>,
    sequence: u64,
    operations: &[PaneOperation],
    pressure: PanePressureSnapProfile,
) -> usize {
    let PaneLiveReflowState {
        layout_tree,
        timeline,
        next_operation_id,
        workspace_generation,
        live_reflow_signature,
    } = state;
    if operations.is_empty() {
        return 0;
    }
    let signature = pane_operations_signature(operations);
    if *live_reflow_signature == Some(signature) {
        return 0;
    }
    let applied = apply_operations_with_timeline(
        PaneTimelineApplyState {
            layout_tree,
            timeline,
            next_operation_id,
            workspace_generation,
        },
        sequence,
        operations,
        pressure,
        false,
    );
    if applied > 0 {
        *live_reflow_signature = Some(signature);
    }
    applied
}

pub fn rollback_timeline_to_cursor(
    layout_tree: &mut PaneTree,
    timeline: &mut PaneInteractionTimeline,
    start_cursor: Option<usize>,
    workspace_generation: &mut u64,
) -> Result<bool, PaneInteractionTimelineError> {
    let Some(start_cursor) = start_cursor else {
        return Ok(false);
    };
    let mut rolled_back = false;
    while timeline.cursor > start_cursor {
        match timeline.undo(layout_tree)? {
            true => rolled_back = true,
            false => break,
        }
    }
    if rolled_back {
        *workspace_generation = workspace_generation.saturating_add(1);
    }
    Ok(rolled_back)
}

#[must_use]
pub fn apply_drag_semantics(
    context: PaneDragSemanticsContext<'_>,
    input: PaneDragSemanticsInput,
) -> usize {
    let PaneDragSemanticsContext {
        layout_tree,
        timeline,
        next_operation_id,
        workspace_generation,
        selection,
        preview_state,
        live_reflow_signature,
        viewport,
        baseline_magnetic_field_cells,
    } = context;
    let PaneDragSemanticsInput {
        sequence,
        active,
        pointer,
        pressure,
        motion,
        projected_position,
        inertial_throw,
        committed,
    } = input;
    let Ok(layout) = layout_tree.solve_layout(viewport) else {
        *preview_state = PanePreviewState::default();
        *live_reflow_signature = None;
        return 0;
    };
    match active.mode {
        PaneGestureMode::Resize(grip) => {
            *live_reflow_signature = None;
            *preview_state = PanePreviewState::default();
            if selection.selected.len() > 1 && selection.selected.contains(&active.leaf) {
                let Ok(plan) =
                    layout_tree.plan_group_resize(selection, &layout, grip, pointer, pressure)
                else {
                    return 0;
                };
                apply_resize_operations_with_timeline(
                    PaneTimelineApplyState {
                        layout_tree,
                        timeline,
                        next_operation_id,
                        workspace_generation,
                    },
                    sequence,
                    &plan.operations,
                    active.coalesce_after_operation_id,
                    pressure,
                    !committed,
                )
            } else {
                let Ok(plan) =
                    layout_tree.plan_edge_resize(active.leaf, &layout, grip, pointer, pressure)
                else {
                    return 0;
                };
                apply_resize_operations_with_timeline(
                    PaneTimelineApplyState {
                        layout_tree,
                        timeline,
                        next_operation_id,
                        workspace_generation,
                    },
                    sequence,
                    &plan.operations,
                    active.coalesce_after_operation_id,
                    pressure,
                    !committed,
                )
            }
        }
        PaneGestureMode::Move => {
            let move_policy = pane_move_policy(PaneMovePolicyInput {
                pointer,
                projected_position,
                motion,
                pressure,
                release_inertial_throw: inertial_throw,
                committed,
                viewport,
                baseline_magnetic_field_cells,
            });
            let projected_pointer = move_policy.projected_pointer;
            let inertial = move_policy.inertial_throw;
            let magnetic_field = move_policy.magnetic_field_cells;
            let switch_advantage_bps = move_policy.switch_advantage_bps;
            let live_reflow_threshold_bps = move_policy.live_reflow_threshold_bps;
            let motion_speed_cps = motion.speed.round().clamp(0.0, 65_535.0) as u16;

            if selection.selected.len() > 1 && selection.selected.contains(&active.leaf) {
                let anchor = selection.anchor.unwrap_or(active.leaf);
                let selection_bounds = layout.cluster_bounds(&selection.selected);
                let Ok(preview_plan) = layout_tree.plan_reflow_move_with_preview(
                    anchor,
                    &layout,
                    pointer,
                    motion,
                    inertial,
                    magnetic_field,
                ) else {
                    *preview_state = PanePreviewState::default();
                    *live_reflow_signature = None;
                    return 0;
                };
                let dock_strength_bps = adaptive_dock_strength_bps(
                    preview_plan.preview.score,
                    motion,
                    pressure,
                    committed,
                );
                let ghost_rect = blend_preview_ghost_rect(
                    preview_state.ghost_rect,
                    preview_plan.preview.ghost_rect,
                    pressure,
                    dock_strength_bps,
                );
                let mut ranked = layout_tree.ranked_dock_previews_with_motion(
                    &layout,
                    pointer,
                    motion,
                    magnetic_field,
                    Some(anchor),
                    DOCK_PREVIEW_CANDIDATE_LIMIT,
                );
                if ranked.is_empty() {
                    ranked.push(preview_plan.preview);
                }
                let allow_switch = allow_preview_switch(
                    *preview_state,
                    preview_plan.preview.target,
                    preview_plan.preview.zone,
                    dock_strength_bps,
                    switch_advantage_bps,
                );
                if allow_switch {
                    *preview_state = build_preview_state_from_candidates(
                        anchor,
                        preview_plan.preview,
                        ghost_rect,
                        dock_strength_bps,
                        motion_speed_cps,
                        &ranked,
                        selection_bounds,
                    );
                }
                let should_live_apply =
                    !committed && allow_switch && dock_strength_bps >= live_reflow_threshold_bps;
                if committed || should_live_apply {
                    let Ok(plan) = layout_tree.plan_group_move(
                        selection,
                        &layout,
                        if committed {
                            projected_pointer
                        } else {
                            pointer
                        },
                        motion,
                        inertial,
                        magnetic_field,
                    ) else {
                        *preview_state = PanePreviewState::default();
                        *live_reflow_signature = None;
                        return 0;
                    };
                    apply_live_reflow_if_needed(
                        PaneLiveReflowState {
                            layout_tree,
                            timeline,
                            next_operation_id,
                            workspace_generation,
                            live_reflow_signature,
                        },
                        sequence,
                        &plan.operations,
                        pressure,
                    )
                } else {
                    0
                }
            } else {
                let selection_bounds = layout
                    .visual_rect(active.leaf)
                    .or_else(|| layout.rect(active.leaf));
                let Ok(plan) = layout_tree.plan_reflow_move_with_preview(
                    active.leaf,
                    &layout,
                    pointer,
                    motion,
                    inertial,
                    magnetic_field,
                ) else {
                    *preview_state = PanePreviewState::default();
                    *live_reflow_signature = None;
                    return 0;
                };
                let dock_strength_bps =
                    adaptive_dock_strength_bps(plan.preview.score, motion, pressure, committed);
                let ghost_rect = blend_preview_ghost_rect(
                    preview_state.ghost_rect,
                    plan.preview.ghost_rect,
                    pressure,
                    dock_strength_bps,
                );
                let mut ranked = layout_tree.ranked_dock_previews_with_motion(
                    &layout,
                    pointer,
                    motion,
                    magnetic_field,
                    Some(active.leaf),
                    DOCK_PREVIEW_CANDIDATE_LIMIT,
                );
                if ranked.is_empty() {
                    ranked.push(plan.preview);
                }
                let allow_switch = allow_preview_switch(
                    *preview_state,
                    plan.preview.target,
                    plan.preview.zone,
                    dock_strength_bps,
                    switch_advantage_bps,
                );
                if allow_switch {
                    *preview_state = build_preview_state_from_candidates(
                        active.leaf,
                        plan.preview,
                        ghost_rect,
                        dock_strength_bps,
                        motion_speed_cps,
                        &ranked,
                        selection_bounds,
                    );
                }
                let should_live_apply =
                    !committed && allow_switch && dock_strength_bps >= live_reflow_threshold_bps;
                if committed || should_live_apply {
                    apply_live_reflow_if_needed(
                        PaneLiveReflowState {
                            layout_tree,
                            timeline,
                            next_operation_id,
                            workspace_generation,
                            live_reflow_signature,
                        },
                        sequence,
                        &plan.operations,
                        pressure,
                    )
                } else {
                    0
                }
            }
        }
    }
}

#[must_use]
pub fn spring_adjust_split_ratio_operation(
    layout_tree: &PaneTree,
    operation: PaneOperation,
    pressure: PanePressureSnapProfile,
    spring_blend: bool,
) -> PaneOperation {
    if !spring_blend {
        return operation;
    }
    let PaneOperation::SetSplitRatio { split, ratio } = operation else {
        return operation;
    };
    let Some(node) = layout_tree.node(split) else {
        return PaneOperation::SetSplitRatio { split, ratio };
    };
    let PaneNodeKind::Split(split_node) = &node.kind else {
        return PaneOperation::SetSplitRatio { split, ratio };
    };
    let current = ratio_to_bps(split_node.ratio);
    let target = ratio_to_bps(ratio);
    let spring_bps = (u32::from(DEFAULT_SPRING_BLEND_BPS) + (u32::from(pressure.strength_bps) / 3))
        .clamp(1_500, 9_000) as u16;
    let blended = blend_bps(current, target, spring_bps);
    let denominator = 10_000_u32.saturating_sub(u32::from(blended)).max(1);
    let ratio = PaneSplitRatio::new(u32::from(blended.max(1)), denominator).unwrap_or(ratio);
    PaneOperation::SetSplitRatio { split, ratio }
}

#[must_use]
fn score_to_strength_bps(score: f64) -> u16 {
    (score * 10_000.0).round().clamp(0.0, 10_000.0) as u16
}

#[must_use]
fn grip_primary_axis(grip: PaneResizeGrip) -> SplitAxis {
    match grip {
        PaneResizeGrip::Left
        | PaneResizeGrip::Right
        | PaneResizeGrip::TopLeft
        | PaneResizeGrip::TopRight
        | PaneResizeGrip::BottomLeft
        | PaneResizeGrip::BottomRight => SplitAxis::Horizontal,
        PaneResizeGrip::Top | PaneResizeGrip::Bottom => SplitAxis::Vertical,
    }
}

#[must_use]
fn pointer_hits_rect(pointer: PanePointerPosition, rect: Rect) -> bool {
    let Ok(x) = u16::try_from(pointer.x) else {
        return false;
    };
    let Ok(y) = u16::try_from(pointer.y) else {
        return false;
    };
    rect.contains(x, y)
}

#[must_use]
fn active_resize_split_id(
    tree: &PaneTree,
    active_gesture: Option<ActivePaneGesture>,
) -> Option<PaneId> {
    let active = active_gesture?;
    let PaneGestureMode::Resize(grip) = active.mode else {
        return None;
    };
    let axis = grip_primary_axis(grip);
    let mut cursor = active.leaf;
    loop {
        let node = tree.node(cursor)?;
        let parent = node.parent?;
        let parent_node = tree.node(parent)?;
        if let PaneNodeKind::Split(split) = &parent_node.kind
            && split.axis == axis
        {
            return Some(parent);
        }
        cursor = parent;
    }
}

fn set_single_selection(selection: &mut PaneSelectionState, pane_id: PaneId) {
    selection.selected.clear();
    let _ = selection.selected.insert(pane_id);
    selection.anchor = Some(pane_id);
}

#[must_use]
fn leaf_id_for_key(tree: &PaneTree, key: &str) -> Option<PaneId> {
    tree.nodes().find_map(|node| match &node.kind {
        PaneNodeKind::Leaf(leaf) if leaf.surface_key == key => Some(node.id),
        _ => None,
    })
}

#[must_use]
fn blend_u16_value(current: u16, target: u16, blend_factor_bps: u16) -> u16 {
    let blend = u32::from(blend_factor_bps.clamp(1, 10_000));
    let current = i32::from(current);
    let target = i32::from(target);
    let delta = target.saturating_sub(current);
    let blended = current + ((delta * i32::try_from(blend).unwrap_or(10_000)) / 10_000);
    blended.clamp(0, i32::from(u16::MAX)) as u16
}

#[must_use]
fn round_f64_to_i32(value: f64) -> i32 {
    if value.is_nan() {
        return 0;
    }
    if value >= f64::from(i32::MAX) {
        return i32::MAX;
    }
    if value <= f64::from(i32::MIN) {
        return i32::MIN;
    }
    value.round() as i32
}

#[must_use]
fn blend_rect(current: Rect, target: Rect, blend_factor_bps: u16) -> Rect {
    Rect::new(
        blend_u16_value(current.x, target.x, blend_factor_bps),
        blend_u16_value(current.y, target.y, blend_factor_bps),
        blend_u16_value(current.width.max(1), target.width.max(1), blend_factor_bps).max(1),
        blend_u16_value(
            current.height.max(1),
            target.height.max(1),
            blend_factor_bps,
        )
        .max(1),
    )
}

#[must_use]
fn fnv1a64_extend(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV64_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_dock_strength_rewards_fast_confident_commits() {
        let fast = adaptive_dock_strength_bps(
            0.72,
            PaneMotionVector::from_delta(42, 4, 34, 0),
            PanePressureSnapProfile {
                strength_bps: 8_400,
                hysteresis_bps: 520,
            },
            true,
        );
        let precise = adaptive_dock_strength_bps(
            0.72,
            PaneMotionVector::from_delta(6, 1, 240, 7),
            PanePressureSnapProfile {
                strength_bps: 2_800,
                hysteresis_bps: 180,
            },
            false,
        );
        assert!(fast > precise);
    }

    #[test]
    fn preview_state_includes_secondary_dock_candidates() {
        let primary = PaneDockPreview {
            target: PaneId::new(2).expect("valid pane id"),
            zone: PaneDockZone::Right,
            score: 0.88,
            ghost_rect: Rect::new(20, 4, 30, 12),
        };
        let secondary = PaneDockPreview {
            target: PaneId::new(3).expect("valid pane id"),
            zone: PaneDockZone::Bottom,
            score: 0.61,
            ghost_rect: Rect::new(20, 16, 30, 8),
        };
        let tertiary = PaneDockPreview {
            target: PaneId::new(4).expect("valid pane id"),
            zone: PaneDockZone::Center,
            score: 0.54,
            ghost_rect: Rect::new(10, 4, 40, 20),
        };
        let state = build_preview_state_from_candidates(
            PaneId::new(1).expect("valid pane id"),
            primary,
            primary.ghost_rect,
            8_800,
            420,
            &[primary, secondary, tertiary],
            Some(Rect::new(8, 3, 40, 22)),
        );
        assert_eq!(state.alt_one_target, Some(secondary.target));
        assert_eq!(state.alt_two_target, Some(tertiary.target));
        assert!(state.alt_one_strength_bps > state.alt_two_strength_bps);
        assert_eq!(state.selection_bounds, Some(Rect::new(8, 3, 40, 22)));
    }

    #[test]
    fn edge_fling_projection_pushes_fast_edge_release_outward() {
        let viewport = Rect::new(0, 0, 120, 40);
        let projected = edge_fling_projection(
            PanePointerPosition::new(2, 20),
            PaneMotionVector::from_delta(-36, 0, 42, 0),
            viewport,
        );
        assert!(projected.x < 2);
    }

    #[test]
    fn edge_fling_projection_keeps_slow_motion_unchanged() {
        let viewport = Rect::new(0, 0, 120, 40);
        let pointer = PanePointerPosition::new(2, 20);
        let projected = edge_fling_projection(
            pointer,
            PaneMotionVector::from_delta(-3, 0, 420, 0),
            viewport,
        );
        assert_eq!(projected, pointer);
    }

    #[test]
    fn default_tree_has_four_leaves_with_expected_surface_keys() {
        let tree = default_pane_layout_tree();
        let mut keys = tree
            .nodes()
            .filter_map(|node| match &node.kind {
                PaneNodeKind::Leaf(leaf) => Some(leaf.surface_key.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(keys, vec!["pane-1", "pane-2", "pane-3", "pane-4"]);
    }

    #[test]
    fn ratio_to_bps_returns_even_split_for_one_to_one() {
        let ratio = PaneSplitRatio::new(1, 1).expect("valid ratio");
        assert_eq!(ratio_to_bps(ratio), 5_000);
    }

    #[test]
    fn pane_operation_signature_is_stable_for_same_sequence() {
        let split = PaneId::new(7).expect("valid pane id");
        let ratio = PaneSplitRatio::new(3, 2).expect("valid ratio");
        let ops = vec![PaneOperation::SetSplitRatio { split, ratio }];
        assert_eq!(
            pane_operations_signature(&ops),
            pane_operations_signature(&ops)
        );
    }

    #[test]
    fn spring_adjust_split_ratio_moves_toward_target() {
        let tree = default_pane_layout_tree();
        let split = tree
            .nodes()
            .find_map(|node| match &node.kind {
                PaneNodeKind::Split(_) => Some(node.id),
                _ => None,
            })
            .expect("expected split node");
        let target_ratio = PaneSplitRatio::new(8, 2).expect("valid target ratio");
        let operation = PaneOperation::SetSplitRatio {
            split,
            ratio: target_ratio,
        };
        let adjusted = spring_adjust_split_ratio_operation(
            &tree,
            operation,
            PanePressureSnapProfile {
                strength_bps: 7_800,
                hysteresis_bps: 420,
            },
            true,
        );
        let PaneOperation::SetSplitRatio { ratio, .. } = adjusted else {
            let expected_split_ratio_operation = false;
            assert!(
                expected_split_ratio_operation,
                "expected split ratio operation"
            );
            return;
        };
        assert!(ratio_to_bps(ratio) >= 5_000);
        // Ensure spring blend didn't jump all the way to target in one step.
        assert!(ratio_to_bps(ratio) < ratio_to_bps(target_ratio));
    }

    #[test]
    fn update_selection_preserves_cluster_when_clicking_selected_member_without_shift() {
        let mut selection = PaneSelectionState::default();
        let a = PaneId::new(1).expect("valid pane id");
        let b = PaneId::new(2).expect("valid pane id");
        let _ = selection.selected.insert(a);
        let _ = selection.selected.insert(b);
        selection.anchor = Some(a);

        update_selection_for_pointer_down(&mut selection, b, false);

        assert!(selection.selected.contains(&a));
        assert!(selection.selected.contains(&b));
        assert_eq!(selection.selected.len(), 2);
        assert_eq!(selection.anchor, Some(a));
    }

    #[test]
    fn allow_preview_switch_requires_strength_advantage_for_target_change() {
        let preview_state = PanePreviewState {
            source: Some(PaneId::new(1).expect("valid pane id")),
            target: Some(PaneId::new(2).expect("valid pane id")),
            zone: Some(PaneDockZone::Left),
            ghost_rect: Some(Rect::new(0, 0, 10, 5)),
            dock_strength_bps: 5_000,
            motion_speed_cps: 120,
            alt_one_target: None,
            alt_one_zone: None,
            alt_one_ghost_rect: None,
            alt_one_strength_bps: 0,
            alt_two_target: None,
            alt_two_zone: None,
            alt_two_ghost_rect: None,
            alt_two_strength_bps: 0,
            selection_bounds: None,
        };
        let next_target = PaneId::new(3).expect("valid pane id");

        assert!(!allow_preview_switch(
            preview_state,
            next_target,
            PaneDockZone::Right,
            3_200,
            300
        ));
        assert!(allow_preview_switch(
            preview_state,
            next_target,
            PaneDockZone::Right,
            4_800,
            300
        ));
    }

    #[test]
    fn pointer_down_context_detects_edge_resize_mode_and_axis_split() {
        let tree = default_pane_layout_tree();
        let viewport = Rect::new(0, 0, 120, 40);
        let layout = tree
            .solve_layout(viewport)
            .expect("default layout should solve in viewport");
        let pane_one = leaf_id_for_key(&tree, "pane-1").expect("pane-1 leaf should exist");
        let rect = layout.rect(pane_one).expect("pane-1 rect should exist");
        let pointer = PanePointerPosition::new(
            i32::from(rect.x.saturating_add(rect.width.saturating_sub(1))),
            i32::from(rect.y.saturating_add(rect.height / 2)),
        );

        let context =
            pointer_down_context_at(&tree, viewport, pointer, 1.2).expect("context should exist");
        assert_eq!(context.leaf, pane_one);
        assert_eq!(context.mode, PaneGestureMode::Resize(PaneResizeGrip::Right));
        let split = tree
            .node(context.target.split_id)
            .expect("target split id should resolve");
        let PaneNodeKind::Split(split) = &split.kind else {
            let expected_split_target = false;
            assert!(expected_split_target, "expected split target");
            return;
        };
        assert_eq!(split.axis, SplitAxis::Horizontal);
    }

    #[test]
    fn pane_move_policy_projects_and_keeps_release_inertia_on_commit() {
        let viewport = Rect::new(0, 0, 120, 40);
        let pointer = PanePointerPosition::new(2, 20);
        let motion = PaneMotionVector::from_delta(-40, 0, 32, 0);
        let release_inertial = Some(PaneInertialThrow::from_motion(
            PaneMotionVector::from_delta(-12, 0, 18, 0),
        ));

        let policy = pane_move_policy(PaneMovePolicyInput {
            pointer,
            projected_position: Some(pointer),
            motion,
            pressure: PanePressureSnapProfile {
                strength_bps: 8_200,
                hysteresis_bps: 450,
            },
            release_inertial_throw: release_inertial,
            committed: true,
            viewport,
            baseline_magnetic_field_cells: 6.0,
        });
        assert!(policy.projected_pointer.x < pointer.x);
        assert_eq!(policy.inertial_throw, release_inertial);
    }

    #[test]
    fn pane_move_policy_arms_predicted_inertia_for_fast_live_drag() {
        let policy = pane_move_policy(PaneMovePolicyInput {
            pointer: PanePointerPosition::new(30, 10),
            projected_position: None,
            motion: PaneMotionVector::from_delta(26, 4, 24, 0),
            pressure: PanePressureSnapProfile {
                strength_bps: 6_700,
                hysteresis_bps: 360,
            },
            release_inertial_throw: None,
            committed: false,
            viewport: Rect::new(0, 0, 120, 40),
            baseline_magnetic_field_cells: 6.0,
        });
        assert!(policy.inertial_throw.is_some());
        assert_eq!(policy.projected_pointer, PanePointerPosition::new(30, 10));
    }

    #[test]
    fn pane_move_policy_keeps_slow_live_drag_non_inertial() {
        let policy = pane_move_policy(PaneMovePolicyInput {
            pointer: PanePointerPosition::new(30, 10),
            projected_position: None,
            motion: PaneMotionVector::from_delta(3, 0, 240, 0),
            pressure: PanePressureSnapProfile {
                strength_bps: 2_200,
                hysteresis_bps: 120,
            },
            release_inertial_throw: None,
            committed: false,
            viewport: Rect::new(0, 0, 120, 40),
            baseline_magnetic_field_cells: 6.0,
        });
        assert!(policy.inertial_throw.is_none());
    }

    #[test]
    fn apply_operations_with_timeline_updates_generation_and_timeline() {
        let mut tree = default_pane_layout_tree();
        let mut timeline = PaneInteractionTimeline::with_baseline(&tree);
        let mut next_operation_id = 1_u64;
        let mut generation = 0_u64;
        let split = tree
            .nodes()
            .find_map(|node| match node.kind {
                PaneNodeKind::Split(_) => Some(node.id),
                _ => None,
            })
            .expect("default tree should include a split");
        let target_ratio = PaneSplitRatio::new(3, 2).expect("valid ratio");
        let operations = vec![PaneOperation::SetSplitRatio {
            split,
            ratio: target_ratio,
        }];

        let applied = apply_operations_with_timeline(
            PaneTimelineApplyState {
                layout_tree: &mut tree,
                timeline: &mut timeline,
                next_operation_id: &mut next_operation_id,
                workspace_generation: &mut generation,
            },
            7,
            &operations,
            PanePressureSnapProfile {
                strength_bps: 8_000,
                hysteresis_bps: 350,
            },
            true,
        );

        assert_eq!(applied, 1);
        assert_eq!(timeline.cursor, 1);
        assert_eq!(timeline.entries.len(), 1);
        assert_eq!(next_operation_id, 2);
        assert_eq!(generation, 1);
    }

    #[test]
    fn apply_resize_operations_with_timeline_coalesces_same_split_deltas() {
        let mut tree = default_pane_layout_tree();
        let mut timeline = PaneInteractionTimeline::with_baseline(&tree);
        let mut next_operation_id = 1_u64;
        let mut generation = 0_u64;
        let split = tree
            .nodes()
            .find_map(|node| match node.kind {
                PaneNodeKind::Split(_) => Some(node.id),
                _ => None,
            })
            .expect("default tree should include a split");
        let first = [PaneOperation::SetSplitRatio {
            split,
            ratio: PaneSplitRatio::new(6, 4).expect("valid ratio"),
        }];
        let second = [PaneOperation::SetSplitRatio {
            split,
            ratio: PaneSplitRatio::new(7, 3).expect("valid ratio"),
        }];
        let coalesce_after_operation_id = next_operation_id.saturating_sub(1);

        let first_applied = apply_resize_operations_with_timeline(
            PaneTimelineApplyState {
                layout_tree: &mut tree,
                timeline: &mut timeline,
                next_operation_id: &mut next_operation_id,
                workspace_generation: &mut generation,
            },
            1,
            &first,
            coalesce_after_operation_id,
            PanePressureSnapProfile {
                strength_bps: 8_000,
                hysteresis_bps: 350,
            },
            false,
        );
        let second_applied = apply_resize_operations_with_timeline(
            PaneTimelineApplyState {
                layout_tree: &mut tree,
                timeline: &mut timeline,
                next_operation_id: &mut next_operation_id,
                workspace_generation: &mut generation,
            },
            2,
            &second,
            coalesce_after_operation_id,
            PanePressureSnapProfile {
                strength_bps: 8_000,
                hysteresis_bps: 350,
            },
            false,
        );

        assert_eq!(first_applied, 1);
        assert_eq!(second_applied, 1);
        assert_eq!(timeline.cursor, 1);
        assert_eq!(timeline.entries.len(), 1);
        assert_eq!(timeline.entries[0].operation_id, 2);
        assert_eq!(next_operation_id, 3);
        assert_eq!(generation, 2);
    }

    #[test]
    fn apply_live_reflow_if_needed_deduplicates_same_signature() {
        let mut tree = default_pane_layout_tree();
        let mut timeline = PaneInteractionTimeline::with_baseline(&tree);
        let mut next_operation_id = 1_u64;
        let mut generation = 0_u64;
        let mut signature = None;
        let split = tree
            .nodes()
            .find_map(|node| match node.kind {
                PaneNodeKind::Split(_) => Some(node.id),
                _ => None,
            })
            .expect("default tree should include a split");
        let operations = vec![PaneOperation::SetSplitRatio {
            split,
            ratio: PaneSplitRatio::new(6, 4).expect("valid ratio"),
        }];

        let first = apply_live_reflow_if_needed(
            PaneLiveReflowState {
                layout_tree: &mut tree,
                timeline: &mut timeline,
                next_operation_id: &mut next_operation_id,
                workspace_generation: &mut generation,
                live_reflow_signature: &mut signature,
            },
            1,
            &operations,
            PanePressureSnapProfile {
                strength_bps: 6_000,
                hysteresis_bps: 320,
            },
        );
        let second = apply_live_reflow_if_needed(
            PaneLiveReflowState {
                layout_tree: &mut tree,
                timeline: &mut timeline,
                next_operation_id: &mut next_operation_id,
                workspace_generation: &mut generation,
                live_reflow_signature: &mut signature,
            },
            2,
            &operations,
            PanePressureSnapProfile {
                strength_bps: 6_000,
                hysteresis_bps: 320,
            },
        );

        assert_eq!(first, 1);
        assert_eq!(second, 0);
        assert_eq!(timeline.cursor, 1);
    }

    #[test]
    fn rollback_timeline_to_cursor_undoes_applied_mutations() {
        let mut tree = default_pane_layout_tree();
        let split = tree
            .nodes()
            .find_map(|node| match &node.kind {
                PaneNodeKind::Split(_) => Some(node.id),
                _ => None,
            })
            .expect("default tree should include split");
        let mut timeline = PaneInteractionTimeline::with_baseline(&tree);
        let mut next_operation_id = 1_u64;
        let mut generation = 0_u64;
        let ops_a = vec![PaneOperation::SetSplitRatio {
            split,
            ratio: PaneSplitRatio::new(7, 3).expect("valid ratio"),
        }];
        let ops_b = vec![PaneOperation::SetSplitRatio {
            split,
            ratio: PaneSplitRatio::new(8, 2).expect("valid ratio"),
        }];
        let _ = apply_operations_with_timeline(
            PaneTimelineApplyState {
                layout_tree: &mut tree,
                timeline: &mut timeline,
                next_operation_id: &mut next_operation_id,
                workspace_generation: &mut generation,
            },
            1,
            &ops_a,
            PanePressureSnapProfile {
                strength_bps: 7_500,
                hysteresis_bps: 330,
            },
            false,
        );
        let _ = apply_operations_with_timeline(
            PaneTimelineApplyState {
                layout_tree: &mut tree,
                timeline: &mut timeline,
                next_operation_id: &mut next_operation_id,
                workspace_generation: &mut generation,
            },
            2,
            &ops_b,
            PanePressureSnapProfile {
                strength_bps: 7_500,
                hysteresis_bps: 330,
            },
            false,
        );
        assert_eq!(timeline.cursor, 2);
        let ratio_before = tree
            .node(split)
            .and_then(|node| match &node.kind {
                PaneNodeKind::Split(split) => Some(split.ratio),
                _ => None,
            })
            .expect("split ratio should be readable");

        let rolled_back =
            rollback_timeline_to_cursor(&mut tree, &mut timeline, Some(1), &mut generation)
                .expect("rollback should succeed");
        let ratio_after = tree
            .node(split)
            .and_then(|node| match &node.kind {
                PaneNodeKind::Split(split) => Some(split.ratio),
                _ => None,
            })
            .expect("split ratio should be readable");

        assert!(rolled_back);
        assert_eq!(timeline.cursor, 1);
        assert_ne!(ratio_before, ratio_after);
    }

    #[test]
    fn apply_drag_semantics_resets_transient_state_when_plan_fails() {
        let mut tree = default_pane_layout_tree();
        let mut timeline = PaneInteractionTimeline::with_baseline(&tree);
        let mut next_operation_id = 1_u64;
        let mut generation = 0_u64;
        let selection = PaneSelectionState::default();
        let mut preview_state = PanePreviewState {
            source: Some(PaneId::new(1).expect("valid pane id")),
            target: Some(PaneId::new(2).expect("valid pane id")),
            zone: Some(PaneDockZone::Right),
            ghost_rect: Some(Rect::new(2, 2, 10, 6)),
            dock_strength_bps: 8_400,
            motion_speed_cps: 220,
            alt_one_target: None,
            alt_one_zone: None,
            alt_one_ghost_rect: None,
            alt_one_strength_bps: 0,
            alt_two_target: None,
            alt_two_zone: None,
            alt_two_ghost_rect: None,
            alt_two_strength_bps: 0,
            selection_bounds: None,
        };
        let mut live_signature = Some(0xfeed_beef_u64);

        let applied = apply_drag_semantics(
            PaneDragSemanticsContext {
                layout_tree: &mut tree,
                timeline: &mut timeline,
                next_operation_id: &mut next_operation_id,
                workspace_generation: &mut generation,
                selection: &selection,
                preview_state: &mut preview_state,
                live_reflow_signature: &mut live_signature,
                viewport: Rect::new(0, 0, 120, 40),
                baseline_magnetic_field_cells: 6.0,
            },
            PaneDragSemanticsInput {
                sequence: 1,
                active: ActivePaneGesture {
                    pointer_id: 7,
                    leaf: PaneId::new(999).expect("nonexistent pane id should be constructible"),
                    mode: PaneGestureMode::Move,
                    coalesce_after_operation_id: 0,
                },
                pointer: PanePointerPosition::new(30, 10),
                pressure: PanePressureSnapProfile {
                    strength_bps: 6_400,
                    hysteresis_bps: 320,
                },
                motion: PaneMotionVector::from_delta(24, 0, 32, 0),
                projected_position: None,
                inertial_throw: None,
                committed: false,
            },
        );

        assert_eq!(applied, 0);
        assert_eq!(preview_state, PanePreviewState::default());
        assert_eq!(live_signature, None);
        assert_eq!(timeline.cursor, 0);
        assert_eq!(generation, 0);
    }

    #[test]
    fn collect_splitter_primitives_defaults_to_idle_and_adaptive_thickness() {
        let tree = default_pane_layout_tree();
        let viewport = Rect::new(0, 0, 120, 40);
        let layout = tree
            .solve_layout(viewport)
            .expect("default layout should solve");

        let splitters = collect_splitter_primitives(&tree, &layout, viewport, None, None);
        assert!(
            !splitters.is_empty(),
            "expected at least one splitter primitive"
        );
        assert!(
            splitters
                .iter()
                .all(|splitter| splitter.state == PaneSplitterVisualState::Idle)
        );
        assert!(
            splitters.iter().any(|splitter| {
                (splitter.axis == SplitAxis::Horizontal && splitter.rail_rect.width == 2)
                    || (splitter.axis == SplitAxis::Vertical && splitter.rail_rect.height == 2)
            }),
            "large viewport should promote rail thickness to 2 cells"
        );
    }

    #[test]
    fn collect_splitter_primitives_marks_hover_and_active_states() {
        let tree = default_pane_layout_tree();
        let viewport = Rect::new(0, 0, 120, 40);
        let layout = tree
            .solve_layout(viewport)
            .expect("default layout should solve");
        let splitters = collect_splitter_primitives(&tree, &layout, viewport, None, None);
        let first = splitters.first().copied().expect("splitter primitive");

        let hover_pointer = PanePointerPosition::new(
            i32::from(first.rail_rect.x + first.rail_rect.width / 2),
            i32::from(first.rail_rect.y + first.rail_rect.height / 2),
        );

        let hovered =
            collect_splitter_primitives(&tree, &layout, viewport, Some(hover_pointer), None);
        let hovered_match = hovered
            .iter()
            .find(|splitter| splitter.split_id == first.split_id)
            .expect("hovered splitter");
        assert_eq!(hovered_match.state, PaneSplitterVisualState::Hover);

        let leaf = tree
            .nodes()
            .find_map(|node| {
                matches!(node.kind, PaneNodeKind::Leaf(_))
                    .then_some(node.id)
                    .filter(|leaf_id| {
                        nearest_axis_split_for_node(&tree, *leaf_id, first.axis)
                            == Some(first.split_id)
                    })
            })
            .expect("active leaf for split should resolve");
        let active_gesture = ActivePaneGesture {
            pointer_id: 9,
            leaf,
            mode: match first.axis {
                SplitAxis::Horizontal => PaneGestureMode::Resize(PaneResizeGrip::Right),
                SplitAxis::Vertical => PaneGestureMode::Resize(PaneResizeGrip::Bottom),
            },
            coalesce_after_operation_id: 0,
        };
        let active = collect_splitter_primitives(
            &tree,
            &layout,
            viewport,
            Some(hover_pointer),
            Some(active_gesture),
        );
        let active_match = active
            .iter()
            .find(|splitter| splitter.split_id == first.split_id)
            .expect("active splitter");
        assert_eq!(active_match.state, PaneSplitterVisualState::Active);
    }

    /// Resolve a usable `(PaneResizeTarget, split_rect)` for the first splitter.
    fn first_resize_target() -> (PaneTree, PaneLayout, PaneResizeTarget, Rect) {
        let tree = default_pane_layout_tree();
        let viewport = Rect::new(0, 0, 120, 40);
        let layout = tree.solve_layout(viewport).expect("layout solves");
        let splitters = collect_splitter_primitives(&tree, &layout, viewport, None, None);
        let first = splitters.first().copied().expect("a splitter exists");
        let split_rect = layout.rect(first.split_id).expect("split has a rect");
        (tree, layout, first.target, split_rect)
    }

    #[test]
    fn resize_overlay_ghost_tracks_proposed_share() {
        let (tree, layout, target, _rect) = first_resize_target();
        let cfg = PaneResizeOverlayConfig::default();
        let low = build_resize_overlay(&tree, &layout, target, 2_000, cfg).expect("overlay builds");
        let high =
            build_resize_overlay(&tree, &layout, target, 8_000, cfg).expect("overlay builds");
        assert_eq!(low.ghost.share_bps, 2_000);
        assert_eq!(high.ghost.share_bps, 8_000);
        // A larger first-child share pushes the ghost divider further along the
        // split axis.
        match target.axis {
            SplitAxis::Horizontal => assert!(high.ghost.rail.x > low.ghost.rail.x),
            SplitAxis::Vertical => assert!(high.ghost.rail.y > low.ghost.rail.y),
        }
        // The ghost rail is one cell thick on its cross axis.
        match target.axis {
            SplitAxis::Horizontal => assert_eq!(low.ghost.rail.width, 1),
            SplitAxis::Vertical => assert_eq!(low.ghost.rail.height, 1),
        }
    }

    #[test]
    fn resize_overlay_badges_partition_the_split() {
        let (tree, layout, target, split_rect) = first_resize_target();
        let overlay = build_resize_overlay(
            &tree,
            &layout,
            target,
            6_000,
            PaneResizeOverlayConfig::default(),
        )
        .expect("overlay builds");
        assert_eq!(overlay.badges.len(), 2, "one badge per child");
        let (first, second) = (&overlay.badges[0], &overlay.badges[1]);
        // The two child rects tile the split with no gap or overlap on the axis.
        match target.axis {
            SplitAxis::Horizontal => {
                assert_eq!(first.anchor.width + second.anchor.width, split_rect.width);
                assert_eq!(second.anchor.x, first.anchor.x + first.anchor.width);
            }
            SplitAxis::Vertical => {
                assert_eq!(
                    first.anchor.height + second.anchor.height,
                    split_rect.height
                );
                assert_eq!(second.anchor.y, first.anchor.y + first.anchor.height);
            }
        }
        // Labels are deterministic: 60% / 40% with the proposed dimensions.
        assert!(first.label.starts_with("60% "), "got {}", first.label);
        assert!(second.label.starts_with("40% "), "got {}", second.label);
    }

    #[test]
    fn resize_overlay_detects_snap_targets() {
        let (tree, layout, target, _rect) = first_resize_target();
        let cfg = PaneResizeOverlayConfig::default(); // step = 500 bps
        // Exactly on a snap multiple -> snapped, guide sits at the same share.
        let on = build_resize_overlay(&tree, &layout, target, 5_000, cfg).expect("builds");
        assert!(on.ghost.snapped);
        assert_eq!(on.snap_guides.len(), 1);
        assert_eq!(on.snap_guides[0].share_bps, 5_000);
        // Midway between two snap multiples (5_000 and 5_500) -> not snapped, but
        // the guide still points at the nearest target.
        let off = build_resize_overlay(&tree, &layout, target, 5_250, cfg).expect("builds");
        assert!(!off.ghost.snapped);
        assert!(off.snap_guides[0].share_bps == 5_000 || off.snap_guides[0].share_bps == 5_500);
    }

    #[test]
    fn resize_overlay_config_toggles_keep_it_uncluttered() {
        let (tree, layout, target, _rect) = first_resize_target();
        let minimal = PaneResizeOverlayConfig {
            show_snap_guides: false,
            show_size_badges: false,
            snap_step_bps: PANE_SNAP_DEFAULT_STEP_BPS,
            snap_hysteresis_bps: PANE_SNAP_DEFAULT_HYSTERESIS_BPS,
        };
        let overlay = build_resize_overlay(&tree, &layout, target, 5_000, minimal).expect("builds");
        assert!(overlay.snap_guides.is_empty(), "guides suppressed");
        assert!(overlay.badges.is_empty(), "badges suppressed");
        // The ghost boundary always renders (it is the core resize feedback).
        assert_eq!(overlay.ghost.share_bps, 5_000);
    }

    #[test]
    fn resize_overlay_is_stable_under_repeated_builds() {
        let (tree, layout, target, _rect) = first_resize_target();
        let cfg = PaneResizeOverlayConfig::default();
        let a = build_resize_overlay(&tree, &layout, target, 4_321, cfg).expect("builds");
        let b = build_resize_overlay(&tree, &layout, target, 4_321, cfg).expect("builds");
        assert_eq!(a, b, "identical drag sample yields identical overlay");
    }

    #[test]
    fn resize_overlay_rejects_non_split_target() {
        let tree = default_pane_layout_tree();
        let viewport = Rect::new(0, 0, 120, 40);
        let layout = tree.solve_layout(viewport).expect("layout solves");
        let leaf = tree
            .nodes()
            .find_map(|n| matches!(n.kind, PaneNodeKind::Leaf(_)).then_some(n.id))
            .expect("a leaf exists");
        let bogus = PaneResizeTarget {
            split_id: leaf,
            axis: SplitAxis::Horizontal,
        };
        assert!(
            build_resize_overlay(
                &tree,
                &layout,
                bogus,
                5_000,
                PaneResizeOverlayConfig::default()
            )
            .is_none(),
            "a leaf target must not produce a resize overlay"
        );
    }

    #[test]
    fn overlay_layer_z_order_is_deterministic() {
        // Guides paint under the ghost, which paints under badges.
        assert!(PaneOverlayLayer::SnapGuide.z_index() < PaneOverlayLayer::GhostBoundary.z_index());
        assert!(PaneOverlayLayer::GhostBoundary.z_index() < PaneOverlayLayer::SizeBadge.z_index());
        // Ord agrees with z_index.
        assert!(PaneOverlayLayer::SnapGuide < PaneOverlayLayer::SizeBadge);
    }
}

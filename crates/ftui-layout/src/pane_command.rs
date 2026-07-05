//! Canonical keyboard command model and focus-graph semantics for panes
//! (bd-21pbi.1).
//!
//! This module defines a **host-agnostic** keyboard interaction contract for
//! the pane workspace: a normative command vocabulary ([`PaneCommand`]), a
//! deterministic focus graph over the split tree (in-order traversal and
//! spatial/directional navigation with explicit tie-breaks), a pure command
//! resolver ([`resolve`]) that maps each command to a focus change, a list of
//! structural [`PaneOperation`]s, or a transient view-state change, plus
//! explicit repeat/acceleration ([`PaneCommandAcceleration`]) and keymap
//! precedence ([`PaneKeymapPrecedence`]) policies.
//!
//! Why a command layer? Pointer drag-resize already has a first-class semantic
//! pipeline (`PaneSemanticInputEvent` -> `PaneDragResizeMachine` ->
//! `PaneTree::operations_for_transition`). Keyboard interaction needs an
//! equivalent, but its vocabulary is broader than resize (focus navigation,
//! split/close/move/swap, maximize/restore) and must NOT be expressed as
//! host-specific key events. [`PaneCommand`] is that vocabulary: terminal
//! (bd-21pbi.2) and web (bd-21pbi.3) hosts translate raw key events into
//! `PaneCommand`s, and [`resolve`] turns each command into the same outcome on
//! every host. This is the executable form of the normative spec in
//! `docs/spec/pane-keyboard-interaction.md`.
//!
//! Determinism guarantee: for a fixed split-tree topology, layout, and focus
//! context, [`resolve`] is a pure function. Equivalent command streams
//! therefore yield identical pane state (topology hash + active pane) across
//! hosts — the cross-host parity property the pane validation epic requires.

use crate::Rect;
use crate::pane::{
    PANE_SNAP_DEFAULT_HYSTERESIS_BPS, PaneAffordanceMotion, PaneDragResizeMachine, PaneId,
    PaneLayout, PaneLeaf, PaneNodeKind, PaneOperation, PanePlacement, PanePressureSnapProfile,
    PaneResizeDirection, PaneResizeTarget, PaneSemanticInputEvent, PaneSemanticInputEventKind,
    PaneSplitRatio, PaneTree, SplitAxis,
};

/// Neutral snap profile used when lowering keyboard resize commands through the
/// shared drag/resize machine. Keyboard nudges apply a fixed basis-point step
/// and are independent of snap pressure, so any valid profile yields identical
/// operations; this constant keeps the lowering deterministic.
const KEYBOARD_NEUTRAL_PRESSURE: PanePressureSnapProfile = PanePressureSnapProfile {
    strength_bps: 5_000,
    hysteresis_bps: PANE_SNAP_DEFAULT_HYSTERESIS_BPS,
};

/// A cardinal direction for spatial focus and pane movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneCardinalDirection {
    /// Toward smaller x.
    Left,
    /// Toward larger x.
    Right,
    /// Toward smaller y.
    Up,
    /// Toward larger y.
    Down,
}

impl PaneCardinalDirection {
    /// The split axis a move/dock in this direction creates.
    #[must_use]
    pub const fn axis(self) -> SplitAxis {
        match self {
            Self::Left | Self::Right => SplitAxis::Horizontal,
            Self::Up | Self::Down => SplitAxis::Vertical,
        }
    }

    /// Placement of an incoming (moved) pane relative to a dock target in this
    /// direction. Left/Up place the incoming pane first; Right/Down place it
    /// second.
    #[must_use]
    pub const fn incoming_placement(self) -> PanePlacement {
        match self {
            Self::Left | Self::Up => PanePlacement::IncomingFirst,
            Self::Right | Self::Down => PanePlacement::ExistingFirst,
        }
    }
}

/// Cyclic ordinal over the deterministic focus order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneFocusOrdinal {
    /// The next leaf in focus order (wraps to the first).
    Next,
    /// The previous leaf in focus order (wraps to the last).
    Previous,
}

/// The canonical, host-agnostic pane keyboard command vocabulary.
///
/// Hosts translate raw key events into these commands; [`resolve`] maps each to
/// a deterministic outcome. Resize unit counts are supplied by the caller
/// (computed via [`PaneCommandAcceleration`]) so the command itself stays a
/// pure intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneCommand {
    /// Move focus to the next leaf in deterministic focus order (cyclic).
    FocusNext,
    /// Move focus to the previous leaf in deterministic focus order (cyclic).
    FocusPrevious,
    /// Move focus to the nearest leaf in a direction (spatial).
    FocusDirectional(PaneCardinalDirection),
    /// Move focus to the extreme leaf in a direction (jump to edge).
    FocusEdge(PaneCardinalDirection),
    /// Grow (`Increase`) or shrink (`Decrease`) the active pane by `units`
    /// snap steps, resizing its enclosing split.
    ResizeStep {
        /// `Increase` grows the active pane; `Decrease` shrinks it.
        direction: PaneResizeDirection,
        /// Number of snap steps (one step = `PANE_SNAP_DEFAULT_STEP_BPS`).
        units: u16,
    },
    /// Split the active leaf along `axis`, adding a new sibling leaf.
    Split(SplitAxis),
    /// Close the active leaf, promoting its sibling.
    Close,
    /// Move the active pane to dock against the nearest pane in a direction.
    MovePane(PaneCardinalDirection),
    /// Swap the active pane with its cyclic neighbour.
    SwapPane(PaneFocusOrdinal),
    /// Maximize the active pane (transient view state; no topology change).
    Maximize,
    /// Restore from a maximized state (transient view state).
    Restore,
}

/// Host-owned focus context consumed by [`resolve`]. This is plain input data,
/// not competing persistent state: hosts store the active/maximized pane
/// wherever they like (e.g. `WorkspaceSnapshot::active_pane_id`) and pass a
/// snapshot here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaneFocusContext {
    /// The currently focused leaf, if any.
    pub active: Option<PaneId>,
    /// The currently maximized leaf, if any.
    pub maximized: Option<PaneId>,
}

impl PaneFocusContext {
    /// Construct a focus context with the given active leaf and no maximized
    /// pane.
    #[must_use]
    pub const fn active(active: PaneId) -> Self {
        Self {
            active: Some(active),
            maximized: None,
        }
    }
}

/// The concrete effect a resolved command produces. Focus changes and maximize
/// state are transient (no tree mutation); structural changes are a list of
/// [`PaneOperation`]s the host applies via `PaneTree::apply_operation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneCommandEffect {
    /// Focus moved from `previous` to `active` with no topology change.
    Focus {
        /// The previously focused leaf, if any.
        previous: Option<PaneId>,
        /// The newly focused leaf.
        active: PaneId,
    },
    /// Apply these structural operations in order.
    Structural(Vec<PaneOperation>),
    /// Enter the maximized view state for `target`.
    Maximize {
        /// The leaf to maximize.
        target: PaneId,
    },
    /// Leave the maximized view state previously held by `previous`.
    Restore {
        /// The leaf that was maximized.
        previous: PaneId,
    },
    /// The command had no effect; see the reason.
    Noop(PaneCommandNoopReason),
}

/// Why a command resolved to a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneCommandNoopReason {
    /// No active pane was set in the focus context.
    NoActivePane,
    /// The active pane id was not a leaf in the tree.
    ActiveNotLeaf,
    /// Only one pane exists; navigation/structural change is meaningless.
    OnlyOnePane,
    /// No pane exists in the requested direction.
    NoTargetInDirection,
    /// The active pane is the root and cannot be closed.
    RootCannotClose,
    /// The active leaf has no enclosing split to resize.
    NoEnclosingSplit,
    /// A pane is already maximized (or the active one is).
    AlreadyMaximized,
    /// No pane is currently maximized.
    NotMaximized,
}

/// The full result of resolving one [`PaneCommand`]: the effect plus the
/// resulting focus context the host should adopt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCommandResolution {
    /// What the command does.
    pub effect: PaneCommandEffect,
    /// The active pane after the command (unchanged on no-op).
    pub next_active: Option<PaneId>,
    /// The maximized pane after the command (unchanged on no-op).
    pub next_maximized: Option<PaneId>,
}

impl PaneCommandResolution {
    fn noop(reason: PaneCommandNoopReason, ctx: PaneFocusContext) -> Self {
        Self {
            effect: PaneCommandEffect::Noop(reason),
            next_active: ctx.active,
            next_maximized: ctx.maximized,
        }
    }

    /// True when the command produced a real effect (not a no-op).
    #[must_use]
    pub const fn is_effective(&self) -> bool {
        !matches!(self.effect, PaneCommandEffect::Noop(_))
    }
}

/// Repeat-key acceleration policy for resize stepping. Explicit and testable:
/// the first `accelerate_after_repeats` presses use `base_units`; sustained
/// repeats escalate to `accelerated_units`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneCommandAcceleration {
    /// Snap steps applied on a non-repeated (or early-repeat) press.
    pub base_units: u16,
    /// Snap steps applied once the key has repeated enough.
    pub accelerated_units: u16,
    /// Repeat count at/after which acceleration engages.
    pub accelerate_after_repeats: u16,
}

impl Default for PaneCommandAcceleration {
    fn default() -> Self {
        Self {
            base_units: 1,
            accelerated_units: 5,
            accelerate_after_repeats: 3,
        }
    }
}

impl PaneCommandAcceleration {
    /// Snap steps for a key held with the given repeat count (0 = first press).
    #[must_use]
    pub const fn units_for(&self, repeat_count: u16) -> u16 {
        if repeat_count >= self.accelerate_after_repeats {
            self.accelerated_units
        } else {
            self.base_units
        }
    }
}

/// Conflict precedence between application-level keymaps and the pane manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaneKeymapPrecedence {
    /// The pane manager wins conflicts while it holds focus (default). Apps
    /// must opt specific keys out via [`PaneKeymapPrecedence::ApplicationFirst`]
    /// or by not binding them to pane commands.
    #[default]
    PaneManagerFirst,
    /// The application wins conflicts (e.g. for globally reserved shortcuts).
    ApplicationFirst,
}

/// Who owns a key given its bindings and the active precedence policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKeymapOwner {
    /// The application handles the key.
    Application,
    /// The pane manager handles the key.
    PaneManager,
    /// Neither binds the key.
    Unbound,
}

impl PaneKeymapPrecedence {
    /// Resolve which layer owns a key given whether each layer binds it.
    #[must_use]
    pub const fn resolve(self, app_bound: bool, pane_bound: bool) -> PaneKeymapOwner {
        match (app_bound, pane_bound) {
            (false, false) => PaneKeymapOwner::Unbound,
            (true, false) => PaneKeymapOwner::Application,
            (false, true) => PaneKeymapOwner::PaneManager,
            (true, true) => match self {
                Self::PaneManagerFirst => PaneKeymapOwner::PaneManager,
                Self::ApplicationFirst => PaneKeymapOwner::Application,
            },
        }
    }
}

/// Host-agnostic accessibility preferences for pane interaction (bd-21pbi.5).
///
/// These three adaptive modes are applied uniformly across hosts so a
/// pointer-free, low-vision, or touch user gets the same behavior in the
/// terminal and in the browser:
///
/// - **Reduced motion** collapses affordance micro-animations to instant steps
///   (see [`PaneAffordanceMotion`]) — the state change is preserved, the motion
///   is not.
/// - **High contrast** lifts splitter / focus-ring affordance colors to WCAG
///   AAA (applied by the host theme; see `ftui_style::PaneAffordanceTheme`,
///   which takes this flag).
/// - **Large target** enlarges splitter handles / hit regions for precision and
///   touch ergonomics (see [`enlarge_target`](Self::enlarge_target)).
///
/// Crucially, none of these modes changes the keyboard command vocabulary, the
/// focus graph, or announcements — pane *semantics* are mode-invariant. They are
/// not inputs to [`resolve`] at all; they only affect presentation (motion,
/// color, size). This is what keeps keyboard/focus behavior deterministic and
/// identical regardless of the active accessibility modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaneAccessibilityPreferences {
    /// Collapse affordance micro-animations to instant steps.
    pub reduced_motion: bool,
    /// Use high-contrast (WCAG AAA) affordance colors.
    pub high_contrast: bool,
    /// Enlarge splitter handles / hit regions.
    pub large_target: bool,
}

impl PaneAccessibilityPreferences {
    /// No accessibility modes enabled (equivalent to [`Default`]).
    #[must_use]
    pub const fn none() -> Self {
        Self {
            reduced_motion: false,
            high_contrast: false,
            large_target: false,
        }
    }

    /// All accessibility modes enabled.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            reduced_motion: true,
            high_contrast: true,
            large_target: true,
        }
    }

    /// Set the reduced-motion preference.
    #[must_use]
    pub const fn with_reduced_motion(mut self, on: bool) -> Self {
        self.reduced_motion = on;
        self
    }

    /// Set the high-contrast preference.
    #[must_use]
    pub const fn with_high_contrast(mut self, on: bool) -> Self {
        self.high_contrast = on;
        self
    }

    /// Set the large-target preference.
    #[must_use]
    pub const fn with_large_target(mut self, on: bool) -> Self {
        self.large_target = on;
        self
    }

    /// Whether any adaptive mode is active.
    #[must_use]
    pub const fn any(self) -> bool {
        self.reduced_motion || self.high_contrast || self.large_target
    }

    /// The affordance micro-animation policy implied by these preferences:
    /// the default timing, with motion stepped when reduced-motion is set.
    #[must_use]
    pub fn affordance_motion(self) -> PaneAffordanceMotion {
        PaneAffordanceMotion::default().with_reduced_motion(self.reduced_motion)
    }

    /// The minimum interactive target size (in cells) for a `base` size.
    ///
    /// In large-target mode a base of `n` cells grows to ~150% (rounded up),
    /// with at least a +1-cell bump so even a 1-cell rail becomes easier to hit;
    /// otherwise `base` is returned unchanged. Monotonic non-decreasing in
    /// `base`, and never smaller than `base`.
    #[must_use]
    pub fn enlarge_target(self, base: u16) -> u16 {
        if !self.large_target {
            return base;
        }
        let scaled = (u32::from(base) * 3).div_ceil(2) as u16;
        scaled.max(base.saturating_add(1))
    }
}

// --------------------------------------------------------------------------
// Focus graph
// --------------------------------------------------------------------------

/// Deterministic focus order: leaves in topological depth-first order
/// (`first` child before `second` at every split). This is the canonical "tab
/// order" and is independent of solved geometry, so it is stable for any
/// layout area.
#[must_use]
pub fn focus_order(tree: &PaneTree) -> Vec<PaneId> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let Some(record) = tree.node(id) else {
            continue;
        };
        match &record.kind {
            PaneNodeKind::Leaf(_) => out.push(id),
            PaneNodeKind::Split(split) => {
                // Push second then first so first is visited first (in-order).
                stack.push(split.second);
                stack.push(split.first);
            }
        }
    }
    out
}

/// The cyclic neighbour of `active` in [`focus_order`]. Returns `None` when
/// `active` is absent from the order or the tree has fewer than two leaves.
#[must_use]
pub fn focus_cyclic(tree: &PaneTree, active: PaneId, ordinal: PaneFocusOrdinal) -> Option<PaneId> {
    let order = focus_order(tree);
    if order.len() < 2 {
        return None;
    }
    let index = order.iter().position(|&id| id == active)?;
    let len = order.len();
    let next = match ordinal {
        PaneFocusOrdinal::Next => (index + 1) % len,
        PaneFocusOrdinal::Previous => (index + len - 1) % len,
    };
    Some(order[next])
}

fn center(rect: Rect) -> (i32, i32) {
    (
        i32::from(rect.x) + i32::from(rect.width) / 2,
        i32::from(rect.y) + i32::from(rect.height) / 2,
    )
}

/// Overlap length of two half-open intervals `[a0, a1)` and `[b0, b1)`.
fn overlap_1d(a0: u16, a1: u16, b0: u16, b1: u16) -> i32 {
    let lo = a0.max(b0);
    let hi = a1.min(b1);
    i32::from(hi.saturating_sub(lo))
}

/// The nearest leaf to `active` in a cardinal direction, using solved geometry.
///
/// Tie-break order (all deterministic): smallest primary-axis center distance,
/// then largest perpendicular overlap, then earliest [`focus_order`] index.
/// Returns `None` when no leaf lies in the direction.
#[must_use]
pub fn focus_directional(
    tree: &PaneTree,
    layout: &PaneLayout,
    active: PaneId,
    direction: PaneCardinalDirection,
) -> Option<PaneId> {
    let order = focus_order(tree);
    let active_rect = layout.rect(active)?;
    let (acx, acy) = center(active_rect);

    let mut best: Option<((i32, i32, usize), PaneId)> = None;
    for (index, &leaf) in order.iter().enumerate() {
        if leaf == active {
            continue;
        }
        let Some(rect) = layout.rect(leaf) else {
            continue;
        };
        let (cx, cy) = center(rect);
        // Pane layouts are guillotine splits (axis-aligned tilings), so the
        // correct "in direction" test is by EDGE, not center: a candidate must
        // lie wholly on the requested side of the active pane. This excludes
        // diagonal neighbours that merely have a more extreme center.
        let in_direction = match direction {
            PaneCardinalDirection::Left => rect.right() <= active_rect.left(),
            PaneCardinalDirection::Right => rect.left() >= active_rect.right(),
            PaneCardinalDirection::Up => rect.bottom() <= active_rect.top(),
            PaneCardinalDirection::Down => rect.top() >= active_rect.bottom(),
        };
        if !in_direction {
            continue;
        }
        let primary = match direction {
            PaneCardinalDirection::Left | PaneCardinalDirection::Right => (cx - acx).abs(),
            PaneCardinalDirection::Up | PaneCardinalDirection::Down => (cy - acy).abs(),
        };
        let overlap = match direction {
            PaneCardinalDirection::Left | PaneCardinalDirection::Right => overlap_1d(
                active_rect.top(),
                active_rect.bottom(),
                rect.top(),
                rect.bottom(),
            ),
            PaneCardinalDirection::Up | PaneCardinalDirection::Down => overlap_1d(
                active_rect.left(),
                active_rect.right(),
                rect.left(),
                rect.right(),
            ),
        };
        // Minimize primary distance, then maximize overlap (store negated),
        // then minimize focus-order index.
        let key = (primary, -overlap, index);
        if best.as_ref().is_none_or(|(best_key, _)| key < *best_key) {
            best = Some((key, leaf));
        }
    }
    best.map(|(_, leaf)| leaf)
}

/// The extreme leaf in a cardinal direction (jump to edge). Returns the active
/// leaf's own id when it is already the extreme, so callers can treat that as a
/// no-op.
#[must_use]
pub fn focus_edge(
    tree: &PaneTree,
    layout: &PaneLayout,
    direction: PaneCardinalDirection,
) -> Option<PaneId> {
    let order = focus_order(tree);
    let mut best: Option<((i32, i32, usize), PaneId)> = None;
    for (index, &leaf) in order.iter().enumerate() {
        let Some(rect) = layout.rect(leaf) else {
            continue;
        };
        let (cx, cy) = center(rect);
        // Sort key: the primary coordinate oriented so the extreme is the
        // minimum, then a stable secondary coordinate, then focus-order index.
        let key = match direction {
            PaneCardinalDirection::Left => (cx, cy, index),
            PaneCardinalDirection::Right => (-cx, cy, index),
            PaneCardinalDirection::Up => (cy, cx, index),
            PaneCardinalDirection::Down => (-cy, cx, index),
        };
        if best.as_ref().is_none_or(|(best_key, _)| key < *best_key) {
            best = Some((key, leaf));
        }
    }
    best.map(|(_, leaf)| leaf)
}

// --------------------------------------------------------------------------
// Resolution
// --------------------------------------------------------------------------

/// Resolve a single [`PaneCommand`] against the current tree, solved layout,
/// and focus context. Pure and deterministic: identical inputs always yield an
/// identical [`PaneCommandResolution`], which is what makes equivalent command
/// streams produce identical pane state across hosts.
#[must_use]
pub fn resolve(
    tree: &PaneTree,
    layout: &PaneLayout,
    ctx: PaneFocusContext,
    command: PaneCommand,
) -> PaneCommandResolution {
    match command {
        PaneCommand::FocusNext => resolve_cyclic(tree, ctx, PaneFocusOrdinal::Next),
        PaneCommand::FocusPrevious => resolve_cyclic(tree, ctx, PaneFocusOrdinal::Previous),
        PaneCommand::FocusDirectional(direction) => {
            resolve_focus(tree, ctx, focus_dir(tree, layout, ctx, direction))
        }
        PaneCommand::FocusEdge(direction) => {
            resolve_focus(tree, ctx, focus_edge(tree, layout, direction))
        }
        PaneCommand::ResizeStep { direction, units } => {
            resolve_resize(tree, layout, ctx, direction, units)
        }
        PaneCommand::Split(axis) => resolve_split(tree, ctx, axis),
        PaneCommand::Close => resolve_close(tree, ctx),
        PaneCommand::MovePane(direction) => resolve_move(tree, layout, ctx, direction),
        PaneCommand::SwapPane(ordinal) => resolve_swap(tree, ctx, ordinal),
        PaneCommand::Maximize => resolve_maximize(tree, ctx),
        PaneCommand::Restore => resolve_restore(ctx),
    }
}

/// The active leaf id, if present and actually a leaf.
fn active_leaf(tree: &PaneTree, ctx: PaneFocusContext) -> Result<PaneId, PaneCommandNoopReason> {
    let active = ctx.active.ok_or(PaneCommandNoopReason::NoActivePane)?;
    match tree.node(active).map(|record| &record.kind) {
        Some(PaneNodeKind::Leaf(_)) => Ok(active),
        _ => Err(PaneCommandNoopReason::ActiveNotLeaf),
    }
}

fn focus_dir(
    tree: &PaneTree,
    layout: &PaneLayout,
    ctx: PaneFocusContext,
    direction: PaneCardinalDirection,
) -> Option<PaneId> {
    let active = ctx.active?;
    focus_directional(tree, layout, active, direction)
}

fn resolve_cyclic(
    tree: &PaneTree,
    ctx: PaneFocusContext,
    ordinal: PaneFocusOrdinal,
) -> PaneCommandResolution {
    let active = match active_leaf(tree, ctx) {
        Ok(active) => active,
        Err(reason) => return PaneCommandResolution::noop(reason, ctx),
    };
    match focus_cyclic(tree, active, ordinal) {
        Some(next) => focus_changed(ctx, next),
        None => PaneCommandResolution::noop(PaneCommandNoopReason::OnlyOnePane, ctx),
    }
}

fn resolve_focus(
    tree: &PaneTree,
    ctx: PaneFocusContext,
    target: Option<PaneId>,
) -> PaneCommandResolution {
    if active_leaf(tree, ctx).is_err() && ctx.active.is_some() {
        return PaneCommandResolution::noop(PaneCommandNoopReason::ActiveNotLeaf, ctx);
    }
    match target {
        Some(next) if Some(next) != ctx.active => focus_changed(ctx, next),
        _ => PaneCommandResolution::noop(PaneCommandNoopReason::NoTargetInDirection, ctx),
    }
}

fn focus_changed(ctx: PaneFocusContext, next: PaneId) -> PaneCommandResolution {
    PaneCommandResolution {
        effect: PaneCommandEffect::Focus {
            previous: ctx.active,
            active: next,
        },
        next_active: Some(next),
        next_maximized: ctx.maximized,
    }
}

fn structural(
    ctx: PaneFocusContext,
    operations: Vec<PaneOperation>,
    next_active: Option<PaneId>,
) -> PaneCommandResolution {
    PaneCommandResolution {
        effect: PaneCommandEffect::Structural(operations),
        next_active,
        next_maximized: ctx.maximized,
    }
}

/// Enclosing split of a leaf: its parent, when that parent is a split.
fn enclosing_split(tree: &PaneTree, leaf: PaneId) -> Option<PaneId> {
    let parent = tree.node(leaf)?.parent?;
    match &tree.node(parent)?.kind {
        PaneNodeKind::Split(_) => Some(parent),
        PaneNodeKind::Leaf(_) => None,
    }
}

fn resolve_resize(
    tree: &PaneTree,
    layout: &PaneLayout,
    ctx: PaneFocusContext,
    direction: PaneResizeDirection,
    units: u16,
) -> PaneCommandResolution {
    let active = match active_leaf(tree, ctx) {
        Ok(active) => active,
        Err(reason) => return PaneCommandResolution::noop(reason, ctx),
    };
    let Some(split_id) = enclosing_split(tree, active) else {
        return PaneCommandResolution::noop(PaneCommandNoopReason::NoEnclosingSplit, ctx);
    };
    let Some(PaneNodeKind::Split(split)) = tree.node(split_id).map(|record| &record.kind) else {
        return PaneCommandResolution::noop(PaneCommandNoopReason::NoEnclosingSplit, ctx);
    };
    // The command direction is active-pane-relative (grow/shrink the active
    // pane). Translate to the split's first-share direction: growing the
    // first child increases first-share; growing the second decreases it.
    let split_direction = if split.first == active {
        direction
    } else {
        flip_direction(direction)
    };
    let target = PaneResizeTarget {
        split_id,
        axis: split.axis,
    };
    // Lower through the shared resize machine so keyboard resize uses the exact
    // same nudge math as pointer/wheel resize (no duplicated logic).
    let event = PaneSemanticInputEvent::new(
        1,
        PaneSemanticInputEventKind::KeyboardResize {
            target,
            direction: split_direction,
            units,
        },
    );
    let mut machine = PaneDragResizeMachine::default();
    let Ok(transition) = machine.apply_event(&event) else {
        return PaneCommandResolution::noop(PaneCommandNoopReason::NoEnclosingSplit, ctx);
    };
    let operations = tree.operations_for_transition(&transition, layout, KEYBOARD_NEUTRAL_PRESSURE);
    structural(ctx, operations, ctx.active)
}

const fn flip_direction(direction: PaneResizeDirection) -> PaneResizeDirection {
    match direction {
        PaneResizeDirection::Increase => PaneResizeDirection::Decrease,
        PaneResizeDirection::Decrease => PaneResizeDirection::Increase,
    }
}

fn resolve_split(tree: &PaneTree, ctx: PaneFocusContext, axis: SplitAxis) -> PaneCommandResolution {
    let active = match active_leaf(tree, ctx) {
        Ok(active) => active,
        Err(reason) => return PaneCommandResolution::noop(reason, ctx),
    };
    let surface_key = leaf_surface_key(tree, active).unwrap_or_default();
    let new_leaf = PaneLeaf::new(format!("{surface_key}#split"));
    let ratio = PaneSplitRatio::default();
    let op = PaneOperation::SplitLeaf {
        target: active,
        axis,
        ratio,
        placement: PanePlacement::ExistingFirst,
        new_leaf,
    };
    // The new leaf id is allocated by `apply_operation`, so focus stays on the
    // existing pane; hosts may refocus the new leaf after applying.
    structural(ctx, vec![op], ctx.active)
}

fn leaf_surface_key(tree: &PaneTree, leaf: PaneId) -> Option<String> {
    match &tree.node(leaf)?.kind {
        PaneNodeKind::Leaf(record) => Some(record.surface_key.clone()),
        PaneNodeKind::Split(_) => None,
    }
}

fn resolve_close(tree: &PaneTree, ctx: PaneFocusContext) -> PaneCommandResolution {
    let active = match active_leaf(tree, ctx) {
        Ok(active) => active,
        Err(reason) => return PaneCommandResolution::noop(reason, ctx),
    };
    if active == tree.root() {
        return PaneCommandResolution::noop(PaneCommandNoopReason::RootCannotClose, ctx);
    }
    let order = focus_order(tree);
    if order.len() < 2 {
        return PaneCommandResolution::noop(PaneCommandNoopReason::OnlyOnePane, ctx);
    }
    // Focus the deterministic survivor: the next leaf in focus order, or the
    // previous when the active pane was last.
    let next_active = focus_cyclic(tree, active, PaneFocusOrdinal::Next)
        .filter(|&id| id != active)
        .or_else(|| focus_cyclic(tree, active, PaneFocusOrdinal::Previous))
        .filter(|&id| id != active);
    let mut resolution = structural(
        ctx,
        vec![PaneOperation::CloseNode { target: active }],
        next_active,
    );
    // Closing the maximized pane must clear the maximize state — carrying it
    // forward would leave a dangling id pointing at a removed node.
    if ctx.maximized == Some(active) {
        resolution.next_maximized = None;
    }
    resolution
}

fn resolve_move(
    tree: &PaneTree,
    layout: &PaneLayout,
    ctx: PaneFocusContext,
    direction: PaneCardinalDirection,
) -> PaneCommandResolution {
    let active = match active_leaf(tree, ctx) {
        Ok(active) => active,
        Err(reason) => return PaneCommandResolution::noop(reason, ctx),
    };
    let Some(target) = focus_directional(tree, layout, active, direction) else {
        return PaneCommandResolution::noop(PaneCommandNoopReason::NoTargetInDirection, ctx);
    };
    let ratio = PaneSplitRatio::default();
    let op = PaneOperation::MoveSubtree {
        source: active,
        target,
        axis: direction.axis(),
        ratio,
        placement: direction.incoming_placement(),
    };
    structural(ctx, vec![op], ctx.active)
}

fn resolve_swap(
    tree: &PaneTree,
    ctx: PaneFocusContext,
    ordinal: PaneFocusOrdinal,
) -> PaneCommandResolution {
    let active = match active_leaf(tree, ctx) {
        Ok(active) => active,
        Err(reason) => return PaneCommandResolution::noop(reason, ctx),
    };
    match focus_cyclic(tree, active, ordinal) {
        Some(other) if other != active => structural(
            ctx,
            vec![PaneOperation::SwapNodes {
                first: active,
                second: other,
            }],
            ctx.active,
        ),
        _ => PaneCommandResolution::noop(PaneCommandNoopReason::OnlyOnePane, ctx),
    }
}

fn resolve_maximize(tree: &PaneTree, ctx: PaneFocusContext) -> PaneCommandResolution {
    let active = match active_leaf(tree, ctx) {
        Ok(active) => active,
        Err(reason) => return PaneCommandResolution::noop(reason, ctx),
    };
    if ctx.maximized == Some(active) {
        return PaneCommandResolution::noop(PaneCommandNoopReason::AlreadyMaximized, ctx);
    }
    PaneCommandResolution {
        effect: PaneCommandEffect::Maximize { target: active },
        next_active: Some(active),
        next_maximized: Some(active),
    }
}

fn resolve_restore(ctx: PaneFocusContext) -> PaneCommandResolution {
    match ctx.maximized {
        Some(previous) => PaneCommandResolution {
            effect: PaneCommandEffect::Restore { previous },
            next_active: ctx.active,
            next_maximized: None,
        },
        None => PaneCommandResolution::noop(PaneCommandNoopReason::NotMaximized, ctx),
    }
}

// --------------------------------------------------------------------------
// Accessible announcements (bd-21pbi.4)
// --------------------------------------------------------------------------

/// Category of a pane state-change announcement (for host channel routing and
/// coalescing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneAnnouncementCategory {
    /// Focus moved to a different pane.
    Focus,
    /// A pane was resized.
    Resize,
    /// A pane was split.
    Split,
    /// A pane was closed.
    Close,
    /// A pane was moved/docked.
    Move,
    /// Two panes were swapped.
    Swap,
    /// A pane was maximized.
    Maximize,
    /// The layout was restored from maximized.
    Restore,
}

/// A concise, host-agnostic accessibility announcement for a pane state change.
/// The web host renders `text` into an `aria-live` region; the terminal host
/// surfaces it via a status line / log hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneAnnouncement {
    /// The human-readable announcement text.
    pub text: String,
    /// The announcement category.
    pub category: PaneAnnouncementCategory,
}

impl PaneAnnouncement {
    fn new(category: PaneAnnouncementCategory, text: impl Into<String>) -> Self {
        Self {
            category,
            text: text.into(),
        }
    }
}

const fn cardinal_label(direction: PaneCardinalDirection) -> &'static str {
    match direction {
        PaneCardinalDirection::Left => "left",
        PaneCardinalDirection::Right => "right",
        PaneCardinalDirection::Up => "up",
        PaneCardinalDirection::Down => "down",
    }
}

const fn axis_label(axis: SplitAxis) -> &'static str {
    match axis {
        SplitAxis::Horizontal => "horizontal",
        SplitAxis::Vertical => "vertical",
    }
}

/// The active pane's own share of its enclosing split, as a percentage.
fn active_pane_share_pct(tree: &PaneTree, active: PaneId) -> Option<u16> {
    let split_id = enclosing_split(tree, active)?;
    let PaneNodeKind::Split(split) = &tree.node(split_id)?.kind else {
        return None;
    };
    // u64 arithmetic: `num + den` can overflow u32 (any nonzero pair is a
    // valid PaneSplitRatio, e.g. u32::MAX:1), which would wrap into a
    // division by zero in release builds.
    let num = u64::from(split.ratio.numerator());
    let den = u64::from(split.ratio.denominator());
    let first_share = num * 100 / (num + den);
    let share = if split.first == active {
        first_share
    } else {
        100 - first_share
    };
    u16::try_from(share).ok()
}

/// Generate a concise accessibility announcement for a resolved command.
///
/// Returns `None` for no-ops (nothing changed). `tree` MUST be the post-apply
/// state, so counts and ratios reflect the result.
#[must_use]
pub fn announce_command(
    command: PaneCommand,
    resolution: &PaneCommandResolution,
    tree: &PaneTree,
) -> Option<PaneAnnouncement> {
    if matches!(resolution.effect, PaneCommandEffect::Noop(_)) {
        return None;
    }
    let leaf_count = focus_order(tree).len();
    match command {
        PaneCommand::FocusNext
        | PaneCommand::FocusPrevious
        | PaneCommand::FocusDirectional(_)
        | PaneCommand::FocusEdge(_) => {
            let active = resolution.next_active?;
            let label = leaf_surface_key(tree, active).unwrap_or_else(|| "pane".to_owned());
            Some(PaneAnnouncement::new(
                PaneAnnouncementCategory::Focus,
                format!("Focused pane {label}"),
            ))
        }
        PaneCommand::ResizeStep { .. } => {
            let active = resolution.next_active?;
            let pct = active_pane_share_pct(tree, active)?;
            Some(PaneAnnouncement::new(
                PaneAnnouncementCategory::Resize,
                format!("Resized pane to {pct} percent"),
            ))
        }
        PaneCommand::Split(axis) => Some(PaneAnnouncement::new(
            PaneAnnouncementCategory::Split,
            format!("Split pane {}, {leaf_count} panes", axis_label(axis)),
        )),
        PaneCommand::Close => Some(PaneAnnouncement::new(
            PaneAnnouncementCategory::Close,
            format!("Closed pane, {leaf_count} remaining"),
        )),
        PaneCommand::MovePane(dir) => Some(PaneAnnouncement::new(
            PaneAnnouncementCategory::Move,
            format!("Moved pane {}", cardinal_label(dir)),
        )),
        PaneCommand::SwapPane(_) => Some(PaneAnnouncement::new(
            PaneAnnouncementCategory::Swap,
            "Swapped pane",
        )),
        PaneCommand::Maximize => Some(PaneAnnouncement::new(
            PaneAnnouncementCategory::Maximize,
            "Maximized pane",
        )),
        PaneCommand::Restore => Some(PaneAnnouncement::new(
            PaneAnnouncementCategory::Restore,
            "Restored pane",
        )),
    }
}

/// Coalescing announcer that keeps host live regions / status lines bounded and
/// non-spammy: the latest offered announcement wins (so a burst of resize/repeat
/// announcements collapses to one), and consecutive identical text is
/// suppressed. Hosts call [`PaneAnnouncer::take`] once per render / live-region
/// update.
#[derive(Debug, Clone, Default)]
pub struct PaneAnnouncer {
    pending: Option<PaneAnnouncement>,
    last_spoken: Option<String>,
}

impl PaneAnnouncer {
    /// Create an empty announcer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer an announcement; the most recent non-empty offer is retained, so
    /// rapid bursts coalesce to the final state.
    pub fn offer(&mut self, announcement: Option<PaneAnnouncement>) {
        if announcement.is_some() {
            self.pending = announcement;
        }
    }

    /// Take the pending announcement to speak now, suppressing an exact repeat
    /// of the previously spoken text.
    pub fn take(&mut self) -> Option<PaneAnnouncement> {
        let pending = self.pending.take()?;
        if self.last_spoken.as_deref() == Some(pending.text.as_str()) {
            return None;
        }
        self.last_spoken = Some(pending.text.clone());
        Some(pending)
    }

    /// Peek the pending announcement without consuming it.
    #[must_use]
    pub fn pending(&self) -> Option<&PaneAnnouncement> {
        self.pending.as_ref()
    }

    /// The most recently spoken announcement text.
    #[must_use]
    pub fn last_spoken(&self) -> Option<&str> {
        self.last_spoken.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::{
        PANE_SNAP_DEFAULT_STEP_BPS, PANE_TREE_SCHEMA_VERSION, PaneNodeRecord, PaneSplit,
        PaneTreeSnapshot,
    };
    use std::collections::BTreeMap;

    fn pid(raw: u64) -> PaneId {
        PaneId::new(raw).expect("non-zero id")
    }

    #[test]
    fn accessibility_preferences_constructors() {
        assert_eq!(
            PaneAccessibilityPreferences::none(),
            PaneAccessibilityPreferences::default()
        );
        assert!(!PaneAccessibilityPreferences::none().any());
        let all = PaneAccessibilityPreferences::all();
        assert!(all.reduced_motion && all.high_contrast && all.large_target);
        assert!(all.any());
        // Builder setters are independent.
        let only_hc = PaneAccessibilityPreferences::none().with_high_contrast(true);
        assert!(only_hc.high_contrast && !only_hc.reduced_motion && !only_hc.large_target);
        assert!(only_hc.any());
    }

    #[test]
    fn affordance_motion_reflects_reduced_motion_preference() {
        let on = PaneAccessibilityPreferences::none().with_reduced_motion(true);
        assert!(on.affordance_motion().reduced_motion);
        // Stepped: full emphasis on frame 0.
        assert_eq!(
            on.affordance_motion().hover_emphasis_bps(0),
            crate::pane::PANE_AFFORDANCE_EMPHASIS_FULL_BPS
        );
        let off = PaneAccessibilityPreferences::none();
        assert!(!off.affordance_motion().reduced_motion);
        // Animated: frame 0 is below full when not reduced.
        assert!(
            off.affordance_motion().hover_emphasis_bps(0)
                < crate::pane::PANE_AFFORDANCE_EMPHASIS_FULL_BPS
        );
    }

    #[test]
    fn enlarge_target_grows_only_in_large_target_mode_and_is_monotonic() {
        let normal = PaneAccessibilityPreferences::none();
        let large = PaneAccessibilityPreferences::none().with_large_target(true);
        let mut prev_normal = 0u16;
        let mut prev_large = 0u16;
        for base in 0..=20u16 {
            // Default mode is identity.
            assert_eq!(normal.enlarge_target(base), base);
            let grown = large.enlarge_target(base);
            // Large target never shrinks, and strictly grows any positive base.
            assert!(grown >= base, "large target must not shrink base {base}");
            if base > 0 {
                assert!(grown > base, "large target must grow base {base}");
            }
            // Both functions are monotonic non-decreasing in base.
            assert!(normal.enlarge_target(base) >= prev_normal);
            assert!(grown >= prev_large);
            prev_normal = normal.enlarge_target(base);
            prev_large = grown;
        }
        // Concrete ergonomics: a 1-cell rail becomes 2, a 2-cell rail becomes 3.
        assert_eq!(large.enlarge_target(1), 2);
        assert_eq!(large.enlarge_target(2), 3);
        // Saturating: a max base does not overflow.
        assert!(large.enlarge_target(u16::MAX) >= u16::MAX - 1);
    }

    #[test]
    fn accessibility_preferences_are_not_an_input_to_resolution() {
        // The modes are presentation-only: resolve() takes no preferences, so a
        // fixed command resolves to byte-identical effects regardless of which
        // modes a host has enabled. We demonstrate this by resolving the same
        // command and confirming the resolution is independent of any prefs the
        // caller might be tracking alongside it.
        let tree = nested();
        let layout = tree.solve_layout(Rect::new(0, 0, 80, 24)).expect("solves");
        let ctx = PaneFocusContext {
            active: Some(pid(2)),
            maximized: None,
        };
        let baseline = resolve(&tree, &layout, ctx, PaneCommand::FocusNext);
        for prefs in [
            PaneAccessibilityPreferences::none(),
            PaneAccessibilityPreferences::all(),
            PaneAccessibilityPreferences::none().with_large_target(true),
        ] {
            // prefs deliberately do not participate in resolution.
            let _ = prefs;
            let again = resolve(&tree, &layout, ctx, PaneCommand::FocusNext);
            assert_eq!(again.next_active, baseline.next_active);
            assert_eq!(again.next_maximized, baseline.next_maximized);
        }
    }

    /// First-child share of a split node in basis points.
    fn first_share_bps(tree: &PaneTree, split: PaneId) -> u32 {
        match &tree.node(split).expect("split present").kind {
            PaneNodeKind::Split(node) => {
                node.ratio.numerator() * 10_000
                    / (node.ratio.numerator() + node.ratio.denominator())
            }
            PaneNodeKind::Leaf(_) => panic!("expected split node"),
        }
    }

    /// Single leaf root.
    fn single() -> PaneTree {
        let snapshot = PaneTreeSnapshot {
            schema_version: PANE_TREE_SCHEMA_VERSION,
            root: pid(1),
            next_id: pid(2),
            nodes: vec![PaneNodeRecord::leaf(pid(1), None, PaneLeaf::new("only"))],
            extensions: BTreeMap::new(),
        };
        PaneTree::from_snapshot(snapshot).expect("valid single tree")
    }

    /// Horizontal root: left(2) | right-column where right is vertical
    /// top(4)/bottom(5). Leaves in focus order: [2, 4, 5].
    fn nested() -> PaneTree {
        let snapshot = PaneTreeSnapshot {
            schema_version: PANE_TREE_SCHEMA_VERSION,
            root: pid(1),
            next_id: pid(6),
            nodes: vec![
                PaneNodeRecord::split(
                    pid(1),
                    None,
                    PaneSplit {
                        axis: SplitAxis::Horizontal,
                        ratio: PaneSplitRatio::new(1, 1).unwrap(),
                        first: pid(2),
                        second: pid(3),
                    },
                ),
                PaneNodeRecord::leaf(pid(2), Some(pid(1)), PaneLeaf::new("left")),
                PaneNodeRecord::split(
                    pid(3),
                    Some(pid(1)),
                    PaneSplit {
                        axis: SplitAxis::Vertical,
                        ratio: PaneSplitRatio::new(1, 1).unwrap(),
                        first: pid(4),
                        second: pid(5),
                    },
                ),
                PaneNodeRecord::leaf(pid(4), Some(pid(3)), PaneLeaf::new("right_top")),
                PaneNodeRecord::leaf(pid(5), Some(pid(3)), PaneLeaf::new("right_bottom")),
            ],
            extensions: BTreeMap::new(),
        };
        PaneTree::from_snapshot(snapshot).expect("valid nested tree")
    }

    fn solved(tree: &PaneTree) -> PaneLayout {
        tree.solve_layout(Rect::new(0, 0, 80, 24))
            .expect("layout solves")
    }

    #[test]
    fn closing_the_maximized_pane_clears_maximize_state() {
        let tree = nested();
        let layout = solved(&tree);
        let res = resolve(
            &tree,
            &layout,
            PaneFocusContext {
                active: Some(pid(4)),
                maximized: Some(pid(4)),
            },
            PaneCommand::Close,
        );
        assert!(matches!(res.effect, PaneCommandEffect::Structural(_)));
        assert_eq!(
            res.next_maximized, None,
            "a closed pane must not remain the maximize target"
        );
        // Closing a non-maximized pane keeps the maximize state.
        let res2 = resolve(
            &tree,
            &layout,
            PaneFocusContext {
                active: Some(pid(4)),
                maximized: Some(pid(2)),
            },
            PaneCommand::Close,
        );
        assert_eq!(res2.next_maximized, Some(pid(2)));
    }

    #[test]
    fn share_pct_survives_extreme_ratios() {
        // Any nonzero u32 pair is a valid ratio; u32 math would overflow (or
        // divide by zero on wrap) for u32::MAX:1.
        let snapshot = PaneTreeSnapshot {
            schema_version: PANE_TREE_SCHEMA_VERSION,
            root: pid(1),
            next_id: pid(4),
            nodes: vec![
                PaneNodeRecord::split(
                    pid(1),
                    None,
                    PaneSplit {
                        axis: SplitAxis::Horizontal,
                        ratio: PaneSplitRatio::new(u32::MAX, 1).unwrap(),
                        first: pid(2),
                        second: pid(3),
                    },
                ),
                PaneNodeRecord::leaf(pid(2), Some(pid(1)), PaneLeaf::new("a")),
                PaneNodeRecord::leaf(pid(3), Some(pid(1)), PaneLeaf::new("b")),
            ],
            extensions: BTreeMap::new(),
        };
        let tree = PaneTree::from_snapshot(snapshot).expect("valid tree");
        assert_eq!(active_pane_share_pct(&tree, pid(2)), Some(99));
        assert_eq!(active_pane_share_pct(&tree, pid(3)), Some(1));
    }

    #[test]
    fn focus_order_is_topological_in_order() {
        assert_eq!(focus_order(&single()), vec![pid(1)]);
        assert_eq!(focus_order(&nested()), vec![pid(2), pid(4), pid(5)]);
    }

    #[test]
    fn focus_cyclic_wraps_both_directions() {
        let tree = nested();
        assert_eq!(
            focus_cyclic(&tree, pid(2), PaneFocusOrdinal::Next),
            Some(pid(4))
        );
        assert_eq!(
            focus_cyclic(&tree, pid(5), PaneFocusOrdinal::Next),
            Some(pid(2))
        );
        assert_eq!(
            focus_cyclic(&tree, pid(2), PaneFocusOrdinal::Previous),
            Some(pid(5))
        );
        assert_eq!(
            focus_cyclic(&single(), pid(1), PaneFocusOrdinal::Next),
            None
        );
    }

    #[test]
    fn focus_directional_uses_geometry() {
        let tree = nested();
        let layout = solved(&tree);
        // left(2) is to the LEFT of the right column; right of left is top(4)
        // (nearest by overlap/order among 4,5).
        assert_eq!(
            focus_directional(&tree, &layout, pid(2), PaneCardinalDirection::Right),
            Some(pid(4))
        );
        // From top(4), down -> bottom(5); left -> left(2).
        assert_eq!(
            focus_directional(&tree, &layout, pid(4), PaneCardinalDirection::Down),
            Some(pid(5))
        );
        assert_eq!(
            focus_directional(&tree, &layout, pid(4), PaneCardinalDirection::Left),
            Some(pid(2))
        );
        // Nothing to the left of the leftmost pane.
        assert_eq!(
            focus_directional(&tree, &layout, pid(2), PaneCardinalDirection::Left),
            None
        );
    }

    #[test]
    fn focus_edge_jumps_to_extremes() {
        let tree = nested();
        let layout = solved(&tree);
        assert_eq!(
            focus_edge(&tree, &layout, PaneCardinalDirection::Left),
            Some(pid(2))
        );
        assert_eq!(
            focus_edge(&tree, &layout, PaneCardinalDirection::Down),
            Some(pid(5))
        );
    }

    #[test]
    fn resolve_focus_next_changes_active() {
        let tree = nested();
        let layout = solved(&tree);
        let res = resolve(
            &tree,
            &layout,
            PaneFocusContext::active(pid(2)),
            PaneCommand::FocusNext,
        );
        assert_eq!(
            res.effect,
            PaneCommandEffect::Focus {
                previous: Some(pid(2)),
                active: pid(4)
            }
        );
        assert_eq!(res.next_active, Some(pid(4)));
    }

    #[test]
    fn resolve_no_active_is_noop() {
        let tree = nested();
        let layout = solved(&tree);
        let res = resolve(
            &tree,
            &layout,
            PaneFocusContext::default(),
            PaneCommand::FocusNext,
        );
        assert_eq!(
            res.effect,
            PaneCommandEffect::Noop(PaneCommandNoopReason::NoActivePane)
        );
        assert!(!res.is_effective());
    }

    #[test]
    fn resolve_resize_grows_active_via_existing_nudge() {
        let tree = nested();
        let layout = solved(&tree);
        // left(2) is the FIRST child of root split: grow active => increase
        // first-share by 2 steps.
        let res = resolve(
            &tree,
            &layout,
            PaneFocusContext::active(pid(2)),
            PaneCommand::ResizeStep {
                direction: PaneResizeDirection::Increase,
                units: 2,
            },
        );
        let PaneCommandEffect::Structural(ops) = &res.effect else {
            panic!("expected structural effect, got {:?}", res.effect);
        };
        assert_eq!(ops.len(), 1);
        // Apply and confirm the root first-share grew by 2 * step.
        let mut applied = tree.clone();
        let before = first_share_bps(&applied, pid(1));
        for (idx, op) in ops.iter().enumerate() {
            applied
                .apply_operation(idx as u64, op.clone())
                .expect("op applies");
        }
        let after = first_share_bps(&applied, pid(1));
        assert_eq!(after, before + 2 * u32::from(PANE_SNAP_DEFAULT_STEP_BPS));
    }

    #[test]
    fn resolve_resize_second_child_flips_direction() {
        let tree = nested();
        let layout = solved(&tree);
        // bottom(5) is the SECOND child of the vertical split(3): growing it
        // must DECREASE that split's first-share.
        let res = resolve(
            &tree,
            &layout,
            PaneFocusContext::active(pid(5)),
            PaneCommand::ResizeStep {
                direction: PaneResizeDirection::Increase,
                units: 1,
            },
        );
        let PaneCommandEffect::Structural(ops) = &res.effect else {
            panic!("expected structural effect");
        };
        let mut applied = tree.clone();
        let before = first_share_bps(&applied, pid(3));
        for (idx, op) in ops.iter().enumerate() {
            applied
                .apply_operation(idx as u64, op.clone())
                .expect("applies");
        }
        let after = first_share_bps(&applied, pid(3));
        assert_eq!(after, before - u32::from(PANE_SNAP_DEFAULT_STEP_BPS));
    }

    #[test]
    fn resolve_close_picks_deterministic_survivor() {
        let tree = nested();
        let layout = solved(&tree);
        let res = resolve(
            &tree,
            &layout,
            PaneFocusContext::active(pid(2)),
            PaneCommand::Close,
        );
        assert!(matches!(res.effect, PaneCommandEffect::Structural(_)));
        // Closing left(2): next survivor in focus order is top(4).
        assert_eq!(res.next_active, Some(pid(4)));
        // Root cannot be closed.
        let single_tree = single();
        let single_layout = solved(&single_tree);
        let res = resolve(
            &single_tree,
            &single_layout,
            PaneFocusContext::active(pid(1)),
            PaneCommand::Close,
        );
        assert_eq!(
            res.effect,
            PaneCommandEffect::Noop(PaneCommandNoopReason::RootCannotClose)
        );
    }

    #[test]
    fn resolve_maximize_and_restore_roundtrip() {
        let tree = nested();
        let layout = solved(&tree);
        let max = resolve(
            &tree,
            &layout,
            PaneFocusContext::active(pid(4)),
            PaneCommand::Maximize,
        );
        assert_eq!(max.effect, PaneCommandEffect::Maximize { target: pid(4) });
        assert_eq!(max.next_maximized, Some(pid(4)));
        // Maximizing again is a no-op.
        let again = resolve(
            &tree,
            &layout,
            PaneFocusContext {
                active: Some(pid(4)),
                maximized: Some(pid(4)),
            },
            PaneCommand::Maximize,
        );
        assert_eq!(
            again.effect,
            PaneCommandEffect::Noop(PaneCommandNoopReason::AlreadyMaximized)
        );
        // Restore clears it.
        let restore = resolve(
            &tree,
            &layout,
            PaneFocusContext {
                active: Some(pid(4)),
                maximized: Some(pid(4)),
            },
            PaneCommand::Restore,
        );
        assert_eq!(
            restore.effect,
            PaneCommandEffect::Restore { previous: pid(4) }
        );
        assert_eq!(restore.next_maximized, None);
    }

    #[test]
    fn acceleration_policy_is_explicit() {
        let accel = PaneCommandAcceleration::default();
        assert_eq!(accel.units_for(0), 1);
        assert_eq!(accel.units_for(2), 1);
        assert_eq!(accel.units_for(3), 5);
        assert_eq!(accel.units_for(50), 5);
    }

    #[test]
    fn precedence_policy_resolves_conflicts() {
        use PaneKeymapOwner::{Application, PaneManager, Unbound};
        let pm = PaneKeymapPrecedence::PaneManagerFirst;
        let app = PaneKeymapPrecedence::ApplicationFirst;
        assert_eq!(pm.resolve(false, false), Unbound);
        assert_eq!(pm.resolve(true, false), Application);
        assert_eq!(pm.resolve(false, true), PaneManager);
        assert_eq!(pm.resolve(true, true), PaneManager);
        assert_eq!(app.resolve(true, true), Application);
    }

    /// Cross-host equivalence + determinism: a fixed command stream, applied
    /// via the resolver, produces an identical final topology hash and active
    /// pane regardless of which host emitted the commands. Running twice must
    /// match byte-for-byte.
    #[test]
    fn command_stream_is_deterministic_and_host_agnostic() {
        fn run_stream() -> (u64, Option<PaneId>) {
            let mut tree = nested();
            let mut ctx = PaneFocusContext::active(pid(2));
            let stream = [
                PaneCommand::FocusNext,
                PaneCommand::ResizeStep {
                    direction: PaneResizeDirection::Increase,
                    units: 2,
                },
                PaneCommand::FocusDirectional(PaneCardinalDirection::Down),
                PaneCommand::SwapPane(PaneFocusOrdinal::Previous),
                PaneCommand::FocusEdge(PaneCardinalDirection::Left),
            ];
            let mut op_id = 0u64;
            for command in stream {
                let layout = tree.solve_layout(Rect::new(0, 0, 80, 24)).expect("solves");
                let res = resolve(&tree, &layout, ctx, command);
                if let PaneCommandEffect::Structural(ops) = &res.effect {
                    for op in ops {
                        tree.apply_operation(op_id, op.clone()).expect("applies");
                        op_id += 1;
                    }
                }
                ctx.active = res.next_active;
                ctx.maximized = res.next_maximized;
            }
            (tree.state_hash(), ctx.active)
        }

        let first = run_stream();
        let second = run_stream();
        assert_eq!(first, second, "command stream must be deterministic");
        // Active pane must be a real leaf after the stream.
        let tree = nested();
        assert!(focus_order(&tree).contains(&first.1.expect("active set")));
    }

    fn announce(
        tree: &PaneTree,
        layout: &PaneLayout,
        ctx: PaneFocusContext,
        command: PaneCommand,
    ) -> Option<PaneAnnouncement> {
        let res = resolve(tree, layout, ctx, command);
        announce_command(command, &res, tree)
    }

    #[test]
    fn announcements_describe_each_transition() {
        let tree = nested();
        let layout = solved(&tree);
        // Focus.
        let a = announce(
            &tree,
            &layout,
            PaneFocusContext::active(pid(2)),
            PaneCommand::FocusNext,
        )
        .unwrap();
        assert_eq!(a.category, PaneAnnouncementCategory::Focus);
        assert_eq!(a.text, "Focused pane right_top");
        // Maximize / restore.
        let a = announce(
            &tree,
            &layout,
            PaneFocusContext::active(pid(2)),
            PaneCommand::Maximize,
        )
        .unwrap();
        assert_eq!(a.text, "Maximized pane");
        let restore_ctx = PaneFocusContext {
            active: Some(pid(2)),
            maximized: Some(pid(2)),
        };
        let a = announce(&tree, &layout, restore_ctx, PaneCommand::Restore).unwrap();
        assert_eq!(a.text, "Restored pane");
    }

    #[test]
    fn announcement_reports_split_and_close_counts() {
        // Split grows the leaf count to 4; the announcement reflects the AFTER tree.
        let mut tree = nested();
        let layout = solved(&tree);
        let res = resolve(
            &tree,
            &layout,
            PaneFocusContext::active(pid(2)),
            PaneCommand::Split(SplitAxis::Vertical),
        );
        if let PaneCommandEffect::Structural(ops) = &res.effect {
            for (i, op) in ops.iter().enumerate() {
                tree.apply_operation(i as u64, op.clone()).unwrap();
            }
        }
        let a = announce_command(PaneCommand::Split(SplitAxis::Vertical), &res, &tree).unwrap();
        assert_eq!(a.category, PaneAnnouncementCategory::Split);
        assert_eq!(a.text, "Split pane vertical, 4 panes");
    }

    #[test]
    fn resize_announcement_reports_active_share() {
        // left(2) is the first child of the 1:1 root; grow by 1 step (+500 bps = +5%).
        let mut tree = nested();
        let layout = solved(&tree);
        let res = resolve(
            &tree,
            &layout,
            PaneFocusContext::active(pid(2)),
            PaneCommand::ResizeStep {
                direction: PaneResizeDirection::Increase,
                units: 1,
            },
        );
        if let PaneCommandEffect::Structural(ops) = &res.effect {
            for (i, op) in ops.iter().enumerate() {
                tree.apply_operation(i as u64, op.clone()).unwrap();
            }
        }
        let a = announce_command(
            PaneCommand::ResizeStep {
                direction: PaneResizeDirection::Increase,
                units: 1,
            },
            &res,
            &tree,
        )
        .unwrap();
        assert_eq!(a.category, PaneAnnouncementCategory::Resize);
        assert_eq!(a.text, "Resized pane to 55 percent");
    }

    #[test]
    fn noop_command_is_not_announced() {
        let tree = nested();
        let layout = solved(&tree);
        // No active pane -> FocusNext is a no-op -> no announcement.
        let a = announce(
            &tree,
            &layout,
            PaneFocusContext::default(),
            PaneCommand::FocusNext,
        );
        assert!(a.is_none());
    }

    #[test]
    fn announcer_coalesces_bursts_and_dedupes() {
        let mut announcer = PaneAnnouncer::new();
        // A burst of resize announcements coalesces to the latest on take().
        announcer.offer(Some(PaneAnnouncement::new(
            PaneAnnouncementCategory::Resize,
            "Resized pane to 55 percent",
        )));
        announcer.offer(Some(PaneAnnouncement::new(
            PaneAnnouncementCategory::Resize,
            "Resized pane to 60 percent",
        )));
        announcer.offer(Some(PaneAnnouncement::new(
            PaneAnnouncementCategory::Resize,
            "Resized pane to 65 percent",
        )));
        let spoken = announcer.take().unwrap();
        assert_eq!(spoken.text, "Resized pane to 65 percent");
        // Nothing pending now.
        assert!(announcer.take().is_none());
        // Offering the same text again is suppressed (dedupe).
        announcer.offer(Some(PaneAnnouncement::new(
            PaneAnnouncementCategory::Resize,
            "Resized pane to 65 percent",
        )));
        assert!(announcer.take().is_none());
        // A different text is spoken.
        announcer.offer(Some(PaneAnnouncement::new(
            PaneAnnouncementCategory::Focus,
            "Focused pane left",
        )));
        assert_eq!(announcer.take().unwrap().text, "Focused pane left");
        // Offering None does not disturb state.
        announcer.offer(None);
        assert!(announcer.take().is_none());
    }
}

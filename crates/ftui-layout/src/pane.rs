//! Canonical pane split-tree schema and validation.
//!
//! This module defines a host-agnostic pane tree model intended to be shared
//! by terminal and web adapters. It focuses on:
//!
//! - Deterministic node identifiers suitable for replay/diff.
//! - Explicit parent/child relationships for split trees.
//! - Canonical serialization snapshots with forward-compatible extension bags.
//! - Strict validation that rejects malformed trees.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use ftui_core::geometry::Rect;
use serde::{Deserialize, Serialize};

/// Current pane tree schema version.
pub const PANE_TREE_SCHEMA_VERSION: u16 = 1;

/// Current schema version for semantic pane interaction events.
///
/// Versioning policy:
/// - Additive metadata can be carried in `extensions` without a version bump.
/// - Breaking field/semantic changes must bump this version.
pub const PANE_SEMANTIC_INPUT_EVENT_SCHEMA_VERSION: u16 = 1;

/// Stable identifier for pane nodes.
///
/// `0` is reserved/invalid so IDs are always non-zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(u64);

impl PaneId {
    /// Lowest valid pane ID.
    pub const MIN: Self = Self(1);

    /// Create a new pane ID, rejecting 0.
    pub fn new(raw: u64) -> Result<Self, PaneModelError> {
        if raw == 0 {
            return Err(PaneModelError::ZeroPaneId);
        }
        Ok(Self(raw))
    }

    /// Get the raw numeric value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Return the next ID, or an error on overflow.
    pub fn checked_next(self) -> Result<Self, PaneModelError> {
        let Some(next) = self.0.checked_add(1) else {
            return Err(PaneModelError::PaneIdOverflow { current: self });
        };
        Self::new(next)
    }
}

impl Default for PaneId {
    fn default() -> Self {
        Self::MIN
    }
}

/// Orientation of a split node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

/// Ratio between split children, stored in reduced form.
///
/// Interpreted as weight pair `first:second` (not a direct fraction).
/// Example: `3:2` assigns `3 / (3 + 2)` of available space to the first child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSplitRatio {
    numerator: u32,
    denominator: u32,
}

impl PaneSplitRatio {
    /// Create and normalize a ratio.
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, PaneModelError> {
        if numerator == 0 || denominator == 0 {
            return Err(PaneModelError::InvalidSplitRatio {
                numerator,
                denominator,
            });
        }
        let gcd = gcd_u32(numerator, denominator);
        Ok(Self {
            numerator: numerator / gcd,
            denominator: denominator / gcd,
        })
    }

    /// Numerator (always > 0).
    #[must_use]
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    /// Denominator (always > 0).
    #[must_use]
    pub const fn denominator(self) -> u32 {
        self.denominator
    }
}

impl Default for PaneSplitRatio {
    fn default() -> Self {
        Self {
            numerator: 1,
            denominator: 1,
        }
    }
}

/// Per-node size bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneConstraints {
    pub min_width: u16,
    pub min_height: u16,
    pub max_width: Option<u16>,
    pub max_height: Option<u16>,
    pub collapsible: bool,
}

impl PaneConstraints {
    /// Validate constraints for a given node.
    pub fn validate(self, node_id: PaneId) -> Result<(), PaneModelError> {
        if let Some(max_width) = self.max_width
            && max_width < self.min_width
        {
            return Err(PaneModelError::InvalidConstraint {
                node_id,
                axis: "width",
                min: self.min_width,
                max: max_width,
            });
        }
        if let Some(max_height) = self.max_height
            && max_height < self.min_height
        {
            return Err(PaneModelError::InvalidConstraint {
                node_id,
                axis: "height",
                min: self.min_height,
                max: max_height,
            });
        }
        Ok(())
    }
}

impl Default for PaneConstraints {
    fn default() -> Self {
        Self {
            min_width: 1,
            min_height: 1,
            max_width: None,
            max_height: None,
            collapsible: false,
        }
    }
}

/// Leaf payload for pane content identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneLeaf {
    /// Host-provided stable surface key (for replay/diff mapping).
    pub surface_key: String,
    /// Forward-compatible extension bag.
    #[serde(default)]
    pub extensions: BTreeMap<String, String>,
}

impl PaneLeaf {
    /// Build a leaf with a stable surface key.
    #[must_use]
    pub fn new(surface_key: impl Into<String>) -> Self {
        Self {
            surface_key: surface_key.into(),
            extensions: BTreeMap::new(),
        }
    }
}

/// Split payload with child references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSplit {
    pub axis: SplitAxis,
    pub ratio: PaneSplitRatio,
    pub first: PaneId,
    pub second: PaneId,
}

/// Node payload variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneNodeKind {
    Leaf(PaneLeaf),
    Split(PaneSplit),
}

/// Serializable node record in the canonical schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneNodeRecord {
    pub id: PaneId,
    #[serde(default)]
    pub parent: Option<PaneId>,
    #[serde(default)]
    pub constraints: PaneConstraints,
    #[serde(flatten)]
    pub kind: PaneNodeKind,
    /// Forward-compatible extension bag.
    #[serde(default)]
    pub extensions: BTreeMap<String, String>,
}

impl PaneNodeRecord {
    /// Construct a leaf node record.
    #[must_use]
    pub fn leaf(id: PaneId, parent: Option<PaneId>, leaf: PaneLeaf) -> Self {
        Self {
            id,
            parent,
            constraints: PaneConstraints::default(),
            kind: PaneNodeKind::Leaf(leaf),
            extensions: BTreeMap::new(),
        }
    }

    /// Construct a split node record.
    #[must_use]
    pub fn split(id: PaneId, parent: Option<PaneId>, split: PaneSplit) -> Self {
        Self {
            id,
            parent,
            constraints: PaneConstraints::default(),
            kind: PaneNodeKind::Split(split),
            extensions: BTreeMap::new(),
        }
    }
}

/// Canonical serialized pane tree shape.
///
/// The extension maps are reserved for forward-compatible fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneTreeSnapshot {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    pub root: PaneId,
    pub next_id: PaneId,
    pub nodes: Vec<PaneNodeRecord>,
    #[serde(default)]
    pub extensions: BTreeMap<String, String>,
}

fn default_schema_version() -> u16 {
    PANE_TREE_SCHEMA_VERSION
}

impl PaneTreeSnapshot {
    /// Canonicalize node ordering by ID for deterministic serialization.
    pub fn canonicalize(&mut self) {
        self.nodes.sort_by_key(|node| node.id);
    }

    /// Deterministic hash for diagnostics over serialized tree state.
    #[must_use]
    pub fn state_hash(&self) -> u64 {
        snapshot_state_hash(self)
    }

    /// Inspect invariants and emit a structured diagnostics report.
    #[must_use]
    pub fn invariant_report(&self) -> PaneInvariantReport {
        build_invariant_report(self)
    }

    /// Attempt deterministic safe repairs for recoverable invariant issues.
    ///
    /// Safety guardrail: any unrepairable error in the pre-repair report causes
    /// this method to fail without modifying topology.
    pub fn repair_safe(self) -> Result<PaneRepairOutcome, PaneRepairError> {
        repair_snapshot_safe(self)
    }
}

/// Severity for one invariant finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneInvariantSeverity {
    Error,
    Warning,
}

/// Stable code for invariant findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneInvariantCode {
    UnsupportedSchemaVersion,
    DuplicateNodeId,
    MissingRoot,
    RootHasParent,
    MissingParent,
    MissingChild,
    MultipleParents,
    ParentMismatch,
    SelfReferentialSplit,
    DuplicateSplitChildren,
    InvalidSplitRatio,
    InvalidConstraint,
    CycleDetected,
    UnreachableNode,
    NextIdNotGreaterThanExisting,
}

/// One actionable invariant finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInvariantIssue {
    pub code: PaneInvariantCode,
    pub severity: PaneInvariantSeverity,
    pub repairable: bool,
    pub node_id: Option<PaneId>,
    pub related_node: Option<PaneId>,
    pub message: String,
}

/// Structured invariant report over a pane tree snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInvariantReport {
    pub snapshot_hash: u64,
    pub issues: Vec<PaneInvariantIssue>,
}

impl PaneInvariantReport {
    /// Return true if any error-level finding exists.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == PaneInvariantSeverity::Error)
    }

    /// Return true if any unrepairable error-level finding exists.
    #[must_use]
    pub fn has_unrepairable_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == PaneInvariantSeverity::Error && !issue.repairable)
    }
}

/// One deterministic repair action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PaneRepairAction {
    ReparentNode {
        node_id: PaneId,
        before_parent: Option<PaneId>,
        after_parent: Option<PaneId>,
    },
    NormalizeRatio {
        node_id: PaneId,
        before_numerator: u32,
        before_denominator: u32,
        after_numerator: u32,
        after_denominator: u32,
    },
    RemoveOrphanNode {
        node_id: PaneId,
    },
    BumpNextId {
        before: PaneId,
        after: PaneId,
    },
}

/// Outcome from successful safe repair pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRepairOutcome {
    pub before_hash: u64,
    pub after_hash: u64,
    pub report_before: PaneInvariantReport,
    pub report_after: PaneInvariantReport,
    pub actions: Vec<PaneRepairAction>,
    pub tree: PaneTree,
}

/// Failure reason for safe repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneRepairFailure {
    UnsafeIssuesPresent { codes: Vec<PaneInvariantCode> },
    ValidationFailed { error: PaneModelError },
}

impl fmt::Display for PaneRepairFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsafeIssuesPresent { codes } => {
                write!(f, "snapshot contains unsafe invariant issues: {codes:?}")
            }
            Self::ValidationFailed { error } => {
                write!(f, "repaired snapshot failed validation: {error}")
            }
        }
    }
}

impl std::error::Error for PaneRepairFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::ValidationFailed { error } = self {
            return Some(error);
        }
        None
    }
}

/// Error payload for repair attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRepairError {
    pub before_hash: u64,
    pub report: PaneInvariantReport,
    pub reason: PaneRepairFailure,
}

impl fmt::Display for PaneRepairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pane repair failed: {} (before_hash={:#x}, issues={})",
            self.reason,
            self.before_hash,
            self.report.issues.len()
        )
    }
}

impl std::error::Error for PaneRepairError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.reason)
    }
}

/// Concrete layout result for a solved pane tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneLayout {
    pub area: Rect,
    rects: BTreeMap<PaneId, Rect>,
}

impl PaneLayout {
    /// Lookup rectangle for a specific pane node.
    #[must_use]
    pub fn rect(&self, node_id: PaneId) -> Option<Rect> {
        self.rects.get(&node_id).copied()
    }

    /// Iterate all solved rectangles in deterministic ID order.
    pub fn iter(&self) -> impl Iterator<Item = (PaneId, Rect)> + '_ {
        self.rects.iter().map(|(node_id, rect)| (*node_id, *rect))
    }
}

/// Placement of an incoming node relative to an existing node inside a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanePlacement {
    ExistingFirst,
    IncomingFirst,
}

impl PanePlacement {
    fn ordered(self, existing: PaneId, incoming: PaneId) -> (PaneId, PaneId) {
        match self {
            Self::ExistingFirst => (existing, incoming),
            Self::IncomingFirst => (incoming, existing),
        }
    }
}

/// Pointer button for pane interaction events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanePointerButton {
    Primary,
    Secondary,
    Middle,
}

/// Normalized interaction position in pane-local coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanePointerPosition {
    pub x: i32,
    pub y: i32,
}

impl PanePointerPosition {
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Snapshot of active modifiers captured with one semantic event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneModifierSnapshot {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
}

impl PaneModifierSnapshot {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            shift: false,
            alt: false,
            ctrl: false,
            meta: false,
        }
    }
}

impl Default for PaneModifierSnapshot {
    fn default() -> Self {
        Self::none()
    }
}

/// Canonical resize target for semantic pane input events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneResizeTarget {
    pub split_id: PaneId,
    pub axis: SplitAxis,
}

/// Direction for semantic resize commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneResizeDirection {
    Increase,
    Decrease,
}

/// Canonical cancel reasons for pane interaction state machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneCancelReason {
    EscapeKey,
    PointerCancel,
    FocusLost,
    Blur,
    Programmatic,
}

/// Versioned semantic pane interaction event kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PaneSemanticInputEventKind {
    PointerDown {
        target: PaneResizeTarget,
        pointer_id: u32,
        button: PanePointerButton,
        position: PanePointerPosition,
    },
    PointerMove {
        target: PaneResizeTarget,
        pointer_id: u32,
        position: PanePointerPosition,
        delta_x: i32,
        delta_y: i32,
    },
    PointerUp {
        target: PaneResizeTarget,
        pointer_id: u32,
        button: PanePointerButton,
        position: PanePointerPosition,
    },
    WheelNudge {
        target: PaneResizeTarget,
        lines: i16,
    },
    KeyboardResize {
        target: PaneResizeTarget,
        direction: PaneResizeDirection,
        units: u16,
    },
    Cancel {
        target: Option<PaneResizeTarget>,
        reason: PaneCancelReason,
    },
    Blur {
        target: Option<PaneResizeTarget>,
    },
}

/// Versioned semantic pane interaction event consumed by pane-core and emitted
/// by host adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSemanticInputEvent {
    #[serde(default = "default_pane_semantic_input_event_schema_version")]
    pub schema_version: u16,
    pub sequence: u64,
    #[serde(default)]
    pub modifiers: PaneModifierSnapshot,
    #[serde(flatten)]
    pub kind: PaneSemanticInputEventKind,
    #[serde(default)]
    pub extensions: BTreeMap<String, String>,
}

fn default_pane_semantic_input_event_schema_version() -> u16 {
    PANE_SEMANTIC_INPUT_EVENT_SCHEMA_VERSION
}

impl PaneSemanticInputEvent {
    /// Build a schema-versioned semantic pane input event.
    #[must_use]
    pub fn new(sequence: u64, kind: PaneSemanticInputEventKind) -> Self {
        Self {
            schema_version: PANE_SEMANTIC_INPUT_EVENT_SCHEMA_VERSION,
            sequence,
            modifiers: PaneModifierSnapshot::default(),
            kind,
            extensions: BTreeMap::new(),
        }
    }

    /// Validate event invariants required for deterministic replay.
    pub fn validate(&self) -> Result<(), PaneSemanticInputEventError> {
        if self.schema_version != PANE_SEMANTIC_INPUT_EVENT_SCHEMA_VERSION {
            return Err(PaneSemanticInputEventError::UnsupportedSchemaVersion {
                version: self.schema_version,
                expected: PANE_SEMANTIC_INPUT_EVENT_SCHEMA_VERSION,
            });
        }
        if self.sequence == 0 {
            return Err(PaneSemanticInputEventError::ZeroSequence);
        }

        match self.kind {
            PaneSemanticInputEventKind::PointerDown { pointer_id, .. }
            | PaneSemanticInputEventKind::PointerMove { pointer_id, .. }
            | PaneSemanticInputEventKind::PointerUp { pointer_id, .. } => {
                if pointer_id == 0 {
                    return Err(PaneSemanticInputEventError::ZeroPointerId);
                }
            }
            PaneSemanticInputEventKind::WheelNudge { lines, .. } => {
                if lines == 0 {
                    return Err(PaneSemanticInputEventError::ZeroWheelLines);
                }
            }
            PaneSemanticInputEventKind::KeyboardResize { units, .. } => {
                if units == 0 {
                    return Err(PaneSemanticInputEventError::ZeroResizeUnits);
                }
            }
            PaneSemanticInputEventKind::Cancel { .. } | PaneSemanticInputEventKind::Blur { .. } => {
            }
        }

        Ok(())
    }
}

/// Validation failures for semantic pane input events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneSemanticInputEventError {
    UnsupportedSchemaVersion { version: u16, expected: u16 },
    ZeroSequence,
    ZeroPointerId,
    ZeroWheelLines,
    ZeroResizeUnits,
}

impl fmt::Display for PaneSemanticInputEventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { version, expected } => write!(
                f,
                "unsupported pane semantic input schema version {version} (expected {expected})"
            ),
            Self::ZeroSequence => write!(f, "semantic pane input event sequence must be non-zero"),
            Self::ZeroPointerId => {
                write!(
                    f,
                    "semantic pane pointer events require non-zero pointer_id"
                )
            }
            Self::ZeroWheelLines => write!(f, "semantic pane wheel nudge must be non-zero"),
            Self::ZeroResizeUnits => {
                write!(f, "semantic pane keyboard resize units must be non-zero")
            }
        }
    }
}

impl std::error::Error for PaneSemanticInputEventError {}

/// Default move threshold (in coordinate units) for transitioning from
/// `Armed` to `Dragging`.
pub const PANE_DRAG_RESIZE_DEFAULT_THRESHOLD: u16 = 2;

/// Deterministic pane drag/resize lifecycle state.
///
/// ```text
/// Idle -> Armed -> Dragging -> Idle
///    \------> Idle (commit/cancel from Armed)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PaneDragResizeState {
    Idle,
    Armed {
        target: PaneResizeTarget,
        pointer_id: u32,
        origin: PanePointerPosition,
        current: PanePointerPosition,
        started_sequence: u64,
    },
    Dragging {
        target: PaneResizeTarget,
        pointer_id: u32,
        origin: PanePointerPosition,
        current: PanePointerPosition,
        started_sequence: u64,
        drag_started_sequence: u64,
    },
}

/// Explicit no-op diagnostics for lifecycle events that are safely ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneDragResizeNoopReason {
    IdleWithoutActiveDrag,
    ActiveDragAlreadyInProgress,
    PointerMismatch,
    TargetMismatch,
    ActiveStateDisallowsDiscreteInput,
    ThresholdNotReached,
}

/// Transition effect emitted by one lifecycle step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum PaneDragResizeEffect {
    Armed {
        target: PaneResizeTarget,
        pointer_id: u32,
        origin: PanePointerPosition,
    },
    DragStarted {
        target: PaneResizeTarget,
        pointer_id: u32,
        origin: PanePointerPosition,
        current: PanePointerPosition,
        total_delta_x: i32,
        total_delta_y: i32,
    },
    DragUpdated {
        target: PaneResizeTarget,
        pointer_id: u32,
        previous: PanePointerPosition,
        current: PanePointerPosition,
        delta_x: i32,
        delta_y: i32,
        total_delta_x: i32,
        total_delta_y: i32,
    },
    Committed {
        target: PaneResizeTarget,
        pointer_id: u32,
        origin: PanePointerPosition,
        end: PanePointerPosition,
        total_delta_x: i32,
        total_delta_y: i32,
    },
    Canceled {
        target: Option<PaneResizeTarget>,
        pointer_id: Option<u32>,
        reason: PaneCancelReason,
    },
    KeyboardApplied {
        target: PaneResizeTarget,
        direction: PaneResizeDirection,
        units: u16,
    },
    WheelApplied {
        target: PaneResizeTarget,
        lines: i16,
    },
    Noop {
        reason: PaneDragResizeNoopReason,
    },
}

/// One state-machine transition with deterministic telemetry fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneDragResizeTransition {
    pub transition_id: u64,
    pub sequence: u64,
    pub from: PaneDragResizeState,
    pub to: PaneDragResizeState,
    pub effect: PaneDragResizeEffect,
}

/// Runtime lifecycle machine for pane drag/resize interactions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneDragResizeMachine {
    state: PaneDragResizeState,
    drag_threshold: u16,
    transition_counter: u64,
}

impl Default for PaneDragResizeMachine {
    fn default() -> Self {
        Self {
            state: PaneDragResizeState::Idle,
            drag_threshold: PANE_DRAG_RESIZE_DEFAULT_THRESHOLD,
            transition_counter: 0,
        }
    }
}

impl PaneDragResizeMachine {
    /// Construct a drag/resize lifecycle machine with explicit threshold.
    pub fn new(drag_threshold: u16) -> Result<Self, PaneDragResizeMachineError> {
        if drag_threshold == 0 {
            return Err(PaneDragResizeMachineError::InvalidDragThreshold {
                threshold: drag_threshold,
            });
        }
        Ok(Self {
            state: PaneDragResizeState::Idle,
            drag_threshold,
            transition_counter: 0,
        })
    }

    /// Current lifecycle state.
    #[must_use]
    pub const fn state(&self) -> PaneDragResizeState {
        self.state
    }

    /// Configured drag-start threshold.
    #[must_use]
    pub const fn drag_threshold(&self) -> u16 {
        self.drag_threshold
    }

    /// Apply one semantic pane input event and emit deterministic transition
    /// diagnostics.
    pub fn apply_event(
        &mut self,
        event: &PaneSemanticInputEvent,
    ) -> Result<PaneDragResizeTransition, PaneDragResizeMachineError> {
        event
            .validate()
            .map_err(PaneDragResizeMachineError::InvalidEvent)?;

        let from = self.state;
        let effect = match (self.state, &event.kind) {
            (
                PaneDragResizeState::Idle,
                PaneSemanticInputEventKind::PointerDown {
                    target,
                    pointer_id,
                    position,
                    ..
                },
            ) => {
                self.state = PaneDragResizeState::Armed {
                    target: *target,
                    pointer_id: *pointer_id,
                    origin: *position,
                    current: *position,
                    started_sequence: event.sequence,
                };
                PaneDragResizeEffect::Armed {
                    target: *target,
                    pointer_id: *pointer_id,
                    origin: *position,
                }
            }
            (
                PaneDragResizeState::Idle,
                PaneSemanticInputEventKind::KeyboardResize {
                    target,
                    direction,
                    units,
                },
            ) => PaneDragResizeEffect::KeyboardApplied {
                target: *target,
                direction: *direction,
                units: *units,
            },
            (
                PaneDragResizeState::Idle,
                PaneSemanticInputEventKind::WheelNudge { target, lines },
            ) => PaneDragResizeEffect::WheelApplied {
                target: *target,
                lines: *lines,
            },
            (PaneDragResizeState::Idle, _) => PaneDragResizeEffect::Noop {
                reason: PaneDragResizeNoopReason::IdleWithoutActiveDrag,
            },
            (
                PaneDragResizeState::Armed {
                    target,
                    pointer_id,
                    origin,
                    current: _,
                    started_sequence,
                },
                PaneSemanticInputEventKind::PointerMove {
                    target: incoming_target,
                    pointer_id: incoming_pointer_id,
                    position,
                    ..
                },
            ) => {
                if *incoming_pointer_id != pointer_id {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::PointerMismatch,
                    }
                } else if *incoming_target != target {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::TargetMismatch,
                    }
                } else {
                    self.state = PaneDragResizeState::Armed {
                        target,
                        pointer_id,
                        origin,
                        current: *position,
                        started_sequence,
                    };
                    if crossed_drag_threshold(origin, *position, self.drag_threshold) {
                        self.state = PaneDragResizeState::Dragging {
                            target,
                            pointer_id,
                            origin,
                            current: *position,
                            started_sequence,
                            drag_started_sequence: event.sequence,
                        };
                        let (total_delta_x, total_delta_y) = delta(origin, *position);
                        PaneDragResizeEffect::DragStarted {
                            target,
                            pointer_id,
                            origin,
                            current: *position,
                            total_delta_x,
                            total_delta_y,
                        }
                    } else {
                        PaneDragResizeEffect::Noop {
                            reason: PaneDragResizeNoopReason::ThresholdNotReached,
                        }
                    }
                }
            }
            (
                PaneDragResizeState::Armed {
                    target,
                    pointer_id,
                    origin,
                    ..
                },
                PaneSemanticInputEventKind::PointerUp {
                    target: incoming_target,
                    pointer_id: incoming_pointer_id,
                    position,
                    ..
                },
            ) => {
                if *incoming_pointer_id != pointer_id {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::PointerMismatch,
                    }
                } else if *incoming_target != target {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::TargetMismatch,
                    }
                } else {
                    self.state = PaneDragResizeState::Idle;
                    let (total_delta_x, total_delta_y) = delta(origin, *position);
                    PaneDragResizeEffect::Committed {
                        target,
                        pointer_id,
                        origin,
                        end: *position,
                        total_delta_x,
                        total_delta_y,
                    }
                }
            }
            (
                PaneDragResizeState::Armed {
                    target, pointer_id, ..
                },
                PaneSemanticInputEventKind::Cancel {
                    target: incoming_target,
                    reason,
                },
            ) => {
                if !cancel_target_matches(target, *incoming_target) {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::TargetMismatch,
                    }
                } else {
                    self.state = PaneDragResizeState::Idle;
                    PaneDragResizeEffect::Canceled {
                        target: Some(target),
                        pointer_id: Some(pointer_id),
                        reason: *reason,
                    }
                }
            }
            (
                PaneDragResizeState::Armed {
                    target, pointer_id, ..
                },
                PaneSemanticInputEventKind::Blur {
                    target: incoming_target,
                },
            ) => {
                if !cancel_target_matches(target, *incoming_target) {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::TargetMismatch,
                    }
                } else {
                    self.state = PaneDragResizeState::Idle;
                    PaneDragResizeEffect::Canceled {
                        target: Some(target),
                        pointer_id: Some(pointer_id),
                        reason: PaneCancelReason::Blur,
                    }
                }
            }
            (PaneDragResizeState::Armed { .. }, PaneSemanticInputEventKind::PointerDown { .. }) => {
                PaneDragResizeEffect::Noop {
                    reason: PaneDragResizeNoopReason::ActiveDragAlreadyInProgress,
                }
            }
            (
                PaneDragResizeState::Armed { .. },
                PaneSemanticInputEventKind::KeyboardResize { .. }
                | PaneSemanticInputEventKind::WheelNudge { .. },
            ) => PaneDragResizeEffect::Noop {
                reason: PaneDragResizeNoopReason::ActiveStateDisallowsDiscreteInput,
            },
            (
                PaneDragResizeState::Dragging {
                    target,
                    pointer_id,
                    origin,
                    current,
                    started_sequence,
                    drag_started_sequence,
                },
                PaneSemanticInputEventKind::PointerMove {
                    target: incoming_target,
                    pointer_id: incoming_pointer_id,
                    position,
                    ..
                },
            ) => {
                if *incoming_pointer_id != pointer_id {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::PointerMismatch,
                    }
                } else if *incoming_target != target {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::TargetMismatch,
                    }
                } else {
                    let previous = current;
                    let (delta_x, delta_y) = delta(previous, *position);
                    let (total_delta_x, total_delta_y) = delta(origin, *position);
                    self.state = PaneDragResizeState::Dragging {
                        target,
                        pointer_id,
                        origin,
                        current: *position,
                        started_sequence,
                        drag_started_sequence,
                    };
                    PaneDragResizeEffect::DragUpdated {
                        target,
                        pointer_id,
                        previous,
                        current: *position,
                        delta_x,
                        delta_y,
                        total_delta_x,
                        total_delta_y,
                    }
                }
            }
            (
                PaneDragResizeState::Dragging {
                    target,
                    pointer_id,
                    origin,
                    ..
                },
                PaneSemanticInputEventKind::PointerUp {
                    target: incoming_target,
                    pointer_id: incoming_pointer_id,
                    position,
                    ..
                },
            ) => {
                if *incoming_pointer_id != pointer_id {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::PointerMismatch,
                    }
                } else if *incoming_target != target {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::TargetMismatch,
                    }
                } else {
                    self.state = PaneDragResizeState::Idle;
                    let (total_delta_x, total_delta_y) = delta(origin, *position);
                    PaneDragResizeEffect::Committed {
                        target,
                        pointer_id,
                        origin,
                        end: *position,
                        total_delta_x,
                        total_delta_y,
                    }
                }
            }
            (
                PaneDragResizeState::Dragging {
                    target, pointer_id, ..
                },
                PaneSemanticInputEventKind::Cancel {
                    target: incoming_target,
                    reason,
                },
            ) => {
                if !cancel_target_matches(target, *incoming_target) {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::TargetMismatch,
                    }
                } else {
                    self.state = PaneDragResizeState::Idle;
                    PaneDragResizeEffect::Canceled {
                        target: Some(target),
                        pointer_id: Some(pointer_id),
                        reason: *reason,
                    }
                }
            }
            (
                PaneDragResizeState::Dragging {
                    target, pointer_id, ..
                },
                PaneSemanticInputEventKind::Blur {
                    target: incoming_target,
                },
            ) => {
                if !cancel_target_matches(target, *incoming_target) {
                    PaneDragResizeEffect::Noop {
                        reason: PaneDragResizeNoopReason::TargetMismatch,
                    }
                } else {
                    self.state = PaneDragResizeState::Idle;
                    PaneDragResizeEffect::Canceled {
                        target: Some(target),
                        pointer_id: Some(pointer_id),
                        reason: PaneCancelReason::Blur,
                    }
                }
            }
            (
                PaneDragResizeState::Dragging { .. },
                PaneSemanticInputEventKind::PointerDown { .. },
            ) => PaneDragResizeEffect::Noop {
                reason: PaneDragResizeNoopReason::ActiveDragAlreadyInProgress,
            },
            (
                PaneDragResizeState::Dragging { .. },
                PaneSemanticInputEventKind::KeyboardResize { .. }
                | PaneSemanticInputEventKind::WheelNudge { .. },
            ) => PaneDragResizeEffect::Noop {
                reason: PaneDragResizeNoopReason::ActiveStateDisallowsDiscreteInput,
            },
        };

        self.transition_counter = self.transition_counter.saturating_add(1);
        Ok(PaneDragResizeTransition {
            transition_id: self.transition_counter,
            sequence: event.sequence,
            from,
            to: self.state,
            effect,
        })
    }
}

/// Lifecycle machine configuration/runtime errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneDragResizeMachineError {
    InvalidDragThreshold { threshold: u16 },
    InvalidEvent(PaneSemanticInputEventError),
}

impl fmt::Display for PaneDragResizeMachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDragThreshold { threshold } => {
                write!(f, "drag threshold must be > 0 (got {threshold})")
            }
            Self::InvalidEvent(error) => write!(f, "invalid semantic pane input event: {error}"),
        }
    }
}

impl std::error::Error for PaneDragResizeMachineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::InvalidEvent(error) = self {
            return Some(error);
        }
        None
    }
}

fn delta(origin: PanePointerPosition, current: PanePointerPosition) -> (i32, i32) {
    (current.x - origin.x, current.y - origin.y)
}

fn crossed_drag_threshold(
    origin: PanePointerPosition,
    current: PanePointerPosition,
    threshold: u16,
) -> bool {
    let (dx, dy) = delta(origin, current);
    let threshold = i64::from(threshold);
    let squared_distance = i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy);
    squared_distance >= threshold * threshold
}

fn cancel_target_matches(active: PaneResizeTarget, incoming: Option<PaneResizeTarget>) -> bool {
    incoming.is_none() || incoming == Some(active)
}

/// Supported structural pane operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PaneOperation {
    /// Split an existing leaf by wrapping it with a new split parent and adding
    /// one new sibling leaf.
    SplitLeaf {
        target: PaneId,
        axis: SplitAxis,
        ratio: PaneSplitRatio,
        placement: PanePlacement,
        new_leaf: PaneLeaf,
    },
    /// Close a non-root pane (leaf or subtree) and promote its sibling.
    CloseNode { target: PaneId },
    /// Move an existing subtree next to a target node by wrapping the target in
    /// a new split with the source subtree.
    MoveSubtree {
        source: PaneId,
        target: PaneId,
        axis: SplitAxis,
        ratio: PaneSplitRatio,
        placement: PanePlacement,
    },
    /// Swap two non-ancestor subtrees.
    SwapNodes { first: PaneId, second: PaneId },
    /// Canonicalize all split ratios to reduced form and validate positivity.
    NormalizeRatios,
}

impl PaneOperation {
    /// Operation family.
    #[must_use]
    pub const fn kind(&self) -> PaneOperationKind {
        match self {
            Self::SplitLeaf { .. } => PaneOperationKind::SplitLeaf,
            Self::CloseNode { .. } => PaneOperationKind::CloseNode,
            Self::MoveSubtree { .. } => PaneOperationKind::MoveSubtree,
            Self::SwapNodes { .. } => PaneOperationKind::SwapNodes,
            Self::NormalizeRatios => PaneOperationKind::NormalizeRatios,
        }
    }

    #[must_use]
    fn referenced_nodes(&self) -> Vec<PaneId> {
        match self {
            Self::SplitLeaf { target, .. } | Self::CloseNode { target } => vec![*target],
            Self::MoveSubtree { source, target, .. }
            | Self::SwapNodes {
                first: source,
                second: target,
            } => {
                vec![*source, *target]
            }
            Self::NormalizeRatios => Vec::new(),
        }
    }
}

/// Stable operation discriminator used in logs and telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneOperationKind {
    SplitLeaf,
    CloseNode,
    MoveSubtree,
    SwapNodes,
    NormalizeRatios,
}

/// Successful transactional operation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneOperationOutcome {
    pub operation_id: u64,
    pub kind: PaneOperationKind,
    pub touched_nodes: Vec<PaneId>,
    pub before_hash: u64,
    pub after_hash: u64,
}

/// Failure payload for transactional operation APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneOperationError {
    pub operation_id: u64,
    pub kind: PaneOperationKind,
    pub touched_nodes: Vec<PaneId>,
    pub before_hash: u64,
    pub after_hash: u64,
    pub reason: PaneOperationFailure,
}

/// Structured reasons for pane operation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneOperationFailure {
    MissingNode {
        node_id: PaneId,
    },
    NodeNotLeaf {
        node_id: PaneId,
    },
    ParentNotSplit {
        node_id: PaneId,
    },
    ParentChildMismatch {
        parent: PaneId,
        child: PaneId,
    },
    CannotCloseRoot {
        node_id: PaneId,
    },
    CannotMoveRoot {
        node_id: PaneId,
    },
    SameNode {
        first: PaneId,
        second: PaneId,
    },
    AncestorConflict {
        ancestor: PaneId,
        descendant: PaneId,
    },
    TargetRemovedByDetach {
        target: PaneId,
        detached_parent: PaneId,
    },
    PaneIdOverflow {
        current: PaneId,
    },
    InvalidRatio {
        node_id: PaneId,
        numerator: u32,
        denominator: u32,
    },
    Validation(PaneModelError),
}

impl fmt::Display for PaneOperationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNode { node_id } => write!(f, "node {} not found", node_id.0),
            Self::NodeNotLeaf { node_id } => write!(f, "node {} is not a leaf", node_id.0),
            Self::ParentNotSplit { node_id } => {
                write!(f, "node {} is not a split parent", node_id.0)
            }
            Self::ParentChildMismatch { parent, child } => write!(
                f,
                "split parent {} does not reference child {}",
                parent.0, child.0
            ),
            Self::CannotCloseRoot { node_id } => {
                write!(f, "cannot close root node {}", node_id.0)
            }
            Self::CannotMoveRoot { node_id } => {
                write!(f, "cannot move root node {}", node_id.0)
            }
            Self::SameNode { first, second } => write!(
                f,
                "operation requires distinct nodes, got {} and {}",
                first.0, second.0
            ),
            Self::AncestorConflict {
                ancestor,
                descendant,
            } => write!(
                f,
                "operation would create cycle: node {} is an ancestor of {}",
                ancestor.0, descendant.0
            ),
            Self::TargetRemovedByDetach {
                target,
                detached_parent,
            } => write!(
                f,
                "target {} would be removed while detaching parent {}",
                target.0, detached_parent.0
            ),
            Self::PaneIdOverflow { current } => {
                write!(f, "pane id overflow after {}", current.0)
            }
            Self::InvalidRatio {
                node_id,
                numerator,
                denominator,
            } => write!(
                f,
                "split node {} has invalid ratio {numerator}/{denominator}",
                node_id.0
            ),
            Self::Validation(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for PaneOperationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Validation(err) = self {
            return Some(err);
        }
        None
    }
}

impl fmt::Display for PaneOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pane op {} ({:?}) failed: {} [nodes={:?}, before_hash={:#x}, after_hash={:#x}]",
            self.operation_id,
            self.kind,
            self.reason,
            self.touched_nodes
                .iter()
                .map(|node_id| node_id.0)
                .collect::<Vec<_>>(),
            self.before_hash,
            self.after_hash
        )
    }
}

impl std::error::Error for PaneOperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.reason)
    }
}

/// One deterministic operation journal row emitted by a transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOperationJournalEntry {
    pub transaction_id: u64,
    pub sequence: u64,
    pub operation_id: u64,
    pub operation: PaneOperation,
    pub kind: PaneOperationKind,
    pub touched_nodes: Vec<PaneId>,
    pub before_hash: u64,
    pub after_hash: u64,
    pub result: PaneOperationJournalResult,
}

/// Journal result state for one attempted operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PaneOperationJournalResult {
    Applied,
    Rejected { reason: String },
}

/// Finalized transaction payload emitted by commit/rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneTransactionOutcome {
    pub transaction_id: u64,
    pub committed: bool,
    pub tree: PaneTree,
    pub journal: Vec<PaneOperationJournalEntry>,
}

/// Transaction boundary wrapper for pane mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneTransaction {
    transaction_id: u64,
    sequence: u64,
    base_tree: PaneTree,
    working_tree: PaneTree,
    journal: Vec<PaneOperationJournalEntry>,
}

impl PaneTransaction {
    fn new(transaction_id: u64, base_tree: PaneTree) -> Self {
        Self {
            transaction_id,
            sequence: 1,
            base_tree: base_tree.clone(),
            working_tree: base_tree,
            journal: Vec::new(),
        }
    }

    /// Transaction identifier supplied by the caller.
    #[must_use]
    pub const fn transaction_id(&self) -> u64 {
        self.transaction_id
    }

    /// Current mutable working tree for read-only inspection.
    #[must_use]
    pub fn tree(&self) -> &PaneTree {
        &self.working_tree
    }

    /// Journal entries in deterministic insertion order.
    #[must_use]
    pub fn journal(&self) -> &[PaneOperationJournalEntry] {
        &self.journal
    }

    /// Attempt one operation against the transaction working tree.
    ///
    /// Every attempt is journaled, including rejected operations.
    pub fn apply_operation(
        &mut self,
        operation_id: u64,
        operation: PaneOperation,
    ) -> Result<PaneOperationOutcome, PaneOperationError> {
        let operation_for_journal = operation.clone();
        let kind = operation_for_journal.kind();
        let sequence = self.next_sequence();

        match self.working_tree.apply_operation(operation_id, operation) {
            Ok(outcome) => {
                self.journal.push(PaneOperationJournalEntry {
                    transaction_id: self.transaction_id,
                    sequence,
                    operation_id,
                    operation: operation_for_journal,
                    kind,
                    touched_nodes: outcome.touched_nodes.clone(),
                    before_hash: outcome.before_hash,
                    after_hash: outcome.after_hash,
                    result: PaneOperationJournalResult::Applied,
                });
                Ok(outcome)
            }
            Err(err) => {
                self.journal.push(PaneOperationJournalEntry {
                    transaction_id: self.transaction_id,
                    sequence,
                    operation_id,
                    operation: operation_for_journal,
                    kind,
                    touched_nodes: err.touched_nodes.clone(),
                    before_hash: err.before_hash,
                    after_hash: err.after_hash,
                    result: PaneOperationJournalResult::Rejected {
                        reason: err.reason.to_string(),
                    },
                });
                Err(err)
            }
        }
    }

    /// Finalize and keep all successful mutations.
    #[must_use]
    pub fn commit(self) -> PaneTransactionOutcome {
        PaneTransactionOutcome {
            transaction_id: self.transaction_id,
            committed: true,
            tree: self.working_tree,
            journal: self.journal,
        }
    }

    /// Finalize and discard all mutations.
    #[must_use]
    pub fn rollback(self) -> PaneTransactionOutcome {
        PaneTransactionOutcome {
            transaction_id: self.transaction_id,
            committed: false,
            tree: self.base_tree,
            journal: self.journal,
        }
    }

    fn next_sequence(&mut self) -> u64 {
        let sequence = self.sequence;
        self.sequence = self.sequence.saturating_add(1);
        sequence
    }
}

/// Validated pane tree model for runtime usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneTree {
    schema_version: u16,
    root: PaneId,
    next_id: PaneId,
    nodes: BTreeMap<PaneId, PaneNodeRecord>,
    extensions: BTreeMap<String, String>,
}

impl PaneTree {
    /// Build a singleton tree with one root leaf.
    #[must_use]
    pub fn singleton(surface_key: impl Into<String>) -> Self {
        let root = PaneId::MIN;
        let mut nodes = BTreeMap::new();
        let _ = nodes.insert(
            root,
            PaneNodeRecord::leaf(root, None, PaneLeaf::new(surface_key)),
        );
        Self {
            schema_version: PANE_TREE_SCHEMA_VERSION,
            root,
            next_id: root.checked_next().unwrap_or(root),
            nodes,
            extensions: BTreeMap::new(),
        }
    }

    /// Construct and validate from a serial snapshot.
    pub fn from_snapshot(mut snapshot: PaneTreeSnapshot) -> Result<Self, PaneModelError> {
        if snapshot.schema_version != PANE_TREE_SCHEMA_VERSION {
            return Err(PaneModelError::UnsupportedSchemaVersion {
                version: snapshot.schema_version,
            });
        }
        snapshot.canonicalize();
        let mut nodes = BTreeMap::new();
        for node in snapshot.nodes {
            let node_id = node.id;
            if nodes.insert(node_id, node).is_some() {
                return Err(PaneModelError::DuplicateNodeId { node_id });
            }
        }
        validate_tree(snapshot.root, snapshot.next_id, &nodes)?;
        Ok(Self {
            schema_version: snapshot.schema_version,
            root: snapshot.root,
            next_id: snapshot.next_id,
            nodes,
            extensions: snapshot.extensions,
        })
    }

    /// Export to canonical snapshot form.
    #[must_use]
    pub fn to_snapshot(&self) -> PaneTreeSnapshot {
        let mut snapshot = PaneTreeSnapshot {
            schema_version: self.schema_version,
            root: self.root,
            next_id: self.next_id,
            nodes: self.nodes.values().cloned().collect(),
            extensions: self.extensions.clone(),
        };
        snapshot.canonicalize();
        snapshot
    }

    /// Root node ID.
    #[must_use]
    pub const fn root(&self) -> PaneId {
        self.root
    }

    /// Next deterministic ID value.
    #[must_use]
    pub const fn next_id(&self) -> PaneId {
        self.next_id
    }

    /// Current schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Lookup a node by ID.
    #[must_use]
    pub fn node(&self, id: PaneId) -> Option<&PaneNodeRecord> {
        self.nodes.get(&id)
    }

    /// Iterate nodes in canonical ID order.
    pub fn nodes(&self) -> impl Iterator<Item = &PaneNodeRecord> {
        self.nodes.values()
    }

    /// Validate internal invariants.
    pub fn validate(&self) -> Result<(), PaneModelError> {
        validate_tree(self.root, self.next_id, &self.nodes)
    }

    /// Structured invariant diagnostics for the current tree snapshot.
    #[must_use]
    pub fn invariant_report(&self) -> PaneInvariantReport {
        self.to_snapshot().invariant_report()
    }

    /// Deterministic structural hash of the current tree state.
    ///
    /// This is intended for operation logs and replay diagnostics.
    #[must_use]
    pub fn state_hash(&self) -> u64 {
        const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0001_0000_01b3;

        fn mix(hash: &mut u64, byte: u8) {
            *hash ^= u64::from(byte);
            *hash = hash.wrapping_mul(PRIME);
        }

        fn mix_bytes(hash: &mut u64, bytes: &[u8]) {
            for byte in bytes {
                mix(hash, *byte);
            }
        }

        fn mix_u16(hash: &mut u64, value: u16) {
            mix_bytes(hash, &value.to_le_bytes());
        }

        fn mix_u32(hash: &mut u64, value: u32) {
            mix_bytes(hash, &value.to_le_bytes());
        }

        fn mix_u64(hash: &mut u64, value: u64) {
            mix_bytes(hash, &value.to_le_bytes());
        }

        fn mix_bool(hash: &mut u64, value: bool) {
            mix(hash, u8::from(value));
        }

        fn mix_opt_u16(hash: &mut u64, value: Option<u16>) {
            match value {
                Some(value) => {
                    mix(hash, 1);
                    mix_u16(hash, value);
                }
                None => mix(hash, 0),
            }
        }

        fn mix_opt_pane_id(hash: &mut u64, value: Option<PaneId>) {
            match value {
                Some(value) => {
                    mix(hash, 1);
                    mix_u64(hash, value.get());
                }
                None => mix(hash, 0),
            }
        }

        fn mix_str(hash: &mut u64, value: &str) {
            mix_u64(hash, value.len() as u64);
            mix_bytes(hash, value.as_bytes());
        }

        fn mix_extensions(hash: &mut u64, extensions: &BTreeMap<String, String>) {
            mix_u64(hash, extensions.len() as u64);
            for (key, value) in extensions {
                mix_str(hash, key);
                mix_str(hash, value);
            }
        }

        fn mix_constraints(hash: &mut u64, constraints: PaneConstraints) {
            mix_u16(hash, constraints.min_width);
            mix_u16(hash, constraints.min_height);
            mix_opt_u16(hash, constraints.max_width);
            mix_opt_u16(hash, constraints.max_height);
            mix_bool(hash, constraints.collapsible);
        }

        let mut hash = OFFSET_BASIS;
        mix_u16(&mut hash, self.schema_version);
        mix_u64(&mut hash, self.root.get());
        mix_u64(&mut hash, self.next_id.get());
        mix_extensions(&mut hash, &self.extensions);
        mix_u64(&mut hash, self.nodes.len() as u64);

        for node in self.nodes.values() {
            mix_u64(&mut hash, node.id.get());
            mix_opt_pane_id(&mut hash, node.parent);
            mix_constraints(&mut hash, node.constraints);
            mix_extensions(&mut hash, &node.extensions);

            match &node.kind {
                PaneNodeKind::Leaf(leaf) => {
                    mix(&mut hash, 1);
                    mix_str(&mut hash, &leaf.surface_key);
                    mix_extensions(&mut hash, &leaf.extensions);
                }
                PaneNodeKind::Split(split) => {
                    mix(&mut hash, 2);
                    let axis_byte = match split.axis {
                        SplitAxis::Horizontal => 1,
                        SplitAxis::Vertical => 2,
                    };
                    mix(&mut hash, axis_byte);
                    mix_u32(&mut hash, split.ratio.numerator());
                    mix_u32(&mut hash, split.ratio.denominator());
                    mix_u64(&mut hash, split.first.get());
                    mix_u64(&mut hash, split.second.get());
                }
            }
        }

        hash
    }

    /// Start a transaction boundary for one or more structural operations.
    ///
    /// Transactions stage mutations on a cloned working tree and provide a
    /// deterministic operation journal for replay, undo/redo, and auditing.
    #[must_use]
    pub fn begin_transaction(&self, transaction_id: u64) -> PaneTransaction {
        PaneTransaction::new(transaction_id, self.clone())
    }

    /// Apply one structural operation atomically.
    ///
    /// The operation is executed on a cloned working tree. On success, the
    /// mutated clone replaces `self`; on failure, `self` is unchanged.
    pub fn apply_operation(
        &mut self,
        operation_id: u64,
        operation: PaneOperation,
    ) -> Result<PaneOperationOutcome, PaneOperationError> {
        let kind = operation.kind();
        let before_hash = self.state_hash();
        let mut working = self.clone();
        let mut touched = operation
            .referenced_nodes()
            .into_iter()
            .collect::<BTreeSet<_>>();

        if let Err(reason) = working.apply_operation_inner(operation, &mut touched) {
            return Err(PaneOperationError {
                operation_id,
                kind,
                touched_nodes: touched.into_iter().collect(),
                before_hash,
                after_hash: working.state_hash(),
                reason,
            });
        }

        if let Err(err) = working.validate() {
            return Err(PaneOperationError {
                operation_id,
                kind,
                touched_nodes: touched.into_iter().collect(),
                before_hash,
                after_hash: working.state_hash(),
                reason: PaneOperationFailure::Validation(err),
            });
        }

        let after_hash = working.state_hash();
        *self = working;

        Ok(PaneOperationOutcome {
            operation_id,
            kind,
            touched_nodes: touched.into_iter().collect(),
            before_hash,
            after_hash,
        })
    }

    fn apply_operation_inner(
        &mut self,
        operation: PaneOperation,
        touched: &mut BTreeSet<PaneId>,
    ) -> Result<(), PaneOperationFailure> {
        match operation {
            PaneOperation::SplitLeaf {
                target,
                axis,
                ratio,
                placement,
                new_leaf,
            } => self.apply_split_leaf(target, axis, ratio, placement, new_leaf, touched),
            PaneOperation::CloseNode { target } => self.apply_close_node(target, touched),
            PaneOperation::MoveSubtree {
                source,
                target,
                axis,
                ratio,
                placement,
            } => self.apply_move_subtree(source, target, axis, ratio, placement, touched),
            PaneOperation::SwapNodes { first, second } => {
                self.apply_swap_nodes(first, second, touched)
            }
            PaneOperation::NormalizeRatios => self.apply_normalize_ratios(touched),
        }
    }

    fn apply_split_leaf(
        &mut self,
        target: PaneId,
        axis: SplitAxis,
        ratio: PaneSplitRatio,
        placement: PanePlacement,
        new_leaf: PaneLeaf,
        touched: &mut BTreeSet<PaneId>,
    ) -> Result<(), PaneOperationFailure> {
        let target_parent = match self.nodes.get(&target) {
            Some(PaneNodeRecord {
                parent,
                kind: PaneNodeKind::Leaf(_),
                ..
            }) => *parent,
            Some(_) => {
                return Err(PaneOperationFailure::NodeNotLeaf { node_id: target });
            }
            None => {
                return Err(PaneOperationFailure::MissingNode { node_id: target });
            }
        };

        let split_id = self.allocate_node_id()?;
        let new_leaf_id = self.allocate_node_id()?;
        touched.extend([target, split_id, new_leaf_id]);
        if let Some(parent_id) = target_parent {
            let _ = touched.insert(parent_id);
        }

        let (first, second) = placement.ordered(target, new_leaf_id);
        let split_record = PaneNodeRecord::split(
            split_id,
            target_parent,
            PaneSplit {
                axis,
                ratio,
                first,
                second,
            },
        );

        if let Some(target_node) = self.nodes.get_mut(&target) {
            target_node.parent = Some(split_id);
        }
        let _ = self.nodes.insert(
            new_leaf_id,
            PaneNodeRecord::leaf(new_leaf_id, Some(split_id), new_leaf),
        );
        let _ = self.nodes.insert(split_id, split_record);

        if let Some(parent_id) = target_parent {
            self.replace_child(parent_id, target, split_id)?;
        } else {
            self.root = split_id;
        }

        Ok(())
    }

    fn apply_close_node(
        &mut self,
        target: PaneId,
        touched: &mut BTreeSet<PaneId>,
    ) -> Result<(), PaneOperationFailure> {
        if !self.nodes.contains_key(&target) {
            return Err(PaneOperationFailure::MissingNode { node_id: target });
        }
        if target == self.root {
            return Err(PaneOperationFailure::CannotCloseRoot { node_id: target });
        }

        let subtree_ids = self.collect_subtree_ids(target)?;
        for node_id in &subtree_ids {
            let _ = touched.insert(*node_id);
        }

        let (parent_id, sibling_id, grandparent_id) =
            self.promote_sibling_after_detach(target, touched)?;
        let _ = touched.insert(parent_id);
        let _ = touched.insert(sibling_id);
        if let Some(grandparent_id) = grandparent_id {
            let _ = touched.insert(grandparent_id);
        }

        for node_id in subtree_ids {
            let _ = self.nodes.remove(&node_id);
        }

        Ok(())
    }

    fn apply_move_subtree(
        &mut self,
        source: PaneId,
        target: PaneId,
        axis: SplitAxis,
        ratio: PaneSplitRatio,
        placement: PanePlacement,
        touched: &mut BTreeSet<PaneId>,
    ) -> Result<(), PaneOperationFailure> {
        if source == target {
            return Err(PaneOperationFailure::SameNode {
                first: source,
                second: target,
            });
        }

        if !self.nodes.contains_key(&source) {
            return Err(PaneOperationFailure::MissingNode { node_id: source });
        }
        if !self.nodes.contains_key(&target) {
            return Err(PaneOperationFailure::MissingNode { node_id: target });
        }

        if source == self.root {
            return Err(PaneOperationFailure::CannotMoveRoot { node_id: source });
        }
        if self.is_ancestor(source, target)? {
            return Err(PaneOperationFailure::AncestorConflict {
                ancestor: source,
                descendant: target,
            });
        }

        let source_parent = self
            .nodes
            .get(&source)
            .and_then(|node| node.parent)
            .ok_or(PaneOperationFailure::CannotMoveRoot { node_id: source })?;
        if source_parent == target {
            return Err(PaneOperationFailure::TargetRemovedByDetach {
                target,
                detached_parent: source_parent,
            });
        }

        let _ = touched.insert(source);
        let _ = touched.insert(target);
        let _ = touched.insert(source_parent);

        let (removed_parent, sibling_id, grandparent_id) =
            self.promote_sibling_after_detach(source, touched)?;
        let _ = touched.insert(removed_parent);
        let _ = touched.insert(sibling_id);
        if let Some(grandparent_id) = grandparent_id {
            let _ = touched.insert(grandparent_id);
        }

        if let Some(source_node) = self.nodes.get_mut(&source) {
            source_node.parent = None;
        }

        if !self.nodes.contains_key(&target) {
            return Err(PaneOperationFailure::MissingNode { node_id: target });
        }
        let target_parent = self.nodes.get(&target).and_then(|node| node.parent);
        if let Some(parent_id) = target_parent {
            let _ = touched.insert(parent_id);
        }

        let split_id = self.allocate_node_id()?;
        let _ = touched.insert(split_id);
        let (first, second) = placement.ordered(target, source);

        if let Some(target_node) = self.nodes.get_mut(&target) {
            target_node.parent = Some(split_id);
        }
        if let Some(source_node) = self.nodes.get_mut(&source) {
            source_node.parent = Some(split_id);
        }

        let _ = self.nodes.insert(
            split_id,
            PaneNodeRecord::split(
                split_id,
                target_parent,
                PaneSplit {
                    axis,
                    ratio,
                    first,
                    second,
                },
            ),
        );

        if let Some(parent_id) = target_parent {
            self.replace_child(parent_id, target, split_id)?;
        } else {
            self.root = split_id;
        }

        Ok(())
    }

    fn apply_swap_nodes(
        &mut self,
        first: PaneId,
        second: PaneId,
        touched: &mut BTreeSet<PaneId>,
    ) -> Result<(), PaneOperationFailure> {
        if first == second {
            return Ok(());
        }

        if !self.nodes.contains_key(&first) {
            return Err(PaneOperationFailure::MissingNode { node_id: first });
        }
        if !self.nodes.contains_key(&second) {
            return Err(PaneOperationFailure::MissingNode { node_id: second });
        }
        if self.is_ancestor(first, second)? {
            return Err(PaneOperationFailure::AncestorConflict {
                ancestor: first,
                descendant: second,
            });
        }
        if self.is_ancestor(second, first)? {
            return Err(PaneOperationFailure::AncestorConflict {
                ancestor: second,
                descendant: first,
            });
        }

        let _ = touched.insert(first);
        let _ = touched.insert(second);

        let first_parent = self.nodes.get(&first).and_then(|node| node.parent);
        let second_parent = self.nodes.get(&second).and_then(|node| node.parent);

        if first_parent == second_parent {
            if let Some(parent_id) = first_parent {
                let _ = touched.insert(parent_id);
                self.swap_children(parent_id, first, second)?;
            }
            return Ok(());
        }

        match (first_parent, second_parent) {
            (Some(left_parent), Some(right_parent)) => {
                let _ = touched.insert(left_parent);
                let _ = touched.insert(right_parent);
                self.replace_child(left_parent, first, second)?;
                self.replace_child(right_parent, second, first)?;
                if let Some(left) = self.nodes.get_mut(&first) {
                    left.parent = Some(right_parent);
                }
                if let Some(right) = self.nodes.get_mut(&second) {
                    right.parent = Some(left_parent);
                }
            }
            (None, Some(parent_id)) => {
                let _ = touched.insert(parent_id);
                self.replace_child(parent_id, second, first)?;
                if let Some(first_node) = self.nodes.get_mut(&first) {
                    first_node.parent = Some(parent_id);
                }
                if let Some(second_node) = self.nodes.get_mut(&second) {
                    second_node.parent = None;
                }
                self.root = second;
            }
            (Some(parent_id), None) => {
                let _ = touched.insert(parent_id);
                self.replace_child(parent_id, first, second)?;
                if let Some(first_node) = self.nodes.get_mut(&first) {
                    first_node.parent = None;
                }
                if let Some(second_node) = self.nodes.get_mut(&second) {
                    second_node.parent = Some(parent_id);
                }
                self.root = first;
            }
            (None, None) => {}
        }

        Ok(())
    }

    fn apply_normalize_ratios(
        &mut self,
        touched: &mut BTreeSet<PaneId>,
    ) -> Result<(), PaneOperationFailure> {
        for node in self.nodes.values_mut() {
            if let PaneNodeKind::Split(split) = &mut node.kind {
                let normalized =
                    PaneSplitRatio::new(split.ratio.numerator(), split.ratio.denominator())
                        .map_err(|_| PaneOperationFailure::InvalidRatio {
                            node_id: node.id,
                            numerator: split.ratio.numerator(),
                            denominator: split.ratio.denominator(),
                        })?;
                split.ratio = normalized;
                let _ = touched.insert(node.id);
            }
        }
        Ok(())
    }

    fn replace_child(
        &mut self,
        parent_id: PaneId,
        old_child: PaneId,
        new_child: PaneId,
    ) -> Result<(), PaneOperationFailure> {
        let parent = self
            .nodes
            .get_mut(&parent_id)
            .ok_or(PaneOperationFailure::MissingNode { node_id: parent_id })?;
        let PaneNodeKind::Split(split) = &mut parent.kind else {
            return Err(PaneOperationFailure::ParentNotSplit { node_id: parent_id });
        };

        if split.first == old_child {
            split.first = new_child;
            return Ok(());
        }
        if split.second == old_child {
            split.second = new_child;
            return Ok(());
        }

        Err(PaneOperationFailure::ParentChildMismatch {
            parent: parent_id,
            child: old_child,
        })
    }

    fn swap_children(
        &mut self,
        parent_id: PaneId,
        left: PaneId,
        right: PaneId,
    ) -> Result<(), PaneOperationFailure> {
        let parent = self
            .nodes
            .get_mut(&parent_id)
            .ok_or(PaneOperationFailure::MissingNode { node_id: parent_id })?;
        let PaneNodeKind::Split(split) = &mut parent.kind else {
            return Err(PaneOperationFailure::ParentNotSplit { node_id: parent_id });
        };

        let has_pair = (split.first == left && split.second == right)
            || (split.first == right && split.second == left);
        if !has_pair {
            return Err(PaneOperationFailure::ParentChildMismatch {
                parent: parent_id,
                child: left,
            });
        }

        std::mem::swap(&mut split.first, &mut split.second);
        Ok(())
    }

    fn promote_sibling_after_detach(
        &mut self,
        detached: PaneId,
        touched: &mut BTreeSet<PaneId>,
    ) -> Result<(PaneId, PaneId, Option<PaneId>), PaneOperationFailure> {
        let parent_id = self
            .nodes
            .get(&detached)
            .ok_or(PaneOperationFailure::MissingNode { node_id: detached })?
            .parent
            .ok_or(PaneOperationFailure::CannotMoveRoot { node_id: detached })?;
        let parent_node = self
            .nodes
            .get(&parent_id)
            .ok_or(PaneOperationFailure::MissingNode { node_id: parent_id })?;
        let PaneNodeKind::Split(parent_split) = &parent_node.kind else {
            return Err(PaneOperationFailure::ParentNotSplit { node_id: parent_id });
        };

        let sibling_id = if parent_split.first == detached {
            parent_split.second
        } else if parent_split.second == detached {
            parent_split.first
        } else {
            return Err(PaneOperationFailure::ParentChildMismatch {
                parent: parent_id,
                child: detached,
            });
        };

        let grandparent_id = parent_node.parent;
        let _ = touched.insert(parent_id);
        let _ = touched.insert(sibling_id);
        if let Some(grandparent_id) = grandparent_id {
            let _ = touched.insert(grandparent_id);
            self.replace_child(grandparent_id, parent_id, sibling_id)?;
        } else {
            self.root = sibling_id;
        }

        let sibling_node =
            self.nodes
                .get_mut(&sibling_id)
                .ok_or(PaneOperationFailure::MissingNode {
                    node_id: sibling_id,
                })?;
        sibling_node.parent = grandparent_id;
        let _ = self.nodes.remove(&parent_id);

        Ok((parent_id, sibling_id, grandparent_id))
    }

    fn is_ancestor(
        &self,
        ancestor: PaneId,
        mut node_id: PaneId,
    ) -> Result<bool, PaneOperationFailure> {
        loop {
            let node = self
                .nodes
                .get(&node_id)
                .ok_or(PaneOperationFailure::MissingNode { node_id })?;
            let Some(parent_id) = node.parent else {
                return Ok(false);
            };
            if parent_id == ancestor {
                return Ok(true);
            }
            node_id = parent_id;
        }
    }

    fn collect_subtree_ids(&self, root_id: PaneId) -> Result<Vec<PaneId>, PaneOperationFailure> {
        if !self.nodes.contains_key(&root_id) {
            return Err(PaneOperationFailure::MissingNode { node_id: root_id });
        }

        let mut out = Vec::new();
        let mut stack = vec![root_id];
        while let Some(node_id) = stack.pop() {
            let node = self
                .nodes
                .get(&node_id)
                .ok_or(PaneOperationFailure::MissingNode { node_id })?;
            out.push(node_id);
            if let PaneNodeKind::Split(split) = &node.kind {
                stack.push(split.first);
                stack.push(split.second);
            }
        }
        Ok(out)
    }

    fn allocate_node_id(&mut self) -> Result<PaneId, PaneOperationFailure> {
        let current = self.next_id;
        self.next_id = self
            .next_id
            .checked_next()
            .map_err(|_| PaneOperationFailure::PaneIdOverflow { current })?;
        Ok(current)
    }

    /// Solve the split-tree into concrete rectangles for the provided viewport.
    ///
    /// Deterministic tie-break rule:
    /// - Desired split size is `floor(available * ratio)`.
    /// - If clamping is required by constraints, we clamp into the feasible
    ///   interval for the first child; remainder goes to the second child.
    ///
    /// Complexity:
    /// - Time: `O(node_count)` (single DFS over split tree)
    /// - Space: `O(node_count)` (output rectangle map)
    pub fn solve_layout(&self, area: Rect) -> Result<PaneLayout, PaneModelError> {
        let mut rects = BTreeMap::new();
        self.solve_node(self.root, area, &mut rects)?;
        Ok(PaneLayout { area, rects })
    }

    fn solve_node(
        &self,
        node_id: PaneId,
        area: Rect,
        rects: &mut BTreeMap<PaneId, Rect>,
    ) -> Result<(), PaneModelError> {
        let Some(node) = self.nodes.get(&node_id) else {
            return Err(PaneModelError::MissingRoot { root: node_id });
        };

        validate_area_against_constraints(node_id, area, node.constraints)?;
        let _ = rects.insert(node_id, area);

        let PaneNodeKind::Split(split) = &node.kind else {
            return Ok(());
        };

        let first_node = self
            .nodes
            .get(&split.first)
            .ok_or(PaneModelError::MissingChild {
                parent: node_id,
                child: split.first,
            })?;
        let second_node = self
            .nodes
            .get(&split.second)
            .ok_or(PaneModelError::MissingChild {
                parent: node_id,
                child: split.second,
            })?;

        let (first_bounds, second_bounds, available) = match split.axis {
            SplitAxis::Horizontal => (
                axis_bounds(first_node.constraints, split.axis),
                axis_bounds(second_node.constraints, split.axis),
                area.width,
            ),
            SplitAxis::Vertical => (
                axis_bounds(first_node.constraints, split.axis),
                axis_bounds(second_node.constraints, split.axis),
                area.height,
            ),
        };

        let (first_size, second_size) = solve_split_sizes(
            node_id,
            split.axis,
            available,
            split.ratio,
            first_bounds,
            second_bounds,
        )?;

        let (first_rect, second_rect) = match split.axis {
            SplitAxis::Horizontal => (
                Rect::new(area.x, area.y, first_size, area.height),
                Rect::new(
                    area.x.saturating_add(first_size),
                    area.y,
                    second_size,
                    area.height,
                ),
            ),
            SplitAxis::Vertical => (
                Rect::new(area.x, area.y, area.width, first_size),
                Rect::new(
                    area.x,
                    area.y.saturating_add(first_size),
                    area.width,
                    second_size,
                ),
            ),
        };

        self.solve_node(split.first, first_rect, rects)?;
        self.solve_node(split.second, second_rect, rects)?;
        Ok(())
    }
}

/// Deterministic allocator for pane IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneIdAllocator {
    next: PaneId,
}

impl PaneIdAllocator {
    /// Start allocating from a known ID.
    #[must_use]
    pub const fn with_next(next: PaneId) -> Self {
        Self { next }
    }

    /// Create allocator from the next ID in a validated tree.
    #[must_use]
    pub fn from_tree(tree: &PaneTree) -> Self {
        Self { next: tree.next_id }
    }

    /// Peek at the next ID without consuming.
    #[must_use]
    pub const fn peek(&self) -> PaneId {
        self.next
    }

    /// Allocate the next ID and advance.
    pub fn allocate(&mut self) -> Result<PaneId, PaneModelError> {
        let current = self.next;
        self.next = self.next.checked_next()?;
        Ok(current)
    }
}

impl Default for PaneIdAllocator {
    fn default() -> Self {
        Self { next: PaneId::MIN }
    }
}

/// Validation errors for pane schema construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneModelError {
    ZeroPaneId,
    UnsupportedSchemaVersion {
        version: u16,
    },
    DuplicateNodeId {
        node_id: PaneId,
    },
    MissingRoot {
        root: PaneId,
    },
    RootHasParent {
        root: PaneId,
        parent: PaneId,
    },
    MissingParent {
        node_id: PaneId,
        parent: PaneId,
    },
    MissingChild {
        parent: PaneId,
        child: PaneId,
    },
    MultipleParents {
        child: PaneId,
        first_parent: PaneId,
        second_parent: PaneId,
    },
    ParentMismatch {
        node_id: PaneId,
        expected: Option<PaneId>,
        actual: Option<PaneId>,
    },
    SelfReferentialSplit {
        node_id: PaneId,
    },
    DuplicateSplitChildren {
        node_id: PaneId,
        child: PaneId,
    },
    InvalidSplitRatio {
        numerator: u32,
        denominator: u32,
    },
    InvalidConstraint {
        node_id: PaneId,
        axis: &'static str,
        min: u16,
        max: u16,
    },
    NodeConstraintUnsatisfied {
        node_id: PaneId,
        axis: &'static str,
        actual: u16,
        min: u16,
        max: Option<u16>,
    },
    OverconstrainedSplit {
        node_id: PaneId,
        axis: SplitAxis,
        available: u16,
        first_min: u16,
        first_max: u16,
        second_min: u16,
        second_max: u16,
    },
    CycleDetected {
        node_id: PaneId,
    },
    UnreachableNode {
        node_id: PaneId,
    },
    NextIdNotGreaterThanExisting {
        next_id: PaneId,
        max_existing: PaneId,
    },
    PaneIdOverflow {
        current: PaneId,
    },
}

impl fmt::Display for PaneModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroPaneId => write!(f, "pane id 0 is invalid"),
            Self::UnsupportedSchemaVersion { version } => {
                write!(
                    f,
                    "unsupported pane schema version {version} (expected {PANE_TREE_SCHEMA_VERSION})"
                )
            }
            Self::DuplicateNodeId { node_id } => write!(f, "duplicate pane node id {}", node_id.0),
            Self::MissingRoot { root } => write!(f, "root pane node {} not found", root.0),
            Self::RootHasParent { root, parent } => write!(
                f,
                "root pane node {} must not have parent {}",
                root.0, parent.0
            ),
            Self::MissingParent { node_id, parent } => write!(
                f,
                "node {} references missing parent {}",
                node_id.0, parent.0
            ),
            Self::MissingChild { parent, child } => write!(
                f,
                "split node {} references missing child {}",
                parent.0, child.0
            ),
            Self::MultipleParents {
                child,
                first_parent,
                second_parent,
            } => write!(
                f,
                "node {} has multiple parents: {} and {}",
                child.0, first_parent.0, second_parent.0
            ),
            Self::ParentMismatch {
                node_id,
                expected,
                actual,
            } => write!(
                f,
                "node {} parent mismatch: expected {:?}, got {:?}",
                node_id.0,
                expected.map(PaneId::get),
                actual.map(PaneId::get)
            ),
            Self::SelfReferentialSplit { node_id } => {
                write!(f, "split node {} cannot reference itself", node_id.0)
            }
            Self::DuplicateSplitChildren { node_id, child } => write!(
                f,
                "split node {} references child {} twice",
                node_id.0, child.0
            ),
            Self::InvalidSplitRatio {
                numerator,
                denominator,
            } => write!(
                f,
                "invalid split ratio {numerator}/{denominator}: both values must be > 0"
            ),
            Self::InvalidConstraint {
                node_id,
                axis,
                min,
                max,
            } => write!(
                f,
                "invalid {axis} constraints for node {}: max {max} < min {min}",
                node_id.0
            ),
            Self::NodeConstraintUnsatisfied {
                node_id,
                axis,
                actual,
                min,
                max,
            } => write!(
                f,
                "node {} {axis}={} violates constraints [min={}, max={:?}]",
                node_id.0, actual, min, max
            ),
            Self::OverconstrainedSplit {
                node_id,
                axis,
                available,
                first_min,
                first_max,
                second_min,
                second_max,
            } => write!(
                f,
                "overconstrained {:?} split at node {} (available={}): first[min={}, max={}], second[min={}, max={}]",
                axis, node_id.0, available, first_min, first_max, second_min, second_max
            ),
            Self::CycleDetected { node_id } => {
                write!(f, "cycle detected at node {}", node_id.0)
            }
            Self::UnreachableNode { node_id } => {
                write!(f, "node {} is unreachable from root", node_id.0)
            }
            Self::NextIdNotGreaterThanExisting {
                next_id,
                max_existing,
            } => write!(
                f,
                "next_id {} must be greater than max existing id {}",
                next_id.0, max_existing.0
            ),
            Self::PaneIdOverflow { current } => {
                write!(f, "pane id overflow after {}", current.0)
            }
        }
    }
}

impl std::error::Error for PaneModelError {}

fn snapshot_state_hash(snapshot: &PaneTreeSnapshot) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0001_0000_01b3;

    fn mix(hash: &mut u64, byte: u8) {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(PRIME);
    }

    fn mix_bytes(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            mix(hash, *byte);
        }
    }

    fn mix_u16(hash: &mut u64, value: u16) {
        mix_bytes(hash, &value.to_le_bytes());
    }

    fn mix_u32(hash: &mut u64, value: u32) {
        mix_bytes(hash, &value.to_le_bytes());
    }

    fn mix_u64(hash: &mut u64, value: u64) {
        mix_bytes(hash, &value.to_le_bytes());
    }

    fn mix_bool(hash: &mut u64, value: bool) {
        mix(hash, u8::from(value));
    }

    fn mix_opt_u16(hash: &mut u64, value: Option<u16>) {
        match value {
            Some(value) => {
                mix(hash, 1);
                mix_u16(hash, value);
            }
            None => mix(hash, 0),
        }
    }

    fn mix_opt_pane_id(hash: &mut u64, value: Option<PaneId>) {
        match value {
            Some(value) => {
                mix(hash, 1);
                mix_u64(hash, value.get());
            }
            None => mix(hash, 0),
        }
    }

    fn mix_str(hash: &mut u64, value: &str) {
        mix_u64(hash, value.len() as u64);
        mix_bytes(hash, value.as_bytes());
    }

    fn mix_extensions(hash: &mut u64, extensions: &BTreeMap<String, String>) {
        mix_u64(hash, extensions.len() as u64);
        for (key, value) in extensions {
            mix_str(hash, key);
            mix_str(hash, value);
        }
    }

    let mut canonical = snapshot.clone();
    canonical.canonicalize();

    let mut hash = OFFSET_BASIS;
    mix_u16(&mut hash, canonical.schema_version);
    mix_u64(&mut hash, canonical.root.get());
    mix_u64(&mut hash, canonical.next_id.get());
    mix_extensions(&mut hash, &canonical.extensions);
    mix_u64(&mut hash, canonical.nodes.len() as u64);

    for node in &canonical.nodes {
        mix_u64(&mut hash, node.id.get());
        mix_opt_pane_id(&mut hash, node.parent);
        mix_u16(&mut hash, node.constraints.min_width);
        mix_u16(&mut hash, node.constraints.min_height);
        mix_opt_u16(&mut hash, node.constraints.max_width);
        mix_opt_u16(&mut hash, node.constraints.max_height);
        mix_bool(&mut hash, node.constraints.collapsible);
        mix_extensions(&mut hash, &node.extensions);

        match &node.kind {
            PaneNodeKind::Leaf(leaf) => {
                mix(&mut hash, 1);
                mix_str(&mut hash, &leaf.surface_key);
                mix_extensions(&mut hash, &leaf.extensions);
            }
            PaneNodeKind::Split(split) => {
                mix(&mut hash, 2);
                let axis_byte = match split.axis {
                    SplitAxis::Horizontal => 1,
                    SplitAxis::Vertical => 2,
                };
                mix(&mut hash, axis_byte);
                mix_u32(&mut hash, split.ratio.numerator());
                mix_u32(&mut hash, split.ratio.denominator());
                mix_u64(&mut hash, split.first.get());
                mix_u64(&mut hash, split.second.get());
            }
        }
    }

    hash
}

fn push_invariant_issue(
    issues: &mut Vec<PaneInvariantIssue>,
    code: PaneInvariantCode,
    repairable: bool,
    node_id: Option<PaneId>,
    related_node: Option<PaneId>,
    message: impl Into<String>,
) {
    issues.push(PaneInvariantIssue {
        code,
        severity: PaneInvariantSeverity::Error,
        repairable,
        node_id,
        related_node,
        message: message.into(),
    });
}

fn dfs_collect_cycles_and_reachable(
    node_id: PaneId,
    nodes: &BTreeMap<PaneId, PaneNodeRecord>,
    visiting: &mut BTreeSet<PaneId>,
    visited: &mut BTreeSet<PaneId>,
    cycle_nodes: &mut BTreeSet<PaneId>,
) {
    if visiting.contains(&node_id) {
        let _ = cycle_nodes.insert(node_id);
        return;
    }
    if !visited.insert(node_id) {
        return;
    }

    let _ = visiting.insert(node_id);
    if let Some(node) = nodes.get(&node_id)
        && let PaneNodeKind::Split(split) = &node.kind
    {
        for child in [split.first, split.second] {
            if nodes.contains_key(&child) {
                dfs_collect_cycles_and_reachable(child, nodes, visiting, visited, cycle_nodes);
            }
        }
    }
    let _ = visiting.remove(&node_id);
}

fn build_invariant_report(snapshot: &PaneTreeSnapshot) -> PaneInvariantReport {
    let mut issues = Vec::new();

    if snapshot.schema_version != PANE_TREE_SCHEMA_VERSION {
        push_invariant_issue(
            &mut issues,
            PaneInvariantCode::UnsupportedSchemaVersion,
            false,
            None,
            None,
            format!(
                "unsupported schema version {} (expected {})",
                snapshot.schema_version, PANE_TREE_SCHEMA_VERSION
            ),
        );
    }

    let mut nodes = BTreeMap::new();
    for node in &snapshot.nodes {
        if nodes.insert(node.id, node.clone()).is_some() {
            push_invariant_issue(
                &mut issues,
                PaneInvariantCode::DuplicateNodeId,
                false,
                Some(node.id),
                None,
                format!("duplicate node id {}", node.id.get()),
            );
        }
    }

    if let Some(max_existing) = nodes.keys().next_back().copied()
        && snapshot.next_id <= max_existing
    {
        push_invariant_issue(
            &mut issues,
            PaneInvariantCode::NextIdNotGreaterThanExisting,
            true,
            Some(snapshot.next_id),
            Some(max_existing),
            format!(
                "next_id {} must be greater than max node id {}",
                snapshot.next_id.get(),
                max_existing.get()
            ),
        );
    }

    if !nodes.contains_key(&snapshot.root) {
        push_invariant_issue(
            &mut issues,
            PaneInvariantCode::MissingRoot,
            false,
            Some(snapshot.root),
            None,
            format!("root node {} is missing", snapshot.root.get()),
        );
    }

    let mut expected_parents = BTreeMap::new();
    for node in nodes.values() {
        if let Err(err) = node.constraints.validate(node.id) {
            push_invariant_issue(
                &mut issues,
                PaneInvariantCode::InvalidConstraint,
                false,
                Some(node.id),
                None,
                err.to_string(),
            );
        }

        if let Some(parent) = node.parent
            && !nodes.contains_key(&parent)
        {
            push_invariant_issue(
                &mut issues,
                PaneInvariantCode::MissingParent,
                true,
                Some(node.id),
                Some(parent),
                format!(
                    "node {} references missing parent {}",
                    node.id.get(),
                    parent.get()
                ),
            );
        }

        if let PaneNodeKind::Split(split) = &node.kind {
            if split.ratio.numerator() == 0 || split.ratio.denominator() == 0 {
                push_invariant_issue(
                    &mut issues,
                    PaneInvariantCode::InvalidSplitRatio,
                    false,
                    Some(node.id),
                    None,
                    format!(
                        "split node {} has invalid ratio {}/{}",
                        node.id.get(),
                        split.ratio.numerator(),
                        split.ratio.denominator()
                    ),
                );
            }

            if split.first == node.id || split.second == node.id {
                push_invariant_issue(
                    &mut issues,
                    PaneInvariantCode::SelfReferentialSplit,
                    false,
                    Some(node.id),
                    None,
                    format!("split node {} references itself", node.id.get()),
                );
            }

            if split.first == split.second {
                push_invariant_issue(
                    &mut issues,
                    PaneInvariantCode::DuplicateSplitChildren,
                    false,
                    Some(node.id),
                    Some(split.first),
                    format!(
                        "split node {} references child {} twice",
                        node.id.get(),
                        split.first.get()
                    ),
                );
            }

            for child in [split.first, split.second] {
                if !nodes.contains_key(&child) {
                    push_invariant_issue(
                        &mut issues,
                        PaneInvariantCode::MissingChild,
                        false,
                        Some(node.id),
                        Some(child),
                        format!(
                            "split node {} references missing child {}",
                            node.id.get(),
                            child.get()
                        ),
                    );
                    continue;
                }

                if let Some(first_parent) = expected_parents.insert(child, node.id)
                    && first_parent != node.id
                {
                    push_invariant_issue(
                        &mut issues,
                        PaneInvariantCode::MultipleParents,
                        false,
                        Some(child),
                        Some(node.id),
                        format!(
                            "node {} has multiple split parents {} and {}",
                            child.get(),
                            first_parent.get(),
                            node.id.get()
                        ),
                    );
                }
            }
        }
    }

    if let Some(root_node) = nodes.get(&snapshot.root)
        && let Some(parent) = root_node.parent
    {
        push_invariant_issue(
            &mut issues,
            PaneInvariantCode::RootHasParent,
            true,
            Some(snapshot.root),
            Some(parent),
            format!(
                "root node {} must not have parent {}",
                snapshot.root.get(),
                parent.get()
            ),
        );
    }

    for node in nodes.values() {
        let expected_parent = if node.id == snapshot.root {
            None
        } else {
            expected_parents.get(&node.id).copied()
        };

        if node.parent != expected_parent {
            push_invariant_issue(
                &mut issues,
                PaneInvariantCode::ParentMismatch,
                true,
                Some(node.id),
                expected_parent,
                format!(
                    "node {} parent mismatch: expected {:?}, got {:?}",
                    node.id.get(),
                    expected_parent.map(PaneId::get),
                    node.parent.map(PaneId::get)
                ),
            );
        }
    }

    if nodes.contains_key(&snapshot.root) {
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut cycle_nodes = BTreeSet::new();
        dfs_collect_cycles_and_reachable(
            snapshot.root,
            &nodes,
            &mut visiting,
            &mut visited,
            &mut cycle_nodes,
        );

        for node_id in cycle_nodes {
            push_invariant_issue(
                &mut issues,
                PaneInvariantCode::CycleDetected,
                false,
                Some(node_id),
                None,
                format!("cycle detected at node {}", node_id.get()),
            );
        }

        for node_id in nodes.keys() {
            if !visited.contains(node_id) {
                push_invariant_issue(
                    &mut issues,
                    PaneInvariantCode::UnreachableNode,
                    true,
                    Some(*node_id),
                    None,
                    format!("node {} is unreachable from root", node_id.get()),
                );
            }
        }
    }

    issues.sort_by(|left, right| {
        (
            left.code,
            left.node_id.is_none(),
            left.node_id,
            left.related_node.is_none(),
            left.related_node,
            &left.message,
        )
            .cmp(&(
                right.code,
                right.node_id.is_none(),
                right.node_id,
                right.related_node.is_none(),
                right.related_node,
                &right.message,
            ))
    });

    PaneInvariantReport {
        snapshot_hash: snapshot_state_hash(snapshot),
        issues,
    }
}

fn repair_snapshot_safe(
    mut snapshot: PaneTreeSnapshot,
) -> Result<PaneRepairOutcome, PaneRepairError> {
    snapshot.canonicalize();

    let before_hash = snapshot_state_hash(&snapshot);
    let report_before = build_invariant_report(&snapshot);
    let mut unsafe_codes = report_before
        .issues
        .iter()
        .filter(|issue| issue.severity == PaneInvariantSeverity::Error && !issue.repairable)
        .map(|issue| issue.code)
        .collect::<Vec<_>>();
    unsafe_codes.sort();
    unsafe_codes.dedup();

    if !unsafe_codes.is_empty() {
        return Err(PaneRepairError {
            before_hash,
            report: report_before,
            reason: PaneRepairFailure::UnsafeIssuesPresent {
                codes: unsafe_codes,
            },
        });
    }

    let mut nodes = BTreeMap::new();
    for node in snapshot.nodes {
        let _ = nodes.entry(node.id).or_insert(node);
    }

    let mut actions = Vec::new();
    let mut expected_parents = BTreeMap::new();
    for node in nodes.values() {
        if let PaneNodeKind::Split(split) = &node.kind {
            for child in [split.first, split.second] {
                let _ = expected_parents.entry(child).or_insert(node.id);
            }
        }
    }

    for node in nodes.values_mut() {
        let expected_parent = if node.id == snapshot.root {
            None
        } else {
            expected_parents.get(&node.id).copied()
        };
        if node.parent != expected_parent {
            actions.push(PaneRepairAction::ReparentNode {
                node_id: node.id,
                before_parent: node.parent,
                after_parent: expected_parent,
            });
            node.parent = expected_parent;
        }

        if let PaneNodeKind::Split(split) = &mut node.kind {
            let normalized =
                PaneSplitRatio::new(split.ratio.numerator(), split.ratio.denominator()).map_err(
                    |error| PaneRepairError {
                        before_hash,
                        report: report_before.clone(),
                        reason: PaneRepairFailure::ValidationFailed { error },
                    },
                )?;
            if split.ratio != normalized {
                actions.push(PaneRepairAction::NormalizeRatio {
                    node_id: node.id,
                    before_numerator: split.ratio.numerator(),
                    before_denominator: split.ratio.denominator(),
                    after_numerator: normalized.numerator(),
                    after_denominator: normalized.denominator(),
                });
                split.ratio = normalized;
            }
        }
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut cycle_nodes = BTreeSet::new();
    if nodes.contains_key(&snapshot.root) {
        dfs_collect_cycles_and_reachable(
            snapshot.root,
            &nodes,
            &mut visiting,
            &mut visited,
            &mut cycle_nodes,
        );
    }
    if !cycle_nodes.is_empty() {
        let mut codes = vec![PaneInvariantCode::CycleDetected];
        codes.sort();
        codes.dedup();
        return Err(PaneRepairError {
            before_hash,
            report: report_before,
            reason: PaneRepairFailure::UnsafeIssuesPresent { codes },
        });
    }

    let all_node_ids = nodes.keys().copied().collect::<Vec<_>>();
    for node_id in all_node_ids {
        if !visited.contains(&node_id) {
            let _ = nodes.remove(&node_id);
            actions.push(PaneRepairAction::RemoveOrphanNode { node_id });
        }
    }

    if let Some(max_existing) = nodes.keys().next_back().copied()
        && snapshot.next_id <= max_existing
    {
        let after = max_existing
            .checked_next()
            .map_err(|error| PaneRepairError {
                before_hash,
                report: report_before.clone(),
                reason: PaneRepairFailure::ValidationFailed { error },
            })?;
        actions.push(PaneRepairAction::BumpNextId {
            before: snapshot.next_id,
            after,
        });
        snapshot.next_id = after;
    }

    snapshot.nodes = nodes.into_values().collect();
    snapshot.canonicalize();

    let tree = PaneTree::from_snapshot(snapshot).map_err(|error| PaneRepairError {
        before_hash,
        report: report_before.clone(),
        reason: PaneRepairFailure::ValidationFailed { error },
    })?;
    let report_after = tree.invariant_report();
    let after_hash = tree.state_hash();

    Ok(PaneRepairOutcome {
        before_hash,
        after_hash,
        report_before,
        report_after,
        actions,
        tree,
    })
}

fn validate_tree(
    root: PaneId,
    next_id: PaneId,
    nodes: &BTreeMap<PaneId, PaneNodeRecord>,
) -> Result<(), PaneModelError> {
    if !nodes.contains_key(&root) {
        return Err(PaneModelError::MissingRoot { root });
    }

    let max_existing = nodes.keys().next_back().copied().unwrap_or(root);
    if next_id <= max_existing {
        return Err(PaneModelError::NextIdNotGreaterThanExisting {
            next_id,
            max_existing,
        });
    }

    let mut expected_parents = BTreeMap::new();

    for node in nodes.values() {
        node.constraints.validate(node.id)?;

        if let Some(parent) = node.parent
            && !nodes.contains_key(&parent)
        {
            return Err(PaneModelError::MissingParent {
                node_id: node.id,
                parent,
            });
        }

        if let PaneNodeKind::Split(split) = &node.kind {
            if split.ratio.numerator() == 0 || split.ratio.denominator() == 0 {
                return Err(PaneModelError::InvalidSplitRatio {
                    numerator: split.ratio.numerator(),
                    denominator: split.ratio.denominator(),
                });
            }

            if split.first == node.id || split.second == node.id {
                return Err(PaneModelError::SelfReferentialSplit { node_id: node.id });
            }
            if split.first == split.second {
                return Err(PaneModelError::DuplicateSplitChildren {
                    node_id: node.id,
                    child: split.first,
                });
            }

            for child in [split.first, split.second] {
                if !nodes.contains_key(&child) {
                    return Err(PaneModelError::MissingChild {
                        parent: node.id,
                        child,
                    });
                }
                if let Some(first_parent) = expected_parents.insert(child, node.id)
                    && first_parent != node.id
                {
                    return Err(PaneModelError::MultipleParents {
                        child,
                        first_parent,
                        second_parent: node.id,
                    });
                }
            }
        }
    }

    if let Some(parent) = nodes.get(&root).and_then(|node| node.parent) {
        return Err(PaneModelError::RootHasParent { root, parent });
    }

    for node in nodes.values() {
        let expected = if node.id == root {
            None
        } else {
            expected_parents.get(&node.id).copied()
        };
        if node.parent != expected {
            return Err(PaneModelError::ParentMismatch {
                node_id: node.id,
                expected,
                actual: node.parent,
            });
        }
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    dfs_validate(root, nodes, &mut visiting, &mut visited)?;

    if visited.len() != nodes.len()
        && let Some(node_id) = nodes.keys().find(|node_id| !visited.contains(node_id))
    {
        return Err(PaneModelError::UnreachableNode { node_id: *node_id });
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct AxisBounds {
    min: u16,
    max: Option<u16>,
}

fn axis_bounds(constraints: PaneConstraints, axis: SplitAxis) -> AxisBounds {
    match axis {
        SplitAxis::Horizontal => AxisBounds {
            min: constraints.min_width,
            max: constraints.max_width,
        },
        SplitAxis::Vertical => AxisBounds {
            min: constraints.min_height,
            max: constraints.max_height,
        },
    }
}

fn validate_area_against_constraints(
    node_id: PaneId,
    area: Rect,
    constraints: PaneConstraints,
) -> Result<(), PaneModelError> {
    if area.width < constraints.min_width {
        return Err(PaneModelError::NodeConstraintUnsatisfied {
            node_id,
            axis: "width",
            actual: area.width,
            min: constraints.min_width,
            max: constraints.max_width,
        });
    }
    if area.height < constraints.min_height {
        return Err(PaneModelError::NodeConstraintUnsatisfied {
            node_id,
            axis: "height",
            actual: area.height,
            min: constraints.min_height,
            max: constraints.max_height,
        });
    }
    if let Some(max_width) = constraints.max_width
        && area.width > max_width
    {
        return Err(PaneModelError::NodeConstraintUnsatisfied {
            node_id,
            axis: "width",
            actual: area.width,
            min: constraints.min_width,
            max: constraints.max_width,
        });
    }
    if let Some(max_height) = constraints.max_height
        && area.height > max_height
    {
        return Err(PaneModelError::NodeConstraintUnsatisfied {
            node_id,
            axis: "height",
            actual: area.height,
            min: constraints.min_height,
            max: constraints.max_height,
        });
    }
    Ok(())
}

fn solve_split_sizes(
    node_id: PaneId,
    axis: SplitAxis,
    available: u16,
    ratio: PaneSplitRatio,
    first: AxisBounds,
    second: AxisBounds,
) -> Result<(u16, u16), PaneModelError> {
    let first_max = first.max.unwrap_or(available).min(available);
    let second_max = second.max.unwrap_or(available).min(available);

    let feasible_first_min = first.min.max(available.saturating_sub(second_max));
    let feasible_first_max = first_max.min(available.saturating_sub(second.min));

    if feasible_first_min > feasible_first_max {
        return Err(PaneModelError::OverconstrainedSplit {
            node_id,
            axis,
            available,
            first_min: first.min,
            first_max,
            second_min: second.min,
            second_max,
        });
    }

    let total_weight = u64::from(ratio.numerator()) + u64::from(ratio.denominator());
    let desired_first_u64 = (u64::from(available) * u64::from(ratio.numerator())) / total_weight;
    let desired_first = desired_first_u64 as u16;

    let first_size = desired_first.clamp(feasible_first_min, feasible_first_max);
    let second_size = available.saturating_sub(first_size);
    Ok((first_size, second_size))
}

fn dfs_validate(
    node_id: PaneId,
    nodes: &BTreeMap<PaneId, PaneNodeRecord>,
    visiting: &mut BTreeSet<PaneId>,
    visited: &mut BTreeSet<PaneId>,
) -> Result<(), PaneModelError> {
    if visiting.contains(&node_id) {
        return Err(PaneModelError::CycleDetected { node_id });
    }
    if !visited.insert(node_id) {
        return Ok(());
    }

    let _ = visiting.insert(node_id);
    if let Some(node) = nodes.get(&node_id)
        && let PaneNodeKind::Split(split) = &node.kind
    {
        dfs_validate(split.first, nodes, visiting, visited)?;
        dfs_validate(split.second, nodes, visiting, visited)?;
    }
    let _ = visiting.remove(&node_id);
    Ok(())
}

fn gcd_u32(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let rem = left % right;
        left = right;
        right = rem;
    }
    left.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn id(raw: u64) -> PaneId {
        PaneId::new(raw).expect("test ID must be non-zero")
    }

    fn make_valid_snapshot() -> PaneTreeSnapshot {
        let root = id(1);
        let left = id(2);
        let right = id(3);

        PaneTreeSnapshot {
            schema_version: PANE_TREE_SCHEMA_VERSION,
            root,
            next_id: id(4),
            nodes: vec![
                PaneNodeRecord::leaf(
                    right,
                    Some(root),
                    PaneLeaf {
                        surface_key: "right".to_string(),
                        extensions: BTreeMap::new(),
                    },
                ),
                PaneNodeRecord::split(
                    root,
                    None,
                    PaneSplit {
                        axis: SplitAxis::Horizontal,
                        ratio: PaneSplitRatio::new(3, 2).expect("valid ratio"),
                        first: left,
                        second: right,
                    },
                ),
                PaneNodeRecord::leaf(
                    left,
                    Some(root),
                    PaneLeaf {
                        surface_key: "left".to_string(),
                        extensions: BTreeMap::new(),
                    },
                ),
            ],
            extensions: BTreeMap::new(),
        }
    }

    fn make_nested_snapshot() -> PaneTreeSnapshot {
        let root = id(1);
        let left = id(2);
        let right_split = id(3);
        let right_top = id(4);
        let right_bottom = id(5);

        PaneTreeSnapshot {
            schema_version: PANE_TREE_SCHEMA_VERSION,
            root,
            next_id: id(6),
            nodes: vec![
                PaneNodeRecord::split(
                    root,
                    None,
                    PaneSplit {
                        axis: SplitAxis::Horizontal,
                        ratio: PaneSplitRatio::new(1, 1).expect("valid ratio"),
                        first: left,
                        second: right_split,
                    },
                ),
                PaneNodeRecord::leaf(left, Some(root), PaneLeaf::new("left")),
                PaneNodeRecord::split(
                    right_split,
                    Some(root),
                    PaneSplit {
                        axis: SplitAxis::Vertical,
                        ratio: PaneSplitRatio::new(1, 1).expect("valid ratio"),
                        first: right_top,
                        second: right_bottom,
                    },
                ),
                PaneNodeRecord::leaf(right_top, Some(right_split), PaneLeaf::new("right_top")),
                PaneNodeRecord::leaf(
                    right_bottom,
                    Some(right_split),
                    PaneLeaf::new("right_bottom"),
                ),
            ],
            extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn ratio_is_normalized() {
        let ratio = PaneSplitRatio::new(12, 8).expect("ratio should normalize");
        assert_eq!(ratio.numerator(), 3);
        assert_eq!(ratio.denominator(), 2);
    }

    #[test]
    fn snapshot_round_trip_preserves_canonical_order() {
        let tree =
            PaneTree::from_snapshot(make_valid_snapshot()).expect("snapshot should validate");
        let snapshot = tree.to_snapshot();
        let ids = snapshot
            .nodes
            .iter()
            .map(|node| node.id.get())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn duplicate_node_id_is_rejected() {
        let mut snapshot = make_valid_snapshot();
        snapshot.nodes.push(PaneNodeRecord::leaf(
            id(2),
            Some(id(1)),
            PaneLeaf::new("dup"),
        ));
        let err = PaneTree::from_snapshot(snapshot).expect_err("duplicate ID should fail");
        assert_eq!(err, PaneModelError::DuplicateNodeId { node_id: id(2) });
    }

    #[test]
    fn missing_child_is_rejected() {
        let mut snapshot = make_valid_snapshot();
        snapshot.nodes.retain(|node| node.id != id(3));
        let err = PaneTree::from_snapshot(snapshot).expect_err("missing child should fail");
        assert_eq!(
            err,
            PaneModelError::MissingChild {
                parent: id(1),
                child: id(3),
            }
        );
    }

    #[test]
    fn unreachable_node_is_rejected() {
        let mut snapshot = make_valid_snapshot();
        snapshot
            .nodes
            .push(PaneNodeRecord::leaf(id(10), None, PaneLeaf::new("orphan")));
        snapshot.next_id = id(11);
        let err = PaneTree::from_snapshot(snapshot).expect_err("orphan should fail");
        assert_eq!(err, PaneModelError::UnreachableNode { node_id: id(10) });
    }

    #[test]
    fn next_id_must_be_greater_than_existing_ids() {
        let mut snapshot = make_valid_snapshot();
        snapshot.next_id = id(3);
        let err = PaneTree::from_snapshot(snapshot).expect_err("next_id should be > max ID");
        assert_eq!(
            err,
            PaneModelError::NextIdNotGreaterThanExisting {
                next_id: id(3),
                max_existing: id(3),
            }
        );
    }

    #[test]
    fn constraints_validate_bounds() {
        let constraints = PaneConstraints {
            min_width: 8,
            min_height: 1,
            max_width: Some(4),
            max_height: None,
            collapsible: false,
        };
        let err = constraints
            .validate(id(5))
            .expect_err("max width below min width must fail");
        assert_eq!(
            err,
            PaneModelError::InvalidConstraint {
                node_id: id(5),
                axis: "width",
                min: 8,
                max: 4,
            }
        );
    }

    #[test]
    fn allocator_is_deterministic() {
        let mut allocator = PaneIdAllocator::default();
        assert_eq!(allocator.allocate().expect("id 1"), id(1));
        assert_eq!(allocator.allocate().expect("id 2"), id(2));
        assert_eq!(allocator.peek(), id(3));
    }

    #[test]
    fn snapshot_json_shape_contains_forward_compat_fields() {
        let tree = PaneTree::from_snapshot(make_valid_snapshot()).expect("valid tree");
        let json = serde_json::to_value(tree.to_snapshot()).expect("snapshot should serialize");
        assert_eq!(json["schema_version"], serde_json::json!(1));
        assert!(json.get("extensions").is_some());
        let nodes = json["nodes"]
            .as_array()
            .expect("nodes should serialize as array");
        assert_eq!(nodes.len(), 3);
        assert!(nodes[0].get("kind").is_some());
    }

    #[test]
    fn solver_horizontal_ratio_split() {
        let tree = PaneTree::from_snapshot(make_valid_snapshot()).expect("valid tree");
        let layout = tree
            .solve_layout(Rect::new(0, 0, 50, 10))
            .expect("layout solve should succeed");

        assert_eq!(layout.rect(id(1)), Some(Rect::new(0, 0, 50, 10)));
        assert_eq!(layout.rect(id(2)), Some(Rect::new(0, 0, 30, 10)));
        assert_eq!(layout.rect(id(3)), Some(Rect::new(30, 0, 20, 10)));
    }

    #[test]
    fn solver_clamps_to_child_minimum_constraints() {
        let mut snapshot = make_valid_snapshot();
        for node in &mut snapshot.nodes {
            if node.id == id(2) {
                node.constraints.min_width = 35;
            }
        }

        let tree = PaneTree::from_snapshot(snapshot).expect("valid tree");
        let layout = tree
            .solve_layout(Rect::new(0, 0, 50, 10))
            .expect("layout solve should succeed");

        assert_eq!(layout.rect(id(2)), Some(Rect::new(0, 0, 35, 10)));
        assert_eq!(layout.rect(id(3)), Some(Rect::new(35, 0, 15, 10)));
    }

    #[test]
    fn solver_rejects_overconstrained_split() {
        let mut snapshot = make_valid_snapshot();
        for node in &mut snapshot.nodes {
            if node.id == id(2) {
                node.constraints.min_width = 30;
            }
            if node.id == id(3) {
                node.constraints.min_width = 30;
            }
        }

        let tree = PaneTree::from_snapshot(snapshot).expect("valid tree");
        let err = tree
            .solve_layout(Rect::new(0, 0, 50, 10))
            .expect_err("infeasible constraints should fail");

        assert_eq!(
            err,
            PaneModelError::OverconstrainedSplit {
                node_id: id(1),
                axis: SplitAxis::Horizontal,
                available: 50,
                first_min: 30,
                first_max: 50,
                second_min: 30,
                second_max: 50,
            }
        );
    }

    #[test]
    fn solver_is_deterministic() {
        let tree = PaneTree::from_snapshot(make_valid_snapshot()).expect("valid tree");
        let first = tree
            .solve_layout(Rect::new(0, 0, 79, 17))
            .expect("first solve should succeed");
        let second = tree
            .solve_layout(Rect::new(0, 0, 79, 17))
            .expect("second solve should succeed");
        assert_eq!(first, second);
    }

    #[test]
    fn split_leaf_wraps_existing_leaf_with_new_split() {
        let mut tree = PaneTree::singleton("root");
        let outcome = tree
            .apply_operation(
                7,
                PaneOperation::SplitLeaf {
                    target: id(1),
                    axis: SplitAxis::Horizontal,
                    ratio: PaneSplitRatio::new(3, 2).expect("valid ratio"),
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new("new"),
                },
            )
            .expect("split should succeed");

        assert_eq!(outcome.operation_id, 7);
        assert_eq!(outcome.kind, PaneOperationKind::SplitLeaf);
        assert_ne!(outcome.before_hash, outcome.after_hash);
        assert_eq!(tree.root(), id(2));

        let root = tree.node(id(2)).expect("split node exists");
        let PaneNodeKind::Split(split) = &root.kind else {
            unreachable!("root should be split");
        };
        assert_eq!(split.first, id(1));
        assert_eq!(split.second, id(3));

        let original = tree.node(id(1)).expect("original leaf exists");
        assert_eq!(original.parent, Some(id(2)));
        assert!(matches!(original.kind, PaneNodeKind::Leaf(_)));

        let new_leaf = tree.node(id(3)).expect("new leaf exists");
        assert_eq!(new_leaf.parent, Some(id(2)));
        let PaneNodeKind::Leaf(leaf) = &new_leaf.kind else {
            unreachable!("new node must be leaf");
        };
        assert_eq!(leaf.surface_key, "new");
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn close_node_promotes_sibling_and_removes_split_parent() {
        let mut tree = PaneTree::from_snapshot(make_valid_snapshot()).expect("valid tree");
        let outcome = tree
            .apply_operation(8, PaneOperation::CloseNode { target: id(2) })
            .expect("close should succeed");
        assert_eq!(outcome.kind, PaneOperationKind::CloseNode);

        assert_eq!(tree.root(), id(3));
        assert!(tree.node(id(1)).is_none());
        assert!(tree.node(id(2)).is_none());
        assert_eq!(tree.node(id(3)).and_then(|node| node.parent), None);
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn close_root_is_rejected_with_stable_hashes() {
        let mut tree = PaneTree::singleton("root");
        let err = tree
            .apply_operation(9, PaneOperation::CloseNode { target: id(1) })
            .expect_err("closing root must fail");

        assert_eq!(err.operation_id, 9);
        assert_eq!(err.kind, PaneOperationKind::CloseNode);
        assert_eq!(
            err.reason,
            PaneOperationFailure::CannotCloseRoot { node_id: id(1) }
        );
        assert_eq!(err.before_hash, err.after_hash);
        assert_eq!(tree.root(), id(1));
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn move_subtree_wraps_target_and_detaches_old_parent() {
        let mut tree = PaneTree::from_snapshot(make_nested_snapshot()).expect("valid tree");
        let outcome = tree
            .apply_operation(
                10,
                PaneOperation::MoveSubtree {
                    source: id(4),
                    target: id(2),
                    axis: SplitAxis::Vertical,
                    ratio: PaneSplitRatio::new(2, 1).expect("valid ratio"),
                    placement: PanePlacement::ExistingFirst,
                },
            )
            .expect("move should succeed");
        assert_eq!(outcome.kind, PaneOperationKind::MoveSubtree);

        assert!(
            tree.node(id(3)).is_none(),
            "old split parent should be removed"
        );
        assert_eq!(tree.node(id(5)).and_then(|node| node.parent), Some(id(1)));

        let inserted_split = tree
            .nodes()
            .find(|node| matches!(node.kind, PaneNodeKind::Split(_)) && node.id.get() >= 6)
            .expect("new split should exist");
        let PaneNodeKind::Split(split) = &inserted_split.kind else {
            unreachable!();
        };
        assert_eq!(split.first, id(2));
        assert_eq!(split.second, id(4));
        assert_eq!(
            tree.node(id(2)).and_then(|node| node.parent),
            Some(inserted_split.id)
        );
        assert_eq!(
            tree.node(id(4)).and_then(|node| node.parent),
            Some(inserted_split.id)
        );
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn move_subtree_rejects_ancestor_target() {
        let mut tree = PaneTree::from_snapshot(make_nested_snapshot()).expect("valid tree");
        let err = tree
            .apply_operation(
                11,
                PaneOperation::MoveSubtree {
                    source: id(3),
                    target: id(4),
                    axis: SplitAxis::Horizontal,
                    ratio: PaneSplitRatio::new(1, 1).expect("valid ratio"),
                    placement: PanePlacement::ExistingFirst,
                },
            )
            .expect_err("ancestor move must fail");

        assert_eq!(err.kind, PaneOperationKind::MoveSubtree);
        assert_eq!(
            err.reason,
            PaneOperationFailure::AncestorConflict {
                ancestor: id(3),
                descendant: id(4),
            }
        );
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn swap_nodes_exchanges_sibling_positions() {
        let mut tree = PaneTree::from_snapshot(make_valid_snapshot()).expect("valid tree");
        let outcome = tree
            .apply_operation(
                12,
                PaneOperation::SwapNodes {
                    first: id(2),
                    second: id(3),
                },
            )
            .expect("swap should succeed");
        assert_eq!(outcome.kind, PaneOperationKind::SwapNodes);

        let root = tree.node(id(1)).expect("root exists");
        let PaneNodeKind::Split(split) = &root.kind else {
            unreachable!("root should remain split");
        };
        assert_eq!(split.first, id(3));
        assert_eq!(split.second, id(2));
        assert_eq!(tree.node(id(2)).and_then(|node| node.parent), Some(id(1)));
        assert_eq!(tree.node(id(3)).and_then(|node| node.parent), Some(id(1)));
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn swap_nodes_rejects_ancestor_relation() {
        let mut tree = PaneTree::from_snapshot(make_nested_snapshot()).expect("valid tree");
        let err = tree
            .apply_operation(
                13,
                PaneOperation::SwapNodes {
                    first: id(3),
                    second: id(4),
                },
            )
            .expect_err("ancestor swap must fail");

        assert_eq!(err.kind, PaneOperationKind::SwapNodes);
        assert_eq!(
            err.reason,
            PaneOperationFailure::AncestorConflict {
                ancestor: id(3),
                descendant: id(4),
            }
        );
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn normalize_ratios_canonicalizes_non_reduced_values() {
        let mut snapshot = make_valid_snapshot();
        for node in &mut snapshot.nodes {
            if let PaneNodeKind::Split(split) = &mut node.kind {
                split.ratio = PaneSplitRatio {
                    numerator: 12,
                    denominator: 8,
                };
            }
        }

        let mut tree = PaneTree::from_snapshot(snapshot).expect("valid tree");
        let outcome = tree
            .apply_operation(14, PaneOperation::NormalizeRatios)
            .expect("normalize should succeed");
        assert_eq!(outcome.kind, PaneOperationKind::NormalizeRatios);

        let root = tree.node(id(1)).expect("root exists");
        let PaneNodeKind::Split(split) = &root.kind else {
            unreachable!("root should be split");
        };
        assert_eq!(split.ratio.numerator(), 3);
        assert_eq!(split.ratio.denominator(), 2);
    }

    #[test]
    fn transaction_commit_persists_mutations_and_journal_order() {
        let tree = PaneTree::singleton("root");
        let mut tx = tree.begin_transaction(77);

        let split = tx
            .apply_operation(
                100,
                PaneOperation::SplitLeaf {
                    target: id(1),
                    axis: SplitAxis::Horizontal,
                    ratio: PaneSplitRatio::new(1, 1).expect("valid ratio"),
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new("secondary"),
                },
            )
            .expect("split should succeed");
        assert_eq!(split.kind, PaneOperationKind::SplitLeaf);

        let normalize = tx
            .apply_operation(101, PaneOperation::NormalizeRatios)
            .expect("normalize should succeed");
        assert_eq!(normalize.kind, PaneOperationKind::NormalizeRatios);

        let outcome = tx.commit();
        assert!(outcome.committed);
        assert_eq!(outcome.transaction_id, 77);
        assert_eq!(outcome.tree.root(), id(2));
        assert_eq!(outcome.journal.len(), 2);
        assert_eq!(outcome.journal[0].sequence, 1);
        assert_eq!(outcome.journal[1].sequence, 2);
        assert_eq!(outcome.journal[0].operation_id, 100);
        assert_eq!(outcome.journal[1].operation_id, 101);
        assert_eq!(
            outcome.journal[0].result,
            PaneOperationJournalResult::Applied
        );
        assert_eq!(
            outcome.journal[1].result,
            PaneOperationJournalResult::Applied
        );
    }

    #[test]
    fn transaction_rollback_discards_mutations() {
        let tree = PaneTree::singleton("root");
        let before_hash = tree.state_hash();
        let mut tx = tree.begin_transaction(78);

        tx.apply_operation(
            200,
            PaneOperation::SplitLeaf {
                target: id(1),
                axis: SplitAxis::Vertical,
                ratio: PaneSplitRatio::new(2, 1).expect("valid ratio"),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("extra"),
            },
        )
        .expect("split should succeed");

        let outcome = tx.rollback();
        assert!(!outcome.committed);
        assert_eq!(outcome.tree.state_hash(), before_hash);
        assert_eq!(outcome.tree.root(), id(1));
        assert_eq!(outcome.journal.len(), 1);
        assert_eq!(outcome.journal[0].operation_id, 200);
    }

    #[test]
    fn transaction_journals_rejected_operation_without_mutation() {
        let tree = PaneTree::singleton("root");
        let mut tx = tree.begin_transaction(79);
        let before_hash = tx.tree().state_hash();

        let err = tx
            .apply_operation(300, PaneOperation::CloseNode { target: id(1) })
            .expect_err("close root should fail");
        assert_eq!(err.before_hash, err.after_hash);
        assert_eq!(tx.tree().state_hash(), before_hash);

        let journal = tx.journal();
        assert_eq!(journal.len(), 1);
        assert_eq!(journal[0].operation_id, 300);
        let PaneOperationJournalResult::Rejected { reason } = &journal[0].result else {
            unreachable!("journal entry should be rejected");
        };
        assert!(reason.contains("cannot close root"));
    }

    #[test]
    fn transaction_journal_is_deterministic_for_equivalent_runs() {
        let base = PaneTree::singleton("root");

        let mut first_tx = base.begin_transaction(80);
        first_tx
            .apply_operation(
                1,
                PaneOperation::SplitLeaf {
                    target: id(1),
                    axis: SplitAxis::Horizontal,
                    ratio: PaneSplitRatio::new(3, 1).expect("valid ratio"),
                    placement: PanePlacement::IncomingFirst,
                    new_leaf: PaneLeaf::new("new"),
                },
            )
            .expect("split should succeed");
        first_tx
            .apply_operation(2, PaneOperation::NormalizeRatios)
            .expect("normalize should succeed");
        let first = first_tx.commit();

        let mut second_tx = base.begin_transaction(80);
        second_tx
            .apply_operation(
                1,
                PaneOperation::SplitLeaf {
                    target: id(1),
                    axis: SplitAxis::Horizontal,
                    ratio: PaneSplitRatio::new(3, 1).expect("valid ratio"),
                    placement: PanePlacement::IncomingFirst,
                    new_leaf: PaneLeaf::new("new"),
                },
            )
            .expect("split should succeed");
        second_tx
            .apply_operation(2, PaneOperation::NormalizeRatios)
            .expect("normalize should succeed");
        let second = second_tx.commit();

        assert_eq!(first.tree.state_hash(), second.tree.state_hash());
        assert_eq!(first.journal, second.journal);
    }

    #[test]
    fn invariant_report_detects_parent_mismatch_and_orphan() {
        let mut snapshot = make_valid_snapshot();
        for node in &mut snapshot.nodes {
            if node.id == id(2) {
                node.parent = Some(id(3));
            }
        }
        snapshot
            .nodes
            .push(PaneNodeRecord::leaf(id(10), None, PaneLeaf::new("orphan")));
        snapshot.next_id = id(11);

        let report = snapshot.invariant_report();
        assert!(report.has_errors());
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == PaneInvariantCode::ParentMismatch)
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == PaneInvariantCode::UnreachableNode)
        );
    }

    #[test]
    fn repair_safe_normalizes_ratio_repairs_parents_and_removes_orphans() {
        let mut snapshot = make_valid_snapshot();
        for node in &mut snapshot.nodes {
            if node.id == id(1) {
                node.parent = Some(id(3));
                let PaneNodeKind::Split(split) = &mut node.kind else {
                    unreachable!("root should be split");
                };
                split.ratio = PaneSplitRatio {
                    numerator: 12,
                    denominator: 8,
                };
            }
            if node.id == id(2) {
                node.parent = Some(id(3));
            }
        }
        snapshot
            .nodes
            .push(PaneNodeRecord::leaf(id(10), None, PaneLeaf::new("orphan")));
        snapshot.next_id = id(11);

        let repaired = snapshot.repair_safe().expect("repair should succeed");
        assert_ne!(repaired.before_hash, repaired.after_hash);
        assert!(repaired.tree.validate().is_ok());
        assert!(!repaired.report_after.has_errors());
        assert!(
            repaired
                .actions
                .iter()
                .any(|action| matches!(action, PaneRepairAction::NormalizeRatio { node_id, .. } if *node_id == id(1)))
        );
        assert!(
            repaired
                .actions
                .iter()
                .any(|action| matches!(action, PaneRepairAction::ReparentNode { node_id, .. } if *node_id == id(1)))
        );
        assert!(
            repaired
                .actions
                .iter()
                .any(|action| matches!(action, PaneRepairAction::RemoveOrphanNode { node_id } if *node_id == id(10)))
        );
    }

    #[test]
    fn repair_safe_rejects_unsafe_topology() {
        let mut snapshot = make_valid_snapshot();
        snapshot.nodes.retain(|node| node.id != id(3));

        let err = snapshot
            .repair_safe()
            .expect_err("missing-child topology must be rejected");
        assert!(matches!(
            err.reason,
            PaneRepairFailure::UnsafeIssuesPresent { .. }
        ));
        let PaneRepairFailure::UnsafeIssuesPresent { codes } = err.reason else {
            unreachable!("expected unsafe issue failure");
        };
        assert!(codes.contains(&PaneInvariantCode::MissingChild));
    }

    #[test]
    fn repair_safe_is_deterministic_for_equivalent_snapshot() {
        let mut snapshot = make_valid_snapshot();
        for node in &mut snapshot.nodes {
            if node.id == id(1) {
                let PaneNodeKind::Split(split) = &mut node.kind else {
                    unreachable!("root should be split");
                };
                split.ratio = PaneSplitRatio {
                    numerator: 12,
                    denominator: 8,
                };
            }
        }
        snapshot
            .nodes
            .push(PaneNodeRecord::leaf(id(10), None, PaneLeaf::new("orphan")));
        snapshot.next_id = id(11);

        let first = snapshot.clone().repair_safe().expect("first repair");
        let second = snapshot.repair_safe().expect("second repair");

        assert_eq!(first.tree.state_hash(), second.tree.state_hash());
        assert_eq!(first.actions, second.actions);
        assert_eq!(first.report_after, second.report_after);
    }

    fn default_target() -> PaneResizeTarget {
        PaneResizeTarget {
            split_id: id(7),
            axis: SplitAxis::Horizontal,
        }
    }

    #[test]
    fn semantic_input_event_fixture_round_trip_covers_all_variants() {
        let mut pointer_down = PaneSemanticInputEvent::new(
            1,
            PaneSemanticInputEventKind::PointerDown {
                target: default_target(),
                pointer_id: 11,
                button: PanePointerButton::Primary,
                position: PanePointerPosition::new(42, 9),
            },
        );
        pointer_down.modifiers = PaneModifierSnapshot {
            shift: true,
            alt: false,
            ctrl: true,
            meta: false,
        };
        let pointer_down_fixture = r#"{"schema_version":1,"sequence":1,"modifiers":{"shift":true,"alt":false,"ctrl":true,"meta":false},"event":"pointer_down","target":{"split_id":7,"axis":"horizontal"},"pointer_id":11,"button":"primary","position":{"x":42,"y":9},"extensions":{}}"#;

        let pointer_move = PaneSemanticInputEvent::new(
            2,
            PaneSemanticInputEventKind::PointerMove {
                target: default_target(),
                pointer_id: 11,
                position: PanePointerPosition::new(45, 8),
                delta_x: 3,
                delta_y: -1,
            },
        );
        let pointer_move_fixture = r#"{"schema_version":1,"sequence":2,"modifiers":{"shift":false,"alt":false,"ctrl":false,"meta":false},"event":"pointer_move","target":{"split_id":7,"axis":"horizontal"},"pointer_id":11,"position":{"x":45,"y":8},"delta_x":3,"delta_y":-1,"extensions":{}}"#;

        let pointer_up = PaneSemanticInputEvent::new(
            3,
            PaneSemanticInputEventKind::PointerUp {
                target: default_target(),
                pointer_id: 11,
                button: PanePointerButton::Primary,
                position: PanePointerPosition::new(45, 8),
            },
        );
        let pointer_up_fixture = r#"{"schema_version":1,"sequence":3,"modifiers":{"shift":false,"alt":false,"ctrl":false,"meta":false},"event":"pointer_up","target":{"split_id":7,"axis":"horizontal"},"pointer_id":11,"button":"primary","position":{"x":45,"y":8},"extensions":{}}"#;

        let wheel_nudge = PaneSemanticInputEvent::new(
            4,
            PaneSemanticInputEventKind::WheelNudge {
                target: default_target(),
                lines: -2,
            },
        );
        let wheel_nudge_fixture = r#"{"schema_version":1,"sequence":4,"modifiers":{"shift":false,"alt":false,"ctrl":false,"meta":false},"event":"wheel_nudge","target":{"split_id":7,"axis":"horizontal"},"lines":-2,"extensions":{}}"#;

        let keyboard_resize = PaneSemanticInputEvent::new(
            5,
            PaneSemanticInputEventKind::KeyboardResize {
                target: default_target(),
                direction: PaneResizeDirection::Increase,
                units: 3,
            },
        );
        let keyboard_resize_fixture = r#"{"schema_version":1,"sequence":5,"modifiers":{"shift":false,"alt":false,"ctrl":false,"meta":false},"event":"keyboard_resize","target":{"split_id":7,"axis":"horizontal"},"direction":"increase","units":3,"extensions":{}}"#;

        let cancel = PaneSemanticInputEvent::new(
            6,
            PaneSemanticInputEventKind::Cancel {
                target: Some(default_target()),
                reason: PaneCancelReason::PointerCancel,
            },
        );
        let cancel_fixture = r#"{"schema_version":1,"sequence":6,"modifiers":{"shift":false,"alt":false,"ctrl":false,"meta":false},"event":"cancel","target":{"split_id":7,"axis":"horizontal"},"reason":"pointer_cancel","extensions":{}}"#;

        let blur =
            PaneSemanticInputEvent::new(7, PaneSemanticInputEventKind::Blur { target: None });
        let blur_fixture = r#"{"schema_version":1,"sequence":7,"modifiers":{"shift":false,"alt":false,"ctrl":false,"meta":false},"event":"blur","target":null,"extensions":{}}"#;

        let fixtures = [
            ("pointer_down", pointer_down_fixture, pointer_down),
            ("pointer_move", pointer_move_fixture, pointer_move),
            ("pointer_up", pointer_up_fixture, pointer_up),
            ("wheel_nudge", wheel_nudge_fixture, wheel_nudge),
            ("keyboard_resize", keyboard_resize_fixture, keyboard_resize),
            ("cancel", cancel_fixture, cancel),
            ("blur", blur_fixture, blur),
        ];

        for (name, fixture, expected) in fixtures {
            let parsed: PaneSemanticInputEvent =
                serde_json::from_str(fixture).expect("fixture should parse");
            assert_eq!(
                parsed, expected,
                "{name} fixture should match expected shape"
            );
            parsed.validate().expect("fixture should validate");
            let encoded = serde_json::to_string(&parsed).expect("event should encode");
            assert_eq!(encoded, fixture, "{name} fixture should be canonical");
        }
    }

    #[test]
    fn semantic_input_event_defaults_schema_version_to_current() {
        let fixture = r#"{"sequence":9,"modifiers":{"shift":false,"alt":false,"ctrl":false,"meta":false},"event":"blur","target":null,"extensions":{}}"#;
        let parsed: PaneSemanticInputEvent =
            serde_json::from_str(fixture).expect("fixture should parse");
        assert_eq!(
            parsed.schema_version,
            PANE_SEMANTIC_INPUT_EVENT_SCHEMA_VERSION
        );
        parsed.validate().expect("defaulted event should validate");
    }

    #[test]
    fn semantic_input_event_rejects_invalid_invariants() {
        let target = default_target();

        let mut schema_version = PaneSemanticInputEvent::new(
            1,
            PaneSemanticInputEventKind::Blur {
                target: Some(target),
            },
        );
        schema_version.schema_version = 99;
        assert_eq!(
            schema_version.validate(),
            Err(PaneSemanticInputEventError::UnsupportedSchemaVersion {
                version: 99,
                expected: PANE_SEMANTIC_INPUT_EVENT_SCHEMA_VERSION
            })
        );

        let sequence = PaneSemanticInputEvent::new(
            0,
            PaneSemanticInputEventKind::Blur {
                target: Some(target),
            },
        );
        assert_eq!(
            sequence.validate(),
            Err(PaneSemanticInputEventError::ZeroSequence)
        );

        let pointer = PaneSemanticInputEvent::new(
            2,
            PaneSemanticInputEventKind::PointerDown {
                target,
                pointer_id: 0,
                button: PanePointerButton::Primary,
                position: PanePointerPosition::new(0, 0),
            },
        );
        assert_eq!(
            pointer.validate(),
            Err(PaneSemanticInputEventError::ZeroPointerId)
        );

        let wheel = PaneSemanticInputEvent::new(
            3,
            PaneSemanticInputEventKind::WheelNudge { target, lines: 0 },
        );
        assert_eq!(
            wheel.validate(),
            Err(PaneSemanticInputEventError::ZeroWheelLines)
        );

        let keyboard = PaneSemanticInputEvent::new(
            4,
            PaneSemanticInputEventKind::KeyboardResize {
                target,
                direction: PaneResizeDirection::Decrease,
                units: 0,
            },
        );
        assert_eq!(
            keyboard.validate(),
            Err(PaneSemanticInputEventError::ZeroResizeUnits)
        );
    }

    fn pointer_down_event(
        sequence: u64,
        target: PaneResizeTarget,
        pointer_id: u32,
        x: i32,
        y: i32,
    ) -> PaneSemanticInputEvent {
        PaneSemanticInputEvent::new(
            sequence,
            PaneSemanticInputEventKind::PointerDown {
                target,
                pointer_id,
                button: PanePointerButton::Primary,
                position: PanePointerPosition::new(x, y),
            },
        )
    }

    fn pointer_move_event(
        sequence: u64,
        target: PaneResizeTarget,
        pointer_id: u32,
        x: i32,
        y: i32,
    ) -> PaneSemanticInputEvent {
        PaneSemanticInputEvent::new(
            sequence,
            PaneSemanticInputEventKind::PointerMove {
                target,
                pointer_id,
                position: PanePointerPosition::new(x, y),
                delta_x: 0,
                delta_y: 0,
            },
        )
    }

    fn pointer_up_event(
        sequence: u64,
        target: PaneResizeTarget,
        pointer_id: u32,
        x: i32,
        y: i32,
    ) -> PaneSemanticInputEvent {
        PaneSemanticInputEvent::new(
            sequence,
            PaneSemanticInputEventKind::PointerUp {
                target,
                pointer_id,
                button: PanePointerButton::Primary,
                position: PanePointerPosition::new(x, y),
            },
        )
    }

    #[test]
    fn drag_resize_machine_full_lifecycle_commit() {
        let mut machine = PaneDragResizeMachine::default();
        let target = default_target();

        let down = machine
            .apply_event(&pointer_down_event(1, target, 10, 10, 4))
            .expect("down should arm");
        assert_eq!(down.transition_id, 1);
        assert_eq!(down.sequence, 1);
        assert_eq!(machine.state(), down.to);
        assert!(matches!(
            down.effect,
            PaneDragResizeEffect::Armed {
                target: t,
                pointer_id: 10,
                origin: PanePointerPosition { x: 10, y: 4 }
            } if t == target
        ));

        let below_threshold = machine
            .apply_event(&pointer_move_event(2, target, 10, 11, 4))
            .expect("small move should not start drag");
        assert_eq!(
            below_threshold.effect,
            PaneDragResizeEffect::Noop {
                reason: PaneDragResizeNoopReason::ThresholdNotReached
            }
        );
        assert!(matches!(machine.state(), PaneDragResizeState::Armed { .. }));

        let drag_start = machine
            .apply_event(&pointer_move_event(3, target, 10, 13, 4))
            .expect("large move should start drag");
        assert!(matches!(
            drag_start.effect,
            PaneDragResizeEffect::DragStarted {
                target: t,
                pointer_id: 10,
                total_delta_x: 3,
                total_delta_y: 0,
                ..
            } if t == target
        ));
        assert!(matches!(
            machine.state(),
            PaneDragResizeState::Dragging { .. }
        ));

        let drag_update = machine
            .apply_event(&pointer_move_event(4, target, 10, 15, 6))
            .expect("drag move should update");
        assert!(matches!(
            drag_update.effect,
            PaneDragResizeEffect::DragUpdated {
                target: t,
                pointer_id: 10,
                delta_x: 2,
                delta_y: 2,
                total_delta_x: 5,
                total_delta_y: 2,
                ..
            } if t == target
        ));

        let commit = machine
            .apply_event(&pointer_up_event(5, target, 10, 16, 6))
            .expect("up should commit drag");
        assert!(matches!(
            commit.effect,
            PaneDragResizeEffect::Committed {
                target: t,
                pointer_id: 10,
                total_delta_x: 6,
                total_delta_y: 2,
                ..
            } if t == target
        ));
        assert_eq!(machine.state(), PaneDragResizeState::Idle);
    }

    #[test]
    fn drag_resize_machine_cancel_and_blur_paths_are_reason_coded() {
        let target = default_target();

        let mut cancel_machine = PaneDragResizeMachine::default();
        cancel_machine
            .apply_event(&pointer_down_event(1, target, 1, 2, 2))
            .expect("down should arm");
        let cancel = cancel_machine
            .apply_event(&PaneSemanticInputEvent::new(
                2,
                PaneSemanticInputEventKind::Cancel {
                    target: Some(target),
                    reason: PaneCancelReason::FocusLost,
                },
            ))
            .expect("cancel should reset to idle");
        assert_eq!(cancel_machine.state(), PaneDragResizeState::Idle);
        assert_eq!(
            cancel.effect,
            PaneDragResizeEffect::Canceled {
                target: Some(target),
                pointer_id: Some(1),
                reason: PaneCancelReason::FocusLost
            }
        );

        let mut blur_machine = PaneDragResizeMachine::default();
        blur_machine
            .apply_event(&pointer_down_event(3, target, 2, 5, 5))
            .expect("down should arm");
        blur_machine
            .apply_event(&pointer_move_event(4, target, 2, 8, 5))
            .expect("move should start dragging");
        let blur = blur_machine
            .apply_event(&PaneSemanticInputEvent::new(
                5,
                PaneSemanticInputEventKind::Blur {
                    target: Some(target),
                },
            ))
            .expect("blur should cancel active drag");
        assert_eq!(blur_machine.state(), PaneDragResizeState::Idle);
        assert_eq!(
            blur.effect,
            PaneDragResizeEffect::Canceled {
                target: Some(target),
                pointer_id: Some(2),
                reason: PaneCancelReason::Blur
            }
        );
    }

    #[test]
    fn drag_resize_machine_duplicate_end_and_pointer_mismatch_are_safe_noops() {
        let mut machine = PaneDragResizeMachine::default();
        let target = default_target();

        machine
            .apply_event(&pointer_down_event(1, target, 9, 0, 0))
            .expect("down should arm");

        let mismatch = machine
            .apply_event(&pointer_move_event(2, target, 99, 3, 0))
            .expect("mismatch should be ignored");
        assert_eq!(
            mismatch.effect,
            PaneDragResizeEffect::Noop {
                reason: PaneDragResizeNoopReason::PointerMismatch
            }
        );
        assert!(matches!(machine.state(), PaneDragResizeState::Armed { .. }));

        machine
            .apply_event(&pointer_move_event(3, target, 9, 3, 0))
            .expect("drag should start");
        machine
            .apply_event(&pointer_up_event(4, target, 9, 3, 0))
            .expect("up should commit");
        assert_eq!(machine.state(), PaneDragResizeState::Idle);

        let duplicate_end = machine
            .apply_event(&pointer_up_event(5, target, 9, 3, 0))
            .expect("duplicate end should noop");
        assert_eq!(
            duplicate_end.effect,
            PaneDragResizeEffect::Noop {
                reason: PaneDragResizeNoopReason::IdleWithoutActiveDrag
            }
        );
    }

    #[test]
    fn drag_resize_machine_discrete_inputs_in_idle_and_validation_errors() {
        let mut machine = PaneDragResizeMachine::default();
        let target = default_target();

        let keyboard = machine
            .apply_event(&PaneSemanticInputEvent::new(
                1,
                PaneSemanticInputEventKind::KeyboardResize {
                    target,
                    direction: PaneResizeDirection::Increase,
                    units: 2,
                },
            ))
            .expect("keyboard resize should apply in idle");
        assert_eq!(
            keyboard.effect,
            PaneDragResizeEffect::KeyboardApplied {
                target,
                direction: PaneResizeDirection::Increase,
                units: 2
            }
        );
        assert_eq!(machine.state(), PaneDragResizeState::Idle);

        let wheel = machine
            .apply_event(&PaneSemanticInputEvent::new(
                2,
                PaneSemanticInputEventKind::WheelNudge { target, lines: -1 },
            ))
            .expect("wheel nudge should apply in idle");
        assert_eq!(
            wheel.effect,
            PaneDragResizeEffect::WheelApplied { target, lines: -1 }
        );

        let invalid_pointer = PaneSemanticInputEvent::new(
            3,
            PaneSemanticInputEventKind::PointerDown {
                target,
                pointer_id: 0,
                button: PanePointerButton::Primary,
                position: PanePointerPosition::new(0, 0),
            },
        );
        let err = machine
            .apply_event(&invalid_pointer)
            .expect_err("invalid input should be rejected");
        assert_eq!(
            err,
            PaneDragResizeMachineError::InvalidEvent(PaneSemanticInputEventError::ZeroPointerId)
        );

        assert_eq!(
            PaneDragResizeMachine::new(0).expect_err("zero threshold should fail"),
            PaneDragResizeMachineError::InvalidDragThreshold { threshold: 0 }
        );
    }

    proptest! {
        #[test]
        fn ratio_is_always_reduced(numerator in 1u32..100_000, denominator in 1u32..100_000) {
            let ratio = PaneSplitRatio::new(numerator, denominator).expect("positive ratio must be valid");
            let gcd = gcd_u32(ratio.numerator(), ratio.denominator());
            prop_assert_eq!(gcd, 1);
        }

        #[test]
        fn allocator_produces_monotonic_ids(
            start in 1u64..1_000_000,
            count in 1usize..64,
        ) {
            let mut allocator = PaneIdAllocator::with_next(PaneId::new(start).expect("start must be valid"));
            let mut prev = 0u64;
            for _ in 0..count {
                let current = allocator.allocate().expect("allocation must succeed").get();
                prop_assert!(current > prev);
                prev = current;
            }
        }

        #[test]
        fn split_solver_preserves_available_space(
            numerator in 1u32..64,
            denominator in 1u32..64,
            first_min in 0u16..40,
            second_min in 0u16..40,
            available in 0u16..80,
        ) {
            let ratio = PaneSplitRatio::new(numerator, denominator).expect("ratio must be valid");
            prop_assume!(first_min.saturating_add(second_min) <= available);

            let (first_size, second_size) = solve_split_sizes(
                id(1),
                SplitAxis::Horizontal,
                available,
                ratio,
                AxisBounds { min: first_min, max: None },
                AxisBounds { min: second_min, max: None },
            ).expect("feasible split should solve");

            prop_assert_eq!(first_size.saturating_add(second_size), available);
            prop_assert!(first_size >= first_min);
            prop_assert!(second_size >= second_min);
        }

        #[test]
        fn split_then_close_round_trip_preserves_validity(
            numerator in 1u32..32,
            denominator in 1u32..32,
            incoming_first in any::<bool>(),
        ) {
            let mut tree = PaneTree::singleton("root");
            let placement = if incoming_first {
                PanePlacement::IncomingFirst
            } else {
                PanePlacement::ExistingFirst
            };
            let ratio = PaneSplitRatio::new(numerator, denominator).expect("ratio must be valid");

            tree.apply_operation(
                1,
                PaneOperation::SplitLeaf {
                    target: id(1),
                    axis: SplitAxis::Horizontal,
                    ratio,
                    placement,
                    new_leaf: PaneLeaf::new("extra"),
                },
            ).expect("split should succeed");

            let split_root_id = tree.root();
            let split_root = tree.node(split_root_id).expect("split root exists");
            let PaneNodeKind::Split(split) = &split_root.kind else {
                unreachable!("root should be split");
            };
            let extra_leaf_id = if split.first == id(1) {
                split.second
            } else {
                split.first
            };

            tree.apply_operation(2, PaneOperation::CloseNode { target: extra_leaf_id })
                .expect("close should succeed");

            prop_assert_eq!(tree.root(), id(1));
            prop_assert!(matches!(
                tree.node(id(1)).map(|node| &node.kind),
                Some(PaneNodeKind::Leaf(_))
            ));
            prop_assert!(tree.validate().is_ok());
        }

        #[test]
        fn transaction_rollback_restores_initial_state_hash(
            numerator in 1u32..64,
            denominator in 1u32..64,
            incoming_first in any::<bool>(),
        ) {
            let base = PaneTree::singleton("root");
            let initial_hash = base.state_hash();
            let mut tx = base.begin_transaction(90);
            let placement = if incoming_first {
                PanePlacement::IncomingFirst
            } else {
                PanePlacement::ExistingFirst
            };

            tx.apply_operation(
                1,
                PaneOperation::SplitLeaf {
                    target: id(1),
                    axis: SplitAxis::Horizontal,
                    ratio: PaneSplitRatio::new(numerator, denominator).expect("valid ratio"),
                    placement,
                    new_leaf: PaneLeaf::new("new"),
                },
            ).expect("split should succeed");

            let rolled_back = tx.rollback();
            prop_assert_eq!(rolled_back.tree.state_hash(), initial_hash);
            prop_assert_eq!(rolled_back.tree.root(), id(1));
            prop_assert!(rolled_back.tree.validate().is_ok());
        }

        #[test]
        fn repair_safe_is_deterministic_under_recoverable_damage(
            numerator in 1u32..32,
            denominator in 1u32..32,
            add_orphan in any::<bool>(),
            mismatch_parent in any::<bool>(),
        ) {
            let mut snapshot = make_valid_snapshot();
            for node in &mut snapshot.nodes {
                if node.id == id(1) {
                    let PaneNodeKind::Split(split) = &mut node.kind else {
                        unreachable!("root should be split");
                    };
                    split.ratio = PaneSplitRatio {
                        numerator: numerator.saturating_mul(2),
                        denominator: denominator.saturating_mul(2),
                    };
                }
                if mismatch_parent && node.id == id(2) {
                    node.parent = Some(id(3));
                }
            }
            if add_orphan {
                snapshot
                    .nodes
                    .push(PaneNodeRecord::leaf(id(10), None, PaneLeaf::new("orphan")));
                snapshot.next_id = id(11);
            }

            let first = snapshot.clone().repair_safe().expect("first repair should succeed");
            let second = snapshot.repair_safe().expect("second repair should succeed");

            prop_assert_eq!(first.tree.state_hash(), second.tree.state_hash());
            prop_assert_eq!(first.actions, second.actions);
            prop_assert_eq!(first.report_after, second.report_after);
        }
    }
}

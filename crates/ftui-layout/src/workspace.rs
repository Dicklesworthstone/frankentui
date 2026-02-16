//! Persisted workspace schema v1 with versioning and migration scaffolding.
//!
//! A [`WorkspaceSnapshot`] wraps the pane tree snapshot with workspace-level
//! metadata, active pane tracking, and forward-compatible extension bags.
//!
//! # Schema Versioning Policy
//!
//! - **Additive fields** may be carried in `extensions` maps without a version bump.
//! - **Breaking changes** (field removal, semantic changes) require incrementing
//!   [`WORKSPACE_SCHEMA_VERSION`] and adding a migration path.
//! - All snapshots carry their schema version; loaders reject unknown versions
//!   with actionable diagnostics.
//!
//! # Usage
//!
//! ```
//! use ftui_layout::workspace::{WorkspaceSnapshot, WorkspaceMetadata, WORKSPACE_SCHEMA_VERSION};
//! use ftui_layout::pane::{PaneTreeSnapshot, PaneId, PaneNodeRecord, PaneLeaf, PANE_TREE_SCHEMA_VERSION};
//! use std::collections::BTreeMap;
//!
//! let tree = PaneTreeSnapshot {
//!     schema_version: PANE_TREE_SCHEMA_VERSION,
//!     root: PaneId::default(),
//!     next_id: PaneId::new(2).unwrap(),
//!     nodes: vec![PaneNodeRecord::leaf(PaneId::default(), None, PaneLeaf::new("main"))],
//!     extensions: BTreeMap::new(),
//! };
//!
//! let snapshot = WorkspaceSnapshot::new(tree, WorkspaceMetadata::new("my-workspace"));
//! assert_eq!(snapshot.schema_version, WORKSPACE_SCHEMA_VERSION);
//!
//! // Validate the snapshot
//! let result = snapshot.validate();
//! assert!(result.is_ok());
//! ```

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::pane::{
    PANE_TREE_SCHEMA_VERSION, PaneId, PaneInteractionTimeline, PaneModelError, PaneNodeKind,
    PaneTree, PaneTreeSnapshot,
};

/// Current workspace schema version.
pub const WORKSPACE_SCHEMA_VERSION: u16 = 1;

// =========================================================================
// Core schema types
// =========================================================================

/// Persisted workspace state, wrapping a pane tree with metadata.
///
/// Forward-compatible: unknown fields land in `extensions` for round-tripping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    /// Schema version for migration detection.
    #[serde(default = "default_workspace_version")]
    pub schema_version: u16,
    /// The pane tree layout.
    pub pane_tree: PaneTreeSnapshot,
    /// Which pane had focus when the workspace was persisted.
    #[serde(default)]
    pub active_pane_id: Option<PaneId>,
    /// Workspace metadata (name, timestamps, host info).
    pub metadata: WorkspaceMetadata,
    /// Persistent pane interaction timeline for undo/redo/replay.
    #[serde(default)]
    pub interaction_timeline: PaneInteractionTimeline,
    /// Forward-compatible extension bag.
    #[serde(default)]
    pub extensions: BTreeMap<String, String>,
}

fn default_workspace_version() -> u16 {
    WORKSPACE_SCHEMA_VERSION
}

impl WorkspaceSnapshot {
    /// Create a new v1 workspace snapshot.
    #[must_use]
    pub fn new(pane_tree: PaneTreeSnapshot, metadata: WorkspaceMetadata) -> Self {
        Self {
            schema_version: WORKSPACE_SCHEMA_VERSION,
            pane_tree,
            active_pane_id: None,
            metadata,
            interaction_timeline: PaneInteractionTimeline::default(),
            extensions: BTreeMap::new(),
        }
    }

    /// Create a snapshot with a focused pane.
    #[must_use]
    pub fn with_active_pane(mut self, pane_id: PaneId) -> Self {
        self.active_pane_id = Some(pane_id);
        self
    }

    /// Validate the snapshot against schema and structural invariants.
    pub fn validate(&self) -> Result<(), WorkspaceValidationError> {
        // Version check
        if self.schema_version != WORKSPACE_SCHEMA_VERSION {
            return Err(WorkspaceValidationError::UnsupportedVersion {
                found: self.schema_version,
                expected: WORKSPACE_SCHEMA_VERSION,
            });
        }

        // Pane tree version check
        if self.pane_tree.schema_version != PANE_TREE_SCHEMA_VERSION {
            return Err(WorkspaceValidationError::PaneTreeVersionMismatch {
                found: self.pane_tree.schema_version,
                expected: PANE_TREE_SCHEMA_VERSION,
            });
        }

        // Pane tree structural validation
        let report = self.pane_tree.invariant_report();
        if report.has_errors() {
            return Err(WorkspaceValidationError::PaneTreeInvalid {
                issue_count: report.issues.len(),
                first_issue: report
                    .issues
                    .first()
                    .map(|i| format!("{:?}", i.code))
                    .unwrap_or_default(),
            });
        }

        // Active pane must exist in the tree if set
        if let Some(active_id) = self.active_pane_id {
            let found = self.pane_tree.nodes.iter().any(|n| n.id == active_id);
            if !found {
                return Err(WorkspaceValidationError::ActivePaneNotFound { pane_id: active_id });
            }
            // Active pane should be a leaf (not a split)
            let is_leaf = self
                .pane_tree
                .nodes
                .iter()
                .find(|n| n.id == active_id)
                .map(|n| matches!(n.kind, PaneNodeKind::Leaf(_)))
                .unwrap_or(false);
            if !is_leaf {
                return Err(WorkspaceValidationError::ActivePaneNotLeaf { pane_id: active_id });
            }
        }

        // Metadata validation
        if self.metadata.name.is_empty() {
            return Err(WorkspaceValidationError::EmptyWorkspaceName);
        }

        if self.interaction_timeline.cursor > self.interaction_timeline.entries.len() {
            return Err(WorkspaceValidationError::TimelineCursorOutOfRange {
                cursor: self.interaction_timeline.cursor,
                len: self.interaction_timeline.entries.len(),
            });
        }

        // Timeline must be internally replayable and agree with persisted pane_tree state.
        if self.interaction_timeline.baseline.is_some()
            || !self.interaction_timeline.entries.is_empty()
        {
            let replayed_tree = self.interaction_timeline.replay().map_err(|err| {
                WorkspaceValidationError::TimelineReplayFailed {
                    reason: err.to_string(),
                }
            })?;
            let pane_tree = PaneTree::from_snapshot(self.pane_tree.clone())
                .map_err(WorkspaceValidationError::PaneModel)?;
            let pane_tree_hash = pane_tree.state_hash();
            let replay_hash = replayed_tree.state_hash();
            if replay_hash != pane_tree_hash {
                return Err(WorkspaceValidationError::TimelineReplayMismatch {
                    pane_tree_hash,
                    replay_hash,
                });
            }
        }

        Ok(())
    }

    /// Canonicalize for deterministic serialization.
    pub fn canonicalize(&mut self) {
        self.pane_tree.canonicalize();
    }

    /// Deterministic hash for state diagnostics.
    #[must_use]
    pub fn state_hash(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.schema_version.hash(&mut hasher);
        self.pane_tree.state_hash().hash(&mut hasher);
        self.active_pane_id.map(|id| id.get()).hash(&mut hasher);
        self.metadata.name.hash(&mut hasher);
        self.metadata.created_generation.hash(&mut hasher);
        for (k, v) in &self.extensions {
            k.hash(&mut hasher);
            v.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Count of leaf panes in the tree.
    #[must_use]
    pub fn leaf_count(&self) -> usize {
        self.pane_tree
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, PaneNodeKind::Leaf(_)))
            .count()
    }
}

// =========================================================================
// Metadata
// =========================================================================

/// Workspace metadata for provenance and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMetadata {
    /// Human-readable workspace name.
    pub name: String,
    /// Monotonic generation counter (incremented on each save).
    #[serde(default)]
    pub created_generation: u64,
    /// Last-saved generation counter.
    #[serde(default)]
    pub saved_generation: u64,
    /// Application version that created/saved this workspace.
    #[serde(default)]
    pub app_version: String,
    /// Forward-compatible custom tags.
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

impl WorkspaceMetadata {
    /// Create metadata with a workspace name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            created_generation: 0,
            saved_generation: 0,
            app_version: String::new(),
            tags: BTreeMap::new(),
        }
    }

    /// Set the application version.
    #[must_use]
    pub fn with_app_version(mut self, version: impl Into<String>) -> Self {
        self.app_version = version.into();
        self
    }

    /// Increment the save generation counter.
    pub fn increment_generation(&mut self) {
        self.saved_generation = self.saved_generation.saturating_add(1);
    }
}

// =========================================================================
// Validation errors
// =========================================================================

/// Errors from workspace validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceValidationError {
    /// Schema version is not supported.
    UnsupportedVersion { found: u16, expected: u16 },
    /// Pane tree schema version mismatch.
    PaneTreeVersionMismatch { found: u16, expected: u16 },
    /// Pane tree has structural invariant violations.
    PaneTreeInvalid {
        issue_count: usize,
        first_issue: String,
    },
    /// Active pane ID does not exist in the tree.
    ActivePaneNotFound { pane_id: PaneId },
    /// Active pane is a split node, not a leaf.
    ActivePaneNotLeaf { pane_id: PaneId },
    /// Workspace name is empty.
    EmptyWorkspaceName,
    /// Timeline cursor is outside the recorded history bounds.
    TimelineCursorOutOfRange { cursor: usize, len: usize },
    /// Timeline replay failed (missing/invalid baseline or invalid operation).
    TimelineReplayFailed { reason: String },
    /// Timeline replay does not match the persisted pane tree state.
    TimelineReplayMismatch {
        pane_tree_hash: u64,
        replay_hash: u64,
    },
    /// Pane model error from tree operations.
    PaneModel(PaneModelError),
}

impl fmt::Display for WorkspaceValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { found, expected } => {
                write!(
                    f,
                    "unsupported workspace schema version {found} (expected {expected})"
                )
            }
            Self::PaneTreeVersionMismatch { found, expected } => {
                write!(
                    f,
                    "pane tree schema version {found} does not match expected {expected}"
                )
            }
            Self::PaneTreeInvalid {
                issue_count,
                first_issue,
            } => {
                write!(
                    f,
                    "pane tree has {issue_count} invariant violation(s), first: {first_issue}"
                )
            }
            Self::ActivePaneNotFound { pane_id } => {
                write!(f, "active pane {} not found in tree", pane_id.get())
            }
            Self::ActivePaneNotLeaf { pane_id } => {
                write!(f, "active pane {} is a split, not a leaf", pane_id.get())
            }
            Self::EmptyWorkspaceName => write!(f, "workspace name must not be empty"),
            Self::TimelineCursorOutOfRange { cursor, len } => write!(
                f,
                "interaction timeline cursor {cursor} out of bounds for history length {len}"
            ),
            Self::TimelineReplayFailed { reason } => {
                write!(f, "interaction timeline replay failed: {reason}")
            }
            Self::TimelineReplayMismatch {
                pane_tree_hash,
                replay_hash,
            } => write!(
                f,
                "interaction timeline replay hash {replay_hash} does not match pane tree hash {pane_tree_hash}"
            ),
            Self::PaneModel(e) => write!(f, "pane model error: {e}"),
        }
    }
}

impl From<PaneModelError> for WorkspaceValidationError {
    fn from(err: PaneModelError) -> Self {
        Self::PaneModel(err)
    }
}

use std::fmt;

// =========================================================================
// Migration scaffolding
// =========================================================================

/// Result of attempting to migrate a workspace from an older schema version.
#[derive(Debug, Clone)]
pub struct MigrationResult {
    /// The migrated snapshot.
    pub snapshot: WorkspaceSnapshot,
    /// Source version before migration.
    pub from_version: u16,
    /// Target version after migration.
    pub to_version: u16,
    /// Warnings or notes from the migration.
    pub warnings: Vec<String>,
}

/// Errors from workspace migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceMigrationError {
    /// Version is not recognized or too old to migrate.
    UnsupportedVersion { version: u16 },
    /// Migration from the given version is not implemented.
    NoMigrationPath { from: u16, to: u16 },
    /// Deserialization failed during migration.
    DeserializationFailed { reason: String },
}

impl fmt::Display for WorkspaceMigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { version } => {
                write!(f, "unsupported schema version {version} for migration")
            }
            Self::NoMigrationPath { from, to } => {
                write!(f, "no migration path from v{from} to v{to}")
            }
            Self::DeserializationFailed { reason } => {
                write!(f, "deserialization failed during migration: {reason}")
            }
        }
    }
}

/// Attempt to migrate a workspace snapshot to the current schema version.
///
/// For v1 (current), this is a no-op identity migration. Future versions
/// will chain migrations through each intermediate version.
pub fn migrate_workspace(
    snapshot: WorkspaceSnapshot,
) -> Result<MigrationResult, WorkspaceMigrationError> {
    match snapshot.schema_version {
        WORKSPACE_SCHEMA_VERSION => {
            // Current version — no migration needed.
            Ok(MigrationResult {
                from_version: WORKSPACE_SCHEMA_VERSION,
                to_version: WORKSPACE_SCHEMA_VERSION,
                warnings: Vec::new(),
                snapshot,
            })
        }
        v if v > WORKSPACE_SCHEMA_VERSION => {
            Err(WorkspaceMigrationError::UnsupportedVersion { version: v })
        }
        v => Err(WorkspaceMigrationError::NoMigrationPath {
            from: v,
            to: WORKSPACE_SCHEMA_VERSION,
        }),
    }
}

/// Check whether a snapshot requires migration.
#[must_use]
pub fn needs_migration(snapshot: &WorkspaceSnapshot) -> bool {
    snapshot.schema_version != WORKSPACE_SCHEMA_VERSION
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::{
        PaneInteractionTimelineEntry, PaneLeaf, PaneNodeKind, PaneNodeRecord, PaneOperation,
        PaneSplit, PaneSplitRatio, PaneTree, SplitAxis,
    };

    fn minimal_tree() -> PaneTreeSnapshot {
        PaneTreeSnapshot {
            schema_version: PANE_TREE_SCHEMA_VERSION,
            root: PaneId::default(),
            next_id: PaneId::new(2).unwrap(),
            nodes: vec![PaneNodeRecord::leaf(
                PaneId::default(),
                None,
                PaneLeaf::new("main"),
            )],
            extensions: BTreeMap::new(),
        }
    }

    fn split_tree() -> PaneTreeSnapshot {
        let root_id = PaneId::new(1).unwrap();
        let left_id = PaneId::new(2).unwrap();
        let right_id = PaneId::new(3).unwrap();
        PaneTreeSnapshot {
            schema_version: PANE_TREE_SCHEMA_VERSION,
            root: root_id,
            next_id: PaneId::new(4).unwrap(),
            nodes: vec![
                PaneNodeRecord::split(
                    root_id,
                    None,
                    PaneSplit {
                        axis: SplitAxis::Horizontal,
                        ratio: PaneSplitRatio::new(1, 1).unwrap(),
                        first: left_id,
                        second: right_id,
                    },
                ),
                PaneNodeRecord::leaf(left_id, Some(root_id), PaneLeaf::new("left")),
                PaneNodeRecord::leaf(right_id, Some(root_id), PaneLeaf::new("right")),
            ],
            extensions: BTreeMap::new(),
        }
    }

    fn minimal_snapshot() -> WorkspaceSnapshot {
        WorkspaceSnapshot::new(minimal_tree(), WorkspaceMetadata::new("test"))
    }

    // ---- Construction ----

    #[test]
    fn new_snapshot_has_v1() {
        let snap = minimal_snapshot();
        assert_eq!(snap.schema_version, WORKSPACE_SCHEMA_VERSION);
        assert_eq!(snap.schema_version, 1);
    }

    #[test]
    fn with_active_pane_sets_id() {
        let id = PaneId::default();
        let snap = minimal_snapshot().with_active_pane(id);
        assert_eq!(snap.active_pane_id, Some(id));
    }

    #[test]
    fn metadata_new_defaults() {
        let meta = WorkspaceMetadata::new("ws");
        assert_eq!(meta.name, "ws");
        assert_eq!(meta.created_generation, 0);
        assert_eq!(meta.saved_generation, 0);
        assert!(meta.app_version.is_empty());
        assert!(meta.tags.is_empty());
    }

    #[test]
    fn metadata_with_app_version() {
        let meta = WorkspaceMetadata::new("ws").with_app_version("0.1.0");
        assert_eq!(meta.app_version, "0.1.0");
    }

    #[test]
    fn metadata_increment_generation() {
        let mut meta = WorkspaceMetadata::new("ws");
        meta.increment_generation();
        assert_eq!(meta.saved_generation, 1);
        meta.increment_generation();
        assert_eq!(meta.saved_generation, 2);
    }

    // ---- Validation ----

    #[test]
    fn validate_minimal_ok() {
        let snap = minimal_snapshot();
        assert!(snap.validate().is_ok());
    }

    #[test]
    fn validate_split_tree_ok() {
        let snap = WorkspaceSnapshot::new(split_tree(), WorkspaceMetadata::new("split"));
        assert!(snap.validate().is_ok());
    }

    #[test]
    fn validate_wrong_workspace_version() {
        let mut snap = minimal_snapshot();
        snap.schema_version = 99;
        let err = snap.validate().unwrap_err();
        assert!(matches!(
            err,
            WorkspaceValidationError::UnsupportedVersion {
                found: 99,
                expected: 1
            }
        ));
    }

    #[test]
    fn validate_wrong_pane_tree_version() {
        let mut snap = minimal_snapshot();
        snap.pane_tree.schema_version = 42;
        let err = snap.validate().unwrap_err();
        assert!(matches!(
            err,
            WorkspaceValidationError::PaneTreeVersionMismatch { .. }
        ));
    }

    #[test]
    fn validate_active_pane_not_found() {
        let snap = minimal_snapshot().with_active_pane(PaneId::new(999).unwrap());
        let err = snap.validate().unwrap_err();
        assert!(matches!(
            err,
            WorkspaceValidationError::ActivePaneNotFound { .. }
        ));
    }

    #[test]
    fn validate_active_pane_is_split() {
        let root_id = PaneId::new(1).unwrap();
        let snap = WorkspaceSnapshot::new(split_tree(), WorkspaceMetadata::new("s"))
            .with_active_pane(root_id);
        let err = snap.validate().unwrap_err();
        assert!(matches!(
            err,
            WorkspaceValidationError::ActivePaneNotLeaf { .. }
        ));
    }

    #[test]
    fn validate_active_pane_leaf_ok() {
        let left_id = PaneId::new(2).unwrap();
        let snap = WorkspaceSnapshot::new(split_tree(), WorkspaceMetadata::new("s"))
            .with_active_pane(left_id);
        assert!(snap.validate().is_ok());
    }

    #[test]
    fn validate_empty_name() {
        let snap = WorkspaceSnapshot::new(minimal_tree(), WorkspaceMetadata::new(""));
        let err = snap.validate().unwrap_err();
        assert!(matches!(err, WorkspaceValidationError::EmptyWorkspaceName));
    }

    #[test]
    fn validate_timeline_cursor_out_of_range() {
        let mut snap = minimal_snapshot();
        snap.interaction_timeline.cursor = 2;
        snap.interaction_timeline
            .entries
            .push(PaneInteractionTimelineEntry {
                sequence: 1,
                operation_id: 10,
                operation: PaneOperation::NormalizeRatios,
                before_hash: 1,
                after_hash: 2,
            });
        let err = snap.validate().unwrap_err();
        assert!(matches!(
            err,
            WorkspaceValidationError::TimelineCursorOutOfRange { .. }
        ));
    }

    #[test]
    fn validate_timeline_with_entries_requires_baseline() {
        let mut snap = minimal_snapshot();
        snap.interaction_timeline.cursor = 1;
        snap.interaction_timeline
            .entries
            .push(PaneInteractionTimelineEntry {
                sequence: 1,
                operation_id: 10,
                operation: PaneOperation::NormalizeRatios,
                before_hash: 1,
                after_hash: 2,
            });
        let err = snap.validate().unwrap_err();
        assert!(matches!(
            err,
            WorkspaceValidationError::TimelineReplayFailed { .. }
        ));
    }

    #[test]
    fn validate_rejects_timeline_replay_mismatch() {
        let mut snap = WorkspaceSnapshot::new(split_tree(), WorkspaceMetadata::new("mismatch"));
        let baseline_tree = PaneTree::from_snapshot(minimal_tree())
            .expect("minimal pane tree snapshot should load");
        snap.interaction_timeline = PaneInteractionTimeline::with_baseline(&baseline_tree);
        let err = snap.validate().unwrap_err();
        assert!(matches!(
            err,
            WorkspaceValidationError::TimelineReplayMismatch { .. }
        ));
    }

    // ---- Serialization ----
    //
    // Roundtrip coverage verifies pane/workspace snapshots remain portable across
    // hosts and can be parsed back without lossy key collisions.

    #[test]
    fn serde_serialize_minimal_succeeds() {
        let snap = minimal_snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"schema_version\":1"));
        assert!(json.contains("\"name\":\"test\""));
    }

    #[test]
    fn serde_serialize_split_tree_succeeds() {
        let snap = WorkspaceSnapshot::new(split_tree(), WorkspaceMetadata::new("split"))
            .with_active_pane(PaneId::new(2).unwrap());
        let json = serde_json::to_string_pretty(&snap).unwrap();
        assert!(json.contains("\"active_pane_id\": 2"));
        assert!(json.contains("\"name\": \"split\""));
    }

    #[test]
    fn serde_roundtrip_snapshot_preserves_leaf_and_node_extensions() {
        let mut tree = minimal_tree();
        tree.extensions
            .insert("tree_scope".to_string(), "tree".to_string());
        tree.nodes[0]
            .extensions
            .insert("node_scope".to_string(), "node".to_string());
        let PaneNodeKind::Leaf(leaf) = &mut tree.nodes[0].kind else {
            panic!("minimal tree root should be leaf");
        };
        leaf.extensions
            .insert("leaf_scope".to_string(), "leaf".to_string());

        let mut snap = WorkspaceSnapshot::new(tree, WorkspaceMetadata::new("roundtrip"));
        snap.extensions
            .insert("workspace_scope".to_string(), "workspace".to_string());
        snap.metadata
            .tags
            .insert("metadata_scope".to_string(), "metadata".to_string());

        let json = serde_json::to_string(&snap).unwrap();
        let decoded: WorkspaceSnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(
            decoded
                .extensions
                .get("workspace_scope")
                .map(std::string::String::as_str),
            Some("workspace")
        );
        assert_eq!(
            decoded
                .pane_tree
                .extensions
                .get("tree_scope")
                .map(std::string::String::as_str),
            Some("tree")
        );
        assert_eq!(
            decoded.pane_tree.nodes[0]
                .extensions
                .get("node_scope")
                .map(std::string::String::as_str),
            Some("node")
        );
        let PaneNodeKind::Leaf(decoded_leaf) = &decoded.pane_tree.nodes[0].kind else {
            panic!("decoded minimal tree root should be leaf");
        };
        assert_eq!(
            decoded_leaf
                .extensions
                .get("leaf_scope")
                .map(std::string::String::as_str),
            Some("leaf")
        );
    }

    #[test]
    fn serde_deserialize_from_handcrafted_json() {
        // Hand-crafted JSON matching the expected wire format, with only
        // one `extensions` per node (PaneNodeRecord level, not PaneLeaf).
        let json = r#"{
            "schema_version": 1,
            "pane_tree": {
                "schema_version": 1,
                "root": 1,
                "next_id": 2,
                "nodes": [
                    {"id": 1, "kind": "leaf", "surface_key": "main"}
                ]
            },
            "active_pane_id": 1,
            "metadata": {"name": "from-json"},
            "extensions": {"extra": "data"}
        }"#;
        let snap: WorkspaceSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.schema_version, 1);
        assert_eq!(snap.active_pane_id, Some(PaneId::default()));
        assert_eq!(snap.metadata.name, "from-json");
        assert_eq!(snap.extensions.get("extra").unwrap(), "data");
        assert_eq!(snap.leaf_count(), 1);
    }

    #[test]
    fn serde_workspace_extensions_and_tags_preserved() {
        let json = r#"{
            "pane_tree": {
                "root": 1,
                "next_id": 2,
                "nodes": [{"id": 1, "kind": "leaf", "surface_key": "main"}]
            },
            "metadata": {
                "name": "ext-test",
                "tags": {"custom": "tag"}
            },
            "extensions": {"future_field": "value"}
        }"#;
        let snap: WorkspaceSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.extensions.get("future_field").unwrap(), "value");
        assert_eq!(snap.metadata.tags.get("custom").unwrap(), "tag");
    }

    #[test]
    fn serde_metadata_roundtrip() {
        // WorkspaceMetadata has no flatten issues — full roundtrip works.
        let mut meta = WorkspaceMetadata::new("round-trip");
        meta.app_version = "1.0.0".to_string();
        meta.created_generation = 5;
        meta.saved_generation = 10;
        meta.tags.insert("k".to_string(), "v".to_string());
        let json = serde_json::to_string(&meta).unwrap();
        let deser: WorkspaceMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, deser);
    }

    #[test]
    fn serde_missing_optional_fields_default() {
        // JSON with minimal fields — optional ones should get defaults
        let json = r#"{
            "pane_tree": {
                "root": 1,
                "next_id": 2,
                "nodes": [{"id": 1, "kind": "leaf", "surface_key": "main"}]
            },
            "metadata": {"name": "test"}
        }"#;
        let snap: WorkspaceSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.schema_version, WORKSPACE_SCHEMA_VERSION);
        assert!(snap.active_pane_id.is_none());
        assert!(snap.extensions.is_empty());
    }

    // ---- Deterministic hashing ----

    #[test]
    fn state_hash_deterministic() {
        let s1 = minimal_snapshot();
        let s2 = minimal_snapshot();
        assert_eq!(s1.state_hash(), s2.state_hash());
    }

    #[test]
    fn state_hash_changes_with_active_pane() {
        let s1 = minimal_snapshot();
        let s2 = minimal_snapshot().with_active_pane(PaneId::default());
        assert_ne!(s1.state_hash(), s2.state_hash());
    }

    #[test]
    fn state_hash_changes_with_name() {
        let s1 = WorkspaceSnapshot::new(minimal_tree(), WorkspaceMetadata::new("a"));
        let s2 = WorkspaceSnapshot::new(minimal_tree(), WorkspaceMetadata::new("b"));
        assert_ne!(s1.state_hash(), s2.state_hash());
    }

    // ---- Canonicalization ----

    #[test]
    fn canonicalize_sorts_nodes() {
        let mut snap = WorkspaceSnapshot::new(split_tree(), WorkspaceMetadata::new("s"));
        // Reverse the node order
        snap.pane_tree.nodes.reverse();
        snap.canonicalize();
        let ids: Vec<u64> = snap.pane_tree.nodes.iter().map(|n| n.id.get()).collect();
        assert!(
            ids.windows(2).all(|w| w[0] <= w[1]),
            "nodes should be sorted by ID"
        );
    }

    // ---- Leaf count ----

    #[test]
    fn leaf_count_single() {
        let snap = minimal_snapshot();
        assert_eq!(snap.leaf_count(), 1);
    }

    #[test]
    fn leaf_count_split() {
        let snap = WorkspaceSnapshot::new(split_tree(), WorkspaceMetadata::new("s"));
        assert_eq!(snap.leaf_count(), 2);
    }

    // ---- Migration ----

    #[test]
    fn migrate_v1_is_noop() {
        let snap = minimal_snapshot();
        let result = migrate_workspace(snap.clone()).unwrap();
        assert_eq!(result.from_version, 1);
        assert_eq!(result.to_version, 1);
        assert_eq!(result.snapshot, snap);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn migrate_future_version_fails() {
        let mut snap = minimal_snapshot();
        snap.schema_version = 99;
        let err = migrate_workspace(snap).unwrap_err();
        assert!(matches!(
            err,
            WorkspaceMigrationError::UnsupportedVersion { version: 99 }
        ));
    }

    #[test]
    fn migrate_old_version_fails_no_path() {
        let mut snap = minimal_snapshot();
        snap.schema_version = 0;
        let err = migrate_workspace(snap).unwrap_err();
        assert!(matches!(
            err,
            WorkspaceMigrationError::NoMigrationPath { from: 0, to: 1 }
        ));
    }

    #[test]
    fn needs_migration_false_for_current() {
        let snap = minimal_snapshot();
        assert!(!needs_migration(&snap));
    }

    #[test]
    fn needs_migration_true_for_old() {
        let mut snap = minimal_snapshot();
        snap.schema_version = 0;
        assert!(needs_migration(&snap));
    }

    // ---- Error display ----

    #[test]
    fn validation_error_display() {
        let err = WorkspaceValidationError::UnsupportedVersion {
            found: 99,
            expected: 1,
        };
        let msg = format!("{err}");
        assert!(msg.contains("99"));
        assert!(msg.contains("1"));
    }

    #[test]
    fn migration_error_display() {
        let err = WorkspaceMigrationError::NoMigrationPath { from: 0, to: 1 };
        let msg = format!("{err}");
        assert!(msg.contains("v0"));
        assert!(msg.contains("v1"));
    }

    #[test]
    fn validation_error_from_pane_model() {
        let pane_err = PaneModelError::ZeroPaneId;
        let ws_err: WorkspaceValidationError = pane_err.into();
        assert!(matches!(ws_err, WorkspaceValidationError::PaneModel(_)));
    }

    // ---- Determinism ----

    #[test]
    fn identical_inputs_identical_validation() {
        let s1 = minimal_snapshot();
        let s2 = minimal_snapshot();
        assert_eq!(s1.validate().is_ok(), s2.validate().is_ok());
    }

    #[test]
    fn identical_inputs_identical_migration() {
        let s1 = minimal_snapshot();
        let s2 = minimal_snapshot();
        let r1 = migrate_workspace(s1).unwrap();
        let r2 = migrate_workspace(s2).unwrap();
        assert_eq!(r1.snapshot, r2.snapshot);
    }
}

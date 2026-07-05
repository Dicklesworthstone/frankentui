//! Prototype: persistent / versioned `PaneTree` with structural sharing.
//!
//! This module is the spike deliverable for bead `bd-1k7ek.5`. It answers a
//! single strategic question: does a persistent (structurally shared) pane tree
//! buy enough asymptotic value — specifically **replay-free `O(1)` undo/redo** —
//! to justify its complexity and memory footprint versus the checkpointed
//! [`PaneInteractionTimeline`](crate::pane::PaneInteractionTimeline) replay path?
//!
//! # Design
//!
//! The canonical [`PaneTree`](crate::pane::PaneTree) stores nodes in a flat
//! `BTreeMap<PaneId, PaneNodeRecord>` with explicit parent pointers. That shape
//! is excellent for `O(1)` id lookup but is *not* naturally persistent: cloning
//! the whole map to snapshot a version is `O(nodes)`, and the timeline therefore
//! recovers historical states by **replaying** operations forward from the
//! nearest checkpoint.
//!
//! [`VersionedPaneTree`] instead represents the tree as an immutable tree of
//! `Arc<PersistentNode>`. A structural change produces a **new root** by
//! *path-copying*: only the nodes on the root→target path are re-allocated;
//! every off-path subtree is reused by an `Arc` clone (a refcount bump). A
//! [`PaneVersionStore`] keeps a `Vec` of version roots plus a cursor, so undo
//! and redo are pure index moves — no clone, no validation, no replay.
//!
//! Parent links are intentionally **absent** from [`PersistentNode`]. A node
//! that knew its parent could not be shared between two versions (or two
//! positions), so parents are reconstructed during [`VersionedPaneTree::to_pane_tree`].
//! This is the property that makes structural sharing possible at all.
//!
//! # Scope (prototype simplifications)
//!
//! - Native path-copying covers [`SetSplitRatio`](crate::pane::PaneOperation::SetSplitRatio),
//!   [`SplitLeaf`](crate::pane::PaneOperation::SplitLeaf),
//!   [`CloseNode`](crate::pane::PaneOperation::CloseNode), and
//!   [`SwapNodes`](crate::pane::PaneOperation::SwapNodes) — i.e. the resize,
//!   create, destroy, and rearrange shapes. These exercise in-place mutation,
//!   id-allocating insertion, sibling-promoting removal, and two-path rebuild.
//! - [`MoveSubtree`](crate::pane::PaneOperation::MoveSubtree) and
//!   [`NormalizeRatios`](crate::pane::PaneOperation::NormalizeRatios) use a
//!   *rebuild fallback*: flatten to a canonical [`PaneTree`](crate::pane::PaneTree),
//!   apply the certified conservative operation, and rebuild the persistent
//!   tree. This guarantees total semantic parity for the differential oracle at
//!   the cost of zero sharing for those (rare) operations.
//! - The path-copy search is `O(nodes)` (no id→path index). A production
//!   version would maintain a positional index for `O(depth)` location. The
//!   headline win (`O(1)` version navigation) does not depend on this.
//!
//! Every native operation is proven byte-identical to
//! [`PaneTree::apply_operation_conservative`](crate::pane::PaneTree::apply_operation_conservative)
//! over lockstep histories in `tests/pane_persistent_equivalence.rs`.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::pane::{
    PANE_TREE_SCHEMA_VERSION, PaneConstraints, PaneId, PaneLeaf, PaneModelError, PaneNodeKind,
    PaneNodeRecord, PaneOperation, PaneOperationKind, PanePlacement, PaneSplit, PaneSplitRatio,
    PaneTree, PaneTreeSnapshot, SplitAxis,
};

/// Immutable persistent pane node.
///
/// Split children are `Arc`-shared so that unchanged subtrees are reused across
/// versions. Parent links are deliberately omitted (see the module docs); they
/// are reconstructed during flattening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistentNode {
    /// Terminal content node.
    Leaf {
        /// Stable node id (matches the canonical tree's id space).
        id: PaneId,
        /// Per-node size bounds.
        constraints: PaneConstraints,
        /// Forward-compatible record-level extension bag.
        node_extensions: BTreeMap<String, String>,
        /// Leaf payload (surface key + leaf-level extensions).
        leaf: PaneLeaf,
    },
    /// Interior split node with two shared children.
    Split {
        /// Stable node id.
        id: PaneId,
        /// Per-node size bounds.
        constraints: PaneConstraints,
        /// Forward-compatible record-level extension bag.
        node_extensions: BTreeMap<String, String>,
        /// Split orientation.
        axis: SplitAxis,
        /// First/second weight ratio (always reduced).
        ratio: PaneSplitRatio,
        /// First child subtree.
        first: Arc<PersistentNode>,
        /// Second child subtree.
        second: Arc<PersistentNode>,
    },
}

impl PersistentNode {
    /// Node id of this node.
    #[must_use]
    pub const fn id(&self) -> PaneId {
        match self {
            Self::Leaf { id, .. } | Self::Split { id, .. } => *id,
        }
    }

    /// Whether this node is a leaf.
    #[must_use]
    pub const fn is_leaf(&self) -> bool {
        matches!(self, Self::Leaf { .. })
    }

    /// Total number of nodes in the subtree rooted at `self`.
    #[must_use]
    pub fn node_count(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Split { first, second, .. } => 1 + first.node_count() + second.node_count(),
        }
    }

    /// Maximum depth (root counts as depth `1`).
    #[must_use]
    pub fn depth(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Split { first, second, .. } => 1 + first.depth().max(second.depth()),
        }
    }
}

/// One immutable version of a pane tree.
///
/// Cloning a version is cheap: it bumps the root `Arc` refcount and clones the
/// (usually empty) tree-level extension map.
#[derive(Debug, Clone)]
pub struct VersionedPaneTree {
    schema_version: u16,
    root: Arc<PersistentNode>,
    next_id: PaneId,
    extensions: BTreeMap<String, String>,
}

impl VersionedPaneTree {
    /// Build a singleton versioned tree holding one root leaf.
    #[must_use]
    pub fn singleton(surface_key: impl Into<String>) -> Self {
        let root = PaneId::MIN;
        let node = Arc::new(PersistentNode::Leaf {
            id: root,
            constraints: PaneConstraints::default(),
            node_extensions: BTreeMap::new(),
            leaf: PaneLeaf::new(surface_key),
        });
        Self {
            schema_version: PANE_TREE_SCHEMA_VERSION,
            root: node,
            next_id: root.checked_next().unwrap_or(root),
            extensions: BTreeMap::new(),
        }
    }

    /// Build a versioned tree from a canonical [`PaneTree`].
    #[must_use]
    pub fn from_pane_tree(tree: &PaneTree) -> Self {
        Self::from_snapshot(&tree.to_snapshot())
    }

    /// Build a versioned tree from a canonical snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: &PaneTreeSnapshot) -> Self {
        let mut records: BTreeMap<PaneId, &PaneNodeRecord> = BTreeMap::new();
        for record in &snapshot.nodes {
            let _ = records.insert(record.id, record);
        }
        let root = build_persistent(&records, snapshot.root);
        Self {
            schema_version: snapshot.schema_version,
            root,
            next_id: snapshot.next_id,
            extensions: snapshot.extensions.clone(),
        }
    }

    /// Root node of this version.
    #[must_use]
    pub fn root(&self) -> &Arc<PersistentNode> {
        &self.root
    }

    /// Root pane id.
    #[must_use]
    pub fn root_id(&self) -> PaneId {
        self.root.id()
    }

    /// Next deterministic id value.
    #[must_use]
    pub const fn next_id(&self) -> PaneId {
        self.next_id
    }

    /// Schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Number of nodes in this version.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.root.node_count()
    }

    /// Maximum tree depth in this version.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.root.depth()
    }

    /// Flatten to a canonical snapshot (reconstructing parent links).
    #[must_use]
    pub fn to_snapshot(&self) -> PaneTreeSnapshot {
        let mut nodes = Vec::with_capacity(self.node_count());
        flatten_persistent(&self.root, None, &mut nodes);
        let mut snapshot = PaneTreeSnapshot {
            schema_version: self.schema_version,
            root: self.root.id(),
            next_id: self.next_id,
            nodes,
            extensions: self.extensions.clone(),
        };
        snapshot.canonicalize();
        snapshot
    }

    /// Flatten and validate into a canonical [`PaneTree`].
    ///
    /// This is the bridge back to the production data structure and the basis of
    /// the differential equivalence oracle.
    ///
    /// # Errors
    ///
    /// Returns the canonical tree's validation error if the flattened structure
    /// violates a pane-tree invariant (which should never happen for a tree
    /// built solely through [`VersionedPaneTree::apply_operation`]).
    pub fn to_pane_tree(&self) -> Result<PaneTree, PaneModelError> {
        PaneTree::from_snapshot(self.to_snapshot())
    }

    /// Deterministic structural hash, identical to the canonical
    /// [`PaneTree::state_hash`](crate::pane::PaneTree::state_hash).
    ///
    /// # Errors
    ///
    /// Returns a validation error if the flattened tree is malformed.
    pub fn state_hash(&self) -> Result<u64, PaneModelError> {
        Ok(self.to_pane_tree()?.state_hash())
    }

    /// Apply one structural operation, returning a **new** version.
    ///
    /// `self` is never mutated (persistence). Native operations path-copy with
    /// structural sharing; [`PersistentApplyStrategy::Rebuild`] operations
    /// delegate to the certified conservative baseline.
    ///
    /// # Errors
    ///
    /// Returns a [`PersistentApplyError`] mirroring the canonical baseline's
    /// accept/reject decision for the operation.
    pub fn apply_operation(&self, operation: &PaneOperation) -> Result<Self, PersistentApplyError> {
        match operation {
            PaneOperation::SetSplitRatio { split, ratio } => {
                self.apply_set_split_ratio(*split, *ratio)
            }
            PaneOperation::SplitLeaf {
                target,
                axis,
                ratio,
                placement,
                new_leaf,
            } => self.apply_split_leaf(*target, *axis, *ratio, *placement, new_leaf),
            PaneOperation::CloseNode { target } => self.apply_close_node(*target),
            PaneOperation::SwapNodes { first, second } => self.apply_swap_nodes(*first, *second),
            PaneOperation::MoveSubtree { .. } | PaneOperation::NormalizeRatios => {
                self.apply_via_rebuild(operation)
            }
        }
    }

    /// Strategy this prototype uses for an operation kind.
    #[must_use]
    pub const fn operation_strategy(kind: PaneOperationKind) -> PersistentApplyStrategy {
        match kind {
            PaneOperationKind::SetSplitRatio
            | PaneOperationKind::SplitLeaf
            | PaneOperationKind::CloseNode
            | PaneOperationKind::SwapNodes => PersistentApplyStrategy::PathCopy,
            PaneOperationKind::MoveSubtree | PaneOperationKind::NormalizeRatios => {
                PersistentApplyStrategy::Rebuild
            }
        }
    }

    fn with_root(&self, root: Arc<PersistentNode>, next_id: PaneId) -> Self {
        Self {
            schema_version: self.schema_version,
            root,
            next_id,
            extensions: self.extensions.clone(),
        }
    }

    fn apply_set_split_ratio(
        &self,
        split: PaneId,
        ratio: PaneSplitRatio,
    ) -> Result<Self, PersistentApplyError> {
        // `ratio` is already reduced: `PaneSplitRatio` has no non-normalizing
        // constructor, so re-normalizing (as the baseline does) is a no-op.
        match rebuild_set_ratio(&self.root, split, ratio)? {
            Some(new_root) => Ok(self.with_root(new_root, self.next_id)),
            None => Err(PersistentApplyError::MissingNode { node_id: split }),
        }
    }

    fn apply_split_leaf(
        &self,
        target: PaneId,
        axis: SplitAxis,
        ratio: PaneSplitRatio,
        placement: PanePlacement,
        new_leaf: &PaneLeaf,
    ) -> Result<Self, PersistentApplyError> {
        let mut next_id = self.next_id;
        match rebuild_split_leaf(
            &self.root,
            target,
            axis,
            ratio,
            placement,
            new_leaf,
            &mut next_id,
        )? {
            Some(new_root) => Ok(self.with_root(new_root, next_id)),
            None => Err(PersistentApplyError::MissingNode { node_id: target }),
        }
    }

    fn apply_close_node(&self, target: PaneId) -> Result<Self, PersistentApplyError> {
        if target == self.root.id() {
            return Err(PersistentApplyError::CannotCloseRoot { node_id: target });
        }
        match rebuild_close_node(&self.root, target) {
            Some(new_root) => Ok(self.with_root(new_root, self.next_id)),
            None => Err(PersistentApplyError::MissingNode { node_id: target }),
        }
    }

    fn apply_swap_nodes(
        &self,
        first: PaneId,
        second: PaneId,
    ) -> Result<Self, PersistentApplyError> {
        if first == second {
            // Baseline short-circuits to a no-op before existence checks.
            return Ok(self.clone());
        }
        let Some(first_subtree) = find_subtree(&self.root, first) else {
            return Err(PersistentApplyError::MissingNode { node_id: first });
        };
        let Some(second_subtree) = find_subtree(&self.root, second) else {
            return Err(PersistentApplyError::MissingNode { node_id: second });
        };
        if subtree_contains(&first_subtree, second) {
            return Err(PersistentApplyError::AncestorConflict {
                ancestor: first,
                descendant: second,
            });
        }
        if subtree_contains(&second_subtree, first) {
            return Err(PersistentApplyError::AncestorConflict {
                ancestor: second,
                descendant: first,
            });
        }
        let new_root = replace_two(&self.root, first, &second_subtree, second, &first_subtree);
        Ok(self.with_root(new_root, self.next_id))
    }

    fn apply_via_rebuild(&self, operation: &PaneOperation) -> Result<Self, PersistentApplyError> {
        let mut tree = self
            .to_pane_tree()
            .map_err(PersistentApplyError::InvalidTree)?;
        tree.apply_operation_conservative(0, operation.clone())
            .map_err(|err| PersistentApplyError::Rebuild(format!("{:?}", err.reason)))?;
        Ok(Self::from_pane_tree(&tree))
    }
}

/// Which application strategy the prototype uses for a given operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistentApplyStrategy {
    /// Native structural-sharing path copy (`O(depth)` new nodes).
    PathCopy,
    /// Flatten → conservative baseline → rebuild (no sharing).
    Rebuild,
}

/// Failure from [`VersionedPaneTree::apply_operation`].
///
/// The variants mirror the canonical baseline's reject conditions closely
/// enough that the differential oracle only needs accept/reject agreement plus
/// state-hash equality on the accept path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistentApplyError {
    /// Referenced node id is absent from the tree.
    MissingNode {
        /// The missing node id.
        node_id: PaneId,
    },
    /// Operation targeted a non-split node where a split was required.
    NotSplit {
        /// The offending node id.
        node_id: PaneId,
    },
    /// Operation targeted a non-leaf node where a leaf was required.
    NotLeaf {
        /// The offending node id.
        node_id: PaneId,
    },
    /// Attempted to close the root node.
    CannotCloseRoot {
        /// The root id.
        node_id: PaneId,
    },
    /// One node is an ancestor of the other (illegal for swap).
    AncestorConflict {
        /// The ancestor node.
        ancestor: PaneId,
        /// The descendant node.
        descendant: PaneId,
    },
    /// Ran out of the pane id space.
    IdOverflow {
        /// The id at which allocation failed.
        current: PaneId,
    },
    /// The flattened tree failed canonical validation (should be unreachable).
    InvalidTree(PaneModelError),
    /// The rebuild-fallback baseline rejected the operation.
    Rebuild(String),
}

/// A history of pane-tree versions with replay-free undo/redo.
///
/// `undo`/`redo`/`current` are `O(1)` — they move or read a cursor over a `Vec`
/// of immutable version roots. Applying a new operation truncates any redo
/// branch (matching the canonical timeline) and appends a new version.
#[derive(Debug, Clone)]
pub struct PaneVersionStore {
    versions: Vec<VersionedPaneTree>,
    cursor: usize,
    max_versions: usize,
    pruned: usize,
}

impl PaneVersionStore {
    /// Create a store seeded with an initial version (the baseline).
    #[must_use]
    pub fn new(initial: VersionedPaneTree) -> Self {
        Self {
            versions: vec![initial],
            cursor: 0,
            max_versions: 0,
            pruned: 0,
        }
    }

    /// Create a store with a bounded retention window.
    ///
    /// When the retained version count exceeds `max_versions`, the oldest
    /// versions are pruned (which discards the ability to undo into them, the
    /// same posture as the canonical timeline's entry limit). `0` is unbounded.
    #[must_use]
    pub fn with_max_versions(initial: VersionedPaneTree, max_versions: usize) -> Self {
        let mut store = Self::new(initial);
        store.max_versions = max_versions;
        store
    }

    /// Install a retention bound and immediately enforce it, pruning the oldest
    /// versions if the store currently exceeds `max_versions`.
    ///
    /// Unlike [`with_max_versions`](Self::with_max_versions) this acts on an
    /// existing store, so a retention policy can re-bound it retroactively.
    /// Pruning never reaches the cursor: the current version — and therefore
    /// [`current`](Self::current)'s state hash — plus the redo tail always
    /// survive, so after a deep undo the store may retain more than
    /// `max_versions` until the cursor advances again. Returns the number of
    /// versions pruned by this call (possibly `0` when the cursor pins all
    /// history). `0` is unbounded.
    pub fn set_max_versions(&mut self, max_versions: usize) -> usize {
        let pruned_before = self.pruned;
        self.max_versions = max_versions;
        self.enforce_retention();
        self.pruned.saturating_sub(pruned_before)
    }

    /// Apply an operation to the current version, appending a new version.
    ///
    /// # Errors
    ///
    /// Propagates the [`PersistentApplyError`] from the underlying apply.
    pub fn apply(
        &mut self,
        operation: &PaneOperation,
    ) -> Result<&VersionedPaneTree, PersistentApplyError> {
        let next = self.current().apply_operation(operation)?;
        // Drop any redo branch, then append.
        self.versions.truncate(self.cursor + 1);
        self.versions.push(next);
        self.cursor = self.versions.len() - 1;
        self.enforce_retention();
        Ok(self.current())
    }

    /// Move to the previous version. Returns `false` at the oldest retained version.
    pub fn undo(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        true
    }

    /// Move to the next version. Returns `false` at the newest version.
    pub fn redo(&mut self) -> bool {
        if self.cursor + 1 >= self.versions.len() {
            return false;
        }
        self.cursor += 1;
        true
    }

    /// The current version.
    #[must_use]
    pub fn current(&self) -> &VersionedPaneTree {
        &self.versions[self.cursor]
    }

    /// Number of retained versions.
    #[must_use]
    pub fn version_count(&self) -> usize {
        self.versions.len()
    }

    /// Current cursor index.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether [`undo`](Self::undo) would succeed.
    #[must_use]
    pub const fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    /// Whether [`redo`](Self::redo) would succeed.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.versions.len()
    }

    /// Number of versions pruned by the retention bound so far.
    #[must_use]
    pub const fn pruned(&self) -> usize {
        self.pruned
    }

    /// Compute a structural-sharing and memory-retention report.
    #[must_use]
    pub fn report(&self) -> PaneVersioningReport {
        let mut distinct: HashSet<usize> = HashSet::new();
        collect_distinct(
            self.versions.iter().map(VersionedPaneTree::root),
            &mut distinct,
        );
        let total_logical_nodes: usize = self
            .versions
            .iter()
            .map(VersionedPaneTree::node_count)
            .sum();
        let distinct_nodes = distinct.len();
        let shared_nodes = total_logical_nodes.saturating_sub(distinct_nodes);
        let sharing_ratio = if total_logical_nodes == 0 {
            0.0
        } else {
            shared_nodes as f64 / total_logical_nodes as f64
        };
        let current = self.current();
        PaneVersioningReport {
            version_count: self.versions.len(),
            cursor: self.cursor,
            distinct_nodes,
            total_logical_nodes,
            shared_nodes,
            sharing_ratio,
            current_node_count: current.node_count(),
            current_depth: current.depth(),
            pruned_versions: self.pruned,
        }
    }

    /// Compute a deterministic retained-memory byte model over the distinct
    /// nodes physically held by all retained versions.
    ///
    /// The model mirrors the canonical timeline's
    /// [`retention_diagnostics`](crate::pane::PaneInteractionTimeline::retention_diagnostics)
    /// methodology — `size_of` struct estimates plus measured string payload
    /// bytes — so the two strategies are directly comparable. Because shared
    /// subtrees are counted once (via `Arc` pointer identity), node bytes scale
    /// with *distinct* nodes, not the logical node total.
    #[must_use]
    pub fn retention(&self) -> PaneVersionRetention {
        // Per-distinct-node byte accounting in a single traversal.
        let mut seen: HashSet<usize> = HashSet::new();
        let mut distinct_node_count = 0usize;
        let mut distinct_leaf_payload_bytes = 0usize;
        let mut distinct_extension_payload_bytes = 0usize;
        let mut stack: Vec<&Arc<PersistentNode>> =
            self.versions.iter().map(VersionedPaneTree::root).collect();
        while let Some(node) = stack.pop() {
            if !seen.insert(Arc::as_ptr(node).addr()) {
                continue;
            }
            distinct_node_count += 1;
            match &**node {
                PersistentNode::Leaf {
                    node_extensions,
                    leaf,
                    ..
                } => {
                    distinct_leaf_payload_bytes += leaf.surface_key.len();
                    distinct_extension_payload_bytes += string_map_payload_bytes(&leaf.extensions)
                        .saturating_add(string_map_payload_bytes(node_extensions));
                }
                PersistentNode::Split {
                    node_extensions,
                    first,
                    second,
                    ..
                } => {
                    distinct_extension_payload_bytes += string_map_payload_bytes(node_extensions);
                    stack.push(first);
                    stack.push(second);
                }
            }
        }

        let total_logical_node_count: usize = self
            .versions
            .iter()
            .map(VersionedPaneTree::node_count)
            .sum();
        let arc_node_bytes = std::mem::size_of::<PersistentNode>().saturating_add(ARC_HEADER_BYTES);
        let distinct_struct_bytes = distinct_node_count.saturating_mul(arc_node_bytes);
        let version_metadata_bytes = self
            .versions
            .len()
            .saturating_mul(std::mem::size_of::<VersionedPaneTree>());
        let estimated_total_retained_bytes = std::mem::size_of::<Self>()
            .saturating_add(distinct_struct_bytes)
            .saturating_add(distinct_leaf_payload_bytes)
            .saturating_add(distinct_extension_payload_bytes)
            .saturating_add(version_metadata_bytes);
        let shared_nodes = total_logical_node_count.saturating_sub(distinct_node_count);
        let sharing_ratio = if total_logical_node_count == 0 {
            0.0
        } else {
            shared_nodes as f64 / total_logical_node_count as f64
        };

        PaneVersionRetention {
            version_count: self.versions.len(),
            distinct_node_count,
            total_logical_node_count,
            sharing_ratio,
            distinct_struct_bytes,
            distinct_leaf_payload_bytes,
            distinct_extension_payload_bytes,
            version_metadata_bytes,
            estimated_total_retained_bytes,
        }
    }

    fn enforce_retention(&mut self) {
        if self.max_versions == 0 || self.versions.len() <= self.max_versions {
            return;
        }
        // Never prune at or after the cursor: the version `current()` points
        // at (and everything the user can still redo into) must survive, even
        // when the user has undone deep into history. Any excess beyond
        // `max_versions` is tolerated until the cursor advances again.
        let drop_count = (self.versions.len() - self.max_versions).min(self.cursor);
        if drop_count == 0 {
            return;
        }
        let _ = self.versions.drain(0..drop_count);
        self.pruned += drop_count;
        self.cursor -= drop_count;
    }
}

/// Structural-sharing and memory-retention diagnostics for a [`PaneVersionStore`].
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PaneVersioningReport {
    /// Number of retained versions.
    pub version_count: usize,
    /// Current cursor index.
    pub cursor: usize,
    /// Physically distinct `Arc` node allocations across all retained versions.
    pub distinct_nodes: usize,
    /// Sum of per-version node counts (what naive snapshot-per-version stores).
    pub total_logical_nodes: usize,
    /// `total_logical_nodes - distinct_nodes`: nodes reused via sharing.
    pub shared_nodes: usize,
    /// `shared_nodes / total_logical_nodes` in `[0, 1]`.
    pub sharing_ratio: f64,
    /// Node count of the current version.
    pub current_node_count: usize,
    /// Depth of the current version.
    pub current_depth: usize,
    /// Versions pruned by the retention bound.
    pub pruned_versions: usize,
}

/// Deterministic retained-memory byte model for a [`PaneVersionStore`].
///
/// Mirrors the canonical timeline's
/// [`PaneInteractionTimelineRetentionDiagnostics`](crate::pane::PaneInteractionTimelineRetentionDiagnostics)
/// methodology (`size_of` struct estimates plus measured string payload bytes)
/// so the persistent and checkpointed strategies are directly comparable. The
/// defining economic property of structural sharing is captured here: node
/// struct bytes scale with *physically distinct* (`Arc`-shared) nodes, not the
/// logical node total summed over every retained version.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct PaneVersionRetention {
    /// Number of retained versions held by the store.
    pub version_count: usize,
    /// Physically distinct `Arc` node allocations across all retained versions.
    pub distinct_node_count: usize,
    /// Sum of per-version node counts (what naive snapshot-per-version stores).
    pub total_logical_node_count: usize,
    /// `(total_logical - distinct) / total_logical` in `[0, 1]`.
    pub sharing_ratio: f64,
    /// `distinct_node_count × (size_of::<PersistentNode>() + ARC_HEADER_BYTES)`.
    pub distinct_struct_bytes: usize,
    /// Measured leaf surface-key payload bytes over distinct nodes.
    pub distinct_leaf_payload_bytes: usize,
    /// Measured node + leaf extension-map payload bytes over distinct nodes.
    pub distinct_extension_payload_bytes: usize,
    /// `version_count × size_of::<VersionedPaneTree>()` (the per-version handles).
    pub version_metadata_bytes: usize,
    /// Estimated total retained bytes (container + structs + payload + metadata).
    pub estimated_total_retained_bytes: usize,
}

// ---------------------------------------------------------------------------
// Path-copying primitives (free functions over `Arc<PersistentNode>`).
//
// Convention for `rebuild_*`: `Ok(None)` means "target not found in this
// subtree" (caller keeps searching / surfaces `MissingNode`); `Ok(Some(node))`
// means "found and rebuilt"; `Err(_)` means "found but the operation is
// illegal here". Off-path subtrees are returned via `Arc::clone`, preserving
// structural sharing.
// ---------------------------------------------------------------------------

fn allocate_id(next_id: &mut PaneId) -> Result<PaneId, PersistentApplyError> {
    let current = *next_id;
    *next_id = current
        .checked_next()
        .map_err(|_| PersistentApplyError::IdOverflow { current })?;
    Ok(current)
}

fn rebuild_split_with(
    template: &Arc<PersistentNode>,
    first: Arc<PersistentNode>,
    second: Arc<PersistentNode>,
) -> Arc<PersistentNode> {
    match &**template {
        PersistentNode::Split {
            id,
            constraints,
            node_extensions,
            axis,
            ratio,
            ..
        } => Arc::new(PersistentNode::Split {
            id: *id,
            constraints: *constraints,
            node_extensions: node_extensions.clone(),
            axis: *axis,
            ratio: *ratio,
            first,
            second,
        }),
        // Only ever called with a split template.
        PersistentNode::Leaf { .. } => template.clone(),
    }
}

fn rebuild_set_ratio(
    node: &Arc<PersistentNode>,
    target: PaneId,
    ratio: PaneSplitRatio,
) -> Result<Option<Arc<PersistentNode>>, PersistentApplyError> {
    match &**node {
        PersistentNode::Leaf { id, .. } => {
            if *id == target {
                Err(PersistentApplyError::NotSplit { node_id: target })
            } else {
                Ok(None)
            }
        }
        PersistentNode::Split {
            id,
            constraints,
            node_extensions,
            axis,
            first,
            second,
            ..
        } => {
            if *id == target {
                return Ok(Some(Arc::new(PersistentNode::Split {
                    id: *id,
                    constraints: *constraints,
                    node_extensions: node_extensions.clone(),
                    axis: *axis,
                    ratio,
                    first: first.clone(),
                    second: second.clone(),
                })));
            }
            if let Some(new_first) = rebuild_set_ratio(first, target, ratio)? {
                return Ok(Some(rebuild_split_with(node, new_first, second.clone())));
            }
            if let Some(new_second) = rebuild_set_ratio(second, target, ratio)? {
                return Ok(Some(rebuild_split_with(node, first.clone(), new_second)));
            }
            Ok(None)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rebuild_split_leaf(
    node: &Arc<PersistentNode>,
    target: PaneId,
    axis: SplitAxis,
    ratio: PaneSplitRatio,
    placement: PanePlacement,
    new_leaf: &PaneLeaf,
    next_id: &mut PaneId,
) -> Result<Option<Arc<PersistentNode>>, PersistentApplyError> {
    match &**node {
        PersistentNode::Leaf { id, .. } => {
            if *id != target {
                return Ok(None);
            }
            // Allocation order matches the baseline: split id, then leaf id.
            let split_id = allocate_id(next_id)?;
            let new_leaf_id = allocate_id(next_id)?;
            let existing = node.clone();
            let incoming = Arc::new(PersistentNode::Leaf {
                id: new_leaf_id,
                constraints: PaneConstraints::default(),
                node_extensions: BTreeMap::new(),
                leaf: new_leaf.clone(),
            });
            let (first, second) = match placement {
                PanePlacement::ExistingFirst => (existing, incoming),
                PanePlacement::IncomingFirst => (incoming, existing),
            };
            Ok(Some(Arc::new(PersistentNode::Split {
                id: split_id,
                constraints: PaneConstraints::default(),
                node_extensions: BTreeMap::new(),
                axis,
                ratio,
                first,
                second,
            })))
        }
        PersistentNode::Split {
            id, first, second, ..
        } => {
            if *id == target {
                return Err(PersistentApplyError::NotLeaf { node_id: target });
            }
            if let Some(new_first) =
                rebuild_split_leaf(first, target, axis, ratio, placement, new_leaf, next_id)?
            {
                return Ok(Some(rebuild_split_with(node, new_first, second.clone())));
            }
            if let Some(new_second) =
                rebuild_split_leaf(second, target, axis, ratio, placement, new_leaf, next_id)?
            {
                return Ok(Some(rebuild_split_with(node, first.clone(), new_second)));
            }
            Ok(None)
        }
    }
}

/// Close `target` by promoting its sibling. Returns the rebuilt parent path, or
/// `None` if `target` is not a child anywhere in the subtree.
fn rebuild_close_node(node: &Arc<PersistentNode>, target: PaneId) -> Option<Arc<PersistentNode>> {
    let PersistentNode::Split { first, second, .. } = &**node else {
        return None;
    };
    if first.id() == target {
        return Some(second.clone());
    }
    if second.id() == target {
        return Some(first.clone());
    }
    if let Some(new_first) = rebuild_close_node(first, target) {
        return Some(rebuild_split_with(node, new_first, second.clone()));
    }
    if let Some(new_second) = rebuild_close_node(second, target) {
        return Some(rebuild_split_with(node, first.clone(), new_second));
    }
    None
}

/// Find the subtree rooted at `target` (an `Arc` clone), if present.
fn find_subtree(node: &Arc<PersistentNode>, target: PaneId) -> Option<Arc<PersistentNode>> {
    if node.id() == target {
        return Some(node.clone());
    }
    if let PersistentNode::Split { first, second, .. } = &**node {
        if let Some(found) = find_subtree(first, target) {
            return Some(found);
        }
        if let Some(found) = find_subtree(second, target) {
            return Some(found);
        }
    }
    None
}

/// Whether `target` appears anywhere in the subtree (including the root).
fn subtree_contains(node: &Arc<PersistentNode>, target: PaneId) -> bool {
    if node.id() == target {
        return true;
    }
    match &**node {
        PersistentNode::Leaf { .. } => false,
        PersistentNode::Split { first, second, .. } => {
            subtree_contains(first, target) || subtree_contains(second, target)
        }
    }
}

/// Rebuild `node`, replacing the node with id `a` by `arc_a` and id `b` by
/// `arc_b`. Subtrees containing neither are returned by `Arc::clone`, so sharing
/// is preserved everywhere off the two replacement paths.
fn replace_two(
    node: &Arc<PersistentNode>,
    a: PaneId,
    arc_a: &Arc<PersistentNode>,
    b: PaneId,
    arc_b: &Arc<PersistentNode>,
) -> Arc<PersistentNode> {
    if node.id() == a {
        return arc_a.clone();
    }
    if node.id() == b {
        return arc_b.clone();
    }
    match &**node {
        PersistentNode::Leaf { .. } => node.clone(),
        PersistentNode::Split { first, second, .. } => {
            let new_first = replace_two(first, a, arc_a, b, arc_b);
            let new_second = replace_two(second, a, arc_a, b, arc_b);
            if Arc::ptr_eq(&new_first, first) && Arc::ptr_eq(&new_second, second) {
                node.clone()
            } else {
                rebuild_split_with(node, new_first, new_second)
            }
        }
    }
}

fn build_persistent(
    records: &BTreeMap<PaneId, &PaneNodeRecord>,
    id: PaneId,
) -> Arc<PersistentNode> {
    let record = records
        .get(&id)
        .copied()
        .expect("snapshot references only existing ids");
    match &record.kind {
        PaneNodeKind::Leaf(leaf) => Arc::new(PersistentNode::Leaf {
            id: record.id,
            constraints: record.constraints,
            node_extensions: record.extensions.clone(),
            leaf: leaf.clone(),
        }),
        PaneNodeKind::Split(split) => Arc::new(PersistentNode::Split {
            id: record.id,
            constraints: record.constraints,
            node_extensions: record.extensions.clone(),
            axis: split.axis,
            ratio: split.ratio,
            first: build_persistent(records, split.first),
            second: build_persistent(records, split.second),
        }),
    }
}

fn flatten_persistent(
    node: &Arc<PersistentNode>,
    parent: Option<PaneId>,
    out: &mut Vec<PaneNodeRecord>,
) {
    match &**node {
        PersistentNode::Leaf {
            id,
            constraints,
            node_extensions,
            leaf,
        } => out.push(PaneNodeRecord {
            id: *id,
            parent,
            constraints: *constraints,
            kind: PaneNodeKind::Leaf(leaf.clone()),
            extensions: node_extensions.clone(),
        }),
        PersistentNode::Split {
            id,
            constraints,
            node_extensions,
            axis,
            ratio,
            first,
            second,
        } => {
            out.push(PaneNodeRecord {
                id: *id,
                parent,
                constraints: *constraints,
                kind: PaneNodeKind::Split(PaneSplit {
                    axis: *axis,
                    ratio: *ratio,
                    first: first.id(),
                    second: second.id(),
                }),
                extensions: node_extensions.clone(),
            });
            flatten_persistent(first, Some(*id), out);
            flatten_persistent(second, Some(*id), out);
        }
    }
}

/// Bytes an `Arc<T>` allocation prepends ahead of `T`: the strong and weak
/// reference counters (`AtomicUsize` each). Added to `size_of::<PersistentNode>()`
/// so the persistent retained-memory model accounts for the real heap cost of
/// each physically distinct shared node.
const ARC_HEADER_BYTES: usize = 2 * std::mem::size_of::<usize>();

/// Measured payload bytes (keys + values) of a string→string extension map.
///
/// Mirrors the canonical timeline's private helper of the same name (in
/// [`crate::pane`]) so the persistent and checkpointed retained-memory models
/// are byte-comparable.
pub(crate) fn string_map_payload_bytes(map: &BTreeMap<String, String>) -> usize {
    map.iter()
        .map(|(key, value)| key.len().saturating_add(value.len()))
        .sum()
}

/// Collect physically distinct node allocations reachable from `roots`.
///
/// A node already in `seen` short-circuits: because sharing is structural, a
/// previously visited `Arc` shares its entire subtree, so the subtree is already
/// counted. This makes distinct-counting proportional to the number of distinct
/// nodes rather than the logical node total.
fn collect_distinct<'a>(
    roots: impl Iterator<Item = &'a Arc<PersistentNode>>,
    seen: &mut HashSet<usize>,
) {
    let mut stack: Vec<&Arc<PersistentNode>> = roots.collect();
    while let Some(node) = stack.pop() {
        let key = Arc::as_ptr(node).addr();
        if !seen.insert(key) {
            continue;
        }
        if let PersistentNode::Split { first, second, .. } = &**node {
            stack.push(first);
            stack.push(second);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::PaneTree;

    fn ratio(n: u32, d: u32) -> PaneSplitRatio {
        PaneSplitRatio::new(n, d).expect("valid ratio")
    }

    /// Build a small balanced-ish tree by splitting the root then its children.
    /// Returns the version plus the ids of the two interior splits.
    fn build_demo() -> VersionedPaneTree {
        let v0 = VersionedPaneTree::singleton("root");
        // Split root leaf (id 1) → split id 2, new leaf id 3.
        let op1 = PaneOperation::SplitLeaf {
            target: PaneId::MIN,
            axis: SplitAxis::Horizontal,
            ratio: ratio(1, 1),
            placement: PanePlacement::ExistingFirst,
            new_leaf: PaneLeaf::new("b"),
        };
        let v1 = v0.apply_operation(&op1).expect("split root");
        // Split leaf id 1 again → split id 4, new leaf id 5.
        let op2 = PaneOperation::SplitLeaf {
            target: PaneId::MIN,
            axis: SplitAxis::Vertical,
            ratio: ratio(2, 1),
            placement: PanePlacement::ExistingFirst,
            new_leaf: PaneLeaf::new("c"),
        };
        v1.apply_operation(&op2).expect("split leaf 1")
    }

    #[test]
    fn singleton_round_trips_through_canonical_tree() {
        let versioned = VersionedPaneTree::singleton("root");
        let canonical = PaneTree::singleton("root");
        assert_eq!(
            versioned.state_hash().expect("hash"),
            canonical.state_hash()
        );
    }

    #[test]
    fn split_leaf_matches_canonical_baseline() {
        let versioned = build_demo();
        let mut canonical = PaneTree::singleton("root");
        canonical
            .apply_operation_conservative(
                1,
                PaneOperation::SplitLeaf {
                    target: PaneId::MIN,
                    axis: SplitAxis::Horizontal,
                    ratio: ratio(1, 1),
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new("b"),
                },
            )
            .expect("split root");
        canonical
            .apply_operation_conservative(
                2,
                PaneOperation::SplitLeaf {
                    target: PaneId::MIN,
                    axis: SplitAxis::Vertical,
                    ratio: ratio(2, 1),
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new("c"),
                },
            )
            .expect("split leaf 1");
        assert_eq!(
            versioned.state_hash().expect("hash"),
            canonical.state_hash()
        );
        assert_eq!(versioned.next_id(), canonical.next_id());
    }

    #[test]
    fn set_split_ratio_preserves_off_path_sharing() {
        let base = build_demo();
        // The interior split created first is id 2 (root). Its second child is
        // leaf id 3, which must be physically shared after we re-ratio split id 4.
        let before_second = match &**base.root() {
            PersistentNode::Split { second, .. } => second.clone(),
            PersistentNode::Leaf { .. } => unreachable!("root is a split"),
        };
        let next = base
            .apply_operation(&PaneOperation::SetSplitRatio {
                split: PaneId::new(4).unwrap(),
                ratio: ratio(3, 1),
            })
            .expect("set ratio");
        let after_second = match &**next.root() {
            PersistentNode::Split { second, .. } => second.clone(),
            PersistentNode::Leaf { .. } => unreachable!("root is a split"),
        };
        assert!(
            Arc::ptr_eq(&before_second, &after_second),
            "off-path subtree must be shared, not re-allocated"
        );
        // And the root itself was re-allocated (the path changed).
        assert!(!Arc::ptr_eq(base.root(), next.root()));
    }

    #[test]
    fn set_split_ratio_on_leaf_is_rejected() {
        let base = build_demo();
        let err = base
            .apply_operation(&PaneOperation::SetSplitRatio {
                split: PaneId::MIN,
                ratio: ratio(1, 1),
            })
            .expect_err("leaf is not a split");
        assert_eq!(
            err,
            PersistentApplyError::NotSplit {
                node_id: PaneId::MIN
            }
        );
    }

    #[test]
    fn close_node_promotes_sibling_like_baseline() {
        let base = build_demo();
        let op = PaneOperation::CloseNode {
            target: PaneId::new(3).unwrap(),
        };
        let next = base.apply_operation(&op).expect("close leaf 3");

        let mut canonical = base.to_pane_tree().expect("flatten");
        canonical
            .apply_operation_conservative(99, op)
            .expect("close leaf 3");
        assert_eq!(next.state_hash().expect("hash"), canonical.state_hash());
    }

    #[test]
    fn close_root_is_rejected() {
        let base = build_demo();
        let err = base
            .apply_operation(&PaneOperation::CloseNode {
                target: base.root_id(),
            })
            .expect_err("cannot close root");
        assert!(matches!(err, PersistentApplyError::CannotCloseRoot { .. }));
    }

    #[test]
    fn swap_nodes_matches_baseline_and_shares() {
        let base = build_demo();
        let op = PaneOperation::SwapNodes {
            first: PaneId::new(3).unwrap(),
            second: PaneId::new(5).unwrap(),
        };
        let next = base.apply_operation(&op).expect("swap leaves");

        let mut canonical = base.to_pane_tree().expect("flatten");
        canonical.apply_operation_conservative(7, op).expect("swap");
        assert_eq!(next.state_hash().expect("hash"), canonical.state_hash());
    }

    #[test]
    fn swap_with_self_is_noop() {
        let base = build_demo();
        let next = base
            .apply_operation(&PaneOperation::SwapNodes {
                first: PaneId::new(3).unwrap(),
                second: PaneId::new(3).unwrap(),
            })
            .expect("self swap is a no-op");
        assert_eq!(next.state_hash().expect("a"), base.state_hash().expect("b"));
    }

    #[test]
    fn version_store_undo_redo_is_o1_and_correct() {
        let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
        let h0 = store.current().state_hash().expect("h0");
        store
            .apply(&PaneOperation::SplitLeaf {
                target: PaneId::MIN,
                axis: SplitAxis::Horizontal,
                ratio: ratio(1, 1),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("b"),
            })
            .expect("split");
        let h1 = store.current().state_hash().expect("h1");
        assert_ne!(h0, h1);

        assert!(store.undo());
        assert_eq!(store.current().state_hash().expect("u"), h0);
        assert!(store.redo());
        assert_eq!(store.current().state_hash().expect("r"), h1);
        assert!(!store.redo());
        assert!(store.undo());
        assert!(!store.undo());
    }

    #[test]
    fn report_quantifies_sharing() {
        let mut store = PaneVersionStore::new(build_demo());
        for n in 1..=8u32 {
            store
                .apply(&PaneOperation::SetSplitRatio {
                    split: PaneId::new(4).unwrap(),
                    ratio: ratio(n, 1),
                })
                .expect("set ratio");
        }
        let report = store.report();
        assert_eq!(report.version_count, 9);
        assert!(
            report.shared_nodes > 0,
            "consecutive versions must share nodes"
        );
        assert!(report.distinct_nodes < report.total_logical_nodes);
        assert!(report.sharing_ratio > 0.0 && report.sharing_ratio < 1.0);
    }

    fn ratio_storm_store() -> PaneVersionStore {
        let mut store = PaneVersionStore::new(build_demo());
        for n in 1..=8u32 {
            store
                .apply(&PaneOperation::SetSplitRatio {
                    split: PaneId::new(4).unwrap(),
                    ratio: ratio(n, 1),
                })
                .expect("set ratio");
        }
        store
    }

    #[test]
    fn retention_model_is_deterministic() {
        // Two independently constructed stores over the identical workload must
        // yield byte-identical retention models (acceptance criterion: telemetry
        // is deterministic enough to compare strategies meaningfully).
        assert_eq!(
            ratio_storm_store().retention(),
            ratio_storm_store().retention()
        );
    }

    #[test]
    fn retention_total_is_faithful_sum_of_classes() {
        let store = ratio_storm_store();
        let r = store.retention();
        let class_sum = std::mem::size_of::<PaneVersionStore>()
            + r.distinct_struct_bytes
            + r.distinct_leaf_payload_bytes
            + r.distinct_extension_payload_bytes
            + r.version_metadata_bytes;
        assert_eq!(r.estimated_total_retained_bytes, class_sum);
        assert_eq!(r.version_count, 9);
        assert!(r.distinct_node_count < r.total_logical_node_count);
    }

    #[test]
    fn retention_node_bytes_scale_with_distinct_not_logical_nodes() {
        // The economic claim of structural sharing: node struct bytes are paid
        // per physically distinct node, far below the naive snapshot-per-version
        // cost of `total_logical_node_count` nodes.
        let store = ratio_storm_store();
        let r = store.retention();
        let per_node = std::mem::size_of::<PersistentNode>() + ARC_HEADER_BYTES;
        assert_eq!(r.distinct_struct_bytes, r.distinct_node_count * per_node);
        let naive_struct_bytes = r.total_logical_node_count * per_node;
        assert!(
            r.distinct_struct_bytes < naive_struct_bytes,
            "sharing must cost fewer node-struct bytes than naive per-version snapshots"
        );
        assert!(r.sharing_ratio > 0.0 && r.sharing_ratio < 1.0);
    }

    #[test]
    fn rebuild_fallback_normalize_ratios_matches_baseline() {
        let base = build_demo();
        let op = PaneOperation::NormalizeRatios;
        let next = base.apply_operation(&op).expect("normalize");
        let mut canonical = base.to_pane_tree().expect("flatten");
        canonical
            .apply_operation_conservative(11, op)
            .expect("normalize baseline");
        assert_eq!(next.state_hash().expect("hash"), canonical.state_hash());
        assert_eq!(
            VersionedPaneTree::operation_strategy(PaneOperationKind::NormalizeRatios),
            PersistentApplyStrategy::Rebuild
        );
    }
}

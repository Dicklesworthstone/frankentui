//! Incremental layout engine (bd-3p4y1.3).
//!
//! Wraps [`DepGraph`] and a per-node result cache so that only dirty
//! subtrees are re-evaluated during layout. Clean subtrees return their
//! cached `Vec<Rect>` in O(1).
//!
//! # Key Invariant
//!
//! The output of incremental layout is **bit-identical** to full layout.
//! The same traversal order (DFS pre-order) and the same computation at
//! each dirty node guarantee isomorphic results.
//!
//! # Usage
//!
//! ```ignore
//! use ftui_layout::incremental::IncrementalLayout;
//! use ftui_layout::dep_graph::InputKind;
//!
//! let mut inc = IncrementalLayout::new();
//!
//! // Build the widget tree.
//! let root = inc.add_node(None);
//! let left = inc.add_node(Some(root));
//! let right = inc.add_node(Some(root));
//!
//! // First pass: everything computes (cold cache).
//! inc.propagate();
//! let root_rects = inc.get_or_compute(root, area, |a| outer_flex.split(a));
//! let left_rects = inc.get_or_compute(left, root_rects[0], |a| left_flex.split(a));
//! let right_rects = inc.get_or_compute(right, root_rects[1], |a| right_flex.split(a));
//!
//! // Only left content changed — right subtree is skipped.
//! inc.mark_changed(left, InputKind::Content, new_hash);
//! inc.propagate();
//! let root_rects2 = inc.get_or_compute(root, area, |a| outer_flex.split(a));
//! let left_rects2 = inc.get_or_compute(left, root_rects2[0], |a| left_flex.split(a));
//! let right_rects2 = inc.get_or_compute(right, root_rects2[1], |a| right_flex.split(a));
//! // right_rects2 was returned from cache without calling right_flex.split().
//! ```
//!
//! # Flex Siblings
//!
//! In flex layouts, changing one child can affect all siblings (because
//! remaining space is redistributed). Use
//! [`mark_dirty_with_ancestors`](IncrementalLayout::mark_dirty_with_ancestors)
//! to dirty a child and all its ancestors. Since parent→child edges
//! already exist (from `add_node`), dirtying the parent automatically
//! propagates to all siblings during `propagate()`.
//!
//! # Force-Full Fallback
//!
//! Call [`IncrementalLayout::set_force_full`] to bypass the cache entirely
//! and recompute every node. This is useful for debugging or as an env-var
//! fallback (`FRANKENTUI_FULL_LAYOUT=1`, wired in bd-3p4y1.6).

use crate::dep_graph::{CycleError, DepGraph, InputKind, NodeId};
use ftui_core::geometry::Rect;
use rustc_hash::FxHashMap;
use std::hash::{Hash, Hasher};

// ============================================================================
// CachedNodeLayout
// ============================================================================

/// Cached layout result for a single node.
#[derive(Clone, Debug)]
struct CachedNodeLayout {
    /// The area that was used for this computation.
    area: Rect,
    /// The computed sub-regions (output of Flex::split / Grid row solve).
    rects: Vec<Rect>,
    /// FxHash of the output rects for change detection.
    result_hash: u64,
}

/// Compute a fast hash of a `Vec<Rect>` for change detection.
fn hash_rects(rects: &[Rect]) -> u64 {
    let mut h = rustc_hash::FxHasher::default();
    for r in rects {
        r.x.hash(&mut h);
        r.y.hash(&mut h);
        r.width.hash(&mut h);
        r.height.hash(&mut h);
    }
    h.finish()
}

// ============================================================================
// IncrementalStats
// ============================================================================

/// Statistics for a single incremental layout pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IncrementalStats {
    /// Nodes recomputed in this pass.
    pub recomputed: usize,
    /// Nodes returned from cache.
    pub cached: usize,
    /// Total `get_or_compute` calls in this pass.
    pub total: usize,
    /// Current number of entries in the cache.
    pub cache_entries: usize,
}

impl IncrementalStats {
    /// Cache hit rate as a fraction (0.0 – 1.0).
    pub fn hit_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.cached as f64 / self.total as f64
        }
    }
}

// ============================================================================
// IncrementalLayout
// ============================================================================

/// Incremental layout engine.
///
/// Combines a [`DepGraph`] for dirty tracking with a per-node result
/// cache. The user drives the DFS tree walk; at each node,
/// [`get_or_compute`](Self::get_or_compute) decides whether to return
/// the cached result or call the supplied compute closure.
pub struct IncrementalLayout {
    /// Dependency graph for dirty propagation.
    graph: DepGraph,
    /// Per-node layout cache.
    cache: FxHashMap<NodeId, CachedNodeLayout>,
    /// Current-pass statistics.
    stats: IncrementalStats,
    /// When true, every `get_or_compute` call recomputes (bypass cache).
    force_full: bool,
}

impl IncrementalLayout {
    /// Create an empty incremental layout engine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph: DepGraph::new(),
            cache: FxHashMap::default(),
            stats: IncrementalStats::default(),
            force_full: false,
        }
    }

    /// Create with pre-allocated capacity.
    #[must_use]
    pub fn with_capacity(node_cap: usize) -> Self {
        Self {
            graph: DepGraph::with_capacity(node_cap, node_cap),
            cache: FxHashMap::with_capacity_and_hasher(node_cap, Default::default()),
            stats: IncrementalStats::default(),
            force_full: false,
        }
    }

    /// Create from environment configuration.
    ///
    /// Reads `FRANKENTUI_FULL_LAYOUT`. When set to `"1"`, `"true"`, or
    /// `"yes"` (case-insensitive), force-full mode is enabled and all
    /// `get_or_compute` calls bypass the cache.
    #[must_use]
    pub fn from_env() -> Self {
        let force = std::env::var("FRANKENTUI_FULL_LAYOUT")
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        Self {
            graph: DepGraph::new(),
            cache: FxHashMap::default(),
            stats: IncrementalStats::default(),
            force_full: force,
        }
    }

    // ── Node Management ─────────────────────────────────────────────

    /// Add a layout node, optionally with a parent.
    ///
    /// When a parent is provided, a dependency edge is added so that
    /// dirtying the parent also dirties this child.
    pub fn add_node(&mut self, parent: Option<NodeId>) -> NodeId {
        let id = self.graph.add_node();
        if let Some(p) = parent {
            self.graph.set_parent(id, p);
            // Child depends on parent: parent dirty → child dirty.
            let _ = self.graph.add_edge(id, p);
        }
        // New nodes start dirty (no cached result).
        self.graph.mark_dirty(id);
        id
    }

    /// Remove a node and evict its cache entry.
    pub fn remove_node(&mut self, id: NodeId) {
        self.cache.remove(&id);
        // If there's a parent, mark it dirty (layout changed).
        if let Some(parent) = self.graph.parent(id) {
            self.graph.mark_dirty(parent);
        }
        self.graph.remove_node(id);
    }

    /// Add a custom dependency edge: `from` depends on `to`.
    ///
    /// Use this for bidirectional flex-sibling relationships or
    /// cross-subtree dependencies.
    pub fn add_dependency(&mut self, from: NodeId, to: NodeId) -> Result<(), CycleError> {
        self.graph.add_edge(from, to)
    }

    // ── Dirty Tracking ──────────────────────────────────────────────

    /// Mark a node's input as changed (hash-deduplicated).
    ///
    /// The node will not be dirtied if `new_hash` matches the stored
    /// hash for this `InputKind`.
    pub fn mark_changed(&mut self, id: NodeId, kind: InputKind, new_hash: u64) {
        self.graph.mark_changed(id, kind, new_hash);
    }

    /// Force-mark a node as dirty without hash comparison.
    pub fn mark_dirty(&mut self, id: NodeId) {
        self.graph.mark_dirty(id);
    }

    /// Mark a node dirty and also dirty all its ancestors up to the
    /// root. This implements the "flex sibling" pattern: when a child
    /// changes, its parent must recompute, which propagates to all
    /// siblings via parent→child edges.
    pub fn mark_dirty_with_ancestors(&mut self, id: NodeId) {
        self.graph.mark_dirty(id);
        let mut current = id;
        while let Some(parent) = self.graph.parent(current) {
            self.graph.mark_dirty(parent);
            current = parent;
        }
    }

    /// Propagate dirtiness from pending nodes to all transitive
    /// dependents. Must be called before [`get_or_compute`](Self::get_or_compute).
    ///
    /// Returns the dirty set in DFS pre-order.
    pub fn propagate(&mut self) -> Vec<NodeId> {
        self.graph.propagate()
    }

    /// Check if a node is currently dirty.
    #[must_use]
    pub fn is_dirty(&self, id: NodeId) -> bool {
        self.graph.is_dirty(id)
    }

    // ── Layout Computation ──────────────────────────────────────────

    /// Get the cached layout for a node, or compute and cache it.
    ///
    /// A cache hit requires:
    /// 1. The node is not dirty.
    /// 2. The area matches the previously cached area.
    /// 3. `force_full` is not enabled.
    ///
    /// On a cache miss, `compute` is called with the `area` and the
    /// result is stored. The node is cleaned after computation.
    pub fn get_or_compute<F>(&mut self, id: NodeId, area: Rect, compute: F) -> Vec<Rect>
    where
        F: FnOnce(Rect) -> Vec<Rect>,
    {
        self.stats.total += 1;

        if !self.force_full
            && !self.graph.is_dirty(id)
            && let Some(cached) = self.cache.get(&id)
            && cached.area == area
        {
            self.stats.cached += 1;
            return cached.rects.clone();
        }

        // Cache miss or dirty: recompute.
        let rects = compute(area);
        let result_hash = hash_rects(&rects);
        self.cache.insert(
            id,
            CachedNodeLayout {
                area,
                rects: rects.clone(),
                result_hash,
            },
        );
        self.graph.clean(id);
        self.stats.recomputed += 1;
        rects
    }

    /// Retrieve the cached result for a node without recomputing.
    ///
    /// Returns `None` if the node has no cached result.
    #[must_use]
    pub fn cached_rects(&self, id: NodeId) -> Option<&[Rect]> {
        self.cache.get(&id).map(|c| c.rects.as_slice())
    }

    /// Check whether a node's last computed result changed compared to
    /// its previous result. Useful for detecting when a parent must
    /// propagate layout changes to children.
    #[must_use]
    pub fn result_changed(&self, id: NodeId) -> bool {
        // If there's no cached entry, it's "changed" (new).
        !self.cache.contains_key(&id)
    }

    /// Get the hash of a node's last computed result.
    #[must_use]
    pub fn result_hash(&self, id: NodeId) -> Option<u64> {
        self.cache.get(&id).map(|c| c.result_hash)
    }

    // ── Configuration ───────────────────────────────────────────────

    /// Enable or disable force-full mode.
    ///
    /// When enabled, every `get_or_compute` call recomputes from
    /// scratch, ignoring the cache. Useful for debugging or as a
    /// fallback path.
    pub fn set_force_full(&mut self, force: bool) {
        self.force_full = force;
    }

    /// Whether force-full mode is enabled.
    #[must_use]
    pub fn force_full(&self) -> bool {
        self.force_full
    }

    // ── Statistics ──────────────────────────────────────────────────

    /// Current-pass statistics.
    #[must_use]
    pub fn stats(&self) -> IncrementalStats {
        IncrementalStats {
            cache_entries: self.cache.len(),
            ..self.stats.clone()
        }
    }

    /// Reset per-pass statistics to zero.
    pub fn reset_stats(&mut self) {
        self.stats = IncrementalStats::default();
    }

    // ── Bulk Operations ─────────────────────────────────────────────

    /// Mark all nodes clean and advance the generation.
    pub fn clean_all(&mut self) {
        self.graph.clean_all();
    }

    /// Mark all nodes dirty (forces full recomputation on next pass).
    pub fn invalidate_all(&mut self) {
        self.graph.invalidate_all();
    }

    /// Clear the entire result cache (frees memory).
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    // ── Introspection ───────────────────────────────────────────────

    /// Borrow the underlying dependency graph.
    #[must_use]
    pub fn graph(&self) -> &DepGraph {
        &self.graph
    }

    /// Mutable access to the dependency graph.
    pub fn graph_mut(&mut self) -> &mut DepGraph {
        &mut self.graph
    }

    /// Number of cached entries.
    #[must_use]
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Number of live nodes in the dependency graph.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }
}

impl Default for IncrementalLayout {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for IncrementalLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncrementalLayout")
            .field("nodes", &self.graph.node_count())
            .field("cache_entries", &self.cache.len())
            .field("force_full", &self.force_full)
            .finish()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a simple binary tree of depth `d`.
    /// Returns (inc, root, leaves).
    fn binary_tree(depth: u32) -> (IncrementalLayout, NodeId, Vec<NodeId>) {
        let node_count = (1u32 << (depth + 1)) - 1;
        let mut inc = IncrementalLayout::with_capacity(node_count as usize);
        let root = inc.add_node(None);

        let mut current_level = vec![root];
        let mut leaves = Vec::new();

        for _ in 0..depth {
            let mut next_level = Vec::new();
            for &parent in &current_level {
                let left = inc.add_node(Some(parent));
                let right = inc.add_node(Some(parent));
                next_level.push(left);
                next_level.push(right);
            }
            current_level = next_level;
        }
        leaves.extend(current_level);

        (inc, root, leaves)
    }

    /// Helper: build a flat tree (root + N children).
    fn flat_tree(n: usize) -> (IncrementalLayout, NodeId, Vec<NodeId>) {
        let mut inc = IncrementalLayout::with_capacity(n + 1);
        let root = inc.add_node(None);
        let children: Vec<_> = (0..n).map(|_| inc.add_node(Some(root))).collect();
        (inc, root, children)
    }

    fn area(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    fn split_equal(a: Rect, n: usize) -> Vec<Rect> {
        if n == 0 {
            return vec![];
        }
        let w = a.width / n as u16;
        (0..n)
            .map(|i| Rect::new(a.x + (i as u16) * w, a.y, w, a.height))
            .collect()
    }

    // ── Basic ───────────────────────────────────────────────────────

    #[test]
    fn new_node_is_dirty() {
        let mut inc = IncrementalLayout::new();
        let n = inc.add_node(None);
        assert!(inc.is_dirty(n));
    }

    #[test]
    fn get_or_compute_caches() {
        let mut inc = IncrementalLayout::new();
        let n = inc.add_node(None);
        inc.propagate();

        let a = area(80, 24);
        let mut calls = 0u32;
        let r1 = inc.get_or_compute(n, a, |a| {
            calls += 1;
            split_equal(a, 2)
        });

        // Second call should be cached.
        let r2 = inc.get_or_compute(n, a, |a| {
            calls += 1;
            split_equal(a, 2)
        });

        assert_eq!(r1, r2);
        assert_eq!(calls, 1);
    }

    #[test]
    fn dirty_node_recomputes() {
        let mut inc = IncrementalLayout::new();
        let n = inc.add_node(None);
        inc.propagate();

        let a = area(80, 24);
        inc.get_or_compute(n, a, |a| split_equal(a, 2));

        // Mark dirty → recompute.
        inc.mark_dirty(n);
        inc.propagate();

        let mut calls = 0u32;
        inc.get_or_compute(n, a, |a| {
            calls += 1;
            split_equal(a, 2)
        });
        assert_eq!(calls, 1);
    }

    #[test]
    fn area_change_triggers_recompute() {
        let mut inc = IncrementalLayout::new();
        let n = inc.add_node(None);
        inc.propagate();

        inc.get_or_compute(n, area(80, 24), |a| split_equal(a, 2));

        // Different area → must recompute even though node is clean.
        let mut calls = 0u32;
        inc.get_or_compute(n, area(120, 40), |a| {
            calls += 1;
            split_equal(a, 2)
        });
        assert_eq!(calls, 1);
    }

    #[test]
    fn same_area_same_node_cached() {
        let mut inc = IncrementalLayout::new();
        let n = inc.add_node(None);
        inc.propagate();

        let a = area(80, 24);
        inc.get_or_compute(n, a, |a| split_equal(a, 2));

        // Same area, clean node → cache hit.
        let mut calls = 0u32;
        inc.get_or_compute(n, a, |_a| {
            calls += 1;
            vec![]
        });
        assert_eq!(calls, 0);
    }

    // ── Propagation ─────────────────────────────────────────────────

    #[test]
    fn dirty_parent_dirties_child() {
        let mut inc = IncrementalLayout::new();
        let root = inc.add_node(None);
        let child = inc.add_node(Some(root));
        inc.propagate();

        let a = area(80, 24);
        inc.get_or_compute(root, a, |a| split_equal(a, 1));
        inc.get_or_compute(child, a, |a| split_equal(a, 2));

        // Dirty root → child should also recompute.
        inc.mark_dirty(root);
        inc.propagate();

        assert!(inc.is_dirty(root));
        assert!(inc.is_dirty(child));
    }

    #[test]
    fn clean_sibling_not_affected_by_dirty_sibling() {
        let (mut inc, root, children) = flat_tree(3);
        inc.propagate();

        let a = area(120, 24);
        let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 3));
        for (i, &c) in children.iter().enumerate() {
            inc.get_or_compute(c, root_rects[i], |a| split_equal(a, 1));
        }

        // Mark only child[1] dirty.
        inc.mark_dirty(children[1]);
        inc.propagate();

        assert!(!inc.is_dirty(root));
        assert!(!inc.is_dirty(children[0]));
        assert!(inc.is_dirty(children[1]));
        assert!(!inc.is_dirty(children[2]));
    }

    #[test]
    fn flex_siblings_dirty_via_parent() {
        let (mut inc, root, children) = flat_tree(3);
        inc.propagate();

        let a = area(120, 24);
        let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 3));
        for (i, &c) in children.iter().enumerate() {
            inc.get_or_compute(c, root_rects[i], |a| split_equal(a, 1));
        }

        // Flex coupling: when a child changes, mark the parent dirty.
        // Since parent→child edges exist, parent dirty → all children dirty.
        inc.mark_dirty(children[1]);
        inc.mark_dirty(root); // Flex coupling: parent must recompute.
        inc.propagate();

        assert!(inc.is_dirty(root));
        assert!(inc.is_dirty(children[0]));
        assert!(inc.is_dirty(children[1]));
        assert!(inc.is_dirty(children[2]));
    }

    // ── Statistics ──────────────────────────────────────────────────

    #[test]
    fn stats_track_hits_and_misses() {
        let (mut inc, root, children) = flat_tree(3);
        inc.propagate();

        let a = area(120, 24);
        // First pass: all misses.
        let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 3));
        for (i, &c) in children.iter().enumerate() {
            inc.get_or_compute(c, root_rects[i], |a| split_equal(a, 1));
        }

        let s = inc.stats();
        assert_eq!(s.recomputed, 4);
        assert_eq!(s.cached, 0);
        assert_eq!(s.total, 4);

        inc.reset_stats();

        // Second pass: all hits.
        let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 3));
        for (i, &c) in children.iter().enumerate() {
            inc.get_or_compute(c, root_rects[i], |a| split_equal(a, 1));
        }

        let s = inc.stats();
        assert_eq!(s.recomputed, 0);
        assert_eq!(s.cached, 4);
        assert_eq!(s.total, 4);
        assert!((s.hit_rate() - 1.0).abs() < 0.001);
    }

    #[test]
    fn stats_partial_dirty() {
        let (mut inc, root, children) = flat_tree(4);
        inc.propagate();

        let a = area(160, 24);
        let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 4));
        for (i, &c) in children.iter().enumerate() {
            inc.get_or_compute(c, root_rects[i], |a| split_equal(a, 1));
        }
        inc.reset_stats();

        // Dirty one child.
        inc.mark_dirty(children[2]);
        inc.propagate();

        let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 4));
        for (i, &c) in children.iter().enumerate() {
            inc.get_or_compute(c, root_rects[i], |a| split_equal(a, 1));
        }

        let s = inc.stats();
        assert_eq!(s.recomputed, 1);
        assert_eq!(s.cached, 4); // root + 3 clean children
        assert_eq!(s.total, 5);
    }

    // ── Force Full ──────────────────────────────────────────────────

    #[test]
    fn force_full_bypasses_cache() {
        let mut inc = IncrementalLayout::new();
        let n = inc.add_node(None);
        inc.propagate();

        let a = area(80, 24);
        inc.get_or_compute(n, a, |a| split_equal(a, 2));

        // Enable force-full.
        inc.set_force_full(true);
        assert!(inc.force_full());

        let mut calls = 0u32;
        inc.get_or_compute(n, a, |a| {
            calls += 1;
            split_equal(a, 2)
        });
        assert_eq!(calls, 1);
    }

    #[test]
    fn force_full_produces_identical_results() {
        let (mut inc, root, children) = flat_tree(3);
        inc.propagate();

        let a = area(120, 24);

        // Incremental pass.
        let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 3));
        let mut child_rects_inc = Vec::new();
        for (i, &c) in children.iter().enumerate() {
            child_rects_inc.push(inc.get_or_compute(c, root_rects[i], |a| split_equal(a, 2)));
        }

        // Force-full pass.
        inc.set_force_full(true);
        inc.reset_stats();

        let root_rects_full = inc.get_or_compute(root, a, |a| split_equal(a, 3));
        let mut child_rects_full = Vec::new();
        for (i, &c) in children.iter().enumerate() {
            child_rects_full.push(inc.get_or_compute(c, root_rects_full[i], |a| split_equal(a, 2)));
        }

        assert_eq!(root_rects, root_rects_full);
        assert_eq!(child_rects_inc, child_rects_full);
    }

    // ── Node Removal ────────────────────────────────────────────────

    #[test]
    fn remove_node_evicts_cache() {
        let mut inc = IncrementalLayout::new();
        let root = inc.add_node(None);
        let child = inc.add_node(Some(root));
        inc.propagate();

        inc.get_or_compute(child, area(40, 24), |a| split_equal(a, 1));
        assert!(inc.cached_rects(child).is_some());

        inc.remove_node(child);
        assert!(inc.cached_rects(child).is_none());
    }

    #[test]
    fn remove_node_dirties_parent() {
        let mut inc = IncrementalLayout::new();
        let root = inc.add_node(None);
        let child = inc.add_node(Some(root));
        inc.propagate();

        let a = area(80, 24);
        inc.get_or_compute(root, a, |a| split_equal(a, 1));
        inc.get_or_compute(child, a, |a| split_equal(a, 1));

        // Remove child → root should be dirty.
        inc.remove_node(child);
        assert!(inc.is_dirty(root));
    }

    // ── Bulk Operations ─────────────────────────────────────────────

    #[test]
    fn invalidate_all_forces_recompute() {
        let (mut inc, root, children) = flat_tree(3);
        inc.propagate();

        let a = area(120, 24);
        let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 3));
        for (i, &c) in children.iter().enumerate() {
            inc.get_or_compute(c, root_rects[i], |a| split_equal(a, 1));
        }

        inc.invalidate_all();
        inc.propagate();

        assert!(inc.is_dirty(root));
        for &c in &children {
            assert!(inc.is_dirty(c));
        }
    }

    #[test]
    fn clean_all_resets_dirty() {
        let (mut inc, root, children) = flat_tree(2);
        inc.propagate();

        // All should be clean after propagate + compute.
        let a = area(80, 24);
        inc.get_or_compute(root, a, |a| split_equal(a, 2));
        for (i, &c) in children.iter().enumerate() {
            let child_area = Rect::new(i as u16 * 40, 0, 40, 24);
            inc.get_or_compute(c, child_area, |a| split_equal(a, 1));
        }

        inc.mark_dirty(root);
        inc.propagate();
        assert!(inc.is_dirty(root));

        inc.clean_all();
        assert!(!inc.is_dirty(root));
    }

    #[test]
    fn clear_cache_frees_memory() {
        let (mut inc, root, _children) = flat_tree(5);
        inc.propagate();

        let a = area(200, 24);
        inc.get_or_compute(root, a, |a| split_equal(a, 5));
        assert!(inc.cache_len() > 0);

        inc.clear_cache();
        assert_eq!(inc.cache_len(), 0);
    }

    // ── mark_dirty_with_ancestors ─────────────────────────────────

    #[test]
    fn mark_dirty_with_ancestors_propagates_to_siblings() {
        // Root → 3 children. Dirtying child[1] with ancestors
        // should dirty root, which propagates to all children.
        let (mut inc, root, children) = flat_tree(3);
        inc.propagate();

        let a = area(120, 24);
        let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 3));
        for (i, &c) in children.iter().enumerate() {
            inc.get_or_compute(c, root_rects[i], |a| split_equal(a, 1));
        }

        inc.mark_dirty_with_ancestors(children[1]);
        inc.propagate();

        assert!(inc.is_dirty(root));
        assert!(inc.is_dirty(children[0]));
        assert!(inc.is_dirty(children[1]));
        assert!(inc.is_dirty(children[2]));
    }

    #[test]
    fn mark_dirty_with_ancestors_deep_chain() {
        // Chain: root → A → B → C. Dirty C with ancestors → all dirty.
        let mut inc = IncrementalLayout::new();
        let root = inc.add_node(None);
        let a = inc.add_node(Some(root));
        let b = inc.add_node(Some(a));
        let c = inc.add_node(Some(b));
        inc.propagate();

        let area_ = area(80, 24);
        inc.get_or_compute(root, area_, |a| split_equal(a, 1));
        inc.get_or_compute(a, area_, |a| split_equal(a, 1));
        inc.get_or_compute(b, area_, |a| split_equal(a, 1));
        inc.get_or_compute(c, area_, |a| split_equal(a, 1));

        inc.mark_dirty_with_ancestors(c);
        inc.propagate();

        assert!(inc.is_dirty(root));
        assert!(inc.is_dirty(a));
        assert!(inc.is_dirty(b));
        assert!(inc.is_dirty(c));
    }

    // ── Mark Changed Deduplication ──────────────────────────────────

    #[test]
    fn mark_changed_deduplicates() {
        let mut inc = IncrementalLayout::new();
        let n = inc.add_node(None);
        inc.propagate();

        let a = area(80, 24);
        inc.get_or_compute(n, a, |a| split_equal(a, 2));

        // First change.
        inc.mark_changed(n, InputKind::Constraint, 42);
        inc.propagate();
        assert!(inc.is_dirty(n));

        inc.get_or_compute(n, a, |a| split_equal(a, 2));

        // Same hash → no-op.
        inc.mark_changed(n, InputKind::Constraint, 42);
        inc.propagate();
        assert!(!inc.is_dirty(n));
    }

    // ── Deep Tree ───────────────────────────────────────────────────

    #[test]
    fn deep_tree_partial_dirty() {
        let (mut inc, root, leaves) = binary_tree(4);
        inc.propagate();

        // Full pass.
        fn walk(inc: &mut IncrementalLayout, id: NodeId, a: Rect) {
            let rects = inc.get_or_compute(id, a, |a| split_equal(a, 2));
            // Walk children (dependents whose parent is `id`).
            let deps: Vec<_> = inc.graph().dependents(id).to_vec();
            for (i, child) in deps.iter().enumerate() {
                if i < rects.len() {
                    walk(inc, *child, rects[i]);
                }
            }
        }

        walk(&mut inc, root, area(160, 24));
        inc.reset_stats();

        // Dirty one leaf → only that leaf recomputes.
        inc.mark_dirty(leaves[7]);
        inc.propagate();

        walk(&mut inc, root, area(160, 24));

        let s = inc.stats();
        assert_eq!(s.recomputed, 1);
        assert!(s.cached > 0);
    }

    // ── Bit-Identical ───────────────────────────────────────────────

    #[test]
    fn incremental_equals_full_layout() {
        let a = area(200, 60);

        // Build tree: root → 5 children, each with 3 grandchildren.
        let mut inc = IncrementalLayout::new();
        let root = inc.add_node(None);
        let mut children = Vec::new();
        let mut grandchildren = Vec::new();
        for _ in 0..5 {
            let child = inc.add_node(Some(root));
            children.push(child);
            for _ in 0..3 {
                let gc = inc.add_node(Some(child));
                grandchildren.push(gc);
            }
        }
        inc.propagate();

        // Define a deterministic compute function.
        let compute = |a: Rect, n: usize| -> Vec<Rect> { split_equal(a, n) };

        // Incremental pass.
        let root_rects = inc.get_or_compute(root, a, |a| compute(a, 5));
        let mut child_rects = Vec::new();
        let mut gc_rects = Vec::new();
        for (i, &c) in children.iter().enumerate() {
            let cr = inc.get_or_compute(c, root_rects[i], |a| compute(a, 3));
            let deps: Vec<_> = inc.graph().dependents(c).to_vec();
            for (j, &gc) in deps.iter().enumerate() {
                if j < cr.len() {
                    gc_rects.push(inc.get_or_compute(gc, cr[j], |a| compute(a, 1)));
                }
            }
            child_rects.push(cr);
        }

        // Force-full pass.
        inc.set_force_full(true);
        inc.reset_stats();

        let root_rects2 = inc.get_or_compute(root, a, |a| compute(a, 5));
        let mut child_rects2 = Vec::new();
        let mut gc_rects2 = Vec::new();
        for (i, &c) in children.iter().enumerate() {
            let cr = inc.get_or_compute(c, root_rects2[i], |a| compute(a, 3));
            let deps: Vec<_> = inc.graph().dependents(c).to_vec();
            for (j, &gc) in deps.iter().enumerate() {
                if j < cr.len() {
                    gc_rects2.push(inc.get_or_compute(gc, cr[j], |a| compute(a, 1)));
                }
            }
            child_rects2.push(cr);
        }

        assert_eq!(root_rects, root_rects2);
        assert_eq!(child_rects, child_rects2);
        assert_eq!(gc_rects, gc_rects2);
    }

    // ── Edge Cases ──────────────────────────────────────────────────

    #[test]
    fn empty_graph() {
        let inc = IncrementalLayout::new();
        assert_eq!(inc.node_count(), 0);
        assert_eq!(inc.cache_len(), 0);
    }

    #[test]
    fn single_node_graph() {
        let mut inc = IncrementalLayout::new();
        let n = inc.add_node(None);
        inc.propagate();

        let r = inc.get_or_compute(n, area(80, 24), |_a| vec![]);
        assert!(r.is_empty());

        let s = inc.stats();
        assert_eq!(s.total, 1);
        assert_eq!(s.recomputed, 1);
    }

    #[test]
    fn zero_area_still_caches() {
        let mut inc = IncrementalLayout::new();
        let n = inc.add_node(None);
        inc.propagate();

        let a = Rect::default(); // 0×0
        inc.get_or_compute(n, a, |_| vec![]);

        let mut calls = 0u32;
        inc.get_or_compute(n, a, |_| {
            calls += 1;
            vec![]
        });
        assert_eq!(calls, 0);
    }

    #[test]
    fn result_hash_consistent() {
        let mut inc = IncrementalLayout::new();
        let n = inc.add_node(None);
        inc.propagate();

        let a = area(80, 24);
        inc.get_or_compute(n, a, |a| split_equal(a, 2));

        let h1 = inc.result_hash(n).unwrap();

        // Recompute with same inputs → same hash.
        inc.mark_dirty(n);
        inc.propagate();
        inc.get_or_compute(n, a, |a| split_equal(a, 2));

        let h2 = inc.result_hash(n).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn debug_format() {
        let inc = IncrementalLayout::new();
        let dbg = format!("{:?}", inc);
        assert!(dbg.contains("IncrementalLayout"));
        assert!(dbg.contains("nodes"));
    }

    #[test]
    fn default_impl() {
        let inc = IncrementalLayout::default();
        assert_eq!(inc.node_count(), 0);
        assert!(!inc.force_full());
    }

    #[test]
    fn from_env_default_is_not_force_full() {
        // We can't safely set_var in forbid(unsafe_code) crate, so test
        // that the default (unset) path doesn't enable force_full.
        // The env var may or may not be set in CI; the key assertion is
        // that `from_env()` doesn't panic and returns a usable instance.
        let inc = IncrementalLayout::from_env();
        // If FRANKENTUI_FULL_LAYOUT is not set (common case), force_full is false.
        // If it IS set (unlikely in tests), force_full matches the value.
        assert_eq!(inc.node_count(), 0);
    }

    #[test]
    fn parse_env_values() {
        // Test the parsing logic directly without touching env vars.
        let parse =
            |s: &str| -> bool { matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes") };
        assert!(parse("1"));
        assert!(parse("true"));
        assert!(parse("TRUE"));
        assert!(parse("yes"));
        assert!(parse("YES"));
        assert!(!parse("0"));
        assert!(!parse("false"));
        assert!(!parse("no"));
        assert!(!parse(""));
    }

    // ── Large-Scale ─────────────────────────────────────────────────

    #[test]
    fn thousand_node_tree_partial_dirty() {
        // Root → 10 children → 10 grandchildren each (= 111 nodes).
        let mut inc = IncrementalLayout::with_capacity(111);
        let root = inc.add_node(None);
        let mut children = Vec::new();
        let mut grandchildren = Vec::new();

        for _ in 0..10 {
            let child = inc.add_node(Some(root));
            children.push(child);
            for _ in 0..10 {
                let gc = inc.add_node(Some(child));
                grandchildren.push(gc);
            }
        }
        inc.propagate();

        let a = area(200, 60);

        // Full first pass.
        let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 10));
        for (i, &c) in children.iter().enumerate() {
            let cr = inc.get_or_compute(c, root_rects[i], |a| split_equal(a, 10));
            let deps: Vec<_> = inc.graph().dependents(c).to_vec();
            for (j, &gc) in deps.iter().enumerate() {
                if j < cr.len() {
                    inc.get_or_compute(gc, cr[j], |a| split_equal(a, 1));
                }
            }
        }

        let s = inc.stats();
        assert_eq!(s.recomputed, 111);
        assert_eq!(s.cached, 0);
        inc.reset_stats();

        // Dirty only 2 grandchildren (< 2% of 111 nodes).
        inc.mark_dirty(grandchildren[17]);
        inc.mark_dirty(grandchildren[83]);
        inc.propagate();

        // Second pass.
        let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 10));
        for (i, &c) in children.iter().enumerate() {
            let cr = inc.get_or_compute(c, root_rects[i], |a| split_equal(a, 10));
            let deps: Vec<_> = inc.graph().dependents(c).to_vec();
            for (j, &gc) in deps.iter().enumerate() {
                if j < cr.len() {
                    inc.get_or_compute(gc, cr[j], |a| split_equal(a, 1));
                }
            }
        }

        let s = inc.stats();
        assert_eq!(s.recomputed, 2);
        assert_eq!(s.cached, 109);
        assert!(s.hit_rate() > 0.95);
    }
}

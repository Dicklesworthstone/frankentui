//! Dependency graph for incremental layout invalidation (bd-3p4y1.1).
//!
//! # Design
//!
//! The dependency graph tracks which layout computations depend on which
//! inputs. When an input changes (constraint, content, style, children),
//! only the affected subtree is marked dirty and re-evaluated.
//!
//! ## Data Structure: Per-Node Adjacency Lists
//!
//! Each node stores fixed-size metadata (36 bytes) inline. Edges are
//! stored in per-node `Vec<NodeId>` lists for both forward (deps) and
//! reverse (dependents) directions, ensuring correct adjacency even
//! when edges are added in arbitrary order.
//!
//! ### Complexity
//!
//! | Operation            | Time         | Space           |
//! |----------------------|--------------|-----------------|
//! | Create node          | O(1) amort.  | +36 bytes       |
//! | Mark dirty           | O(1)         | —               |
//! | Propagate dirty      | O(k)         | O(k) queue      |
//! | Add dependency edge  | O(V+E) cycle | +4 bytes/edge   |
//! | Cycle detection      | O(V + E)     | O(V) coloring   |
//! | Query dirty set      | O(n) scan    | —               |
//!
//! Where k = dirty nodes + their transitive dependents, n = total nodes.
//!
//! ### Memory: 40 bytes per node (struct only)
//!
//! ```text
//! DepNode {
//!     generation: u32,       //  4 bytes — dirty-check generation
//!     dirty_gen: u32,        //  4 bytes — generation when last dirtied
//!     constraint_hash: u64,  //  8 bytes — detect constraint changes
//!     content_hash: u64,     //  8 bytes — detect content changes
//!     style_hash: u64,       //  8 bytes — detect style changes
//!     parent: u32,           //  4 bytes — parent NodeId (u32::MAX = none)
//! }                          // = 40 bytes (36 raw + 4 alignment padding)
//! ```
//!
//! # Dirty Propagation
//!
//! When a node is marked dirty, BFS traverses reverse edges (dependents)
//! and marks each reachable node dirty. The dirty set is the transitive
//! closure of the initially dirty nodes under the "depends-on" relation.
//!
//! # Cycle Detection
//!
//! Layout cycles are bugs. The graph detects cycles on edge insertion
//! using DFS reachability: before adding A → B, check that B cannot
//! already reach A via existing edges. This is O(V + E) worst case
//! but typically fast due to shallow widget trees.
//!
//! # Deterministic Traversal
//!
//! Dirty nodes are visited in DFS pre-order (matching full layout
//! traversal), ensuring bit-identical output regardless of whether
//! the computation is incremental or full.

use std::collections::VecDeque;
use std::fmt;

// ============================================================================
// NodeId
// ============================================================================

/// Lightweight handle into the dependency graph.
///
/// Uses `u32` for compactness (supports up to ~4 billion nodes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    /// Sentinel value meaning "no parent".
    const NONE: u32 = u32::MAX;

    /// Create a NodeId from a raw u32 index.
    #[must_use]
    pub fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Get the raw u32 index.
    #[must_use]
    pub fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "N{}", self.0)
    }
}

// ============================================================================
// InputHash — what changed
// ============================================================================

/// Identifies which input domain changed on a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// Size constraints (min/max/preferred width/height).
    Constraint,
    /// Widget content (text, children list, embedded content).
    Content,
    /// Style properties (padding, margin, border, flex properties).
    Style,
}

// ============================================================================
// DepNode
// ============================================================================

/// Per-node metadata in the dependency graph. 40 bytes (36 raw + alignment).
///
/// Edge adjacency is stored separately in `Vec<Vec<NodeId>>` to ensure
/// correctness when edges are added in arbitrary order.
#[derive(Clone)]
struct DepNode {
    /// Global generation counter at creation time.
    generation: u32,
    /// Generation when this node was last marked dirty.
    /// Dirty if `dirty_gen >= generation` (after last clean).
    dirty_gen: u32,
    /// Hash of constraint inputs.
    constraint_hash: u64,
    /// Hash of content inputs.
    content_hash: u64,
    /// Hash of style inputs.
    style_hash: u64,
    /// Parent node (u32::MAX = root/no parent).
    parent: u32,
}

impl DepNode {
    fn new(generation: u32) -> Self {
        Self {
            generation,
            dirty_gen: 0,
            constraint_hash: 0,
            content_hash: 0,
            style_hash: 0,
            parent: NodeId::NONE,
        }
    }

    fn is_dirty(&self) -> bool {
        self.dirty_gen >= self.generation
    }
}

// ============================================================================
// CycleError
// ============================================================================

/// Error returned when adding an edge would create a cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleError {
    pub from: NodeId,
    pub to: NodeId,
}

impl fmt::Display for CycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "layout cycle detected: {} → {} would create a cycle",
            self.from, self.to
        )
    }
}

impl std::error::Error for CycleError {}

// ============================================================================
// DepGraph
// ============================================================================

/// Dependency graph for incremental layout invalidation.
///
/// Tracks layout nodes and their dependencies. When a node's input
/// changes, the graph propagates dirtiness to all transitive dependents.
///
/// # Examples
///
/// ```
/// use ftui_layout::dep_graph::{DepGraph, InputKind};
///
/// let mut graph = DepGraph::new();
///
/// // Create a simple parent → child dependency.
/// let parent = graph.add_node();
/// let child = graph.add_node();
/// graph.add_edge(child, parent).unwrap();  // child depends on parent
///
/// // Changing the parent dirties the child.
/// graph.mark_changed(parent, InputKind::Constraint, 42);
/// let dirty = graph.propagate();
/// assert!(dirty.contains(&parent));
/// assert!(dirty.contains(&child));
/// ```
pub struct DepGraph {
    nodes: Vec<DepNode>,
    /// Forward adjacency: `fwd_adj[i]` = nodes that node `i` depends on.
    fwd_adj: Vec<Vec<NodeId>>,
    /// Reverse adjacency: `rev_adj[i]` = nodes that depend on node `i`.
    rev_adj: Vec<Vec<NodeId>>,
    /// Current generation counter for dirty tracking.
    current_gen: u32,
    /// Pending dirty nodes (not yet propagated).
    pending_dirty: Vec<NodeId>,
    /// Free list for recycled node slots.
    free_list: Vec<u32>,
}

impl DepGraph {
    /// Create an empty dependency graph.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            fwd_adj: Vec::new(),
            rev_adj: Vec::new(),
            current_gen: 1,
            pending_dirty: Vec::new(),
            free_list: Vec::new(),
        }
    }

    /// Create a graph with pre-allocated capacity.
    #[must_use]
    pub fn with_capacity(node_cap: usize, _edge_cap: usize) -> Self {
        Self {
            nodes: Vec::with_capacity(node_cap),
            fwd_adj: Vec::with_capacity(node_cap),
            rev_adj: Vec::with_capacity(node_cap),
            current_gen: 1,
            pending_dirty: Vec::new(),
            free_list: Vec::new(),
        }
    }

    /// Add a new node to the graph. Returns its stable identifier.
    pub fn add_node(&mut self) -> NodeId {
        if let Some(slot) = self.free_list.pop() {
            self.nodes[slot as usize] = DepNode::new(self.current_gen);
            self.fwd_adj[slot as usize].clear();
            self.rev_adj[slot as usize].clear();
            NodeId(slot)
        } else {
            let id = self.nodes.len() as u32;
            self.nodes.push(DepNode::new(self.current_gen));
            self.fwd_adj.push(Vec::new());
            self.rev_adj.push(Vec::new());
            NodeId(id)
        }
    }

    /// Remove a node, recycling its slot. Edges are lazily cleaned.
    pub fn remove_node(&mut self, id: NodeId) {
        let idx = id.0 as usize;
        if idx < self.nodes.len() {
            self.nodes[idx].generation = 0;
            self.nodes[idx].dirty_gen = 0;
            self.fwd_adj[idx].clear();
            self.rev_adj[idx].clear();
            self.free_list.push(id.0);
        }
    }

    /// Total number of live nodes.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len() - self.free_list.len()
    }

    /// Total number of edges (forward).
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.fwd_adj.iter().map(|v| v.len()).sum()
    }

    /// Set the parent of a node (structural tree relationship).
    pub fn set_parent(&mut self, child: NodeId, parent: NodeId) {
        if (child.0 as usize) < self.nodes.len() {
            self.nodes[child.0 as usize].parent = parent.0;
        }
    }

    /// Get the parent of a node, if any.
    #[must_use]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        let node = self.nodes.get(id.0 as usize)?;
        if node.parent == NodeId::NONE {
            None
        } else {
            Some(NodeId(node.parent))
        }
    }

    /// Add a dependency edge: `from` depends on `to`.
    ///
    /// Returns `Err(CycleError)` if this would create a cycle.
    pub fn add_edge(&mut self, from: NodeId, to: NodeId) -> Result<(), CycleError> {
        // Self-loops are cycles.
        if from == to {
            return Err(CycleError { from, to });
        }

        // Check: can `to` already reach `from` via existing forward edges?
        // If so, adding `from → to` would create a cycle.
        if self.can_reach(to, from) {
            return Err(CycleError { from, to });
        }

        // Add forward edge: from depends on to.
        self.fwd_adj[from.0 as usize].push(to);
        // Add reverse edge: to is depended on by from.
        self.rev_adj[to.0 as usize].push(from);

        Ok(())
    }

    /// Check if `from` can reach `to` via forward edges (DFS).
    fn can_reach(&self, from: NodeId, to: NodeId) -> bool {
        let mut visited = vec![false; self.nodes.len()];
        let mut stack = vec![from];

        while let Some(current) = stack.pop() {
            if current == to {
                return true;
            }
            let idx = current.0 as usize;
            if idx >= self.nodes.len() || visited[idx] {
                continue;
            }
            visited[idx] = true;

            if self.nodes[idx].generation == 0 {
                continue; // Dead node.
            }
            for &dep in &self.fwd_adj[idx] {
                if !visited[dep.0 as usize] {
                    stack.push(dep);
                }
            }
        }
        false
    }

    /// Mark a node's input as changed. The node and its transitive
    /// dependents will be dirtied on the next `propagate()` call.
    ///
    /// The `new_hash` is compared against the stored hash for the given
    /// `kind`. If unchanged, the node is not dirtied (deduplication).
    pub fn mark_changed(&mut self, id: NodeId, kind: InputKind, new_hash: u64) {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() {
            return;
        }
        let node = &mut self.nodes[idx];
        if node.generation == 0 {
            return; // Dead node.
        }

        let old_hash = match kind {
            InputKind::Constraint => &mut node.constraint_hash,
            InputKind::Content => &mut node.content_hash,
            InputKind::Style => &mut node.style_hash,
        };

        if *old_hash == new_hash {
            return; // No actual change.
        }
        *old_hash = new_hash;

        // Mark dirty.
        node.dirty_gen = self.current_gen;
        self.pending_dirty.push(id);
    }

    /// Force-mark a node as dirty without hash comparison.
    pub fn mark_dirty(&mut self, id: NodeId) {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() {
            return;
        }
        let node = &mut self.nodes[idx];
        if node.generation == 0 {
            return;
        }
        node.dirty_gen = self.current_gen;
        self.pending_dirty.push(id);
    }

    /// Propagate dirtiness from pending dirty nodes to all transitive
    /// dependents via BFS on reverse edges.
    ///
    /// Returns the complete dirty set in **DFS pre-order** (matching
    /// full layout traversal order) for deterministic recomputation.
    pub fn propagate(&mut self) -> Vec<NodeId> {
        if self.pending_dirty.is_empty() {
            return Vec::new();
        }

        // BFS to find all transitive dependents.
        let mut queue = VecDeque::new();
        let mut visited = vec![false; self.nodes.len()];

        for &id in &self.pending_dirty {
            let idx = id.0 as usize;
            if idx < self.nodes.len() && !visited[idx] {
                visited[idx] = true;
                queue.push_back(id);
            }
        }
        self.pending_dirty.clear();

        while let Some(current) = queue.pop_front() {
            let idx = current.0 as usize;
            if self.nodes[idx].generation == 0 {
                continue;
            }

            // Mark dirty.
            self.nodes[idx].dirty_gen = self.current_gen;

            // Enqueue all reverse-edge dependents.
            for i in 0..self.rev_adj[idx].len() {
                let dependent = self.rev_adj[idx][i];
                let dep_idx = dependent.0 as usize;
                if dep_idx < self.nodes.len() && !visited[dep_idx] {
                    visited[dep_idx] = true;
                    queue.push_back(dependent);
                }
            }
        }

        // Collect dirty set in DFS pre-order for deterministic traversal.
        self.collect_dirty_dfs_preorder()
    }

    /// Collect all dirty nodes in DFS pre-order from roots.
    fn collect_dirty_dfs_preorder(&self) -> Vec<NodeId> {
        // Find roots: dirty nodes with no dirty parent.
        let mut roots = Vec::new();
        for (i, node) in self.nodes.iter().enumerate() {
            if node.generation == 0 || !node.is_dirty() {
                continue;
            }
            let is_root = if node.parent == NodeId::NONE {
                true
            } else {
                let parent = &self.nodes[node.parent as usize];
                parent.generation == 0 || !parent.is_dirty()
            };
            if is_root {
                roots.push(NodeId(i as u32));
            }
        }
        roots.sort(); // Deterministic ordering.

        // DFS pre-order from each root.
        let mut result = Vec::new();
        let mut visited = vec![false; self.nodes.len()];

        for root in roots {
            self.dfs_preorder(root, &mut result, &mut visited);
        }
        result
    }

    /// DFS pre-order traversal of dirty nodes via reverse edges (children).
    fn dfs_preorder(&self, id: NodeId, result: &mut Vec<NodeId>, visited: &mut [bool]) {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || visited[idx] {
            return;
        }
        let node = &self.nodes[idx];
        if node.generation == 0 || !node.is_dirty() {
            return;
        }
        visited[idx] = true;
        result.push(id);

        // Visit dependents (children in the dirty tree).
        let mut children: Vec<NodeId> = self.rev_adj[idx]
            .iter()
            .filter(|d| {
                let di = d.0 as usize;
                di < self.nodes.len()
                    && !visited[di]
                    && self.nodes[di].generation != 0
                    && self.nodes[di].is_dirty()
            })
            .copied()
            .collect();
        children.sort(); // Deterministic.
        for child in children {
            self.dfs_preorder(child, result, visited);
        }
    }

    /// Check if a node is currently dirty.
    #[must_use]
    pub fn is_dirty(&self, id: NodeId) -> bool {
        self.nodes
            .get(id.0 as usize)
            .is_some_and(|n| n.generation != 0 && n.is_dirty())
    }

    /// Clean a single node (mark as not dirty).
    pub fn clean(&mut self, id: NodeId) {
        if let Some(node) = self.nodes.get_mut(id.0 as usize) {
            node.generation = self.current_gen;
            node.dirty_gen = 0;
        }
    }

    /// Clean all nodes and advance the generation.
    pub fn clean_all(&mut self) {
        self.current_gen = self.current_gen.wrapping_add(1);
        if self.current_gen == 0 {
            self.current_gen = 1; // Skip 0 (dead sentinel).
        }
        for node in &mut self.nodes {
            if node.generation != 0 {
                node.generation = self.current_gen;
                node.dirty_gen = 0;
            }
        }
        self.pending_dirty.clear();
    }

    /// Get the constraint hash for a node.
    #[must_use]
    pub fn constraint_hash(&self, id: NodeId) -> Option<u64> {
        self.nodes
            .get(id.0 as usize)
            .filter(|n| n.generation != 0)
            .map(|n| n.constraint_hash)
    }

    /// Get the content hash for a node.
    #[must_use]
    pub fn content_hash(&self, id: NodeId) -> Option<u64> {
        self.nodes
            .get(id.0 as usize)
            .filter(|n| n.generation != 0)
            .map(|n| n.content_hash)
    }

    /// Get the style hash for a node.
    #[must_use]
    pub fn style_hash(&self, id: NodeId) -> Option<u64> {
        self.nodes
            .get(id.0 as usize)
            .filter(|n| n.generation != 0)
            .map(|n| n.style_hash)
    }

    /// Iterate all live, dirty node IDs.
    pub fn dirty_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.generation != 0 && n.is_dirty())
            .map(|(i, _)| NodeId(i as u32))
    }

    /// Count of currently dirty nodes.
    #[must_use]
    pub fn dirty_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| n.generation != 0 && n.is_dirty())
            .count()
    }

    /// Get forward dependencies for a node (what it depends on).
    #[must_use]
    pub fn dependencies(&self, id: NodeId) -> &[NodeId] {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].generation == 0 {
            return &[];
        }
        &self.fwd_adj[idx]
    }

    /// Get reverse dependencies for a node (what depends on it).
    #[must_use]
    pub fn dependents(&self, id: NodeId) -> &[NodeId] {
        let idx = id.0 as usize;
        if idx >= self.nodes.len() || self.nodes[idx].generation == 0 {
            return &[];
        }
        &self.rev_adj[idx]
    }

    /// Invalidate all nodes (equivalent to full layout).
    /// Used as a fallback when incremental is not possible.
    pub fn invalidate_all(&mut self) {
        for (i, node) in self.nodes.iter_mut().enumerate() {
            if node.generation != 0 {
                node.dirty_gen = self.current_gen;
                self.pending_dirty.push(NodeId(i as u32));
            }
        }
    }
}

impl Default for DepGraph {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_node_returns_sequential_ids() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        let c = g.add_node();
        assert_eq!(a.raw(), 0);
        assert_eq!(b.raw(), 1);
        assert_eq!(c.raw(), 2);
        assert_eq!(g.node_count(), 3);
    }

    #[test]
    fn remove_node_recycles_slot() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let _b = g.add_node();
        g.remove_node(a);
        assert_eq!(g.node_count(), 1);
        let c = g.add_node();
        assert_eq!(c.raw(), 0); // Recycled slot.
        assert_eq!(g.node_count(), 2);
    }

    #[test]
    fn add_edge_creates_dependency() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        g.add_edge(a, b).unwrap();
        assert_eq!(g.dependencies(a), &[b]);
        assert_eq!(g.dependents(b), &[a]);
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn self_loop_detected() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let err = g.add_edge(a, a).unwrap_err();
        assert_eq!(err, CycleError { from: a, to: a });
    }

    #[test]
    fn two_node_cycle_detected() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        g.add_edge(a, b).unwrap();
        let err = g.add_edge(b, a).unwrap_err();
        assert_eq!(err, CycleError { from: b, to: a });
    }

    #[test]
    fn three_node_cycle_detected() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        let c = g.add_node();
        g.add_edge(a, b).unwrap();
        g.add_edge(b, c).unwrap();
        let err = g.add_edge(c, a).unwrap_err();
        assert_eq!(err, CycleError { from: c, to: a });
    }

    #[test]
    fn dag_allows_diamond() {
        // A → B, A → C, B → D, C → D (diamond, no cycle).
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        let c = g.add_node();
        let d = g.add_node();
        g.add_edge(a, b).unwrap();
        g.add_edge(a, c).unwrap();
        g.add_edge(b, d).unwrap();
        g.add_edge(c, d).unwrap();
        assert_eq!(g.edge_count(), 4);
    }

    #[test]
    fn mark_changed_deduplicates() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        g.mark_changed(a, InputKind::Constraint, 42);
        assert!(g.is_dirty(a));

        // Same hash → no additional dirtying.
        g.clean(a);
        g.mark_changed(a, InputKind::Constraint, 42);
        assert!(!g.is_dirty(a)); // Hash unchanged.
    }

    #[test]
    fn mark_changed_different_hash() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        g.mark_changed(a, InputKind::Constraint, 42);
        g.clean(a);
        g.mark_changed(a, InputKind::Constraint, 99); // Different hash.
        assert!(g.is_dirty(a));
    }

    #[test]
    fn propagate_single_node() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        g.mark_dirty(a);
        let dirty = g.propagate();
        assert_eq!(dirty, vec![a]);
    }

    #[test]
    fn propagate_parent_to_child() {
        let mut g = DepGraph::new();
        let parent = g.add_node();
        let child = g.add_node();
        g.add_edge(child, parent).unwrap(); // child depends on parent
        g.set_parent(child, parent);

        g.mark_dirty(parent);
        let dirty = g.propagate();
        assert!(dirty.contains(&parent));
        assert!(dirty.contains(&child));
    }

    #[test]
    fn propagate_chain() {
        // A → B → C: dirtying A should dirty B and C.
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        let c = g.add_node();
        g.add_edge(b, a).unwrap(); // B depends on A
        g.add_edge(c, b).unwrap(); // C depends on B
        g.set_parent(b, a);
        g.set_parent(c, b);

        g.mark_dirty(a);
        let dirty = g.propagate();
        assert_eq!(dirty.len(), 3);
        assert!(dirty.contains(&a));
        assert!(dirty.contains(&b));
        assert!(dirty.contains(&c));
    }

    #[test]
    fn propagate_only_affected_subtree() {
        // A → B, A → C. D is independent.
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        let c = g.add_node();
        let d = g.add_node();
        g.add_edge(b, a).unwrap();
        g.add_edge(c, a).unwrap();

        g.mark_dirty(a);
        let dirty = g.propagate();
        assert!(dirty.contains(&a));
        assert!(dirty.contains(&b));
        assert!(dirty.contains(&c));
        assert!(!dirty.contains(&d));
    }

    #[test]
    fn propagate_diamond_deduplicates() {
        // D depends on B and C, both depend on A.
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        let c = g.add_node();
        let d = g.add_node();
        g.add_edge(b, a).unwrap();
        g.add_edge(c, a).unwrap();
        g.add_edge(d, b).unwrap();
        g.add_edge(d, c).unwrap();

        g.mark_dirty(a);
        let dirty = g.propagate();
        assert_eq!(dirty.len(), 4);
        // Each node appears exactly once.
        let unique: std::collections::HashSet<_> = dirty.iter().collect();
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn clean_all_resets() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        g.add_edge(b, a).unwrap();
        g.mark_dirty(a);
        g.propagate();
        assert!(g.is_dirty(a));
        assert!(g.is_dirty(b));

        g.clean_all();
        assert!(!g.is_dirty(a));
        assert!(!g.is_dirty(b));
        assert_eq!(g.dirty_count(), 0);
    }

    #[test]
    fn invalidate_all_dirties_everything() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        let c = g.add_node();

        g.invalidate_all();
        let dirty = g.propagate();
        assert_eq!(dirty.len(), 3);
        assert!(dirty.contains(&a));
        assert!(dirty.contains(&b));
        assert!(dirty.contains(&c));
    }

    #[test]
    fn parent_child_relationship() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        assert_eq!(g.parent(a), None);
        g.set_parent(b, a);
        assert_eq!(g.parent(b), Some(a));
    }

    #[test]
    fn input_hashes_stored_independently() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        g.mark_changed(a, InputKind::Constraint, 1);
        g.mark_changed(a, InputKind::Content, 2);
        g.mark_changed(a, InputKind::Style, 3);

        assert_eq!(g.constraint_hash(a), Some(1));
        assert_eq!(g.content_hash(a), Some(2));
        assert_eq!(g.style_hash(a), Some(3));
    }

    #[test]
    fn dead_node_is_not_dirty() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        g.mark_dirty(a);
        g.remove_node(a);
        assert!(!g.is_dirty(a));
    }

    #[test]
    fn propagate_skips_dead_nodes() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        let c = g.add_node();
        g.add_edge(b, a).unwrap();
        g.add_edge(c, b).unwrap();

        // Remove B from the chain.
        g.remove_node(b);
        g.mark_dirty(a);
        let dirty = g.propagate();
        // B is dead, so C should not be reached (B's reverse edges are cleared).
        assert!(dirty.contains(&a));
        assert!(!dirty.contains(&c));
    }

    #[test]
    fn propagate_empty_returns_empty() {
        let mut g = DepGraph::new();
        let _a = g.add_node();
        let dirty = g.propagate();
        assert!(dirty.is_empty());
    }

    #[test]
    fn large_tree_propagation() {
        // 1000-node tree: root → 10 children → 10 grandchildren each.
        let mut g = DepGraph::with_capacity(1000, 1000);
        let root = g.add_node();
        let mut leaves = Vec::new();

        for _ in 0..10 {
            let child = g.add_node();
            g.add_edge(child, root).unwrap();
            g.set_parent(child, root);

            for _ in 0..10 {
                let grandchild = g.add_node();
                g.add_edge(grandchild, child).unwrap();
                g.set_parent(grandchild, child);
                leaves.push(grandchild);
            }
        }
        assert_eq!(g.node_count(), 111); // 1 + 10 + 100

        // Dirty root → all 111 nodes dirty.
        g.mark_dirty(root);
        let dirty = g.propagate();
        assert_eq!(dirty.len(), 111);

        // Clean and dirty one leaf → only that leaf.
        g.clean_all();
        g.mark_dirty(leaves[42]);
        let dirty = g.propagate();
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0], leaves[42]);
    }

    #[test]
    fn node_size_under_64_bytes() {
        assert!(
            std::mem::size_of::<DepNode>() <= 64,
            "DepNode is {} bytes, exceeds 64-byte budget",
            std::mem::size_of::<DepNode>(),
        );
    }

    #[test]
    fn node_size_exactly_40_bytes() {
        // 4+4+8+8+8+4 = 36 raw, but u64 alignment pads to 40.
        assert_eq!(
            std::mem::size_of::<DepNode>(),
            40,
            "DepNode should be 40 bytes, got {}",
            std::mem::size_of::<DepNode>(),
        );
    }

    #[test]
    fn multiple_input_changes_single_propagation() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        g.add_edge(b, a).unwrap();

        // Change constraint AND content before propagating.
        g.mark_changed(a, InputKind::Constraint, 10);
        g.mark_changed(a, InputKind::Content, 20);
        let dirty = g.propagate();
        assert_eq!(dirty.len(), 2); // a and b
    }

    #[test]
    fn propagate_dfs_preorder() {
        // Tree: R → A → (B, C). Should visit R, A, B, C in that order.
        let mut g = DepGraph::new();
        let r = g.add_node(); // 0
        let a = g.add_node(); // 1
        let b = g.add_node(); // 2
        let c = g.add_node(); // 3
        g.add_edge(a, r).unwrap();
        g.add_edge(b, a).unwrap();
        g.add_edge(c, a).unwrap();
        g.set_parent(a, r);
        g.set_parent(b, a);
        g.set_parent(c, a);

        g.mark_dirty(r);
        let dirty = g.propagate();
        assert_eq!(dirty.len(), 4);
        // DFS pre-order from root: R first, then A, then B and C.
        assert_eq!(dirty[0], r);
        assert_eq!(dirty[1], a);
        // B and C are siblings, ordered by NodeId.
        assert_eq!(dirty[2], b);
        assert_eq!(dirty[3], c);
    }

    #[test]
    fn dirty_count_accurate() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        let c = g.add_node();
        assert_eq!(g.dirty_count(), 0);

        g.mark_dirty(a);
        g.mark_dirty(b);
        g.propagate();
        assert_eq!(g.dirty_count(), 2);

        g.clean(a);
        assert_eq!(g.dirty_count(), 1);

        g.clean_all();
        assert_eq!(g.dirty_count(), 0);

        // Suppress unused variable warning.
        let _ = c;
    }

    #[test]
    fn dependencies_and_dependents_api() {
        let mut g = DepGraph::new();
        let a = g.add_node();
        let b = g.add_node();
        let c = g.add_node();
        g.add_edge(b, a).unwrap(); // b depends on a
        g.add_edge(c, a).unwrap(); // c depends on a

        assert_eq!(g.dependencies(b), &[a]);
        assert_eq!(g.dependencies(c), &[a]);
        assert_eq!(g.dependents(a).len(), 2);
    }
}

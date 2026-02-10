#![forbid(unsafe_code)]

//! Directed graph for focus navigation relationships.
//!
//! Each node represents a focusable widget identified by a [`FocusId`].
//! Edges encode navigation direction (up/down/left/right/next/prev).
//!
//! # Invariants
//!
//! 1. Node IDs are unique within the graph.
//! 2. Removing a node removes all edges incident on it (both incoming and outgoing).
//! 3. Edges reference only nodes that exist in the graph.
//! 4. Tab-order traversal visits nodes in ascending `tab_index` order,
//!    breaking ties by insertion order (via `FocusId`).
//!
//! # Complexity
//!
//! | Operation | Time |
//! |-----------|------|
//! | insert | O(1) |
//! | remove | O(E) worst case (cleans incoming edges) |
//! | connect | O(1) |
//! | navigate | O(1) |
//! | find_cycle | O(V+E) |

use ftui_core::geometry::Rect;
use std::collections::HashMap;

/// Unique identifier for a focusable widget.
pub type FocusId = u64;

/// Navigation direction for focus traversal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NavDirection {
    Up,
    Down,
    Left,
    Right,
    /// Tab order: forward.
    Next,
    /// Tab order: backward.
    Prev,
}

impl NavDirection {
    /// All six directions.
    pub const ALL: [NavDirection; 6] = [
        NavDirection::Up,
        NavDirection::Down,
        NavDirection::Left,
        NavDirection::Right,
        NavDirection::Next,
        NavDirection::Prev,
    ];
}

/// A focusable node in the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusNode {
    /// Unique identifier.
    pub id: FocusId,
    /// Bounding rectangle in terminal coordinates.
    pub bounds: Rect,
    /// Tab index for sequential navigation. Negative values skip tab order.
    pub tab_index: i32,
    /// Whether this node can receive focus.
    pub is_focusable: bool,
    /// Optional group for focus trapping regions.
    pub group_id: Option<u32>,
}

impl FocusNode {
    /// Create a new focusable node.
    #[must_use]
    pub fn new(id: FocusId, bounds: Rect) -> Self {
        Self {
            id,
            bounds,
            tab_index: 0,
            is_focusable: true,
            group_id: None,
        }
    }

    /// Builder: set tab index.
    #[must_use]
    pub fn with_tab_index(mut self, idx: i32) -> Self {
        self.tab_index = idx;
        self
    }

    /// Builder: set focusable flag.
    #[must_use]
    pub fn with_focusable(mut self, focusable: bool) -> Self {
        self.is_focusable = focusable;
        self
    }

    /// Builder: set group.
    #[must_use]
    pub fn with_group(mut self, group: u32) -> Self {
        self.group_id = Some(group);
        self
    }
}

/// Directed graph for focus navigation.
///
/// Nodes are focusable widgets; edges encode directional navigation.
/// The graph is sparse (most nodes have ≤6 outgoing edges).
#[derive(Debug, Default)]
pub struct FocusGraph {
    nodes: HashMap<FocusId, FocusNode>,
    /// Outgoing edges: (from, direction) → to.
    edges: HashMap<(FocusId, NavDirection), FocusId>,
}

impl FocusGraph {
    /// Create an empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a node. Returns the node's ID.
    ///
    /// If a node with the same ID exists, it is replaced.
    pub fn insert(&mut self, node: FocusNode) -> FocusId {
        let id = node.id;
        self.nodes.insert(id, node);
        id
    }

    /// Remove a node and all edges incident on it.
    ///
    /// Returns the removed node, or `None` if not present.
    #[must_use = "use the removed node (if any)"]
    pub fn remove(&mut self, id: FocusId) -> Option<FocusNode> {
        let node = self.nodes.remove(&id)?;

        // Remove outgoing edges from this node.
        for dir in NavDirection::ALL {
            self.edges.remove(&(id, dir));
        }

        // Remove incoming edges pointing to this node.
        self.edges.retain(|_, target| *target != id);

        Some(node)
    }

    /// Connect two nodes: navigating `dir` from `from` leads to `to`.
    ///
    /// Both nodes must already exist. Silently no-ops if either is missing.
    pub fn connect(&mut self, from: FocusId, dir: NavDirection, to: FocusId) {
        if self.nodes.contains_key(&from) && self.nodes.contains_key(&to) {
            self.edges.insert((from, dir), to);
        }
    }

    /// Disconnect an edge.
    pub fn disconnect(&mut self, from: FocusId, dir: NavDirection) {
        self.edges.remove(&(from, dir));
    }

    /// Navigate from a node in a direction.
    ///
    /// Returns the target node ID, or `None` if no edge exists.
    #[must_use = "use the returned target id (if any)"]
    pub fn navigate(&self, from: FocusId, dir: NavDirection) -> Option<FocusId> {
        self.edges.get(&(from, dir)).copied()
    }

    /// Look up a node by ID.
    #[must_use = "use the returned node (if any)"]
    pub fn get(&self, id: FocusId) -> Option<&FocusNode> {
        self.nodes.get(&id)
    }

    /// Number of nodes.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of edges.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Whether the graph is empty (no nodes).
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// All node IDs.
    pub fn node_ids(&self) -> impl Iterator<Item = FocusId> + '_ {
        self.nodes.keys().copied()
    }

    /// Nodes in tab order (ascending `tab_index`, ties broken by ID).
    /// Skips nodes with `tab_index < 0` or `!is_focusable`.
    pub fn tab_order(&self) -> Vec<FocusId> {
        let mut ordered: Vec<_> = self
            .nodes
            .values()
            .filter(|n| n.is_focusable && n.tab_index >= 0)
            .collect();
        ordered.sort_by(|a, b| a.tab_index.cmp(&b.tab_index).then(a.id.cmp(&b.id)));
        ordered.iter().map(|n| n.id).collect()
    }

    /// Nodes in a specific group, in tab order.
    pub fn group_tab_order(&self, group: u32) -> Vec<FocusId> {
        let mut ordered: Vec<_> = self
            .nodes
            .values()
            .filter(|n| n.is_focusable && n.tab_index >= 0 && n.group_id == Some(group))
            .collect();
        ordered.sort_by(|a, b| a.tab_index.cmp(&b.tab_index).then(a.id.cmp(&b.id)));
        ordered.iter().map(|n| n.id).collect()
    }

    /// Detect a cycle reachable from `start` following `Next` edges.
    ///
    /// Returns `Some(cycle)` where `cycle` is the sequence of node IDs
    /// forming the cycle (starting and ending at the same node), or `None`.
    ///
    /// Uses tortoise-and-hare for O(V) time, O(1) extra space.
    #[must_use = "use the returned cycle (if any) to diagnose invalid focus graphs"]
    pub fn find_cycle(&self, start: FocusId) -> Option<Vec<FocusId>> {
        // Phase 1: detect cycle with Floyd's algorithm.
        let mut slow = start;
        let mut fast = start;

        loop {
            slow = self.navigate(slow, NavDirection::Next)?;
            fast = self.navigate(fast, NavDirection::Next)?;
            fast = self.navigate(fast, NavDirection::Next)?;
            if slow == fast {
                break;
            }
        }

        // Phase 2: find cycle start.
        let mut p1 = start;
        let mut p2 = slow;
        while p1 != p2 {
            p1 = self.navigate(p1, NavDirection::Next)?;
            p2 = self.navigate(p2, NavDirection::Next)?;
        }

        // Phase 3: collect cycle.
        let cycle_start = p1;
        let mut cycle = vec![cycle_start];
        let mut current = self.navigate(cycle_start, NavDirection::Next)?;
        while current != cycle_start {
            cycle.push(current);
            current = self.navigate(current, NavDirection::Next)?;
        }
        cycle.push(cycle_start); // close the cycle

        Some(cycle)
    }

    /// Detect cycles reachable from `start` following any single direction.
    ///
    /// More general than `find_cycle` (which only follows `Next`).
    #[must_use = "use the returned cycle (if any) to diagnose invalid focus graphs"]
    pub fn find_cycle_in_direction(
        &self,
        start: FocusId,
        dir: NavDirection,
    ) -> Option<Vec<FocusId>> {
        let mut slow = start;
        let mut fast = start;

        loop {
            slow = self.navigate(slow, dir)?;
            fast = self.navigate(fast, dir)?;
            fast = self.navigate(fast, dir)?;
            if slow == fast {
                break;
            }
        }

        let mut p1 = start;
        let mut p2 = slow;
        while p1 != p2 {
            p1 = self.navigate(p1, dir)?;
            p2 = self.navigate(p2, dir)?;
        }

        let cycle_start = p1;
        let mut cycle = vec![cycle_start];
        let mut current = self.navigate(cycle_start, dir)?;
        while current != cycle_start {
            cycle.push(current);
            current = self.navigate(current, dir)?;
        }
        cycle.push(cycle_start);

        Some(cycle)
    }

    /// Build bidirectional `Next`/`Prev` chain from the current tab order.
    ///
    /// Overwrites existing `Next`/`Prev` edges. If `wrap` is true, the
    /// last node links back to the first (and vice versa).
    pub fn build_tab_chain(&mut self, wrap: bool) {
        self.edges
            .retain(|(_, dir), _| *dir != NavDirection::Next && *dir != NavDirection::Prev);
        let order = self.tab_order();
        if order.len() < 2 {
            return;
        }

        for pair in order.windows(2) {
            self.edges.insert((pair[0], NavDirection::Next), pair[1]);
            self.edges.insert((pair[1], NavDirection::Prev), pair[0]);
        }

        if wrap {
            let first = order[0];
            let last = *order.last().unwrap();
            self.edges.insert((last, NavDirection::Next), first);
            self.edges.insert((first, NavDirection::Prev), last);
        }
    }

    /// Clear all nodes and edges.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.edges.clear();
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    fn node(id: FocusId) -> FocusNode {
        FocusNode::new(id, rect(0, 0, 10, 1))
    }

    // --- Basic functionality ---

    #[test]
    fn empty_graph() {
        let g = FocusGraph::new();
        assert!(g.is_empty());
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn insert_node() {
        let mut g = FocusGraph::new();
        let id = g.insert(node(1));
        assert_eq!(id, 1);
        assert_eq!(g.node_count(), 1);
        assert!(g.get(1).is_some());
    }

    #[test]
    fn insert_replaces_existing() {
        let mut g = FocusGraph::new();
        g.insert(FocusNode::new(1, rect(0, 0, 10, 1)));
        g.insert(FocusNode::new(1, rect(5, 5, 20, 2)));
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.get(1).unwrap().bounds, rect(5, 5, 20, 2));
    }

    #[test]
    fn remove_node() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        let removed = g.remove(1);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().id, 1);
        assert!(g.is_empty());
    }

    #[test]
    fn remove_nonexistent() {
        let mut g = FocusGraph::new();
        assert!(g.remove(42).is_none());
    }

    // --- Navigation ---

    #[test]
    fn navigate_connected() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.insert(node(2));
        g.connect(1, NavDirection::Right, 2);

        assert_eq!(g.navigate(1, NavDirection::Right), Some(2));
    }

    #[test]
    fn navigate_unconnected() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        assert_eq!(g.navigate(1, NavDirection::Up), None);
    }

    #[test]
    fn navigate_nonexistent_node() {
        let g = FocusGraph::new();
        assert_eq!(g.navigate(99, NavDirection::Down), None);
    }

    #[test]
    fn connect_ignores_missing_nodes() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.connect(1, NavDirection::Next, 99); // 99 doesn't exist
        assert_eq!(g.navigate(1, NavDirection::Next), None);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn disconnect_edge() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.insert(node(2));
        g.connect(1, NavDirection::Right, 2);
        assert_eq!(g.navigate(1, NavDirection::Right), Some(2));

        g.disconnect(1, NavDirection::Right);
        assert_eq!(g.navigate(1, NavDirection::Right), None);
    }

    // --- Remove cleans edges ---

    #[test]
    fn remove_cleans_outgoing_edges() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.insert(node(2));
        g.connect(1, NavDirection::Next, 2);
        g.remove(1);
        // Edge (1, Next) should be gone.
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn remove_cleans_incoming_edges() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.insert(node(2));
        g.connect(1, NavDirection::Next, 2);
        g.remove(2);
        // Edge (1, Next) → 2 should be removed because target is gone.
        assert_eq!(g.edge_count(), 0);
        assert_eq!(g.navigate(1, NavDirection::Next), None);
    }

    // --- Tab order ---

    #[test]
    fn tab_order_sorts_by_index_then_id() {
        let mut g = FocusGraph::new();
        g.insert(node(3).with_tab_index(1));
        g.insert(node(1).with_tab_index(0));
        g.insert(node(2).with_tab_index(0));

        let order = g.tab_order();
        assert_eq!(order, vec![1, 2, 3]);
    }

    #[test]
    fn tab_order_skips_negative_index() {
        let mut g = FocusGraph::new();
        g.insert(node(1).with_tab_index(0));
        g.insert(node(2).with_tab_index(-1));
        g.insert(node(3).with_tab_index(1));

        let order = g.tab_order();
        assert_eq!(order, vec![1, 3]);
    }

    #[test]
    fn tab_order_skips_unfocusable() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.insert(node(2).with_focusable(false));
        g.insert(node(3));

        let order = g.tab_order();
        assert_eq!(order, vec![1, 3]);
    }

    // --- Group tab order ---

    #[test]
    fn group_tab_order_filters_by_group() {
        let mut g = FocusGraph::new();
        g.insert(node(1).with_group(1));
        g.insert(node(2).with_group(2));
        g.insert(node(3).with_group(1).with_tab_index(1));

        let order = g.group_tab_order(1);
        assert_eq!(order, vec![1, 3]);
    }

    // --- Build tab chain ---

    #[test]
    fn build_tab_chain_no_wrap() {
        let mut g = FocusGraph::new();
        g.insert(node(1).with_tab_index(0));
        g.insert(node(2).with_tab_index(1));
        g.insert(node(3).with_tab_index(2));

        g.build_tab_chain(false);

        assert_eq!(g.navigate(1, NavDirection::Next), Some(2));
        assert_eq!(g.navigate(2, NavDirection::Next), Some(3));
        assert_eq!(g.navigate(3, NavDirection::Next), None);

        assert_eq!(g.navigate(3, NavDirection::Prev), Some(2));
        assert_eq!(g.navigate(2, NavDirection::Prev), Some(1));
        assert_eq!(g.navigate(1, NavDirection::Prev), None);
    }

    #[test]
    fn build_tab_chain_wrap() {
        let mut g = FocusGraph::new();
        g.insert(node(1).with_tab_index(0));
        g.insert(node(2).with_tab_index(1));
        g.insert(node(3).with_tab_index(2));

        g.build_tab_chain(true);

        assert_eq!(g.navigate(3, NavDirection::Next), Some(1));
        assert_eq!(g.navigate(1, NavDirection::Prev), Some(3));
    }

    #[test]
    fn build_tab_chain_single_node_noop() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.build_tab_chain(true);
        assert_eq!(g.edge_count(), 0);
    }

    // --- Cycle detection ---

    #[test]
    fn no_cycle_linear() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.insert(node(2));
        g.insert(node(3));
        g.connect(1, NavDirection::Next, 2);
        g.connect(2, NavDirection::Next, 3);

        assert!(g.find_cycle(1).is_none());
    }

    #[test]
    fn simple_cycle() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.insert(node(2));
        g.insert(node(3));
        g.connect(1, NavDirection::Next, 2);
        g.connect(2, NavDirection::Next, 3);
        g.connect(3, NavDirection::Next, 1);

        let cycle = g.find_cycle(1);
        assert!(cycle.is_some());
        let c = cycle.unwrap();
        // Cycle should start and end with the same node.
        assert_eq!(c.first(), c.last());
        assert_eq!(c.len(), 4); // 1 → 2 → 3 → 1
    }

    #[test]
    fn self_loop_cycle() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.connect(1, NavDirection::Next, 1);

        let cycle = g.find_cycle(1);
        assert!(cycle.is_some());
        let c = cycle.unwrap();
        assert_eq!(c, vec![1, 1]);
    }

    #[test]
    fn cycle_in_middle() {
        // 1 → 2 → 3 → 4 → 2 (cycle at 2-3-4)
        let mut g = FocusGraph::new();
        for id in 1..=4 {
            g.insert(node(id));
        }
        g.connect(1, NavDirection::Next, 2);
        g.connect(2, NavDirection::Next, 3);
        g.connect(3, NavDirection::Next, 4);
        g.connect(4, NavDirection::Next, 2);

        let cycle = g.find_cycle(1);
        assert!(cycle.is_some());
        let c = cycle.unwrap();
        assert_eq!(c.first(), c.last());
        // The cycle is 2 → 3 → 4 → 2
        assert_eq!(c.len(), 4);
        assert!(c.contains(&2));
        assert!(c.contains(&3));
        assert!(c.contains(&4));
    }

    #[test]
    fn find_cycle_from_nonexistent_start() {
        let g = FocusGraph::new();
        assert!(g.find_cycle(99).is_none());
    }

    #[test]
    fn find_cycle_in_direction_right() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.insert(node(2));
        g.connect(1, NavDirection::Right, 2);
        g.connect(2, NavDirection::Right, 1);

        let cycle = g.find_cycle_in_direction(1, NavDirection::Right);
        assert!(cycle.is_some());
    }

    // --- Clear ---

    #[test]
    fn clear_empties_graph() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.insert(node(2));
        g.connect(1, NavDirection::Next, 2);
        g.clear();
        assert!(g.is_empty());
        assert_eq!(g.edge_count(), 0);
    }

    // --- Node builder ---

    #[test]
    fn node_builder_defaults() {
        let n = FocusNode::new(1, rect(0, 0, 10, 1));
        assert_eq!(n.tab_index, 0);
        assert!(n.is_focusable);
        assert!(n.group_id.is_none());
    }

    #[test]
    fn node_builder_chain() {
        let n = FocusNode::new(1, rect(0, 0, 10, 1))
            .with_tab_index(5)
            .with_focusable(false)
            .with_group(3);
        assert_eq!(n.tab_index, 5);
        assert!(!n.is_focusable);
        assert_eq!(n.group_id, Some(3));
    }

    // --- Edge cases ---

    #[test]
    fn multiple_directions_same_source() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.insert(node(2));
        g.insert(node(3));
        g.connect(1, NavDirection::Right, 2);
        g.connect(1, NavDirection::Down, 3);

        assert_eq!(g.navigate(1, NavDirection::Right), Some(2));
        assert_eq!(g.navigate(1, NavDirection::Down), Some(3));
        assert_eq!(g.edge_count(), 2);
    }

    #[test]
    fn overwrite_edge() {
        let mut g = FocusGraph::new();
        g.insert(node(1));
        g.insert(node(2));
        g.insert(node(3));
        g.connect(1, NavDirection::Next, 2);
        g.connect(1, NavDirection::Next, 3);

        assert_eq!(g.navigate(1, NavDirection::Next), Some(3));
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn node_ids_iteration() {
        let mut g = FocusGraph::new();
        g.insert(node(10));
        g.insert(node(20));
        g.insert(node(30));

        let mut ids: Vec<_> = g.node_ids().collect();
        ids.sort();
        assert_eq!(ids, vec![10, 20, 30]);
    }

    // --- Property-style tests ---

    #[test]
    fn property_remove_insert_idempotent() {
        let mut g = FocusGraph::new();
        let n = node(1);
        g.insert(n.clone());
        g.remove(1);
        g.insert(n.clone());
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.get(1).unwrap().id, 1);
    }

    #[test]
    fn property_tab_chain_wrap_forms_cycle() {
        let mut g = FocusGraph::new();
        for i in 1..=5 {
            g.insert(node(i).with_tab_index(i as i32));
        }
        g.build_tab_chain(true);

        let cycle = g.find_cycle(1);
        assert!(cycle.is_some());
        let c = cycle.unwrap();
        assert_eq!(c.len(), 6); // 5 nodes + closing node
    }

    #[test]
    fn property_tab_chain_no_wrap_no_cycle() {
        let mut g = FocusGraph::new();
        for i in 1..=5 {
            g.insert(node(i).with_tab_index(i as i32));
        }
        g.build_tab_chain(false);

        assert!(g.find_cycle(1).is_none());
    }

    #[test]
    fn property_bidirectional_chain_consistency() {
        let mut g = FocusGraph::new();
        for i in 1..=4 {
            g.insert(node(i).with_tab_index(i as i32));
        }
        g.build_tab_chain(false);

        // For every Next edge a→b, there should be a Prev edge b→a.
        for id in 1..=3u64 {
            let next = g.navigate(id, NavDirection::Next).unwrap();
            let prev_of_next = g.navigate(next, NavDirection::Prev).unwrap();
            assert_eq!(prev_of_next, id);
        }
    }

    // --- Stress ---

    #[test]
    fn stress_many_nodes() {
        let mut g = FocusGraph::new();
        for i in 0..1000 {
            g.insert(node(i).with_tab_index(i as i32));
        }
        g.build_tab_chain(true);

        assert_eq!(g.node_count(), 1000);
        // Navigate the full ring.
        let mut current = 0;
        for _ in 0..1000 {
            current = g.navigate(current, NavDirection::Next).unwrap();
        }
        assert_eq!(current, 0); // Full cycle back to start.
    }

    // --- Perf gates ---

    #[test]
    fn perf_insert_1000_nodes() {
        let start = std::time::Instant::now();
        let mut g = FocusGraph::new();
        for i in 0..1000 {
            g.insert(node(i));
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_micros() < 5000,
            "Inserting 1000 nodes took {}μs (budget: 5000μs)",
            elapsed.as_micros()
        );
    }

    #[test]
    fn perf_navigate_10000() {
        let mut g = FocusGraph::new();
        for i in 0..100 {
            g.insert(node(i).with_tab_index(i as i32));
        }
        g.build_tab_chain(true);

        let start = std::time::Instant::now();
        let mut current = 0;
        for _ in 0..10_000 {
            current = g.navigate(current, NavDirection::Next).unwrap();
        }
        let elapsed = start.elapsed();
        // Prevent optimizing away.
        assert!(current < 100);
        // Generous budget for shared CI/multi-agent environments.
        let budget: u128 =
            if std::env::var("CARGO_LLVM_COV").is_ok() || std::env::var("COVERAGE").is_ok() {
                16_000
            } else {
                8_000
            };
        assert!(
            elapsed.as_micros() < budget,
            "10,000 navigations took {}μs (budget: {}μs)",
            elapsed.as_micros(),
            budget
        );
    }

    #[test]
    fn perf_cycle_detection_1000() {
        let mut g = FocusGraph::new();
        for i in 0..1000 {
            g.insert(node(i).with_tab_index(i as i32));
        }
        g.build_tab_chain(true);

        let start = std::time::Instant::now();
        let cycle = g.find_cycle(0);
        let elapsed = start.elapsed();

        assert!(cycle.is_some());
        assert!(
            elapsed.as_micros() < 10_000,
            "Cycle detection on 1000-node ring took {}μs (budget: 10000μs)",
            elapsed.as_micros()
        );
    }
}

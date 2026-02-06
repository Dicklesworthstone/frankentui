#![forbid(unsafe_code)]

//! Deterministic layout engine for Mermaid diagrams.
//!
//! Implements a Sugiyama-style layered layout algorithm:
//!   1. Rank assignment (longest path from sources)
//!   2. Ordering within ranks (barycenter crossing minimization)
//!   3. Coordinate assignment (compact placement with spacing)
//!   4. Cluster boundary computation
//!   5. Simple edge routing (waypoints)
//!
//! All output is deterministic: identical IR input produces identical layout.
//! Coordinates are in abstract "world units", not terminal cells.

use crate::diagram::{grapheme_width, visual_width};
use crate::mermaid::{
    DiagramType, GraphDirection, IrEndpoint, IrNodeId, LayoutConstraint, MermaidConfig,
    MermaidDegradationPlan, MermaidDiagramIr, MermaidFidelity, append_jsonl_line,
};

// ── Layout output types ──────────────────────────────────────────────

/// A point in 2D layout space (world units).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutPoint {
    pub x: f64,
    pub y: f64,
}

/// An axis-aligned rectangle in layout space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl LayoutRect {
    #[must_use]
    pub fn center(&self) -> LayoutPoint {
        LayoutPoint {
            x: self.x + self.width / 2.0,
            y: self.y + self.height / 2.0,
        }
    }

    #[must_use]
    pub fn contains_point(&self, p: LayoutPoint) -> bool {
        p.x >= self.x && p.x <= self.x + self.width && p.y >= self.y && p.y <= self.y + self.height
    }

    /// Expand to include another rect, returning the bounding union.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }
}

/// Positioned node in the layout.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutNodeBox {
    pub node_idx: usize,
    pub rect: LayoutRect,
    pub label_rect: Option<LayoutRect>,
    pub rank: usize,
    pub order: usize,
}

/// Positioned cluster (subgraph) boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutClusterBox {
    pub cluster_idx: usize,
    pub rect: LayoutRect,
    pub title_rect: Option<LayoutRect>,
}

/// Routed edge as a sequence of waypoints.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutEdgePath {
    pub edge_idx: usize,
    pub waypoints: Vec<LayoutPoint>,
    /// Number of underlying IR edges represented by this path.
    ///
    /// `1` means this is a normal edge. Values `> 1` indicate an edge bundle
    /// produced by optional post-layout bundling (bd-70rmj).
    pub bundle_count: usize,
    /// Underlying IR edge indices represented by this bundled edge (sorted).
    ///
    /// For non-bundled edges (`bundle_count == 1`), this is empty to avoid
    /// unnecessary metadata.
    pub bundle_members: Vec<usize>,
}

/// Statistics from the layout computation.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutStats {
    pub iterations_used: usize,
    pub max_iterations: usize,
    pub budget_exceeded: bool,
    pub crossings: usize,
    pub ranks: usize,
    pub max_rank_width: usize,
    /// Total number of bends across all edge paths.
    pub total_bends: usize,
    /// Average position variance within ranks (lower = more regular).
    pub position_variance: f64,
}

/// Complete diagram layout result.
#[derive(Debug, Clone, PartialEq)]
pub struct DiagramLayout {
    pub nodes: Vec<LayoutNodeBox>,
    pub clusters: Vec<LayoutClusterBox>,
    pub edges: Vec<LayoutEdgePath>,
    pub bounding_box: LayoutRect,
    pub stats: LayoutStats,
    pub degradation: Option<MermaidDegradationPlan>,
}

/// Named stage in the layout pipeline for debug overlay hooks (bd-12d5s).
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutStageSnapshot {
    /// Stage identifier (e.g. "rank_assignment", "crossing_minimization", "position_assignment").
    pub stage: &'static str,
    /// Node positions at this stage (index matches `DiagramLayout::nodes`).
    pub node_positions: Vec<LayoutPoint>,
    /// Number of edge crossings at this stage.
    pub crossings: usize,
    /// Iteration count within this stage.
    pub iterations: usize,
}

/// Debug trace capturing per-stage layout snapshots (bd-12d5s).
///
/// When enabled via `layout_diagram_traced`, each major layout phase
/// appends a snapshot. The bd-4cwfj Debug Overlay uses these to render
/// step-by-step layout visualizations.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayoutTrace {
    /// Ordered snapshots for each layout stage.
    pub stages: Vec<LayoutStageSnapshot>,
}

impl LayoutTrace {
    /// Record a stage snapshot.
    pub fn record(
        &mut self,
        stage: &'static str,
        node_positions: Vec<LayoutPoint>,
        crossings: usize,
        iterations: usize,
    ) {
        self.stages.push(LayoutStageSnapshot {
            stage,
            node_positions,
            crossings,
            iterations,
        });
    }

    /// Emit the trace as JSONL evidence lines.
    pub fn emit_jsonl(&self, config: &MermaidConfig) {
        let Some(path) = config.log_path.as_deref() else {
            return;
        };
        for (i, snap) in self.stages.iter().enumerate() {
            let json = serde_json::json!({
                "event": "layout_trace",
                "stage_index": i,
                "stage": snap.stage,
                "node_count": snap.node_positions.len(),
                "crossings": snap.crossings,
                "iterations": snap.iterations,
            });
            let _ = crate::mermaid::append_jsonl_line(path, &json.to_string());
        }
    }
}

// ── Layout configuration ─────────────────────────────────────────────

/// Layout spacing parameters (world units).
#[derive(Debug, Clone, Copy)]
pub struct LayoutSpacing {
    pub node_width: f64,
    pub node_height: f64,
    pub rank_gap: f64,
    pub node_gap: f64,
    pub cluster_padding: f64,
    pub label_padding: f64,
}

impl Default for LayoutSpacing {
    fn default() -> Self {
        Self {
            node_width: 10.0,
            node_height: 3.0,
            rank_gap: 4.0,
            node_gap: 3.0,
            cluster_padding: 2.0,
            label_padding: 1.0,
        }
    }
}

// ── Internal graph representation ────────────────────────────────────

/// Adjacency list for the layout graph.
struct LayoutGraph {
    /// Number of nodes.
    n: usize,
    /// Forward edges: adj[u] = list of v where u→v.
    adj: Vec<Vec<usize>>,
    /// Reverse edges: rev[v] = list of u where u→v.
    rev: Vec<Vec<usize>>,
    /// Node IDs for deterministic tie-breaking.
    node_ids: Vec<String>,
}

impl LayoutGraph {
    fn from_ir(ir: &MermaidDiagramIr) -> Self {
        let n = ir.nodes.len();
        let mut adj = vec![vec![]; n];
        let mut rev = vec![vec![]; n];
        let node_ids: Vec<String> = ir.nodes.iter().map(|node| node.id.clone()).collect();

        for edge in &ir.edges {
            let from = endpoint_node_idx(ir, &edge.from);
            let to = endpoint_node_idx(ir, &edge.to);
            if let (Some(u), Some(v)) = (from, to)
                && u < n
                && v < n
                && u != v
            {
                adj[u].push(v);
                rev[v].push(u);
            }
        }

        // Sort adjacency lists for determinism.
        for list in &mut adj {
            list.sort_unstable();
            list.dedup();
        }
        for list in &mut rev {
            list.sort_unstable();
            list.dedup();
        }

        Self {
            n,
            adj,
            rev,
            node_ids,
        }
    }
}

/// Resolve an IR endpoint to a node index.
fn endpoint_node_idx(ir: &MermaidDiagramIr, ep: &IrEndpoint) -> Option<usize> {
    match ep {
        IrEndpoint::Node(IrNodeId(idx)) => Some(*idx),
        IrEndpoint::Port(port_id) => ir.ports.get(port_id.0).map(|p| p.node.0),
    }
}

// ── Content-aware node sizing ────────────────────────────────────────

/// Compute per-node (width, height) based on label text.
///
/// Nodes with labels wider than the default `node_width` get expanded
/// to fit, with `label_padding` on each side. Nodes without labels
/// keep the default size.
fn compute_node_sizes(ir: &MermaidDiagramIr, spacing: &LayoutSpacing) -> Vec<(f64, f64)> {
    ir.nodes
        .iter()
        .map(|node| {
            let label_width = node
                .label
                .and_then(|lid| ir.labels.get(lid.0))
                .map(|label| visual_width(&label.text) as f64)
                .unwrap_or(0.0);

            // For class diagram nodes with members, expand width for member text
            // and add height for each member line plus separator.
            let member_max_width = node
                .members
                .iter()
                .map(|m| visual_width(m) as f64)
                .fold(0.0_f64, f64::max);

            let width = spacing
                .node_width
                .max(label_width + 2.0 * spacing.label_padding)
                .max(member_max_width + 2.0 * spacing.label_padding);

            // Each member adds 1 line; separator adds 1 line.
            let member_height = if node.members.is_empty() {
                0.0
            } else {
                1.0 + node.members.len() as f64
            };
            let height = spacing.node_height + member_height;
            (width, height)
        })
        .collect()
}

// ── Cluster membership map ──────────────────────────────────────────

/// Build a mapping from node index to optional cluster index.
///
/// Used during crossing minimization to keep cluster members contiguous.
fn build_cluster_map(ir: &MermaidDiagramIr, n: usize) -> Vec<Option<usize>> {
    let mut map = vec![None; n];
    for (ci, cluster) in ir.clusters.iter().enumerate() {
        for member in &cluster.members {
            if member.0 < n {
                map[member.0] = Some(ci);
            }
        }
    }
    map
}

// ── Phase 1: Rank assignment ─────────────────────────────────────────

/// Assign ranks via longest-path layering (deterministic).
///
/// Nodes with no predecessors get rank 0. Each other node gets
/// 1 + max(rank of predecessors). This produces a valid layering
/// where all edges point from lower to higher ranks.
fn assign_ranks(graph: &LayoutGraph) -> Vec<usize> {
    let n = graph.n;
    if n == 0 {
        return vec![];
    }

    // Kahn's topological sort for determinism.
    let mut in_degree: Vec<usize> = graph.rev.iter().map(|preds| preds.len()).collect();

    // Seed queue with sources, sorted by node ID for determinism.
    let mut queue: Vec<usize> = (0..n).filter(|&v| in_degree[v] == 0).collect();
    queue.sort_by(|a, b| graph.node_ids[*a].cmp(&graph.node_ids[*b]));

    let mut ranks = vec![0usize; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);

    let mut head = 0;
    while head < queue.len() {
        let u = queue[head];
        head += 1;
        order.push(u);

        // Collect and sort successors for determinism.
        let mut successors: Vec<usize> = graph.adj[u].clone();
        successors.sort_by(|a, b| graph.node_ids[*a].cmp(&graph.node_ids[*b]));

        for v in successors {
            ranks[v] = ranks[v].max(ranks[u] + 1);
            in_degree[v] -= 1;
            if in_degree[v] == 0 {
                queue.push(v);
            }
        }
    }

    // Handle cycles: any unvisited node gets max_rank + 1.
    if order.len() < n {
        let max_rank = ranks.iter().copied().max().unwrap_or(0);
        let visited: std::collections::HashSet<usize> = order.iter().copied().collect();
        for (v, rank) in ranks.iter_mut().enumerate() {
            if !visited.contains(&v) {
                *rank = max_rank + 1;
            }
        }
    }

    // Reverse ranks for BT direction is handled at coordinate assignment.
    ranks
}

// ── Phase 2: Ordering within ranks ───────────────────────────────────

/// Build rank buckets: rank_order[r] = list of node indices at rank r.
fn build_rank_buckets(ranks: &[usize]) -> Vec<Vec<usize>> {
    if ranks.is_empty() {
        return vec![];
    }
    let max_rank = ranks.iter().copied().max().unwrap_or(0);
    let mut buckets = vec![vec![]; max_rank + 1];
    for (v, &r) in ranks.iter().enumerate() {
        buckets[r].push(v);
    }
    // Initial ordering within each rank: sort by node ID for determinism.
    buckets
}

/// Reuse a positions buffer instead of allocating a new Vec.
fn order_positions_into(order: &[usize], n: usize, positions: &mut Vec<usize>) {
    positions.clear();
    positions.resize(n, usize::MAX);
    for (pos, &node) in order.iter().enumerate() {
        if node < n {
            positions[node] = pos;
        }
    }
}

fn barycenter(prev_pos: &[usize], neighbors: &[usize]) -> f64 {
    if neighbors.is_empty() {
        return f64::MAX;
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    for &nb in neighbors {
        let pos = prev_pos.get(nb).copied().unwrap_or(usize::MAX);
        if pos != usize::MAX {
            sum += pos as f64;
            count += 1;
        }
    }
    if count == 0 {
        f64::MAX
    } else {
        sum / count as f64
    }
}

/// One pass of barycenter ordering: reorder rank `r` based on rank `r-1`.
///
/// When `cluster_map` is provided, cluster members are kept contiguous:
/// nodes are sorted by `(cluster_barycenter, barycenter, node_id)`.
fn barycenter_sweep_forward(
    rank_order: &mut [Vec<usize>],
    graph: &LayoutGraph,
    r: usize,
    cluster_map: &[Option<usize>],
    pos_buf: &mut Vec<usize>,
) {
    if r == 0 || r >= rank_order.len() {
        return;
    }
    order_positions_into(&rank_order[r - 1], graph.n, pos_buf);
    let prev_pos = &*pos_buf;

    let scored: Vec<(usize, f64)> = rank_order[r]
        .iter()
        .map(|&v| {
            let bc = barycenter(prev_pos, &graph.rev[v]);
            (v, bc)
        })
        .collect();

    let cluster_bary = cluster_barycenters(&scored, cluster_map);

    let mut sorted = scored;
    sorted.sort_by(|a, b| {
        cluster_sort_key(a.0, a.1, cluster_map, &cluster_bary)
            .partial_cmp(&cluster_sort_key(b.0, b.1, cluster_map, &cluster_bary))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| graph.node_ids[a.0].cmp(&graph.node_ids[b.0]))
    });

    rank_order[r] = sorted.into_iter().map(|(v, _)| v).collect();
}

/// One pass of barycenter ordering: reorder rank `r` based on rank `r+1`.
fn barycenter_sweep_backward(
    rank_order: &mut [Vec<usize>],
    graph: &LayoutGraph,
    r: usize,
    cluster_map: &[Option<usize>],
    pos_buf: &mut Vec<usize>,
) {
    if r + 1 >= rank_order.len() {
        return;
    }
    order_positions_into(&rank_order[r + 1], graph.n, pos_buf);
    let next_pos = &*pos_buf;

    let scored: Vec<(usize, f64)> = rank_order[r]
        .iter()
        .map(|&v| {
            let bc = barycenter(next_pos, &graph.adj[v]);
            (v, bc)
        })
        .collect();

    let cluster_bary = cluster_barycenters(&scored, cluster_map);

    let mut sorted = scored;
    sorted.sort_by(|a, b| {
        cluster_sort_key(a.0, a.1, cluster_map, &cluster_bary)
            .partial_cmp(&cluster_sort_key(b.0, b.1, cluster_map, &cluster_bary))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| graph.node_ids[a.0].cmp(&graph.node_ids[b.0]))
    });

    rank_order[r] = sorted.into_iter().map(|(v, _)| v).collect();
}

/// Compute barycenter for each cluster from its members' barycenters.
fn cluster_barycenters(
    scored: &[(usize, f64)],
    cluster_map: &[Option<usize>],
) -> std::collections::HashMap<usize, f64> {
    let mut sums: std::collections::HashMap<usize, (f64, usize)> = std::collections::HashMap::new();
    for &(v, bc) in scored {
        if let Some(Some(ci)) = cluster_map.get(v) {
            let entry = sums.entry(*ci).or_insert((0.0, 0));
            if bc < f64::MAX {
                entry.0 += bc;
                entry.1 += 1;
            }
        }
    }
    sums.into_iter()
        .map(|(ci, (sum, count))| {
            if count > 0 {
                (ci, sum / count as f64)
            } else {
                (ci, f64::MAX)
            }
        })
        .collect()
}

/// Composite sort key: (cluster_barycenter, cluster_tag, node_barycenter).
///
/// Nodes in the same cluster share the cluster barycenter and cluster index,
/// keeping them contiguous. Non-cluster nodes use `usize::MAX` as their
/// cluster tag so they never interleave with cluster members at the same
/// primary barycenter.
fn cluster_sort_key(
    node: usize,
    bc: f64,
    cluster_map: &[Option<usize>],
    cluster_bary: &std::collections::HashMap<usize, f64>,
) -> (f64, usize, f64) {
    match cluster_map.get(node).copied().flatten() {
        Some(ci) => {
            let cb = cluster_bary.get(&ci).copied().unwrap_or(f64::MAX);
            (cb, ci, bc)
        }
        None => (bc, usize::MAX, bc),
    }
}

/// Scratch buffers reused across crossing-count operations to avoid
/// per-call heap allocations on the hot path.
struct MermaidCrossingScratch {
    pos_b: Vec<usize>,
    in_b: Vec<bool>,
    edges: Vec<(usize, usize)>,
    fenwick_tree: Vec<usize>,
}

impl MermaidCrossingScratch {
    fn new(n: usize) -> Self {
        Self {
            pos_b: vec![usize::MAX; n],
            in_b: vec![false; n],
            edges: Vec::new(),
            fenwick_tree: Vec::new(),
        }
    }
}

/// Count edge crossings between two adjacent ranks.
#[cfg(test)]
fn count_crossings(rank_a: &[usize], rank_b: &[usize], graph: &LayoutGraph) -> usize {
    struct Fenwick {
        tree: Vec<usize>,
    }

    impl Fenwick {
        fn new(size: usize) -> Self {
            Self {
                tree: vec![0; size.saturating_add(1)],
            }
        }

        fn add(&mut self, idx: usize, value: usize) {
            let mut i = idx.saturating_add(1);
            while i < self.tree.len() {
                self.tree[i] = self.tree[i].saturating_add(value);
                i += i & i.wrapping_neg();
            }
        }

        /// Sum of counts in [0, idx).
        fn sum(&self, idx: usize) -> usize {
            let mut acc = 0usize;
            let mut i = idx.min(self.tree.len().saturating_sub(1));
            while i > 0 {
                acc = acc.saturating_add(self.tree[i]);
                i &= i - 1;
            }
            acc
        }
    }

    // Build position maps.
    let mut pos_b = vec![usize::MAX; graph.n];
    let mut in_b = vec![false; graph.n];
    for (i, &v) in rank_b.iter().enumerate() {
        pos_b[v] = i;
        in_b[v] = true;
    }

    // Collect all edges between rank_a and rank_b as (pos_a, pos_b) pairs.
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (i, &u) in rank_a.iter().enumerate() {
        for &v in &graph.adj[u] {
            if in_b[v] {
                edges.push((i, pos_b[v]));
            }
        }
    }

    if edges.len() < 2 {
        return 0;
    }

    // Count inversions using a Fenwick tree (O(E log V)).
    // We process edges grouped by pos_a so edges sharing the same
    // source do not contribute crossings.
    let mut crossings = 0usize;
    let mut bit = Fenwick::new(rank_b.len());
    let mut total_seen = 0usize;
    let mut idx = 0usize;
    while idx < edges.len() {
        let current_a = edges[idx].0;
        let mut end = idx + 1;
        while end < edges.len() && edges[end].0 == current_a {
            end += 1;
        }

        for &(_, b) in &edges[idx..end] {
            let prefix_inclusive = bit.sum(b.saturating_add(1));
            crossings = crossings.saturating_add(total_seen.saturating_sub(prefix_inclusive));
        }

        for &(_, b) in &edges[idx..end] {
            bit.add(b, 1);
            total_seen = total_seen.saturating_add(1);
        }

        idx = end;
    }
    crossings
}

/// Count edge crossings between two adjacent ranks, reusing scratch buffers.
fn count_crossings_reuse(
    rank_a: &[usize],
    rank_b: &[usize],
    graph: &LayoutGraph,
    scratch: &mut MermaidCrossingScratch,
) -> usize {
    let n = graph.n;
    if scratch.pos_b.len() < n {
        scratch.pos_b.resize(n, usize::MAX);
        scratch.in_b.resize(n, false);
    }
    for v in scratch.pos_b.iter_mut().take(n) {
        *v = usize::MAX;
    }
    for v in scratch.in_b.iter_mut().take(n) {
        *v = false;
    }
    for (i, &v) in rank_b.iter().enumerate() {
        scratch.pos_b[v] = i;
        scratch.in_b[v] = true;
    }

    scratch.edges.clear();
    for (i, &u) in rank_a.iter().enumerate() {
        for &v in &graph.adj[u] {
            if scratch.in_b[v] {
                scratch.edges.push((i, scratch.pos_b[v]));
            }
        }
    }

    if scratch.edges.len() < 2 {
        return 0;
    }

    let bit_size = rank_b.len().saturating_add(1);
    scratch.fenwick_tree.clear();
    scratch.fenwick_tree.resize(bit_size, 0);

    let mut crossings = 0usize;
    let mut total_seen = 0usize;
    let mut idx = 0usize;
    while idx < scratch.edges.len() {
        let current_a = scratch.edges[idx].0;
        let mut end = idx + 1;
        while end < scratch.edges.len() && scratch.edges[end].0 == current_a {
            end += 1;
        }

        for e_idx in idx..end {
            let b = scratch.edges[e_idx].1;
            let mut acc = 0usize;
            let mut fi = (b + 1).min(scratch.fenwick_tree.len().saturating_sub(1));
            while fi > 0 {
                acc = acc.saturating_add(scratch.fenwick_tree[fi]);
                fi &= fi - 1;
            }
            crossings = crossings.saturating_add(total_seen.saturating_sub(acc));
        }

        for e_idx in idx..end {
            let b = scratch.edges[e_idx].1;
            let mut fi = b.saturating_add(1);
            while fi < scratch.fenwick_tree.len() {
                scratch.fenwick_tree[fi] = scratch.fenwick_tree[fi].saturating_add(1);
                fi += fi & fi.wrapping_neg();
            }
            total_seen = total_seen.saturating_add(1);
        }

        idx = end;
    }
    crossings
}

/// Total crossings across all adjacent rank pairs.
#[cfg(test)]
fn total_crossings(rank_order: &[Vec<usize>], graph: &LayoutGraph) -> usize {
    let mut total = 0;
    for r in 0..rank_order.len().saturating_sub(1) {
        total += count_crossings(&rank_order[r], &rank_order[r + 1], graph);
    }
    total
}

/// Total crossings across all adjacent rank pairs, reusing scratch buffers.
fn total_crossings_reuse(
    rank_order: &[Vec<usize>],
    graph: &LayoutGraph,
    scratch: &mut MermaidCrossingScratch,
) -> usize {
    let mut total = 0;
    for r in 0..rank_order.len().saturating_sub(1) {
        total += count_crossings_reuse(&rank_order[r], &rank_order[r + 1], graph, scratch);
    }
    total
}

/// Total crossings with early-exit once `limit` is reached.
#[cfg(test)]
fn total_crossings_with_limit(
    rank_order: &[Vec<usize>],
    graph: &LayoutGraph,
    limit: usize,
) -> usize {
    let mut total = 0usize;
    for r in 0..rank_order.len().saturating_sub(1) {
        total = total.saturating_add(count_crossings(&rank_order[r], &rank_order[r + 1], graph));
        if total >= limit {
            break;
        }
    }
    total
}

/// Total crossings with early-exit, reusing scratch buffers.
fn total_crossings_with_limit_reuse(
    rank_order: &[Vec<usize>],
    graph: &LayoutGraph,
    limit: usize,
    scratch: &mut MermaidCrossingScratch,
) -> usize {
    let mut total = 0usize;
    for r in 0..rank_order.len().saturating_sub(1) {
        total = total.saturating_add(count_crossings_reuse(
            &rank_order[r],
            &rank_order[r + 1],
            graph,
            scratch,
        ));
        if total >= limit {
            break;
        }
    }
    total
}

/// Crossing minimization via iterated barycenter heuristic.
///
/// Alternates forward and backward sweeps, tracking the best ordering found.
/// Stops when budget is exhausted or no improvement is made.
fn minimize_crossings(
    rank_order: &mut Vec<Vec<usize>>,
    graph: &LayoutGraph,
    max_iterations: usize,
    cluster_map: &[Option<usize>],
) -> (usize, usize) {
    if rank_order.len() <= 1 {
        return (0, 0);
    }

    // Pre-allocate scratch buffers for crossing counts (hot path)
    let mut crossing_scratch = MermaidCrossingScratch::new(graph.n);

    let mut best_crossings = total_crossings_reuse(rank_order, graph, &mut crossing_scratch);
    let mut best_order = rank_order.clone();
    let mut iterations_used = 0;
    let mut pos_buf: Vec<usize> = Vec::with_capacity(graph.n);

    for _iter in 0..max_iterations {
        iterations_used += 1;

        // Forward sweep.
        for r in 1..rank_order.len() {
            barycenter_sweep_forward(rank_order, graph, r, cluster_map, &mut pos_buf);
        }

        // Backward sweep.
        for r in (0..rank_order.len().saturating_sub(1)).rev() {
            barycenter_sweep_backward(rank_order, graph, r, cluster_map, &mut pos_buf);
        }

        let crossings = total_crossings_with_limit_reuse(
            rank_order,
            graph,
            best_crossings,
            &mut crossing_scratch,
        );
        if crossings < best_crossings {
            best_crossings = crossings;
            best_order = rank_order.clone();
        } else {
            // No improvement; restore best and stop.
            *rank_order = best_order;
            break;
        }
    }

    (iterations_used, best_crossings)
}

// ── Phase 3: Coordinate assignment ───────────────────────────────────

/// Assign (x, y) coordinates to each node based on rank and order.
///
/// For TB/TD: rank → y, order → x.
/// For LR: rank → x, order → y.
/// For RL/BT: reversed accordingly.
fn assign_coordinates(
    rank_order: &[Vec<usize>],
    _ranks: &[usize],
    direction: GraphDirection,
    spacing: &LayoutSpacing,
    n: usize,
    node_sizes: &[(f64, f64)],
) -> Vec<LayoutRect> {
    let mut rects: Vec<LayoutRect> = (0..n)
        .map(|i| {
            let (w, h) = if i < node_sizes.len() {
                node_sizes[i]
            } else {
                (spacing.node_width, spacing.node_height)
            };
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: w,
                height: h,
            }
        })
        .collect();

    let num_ranks = rank_order.len();

    // Rank step uses the maximum node dimension in the rank direction.
    let max_rank_dim = match direction {
        GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => node_sizes
            .iter()
            .map(|s| s.1)
            .fold(spacing.node_height, f64::max),
        GraphDirection::LR | GraphDirection::RL => node_sizes
            .iter()
            .map(|s| s.0)
            .fold(spacing.node_width, f64::max),
    };
    let rank_step = max_rank_dim + spacing.rank_gap;

    // Place nodes within each rank using accumulated widths.
    for (r, rank_nodes) in rank_order.iter().enumerate() {
        let mut order_offset = 0.0;
        for &node in rank_nodes {
            if node >= n {
                continue;
            }

            let rank_coord = r as f64 * rank_step;
            let (x, y) = match direction {
                GraphDirection::TB | GraphDirection::TD => (order_offset, rank_coord),
                GraphDirection::BT => {
                    let reversed_rank = num_ranks.saturating_sub(1).saturating_sub(r);
                    (order_offset, reversed_rank as f64 * rank_step)
                }
                GraphDirection::LR => (rank_coord, order_offset),
                GraphDirection::RL => {
                    let reversed_rank = num_ranks.saturating_sub(1).saturating_sub(r);
                    (reversed_rank as f64 * rank_step, order_offset)
                }
            };

            rects[node].x = x;
            rects[node].y = y;

            // Advance by this node's span + gap.
            let node_span = match direction {
                GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => rects[node].width,
                GraphDirection::LR | GraphDirection::RL => rects[node].height,
            };
            order_offset += node_span + spacing.node_gap;
        }
    }

    // Center each rank relative to the widest rank.
    let rank_widths: Vec<f64> = rank_order
        .iter()
        .map(|nodes| {
            if nodes.is_empty() {
                return 0.0;
            }
            let total_span: f64 = nodes
                .iter()
                .filter(|&&v| v < n)
                .map(|&v| match direction {
                    GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => rects[v].width,
                    GraphDirection::LR | GraphDirection::RL => rects[v].height,
                })
                .sum();
            let gaps = (nodes.len().saturating_sub(1)) as f64 * spacing.node_gap;
            total_span + gaps
        })
        .collect();

    let max_width = rank_widths.iter().copied().fold(0.0_f64, f64::max);

    for (r, rank_nodes) in rank_order.iter().enumerate() {
        let shift = (max_width - rank_widths[r]) / 2.0;
        if shift > 0.0 {
            for &node in rank_nodes {
                if node < n {
                    match direction {
                        GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => {
                            rects[node].x += shift;
                        }
                        GraphDirection::LR | GraphDirection::RL => {
                            rects[node].y += shift;
                        }
                    }
                }
            }
        }
    }

    rects
}

// ── Phase 4: Cluster boundary computation ────────────────────────────

fn compute_cluster_bounds(
    ir: &MermaidDiagramIr,
    node_rects: &[LayoutRect],
    spacing: &LayoutSpacing,
) -> Vec<LayoutClusterBox> {
    ir.clusters
        .iter()
        .enumerate()
        .map(|(idx, cluster)| {
            let member_rects: Vec<&LayoutRect> = cluster
                .members
                .iter()
                .filter_map(|id| node_rects.get(id.0))
                .collect();

            let rect = if member_rects.is_empty() {
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: spacing.node_width + 2.0 * spacing.cluster_padding,
                    height: spacing.node_height + 2.0 * spacing.cluster_padding,
                }
            } else {
                let mut bounds = *member_rects[0];
                for &r in &member_rects[1..] {
                    bounds = bounds.union(r);
                }
                // Add padding around cluster.
                LayoutRect {
                    x: bounds.x - spacing.cluster_padding,
                    y: bounds.y - spacing.cluster_padding,
                    width: bounds.width + 2.0 * spacing.cluster_padding,
                    height: bounds.height + 2.0 * spacing.cluster_padding,
                }
            };

            let title_rect = cluster.title.map(|_| LayoutRect {
                x: rect.x + spacing.label_padding,
                y: rect.y + spacing.label_padding,
                width: rect.width - 2.0 * spacing.label_padding,
                height: spacing.node_height * 0.5,
            });

            LayoutClusterBox {
                cluster_idx: idx,
                rect,
                title_rect,
            }
        })
        .collect()
}

// ── Phase 5: Edge routing ────────────────────────────────────────────

/// Route edges as simple polylines between node centers.
///
/// For edges spanning multiple ranks, inserts intermediate waypoints at each
/// rank boundary with L-shaped bends when the source and target are offset.
/// For adjacent-rank edges, draws direct lines.
fn route_edges(
    ir: &MermaidDiagramIr,
    node_rects: &[LayoutRect],
    ranks: &[usize],
    rank_order: &[Vec<usize>],
    direction: GraphDirection,
    spacing: &LayoutSpacing,
) -> Vec<LayoutEdgePath> {
    ir.edges
        .iter()
        .enumerate()
        .map(|(idx, edge)| {
            let from_idx = endpoint_node_idx(ir, &edge.from);
            let to_idx = endpoint_node_idx(ir, &edge.to);

            let waypoints = match (from_idx, to_idx) {
                (Some(u), Some(v)) if u < node_rects.len() && v < node_rects.len() => {
                    let from_center = node_rects[u].center();
                    let to_center = node_rects[v].center();

                    let from_port =
                        edge_port(&node_rects[u], from_center, to_center, direction, true);
                    let to_port =
                        edge_port(&node_rects[v], to_center, from_center, direction, false);

                    let from_rank = if u < ranks.len() { ranks[u] } else { 0 };
                    let to_rank = if v < ranks.len() { ranks[v] } else { 0 };
                    let rank_span = from_rank.abs_diff(to_rank);

                    if rank_span <= 1 {
                        vec![from_port, to_port]
                    } else {
                        multi_rank_waypoints(
                            from_port, to_port, from_rank, to_rank, node_rects, rank_order,
                            direction, spacing,
                        )
                    }
                }
                _ => vec![],
            };

            LayoutEdgePath {
                edge_idx: idx,
                waypoints,
                bundle_count: 1,
                bundle_members: Vec::new(),
            }
        })
        .collect()
}

// ── Optional Edge Bundling (bd-70rmj) ───────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BundleEndpointKey {
    Node(usize),
    Cluster(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct EdgeBundleKey<'a> {
    from: BundleEndpointKey,
    to: BundleEndpointKey,
    arrow: &'a str,
    label: Option<usize>,
    style_sig: u64,
}

fn rect_for_bundle_endpoint<'a>(
    ep: BundleEndpointKey,
    node_rects: &'a [LayoutRect],
    clusters: &'a [LayoutClusterBox],
) -> Option<&'a LayoutRect> {
    match ep {
        BundleEndpointKey::Node(idx) => node_rects.get(idx),
        BundleEndpointKey::Cluster(idx) => clusters.get(idx).map(|c| &c.rect),
    }
}

fn apply_bundle_offset(waypoints: &mut [LayoutPoint], delta: f64, direction: GraphDirection) {
    match direction {
        GraphDirection::LR | GraphDirection::RL => {
            for wp in waypoints {
                wp.y += delta;
            }
        }
        GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => {
            for wp in waypoints {
                wp.x += delta;
            }
        }
    }
}

fn style_sig_for_edge(properties: &crate::mermaid::MermaidStyleProperties) -> u64 {
    use crate::mermaid::{MermaidColor, MermaidStrokeDash};

    const FNV1A_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV1A_PRIME: u64 = 0x100000001b3;

    fn hash_u32(hash: &mut u64, val: u32) {
        for byte in val.to_le_bytes() {
            *hash ^= u64::from(byte);
            *hash = hash.wrapping_mul(FNV1A_PRIME);
        }
    }

    fn color_sig(c: Option<MermaidColor>) -> u32 {
        match c {
            None => 0,
            Some(MermaidColor::Rgb(r, g, b)) => {
                0x01_00_00_00 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
            }
            Some(MermaidColor::Transparent) => 0x02_00_00_00,
            Some(MermaidColor::None) => 0x03_00_00_00,
        }
    }

    fn dash_sig(d: Option<MermaidStrokeDash>) -> u32 {
        match d {
            None => 0,
            Some(MermaidStrokeDash::Solid) => 1,
            Some(MermaidStrokeDash::Dashed) => 2,
            Some(MermaidStrokeDash::Dotted) => 3,
        }
    }

    let mut h = FNV1A_OFFSET;
    hash_u32(&mut h, color_sig(properties.stroke));
    hash_u32(&mut h, u32::from(properties.stroke_width.unwrap_or(0)));
    hash_u32(&mut h, dash_sig(properties.stroke_dash));
    h
}

#[allow(clippy::too_many_arguments)]
fn bundle_parallel_edges(
    ir: &MermaidDiagramIr,
    config: &MermaidConfig,
    spacing: &LayoutSpacing,
    node_rects: &[LayoutRect],
    clusters: &[LayoutClusterBox],
    cluster_map: &[Option<usize>],
    direction: GraphDirection,
    edges: &mut Vec<LayoutEdgePath>,
) {
    let min_bundle = config.edge_bundle_min_count.max(2);
    if edges.len() < 2 {
        return;
    }

    let style_sigs: Option<Vec<u64>> = if config.enable_styles {
        let resolved = crate::mermaid::resolve_styles(ir);
        Some(
            resolved
                .edge_styles
                .iter()
                .map(|s| style_sig_for_edge(&s.properties))
                .collect(),
        )
    } else {
        None
    };

    let mut groups: std::collections::HashMap<EdgeBundleKey<'_>, Vec<usize>> =
        std::collections::HashMap::new();

    for (path_idx, path) in edges.iter().enumerate() {
        if path.waypoints.len() < 2 {
            continue;
        }
        let Some(ir_edge) = ir.edges.get(path.edge_idx) else {
            continue;
        };
        let Some(from_node) = endpoint_node_idx(ir, &ir_edge.from) else {
            continue;
        };
        let Some(to_node) = endpoint_node_idx(ir, &ir_edge.to) else {
            continue;
        };
        if from_node >= cluster_map.len() || to_node >= cluster_map.len() {
            continue;
        }

        let from_cluster = cluster_map[from_node];
        let to_cluster = cluster_map[to_node];
        let crossing_boundary = from_cluster != to_cluster;

        let from_key = if crossing_boundary {
            from_cluster
                .map(BundleEndpointKey::Cluster)
                .unwrap_or(BundleEndpointKey::Node(from_node))
        } else {
            BundleEndpointKey::Node(from_node)
        };
        let to_key = if crossing_boundary {
            to_cluster
                .map(BundleEndpointKey::Cluster)
                .unwrap_or(BundleEndpointKey::Node(to_node))
        } else {
            BundleEndpointKey::Node(to_node)
        };

        let style_sig = style_sigs
            .as_ref()
            .and_then(|sigs| sigs.get(path.edge_idx).copied())
            .unwrap_or(0);

        let key = EdgeBundleKey {
            from: from_key,
            to: to_key,
            arrow: ir_edge.arrow.as_str(),
            label: ir_edge.label.map(|lid| lid.0),
            style_sig,
        };
        groups.entry(key).or_default().push(path_idx);
    }

    if groups.is_empty() {
        return;
    }

    let mut drop = vec![false; edges.len()];

    for (key, mut indices) in groups {
        if indices.len() < 2 {
            continue;
        }

        indices.sort_by_key(|&i| edges.get(i).map(|p| p.edge_idx).unwrap_or(usize::MAX));

        if indices.len() >= min_bundle {
            let canonical_i = indices[0];
            let members: Vec<usize> = indices
                .iter()
                .filter_map(|&i| edges.get(i).map(|p| p.edge_idx))
                .collect();

            if let Some(base) = edges.get_mut(canonical_i) {
                base.bundle_count = members.len().max(1);
                base.bundle_members = members;

                // For cluster-level bundles, snap endpoints to the cluster boundary
                // (using the same port selection logic as node routing).
                if base.waypoints.len() >= 2 {
                    let from_rect = rect_for_bundle_endpoint(key.from, node_rects, clusters);
                    let to_rect = rect_for_bundle_endpoint(key.to, node_rects, clusters);
                    if let (Some(from_rect), Some(to_rect)) = (from_rect, to_rect) {
                        let from_center = from_rect.center();
                        let to_center = to_rect.center();

                        if matches!(key.from, BundleEndpointKey::Cluster(_)) {
                            base.waypoints[0] =
                                edge_port(from_rect, from_center, to_center, direction, true);
                        }
                        if matches!(key.to, BundleEndpointKey::Cluster(_)) {
                            let last = base.waypoints.len().saturating_sub(1);
                            base.waypoints[last] =
                                edge_port(to_rect, to_center, from_center, direction, false);
                        }
                    }
                }
            }

            for &i in &indices[1..] {
                drop[i] = true;
            }
        } else if indices.len() == 2 && min_bundle > 2 {
            let a = indices[0];
            let b = indices[1];
            if a >= edges.len() || b >= edges.len() {
                continue;
            }

            let identical =
                edges[a].waypoints.len() >= 2 && edges[a].waypoints == edges[b].waypoints;
            if !identical {
                continue;
            }

            let delta = (spacing.node_gap * 0.4).clamp(0.6, 1.2);
            apply_bundle_offset(&mut edges[a].waypoints, -delta, direction);
            apply_bundle_offset(&mut edges[b].waypoints, delta, direction);
        }
    }

    if drop.iter().any(|d| *d) {
        let mut out = Vec::with_capacity(edges.len());
        for (i, edge) in edges.iter().enumerate() {
            if !drop[i] {
                out.push(edge.clone());
            }
        }
        *edges = out;
    }

    // Enforce invariants for downstream renderers/tests.
    for edge in edges {
        if edge.bundle_count <= 1 {
            edge.bundle_count = 1;
            edge.bundle_members.clear();
        } else if edge.bundle_members.is_empty() {
            edge.bundle_members.push(edge.edge_idx);
        }
    }
}

/// Generate waypoints for an edge spanning multiple ranks.
///
/// Inserts intermediate points at each rank boundary, interpolating the
/// cross-axis position linearly. This produces L-shaped bends when the
/// source and target are horizontally (or vertically) offset.
#[allow(clippy::too_many_arguments)]
fn multi_rank_waypoints(
    from_port: LayoutPoint,
    to_port: LayoutPoint,
    from_rank: usize,
    to_rank: usize,
    node_rects: &[LayoutRect],
    rank_order: &[Vec<usize>],
    direction: GraphDirection,
    _spacing: &LayoutSpacing,
) -> Vec<LayoutPoint> {
    let mut waypoints = vec![from_port];

    let (lo_rank, hi_rank) = if from_rank < to_rank {
        (from_rank, to_rank)
    } else {
        (to_rank, from_rank)
    };

    let total_steps = (hi_rank - lo_rank) as f64;

    // Visit intermediate ranks in edge direction (from_rank toward to_rank)
    // so that waypoints are emitted in traversal order.
    let mid_ranks: Vec<usize> = if from_rank <= to_rank {
        ((lo_rank + 1)..hi_rank).collect()
    } else {
        ((lo_rank + 1)..hi_rank).rev().collect()
    };

    for (step_idx, &mid_rank) in mid_ranks.iter().enumerate() {
        let t = (step_idx + 1) as f64 / total_steps;

        // Interpolate both axes; which is "cross" vs "rank" depends on direction.
        let interp_x = from_port.x + t * (to_port.x - from_port.x);
        let interp_y = from_port.y + t * (to_port.y - from_port.y);

        // For TB/TD/BT: rank axis = Y, cross axis = X.
        // For LR/RL:    rank axis = X, cross axis = Y.
        let (rank_fallback, cross_val) = match direction {
            GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => (interp_y, interp_x),
            GraphDirection::LR | GraphDirection::RL => (interp_x, interp_y),
        };

        // Find the rank-axis coordinate from existing nodes at this rank.
        let rank_coord = rank_order
            .get(mid_rank)
            .and_then(|nodes| {
                nodes.first().and_then(|&n| {
                    node_rects.get(n).map(|r| match direction {
                        GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => {
                            r.center().y
                        }
                        GraphDirection::LR | GraphDirection::RL => r.center().x,
                    })
                })
            })
            .unwrap_or(rank_fallback);

        let point = match direction {
            GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => LayoutPoint {
                x: cross_val,
                y: rank_coord,
            },
            GraphDirection::LR | GraphDirection::RL => LayoutPoint {
                x: rank_coord,
                y: cross_val,
            },
        };

        waypoints.push(point);
    }

    waypoints.push(to_port);
    waypoints
}

/// Compute the port point on a node boundary for an edge connection.
fn edge_port(
    rect: &LayoutRect,
    _self_center: LayoutPoint,
    _other_center: LayoutPoint,
    direction: GraphDirection,
    is_source: bool,
) -> LayoutPoint {
    let center = rect.center();
    match direction {
        GraphDirection::TB | GraphDirection::TD => {
            if is_source {
                LayoutPoint {
                    x: center.x,
                    y: rect.y + rect.height,
                }
            } else {
                LayoutPoint {
                    x: center.x,
                    y: rect.y,
                }
            }
        }
        GraphDirection::BT => {
            if is_source {
                LayoutPoint {
                    x: center.x,
                    y: rect.y,
                }
            } else {
                LayoutPoint {
                    x: center.x,
                    y: rect.y + rect.height,
                }
            }
        }
        GraphDirection::LR => {
            if is_source {
                LayoutPoint {
                    x: rect.x + rect.width,
                    y: center.y,
                }
            } else {
                LayoutPoint {
                    x: rect.x,
                    y: center.y,
                }
            }
        }
        GraphDirection::RL => {
            if is_source {
                LayoutPoint {
                    x: rect.x,
                    y: center.y,
                }
            } else {
                LayoutPoint {
                    x: rect.x + rect.width,
                    y: center.y,
                }
            }
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────

fn layout_gitgraph_diagram(
    ir: &MermaidDiagramIr,
    config: &MermaidConfig,
    spacing: &LayoutSpacing,
) -> DiagramLayout {
    let n = ir.nodes.len();
    if n == 0 {
        return DiagramLayout {
            nodes: vec![],
            clusters: vec![],
            edges: vec![],
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            stats: LayoutStats {
                iterations_used: 0,
                max_iterations: config.layout_iteration_budget,
                budget_exceeded: false,
                crossings: 0,
                ranks: 0,
                max_rank_width: 0,
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        };
    }

    // GitGraph layout: commits top-to-bottom, branches left-to-right.
    //
    // Each cluster represents a branch. Assign each branch a lane (x column).
    // Nodes (commits) not in any cluster go to lane 0 (main/default branch).
    // Commits are placed vertically in IR order.

    let node_sizes = compute_node_sizes(ir, spacing);
    let lane_width = spacing.node_gap + node_sizes.iter().map(|s| s.0).fold(0.0f64, f64::max);
    let row_height = spacing.rank_gap + node_sizes.iter().map(|s| s.1).fold(0.0f64, f64::max);

    // Build node-to-lane mapping via cluster membership.
    let mut node_lane: Vec<usize> = vec![0; n];
    for (cluster_idx, cluster) in ir.clusters.iter().enumerate() {
        let lane = cluster_idx + 1; // lane 0 = default (main), lane 1+ = branches
        for &member_id in &cluster.members {
            if member_id.0 < n {
                node_lane[member_id.0] = lane;
            }
        }
    }

    let num_lanes = ir.clusters.len() + 1;

    let mut nodes = Vec::with_capacity(n);
    for (i, (width, height)) in node_sizes.iter().copied().enumerate() {
        let lane = node_lane[i];
        let x = lane as f64 * lane_width;
        let y = i as f64 * row_height;
        let rect = LayoutRect {
            x,
            y,
            width,
            height,
        };
        let label_rect = ir.nodes[i].label.map(|_| LayoutRect {
            x: rect.x + spacing.label_padding,
            y: rect.y + spacing.label_padding,
            width: rect.width - 2.0 * spacing.label_padding,
            height: rect.height - 2.0 * spacing.label_padding,
        });
        nodes.push(LayoutNodeBox {
            node_idx: i,
            rect,
            label_rect,
            rank: i,
            order: lane,
        });
    }

    // Route edges between commits (merges cross lanes).
    let mut edges = Vec::with_capacity(ir.edges.len());
    for (idx, edge) in ir.edges.iter().enumerate() {
        let Some(from_idx) = endpoint_node_idx(ir, &edge.from) else {
            continue;
        };
        let Some(to_idx) = endpoint_node_idx(ir, &edge.to) else {
            continue;
        };
        if from_idx >= n || to_idx >= n {
            continue;
        }
        let from_rect = &nodes[from_idx].rect;
        let to_rect = &nodes[to_idx].rect;
        let x0 = from_rect.x + from_rect.width / 2.0;
        let y0 = from_rect.y + from_rect.height / 2.0;
        let x1 = to_rect.x + to_rect.width / 2.0;
        let y1 = to_rect.y + to_rect.height / 2.0;
        edges.push(LayoutEdgePath {
            edge_idx: idx,
            waypoints: vec![LayoutPoint { x: x0, y: y0 }, LayoutPoint { x: x1, y: y1 }],
            bundle_count: 1,
            bundle_members: Vec::new(),
        });
    }

    // Compute cluster boxes around branch lane columns.
    let mut clusters = Vec::with_capacity(ir.clusters.len());
    for (cluster_idx, cluster) in ir.clusters.iter().enumerate() {
        let lane = cluster_idx + 1;
        let x = lane as f64 * lane_width - spacing.label_padding;
        let cluster_members: Vec<usize> = cluster
            .members
            .iter()
            .map(|id| id.0)
            .filter(|&idx| idx < n)
            .collect();
        if cluster_members.is_empty() {
            clusters.push(LayoutClusterBox {
                cluster_idx,
                rect: LayoutRect {
                    x,
                    y: 0.0,
                    width: lane_width,
                    height: row_height,
                },
                title_rect: cluster.title.map(|_| LayoutRect {
                    x,
                    y: 0.0,
                    width: lane_width,
                    height: spacing.label_padding * 2.0,
                }),
            });
            continue;
        }
        let min_y = cluster_members
            .iter()
            .map(|&i| nodes[i].rect.y)
            .fold(f64::INFINITY, f64::min);
        let max_y = cluster_members
            .iter()
            .map(|&i| nodes[i].rect.y + nodes[i].rect.height)
            .fold(0.0f64, f64::max);
        let cluster_rect = LayoutRect {
            x,
            y: min_y - spacing.label_padding,
            width: lane_width,
            height: (max_y - min_y) + 2.0 * spacing.label_padding,
        };
        let title_rect = cluster.title.map(|_| LayoutRect {
            x: cluster_rect.x,
            y: cluster_rect.y,
            width: cluster_rect.width,
            height: spacing.label_padding * 2.0,
        });
        clusters.push(LayoutClusterBox {
            cluster_idx,
            rect: cluster_rect,
            title_rect,
        });
    }

    let bounding_box = compute_bounding_box(&nodes, &clusters, &edges);
    let pos_var = compute_position_variance(&nodes);
    DiagramLayout {
        nodes,
        clusters,
        edges,
        bounding_box,
        stats: LayoutStats {
            iterations_used: 0,
            max_iterations: config.layout_iteration_budget,
            budget_exceeded: false,
            crossings: 0,
            ranks: n,
            max_rank_width: num_lanes,
            total_bends: 0,
            position_variance: pos_var,
        },
        degradation: None,
    }
}

// ── requirementDiagram layout ────────────────────────────────────────

/// Layout a requirementDiagram with entity boxes.
///
/// Requirements and elements are arranged using the standard Sugiyama
/// algorithm but with wider entity boxes to accommodate multi-line
/// labels (<<kind>>\nname pattern).
fn layout_requirement_diagram(
    ir: &MermaidDiagramIr,
    config: &MermaidConfig,
    spacing: &LayoutSpacing,
) -> DiagramLayout {
    let n = ir.nodes.len();
    if n == 0 {
        return DiagramLayout {
            nodes: vec![],
            clusters: vec![],
            edges: vec![],
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            stats: LayoutStats {
                iterations_used: 0,
                max_iterations: config.layout_iteration_budget,
                budget_exceeded: false,
                crossings: 0,
                ranks: 0,
                max_rank_width: 0,
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        };
    }

    // Wider entity boxes for requirement labels.
    let entity_spacing = LayoutSpacing {
        node_width: spacing.node_width.max(16.0),
        node_height: spacing.node_height.max(4.0),
        rank_gap: spacing.rank_gap.max(5.0),
        node_gap: spacing.node_gap.max(4.0),
        ..*spacing
    };

    // Standard Sugiyama layout with wider spacing.
    let graph = LayoutGraph::from_ir(ir);
    let mut ranks = assign_ranks(&graph);

    // Apply constraints if present.
    let node_id_map: Option<std::collections::HashMap<&str, usize>> = if ir.constraints.is_empty() {
        None
    } else {
        Some(
            ir.nodes
                .iter()
                .enumerate()
                .map(|(i, n)| (n.id.as_str(), i))
                .collect(),
        )
    };
    if let Some(ref id_map) = node_id_map {
        apply_same_rank_constraints(&mut ranks, &ir.constraints, id_map);
        apply_min_length_constraints(&mut ranks, &ir.constraints, id_map);
    }

    let max_rank = ranks.iter().copied().max().unwrap_or(0);
    let _node_sizes = compute_node_sizes(ir, &entity_spacing);

    // Build rank buckets.
    let mut rank_buckets: Vec<Vec<usize>> = vec![vec![]; max_rank + 1];
    for (i, &r) in ranks.iter().enumerate() {
        rank_buckets[r].push(i);
    }
    for bucket in &mut rank_buckets {
        bucket.sort_unstable();
    }

    if let Some(ref id_map) = node_id_map {
        apply_order_constraints(&mut rank_buckets, &ir.constraints, id_map, &ranks);
    }

    let _cluster_map = build_cluster_map(ir, n);
    let mut node_rects: Vec<LayoutRect> = vec![
        LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        n
    ];
    let mut cursor_y = 0.0;
    let mut max_rank_width = 0;
    for bucket in &rank_buckets {
        if bucket.is_empty() {
            continue;
        }
        max_rank_width = max_rank_width.max(bucket.len());
        let mut cursor_x = 0.0;
        let row_height = entity_spacing.node_height;
        for &node_idx in bucket {
            node_rects[node_idx] = LayoutRect {
                x: cursor_x,
                y: cursor_y,
                width: entity_spacing.node_width,
                height: entity_spacing.node_height,
            };
            cursor_x += entity_spacing.node_width + entity_spacing.node_gap;
        }
        cursor_y += row_height + entity_spacing.rank_gap;
    }

    if let Some(ref id_map) = node_id_map {
        apply_pin_constraints(&mut node_rects, &ir.constraints, id_map);
    }

    let mut nodes: Vec<LayoutNodeBox> = Vec::with_capacity(n);
    for (i, rect) in node_rects.iter().enumerate() {
        let label_rect = ir.nodes[i].label.map(|_| LayoutRect {
            x: rect.x + entity_spacing.label_padding,
            y: rect.y + entity_spacing.label_padding,
            width: rect.width - 2.0 * entity_spacing.label_padding,
            height: rect.height - 2.0 * entity_spacing.label_padding,
        });
        nodes.push(LayoutNodeBox {
            node_idx: i,
            rect: *rect,
            label_rect,
            rank: ranks[i],
            order: rank_buckets[ranks[i]]
                .iter()
                .position(|&idx| idx == i)
                .unwrap_or(0),
        });
    }

    let mut edges: Vec<LayoutEdgePath> = Vec::with_capacity(ir.edges.len());
    for (idx, edge) in ir.edges.iter().enumerate() {
        let Some(from_idx) = endpoint_node_idx(ir, &edge.from) else {
            continue;
        };
        let Some(to_idx) = endpoint_node_idx(ir, &edge.to) else {
            continue;
        };
        let from_r = &node_rects[from_idx];
        let to_r = &node_rects[to_idx];
        let from_cx = from_r.x + from_r.width / 2.0;
        let from_by = from_r.y + from_r.height;
        let to_cx = to_r.x + to_r.width / 2.0;
        let to_ty = to_r.y;

        let waypoints = if (from_cx - to_cx).abs() < 0.1 {
            vec![
                LayoutPoint {
                    x: from_cx,
                    y: from_by,
                },
                LayoutPoint { x: to_cx, y: to_ty },
            ]
        } else {
            let mid_y = (from_by + to_ty) / 2.0;
            vec![
                LayoutPoint {
                    x: from_cx,
                    y: from_by,
                },
                LayoutPoint {
                    x: from_cx,
                    y: mid_y,
                },
                LayoutPoint { x: to_cx, y: mid_y },
                LayoutPoint { x: to_cx, y: to_ty },
            ]
        };

        edges.push(LayoutEdgePath {
            edge_idx: idx,
            waypoints,
            bundle_count: 1,
            bundle_members: Vec::new(),
        });
    }

    let total_bends: usize = edges
        .iter()
        .map(|e| e.waypoints.len().saturating_sub(2))
        .sum();
    let bounding_box = compute_bounding_box(&nodes, &[], &edges);
    let pos_var = compute_position_variance(&nodes);

    DiagramLayout {
        nodes,
        clusters: vec![],
        edges,
        bounding_box,
        stats: LayoutStats {
            iterations_used: 0,
            max_iterations: config.layout_iteration_budget,
            budget_exceeded: false,
            crossings: 0,
            ranks: max_rank + 1,
            max_rank_width,
            total_bends,
            position_variance: pos_var,
        },
        degradation: None,
    }
}

fn layout_journey_diagram(ir: &MermaidDiagramIr, config: &MermaidConfig, spacing: &LayoutSpacing) -> DiagramLayout {
    let n = ir.nodes.len();
    if n == 0 {
        return DiagramLayout {
            nodes: vec![], clusters: vec![], edges: vec![],
            bounding_box: LayoutRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 },
            stats: LayoutStats { iterations_used: 0, max_iterations: config.layout_iteration_budget, budget_exceeded: false, crossings: 0, ranks: 0, max_rank_width: 0, total_bends: 0, position_variance: 0.0 },
            degradation: None,
        };
    }
    let node_sizes = compute_node_sizes(ir, spacing);
    let task_height = spacing.node_height.max(3.0);
    let section_title_height = 2.0;
    let pad = spacing.cluster_padding;
    let max_nw = node_sizes.iter().map(|(w, _)| *w).fold(spacing.node_width, f64::max);
    let sec_w = max_nw + 2.0 * pad;
    let cmap = build_cluster_map(ir, n);
    let mut cn: Vec<Vec<usize>> = vec![Vec::new(); ir.clusters.len()];
    let mut uc: Vec<usize> = Vec::new();
    for (i, cm) in cmap.iter().enumerate().take(n) { if let Some(ci) = *cm { cn[ci].push(i); } else { uc.push(i); } }
    let mut nodes = vec![LayoutNodeBox { node_idx: 0, rect: LayoutRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }, label_rect: None, rank: 0, order: 0 }; n];
    let mut clusters = Vec::with_capacity(ir.clusters.len());
    let mut cy = 0.0;
    let mut ord = 0;
    let mut rnk = 0;
    for &ni in &uc {
        let nw = node_sizes[ni].0.max(max_nw);
        let r = LayoutRect { x: pad, y: cy, width: nw, height: task_height };
        let lr = Some(LayoutRect { x: r.x + spacing.label_padding, y: r.y + spacing.label_padding, width: r.width - 2.0 * spacing.label_padding, height: r.height - 2.0 * spacing.label_padding });
        nodes[ni] = LayoutNodeBox { node_idx: ni, rect: r, label_rect: lr, rank: rnk, order: ord };
        cy += task_height + spacing.node_gap; ord += 1;
    }
    for (ci, members) in cn.iter().enumerate() {
        if members.is_empty() {
            let h = section_title_height + 2.0 * pad;
            clusters.push(LayoutClusterBox { cluster_idx: ci, rect: LayoutRect { x: 0.0, y: cy, width: sec_w, height: h }, title_rect: Some(LayoutRect { x: pad, y: cy + pad * 0.5, width: sec_w - 2.0 * pad, height: section_title_height }) });
            cy += h + spacing.rank_gap; rnk += 1; continue;
        }
        let sy = cy;
        let ty = cy + section_title_height + pad;
        let mut ty2 = ty;
        for (lo, &ni) in members.iter().enumerate() {
            let nw = node_sizes[ni].0.max(max_nw);
            let r = LayoutRect { x: pad, y: ty2, width: nw, height: task_height };
            let lr = Some(LayoutRect { x: r.x + spacing.label_padding, y: r.y + spacing.label_padding, width: r.width - 2.0 * spacing.label_padding, height: r.height - 2.0 * spacing.label_padding });
            nodes[ni] = LayoutNodeBox { node_idx: ni, rect: r, label_rect: lr, rank: rnk, order: ord + lo };
            ty2 += task_height + spacing.node_gap;
        }
        ord += members.len();
        let sh = section_title_height + pad + (ty2 - ty - spacing.node_gap) + pad;
        clusters.push(LayoutClusterBox { cluster_idx: ci, rect: LayoutRect { x: 0.0, y: sy, width: sec_w, height: sh }, title_rect: Some(LayoutRect { x: pad, y: sy + pad * 0.5, width: sec_w - 2.0 * pad, height: section_title_height }) });
        cy = sy + sh + spacing.rank_gap; rnk += 1;
    }
    let th = if cy > spacing.rank_gap { cy - spacing.rank_gap } else { 0.0 };
    DiagramLayout { nodes, clusters, edges: vec![], bounding_box: LayoutRect { x: 0.0, y: 0.0, width: sec_w.max(0.0), height: th.max(0.0) }, stats: LayoutStats { iterations_used: 0, max_iterations: config.layout_iteration_budget, budget_exceeded: false, crossings: 0, ranks: rnk, max_rank_width: cn.iter().map(|m| m.len()).max().unwrap_or(0), total_bends: 0, position_variance: 0.0 }, degradation: None }
}

fn layout_timeline_diagram(ir: &MermaidDiagramIr, config: &MermaidConfig, spacing: &LayoutSpacing) -> DiagramLayout {
    let n = ir.nodes.len();
    if n == 0 {
        return DiagramLayout {
            nodes: vec![], clusters: vec![], edges: vec![],
            bounding_box: LayoutRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 },
            stats: LayoutStats { iterations_used: 0, max_iterations: config.layout_iteration_budget, budget_exceeded: false, crossings: 0, ranks: 0, max_rank_width: 0, total_bends: 0, position_variance: 0.0 },
            degradation: None,
        };
    }
    let node_sizes = compute_node_sizes(ir, spacing);
    let period_height = spacing.node_height.max(3.0);
    let section_title_height = 2.0;
    let pad = spacing.cluster_padding;
    let max_nw = node_sizes.iter().map(|(w, _)| *w).fold(spacing.node_width, f64::max);
    let sec_w = max_nw + 2.0 * pad;
    let cmap = build_cluster_map(ir, n);
    let mut cn: Vec<Vec<usize>> = vec![Vec::new(); ir.clusters.len()];
    let mut uc: Vec<usize> = Vec::new();
    for (i, cm) in cmap.iter().enumerate().take(n) { if let Some(ci) = *cm { cn[ci].push(i); } else { uc.push(i); } }
    let mut nodes = vec![LayoutNodeBox { node_idx: 0, rect: LayoutRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }, label_rect: None, rank: 0, order: 0 }; n];
    let mut clusters = Vec::with_capacity(ir.clusters.len());
    let mut cy = 0.0;
    let mut ord = 0;
    let mut rnk = 0;
    for &ni in &uc {
        let (nw, nh) = node_sizes[ni];
        let h = nh.max(period_height);
        let w = nw.max(max_nw);
        let r = LayoutRect { x: pad, y: cy, width: w, height: h };
        let lr = Some(LayoutRect { x: r.x + spacing.label_padding, y: r.y + spacing.label_padding, width: r.width - 2.0 * spacing.label_padding, height: r.height - 2.0 * spacing.label_padding });
        nodes[ni] = LayoutNodeBox { node_idx: ni, rect: r, label_rect: lr, rank: rnk, order: ord };
        cy += h + spacing.node_gap; ord += 1;
    }
    for (ci, members) in cn.iter().enumerate() {
        if members.is_empty() {
            let h = section_title_height + 2.0 * pad;
            clusters.push(LayoutClusterBox { cluster_idx: ci, rect: LayoutRect { x: 0.0, y: cy, width: sec_w, height: h }, title_rect: Some(LayoutRect { x: pad, y: cy + pad * 0.5, width: sec_w - 2.0 * pad, height: section_title_height }) });
            cy += h + spacing.rank_gap; rnk += 1; continue;
        }
        let sy = cy;
        let ty = cy + section_title_height + pad;
        let mut ty2 = ty;
        for (lo, &ni) in members.iter().enumerate() {
            let (nw, nh) = node_sizes[ni];
            let h = nh.max(period_height);
            let w = nw.max(max_nw);
            let r = LayoutRect { x: pad, y: ty2, width: w, height: h };
            let lr = Some(LayoutRect { x: r.x + spacing.label_padding, y: r.y + spacing.label_padding, width: r.width - 2.0 * spacing.label_padding, height: r.height - 2.0 * spacing.label_padding });
            nodes[ni] = LayoutNodeBox { node_idx: ni, rect: r, label_rect: lr, rank: rnk, order: ord + lo };
            ty2 += h + spacing.node_gap;
        }
        ord += members.len();
        let sh = section_title_height + pad + (ty2 - ty - spacing.node_gap) + pad;
        clusters.push(LayoutClusterBox { cluster_idx: ci, rect: LayoutRect { x: 0.0, y: sy, width: sec_w, height: sh }, title_rect: Some(LayoutRect { x: pad, y: sy + pad * 0.5, width: sec_w - 2.0 * pad, height: section_title_height }) });
        cy = sy + sh + spacing.rank_gap; rnk += 1;
    }
    let th = if cy > spacing.rank_gap { cy - spacing.rank_gap } else { 0.0 };
    DiagramLayout { nodes, clusters, edges: vec![], bounding_box: LayoutRect { x: 0.0, y: 0.0, width: sec_w.max(0.0), height: th.max(0.0) }, stats: LayoutStats { iterations_used: 0, max_iterations: config.layout_iteration_budget, budget_exceeded: false, crossings: 0, ranks: rnk, max_rank_width: cn.iter().map(|m| m.len()).max().unwrap_or(0), total_bends: 0, position_variance: 0.0 }, degradation: None }
}

fn layout_sequence_diagram(
    ir: &MermaidDiagramIr,
    config: &MermaidConfig,
    spacing: &LayoutSpacing,
) -> DiagramLayout {
    let n = ir.nodes.len();
    if n == 0 {
        return DiagramLayout {
            nodes: vec![],
            clusters: vec![],
            edges: vec![],
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            stats: LayoutStats {
                iterations_used: 0,
                max_iterations: config.layout_iteration_budget,
                budget_exceeded: false,
                crossings: 0,
                ranks: 0,
                max_rank_width: 0,
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        };
    }

    let node_sizes = compute_node_sizes(ir, spacing);
    let mut nodes = Vec::with_capacity(n);
    let mut cursor_x = 0.0;
    for (i, (width, height)) in node_sizes.iter().copied().enumerate() {
        let rect = LayoutRect {
            x: cursor_x,
            y: 0.0,
            width,
            height,
        };
        let label_rect = ir.nodes[i].label.map(|_| LayoutRect {
            x: rect.x + spacing.label_padding,
            y: rect.y + spacing.label_padding,
            width: rect.width - 2.0 * spacing.label_padding,
            height: rect.height - 2.0 * spacing.label_padding,
        });
        nodes.push(LayoutNodeBox {
            node_idx: i,
            rect,
            label_rect,
            rank: 0,
            order: i,
        });
        cursor_x += width + spacing.node_gap;
    }

    let message_gap = spacing.rank_gap.max(2.0);
    let actor_height = nodes.iter().map(|n| n.rect.height).fold(0.0f64, f64::max);
    let start_y = actor_height + spacing.rank_gap;
    let mut edges = Vec::with_capacity(ir.edges.len());
    for (idx, edge) in ir.edges.iter().enumerate() {
        let Some(from_idx) = endpoint_node_idx(ir, &edge.from) else {
            continue;
        };
        let Some(to_idx) = endpoint_node_idx(ir, &edge.to) else {
            continue;
        };
        if from_idx >= n || to_idx >= n {
            continue;
        }
        let from_rect = &nodes[from_idx].rect;
        let to_rect = &nodes[to_idx].rect;
        let y = start_y + idx as f64 * message_gap;
        let x0 = from_rect.x + from_rect.width / 2.0;
        let x1 = to_rect.x + to_rect.width / 2.0;
        edges.push(LayoutEdgePath {
            edge_idx: idx,
            waypoints: vec![LayoutPoint { x: x0, y }, LayoutPoint { x: x1, y }],
            bundle_count: 1,
            bundle_members: Vec::new(),
        });
    }

    let mut bounding_box = compute_bounding_box(&nodes, &[], &edges);
    let lifeline_end = if edges.is_empty() {
        start_y
    } else {
        start_y + (edges.len().saturating_sub(1) as f64) * message_gap + spacing.rank_gap
    };
    let bottom = bounding_box.y + bounding_box.height;
    if lifeline_end > bottom {
        bounding_box.height = lifeline_end - bounding_box.y;
    }

    let pos_var = compute_position_variance(&nodes);
    let layout = DiagramLayout {
        nodes,
        clusters: vec![],
        edges,
        bounding_box,
        stats: LayoutStats {
            iterations_used: 0,
            max_iterations: config.layout_iteration_budget,
            budget_exceeded: false,
            crossings: 0,
            ranks: 1,
            max_rank_width: n,
            total_bends: 0,
            position_variance: pos_var,
        },
        degradation: None,
    };

    let obj = evaluate_layout(&layout);
    emit_layout_metrics_jsonl(
        config,
        &layout,
        &obj,
        crate::mermaid::hash_ir(ir),
        ir.diagram_type,
    );

    layout
}

fn mindmap_subtree_height(
    idx: usize,
    children: &[Vec<usize>],
    node_sizes: &[(f64, f64)],
    vertical_gap: f64,
    memo: &mut [Option<f64>],
    visiting: &mut [bool],
) -> f64 {
    if let Some(value) = memo[idx] {
        return value;
    }
    if visiting[idx] {
        return node_sizes[idx].1;
    }
    visiting[idx] = true;
    let mut total = 0.0;
    for &child in &children[idx] {
        total += mindmap_subtree_height(child, children, node_sizes, vertical_gap, memo, visiting)
            + vertical_gap;
    }
    if !children[idx].is_empty() {
        total -= vertical_gap;
    }
    let height = node_sizes[idx].1.max(total);
    memo[idx] = Some(height);
    visiting[idx] = false;
    height
}

type MindmapChildEntry = (usize, f64);
type MindmapSplit = (Vec<MindmapChildEntry>, Vec<MindmapChildEntry>, f64, f64);

fn mindmap_split_root_children(
    child_entries: &[MindmapChildEntry],
    vertical_gap: f64,
) -> MindmapSplit {
    let mut left: Vec<MindmapChildEntry> = Vec::new();
    let mut right: Vec<MindmapChildEntry> = Vec::new();
    let mut left_total = 0.0;
    let mut right_total = 0.0;
    for &entry in child_entries {
        if right_total <= left_total {
            right_total += entry.1 + vertical_gap;
            right.push(entry);
        } else {
            left_total += entry.1 + vertical_gap;
            left.push(entry);
        }
    }
    let left_height = if left.is_empty() {
        0.0
    } else {
        (left_total - vertical_gap).max(0.0)
    };
    let right_height = if right.is_empty() {
        0.0
    } else {
        (right_total - vertical_gap).max(0.0)
    };
    (left, right, left_height, right_height)
}

#[allow(dead_code, clippy::too_many_arguments)]
fn mindmap_layout_side_children(
    entries: &[(usize, f64)],
    side_dir: f64,
    parent_idx: usize,
    parent_center: LayoutPoint,
    depth: usize,
    children: &[Vec<usize>],
    node_sizes: &[(f64, f64)],
    vertical_gap: f64,
    level_gap: f64,
    centers: &mut [LayoutPoint],
    depths: &mut [usize],
    placed: &mut [bool],
    memo: &mut [Option<f64>],
    visiting: &mut [bool],
) {
    if entries.is_empty() {
        return;
    }
    let total_height: f64 = entries.iter().map(|(_, h)| *h).sum::<f64>()
        + vertical_gap * (entries.len().saturating_sub(1) as f64);
    let mut cursor_y = parent_center.y - total_height / 2.0;
    for (child, height) in entries {
        let child_center = LayoutPoint {
            x: parent_center.x
                + side_dir
                    * (level_gap + node_sizes[parent_idx].0 / 2.0 + node_sizes[*child].0 / 2.0),
            y: cursor_y + height / 2.0,
        };
        if !placed[*child] {
            centers[*child] = child_center;
            depths[*child] = depth + 1;
            placed[*child] = true;
            mindmap_place_children(
                *child,
                child_center,
                side_dir,
                depth + 1,
                false,
                children,
                node_sizes,
                vertical_gap,
                level_gap,
                centers,
                depths,
                placed,
                memo,
                visiting,
            );
        }
        cursor_y += height + vertical_gap;
    }
}

#[allow(dead_code, clippy::too_many_arguments)]
fn mindmap_place_children(
    parent_idx: usize,
    parent_center: LayoutPoint,
    side: f64,
    depth: usize,
    is_root: bool,
    children: &[Vec<usize>],
    node_sizes: &[(f64, f64)],
    vertical_gap: f64,
    level_gap: f64,
    centers: &mut [LayoutPoint],
    depths: &mut [usize],
    placed: &mut [bool],
    memo: &mut [Option<f64>],
    visiting: &mut [bool],
) {
    let child_ids = &children[parent_idx];
    if child_ids.is_empty() {
        return;
    }
    let mut child_entries: Vec<(usize, f64)> = child_ids
        .iter()
        .map(|&child| {
            let height =
                mindmap_subtree_height(child, children, node_sizes, vertical_gap, memo, visiting);
            (child, height)
        })
        .collect();
    child_entries.sort_unstable_by_key(|(idx, _)| *idx);

    if is_root {
        let (left, right, _left_height, _right_height) =
            mindmap_split_root_children(&child_entries, vertical_gap);
        mindmap_layout_side_children(
            &right,
            1.0,
            parent_idx,
            parent_center,
            depth,
            children,
            node_sizes,
            vertical_gap,
            level_gap,
            centers,
            depths,
            placed,
            memo,
            visiting,
        );
        mindmap_layout_side_children(
            &left,
            -1.0,
            parent_idx,
            parent_center,
            depth,
            children,
            node_sizes,
            vertical_gap,
            level_gap,
            centers,
            depths,
            placed,
            memo,
            visiting,
        );
    } else {
        let side_dir = if side >= 0.0 { 1.0 } else { -1.0 };
        mindmap_layout_side_children(
            &child_entries,
            side_dir,
            parent_idx,
            parent_center,
            depth,
            children,
            node_sizes,
            vertical_gap,
            level_gap,
            centers,
            depths,
            placed,
            memo,
            visiting,
        );
    }
}

fn layout_mindmap_diagram(
    ir: &MermaidDiagramIr,
    config: &MermaidConfig,
    spacing: &LayoutSpacing,
) -> DiagramLayout {
    let n = ir.nodes.len();
    if n == 0 {
        return DiagramLayout {
            nodes: vec![],
            clusters: vec![],
            edges: vec![],
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            stats: LayoutStats {
                iterations_used: 0,
                max_iterations: config.layout_iteration_budget,
                budget_exceeded: false,
                crossings: 0,
                ranks: 0,
                max_rank_width: 0,
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        };
    }

    let node_sizes = compute_node_sizes(ir, spacing);
    let vertical_gap = spacing.node_gap.max(2.0);
    let level_gap = spacing.rank_gap.max(4.0);

    let mut children = vec![Vec::new(); n];
    let mut incoming = vec![0usize; n];
    for edge in &ir.edges {
        let Some(from_idx) = endpoint_node_idx(ir, &edge.from) else {
            continue;
        };
        let Some(to_idx) = endpoint_node_idx(ir, &edge.to) else {
            continue;
        };
        if from_idx >= n || to_idx >= n || from_idx == to_idx {
            continue;
        }
        children[from_idx].push(to_idx);
        incoming[to_idx] = incoming[to_idx].saturating_add(1);
    }
    for list in &mut children {
        list.sort_unstable();
        list.dedup();
    }

    let mut roots: Vec<usize> = (0..n).filter(|&idx| incoming[idx] == 0).collect();
    roots.sort_unstable();
    if roots.is_empty() {
        roots.push(0);
    }

    let mut memo = vec![None; n];
    let mut visiting = vec![false; n];
    let mut centers = vec![LayoutPoint { x: 0.0, y: 0.0 }; n];
    let mut depths = vec![0usize; n];
    let mut placed = vec![false; n];

    let root_gap = spacing.rank_gap.max(4.0);
    let mut cursor_y = 0.0;
    for root in roots.iter().copied() {
        let root_height = if children[root].is_empty() {
            node_sizes[root].1
        } else {
            let mut child_entries: Vec<(usize, f64)> = children[root]
                .iter()
                .map(|&child| {
                    let height = mindmap_subtree_height(
                        child,
                        &children,
                        &node_sizes,
                        vertical_gap,
                        &mut memo,
                        &mut visiting,
                    );
                    (child, height)
                })
                .collect();
            child_entries.sort_unstable_by_key(|(idx, _)| *idx);
            let (_left, _right, left_height, right_height) =
                mindmap_split_root_children(&child_entries, vertical_gap);
            node_sizes[root].1.max(left_height.max(right_height))
        };
        let center = LayoutPoint {
            x: 0.0,
            y: cursor_y + root_height / 2.0,
        };
        centers[root] = center;
        depths[root] = 0;
        placed[root] = true;
        mindmap_place_children(
            root,
            center,
            0.0,
            0,
            true,
            &children,
            &node_sizes,
            vertical_gap,
            level_gap,
            &mut centers,
            &mut depths,
            &mut placed,
            &mut memo,
            &mut visiting,
        );
        cursor_y += root_height + root_gap;
    }

    for idx in 0..n {
        if placed[idx] {
            continue;
        }
        let height = mindmap_subtree_height(
            idx,
            &children,
            &node_sizes,
            vertical_gap,
            &mut memo,
            &mut visiting,
        );
        let center = LayoutPoint {
            x: 0.0,
            y: cursor_y + height / 2.0,
        };
        centers[idx] = center;
        depths[idx] = 0;
        placed[idx] = true;
        mindmap_place_children(
            idx,
            center,
            0.0,
            0,
            true,
            &children,
            &node_sizes,
            vertical_gap,
            level_gap,
            &mut centers,
            &mut depths,
            &mut placed,
            &mut memo,
            &mut visiting,
        );
        cursor_y += height + root_gap;
    }

    let mut max_depth = 0usize;
    for &depth in &depths {
        max_depth = max_depth.max(depth);
    }

    let mut depth_buckets: Vec<Vec<usize>> = vec![Vec::new(); max_depth + 1];
    for (idx, &depth) in depths.iter().enumerate() {
        depth_buckets[depth].push(idx);
    }

    let mut order_map = vec![0usize; n];
    let mut max_rank_width = 0usize;
    for bucket in &mut depth_buckets {
        bucket.sort_by(|a, b| centers[*a].y.total_cmp(&centers[*b].y));
        max_rank_width = max_rank_width.max(bucket.len());
        for (order, &node_idx) in bucket.iter().enumerate() {
            order_map[node_idx] = order;
        }
    }

    let nodes: Vec<LayoutNodeBox> = (0..n)
        .map(|i| {
            let (width, height) = node_sizes[i];
            let center = centers[i];
            let rect = LayoutRect {
                x: center.x - width / 2.0,
                y: center.y - height / 2.0,
                width,
                height,
            };
            let label_rect = ir.nodes[i].label.map(|_| LayoutRect {
                x: rect.x + spacing.label_padding,
                y: rect.y + spacing.label_padding,
                width: rect.width - 2.0 * spacing.label_padding,
                height: rect.height - 2.0 * spacing.label_padding,
            });
            LayoutNodeBox {
                node_idx: i,
                rect,
                label_rect,
                rank: depths[i],
                order: order_map[i],
            }
        })
        .collect();

    let mut edges = Vec::with_capacity(ir.edges.len());
    let mut total_bends = 0usize;
    for (edge_idx, edge) in ir.edges.iter().enumerate() {
        let Some(from_idx) = endpoint_node_idx(ir, &edge.from) else {
            continue;
        };
        let Some(to_idx) = endpoint_node_idx(ir, &edge.to) else {
            continue;
        };
        if from_idx >= n || to_idx >= n {
            continue;
        }
        let from_rect = &nodes[from_idx].rect;
        let to_rect = &nodes[to_idx].rect;
        let from_center = centers[from_idx];
        let to_center = centers[to_idx];
        let to_right = to_center.x >= from_center.x;
        let start_x = if to_right {
            from_rect.x + from_rect.width
        } else {
            from_rect.x
        };
        let end_x = if to_right {
            to_rect.x
        } else {
            to_rect.x + to_rect.width
        };
        let start = LayoutPoint {
            x: start_x,
            y: from_center.y,
        };
        let end = LayoutPoint {
            x: end_x,
            y: to_center.y,
        };
        let mid_x = (start.x + end.x) / 2.0;
        let waypoints = vec![
            start,
            LayoutPoint {
                x: mid_x,
                y: start.y,
            },
            LayoutPoint { x: mid_x, y: end.y },
            end,
        ];
        total_bends += waypoints.len().saturating_sub(2);
        edges.push(LayoutEdgePath {
            edge_idx,
            waypoints,
            bundle_count: 1,
            bundle_members: Vec::new(),
        });
    }

    let bounding_box = compute_bounding_box(&nodes, &[], &edges);
    let pos_var = compute_position_variance(&nodes);
    let layout = DiagramLayout {
        nodes,
        clusters: vec![],
        edges,
        bounding_box,
        stats: LayoutStats {
            iterations_used: 0,
            max_iterations: config.layout_iteration_budget,
            budget_exceeded: false,
            crossings: 0,
            ranks: max_depth + 1,
            max_rank_width,
            total_bends,
            position_variance: pos_var,
        },
        degradation: None,
    };

    let obj = evaluate_layout(&layout);
    emit_layout_metrics_jsonl(
        config,
        &layout,
        &obj,
        crate::mermaid::hash_ir(ir),
        ir.diagram_type,
    );

    layout
}

/// Compute a deterministic layout for a Mermaid diagram IR.
///
/// Returns a `DiagramLayout` with positioned nodes, clusters, and edges.
/// Respects the `layout_iteration_budget` from config. If the budget is
/// exceeded, produces a degraded layout and sets the degradation plan.
#[must_use]
pub fn layout_diagram(ir: &MermaidDiagramIr, config: &MermaidConfig) -> DiagramLayout {
    layout_diagram_with_spacing(ir, config, &LayoutSpacing::default())
}

// ── Constraint application ──────────────────────────────────────────

/// Apply same-rank constraints: merge ranks so constrained nodes share the lowest rank.
fn apply_same_rank_constraints(
    ranks: &mut [usize],
    constraints: &[LayoutConstraint],
    node_id_map: &std::collections::HashMap<&str, usize>,
) {
    for constraint in constraints {
        if let LayoutConstraint::SameRank { node_ids, .. } = constraint {
            let indices: Vec<usize> = node_ids
                .iter()
                .filter_map(|id| node_id_map.get(id.as_str()).copied())
                .collect();
            if indices.len() < 2 {
                continue;
            }
            let min_rank = indices.iter().map(|&i| ranks[i]).min().unwrap_or(0);
            for &idx in &indices {
                ranks[idx] = min_rank;
            }
        }
    }
}

/// Apply min-length constraints: ensure rank(to) - rank(from) >= min_len.
fn apply_min_length_constraints(
    ranks: &mut [usize],
    constraints: &[LayoutConstraint],
    node_id_map: &std::collections::HashMap<&str, usize>,
) {
    for constraint in constraints {
        if let LayoutConstraint::MinLength {
            from_id,
            to_id,
            min_len,
            ..
        } = constraint
        {
            let from_idx = node_id_map.get(from_id.as_str()).copied();
            let to_idx = node_id_map.get(to_id.as_str()).copied();
            if let (Some(fi), Some(ti)) = (from_idx, to_idx) {
                let current_span = ranks[ti].saturating_sub(ranks[fi]);
                if current_span < *min_len {
                    ranks[ti] = ranks[fi] + *min_len;
                }
            }
        }
    }
}

/// Apply order-in-rank constraints: force left-to-right ordering within each rank.
fn apply_order_constraints(
    rank_order: &mut [Vec<usize>],
    constraints: &[LayoutConstraint],
    node_id_map: &std::collections::HashMap<&str, usize>,
    ranks: &[usize],
) {
    for constraint in constraints {
        if let LayoutConstraint::OrderInRank { node_ids, .. } = constraint {
            let indices: Vec<usize> = node_ids
                .iter()
                .filter_map(|id| node_id_map.get(id.as_str()).copied())
                .collect();
            if indices.len() < 2 {
                continue;
            }
            // Group by rank
            let mut by_rank: std::collections::HashMap<usize, Vec<usize>> =
                std::collections::HashMap::new();
            for &idx in &indices {
                by_rank.entry(ranks[idx]).or_default().push(idx);
            }
            // For each rank containing constrained nodes, enforce ordering
            for (&rank, ordered) in &by_rank {
                if rank >= rank_order.len() || ordered.len() < 2 {
                    continue;
                }
                let bucket = &mut rank_order[rank];
                // Build O(1) membership set for constrained nodes
                let max_node = ordered.iter().copied().max().unwrap_or(0);
                let mut is_ordered = vec![false; max_node + 1];
                for &idx in ordered {
                    is_ordered[idx] = true;
                }
                // Remove constrained nodes from their current positions
                let mut other: Vec<usize> = bucket
                    .iter()
                    .copied()
                    .filter(|&n| n >= is_ordered.len() || !is_ordered[n])
                    .collect();
                // Find insertion point: where the first constrained node was
                let first_pos = bucket
                    .iter()
                    .position(|&n| n < is_ordered.len() && is_ordered[n])
                    .unwrap_or(0);
                let insert_at = first_pos.min(other.len());
                // Insert constrained nodes in specified order at that position
                for (i, &node) in ordered.iter().enumerate() {
                    other.insert(insert_at + i, node);
                }
                *bucket = other;
            }
        }
    }
}

/// Apply pin constraints: override node positions to pinned coordinates.
fn apply_pin_constraints(
    node_rects: &mut [LayoutRect],
    constraints: &[LayoutConstraint],
    node_id_map: &std::collections::HashMap<&str, usize>,
) {
    for constraint in constraints {
        if let LayoutConstraint::Pin { node_id, x, y, .. } = constraint
            && let Some(&idx) = node_id_map.get(node_id.as_str())
            && idx < node_rects.len()
        {
            node_rects[idx].x = *x;
            node_rects[idx].y = *y;
        }
    }
}

pub fn layout_diagram_with_spacing(
    ir: &MermaidDiagramIr,
    config: &MermaidConfig,
    spacing: &LayoutSpacing,
) -> DiagramLayout {
    if ir.diagram_type == DiagramType::GitGraph {
        return layout_gitgraph_diagram(ir, config, spacing);
    }
    if ir.diagram_type == DiagramType::Requirement {
        return layout_requirement_diagram(ir, config, spacing);
    }
    if ir.diagram_type == DiagramType::Mindmap {
        return layout_mindmap_diagram(ir, config, spacing);
    }
    if ir.diagram_type == DiagramType::Sequence {
        return layout_sequence_diagram(ir, config, spacing);
    }
    if ir.diagram_type == DiagramType::Journey {
        return layout_journey_diagram(ir, config, spacing);
    }
    if ir.diagram_type == DiagramType::Timeline {
        return layout_timeline_diagram(ir, config, spacing);
    }

    let n = ir.nodes.len();

    // Empty diagram shortcut.
    if n == 0 {
        return DiagramLayout {
            nodes: vec![],
            clusters: vec![],
            edges: vec![],
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            stats: LayoutStats {
                iterations_used: 0,
                max_iterations: config.layout_iteration_budget,
                budget_exceeded: false,
                crossings: 0,
                ranks: 0,
                max_rank_width: 0,
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        };
    }

    // Build internal graph.
    let graph = LayoutGraph::from_ir(ir);

    // Phase 1: Rank assignment.
    let mut ranks = assign_ranks(&graph);

    // Phase 1b: Apply rank constraints (same-rank, min-length).
    let node_id_map: Option<std::collections::HashMap<&str, usize>> = if ir.constraints.is_empty() {
        None
    } else {
        Some(
            ir.nodes
                .iter()
                .enumerate()
                .map(|(i, node)| (node.id.as_str(), i))
                .collect(),
        )
    };
    if let Some(node_id_map) = node_id_map.as_ref() {
        apply_same_rank_constraints(&mut ranks, &ir.constraints, node_id_map);
        apply_min_length_constraints(&mut ranks, &ir.constraints, node_id_map);
    }

    // Phase 2: Build rank buckets and minimize crossings.
    let mut rank_order = build_rank_buckets(&ranks);

    // Sort initial order within each rank by node ID for determinism.
    for bucket in &mut rank_order {
        bucket.sort_by(|a, b| graph.node_ids[*a].cmp(&graph.node_ids[*b]));
    }

    let max_iterations = config.layout_iteration_budget;
    let cluster_map = build_cluster_map(ir, n);
    let (iterations_used, crossings) =
        minimize_crossings(&mut rank_order, &graph, max_iterations, &cluster_map);
    let budget_exceeded = iterations_used >= max_iterations;

    // Phase 2b: Apply order-in-rank constraints (after crossing minimization).
    if let Some(node_id_map) = node_id_map.as_ref() {
        apply_order_constraints(&mut rank_order, &ir.constraints, node_id_map, &ranks);
    }

    // Phase 3: Coordinate assignment (content-aware sizing).
    let node_sizes = compute_node_sizes(ir, spacing);
    let mut node_rects =
        assign_coordinates(&rank_order, &ranks, ir.direction, spacing, n, &node_sizes);

    // Phase 3b: Constraint-based compaction (3 passes max).
    compact_positions(
        &mut node_rects,
        &rank_order,
        &graph,
        spacing,
        ir.direction,
        3,
    );

    // Phase 3c: Apply pin constraints (override positions).
    if let Some(node_id_map) = node_id_map.as_ref() {
        apply_pin_constraints(&mut node_rects, &ir.constraints, node_id_map);
    }

    // Precompute per-node order within its rank to avoid repeated scans.
    let mut order_map = vec![0usize; n];
    for bucket in &rank_order {
        for (order, &node_idx) in bucket.iter().enumerate() {
            if node_idx < n {
                order_map[node_idx] = order;
            }
        }
    }

    // Build LayoutNodeBox list.
    let nodes: Vec<LayoutNodeBox> = (0..n)
        .map(|i| {
            let rank = ranks[i];
            let order = order_map[i];

            let label_rect = ir.nodes[i].label.map(|_| {
                let r = &node_rects[i];
                LayoutRect {
                    x: r.x + spacing.label_padding,
                    y: r.y + spacing.label_padding,
                    width: r.width - 2.0 * spacing.label_padding,
                    height: r.height - 2.0 * spacing.label_padding,
                }
            });

            LayoutNodeBox {
                node_idx: i,
                rect: node_rects[i],
                label_rect,
                rank,
                order,
            }
        })
        .collect();

    // Phase 4: Cluster bounds.
    let clusters = compute_cluster_bounds(ir, &node_rects, spacing);

    // Phase 5: Edge routing.
    let mut edges = route_edges(ir, &node_rects, &ranks, &rank_order, ir.direction, spacing);
    if config.edge_bundling && ir.diagram_type == DiagramType::Graph && edges.len() >= 2 {
        bundle_parallel_edges(
            ir,
            config,
            spacing,
            &node_rects,
            &clusters,
            &cluster_map,
            ir.direction,
            &mut edges,
        );
    }

    // Compute bounding box (includes edge waypoints).
    let bounding_box = compute_bounding_box(&nodes, &clusters, &edges);

    // Degradation plan if budget was exceeded.
    let degradation = if budget_exceeded {
        Some(MermaidDegradationPlan {
            target_fidelity: MermaidFidelity::Normal,
            hide_labels: false,
            collapse_clusters: false,
            simplify_routing: true,
            reduce_decoration: false,
            force_glyph_mode: None,
        })
    } else {
        None
    };

    let max_rank_width = rank_order.iter().map(Vec::len).max().unwrap_or(0);

    // Compute expanded stats.
    let total_bends: usize = edges
        .iter()
        .map(|e| e.waypoints.len().saturating_sub(2))
        .sum();
    let pos_var = compute_position_variance(&nodes);

    let layout = DiagramLayout {
        nodes,
        clusters,
        edges,
        bounding_box,
        stats: LayoutStats {
            iterations_used,
            max_iterations,
            budget_exceeded,
            crossings,
            ranks: rank_order.len(),
            max_rank_width,
            total_bends,
            position_variance: pos_var,
        },
        degradation,
    };

    // Emit aesthetic metrics to evidence log (bd-19cll).
    let obj = evaluate_layout(&layout);
    emit_layout_metrics_jsonl(
        config,
        &layout,
        &obj,
        crate::mermaid::hash_ir(ir),
        ir.diagram_type,
    );

    layout
}

fn compute_bounding_box(
    nodes: &[LayoutNodeBox],
    clusters: &[LayoutClusterBox],
    edges: &[LayoutEdgePath],
) -> LayoutRect {
    let mut rects = nodes
        .iter()
        .map(|n| &n.rect)
        .chain(clusters.iter().map(|c| &c.rect));
    let Some(first) = rects.next() else {
        return LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
    };

    let mut bounds = *first;
    for r in rects {
        bounds = bounds.union(r);
    }

    // Expand to include edge waypoints.
    for edge in edges {
        for wp in &edge.waypoints {
            if wp.x < bounds.x {
                let delta = bounds.x - wp.x;
                bounds.x = wp.x;
                bounds.width += delta;
            } else if wp.x > bounds.x + bounds.width {
                bounds.width = wp.x - bounds.x;
            }
            if wp.y < bounds.y {
                let delta = bounds.y - wp.y;
                bounds.y = wp.y;
                bounds.height += delta;
            } else if wp.y > bounds.y + bounds.height {
                bounds.height = wp.y - bounds.y;
            }
        }
    }
    bounds
}

// ── Objective scoring ────────────────────────────────────────────────

/// Layout quality metrics for tie-breaking and comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutObjective {
    /// Number of edge crossings (lower is better).
    pub crossings: usize,
    /// Number of edge bends (non-straight segments; lower is better).
    pub bends: usize,
    /// Variance of node positions within each rank (lower = more symmetric).
    pub position_variance: f64,
    /// Sum of edge lengths in world units (lower = more compact).
    pub total_edge_length: f64,
    /// Count of nodes aligned with their rank median (higher is better).
    pub aligned_nodes: usize,
    /// Symmetry: balance across the center axis (0.0–1.0, higher is better).
    pub symmetry: f64,
    /// Compactness: node area / bounding box area (0.0–1.0, higher is better).
    pub compactness: f64,
    /// Edge length variance: std dev of individual edge lengths (lower = more uniform).
    pub edge_length_variance: f64,
    /// Label collision penalty count (lower is better).
    pub label_collisions: usize,
    /// Composite score (lower is better).
    pub score: f64,
}

// ── Aesthetic weight presets (bd-19cll) ──────────────────────────────

/// Tunable weights for layout aesthetic scoring.
///
/// Lower composite scores are better.  Negative weights reward higher
/// values (e.g. `alignment`, `symmetry`, `compactness`).
#[derive(Debug, Clone)]
pub struct AestheticWeights {
    pub crossings: f64,
    pub bends: f64,
    pub variance: f64,
    pub edge_length: f64,
    pub alignment: f64,
    pub symmetry: f64,
    pub compactness: f64,
    pub edge_length_variance: f64,
    pub label_collisions: f64,
}

impl AestheticWeights {
    /// Balanced weights — good for medium-size diagrams.
    #[must_use]
    pub fn normal() -> Self {
        Self {
            crossings: 10.0,
            bends: 2.0,
            variance: 1.0,
            edge_length: 0.5,
            alignment: -1.0,
            symmetry: -3.0,
            compactness: -2.0,
            edge_length_variance: 1.0,
            label_collisions: 8.0,
        }
    }

    /// Compact preset — optimises for small screens.
    #[must_use]
    pub fn compact() -> Self {
        Self {
            crossings: 8.0,
            bends: 1.0,
            variance: 0.5,
            edge_length: 2.0,
            alignment: -0.5,
            symmetry: -1.0,
            compactness: -5.0,
            edge_length_variance: 0.5,
            label_collisions: 6.0,
        }
    }

    /// Rich preset — optimises for large screens where aesthetics dominate.
    #[must_use]
    pub fn rich() -> Self {
        Self {
            crossings: 15.0,
            bends: 3.0,
            variance: 2.0,
            edge_length: 0.2,
            alignment: -2.0,
            symmetry: -5.0,
            compactness: -0.5,
            edge_length_variance: 2.0,
            label_collisions: 10.0,
        }
    }
}

impl Default for AestheticWeights {
    fn default() -> Self {
        Self::normal()
    }
}

impl LayoutObjective {
    fn compute_score(&self) -> f64 {
        self.compute_score_with(&AestheticWeights::normal())
    }

    /// Compute the composite score using the given weight preset.
    #[must_use]
    pub fn compute_score_with(&self, w: &AestheticWeights) -> f64 {
        self.crossings as f64 * w.crossings
            + self.bends as f64 * w.bends
            + self.position_variance * w.variance
            + self.total_edge_length * w.edge_length
            + self.aligned_nodes as f64 * w.alignment
            + self.symmetry * w.symmetry
            + self.compactness * w.compactness
            + self.edge_length_variance * w.edge_length_variance
            + self.label_collisions as f64 * w.label_collisions
    }
}

// ── Layout comparison harness (bd-19cll) ────────────────────────────

/// Result of comparing two layouts side-by-side.
#[derive(Debug, Clone)]
pub struct LayoutComparison {
    pub score_a: f64,
    pub score_b: f64,
    /// Positive ⇒ B is better (lower score); negative ⇒ A is better.
    pub delta: f64,
    /// Per-metric breakdown: (name, a_value, b_value, weighted_delta).
    pub breakdown: Vec<(&'static str, f64, f64, f64)>,
}

/// Compare two layouts and return a detailed per-metric breakdown.
#[must_use]
pub fn compare_layouts(
    a: &LayoutObjective,
    b: &LayoutObjective,
    weights: &AestheticWeights,
) -> LayoutComparison {
    let sa = a.compute_score_with(weights);
    let sb = b.compute_score_with(weights);
    let bd = vec![
        (
            "crossings",
            a.crossings as f64,
            b.crossings as f64,
            (b.crossings as f64 - a.crossings as f64) * weights.crossings,
        ),
        (
            "bends",
            a.bends as f64,
            b.bends as f64,
            (b.bends as f64 - a.bends as f64) * weights.bends,
        ),
        (
            "variance",
            a.position_variance,
            b.position_variance,
            (b.position_variance - a.position_variance) * weights.variance,
        ),
        (
            "edge_length",
            a.total_edge_length,
            b.total_edge_length,
            (b.total_edge_length - a.total_edge_length) * weights.edge_length,
        ),
        (
            "alignment",
            a.aligned_nodes as f64,
            b.aligned_nodes as f64,
            (b.aligned_nodes as f64 - a.aligned_nodes as f64) * weights.alignment,
        ),
        (
            "symmetry",
            a.symmetry,
            b.symmetry,
            (b.symmetry - a.symmetry) * weights.symmetry,
        ),
        (
            "compactness",
            a.compactness,
            b.compactness,
            (b.compactness - a.compactness) * weights.compactness,
        ),
        (
            "edge_length_variance",
            a.edge_length_variance,
            b.edge_length_variance,
            (b.edge_length_variance - a.edge_length_variance) * weights.edge_length_variance,
        ),
        (
            "label_collisions",
            a.label_collisions as f64,
            b.label_collisions as f64,
            (b.label_collisions as f64 - a.label_collisions as f64) * weights.label_collisions,
        ),
    ];
    LayoutComparison {
        score_a: sa,
        score_b: sb,
        delta: sa - sb,
        breakdown: bd,
    }
}

/// Evaluate layout quality for the given diagram layout.
#[must_use]
pub fn evaluate_layout(layout: &DiagramLayout) -> LayoutObjective {
    let crossings = layout.stats.crossings;

    let bends: usize = layout
        .edges
        .iter()
        .map(|e| e.waypoints.len().saturating_sub(2))
        .sum();

    let position_variance = compute_position_variance(&layout.nodes);
    let total_edge_length = compute_total_edge_length(&layout.edges);
    let aligned_nodes = count_aligned_nodes(&layout.nodes);
    let symmetry = compute_symmetry(&layout.nodes, &layout.bounding_box);
    let compactness = compute_compactness(&layout.nodes, &layout.bounding_box);
    let edge_length_variance = compute_edge_length_variance(&layout.edges);

    let mut obj = LayoutObjective {
        crossings,
        bends,
        position_variance,
        total_edge_length,
        aligned_nodes,
        symmetry,
        compactness,
        edge_length_variance,
        label_collisions: 0,
        score: 0.0,
    };
    obj.score = obj.compute_score();
    obj
}

/// Evaluate layout quality, including label collision data.
#[must_use]
pub fn evaluate_layout_with_labels(
    layout: &DiagramLayout,
    label_collisions: usize,
) -> LayoutObjective {
    let mut obj = evaluate_layout(layout);
    obj.label_collisions = label_collisions;
    obj.score = obj.compute_score();
    obj
}

// ── JSONL evidence logging (bd-19cll) ───────────────────────────────

/// Emit layout aesthetic metrics to the JSONL evidence log.
///
/// Writes one line per layout evaluation, including all raw metrics and
/// the composite score under each weight preset.
fn emit_layout_metrics_jsonl(
    config: &MermaidConfig,
    layout: &DiagramLayout,
    obj: &LayoutObjective,
    ir_hash: u64,
    diagram_type: DiagramType,
) {
    let Some(path) = config.log_path.as_deref() else {
        return;
    };
    let score_normal = obj.compute_score_with(&AestheticWeights::normal());
    let score_compact = obj.compute_score_with(&AestheticWeights::compact());
    let score_rich = obj.compute_score_with(&AestheticWeights::rich());
    let json = serde_json::json!({
        "event": "layout_metrics",
        "ir_hash": format!("0x{:016x}", ir_hash),
        "diagram_type": diagram_type.as_str(),
        "nodes": layout.nodes.len(),
        "edges": layout.edges.len(),
        "ranks": layout.stats.ranks,
        "budget_exceeded": layout.stats.budget_exceeded,
        "crossings": obj.crossings,
        "bends": obj.bends,
        "position_variance": obj.position_variance,
        "total_edge_length": obj.total_edge_length,
        "aligned_nodes": obj.aligned_nodes,
        "symmetry": obj.symmetry,
        "compactness": obj.compactness,
        "edge_length_variance": obj.edge_length_variance,
        "label_collisions": obj.label_collisions,
        "score_default": obj.score,
        "score_normal": score_normal,
        "score_compact": score_compact,
        "score_rich": score_rich,
    });
    let _ = append_jsonl_line(path, &json.to_string());
}

fn compute_position_variance(nodes: &[LayoutNodeBox]) -> f64 {
    if nodes.is_empty() {
        return 0.0;
    }
    let max_rank = nodes.iter().map(|n| n.rank).max().unwrap_or(0);
    let mut total_var = 0.0;
    let mut rank_count = 0;

    for r in 0..=max_rank {
        let xs: Vec<f64> = nodes
            .iter()
            .filter(|n| n.rank == r)
            .map(|n| n.rect.center().x)
            .collect();
        if xs.len() < 2 {
            continue;
        }
        let mean = xs.iter().sum::<f64>() / xs.len() as f64;
        let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / xs.len() as f64;
        total_var += var;
        rank_count += 1;
    }

    if rank_count == 0 {
        0.0
    } else {
        total_var / rank_count as f64
    }
}

fn compute_total_edge_length(edges: &[LayoutEdgePath]) -> f64 {
    let mut total = 0.0;
    for edge in edges {
        for w in edge.waypoints.windows(2) {
            let dx = w[1].x - w[0].x;
            let dy = w[1].y - w[0].y;
            total += (dx * dx + dy * dy).sqrt();
        }
    }
    total
}

fn count_aligned_nodes(nodes: &[LayoutNodeBox]) -> usize {
    if nodes.is_empty() {
        return 0;
    }
    let max_rank = nodes.iter().map(|n| n.rank).max().unwrap_or(0);
    let mut aligned = 0;
    let mut counts = vec![0usize; max_rank + 1];
    for node in nodes {
        counts[node.rank] += 1;
    }
    let mut per_rank: Vec<Vec<f64>> = counts
        .iter()
        .map(|&count| Vec::with_capacity(count))
        .collect();
    for node in nodes {
        let center_x = node.rect.x + node.rect.width * 0.5;
        per_rank[node.rank].push(center_x);
    }

    for xs in &mut per_rank {
        if xs.is_empty() {
            continue;
        }
        let mid = xs.len() / 2;
        xs.select_nth_unstable_by(mid, |a, b| {
            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
        });
        let median = xs[mid];
        for &x in xs.iter() {
            if (x - median).abs() < 0.1 {
                aligned += 1;
            }
        }
    }

    aligned
}

// ── New aesthetic metrics (bd-19cll) ─────────────────────────────────

/// Symmetry: how balanced node positions are across the bounding-box center.
///
/// Returns 0.0–1.0, where 1.0 means perfectly balanced left-right.
fn compute_symmetry(nodes: &[LayoutNodeBox], bbox: &LayoutRect) -> f64 {
    if nodes.is_empty() || bbox.width < f64::EPSILON {
        return 1.0;
    }
    let cx = bbox.x + bbox.width / 2.0;
    let mut left_mass = 0.0_f64;
    let mut right_mass = 0.0_f64;
    for n in nodes {
        let nc = n.rect.center().x;
        if nc < cx {
            left_mass += cx - nc;
        } else {
            right_mass += nc - cx;
        }
    }
    let total = left_mass + right_mass;
    if total < f64::EPSILON {
        return 1.0;
    }
    1.0 - (left_mass - right_mass).abs() / total
}

/// Compactness: ratio of total node area to bounding-box area.
///
/// Returns 0.0–1.0, where 1.0 means all space is used by nodes.
fn compute_compactness(nodes: &[LayoutNodeBox], bbox: &LayoutRect) -> f64 {
    let bbox_area = bbox.width * bbox.height;
    if bbox_area < f64::EPSILON {
        return 0.0;
    }
    let node_area: f64 = nodes.iter().map(|n| n.rect.width * n.rect.height).sum();
    (node_area / bbox_area).clamp(0.0, 1.0)
}

/// Standard deviation of individual edge lengths (Euclidean).
///
/// Lower values mean more uniform edge lengths.
fn compute_edge_length_variance(edges: &[LayoutEdgePath]) -> f64 {
    let lengths: Vec<f64> = edges
        .iter()
        .map(|e| {
            let mut len = 0.0_f64;
            for w in e.waypoints.windows(2) {
                let dx = w[1].x - w[0].x;
                let dy = w[1].y - w[0].y;
                len += (dx * dx + dy * dy).sqrt();
            }
            len
        })
        .collect();
    if lengths.len() < 2 {
        return 0.0;
    }
    let mean = lengths.iter().sum::<f64>() / lengths.len() as f64;
    let var = lengths.iter().map(|l| (l - mean) * (l - mean)).sum::<f64>() / lengths.len() as f64;
    var.sqrt()
}

// ── Constraint-based compaction ──────────────────────────────────────

/// Compact node positions within each rank using longest-path compaction.
///
/// This shifts nodes toward the center of their neighbors in adjacent ranks,
/// reducing total edge length while preserving the ordering and non-overlap
/// invariants.
fn compact_positions(
    node_rects: &mut [LayoutRect],
    rank_order: &[Vec<usize>],
    graph: &LayoutGraph,
    spacing: &LayoutSpacing,
    direction: GraphDirection,
    max_passes: usize,
) {
    // Hoist direction check out of the inner loops.
    let is_horizontal = matches!(
        direction,
        GraphDirection::TB | GraphDirection::TD | GraphDirection::BT
    );

    for _pass in 0..max_passes {
        let mut moved = false;

        for rank_nodes in rank_order {
            let min_gap = spacing.node_gap;

            for &node in rank_nodes {
                if node >= node_rects.len() {
                    continue;
                }

                // Compute ideal position: average of connected neighbor centers.
                let mut neighbor_sum = 0.0;
                let mut neighbor_count = 0usize;

                for &pred in &graph.rev[node] {
                    if pred < node_rects.len() {
                        let c = node_rects[pred].center();
                        neighbor_sum += if is_horizontal { c.x } else { c.y };
                        neighbor_count += 1;
                    }
                }
                for &succ in &graph.adj[node] {
                    if succ < node_rects.len() {
                        let c = node_rects[succ].center();
                        neighbor_sum += if is_horizontal { c.x } else { c.y };
                        neighbor_count += 1;
                    }
                }

                if neighbor_count == 0 {
                    continue;
                }

                let ideal = neighbor_sum / neighbor_count as f64;
                let current = if is_horizontal {
                    node_rects[node].center().x
                } else {
                    node_rects[node].center().y
                };

                let delta = ideal - current;
                if delta.abs() < 0.5 {
                    continue;
                }

                // Check if moving wouldn't overlap with rank neighbors.
                let can_move = rank_nodes.iter().all(|&other| {
                    if other == node || other >= node_rects.len() {
                        return true;
                    }
                    if is_horizontal {
                        let new_center = node_rects[node].x + delta + node_rects[node].width / 2.0;
                        let other_center = node_rects[other].x + node_rects[other].width / 2.0;
                        let half_size =
                            node_rects[node].width / 2.0 + node_rects[other].width / 2.0 + min_gap;
                        (new_center - other_center).abs() >= half_size - 0.01
                    } else {
                        let new_center = node_rects[node].y + delta + node_rects[node].height / 2.0;
                        let other_center = node_rects[other].y + node_rects[other].height / 2.0;
                        let half_size = node_rects[node].height / 2.0
                            + node_rects[other].height / 2.0
                            + min_gap;
                        (new_center - other_center).abs() >= half_size - 0.01
                    }
                });

                if can_move {
                    if is_horizontal {
                        node_rects[node].x += delta;
                    } else {
                        node_rects[node].y += delta;
                    }
                    moved = true;
                }
            }
        }

        if !moved {
            break;
        }
    }
}

// ── RouteGrid for obstacle-aware edge routing ────────────────────────

/// A grid-based routing helper for obstacle-aware edge path computation.
///
/// Cells in the grid can be occupied (by nodes or clusters) or free.
/// The router finds paths through free cells using BFS.
#[derive(Debug, Clone)]
pub struct RouteGrid {
    /// Grid width in cells.
    pub cols: usize,
    /// Grid height in cells.
    pub rows: usize,
    /// Cell size in world units.
    pub cell_size: f64,
    /// Origin offset.
    pub origin: LayoutPoint,
    /// Occupied cells (row-major, true = blocked).
    occupied: Vec<bool>,
}

/// Reusable scratch buffers for A* pathfinding, avoiding per-edge allocation.
struct RoutingScratch {
    /// Best g-cost for each (cell, direction) state.
    g_best: Vec<f64>,
    /// Parent link for each (cell, direction) state.
    parent: Vec<Option<(usize, usize, MoveDir)>>,
    /// Priority queue for A*.
    heap: std::collections::BinaryHeap<AStarState>,
    /// Indices into g_best/parent that were dirtied (for sparse reset).
    dirty: Vec<usize>,
    /// Capacity (grid_size * num_dirs) for bounds checking.
    capacity: usize,
}

impl RoutingScratch {
    fn new(grid_size: usize) -> Self {
        let num_dirs = 5;
        let cap = grid_size * num_dirs;
        Self {
            g_best: vec![f64::INFINITY; cap],
            parent: vec![None; cap],
            heap: std::collections::BinaryHeap::new(),
            dirty: Vec::with_capacity(cap.min(4096)),
            capacity: cap,
        }
    }

    /// Ensure buffers are large enough for the given grid size.
    fn ensure_capacity(&mut self, grid_size: usize) {
        let num_dirs = 5;
        let cap = grid_size * num_dirs;
        if cap > self.capacity {
            self.g_best.resize(cap, f64::INFINITY);
            self.parent.resize(cap, None);
            self.capacity = cap;
        }
    }

    /// Reset only the cells that were dirtied in the last search.
    fn sparse_reset(&mut self) {
        for &idx in &self.dirty {
            self.g_best[idx] = f64::INFINITY;
            self.parent[idx] = None;
        }
        self.dirty.clear();
        self.heap.clear();
    }
}

impl RouteGrid {
    /// Build a RouteGrid from node and cluster rectangles.
    #[must_use]
    pub fn from_layout(
        nodes: &[LayoutNodeBox],
        clusters: &[LayoutClusterBox],
        bounding_box: &LayoutRect,
        cell_size: f64,
    ) -> Self {
        let margin = cell_size * 2.0;
        let origin = LayoutPoint {
            x: bounding_box.x - margin,
            y: bounding_box.y - margin,
        };
        let total_width = bounding_box.width + 2.0 * margin;
        let total_height = bounding_box.height + 2.0 * margin;

        let cols = (total_width / cell_size).ceil() as usize + 1;
        let rows = (total_height / cell_size).ceil() as usize + 1;
        let mut occupied = vec![false; cols * rows];

        // Mark node cells as occupied.
        for node in nodes {
            mark_rect_occupied(&mut occupied, cols, rows, cell_size, &origin, &node.rect);
        }
        // Mark cluster boundary cells as occupied (interior remains routable).
        for cluster in clusters {
            mark_rect_boundary_occupied(
                &mut occupied,
                cols,
                rows,
                cell_size,
                &origin,
                &cluster.rect,
            );
        }

        Self {
            cols,
            rows,
            cell_size,
            origin,
            occupied,
        }
    }

    /// Convert a world-space point to grid coordinates.
    fn to_grid(&self, p: LayoutPoint) -> (usize, usize) {
        let col = ((p.x - self.origin.x) / self.cell_size).floor().max(0.0) as usize;
        let row = ((p.y - self.origin.y) / self.cell_size).floor().max(0.0) as usize;
        (
            col.min(self.cols.saturating_sub(1)),
            row.min(self.rows.saturating_sub(1)),
        )
    }

    /// Convert grid coordinates back to world space (center of cell).
    fn to_world(&self, col: usize, row: usize) -> LayoutPoint {
        LayoutPoint {
            x: self.origin.x + (col as f64 + 0.5) * self.cell_size,
            y: self.origin.y + (row as f64 + 0.5) * self.cell_size,
        }
    }

    /// Check if a cell is free for routing.
    fn is_free(&self, col: usize, row: usize) -> bool {
        if col >= self.cols || row >= self.rows {
            return false;
        }
        !self.occupied[row * self.cols + col]
    }

    fn snap_to_free(&self, col: usize, row: usize) -> (usize, usize) {
        if self.is_free(col, row) {
            return (col, row);
        }

        let max_radius = self.cols.max(self.rows);
        let start_c = col as isize;
        let start_r = row as isize;

        for dist in 1..=max_radius {
            let dist = dist as isize;
            for dr in -dist..=dist {
                let dc = dist - dr.abs();
                let mut candidates = [0isize; 2];
                let mut count = 0;
                candidates[count] = dc;
                count += 1;
                if dc != 0 {
                    candidates[count] = -dc;
                    count += 1;
                }
                for &candidate in candidates.iter().take(count) {
                    let nc = start_c + candidate;
                    let nr = start_r + dr;
                    if nc < 0 || nr < 0 {
                        continue;
                    }
                    let nc = nc as usize;
                    let nr = nr as usize;
                    if nc >= self.cols || nr >= self.rows {
                        continue;
                    }
                    if self.is_free(nc, nr) {
                        return (nc, nr);
                    }
                }
            }
        }

        (col, row)
    }

    /// Find a path between two world-space points using BFS.
    ///
    /// Returns waypoints in world space, or a direct line if no path found.
    #[must_use]
    pub fn find_path(&self, from: LayoutPoint, to: LayoutPoint) -> Vec<LayoutPoint> {
        let (sc, sr) = self.to_grid(from);
        let (ec, er) = self.to_grid(to);
        let (sc, sr) = self.snap_to_free(sc, sr);
        let (ec, er) = self.snap_to_free(ec, er);

        if sc == ec && sr == er {
            return vec![from, to];
        }

        // BFS with 4-directional movement.
        let mut visited = vec![false; self.cols * self.rows];
        let mut parent: Vec<Option<(usize, usize)>> = vec![None; self.cols * self.rows];
        let mut queue = std::collections::VecDeque::new();

        // Mark start as visited.
        visited[sr * self.cols + sc] = true;
        queue.push_back((sc, sr));

        let dirs: [(i32, i32); 4] = [(0, -1), (0, 1), (-1, 0), (1, 0)];

        while let Some((c, r)) = queue.pop_front() {
            if c == ec && r == er {
                break;
            }
            for (dc, dr) in &dirs {
                let nc = c as i32 + dc;
                let nr = r as i32 + dr;
                if nc < 0 || nr < 0 {
                    continue;
                }
                let nc = nc as usize;
                let nr = nr as usize;
                if nc >= self.cols || nr >= self.rows {
                    continue;
                }
                let idx = nr * self.cols + nc;
                if visited[idx] {
                    continue;
                }
                // Allow traversal to the endpoint even if it's marked occupied.
                if !(self.is_free(nc, nr) || (nc == ec && nr == er)) {
                    continue;
                }
                visited[idx] = true;
                parent[idx] = Some((c, r));
                queue.push_back((nc, nr));
            }
        }

        // Reconstruct path.
        let end_idx = er * self.cols + ec;
        if !visited[end_idx] {
            // No path found; fall back to direct line.
            return vec![from, to];
        }

        let mut path_grid = vec![(ec, er)];
        let mut cur = (ec, er);
        while let Some(p) = parent[cur.1 * self.cols + cur.0] {
            path_grid.push(p);
            if p == (sc, sr) {
                break;
            }
            cur = p;
        }
        path_grid.reverse();

        // Simplify: remove collinear intermediate points.
        let mut waypoints = vec![from];
        for i in 1..path_grid.len().saturating_sub(1) {
            let prev = path_grid[i - 1];
            let curr = path_grid[i];
            let next = path_grid[i + 1];
            // Keep point only if direction changes.
            let d1 = (curr.0 as i32 - prev.0 as i32, curr.1 as i32 - prev.1 as i32);
            let d2 = (next.0 as i32 - curr.0 as i32, next.1 as i32 - curr.1 as i32);
            if d1 != d2 {
                waypoints.push(self.to_world(curr.0, curr.1));
            }
        }
        waypoints.push(to);
        waypoints
    }
}

fn mark_rect_occupied(
    grid: &mut [bool],
    cols: usize,
    rows: usize,
    cell_size: f64,
    origin: &LayoutPoint,
    rect: &LayoutRect,
) {
    let c0 = ((rect.x - origin.x) / cell_size).floor().max(0.0) as usize;
    let r0 = ((rect.y - origin.y) / cell_size).floor().max(0.0) as usize;
    let c1 = ((rect.x + rect.width - origin.x) / cell_size).ceil() as usize;
    let r1 = ((rect.y + rect.height - origin.y) / cell_size).ceil() as usize;

    for r in r0..r1.min(rows) {
        for c in c0..c1.min(cols) {
            grid[r * cols + c] = true;
        }
    }
}

fn mark_rect_boundary_occupied(
    grid: &mut [bool],
    cols: usize,
    rows: usize,
    cell_size: f64,
    origin: &LayoutPoint,
    rect: &LayoutRect,
) {
    let c0 = ((rect.x - origin.x) / cell_size).floor().max(0.0) as usize;
    let r0 = ((rect.y - origin.y) / cell_size).floor().max(0.0) as usize;
    let c1 = ((rect.x + rect.width - origin.x) / cell_size).ceil() as usize;
    let r1 = ((rect.y + rect.height - origin.y) / cell_size).ceil() as usize;

    let c1 = c1.min(cols);
    let r1 = r1.min(rows);
    if c0 >= c1 || r0 >= r1 {
        return;
    }

    for r in r0..r1 {
        for c in c0..c1 {
            let on_top = r == r0;
            let on_bottom = r + 1 == r1;
            let on_left = c == c0;
            let on_right = c + 1 == c1;
            if on_top || on_bottom || on_left || on_right {
                grid[r * cols + c] = true;
            }
        }
    }
}

// ── A* routing with bend penalties ───────────────────────────────────

/// Cost weights for A* routing decisions.
#[derive(Debug, Clone, Copy)]
pub struct RoutingWeights {
    /// Cost per grid step (base movement cost).
    pub step_cost: f64,
    /// Extra cost for each direction change (bend penalty).
    pub bend_penalty: f64,
    /// Extra cost for crossing another route (crossing penalty).
    pub crossing_penalty: f64,
}

impl Default for RoutingWeights {
    fn default() -> Self {
        Self {
            step_cost: 1.0,
            bend_penalty: 3.0,
            crossing_penalty: 5.0,
        }
    }
}

/// Diagnostics from a single edge routing computation.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteDiagnostics {
    /// Total cost of the route.
    pub cost: f64,
    /// Number of direction changes (bends) in the route.
    pub bends: usize,
    /// Number of cells explored during search.
    pub cells_explored: usize,
    /// Whether the route fell back to a direct line.
    pub fallback: bool,
}

/// Diagnostics for all edges in a diagram.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingReport {
    /// Per-edge diagnostics.
    pub edges: Vec<RouteDiagnostics>,
    /// Total routing cost.
    pub total_cost: f64,
    /// Total bends across all edges.
    pub total_bends: usize,
    /// Total cells explored.
    pub total_cells_explored: usize,
    /// Number of edges that fell back to direct lines.
    pub fallback_count: usize,
}

/// Direction of last move in A* state (for bend penalty tracking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MoveDir {
    Up,
    Down,
    Left,
    Right,
    Start,
}

/// A* state for priority queue.
#[derive(Debug, Clone)]
struct AStarState {
    col: usize,
    row: usize,
    g_cost: f64,
    f_cost: f64,
    dir: MoveDir,
}

impl PartialEq for AStarState {
    fn eq(&self, other: &Self) -> bool {
        self.f_cost == other.f_cost && self.col == other.col && self.row == other.row
    }
}

impl Eq for AStarState {}

impl PartialOrd for AStarState {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AStarState {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse ordering for min-heap: lower f_cost = higher priority.
        other
            .f_cost
            .partial_cmp(&self.f_cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                // Deterministic tie-breaking: prefer lower (col, row).
                self.col
                    .cmp(&other.col)
                    .then_with(|| self.row.cmp(&other.row))
            })
    }
}

impl RouteGrid {
    /// Find a path using A* with bend penalties.
    ///
    /// Returns (waypoints, diagnostics).
    #[must_use]
    pub fn find_path_astar(
        &self,
        from: LayoutPoint,
        to: LayoutPoint,
        weights: &RoutingWeights,
        occupied_routes: &[bool],
    ) -> (Vec<LayoutPoint>, RouteDiagnostics) {
        let (sc, sr) = self.to_grid(from);
        let (ec, er) = self.to_grid(to);

        if sc == ec && sr == er {
            return (
                vec![from, to],
                RouteDiagnostics {
                    cost: 0.0,
                    bends: 0,
                    cells_explored: 0,
                    fallback: false,
                },
            );
        }

        let grid_size = self.cols * self.rows;
        // Track best g_cost for each (col, row, dir) state.
        // Use 5 layers for the 5 directions.
        let num_dirs = 5;
        let mut g_best = vec![f64::INFINITY; grid_size * num_dirs];
        let mut parent: Vec<Option<(usize, usize, MoveDir)>> = vec![None; grid_size * num_dirs];
        let mut cells_explored = 0usize;

        let mut heap = std::collections::BinaryHeap::new();

        let start_idx = sr * self.cols + sc;
        let dir_idx = dir_to_idx(MoveDir::Start);
        g_best[start_idx * num_dirs + dir_idx] = 0.0;
        heap.push(AStarState {
            col: sc,
            row: sr,
            g_cost: 0.0,
            f_cost: heuristic(sc, sr, ec, er, weights.step_cost),
            dir: MoveDir::Start,
        });

        let dirs: [(i32, i32, MoveDir); 4] = [
            (0, -1, MoveDir::Up),
            (0, 1, MoveDir::Down),
            (-1, 0, MoveDir::Left),
            (1, 0, MoveDir::Right),
        ];

        let mut found = false;
        let mut end_dir = MoveDir::Start;

        while let Some(state) = heap.pop() {
            let c = state.col;
            let r = state.row;

            if c == ec && r == er {
                found = true;
                end_dir = state.dir;
                break;
            }

            let idx = r * self.cols + c;
            let di = dir_to_idx(state.dir);
            if state.g_cost > g_best[idx * num_dirs + di] {
                continue;
            }

            cells_explored += 1;

            for &(dc, dr, new_dir) in &dirs {
                let nc = c as i32 + dc;
                let nr = r as i32 + dr;
                if nc < 0 || nr < 0 {
                    continue;
                }
                let nc = nc as usize;
                let nr = nr as usize;
                if nc >= self.cols || nr >= self.rows {
                    continue;
                }

                // Allow endpoint even if occupied.
                if !(self.is_free(nc, nr) || (nc == ec && nr == er)) {
                    continue;
                }

                let new_idx = nr * self.cols + nc;
                let mut step = weights.step_cost;

                // Bend penalty.
                if state.dir != MoveDir::Start && state.dir != new_dir {
                    step += weights.bend_penalty;
                }

                // Crossing penalty.
                if !occupied_routes.is_empty()
                    && new_idx < occupied_routes.len()
                    && occupied_routes[new_idx]
                {
                    step += weights.crossing_penalty;
                }

                let new_g = state.g_cost + step;
                let new_di = dir_to_idx(new_dir);
                if new_g < g_best[new_idx * num_dirs + new_di] {
                    g_best[new_idx * num_dirs + new_di] = new_g;
                    parent[new_idx * num_dirs + new_di] = Some((c, r, state.dir));
                    heap.push(AStarState {
                        col: nc,
                        row: nr,
                        g_cost: new_g,
                        f_cost: new_g + heuristic(nc, nr, ec, er, weights.step_cost),
                        dir: new_dir,
                    });
                }
            }
        }

        if !found {
            return (
                vec![from, to],
                RouteDiagnostics {
                    cost: 0.0,
                    bends: 0,
                    cells_explored,
                    fallback: true,
                },
            );
        }

        // Reconstruct path.
        let mut path_grid = vec![];
        let mut cur_c = ec;
        let mut cur_r = er;
        let mut cur_dir = end_dir;
        loop {
            path_grid.push((cur_c, cur_r));
            let idx = cur_r * self.cols + cur_c;
            let di = dir_to_idx(cur_dir);
            match parent[idx * num_dirs + di] {
                Some((pc, pr, pd)) => {
                    cur_c = pc;
                    cur_r = pr;
                    cur_dir = pd;
                }
                None => break,
            }
        }
        path_grid.reverse();

        // Count bends and compute cost.
        let mut bends = 0;
        let end_idx = er * self.cols + ec;
        let end_di = dir_to_idx(end_dir);
        let cost = g_best[end_idx * num_dirs + end_di];

        // Simplify: remove collinear points, count bends.
        let mut waypoints = vec![from];
        for i in 1..path_grid.len().saturating_sub(1) {
            let prev = path_grid[i - 1];
            let curr = path_grid[i];
            let next = path_grid[i + 1];
            let d1 = (curr.0 as i32 - prev.0 as i32, curr.1 as i32 - prev.1 as i32);
            let d2 = (next.0 as i32 - curr.0 as i32, next.1 as i32 - curr.1 as i32);
            if d1 != d2 {
                waypoints.push(self.to_world(curr.0, curr.1));
                bends += 1;
            }
        }
        waypoints.push(to);

        (
            waypoints,
            RouteDiagnostics {
                cost,
                bends,
                cells_explored,
                fallback: false,
            },
        )
    }

    /// A* pathfinding with reusable scratch buffers (avoids per-edge allocation).
    fn find_path_astar_reuse(
        &self,
        from: LayoutPoint,
        to: LayoutPoint,
        weights: &RoutingWeights,
        occupied_routes: &[bool],
        scratch: &mut RoutingScratch,
    ) -> (Vec<LayoutPoint>, RouteDiagnostics) {
        let (sc, sr) = self.to_grid(from);
        let (ec, er) = self.to_grid(to);

        if sc == ec && sr == er {
            return (
                vec![from, to],
                RouteDiagnostics {
                    cost: 0.0,
                    bends: 0,
                    cells_explored: 0,
                    fallback: false,
                },
            );
        }

        let grid_size = self.cols * self.rows;
        let num_dirs = 5;
        scratch.ensure_capacity(grid_size);
        scratch.sparse_reset();

        let mut cells_explored = 0usize;

        let start_idx = sr * self.cols + sc;
        let dir_idx = dir_to_idx(MoveDir::Start);
        let flat_idx = start_idx * num_dirs + dir_idx;
        scratch.g_best[flat_idx] = 0.0;
        scratch.dirty.push(flat_idx);
        scratch.heap.push(AStarState {
            col: sc,
            row: sr,
            g_cost: 0.0,
            f_cost: heuristic(sc, sr, ec, er, weights.step_cost),
            dir: MoveDir::Start,
        });

        let dirs: [(i32, i32, MoveDir); 4] = [
            (0, -1, MoveDir::Up),
            (0, 1, MoveDir::Down),
            (-1, 0, MoveDir::Left),
            (1, 0, MoveDir::Right),
        ];

        let mut found = false;
        let mut end_dir = MoveDir::Start;

        while let Some(state) = scratch.heap.pop() {
            let c = state.col;
            let r = state.row;

            if c == ec && r == er {
                found = true;
                end_dir = state.dir;
                break;
            }

            let idx = r * self.cols + c;
            let di = dir_to_idx(state.dir);
            if state.g_cost > scratch.g_best[idx * num_dirs + di] {
                continue;
            }

            cells_explored += 1;

            for &(dc, dr, new_dir) in &dirs {
                let nc = c as i32 + dc;
                let nr = r as i32 + dr;
                if nc < 0 || nr < 0 {
                    continue;
                }
                let nc = nc as usize;
                let nr = nr as usize;
                if nc >= self.cols || nr >= self.rows {
                    continue;
                }

                if !(self.is_free(nc, nr) || (nc == ec && nr == er)) {
                    continue;
                }

                let new_idx = nr * self.cols + nc;
                let mut step = weights.step_cost;

                if state.dir != MoveDir::Start && state.dir != new_dir {
                    step += weights.bend_penalty;
                }

                if !occupied_routes.is_empty()
                    && new_idx < occupied_routes.len()
                    && occupied_routes[new_idx]
                {
                    step += weights.crossing_penalty;
                }

                let new_g = state.g_cost + step;
                let new_di = dir_to_idx(new_dir);
                let flat = new_idx * num_dirs + new_di;
                if new_g < scratch.g_best[flat] {
                    scratch.g_best[flat] = new_g;
                    scratch.parent[flat] = Some((c, r, state.dir));
                    scratch.dirty.push(flat);
                    scratch.heap.push(AStarState {
                        col: nc,
                        row: nr,
                        g_cost: new_g,
                        f_cost: new_g + heuristic(nc, nr, ec, er, weights.step_cost),
                        dir: new_dir,
                    });
                }
            }
        }

        if !found {
            return (
                vec![from, to],
                RouteDiagnostics {
                    cost: 0.0,
                    bends: 0,
                    cells_explored,
                    fallback: true,
                },
            );
        }

        // Reconstruct path.
        let mut path_grid = vec![];
        let mut cur_c = ec;
        let mut cur_r = er;
        let mut cur_dir = end_dir;
        loop {
            path_grid.push((cur_c, cur_r));
            let idx = cur_r * self.cols + cur_c;
            let di = dir_to_idx(cur_dir);
            match scratch.parent[idx * num_dirs + di] {
                Some((pc, pr, pd)) => {
                    cur_c = pc;
                    cur_r = pr;
                    cur_dir = pd;
                }
                None => break,
            }
        }
        path_grid.reverse();

        // Count bends and compute cost.
        let end_idx = er * self.cols + ec;
        let end_di = dir_to_idx(end_dir);
        let cost = scratch.g_best[end_idx * num_dirs + end_di];

        // Simplify: remove collinear points, count bends.
        let mut bends = 0;
        let mut waypoints = vec![from];
        for i in 1..path_grid.len().saturating_sub(1) {
            let prev = path_grid[i - 1];
            let curr = path_grid[i];
            let next = path_grid[i + 1];
            let d1 = (curr.0 as i32 - prev.0 as i32, curr.1 as i32 - prev.1 as i32);
            let d2 = (next.0 as i32 - curr.0 as i32, next.1 as i32 - curr.1 as i32);
            if d1 != d2 {
                waypoints.push(self.to_world(curr.0, curr.1));
                bends += 1;
            }
        }
        waypoints.push(to);

        (
            waypoints,
            RouteDiagnostics {
                cost,
                bends,
                cells_explored,
                fallback: false,
            },
        )
    }
}

fn dir_to_idx(dir: MoveDir) -> usize {
    match dir {
        MoveDir::Up => 0,
        MoveDir::Down => 1,
        MoveDir::Left => 2,
        MoveDir::Right => 3,
        MoveDir::Start => 4,
    }
}

fn heuristic(c1: usize, r1: usize, c2: usize, r2: usize, step_cost: f64) -> f64 {
    (c1.abs_diff(c2) + r1.abs_diff(r2)) as f64 * step_cost
}

// ── Self-loops and parallel edge handling ────────────────────────────

/// Generate a self-loop route (node connects to itself).
///
/// Creates a small loop above/right of the node.
#[must_use]
pub fn self_loop_route(node_rect: &LayoutRect, direction: GraphDirection) -> Vec<LayoutPoint> {
    let c = node_rect.center();
    let offset = node_rect.height.min(node_rect.width) * 0.6;

    match direction {
        GraphDirection::TB | GraphDirection::TD => {
            // Loop above-right.
            vec![
                LayoutPoint {
                    x: c.x + node_rect.width / 2.0,
                    y: c.y,
                },
                LayoutPoint {
                    x: c.x + node_rect.width / 2.0 + offset,
                    y: c.y,
                },
                LayoutPoint {
                    x: c.x + node_rect.width / 2.0 + offset,
                    y: c.y - offset,
                },
                LayoutPoint {
                    x: c.x,
                    y: c.y - offset,
                },
                LayoutPoint {
                    x: c.x,
                    y: node_rect.y,
                },
            ]
        }
        GraphDirection::BT => {
            // Loop below-right.
            vec![
                LayoutPoint {
                    x: c.x + node_rect.width / 2.0,
                    y: c.y,
                },
                LayoutPoint {
                    x: c.x + node_rect.width / 2.0 + offset,
                    y: c.y,
                },
                LayoutPoint {
                    x: c.x + node_rect.width / 2.0 + offset,
                    y: c.y + offset,
                },
                LayoutPoint {
                    x: c.x,
                    y: c.y + offset,
                },
                LayoutPoint {
                    x: c.x,
                    y: node_rect.y + node_rect.height,
                },
            ]
        }
        GraphDirection::LR | GraphDirection::RL => {
            // Loop above-right.
            vec![
                LayoutPoint {
                    x: c.x,
                    y: node_rect.y,
                },
                LayoutPoint {
                    x: c.x,
                    y: node_rect.y - offset,
                },
                LayoutPoint {
                    x: c.x + offset,
                    y: node_rect.y - offset,
                },
                LayoutPoint {
                    x: c.x + offset,
                    y: c.y,
                },
                LayoutPoint {
                    x: c.x + node_rect.width / 2.0,
                    y: c.y,
                },
            ]
        }
    }
}

/// Compute a lateral offset for parallel edges between the same pair of nodes.
///
/// `edge_index` is the 0-based index among parallel edges; `total` is the count.
/// Returns an offset perpendicular to the edge direction.
#[must_use]
pub fn parallel_edge_offset(edge_index: usize, total: usize, lane_gap: f64) -> f64 {
    if total <= 1 {
        return 0.0;
    }
    let center = (total - 1) as f64 / 2.0;
    (edge_index as f64 - center) * lane_gap
}

// ── Full routing pipeline ────────────────────────────────────────────

/// Route all edges in a diagram using A* with obstacle avoidance.
///
/// Handles self-loops, parallel edges, and produces per-edge diagnostics.
pub fn route_all_edges(
    ir: &MermaidDiagramIr,
    layout: &DiagramLayout,
    config: &MermaidConfig,
    weights: &RoutingWeights,
) -> (Vec<LayoutEdgePath>, RoutingReport) {
    let grid = RouteGrid::from_layout(
        &layout.nodes,
        &layout.clusters,
        &layout.bounding_box,
        LayoutSpacing::default().node_gap,
    );

    let mut occupied_routes = vec![false; grid.cols * grid.rows];
    let mut all_paths = Vec::with_capacity(ir.edges.len());
    let mut all_diags = Vec::with_capacity(ir.edges.len());
    let mut ops_used = 0usize;

    // Group edges by (from, to) for parallel edge detection.
    let mut edge_groups: std::collections::BTreeMap<(usize, usize), Vec<usize>> =
        std::collections::BTreeMap::new();
    for (idx, edge) in ir.edges.iter().enumerate() {
        let Some(from) = endpoint_node_idx(ir, &edge.from) else {
            continue;
        };
        let Some(to) = endpoint_node_idx(ir, &edge.to) else {
            continue;
        };
        if from >= layout.nodes.len() || to >= layout.nodes.len() {
            continue;
        }
        let key = if from <= to { (from, to) } else { (to, from) };
        edge_groups.entry(key).or_default().push(idx);
    }

    // Pre-compute parallel edge offsets.
    let mut edge_offsets = vec![0.0f64; ir.edges.len()];
    for group in edge_groups.values() {
        for (i, &idx) in group.iter().enumerate() {
            edge_offsets[idx] = parallel_edge_offset(i, group.len(), 1.5);
        }
    }

    let grid_size = grid.cols * grid.rows;
    let mut routing_scratch = RoutingScratch::new(grid_size);

    for (idx, edge) in ir.edges.iter().enumerate() {
        let from_idx = endpoint_node_idx(ir, &edge.from);
        let to_idx = endpoint_node_idx(ir, &edge.to);

        match (from_idx, to_idx) {
            (Some(u), Some(v)) if u < layout.nodes.len() && v < layout.nodes.len() => {
                if u == v {
                    // Self-loop.
                    let waypoints = self_loop_route(&layout.nodes[u].rect, ir.direction);
                    all_diags.push(RouteDiagnostics {
                        cost: 0.0,
                        bends: waypoints.len().saturating_sub(2),
                        cells_explored: 0,
                        fallback: false,
                    });
                    all_paths.push(LayoutEdgePath {
                        edge_idx: idx,
                        waypoints,
                        bundle_count: 1,
                        bundle_members: Vec::new(),
                    });
                    continue;
                }

                // Check route budget.
                if ops_used >= config.route_budget {
                    // Budget exceeded: fall back to direct line.
                    let from_pt = layout.nodes[u].rect.center();
                    let to_pt = layout.nodes[v].rect.center();
                    all_diags.push(RouteDiagnostics {
                        cost: 0.0,
                        bends: 0,
                        cells_explored: 0,
                        fallback: true,
                    });
                    all_paths.push(LayoutEdgePath {
                        edge_idx: idx,
                        waypoints: vec![from_pt, to_pt],
                        bundle_count: 1,
                        bundle_members: Vec::new(),
                    });
                    continue;
                }

                // Compute port points with parallel offset.
                let from_port = edge_port(
                    &layout.nodes[u].rect,
                    layout.nodes[u].rect.center(),
                    layout.nodes[v].rect.center(),
                    ir.direction,
                    true,
                );
                let to_port = edge_port(
                    &layout.nodes[v].rect,
                    layout.nodes[v].rect.center(),
                    layout.nodes[u].rect.center(),
                    ir.direction,
                    false,
                );

                // Apply parallel offset.
                let offset = edge_offsets[idx];
                let (from_pt, to_pt) = apply_offset(from_port, to_port, offset, ir.direction);

                let (waypoints, diag) = grid.find_path_astar_reuse(
                    from_pt,
                    to_pt,
                    weights,
                    &occupied_routes,
                    &mut routing_scratch,
                );

                ops_used += diag.cells_explored;

                // Mark route cells as occupied for crossing penalty.
                if !diag.fallback {
                    mark_route_cells(&grid, &waypoints, &mut occupied_routes);
                }

                all_diags.push(diag);
                all_paths.push(LayoutEdgePath {
                    edge_idx: idx,
                    waypoints,
                    bundle_count: 1,
                    bundle_members: Vec::new(),
                });
            }
            _ => {
                all_diags.push(RouteDiagnostics {
                    cost: 0.0,
                    bends: 0,
                    cells_explored: 0,
                    fallback: true,
                });
                all_paths.push(LayoutEdgePath {
                    edge_idx: idx,
                    waypoints: vec![],
                    bundle_count: 1,
                    bundle_members: Vec::new(),
                });
            }
        }
    }

    let total_cost: f64 = all_diags.iter().map(|d| d.cost).sum();
    let total_bends: usize = all_diags.iter().map(|d| d.bends).sum();
    let total_cells: usize = all_diags.iter().map(|d| d.cells_explored).sum();
    let fallbacks: usize = all_diags.iter().filter(|d| d.fallback).count();

    let report = RoutingReport {
        edges: all_diags,
        total_cost,
        total_bends,
        total_cells_explored: total_cells,
        fallback_count: fallbacks,
    };

    (all_paths, report)
}

/// Apply a perpendicular offset to edge endpoints.
fn apply_offset(
    from: LayoutPoint,
    to: LayoutPoint,
    offset: f64,
    direction: GraphDirection,
) -> (LayoutPoint, LayoutPoint) {
    if offset.abs() < f64::EPSILON {
        return (from, to);
    }
    match direction {
        GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => {
            // Vertical flow: offset horizontally.
            (
                LayoutPoint {
                    x: from.x + offset,
                    y: from.y,
                },
                LayoutPoint {
                    x: to.x + offset,
                    y: to.y,
                },
            )
        }
        GraphDirection::LR | GraphDirection::RL => {
            // Horizontal flow: offset vertically.
            (
                LayoutPoint {
                    x: from.x,
                    y: from.y + offset,
                },
                LayoutPoint {
                    x: to.x,
                    y: to.y + offset,
                },
            )
        }
    }
}

fn mark_route_cells(grid: &RouteGrid, waypoints: &[LayoutPoint], occupied_routes: &mut [bool]) {
    if waypoints.is_empty() {
        return;
    }

    let mut prev = grid.to_grid(waypoints[0]);
    let mark_cell = |col: usize, row: usize, occupied_routes: &mut [bool]| {
        let idx = row * grid.cols + col;
        if idx < occupied_routes.len() {
            occupied_routes[idx] = true;
        }
    };

    mark_cell(prev.0, prev.1, occupied_routes);

    for &wp in waypoints.iter().skip(1) {
        let next = grid.to_grid(wp);
        if prev == next {
            continue;
        }

        if prev.0 == next.0 {
            let (min_r, max_r) = if prev.1 <= next.1 {
                (prev.1, next.1)
            } else {
                (next.1, prev.1)
            };
            for row in min_r..=max_r {
                mark_cell(prev.0, row, occupied_routes);
            }
        } else if prev.1 == next.1 {
            let (min_c, max_c) = if prev.0 <= next.0 {
                (prev.0, next.0)
            } else {
                (next.0, prev.0)
            };
            for col in min_c..=max_c {
                mark_cell(col, prev.1, occupied_routes);
            }
        } else {
            // Fallback for unexpected diagonal: walk a Manhattan path.
            let mut col = prev.0;
            let mut row = prev.1;
            while col != next.0 {
                col = if col < next.0 { col + 1 } else { col - 1 };
                mark_cell(col, row, occupied_routes);
            }
            while row != next.1 {
                row = if row < next.1 { row + 1 } else { row - 1 };
                mark_cell(col, row, occupied_routes);
            }
        }

        prev = next;
    }
}

// ── Label placement and collision avoidance ──────────────────────────

/// A placed label with its resolved position and metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedLabel {
    /// Index into the IR labels array.
    pub label_idx: usize,
    /// Bounding rectangle in world units.
    pub rect: LayoutRect,
    /// Whether this label was offset to avoid a collision.
    pub was_offset: bool,
    /// Whether the label text was truncated.
    pub was_truncated: bool,
    /// Whether this label was spilled to a legend area.
    pub spilled_to_legend: bool,
    /// Leader line connecting label to its anchor (if offset is large).
    pub leader_line: Option<(LayoutPoint, LayoutPoint)>,
}

/// A collision resolution event for diagnostics/logging.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelCollisionEvent {
    /// Label that was moved.
    pub label_idx: usize,
    /// What it collided with.
    pub collider: LabelCollider,
    /// Offset applied (dx, dy) in world units.
    pub offset: (f64, f64),
}

/// What a label collided with.
#[derive(Debug, Clone, PartialEq)]
pub enum LabelCollider {
    /// Another label.
    Label(usize),
    /// A node.
    Node(usize),
    /// An edge waypoint region.
    Edge(usize),
}

/// Configuration for label placement.
#[derive(Debug, Clone, Copy)]
pub struct LabelPlacementConfig {
    /// Maximum label width in world units before wrapping/truncation.
    pub max_label_width: f64,
    /// Maximum label height in world units.
    pub max_label_height: f64,
    /// Padding around labels for collision detection.
    pub label_margin: f64,
    /// Step size for collision-avoidance offset search.
    pub offset_step: f64,
    /// Maximum offset distance to try before giving up.
    pub max_offset: f64,
    /// Character width in world units (for text measurement).
    pub char_width: f64,
    /// Line height in world units.
    pub line_height: f64,
    /// Distance threshold above which a leader line is drawn.
    pub leader_line_threshold: f64,
    /// Maximum number of text lines before vertical truncation.
    pub max_lines: usize,
    /// Whether to enable legend spillover for labels that cannot fit.
    pub legend_enabled: bool,
}

impl Default for LabelPlacementConfig {
    fn default() -> Self {
        Self {
            max_label_width: 20.0,
            max_label_height: 3.0,
            label_margin: 0.5,
            offset_step: 1.0,
            max_offset: 8.0,
            char_width: 1.0,
            line_height: 1.0,
            leader_line_threshold: 3.0,
            max_lines: 3,
            legend_enabled: false,
        }
    }
}

/// Result of label placement for the entire diagram.
#[derive(Debug, Clone)]
pub struct LabelPlacementResult {
    /// Placed edge labels.
    pub edge_labels: Vec<PlacedLabel>,
    /// Placed node labels (if repositioned from default).
    pub node_labels: Vec<PlacedLabel>,
    /// Collision resolution events (for JSONL logging).
    pub collisions: Vec<LabelCollisionEvent>,
    /// Labels that were spilled to a legend area.
    pub legend_labels: Vec<PlacedLabel>,
}

/// Measure text dimensions in world units with multi-line wrapping.
///
/// Returns `(width, height, was_truncated)`.
fn measure_text(text: &str, config: &LabelPlacementConfig) -> (f64, f64, bool) {
    if text.is_empty() {
        return (0.0, 0.0, false);
    }
    let max_cols_per_line = (config.max_label_width / config.char_width)
        .floor()
        .max(1.0) as usize;

    // Split on explicit newlines, then wrap each line by display width
    // (not character count) so CJK/wide characters are measured correctly.
    let mut lines: Vec<usize> = Vec::new(); // display width per wrapped line
    for raw_line in text.lines() {
        if raw_line.is_empty() {
            lines.push(0);
        } else {
            let mut line_width: usize = 0;
            for ch in raw_line.chars() {
                let mut buf = [0u8; 4];
                let ch_w = grapheme_width(ch.encode_utf8(&mut buf));
                if line_width + ch_w > max_cols_per_line && line_width > 0 {
                    lines.push(line_width);
                    line_width = ch_w;
                } else {
                    line_width += ch_w;
                }
            }
            lines.push(line_width);
        }
    }

    // Handle text that doesn't end with a newline but has content.
    if lines.is_empty() {
        lines.push(visual_width(text).min(max_cols_per_line));
    }

    let total_display_cols = visual_width(text);
    let was_truncated =
        lines.len() > config.max_lines || total_display_cols > max_cols_per_line * config.max_lines;

    // Truncate vertically.
    let visible_lines = lines.len().min(config.max_lines);
    let max_line_width = lines[..visible_lines].iter().copied().max().unwrap_or(0);

    let width = (max_line_width as f64 * config.char_width).min(config.max_label_width);
    let height = (visible_lines as f64 * config.line_height).min(config.max_label_height);

    (width, height, was_truncated)
}

/// Check if two rectangles overlap (with margin).
fn rects_overlap(a: &LayoutRect, b: &LayoutRect, margin: f64) -> bool {
    let ax1 = a.x - margin;
    let ay1 = a.y - margin;
    let ax2 = a.x + a.width + margin;
    let ay2 = a.y + a.height + margin;

    let bx1 = b.x - margin;
    let by1 = b.y - margin;
    let bx2 = b.x + b.width + margin;
    let by2 = b.y + b.height + margin;

    ax1 < bx2 && ax2 > bx1 && ay1 < by2 && ay2 > by1
}

/// Compute the midpoint of an edge path for label placement.
fn edge_midpoint(waypoints: &[LayoutPoint]) -> LayoutPoint {
    if waypoints.is_empty() {
        return LayoutPoint { x: 0.0, y: 0.0 };
    }
    if waypoints.len() == 1 {
        return waypoints[0];
    }

    // Find the midpoint along the path by total length.
    let mut total_len = 0.0;
    for w in waypoints.windows(2) {
        let dx = w[1].x - w[0].x;
        let dy = w[1].y - w[0].y;
        total_len += (dx * dx + dy * dy).sqrt();
    }

    let half = total_len / 2.0;
    let mut accumulated = 0.0;
    for w in waypoints.windows(2) {
        let dx = w[1].x - w[0].x;
        let dy = w[1].y - w[0].y;
        let seg_len = (dx * dx + dy * dy).sqrt();
        if accumulated + seg_len >= half && seg_len > 0.0 {
            let t = (half - accumulated) / seg_len;
            return LayoutPoint {
                x: w[0].x + dx * t,
                y: w[0].y + dy * t,
            };
        }
        accumulated += seg_len;
    }

    // Fallback: average of first and last.
    let first = waypoints[0];
    let last = waypoints[waypoints.len() - 1];
    LayoutPoint {
        x: (first.x + last.x) / 2.0,
        y: (first.y + last.y) / 2.0,
    }
}

/// Place all labels in a diagram, resolving collisions deterministically.
///
/// Returns placed labels with their final positions and collision events.
#[must_use]
/// Spatial grid for fast rectangle collision queries during label placement.
/// Divides the diagram area into uniform cells and tracks which occupied
/// rectangle indices fall in each cell. Reduces per-candidate collision
/// checks from O(n) to O(bucket_size).
struct LabelGrid {
    cells: Vec<Vec<usize>>,
    cols: usize,
    rows: usize,
    origin_x: f64,
    origin_y: f64,
    cell_w: f64,
    cell_h: f64,
}

impl LabelGrid {
    fn new(bbox: &LayoutRect, cell_size: f64) -> Self {
        let cell_size = cell_size.max(1.0);
        let cols = ((bbox.width / cell_size).ceil() as usize).max(1);
        let rows = ((bbox.height / cell_size).ceil() as usize).max(1);
        Self {
            cells: vec![Vec::new(); cols * rows],
            cols,
            rows,
            origin_x: bbox.x,
            origin_y: bbox.y,
            cell_w: cell_size,
            cell_h: cell_size,
        }
    }

    fn insert(&mut self, idx: usize, rect: &LayoutRect, margin: f64) {
        let x0 = ((rect.x - margin - self.origin_x) / self.cell_w)
            .floor()
            .max(0.0) as usize;
        let y0 = ((rect.y - margin - self.origin_y) / self.cell_h)
            .floor()
            .max(0.0) as usize;
        let x1 = (((rect.x + rect.width + margin - self.origin_x) / self.cell_w).ceil() as usize)
            .min(self.cols);
        let y1 = (((rect.y + rect.height + margin - self.origin_y) / self.cell_h).ceil() as usize)
            .min(self.rows);
        for cy in y0..y1 {
            for cx in x0..x1 {
                self.cells[cy * self.cols + cx].push(idx);
            }
        }
    }

    /// Return an iterator of occupied-rect indices that *might* overlap `rect`.
    /// Caller must still do the precise `rects_overlap` check.
    fn query_candidates(&self, rect: &LayoutRect, margin: f64) -> impl Iterator<Item = usize> + '_ {
        let x0 = ((rect.x - margin - self.origin_x) / self.cell_w)
            .floor()
            .max(0.0) as usize;
        let y0 = ((rect.y - margin - self.origin_y) / self.cell_h)
            .floor()
            .max(0.0) as usize;
        let x1 = (((rect.x + rect.width + margin - self.origin_x) / self.cell_w).ceil() as usize)
            .min(self.cols);
        let y1 = (((rect.y + rect.height + margin - self.origin_y) / self.cell_h).ceil() as usize)
            .min(self.rows);
        (y0..y1).flat_map(move |cy| {
            (x0..x1.min(self.cols))
                .flat_map(move |cx| self.cells[cy * self.cols + cx].iter().copied())
        })
    }
}

pub fn place_labels(
    ir: &MermaidDiagramIr,
    layout: &DiagramLayout,
    config: &LabelPlacementConfig,
) -> LabelPlacementResult {
    let mut edge_labels = Vec::new();
    let mut node_labels = Vec::new();
    let mut legend_labels = Vec::new();
    let mut collisions = Vec::new();

    // Collect all occupied rectangles: nodes first.
    let node_count = layout.nodes.len();
    let mut occupied: Vec<LayoutRect> = layout.nodes.iter().map(|n| n.rect).collect();

    // Add edge waypoint bounding boxes so labels avoid edge paths.
    let edge_occ_start = occupied.len();
    for edge_path in &layout.edges {
        let seg_rects = edge_segment_rects(&edge_path.waypoints, 0.5);
        occupied.extend(seg_rects);
    }
    let edge_occ_end = occupied.len();

    // Build spatial grid for fast collision queries. Cell size chosen to
    // roughly match typical label dimensions for good bucket distribution.
    let grid_bbox = LayoutRect {
        x: layout.bounding_box.x - config.max_offset - config.label_margin,
        y: layout.bounding_box.y - config.max_offset - config.label_margin,
        width: layout.bounding_box.width + 2.0 * (config.max_offset + config.label_margin) + 20.0,
        height: layout.bounding_box.height + 2.0 * (config.max_offset + config.label_margin) + 10.0,
    };
    let cell_size = 8.0; // ~2x typical label height for good distribution
    let mut grid = LabelGrid::new(&grid_bbox, cell_size);
    for (idx, occ) in occupied.iter().enumerate() {
        grid.insert(idx, occ, config.label_margin);
    }

    // Place node labels (use existing label_rect from layout, or compute).
    for node in &layout.nodes {
        if let Some(label_rect) = &node.label_rect {
            let label_id = ir.nodes[node.node_idx].label;
            if let Some(lid) = label_id {
                let text = ir.labels.get(lid.0).map_or("", |l| l.text.as_str());
                let (tw, th, was_truncated) = measure_text(text, config);

                let placed = PlacedLabel {
                    label_idx: lid.0,
                    rect: LayoutRect {
                        x: label_rect.x,
                        y: label_rect.y,
                        width: tw,
                        height: th,
                    },
                    was_offset: false,
                    was_truncated,
                    spilled_to_legend: false,
                    leader_line: None,
                };
                grid.insert(occupied.len(), &placed.rect, config.label_margin);
                occupied.push(placed.rect);
                node_labels.push(placed);
            }
        }
    }

    // Compute legend position (below bounding box).
    let legend_y = layout.bounding_box.y + layout.bounding_box.height + 2.0;
    let mut legend_x = layout.bounding_box.x;

    // Place edge labels at midpoints, resolving collisions.
    for edge_path in &layout.edges {
        if edge_path.edge_idx >= ir.edges.len() {
            continue;
        }
        let edge = &ir.edges[edge_path.edge_idx];
        let label_id = match edge.label {
            Some(lid) => lid,
            None => continue,
        };

        let text = ir.labels.get(label_id.0).map_or("", |l| l.text.as_str());
        if text.is_empty() {
            continue;
        }

        let (tw, th, was_truncated) = measure_text(text, config);

        // Initial position: edge midpoint.
        let mid = edge_midpoint(&edge_path.waypoints);
        let mut label_rect = LayoutRect {
            x: mid.x - tw / 2.0,
            y: mid.y - th / 2.0,
            width: tw,
            height: th,
        };

        // Check for collisions and offset if needed.
        let mut was_offset = false;
        let mut offset_applied = (0.0, 0.0);
        let mut collider = None;
        let mut placement_found = false;

        // Deterministic offset search: try offsets in a spiral pattern.
        let offsets = generate_offset_candidates(config.offset_step, config.max_offset);

        for &(dx, dy) in &offsets {
            let candidate = LayoutRect {
                x: label_rect.x + dx,
                y: label_rect.y + dy,
                ..label_rect
            };

            let mut collision_found = false;
            // Use spatial grid for fast candidate filtering instead of
            // scanning all occupied rectangles.
            for occ_idx in grid.query_candidates(&candidate, config.label_margin) {
                if let Some(occ) = occupied.get(occ_idx)
                    && rects_overlap(&candidate, occ, config.label_margin)
                {
                    if collider.is_none() {
                        collider = if occ_idx < node_count {
                            Some(LabelCollider::Node(occ_idx))
                        } else if occ_idx < edge_occ_end {
                            Some(LabelCollider::Edge(occ_idx - edge_occ_start))
                        } else {
                            Some(LabelCollider::Label(occ_idx - edge_occ_end))
                        };
                    }
                    collision_found = true;
                    break;
                }
            }

            if !collision_found {
                label_rect = candidate;
                if dx != 0.0 || dy != 0.0 {
                    was_offset = true;
                    offset_applied = (dx, dy);
                }
                placement_found = true;
                break;
            }
        }

        // Record collision event if offset was needed.
        if was_offset && let Some(c) = collider {
            collisions.push(LabelCollisionEvent {
                label_idx: label_id.0,
                collider: c,
                offset: offset_applied,
            });
        }

        // Legend spillover: if no valid placement found and legend enabled.
        if !placement_found && config.legend_enabled {
            let legend_rect = LayoutRect {
                x: legend_x,
                y: legend_y,
                width: tw,
                height: th,
            };
            legend_x += tw + config.label_margin * 2.0;

            let placed = PlacedLabel {
                label_idx: label_id.0,
                rect: legend_rect,
                was_offset: false,
                was_truncated,
                spilled_to_legend: true,
                leader_line: Some((
                    mid,
                    LayoutPoint {
                        x: legend_rect.x,
                        y: legend_rect.y,
                    },
                )),
            };
            grid.insert(occupied.len(), &placed.rect, config.label_margin);
            occupied.push(placed.rect);
            legend_labels.push(placed);
            continue;
        }

        // Compute leader line if offset distance exceeds threshold.
        let leader_line = if was_offset {
            let dist = (offset_applied.0.powi(2) + offset_applied.1.powi(2)).sqrt();
            if dist >= config.leader_line_threshold {
                Some((mid, label_rect.center()))
            } else {
                None
            }
        } else {
            None
        };

        let placed = PlacedLabel {
            label_idx: label_id.0,
            rect: label_rect,
            was_offset,
            was_truncated,
            spilled_to_legend: false,
            leader_line,
        };
        grid.insert(occupied.len(), &placed.rect, config.label_margin);
        occupied.push(placed.rect);
        edge_labels.push(placed);
    }

    LabelPlacementResult {
        edge_labels,
        node_labels,
        collisions,
        legend_labels,
    }
}

/// Generate deterministic offset candidates in a spiral pattern.
fn generate_offset_candidates(step: f64, max_offset: f64) -> Vec<(f64, f64)> {
    let mut offsets = vec![(0.0, 0.0)]; // Try no offset first.

    let mut dist = step;
    while dist <= max_offset {
        // Cardinal directions first (deterministic order).
        offsets.push((0.0, -dist)); // Up
        offsets.push((dist, 0.0)); // Right
        offsets.push((0.0, dist)); // Down
        offsets.push((-dist, 0.0)); // Left
        // Diagonals.
        offsets.push((dist, -dist));
        offsets.push((dist, dist));
        offsets.push((-dist, -dist));
        offsets.push((-dist, dist));
        dist += step;
    }
    offsets
}

/// Compute bounding boxes around edge waypoint segments for collision detection.
///
/// Each consecutive pair of waypoints produces a thin rectangle that prevents
/// labels from overlapping the edge path.
fn edge_segment_rects(waypoints: &[LayoutPoint], thickness: f64) -> Vec<LayoutRect> {
    waypoints
        .windows(2)
        .map(|w| {
            let min_x = w[0].x.min(w[1].x);
            let min_y = w[0].y.min(w[1].y);
            let max_x = w[0].x.max(w[1].x);
            let max_y = w[0].y.max(w[1].y);
            LayoutRect {
                x: min_x - thickness / 2.0,
                y: min_y - thickness / 2.0,
                width: (max_x - min_x) + thickness,
                height: (max_y - min_y) + thickness,
            }
        })
        .collect()
}

/// Collect label bounding boxes for routing grid reservation.
///
/// Returns rectangles that can be marked as obstacles in a [`RouteGrid`]
/// so that edges are routed around placed labels.
#[must_use]
pub fn label_reservation_rects(result: &LabelPlacementResult) -> Vec<LayoutRect> {
    let mut rects = Vec::with_capacity(result.node_labels.len() + result.edge_labels.len());
    for label in &result.node_labels {
        rects.push(label.rect);
    }
    for label in &result.edge_labels {
        rects.push(label.rect);
    }
    rects
}

// ── Legend / Footnote layout primitives (bd-1oa1y) ───────────────────

/// Placement strategy for the legend region relative to the diagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegendPlacement {
    /// Legend appears below the diagram bounding box.
    Below,
    /// Legend appears to the right of the diagram bounding box.
    Right,
}

/// Configuration for legend layout.
#[derive(Debug, Clone, Copy)]
pub struct LegendConfig {
    /// Where to place the legend relative to the diagram.
    pub placement: LegendPlacement,
    /// Maximum height (in world units) the legend may occupy.
    /// Entries beyond this height are truncated with an overflow indicator.
    pub max_height: f64,
    /// Maximum width (in world units) for the legend region.
    /// For Below: defaults to diagram width. For Right: fixed column width.
    pub max_width: f64,
    /// Gap between the diagram bounding box and the legend region.
    pub gap: f64,
    /// Padding inside the legend region.
    pub padding: f64,
    /// Character width in world units (for text measurement).
    pub char_width: f64,
    /// Line height in world units.
    pub line_height: f64,
    /// Maximum characters per legend entry before truncation.
    pub max_entry_chars: usize,
}

impl Default for LegendConfig {
    fn default() -> Self {
        Self {
            placement: LegendPlacement::Below,
            max_height: 10.0,
            max_width: 60.0,
            gap: 1.0,
            padding: 0.5,
            char_width: 1.0,
            line_height: 1.0,
            max_entry_chars: 56,
        }
    }
}

/// A single entry in the legend region.
#[derive(Debug, Clone, PartialEq)]
pub struct LegendEntry {
    /// Display text for this entry (e.g. "[1] https://example.com (Node A)").
    pub text: String,
    /// Bounding rectangle in world units.
    pub rect: LayoutRect,
    /// Whether the entry text was truncated.
    pub was_truncated: bool,
}

/// The computed legend region layout.
#[derive(Debug, Clone, PartialEq)]
pub struct LegendLayout {
    /// Bounding rectangle of the entire legend region.
    pub region: LayoutRect,
    /// Individual legend entries with positions.
    pub entries: Vec<LegendEntry>,
    /// How the legend is placed relative to the diagram.
    pub placement: LegendPlacement,
    /// Number of entries that were truncated due to max_height.
    pub overflow_count: usize,
}

impl LegendLayout {
    /// Returns true if the legend has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Compute the legend layout for link footnotes and spilled labels.
///
/// Takes the diagram bounding box, resolved links (from [`LinkResolution`]),
/// spilled labels, and config. Returns a deterministic layout that does not
/// overlap the diagram.
#[must_use]
pub fn compute_legend_layout(
    diagram_bbox: &LayoutRect,
    footnotes: &[String],
    config: &LegendConfig,
) -> LegendLayout {
    if footnotes.is_empty() {
        return LegendLayout {
            region: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            entries: Vec::new(),
            placement: config.placement,
            overflow_count: 0,
        };
    }

    // Compute legend origin based on placement.
    let (origin_x, origin_y, available_width) = match config.placement {
        LegendPlacement::Below => {
            let x = diagram_bbox.x;
            let y = diagram_bbox.y + diagram_bbox.height + config.gap;
            let w = config.max_width.min(diagram_bbox.width.max(20.0));
            (x, y, w)
        }
        LegendPlacement::Right => {
            let x = diagram_bbox.x + diagram_bbox.width + config.gap;
            let y = diagram_bbox.y;
            (x, y, config.max_width)
        }
    };

    let inner_width = available_width - config.padding * 2.0;
    let max_text_chars = (inner_width / config.char_width).floor().max(1.0) as usize;
    let max_text_chars = max_text_chars.min(config.max_entry_chars);

    let mut entries = Vec::new();
    let mut current_y = origin_y + config.padding;
    let max_y = origin_y + config.max_height;
    let mut overflow_count = 0;

    for footnote_text in footnotes {
        // Check if we've exceeded max height.
        if current_y + config.line_height > max_y {
            overflow_count = footnotes.len() - entries.len();
            break;
        }

        // Truncate entry text if needed.
        let (display_text, was_truncated) = truncate_legend_text(footnote_text, max_text_chars);

        let entry_rect = LayoutRect {
            x: origin_x + config.padding,
            y: current_y,
            width: display_text.chars().count() as f64 * config.char_width,
            height: config.line_height,
        };

        entries.push(LegendEntry {
            text: display_text,
            rect: entry_rect,
            was_truncated,
        });

        current_y += config.line_height;
    }

    // Compute actual region bounds.
    let actual_height = (current_y - origin_y) + config.padding;
    let actual_width =
        entries.iter().map(|e| e.rect.width).fold(0.0_f64, f64::max) + config.padding * 2.0;

    let region = LayoutRect {
        x: origin_x,
        y: origin_y,
        width: actual_width.min(available_width),
        height: actual_height.min(config.max_height),
    };

    LegendLayout {
        region,
        entries,
        placement: config.placement,
        overflow_count,
    }
}

/// Truncate a legend entry to fit within max display-width columns, adding ellipsis if needed.
fn truncate_legend_text(text: &str, max_cols: usize) -> (String, bool) {
    if max_cols == 0 {
        return (String::new(), !text.is_empty());
    }
    let width = visual_width(text);
    if width <= max_cols {
        return (text.to_string(), false);
    }
    // Reserve 1 column for ellipsis character (U+2026 is single-width)
    let budget = if max_cols > 1 { max_cols - 1 } else { max_cols };
    let mut result = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let mut buf = [0u8; 4];
        let ch_w = grapheme_width(ch.encode_utf8(&mut buf));
        if used + ch_w > budget {
            break;
        }
        result.push(ch);
        used += ch_w;
    }
    if max_cols > 1 {
        result.push('…');
    }
    (result, true)
}

/// Build footnote text lines from resolved links.
///
/// Each link produces a line like: `[1] https://example.com (Node A)`
/// Only allowed (non-blocked) links are included.
#[must_use]
pub fn build_link_footnotes(
    links: &[crate::mermaid::IrLink],
    nodes: &[crate::mermaid::IrNode],
) -> Vec<String> {
    let mut footnotes = Vec::new();
    let mut footnote_num = 1;

    for link in links {
        if link.sanitize_outcome != crate::mermaid::LinkSanitizeOutcome::Allowed {
            continue;
        }

        let node_label = nodes
            .get(link.target.0)
            .map(|n| n.id.as_str())
            .unwrap_or("?");

        let line = if let Some(tip) = &link.tooltip {
            format!("[{}] {} ({} - {})", footnote_num, link.url, node_label, tip)
        } else {
            format!("[{}] {} ({})", footnote_num, link.url, node_label)
        };

        footnotes.push(line);
        footnote_num += 1;
    }

    footnotes
}

/// Emit legend layout metrics to JSONL evidence log.
pub fn emit_legend_jsonl(config: &MermaidConfig, legend: &LegendLayout) {
    let Some(path) = config.log_path.as_deref() else {
        return;
    };
    if legend.is_empty() {
        return;
    }
    let json = serde_json::json!({
        "event": "mermaid_legend",
        "legend_mode": match legend.placement {
            LegendPlacement::Below => "below",
            LegendPlacement::Right => "right",
        },
        "legend_height": legend.region.height,
        "legend_width": legend.region.width,
        "legend_lines": legend.entries.len(),
        "overflow_count": legend.overflow_count,
    });
    let line = json.to_string();
    let _ = crate::mermaid::append_jsonl_line(path, &line);
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mermaid::*;
    use std::collections::BTreeMap;

    fn default_config() -> MermaidConfig {
        MermaidConfig::default()
    }

    fn empty_span() -> Span {
        Span {
            start: Position {
                line: 0,
                col: 0,
                byte: 0,
            },
            end: Position {
                line: 0,
                col: 0,
                byte: 0,
            },
        }
    }

    fn empty_guard_report() -> MermaidGuardReport {
        MermaidGuardReport {
            complexity: MermaidComplexity {
                nodes: 0,
                edges: 0,
                labels: 0,
                clusters: 0,
                ports: 0,
                style_refs: 0,
                score: 0,
            },
            label_chars_over: 0,
            label_lines_over: 0,
            node_limit_exceeded: false,
            edge_limit_exceeded: false,
            label_limit_exceeded: false,
            route_budget_exceeded: false,
            layout_budget_exceeded: false,
            limits_exceeded: false,
            budget_exceeded: false,
            route_ops_estimate: 0,
            layout_iterations_estimate: 0,
            degradation: MermaidDegradationPlan {
                target_fidelity: MermaidFidelity::Rich,
                hide_labels: false,
                collapse_clusters: false,
                simplify_routing: false,
                reduce_decoration: false,
                force_glyph_mode: None,
            },
        }
    }

    fn empty_init_parse() -> MermaidInitParse {
        MermaidInitParse {
            config: MermaidInitConfig {
                theme: None,
                theme_variables: BTreeMap::new(),
                flowchart_direction: None,
            },
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub(super) fn make_simple_ir(
        nodes: &[&str],
        edges: &[(usize, usize)],
        direction: GraphDirection,
    ) -> MermaidDiagramIr {
        let ir_nodes: Vec<IrNode> = nodes
            .iter()
            .map(|id| IrNode {
                id: id.to_string(),
                label: None,
                shape: NodeShape::Rect,
                classes: vec![],
                style_ref: None,
                span_primary: empty_span(),
                span_all: vec![],
                implicit: false,
                members: vec![],
            })
            .collect();

        let ir_edges: Vec<IrEdge> = edges
            .iter()
            .map(|(from, to)| IrEdge {
                from: IrEndpoint::Node(IrNodeId(*from)),
                to: IrEndpoint::Node(IrNodeId(*to)),
                arrow: "-->".to_string(),
                label: None,
                style_ref: None,
                span: empty_span(),
            })
            .collect();

        MermaidDiagramIr {
            diagram_type: DiagramType::Graph,
            direction,
            nodes: ir_nodes,
            edges: ir_edges,
            ports: vec![],
            clusters: vec![],
            labels: vec![],
            pie_entries: vec![],
            pie_title: None,
            pie_show_data: false,
            style_refs: vec![],
            links: vec![],
            meta: MermaidDiagramMeta {
                diagram_type: DiagramType::Graph,
                direction,
                support_level: MermaidSupportLevel::Supported,
                init: empty_init_parse(),
                theme_overrides: MermaidThemeOverrides {
                    theme: None,
                    theme_variables: BTreeMap::new(),
                },
                guard: empty_guard_report(),
            },
            constraints: vec![],
        }
    }
    fn count_crossings_bruteforce(
        rank_a: &[usize],
        rank_b: &[usize],
        graph: &LayoutGraph,
    ) -> usize {
        let mut pos_b = vec![usize::MAX; graph.n];
        let mut in_b = vec![false; graph.n];
        for (i, &v) in rank_b.iter().enumerate() {
            pos_b[v] = i;
            in_b[v] = true;
        }

        let mut edges: Vec<(usize, usize)> = Vec::new();
        for (i, &u) in rank_a.iter().enumerate() {
            for &v in &graph.adj[u] {
                if in_b[v] {
                    edges.push((i, pos_b[v]));
                }
            }
        }

        let mut crossings = 0usize;
        for i in 0..edges.len() {
            for j in (i + 1)..edges.len() {
                let (a1, b1) = edges[i];
                let (a2, b2) = edges[j];
                if (a1 < a2 && b1 > b2) || (a1 > a2 && b1 < b2) {
                    crossings += 1;
                }
            }
        }
        crossings
    }

    #[test]
    fn empty_diagram_produces_empty_layout() {
        let ir = make_simple_ir(&[], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        assert!(layout.nodes.is_empty());
        assert!(layout.edges.is_empty());
        assert!(layout.clusters.is_empty());
        assert_eq!(layout.stats.ranks, 0);
        assert!(!layout.stats.budget_exceeded);
    }

    #[test]
    fn single_node_layout() {
        let ir = make_simple_ir(&["A"], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        assert_eq!(layout.nodes.len(), 1);
        assert_eq!(layout.nodes[0].rank, 0);
        assert_eq!(layout.nodes[0].order, 0);
        assert!(layout.nodes[0].rect.width > 0.0);
        assert!(layout.nodes[0].rect.height > 0.0);
    }

    #[test]
    fn linear_chain_tb() {
        // A → B → C should produce ranks 0, 1, 2 in TB direction.
        let ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());

        assert_eq!(layout.nodes.len(), 3);
        assert_eq!(layout.stats.ranks, 3);

        // In TB: higher rank = further down (higher y).
        assert!(layout.nodes[0].rect.y < layout.nodes[1].rect.y);
        assert!(layout.nodes[1].rect.y < layout.nodes[2].rect.y);
    }

    #[test]
    fn linear_chain_lr() {
        // A → B → C in LR should go left to right.
        let ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::LR);
        let layout = layout_diagram(&ir, &default_config());

        // In LR: higher rank = further right (higher x).
        assert!(layout.nodes[0].rect.x < layout.nodes[1].rect.x);
        assert!(layout.nodes[1].rect.x < layout.nodes[2].rect.x);
    }

    #[test]
    fn linear_chain_bt() {
        // A → B → C in BT should go bottom to top.
        let ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::BT);
        let layout = layout_diagram(&ir, &default_config());

        // In BT: rank 0 (A) should be at the bottom (highest y).
        assert!(layout.nodes[0].rect.y > layout.nodes[1].rect.y);
        assert!(layout.nodes[1].rect.y > layout.nodes[2].rect.y);
    }

    #[test]
    fn linear_chain_rl() {
        let ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::RL);
        let layout = layout_diagram(&ir, &default_config());

        // In RL: rank 0 (A) should be at the right (highest x).
        assert!(layout.nodes[0].rect.x > layout.nodes[1].rect.x);
        assert!(layout.nodes[1].rect.x > layout.nodes[2].rect.x);
    }

    #[test]
    fn diamond_graph_no_overlap() {
        //     A
        //    / \
        //   B   C
        //    \ /
        //     D
        let ir = make_simple_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (0, 2), (1, 3), (2, 3)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());

        assert_eq!(layout.nodes.len(), 4);
        // A at rank 0, B/C at rank 1, D at rank 2.
        assert_eq!(layout.nodes[0].rank, 0);
        assert_eq!(layout.nodes[1].rank, 1);
        assert_eq!(layout.nodes[2].rank, 1);
        assert_eq!(layout.nodes[3].rank, 2);

        // B and C should not overlap.
        let b_rect = &layout.nodes[1].rect;
        let c_rect = &layout.nodes[2].rect;
        let no_overlap = b_rect.x + b_rect.width <= c_rect.x || c_rect.x + c_rect.width <= b_rect.x;
        assert!(no_overlap, "B and C should not overlap horizontally");
    }

    #[test]
    fn layout_is_deterministic() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D", "E"],
            &[(0, 1), (0, 2), (1, 3), (2, 3), (3, 4)],
            GraphDirection::TB,
        );
        let layout1 = layout_diagram(&ir, &default_config());
        let layout2 = layout_diagram(&ir, &default_config());

        // Identical inputs must produce identical outputs.
        assert_eq!(layout1.nodes, layout2.nodes);
        assert_eq!(layout1.edges, layout2.edges);
        assert_eq!(layout1.stats, layout2.stats);
    }

    #[test]
    fn edges_have_waypoints() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());

        assert_eq!(layout.edges.len(), 1);
        assert_eq!(
            layout.edges[0].waypoints.len(),
            2,
            "simple edge should have 2 waypoints (source port, target port)"
        );
    }

    #[test]
    fn edge_bundling_bundles_parallel_cluster_edges() {
        let ir = make_named_clustered_ir(
            &["A", "B", "C", "D"],
            &[(0, 2), (0, 3), (1, 2), (1, 3), (0, 2)],
            &[("g0", &[0, 1]), ("g1", &[2, 3])],
            GraphDirection::TB,
        );

        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 3,
            ..MermaidConfig::default()
        };

        let layout = layout_diagram(&ir, &config);
        assert_eq!(
            layout.edges.len(),
            1,
            "expected bundled edge to replace parallel set"
        );
        assert_eq!(
            layout.edges[0].edge_idx, 0,
            "canonical edge should be the lowest idx"
        );
        assert_eq!(layout.edges[0].bundle_count, 5);
        assert_eq!(layout.edges[0].bundle_members, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn edge_bundling_offsets_pair_when_below_min_bundle() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1), (0, 1)], GraphDirection::TB);
        let spacing = LayoutSpacing::default();

        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 3,
            ..MermaidConfig::default()
        };

        let layout = layout_diagram_with_spacing(&ir, &config, &spacing);
        assert_eq!(layout.edges.len(), 2);
        assert_ne!(layout.edges[0].waypoints, layout.edges[1].waypoints);

        // Vertical graph directions offset in X.
        let delta = (spacing.node_gap * 0.4).clamp(0.6, 1.2);
        let dx = (layout.edges[1].waypoints[0].x - layout.edges[0].waypoints[0].x).abs();
        assert!(
            (dx - 2.0 * delta).abs() < 1e-9,
            "expected symmetric ±delta offsets"
        );
        assert_eq!(
            layout.edges[0].waypoints[0].y, layout.edges[1].waypoints[0].y,
            "offset should not affect Y for TB/TD/BT"
        );
    }

    #[test]
    fn cluster_bounds_contain_members() {
        let mut ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::TB);
        ir.clusters.push(IrCluster {
            id: IrClusterId(0),
            title: None,
            members: vec![IrNodeId(0), IrNodeId(1)],
            span: Span {
                start: Position {
                    line: 0,
                    col: 0,
                    byte: 0,
                },
                end: Position {
                    line: 0,
                    col: 0,
                    byte: 0,
                },
            },
        });

        let layout = layout_diagram(&ir, &default_config());

        assert_eq!(layout.clusters.len(), 1);
        let cluster_rect = &layout.clusters[0].rect;

        // Cluster must contain both A and B.
        let a_center = layout.nodes[0].rect.center();
        let b_center = layout.nodes[1].rect.center();
        assert!(
            cluster_rect.contains_point(a_center),
            "cluster should contain node A"
        );
        assert!(
            cluster_rect.contains_point(b_center),
            "cluster should contain node B"
        );
    }

    #[test]
    fn bounding_box_contains_all_nodes() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (0, 2), (1, 3), (2, 3)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());

        for node in &layout.nodes {
            let center = node.rect.center();
            assert!(
                layout.bounding_box.contains_point(center),
                "bounding box should contain node {}",
                node.node_idx
            );
        }
    }

    #[test]
    fn budget_limit_produces_degradation() {
        // Large-ish graph with iteration budget of 1.
        let ir = make_simple_ir(
            &["A", "B", "C", "D", "E", "F", "G", "H"],
            &[
                (0, 2),
                (0, 3),
                (1, 2),
                (1, 3),
                (2, 4),
                (2, 5),
                (3, 4),
                (3, 5),
                (4, 6),
                (4, 7),
                (5, 6),
                (5, 7),
            ],
            GraphDirection::TB,
        );

        let mut config = default_config();
        config.layout_iteration_budget = 1;

        let layout = layout_diagram(&ir, &config);
        // With budget=1, the algorithm should still produce a valid layout
        // (it may or may not exceed the budget depending on convergence).
        assert_eq!(layout.nodes.len(), 8);
        assert!(layout.stats.iterations_used <= 2);
    }

    #[test]
    fn disconnected_nodes_get_rank_zero() {
        // All disconnected nodes should be at rank 0.
        let ir = make_simple_ir(&["A", "B", "C"], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());

        for node in &layout.nodes {
            assert_eq!(node.rank, 0, "disconnected node should be rank 0");
        }
    }

    #[test]
    fn parallel_edges_same_rank() {
        //   A → C
        //   B → C
        // A and B should both be rank 0, C at rank 1.
        let ir = make_simple_ir(&["A", "B", "C"], &[(0, 2), (1, 2)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());

        assert_eq!(layout.nodes[0].rank, 0);
        assert_eq!(layout.nodes[1].rank, 0);
        assert_eq!(layout.nodes[2].rank, 1);
    }

    // =========================================================================
    // Objective scoring tests
    // =========================================================================

    #[test]
    fn objective_empty_layout() {
        let ir = make_simple_ir(&[], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let obj = evaluate_layout(&layout);
        assert_eq!(obj.crossings, 0);
        assert_eq!(obj.bends, 0);
        assert_eq!(obj.position_variance, 0.0);
        assert_eq!(obj.total_edge_length, 0.0);
    }

    #[test]
    fn objective_linear_chain_has_no_crossings() {
        let ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let obj = evaluate_layout(&layout);
        assert_eq!(obj.crossings, 0);
        assert_eq!(obj.bends, 0);
        assert!(obj.total_edge_length > 0.0);
    }

    #[test]
    fn objective_diamond_scores_lower_than_worst_case() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (0, 2), (1, 3), (2, 3)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let obj = evaluate_layout(&layout);
        assert_eq!(obj.crossings, 0);
        assert!(obj.score.is_finite());
    }

    #[test]
    fn objective_score_is_deterministic() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D", "E"],
            &[(0, 1), (0, 2), (1, 3), (2, 3), (3, 4)],
            GraphDirection::TB,
        );
        let layout1 = layout_diagram(&ir, &default_config());
        let layout2 = layout_diagram(&ir, &default_config());
        let obj1 = evaluate_layout(&layout1);
        let obj2 = evaluate_layout(&layout2);
        assert_eq!(obj1.score, obj2.score);
        assert_eq!(obj1.crossings, obj2.crossings);
    }

    // =========================================================================
    // Aesthetic metrics tests (bd-19cll)
    // =========================================================================

    #[test]
    fn symmetry_single_node_is_perfect() {
        let ir = make_simple_ir(&["A"], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let obj = evaluate_layout(&layout);
        assert!(
            obj.symmetry >= 0.99,
            "single node should be ~1.0, got {}",
            obj.symmetry
        );
    }

    #[test]
    fn symmetry_balanced_tree_is_high() {
        let ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (0, 2)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let obj = evaluate_layout(&layout);
        assert!(
            obj.symmetry > 0.5,
            "balanced tree should have symmetry > 0.5, got {}",
            obj.symmetry
        );
    }

    #[test]
    fn compactness_bounded_zero_to_one() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D", "E"],
            &[(0, 2), (1, 2), (2, 3), (2, 4)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let obj = evaluate_layout(&layout);
        assert!(
            (0.0..=1.0).contains(&obj.compactness),
            "compactness should be in [0,1], got {}",
            obj.compactness
        );
    }

    #[test]
    fn edge_length_variance_uniform_chain_is_low() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (1, 2), (2, 3)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let obj = evaluate_layout(&layout);
        // Uniform chain: all edges similar length ⇒ low variance.
        assert!(
            obj.edge_length_variance < 5.0,
            "uniform chain variance should be low, got {}",
            obj.edge_length_variance
        );
    }

    #[test]
    fn weight_presets_produce_different_scores() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D", "E"],
            &[(0, 1), (0, 2), (1, 3), (2, 3), (3, 4)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let obj = evaluate_layout(&layout);

        let sn = obj.compute_score_with(&AestheticWeights::normal());
        let sc = obj.compute_score_with(&AestheticWeights::compact());
        let sr = obj.compute_score_with(&AestheticWeights::rich());

        assert!(sn.is_finite());
        assert!(sc.is_finite());
        assert!(sr.is_finite());
        // Different presets should produce distinct scores (unless the layout
        // happens to sit at a degenerate point, which is unlikely for 5 nodes).
        assert!(
            !(sn == sc && sc == sr),
            "all presets produced the same score: {}",
            sn
        );
    }

    #[test]
    fn compare_layouts_breakdown_length() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let obj = evaluate_layout(&layout);
        let cmp = compare_layouts(&obj, &obj, &AestheticWeights::default());
        assert_eq!(cmp.breakdown.len(), 9, "should have 9 metric entries");
        assert!(
            cmp.delta.abs() < f64::EPSILON,
            "same layout should have zero delta"
        );
    }

    #[test]
    fn compare_layouts_detects_improvement() {
        let mut a = evaluate_layout(&layout_diagram(
            &make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB),
            &default_config(),
        ));
        let mut b = a.clone();
        // Simulate B having fewer crossings.
        a.crossings = 5;
        b.crossings = 0;
        a.score = a.compute_score();
        b.score = b.compute_score();

        let w = AestheticWeights::normal();
        let cmp = compare_layouts(&a, &b, &w);
        assert!(
            cmp.delta > 0.0,
            "B should be better (positive delta), got {}",
            cmp.delta
        );
    }

    #[test]
    fn evaluate_with_labels_includes_collision_penalty() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let obj_clean = evaluate_layout(&layout);
        let obj_dirty = evaluate_layout_with_labels(&layout, 5);

        assert_eq!(obj_clean.label_collisions, 0);
        assert_eq!(obj_dirty.label_collisions, 5);
        assert!(
            obj_dirty.score > obj_clean.score,
            "collisions should increase score"
        );
    }

    #[test]
    fn new_metrics_are_deterministic() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D", "E"],
            &[(0, 1), (0, 2), (1, 3), (2, 3), (3, 4)],
            GraphDirection::TB,
        );
        let l1 = layout_diagram(&ir, &default_config());
        let l2 = layout_diagram(&ir, &default_config());
        let o1 = evaluate_layout(&l1);
        let o2 = evaluate_layout(&l2);
        assert_eq!(o1.symmetry, o2.symmetry);
        assert_eq!(o1.compactness, o2.compactness);
        assert_eq!(o1.edge_length_variance, o2.edge_length_variance);
        assert_eq!(o1.score, o2.score);
    }

    #[test]
    fn emit_layout_metrics_writes_jsonl() {
        let dir = std::env::temp_dir().join("ftui_test_layout_metrics_jsonl");
        let _ = std::fs::remove_file(&dir);
        let log_path = dir.to_str().unwrap().to_string();

        let mut config = default_config();
        config.log_path = Some(log_path.clone());

        let ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::TB);
        let _layout = layout_diagram(&ir, &config);

        let content = std::fs::read_to_string(&log_path).expect("jsonl file should exist");
        let _ = std::fs::remove_file(&dir);

        let lines: Vec<&str> = content.lines().collect();
        assert!(!lines.is_empty(), "should have at least one JSONL line");
        let parsed: serde_json::Value =
            serde_json::from_str(lines[0]).expect("line should be valid JSON");
        assert_eq!(parsed["event"], "layout_metrics");
        assert_eq!(parsed["nodes"], 3);
        assert_eq!(parsed["edges"], 2);
        assert!(parsed["score_normal"].is_number());
        assert!(parsed["score_compact"].is_number());
        assert!(parsed["score_rich"].is_number());
        assert!(parsed["symmetry"].is_number());
        assert!(parsed["compactness"].is_number());
    }

    // =========================================================================
    // Compaction tests
    // =========================================================================

    #[test]
    fn compaction_preserves_no_overlap() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D"],
            &[(0, 2), (0, 3), (1, 2), (1, 3)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());

        for i in 0..layout.nodes.len() {
            for j in (i + 1)..layout.nodes.len() {
                if layout.nodes[i].rank == layout.nodes[j].rank {
                    let ri = &layout.nodes[i].rect;
                    let rj = &layout.nodes[j].rect;
                    let no_overlap =
                        ri.x + ri.width <= rj.x + 0.01 || rj.x + rj.width <= ri.x + 0.01;
                    assert!(
                        no_overlap,
                        "nodes {} and {} in rank {} overlap",
                        i, j, layout.nodes[i].rank,
                    );
                }
            }
        }
    }

    #[test]
    fn compaction_preserves_determinism() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D", "E"],
            &[(0, 2), (1, 2), (2, 3), (2, 4)],
            GraphDirection::TB,
        );
        let l1 = layout_diagram(&ir, &default_config());
        let l2 = layout_diagram(&ir, &default_config());
        assert_eq!(l1.nodes, l2.nodes);
    }

    // =========================================================================
    // RouteGrid tests
    // =========================================================================

    #[test]
    fn route_grid_from_empty_layout() {
        let ir = make_simple_ir(&["A"], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let grid =
            RouteGrid::from_layout(&layout.nodes, &layout.clusters, &layout.bounding_box, 1.0);
        assert!(grid.cols > 0);
        assert!(grid.rows > 0);
    }

    #[test]
    fn route_grid_find_path_same_point() {
        let ir = make_simple_ir(&["A"], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let grid =
            RouteGrid::from_layout(&layout.nodes, &layout.clusters, &layout.bounding_box, 1.0);
        let p = LayoutPoint { x: 5.0, y: 1.5 };
        let path = grid.find_path(p, p);
        assert_eq!(path.len(), 2);
    }

    #[test]
    fn route_grid_roundtrip_cell_center() {
        let grid = RouteGrid {
            cols: 10,
            rows: 8,
            cell_size: 2.0,
            origin: LayoutPoint { x: 0.0, y: 0.0 },
            occupied: vec![false; 80],
        };
        let pt = grid.to_world(4, 6);
        let (c, r) = grid.to_grid(pt);
        assert_eq!((c, r), (4, 6));
    }

    #[test]
    fn route_grid_snap_to_free_avoids_occupied_cell() {
        let mut grid = RouteGrid {
            cols: 3,
            rows: 3,
            cell_size: 1.0,
            origin: LayoutPoint { x: 0.0, y: 0.0 },
            occupied: vec![false; 9],
        };
        grid.occupied[4] = true; // center cell

        let (c, r) = grid.snap_to_free(1, 1);
        assert_ne!((c, r), (1, 1));
        assert!(grid.is_free(c, r));
    }

    #[test]
    fn route_grid_finds_path_between_nodes() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let grid =
            RouteGrid::from_layout(&layout.nodes, &layout.clusters, &layout.bounding_box, 1.0);

        let from = layout.nodes[0].rect.center();
        let to = layout.nodes[1].rect.center();
        let path = grid.find_path(from, to);

        assert!(path.len() >= 2, "path should have at least start and end");
        assert!((path[0].x - from.x).abs() < 0.01);
        assert!((path[0].y - from.y).abs() < 0.01);
        let last = path.last().unwrap();
        assert!((last.x - to.x).abs() < 0.01);
        assert!((last.y - to.y).abs() < 0.01);
    }

    // =========================================================================
    // Invariant tests
    // =========================================================================

    #[test]
    fn invariant_no_overlaps_wide_graph() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D", "E", "F"],
            &[
                (0, 1),
                (0, 2),
                (0, 3),
                (0, 4),
                (1, 5),
                (2, 5),
                (3, 5),
                (4, 5),
            ],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());

        for i in 0..layout.nodes.len() {
            for j in (i + 1)..layout.nodes.len() {
                if layout.nodes[i].rank == layout.nodes[j].rank {
                    let ri = &layout.nodes[i].rect;
                    let rj = &layout.nodes[j].rect;
                    let no_h_overlap =
                        ri.x + ri.width <= rj.x + 0.01 || rj.x + rj.width <= ri.x + 0.01;
                    assert!(no_h_overlap, "overlap between node {} and {}", i, j);
                }
            }
        }
    }

    #[test]
    fn invariant_stable_ordering_equal_cost() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D", "E", "F", "G"],
            &[(0, 3), (1, 3), (2, 3), (3, 4), (3, 5), (3, 6)],
            GraphDirection::TB,
        );
        let l1 = layout_diagram(&ir, &default_config());
        let l2 = layout_diagram(&ir, &default_config());

        for (n1, n2) in l1.nodes.iter().zip(l2.nodes.iter()) {
            assert_eq!(
                n1.order, n2.order,
                "ordering unstable for node {}",
                n1.node_idx
            );
            assert_eq!(n1.rank, n2.rank, "rank unstable for node {}", n1.node_idx);
        }
    }

    #[test]
    fn invariant_all_directions_produce_valid_layout() {
        let directions = [
            GraphDirection::TB,
            GraphDirection::TD,
            GraphDirection::BT,
            GraphDirection::LR,
            GraphDirection::RL,
        ];

        for dir in &directions {
            let ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], *dir);
            let layout = layout_diagram(&ir, &default_config());
            assert_eq!(layout.nodes.len(), 3);
            assert!(layout.bounding_box.width > 0.0);
            assert!(layout.bounding_box.height > 0.0);
        }
    }

    #[test]
    fn custom_spacing_affects_layout() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let config = default_config();

        let tight = LayoutSpacing {
            rank_gap: 1.0,
            node_gap: 1.0,
            ..Default::default()
        };
        let wide = LayoutSpacing {
            rank_gap: 20.0,
            node_gap: 20.0,
            ..Default::default()
        };

        let l_tight = layout_diagram_with_spacing(&ir, &config, &tight);
        let l_wide = layout_diagram_with_spacing(&ir, &config, &wide);

        assert!(l_wide.bounding_box.height > l_tight.bounding_box.height);
    }

    #[test]
    fn layout_handles_self_loop_gracefully() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 0), (0, 1)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        assert_eq!(layout.nodes.len(), 2);
    }

    #[test]
    fn layout_handles_cycle_gracefully() {
        let ir = make_simple_ir(
            &["A", "B", "C"],
            &[(0, 1), (1, 2), (2, 0)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        assert_eq!(layout.nodes.len(), 3);
        assert!(layout.bounding_box.width > 0.0);
    }

    #[test]
    fn count_crossings_matches_bruteforce() {
        let ir = make_simple_ir(&["A", "B", "C", "D"], &[(0, 3), (1, 2)], GraphDirection::TB);
        let graph = LayoutGraph::from_ir(&ir);
        let rank_a = vec![0, 1];
        let rank_b = vec![2, 3];
        let fast = count_crossings(&rank_a, &rank_b, &graph);
        let slow = count_crossings_bruteforce(&rank_a, &rank_b, &graph);
        assert_eq!(fast, slow, "crossing count must match brute force");
        assert_eq!(fast, 1, "expected exactly one crossing");
    }

    #[test]
    fn layout_large_graph_stays_within_budget() {
        let node_names: Vec<String> = (0..20).map(|i| format!("N{i}")).collect();
        let node_refs: Vec<&str> = node_names.iter().map(String::as_str).collect();

        let edges: Vec<(usize, usize)> = (0..19).map(|i| (i, i + 1)).collect();

        let ir = make_simple_ir(&node_refs, &edges, GraphDirection::TB);
        let mut config = default_config();
        config.layout_iteration_budget = 50;

        let layout = layout_diagram(&ir, &config);
        assert_eq!(layout.nodes.len(), 20);
        assert!(
            layout.stats.iterations_used <= 50,
            "iterations {} exceeded budget 50",
            layout.stats.iterations_used
        );
    }

    #[test]
    #[ignore = "perf harness: run manually for profiling"]
    fn perf_large_graph_layout() {
        // Heavier deterministic workload for profiling layout hot paths.
        // Run with:
        // cargo test -p ftui-extras --features diagram perf_large_graph_layout --release -- --ignored --nocapture
        let node_count = 300usize;
        let names: Vec<String> = (0..node_count).map(|i| format!("N{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();

        let mut edges: Vec<(usize, usize)> = Vec::new();
        for i in 0..node_count {
            if i + 1 < node_count {
                edges.push((i, i + 1));
            }
            if i + 2 < node_count {
                edges.push((i, i + 2));
            }
            if i + 10 < node_count {
                edges.push((i, i + 10));
            }
            let mirror = node_count - 1 - i;
            if i < mirror {
                edges.push((i, mirror));
            }
        }
        edges.sort_unstable();
        edges.dedup();

        let ir = make_simple_ir(&refs, &edges, GraphDirection::TB);
        let mut config = default_config();
        config.layout_iteration_budget = 200;

        const GOLDEN_CHECKSUM: usize = 0x0000_0000_004d_ff30;
        let mut checksum = 0usize;
        let region = stats_alloc::Region::new(crate::GLOBAL);
        for _ in 0..200 {
            let layout = layout_diagram(&ir, &config);
            checksum = checksum.wrapping_add(layout.nodes.len());
            checksum = checksum.wrapping_add(layout.stats.crossings);
            checksum = checksum.wrapping_add(layout.stats.total_bends);
        }
        let stats = region.change();
        eprintln!(
            "perf_large_graph_layout checksum=0x{checksum:016x} allocs={} bytes_allocated={} reallocs={} deallocs={} bytes_deallocated={} bytes_reallocated={}",
            stats.allocations,
            stats.bytes_allocated,
            stats.reallocations,
            stats.deallocations,
            stats.bytes_deallocated,
            stats.bytes_reallocated
        );
        assert_eq!(
            checksum, GOLDEN_CHECKSUM,
            "golden checksum changed; update if layout output intentionally changed"
        );
        std::hint::black_box((
            checksum,
            stats.allocations,
            stats.bytes_allocated,
            stats.reallocations,
            stats.deallocations,
            stats.bytes_deallocated,
            stats.bytes_reallocated,
        ));
    }

    fn make_dense_rank_ir(
        rank_count: usize,
        per_rank: usize,
    ) -> (MermaidDiagramIr, Vec<Vec<usize>>) {
        let node_count = rank_count.saturating_mul(per_rank);
        let names: Vec<String> = (0..node_count).map(|i| format!("N{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();

        let mut edges = Vec::with_capacity(per_rank * per_rank * rank_count.saturating_sub(1));
        for r in 0..rank_count.saturating_sub(1) {
            let base_a = r * per_rank;
            let base_b = (r + 1) * per_rank;
            for i in 0..per_rank {
                let from = base_a + i;
                for j in 0..per_rank {
                    edges.push((from, base_b + j));
                }
            }
        }

        let ir = make_simple_ir(&refs, &edges, GraphDirection::TB);
        let mut rank_order = Vec::with_capacity(rank_count);
        for r in 0..rank_count {
            rank_order.push((r * per_rank..(r + 1) * per_rank).collect());
        }
        (ir, rank_order)
    }

    #[test]
    #[ignore = "perf harness: run manually for profiling"]
    fn perf_crossings_dense_full() {
        // Dense multi-rank workload to stress Fenwick crossing counting.
        // Run with:
        // cargo test -p ftui-extras --features diagram perf_crossings_dense_full --release -- --ignored --nocapture
        let (ir, rank_order) = make_dense_rank_ir(6, 200);
        let graph = LayoutGraph::from_ir(&ir);

        let mut checksum = 0usize;
        for _ in 0..25 {
            checksum = checksum.wrapping_add(total_crossings(&rank_order, &graph));
        }
        std::hint::black_box(checksum);
    }

    #[test]
    #[ignore = "perf harness: run manually for profiling"]
    fn perf_crossings_dense_early_exit() {
        // Early-exit variant: uses a small limit so later rank pairs are skipped.
        // Run with:
        // cargo test -p ftui-extras --features diagram perf_crossings_dense_early_exit --release -- --ignored --nocapture
        let (ir, rank_order) = make_dense_rank_ir(6, 200);
        let graph = LayoutGraph::from_ir(&ir);
        let limit = 5_000;

        let mut checksum = 0usize;
        for _ in 0..25 {
            checksum =
                checksum.wrapping_add(total_crossings_with_limit(&rank_order, &graph, limit));
        }
        std::hint::black_box(checksum);
    }

    // ── A* routing tests ─────────────────────────────────────────────

    #[test]
    fn astar_same_point_returns_direct() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let grid =
            RouteGrid::from_layout(&layout.nodes, &layout.clusters, &layout.bounding_box, 3.0);
        let pt = layout.nodes[0].rect.center();
        let (waypoints, diag) = grid.find_path_astar(pt, pt, &RoutingWeights::default(), &[]);
        assert_eq!(waypoints.len(), 2);
        assert_eq!(diag.bends, 0);
        assert!(!diag.fallback);
    }

    #[test]
    fn astar_finds_path_between_nodes() {
        let ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let grid =
            RouteGrid::from_layout(&layout.nodes, &layout.clusters, &layout.bounding_box, 2.0);

        let from = layout.nodes[0].rect.center();
        let to = layout.nodes[2].rect.center();
        let (waypoints, diag) = grid.find_path_astar(from, to, &RoutingWeights::default(), &[]);

        assert!(waypoints.len() >= 2, "should have at least start and end");
        assert!(!diag.fallback, "should find a path without fallback");
    }

    #[test]
    fn astar_routing_is_deterministic() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (0, 2), (1, 3), (2, 3)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let grid =
            RouteGrid::from_layout(&layout.nodes, &layout.clusters, &layout.bounding_box, 2.0);

        let from = layout.nodes[0].rect.center();
        let to = layout.nodes[3].rect.center();
        let weights = RoutingWeights::default();

        let (wp1, d1) = grid.find_path_astar(from, to, &weights, &[]);
        let (wp2, d2) = grid.find_path_astar(from, to, &weights, &[]);

        assert_eq!(wp1, wp2, "A* must be deterministic");
        assert_eq!(d1, d2, "diagnostics must be deterministic");
    }

    #[test]
    fn mark_route_cells_marks_segments() {
        let grid = RouteGrid {
            cols: 6,
            rows: 4,
            cell_size: 1.0,
            origin: LayoutPoint { x: 0.0, y: 0.0 },
            occupied: vec![false; 24],
        };
        let mut occupied_routes = vec![false; grid.cols * grid.rows];
        let waypoints = vec![
            LayoutPoint { x: 1.0, y: 2.0 },
            LayoutPoint { x: 4.0, y: 2.0 },
        ];

        mark_route_cells(&grid, &waypoints, &mut occupied_routes);

        for col in 1..=4 {
            let idx = 2 * grid.cols + col;
            assert!(occupied_routes[idx], "col {col} should be marked");
        }
    }

    #[test]
    fn self_loop_produces_valid_route() {
        let ir = make_simple_ir(&["A"], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let waypoints = self_loop_route(&layout.nodes[0].rect, GraphDirection::TB);
        assert!(
            waypoints.len() >= 3,
            "self-loop should have at least 3 points"
        );
    }

    #[test]
    fn self_loop_all_directions() {
        let rect = LayoutRect {
            x: 10.0,
            y: 10.0,
            width: 10.0,
            height: 3.0,
        };
        for dir in [
            GraphDirection::TB,
            GraphDirection::BT,
            GraphDirection::LR,
            GraphDirection::RL,
        ] {
            let wp = self_loop_route(&rect, dir);
            assert!(wp.len() >= 3, "self-loop for {dir:?} too short");
        }
    }

    #[test]
    fn parallel_edge_offset_single() {
        assert_eq!(parallel_edge_offset(0, 1, 1.5), 0.0);
    }

    #[test]
    fn parallel_edge_offset_two_edges() {
        let o0 = parallel_edge_offset(0, 2, 2.0);
        let o1 = parallel_edge_offset(1, 2, 2.0);
        assert!(o0 < 0.0, "first edge should be offset negatively");
        assert!(o1 > 0.0, "second edge should be offset positively");
        assert!(
            (o0 + o1).abs() < f64::EPSILON,
            "offsets should be symmetric"
        );
    }

    #[test]
    fn parallel_edge_offset_three_edges() {
        let o0 = parallel_edge_offset(0, 3, 1.0);
        let o1 = parallel_edge_offset(1, 3, 1.0);
        let o2 = parallel_edge_offset(2, 3, 1.0);
        assert!(o0 < 0.0);
        assert!(
            o1.abs() < f64::EPSILON,
            "middle edge should have zero offset"
        );
        assert!(o2 > 0.0);
    }

    #[test]
    fn route_all_edges_basic() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let weights = RoutingWeights::default();
        let (paths, report) = route_all_edges(&ir, &layout, &default_config(), &weights);

        assert_eq!(paths.len(), 1);
        assert_eq!(report.edges.len(), 1);
        assert!(!report.edges[0].fallback);
    }

    #[test]
    fn route_all_edges_with_self_loop() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 0), (0, 1)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let weights = RoutingWeights::default();
        let (paths, _report) = route_all_edges(&ir, &layout, &default_config(), &weights);

        assert_eq!(paths.len(), 2);
        assert!(
            paths[0].waypoints.len() >= 3,
            "self-loop should have multiple waypoints"
        );
    }

    #[test]
    fn routing_report_totals_are_consistent() {
        let ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let weights = RoutingWeights::default();
        let (_, report) = route_all_edges(&ir, &layout, &default_config(), &weights);

        let sum_cost: f64 = report.edges.iter().map(|d| d.cost).sum();
        let sum_bends: usize = report.edges.iter().map(|d| d.bends).sum();
        let sum_cells: usize = report.edges.iter().map(|d| d.cells_explored).sum();
        let sum_fallbacks: usize = report.edges.iter().filter(|d| d.fallback).count();

        assert!((report.total_cost - sum_cost).abs() < f64::EPSILON);
        assert_eq!(report.total_bends, sum_bends);
        assert_eq!(report.total_cells_explored, sum_cells);
        assert_eq!(report.fallback_count, sum_fallbacks);
    }

    #[test]
    #[ignore = "perf harness: run manually for profiling"]
    fn perf_route_all_edges_astar_dense() {
        // Route-heavy workload to profile A* expansion and obstacle handling.
        // Run with:
        // cargo test -p ftui-extras --features diagram perf_route_all_edges_astar_dense --release -- --ignored --nocapture
        let ir = make_random_ir(1337, 120, 10, GraphDirection::TB);
        let mut config = default_config();
        config.route_budget = usize::MAX / 2;
        let layout = layout_diagram(&ir, &config);
        let weights = RoutingWeights::default();

        let mut checksum = 0usize;
        for _ in 0..8 {
            let (paths, report) = route_all_edges(&ir, &layout, &config, &weights);
            checksum = checksum.wrapping_add(paths.len());
            checksum = checksum.wrapping_add(report.total_cells_explored);
        }
        std::hint::black_box(checksum);
    }

    // =========================================================================
    // Label placement tests (bd-33fdz)
    // =========================================================================

    #[test]
    fn label_placement_no_labels() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let config = LabelPlacementConfig::default();
        let result = place_labels(&ir, &layout, &config);
        assert!(result.edge_labels.is_empty());
        assert!(result.node_labels.is_empty());
        assert!(result.collisions.is_empty());
    }

    fn make_labeled_ir(
        nodes: &[(&str, Option<&str>)],
        edges: &[(usize, usize, Option<&str>)],
        direction: GraphDirection,
    ) -> MermaidDiagramIr {
        let mut labels = Vec::new();

        let ir_nodes: Vec<IrNode> = nodes
            .iter()
            .map(|(id, label_text)| {
                let label = label_text.map(|t| {
                    let idx = labels.len();
                    labels.push(IrLabel {
                        text: t.to_string(),
                        span: empty_span(),
                    });
                    IrLabelId(idx)
                });
                IrNode {
                    id: id.to_string(),
                    label,
                    shape: NodeShape::Rect,
                    classes: vec![],
                    style_ref: None,
                    span_primary: empty_span(),
                    span_all: vec![],
                    implicit: false,
                    members: vec![],
                }
            })
            .collect();

        let ir_edges: Vec<IrEdge> = edges
            .iter()
            .map(|(from, to, label_text)| {
                let label = label_text.map(|t| {
                    let idx = labels.len();
                    labels.push(IrLabel {
                        text: t.to_string(),
                        span: empty_span(),
                    });
                    IrLabelId(idx)
                });
                IrEdge {
                    from: IrEndpoint::Node(IrNodeId(*from)),
                    to: IrEndpoint::Node(IrNodeId(*to)),
                    arrow: "-->".to_string(),
                    label,
                    style_ref: None,
                    span: empty_span(),
                }
            })
            .collect();

        MermaidDiagramIr {
            diagram_type: DiagramType::Graph,
            direction,
            nodes: ir_nodes,
            edges: ir_edges,
            ports: vec![],
            clusters: vec![],
            labels,
            pie_entries: vec![],
            pie_title: None,
            pie_show_data: false,
            style_refs: vec![],
            links: vec![],
            meta: MermaidDiagramMeta {
                diagram_type: DiagramType::Graph,
                direction,
                support_level: MermaidSupportLevel::Supported,
                init: MermaidInitParse::default(),
                theme_overrides: MermaidThemeOverrides::default(),
                guard: MermaidGuardReport::default(),
            },
            constraints: vec![],
        }
    }
    #[test]
    fn label_placement_edge_labels_at_midpoints() {
        let ir = make_labeled_ir(
            &[("A", None), ("B", None)],
            &[(0, 1, Some("edge label"))],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let config = LabelPlacementConfig::default();
        let result = place_labels(&ir, &layout, &config);

        assert_eq!(result.edge_labels.len(), 1);
        let placed = &result.edge_labels[0];
        assert!(placed.rect.width > 0.0);
        assert!(placed.rect.height > 0.0);
    }

    #[test]
    fn label_placement_node_labels() {
        let ir = make_labeled_ir(
            &[("A", Some("Node A")), ("B", Some("Node B"))],
            &[(0, 1, None)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let config = LabelPlacementConfig::default();
        let result = place_labels(&ir, &layout, &config);

        assert_eq!(result.node_labels.len(), 2);
    }

    #[test]
    fn label_collision_detection_works() {
        // Two edges with labels close together should trigger collision avoidance.
        let ir = make_labeled_ir(
            &[("A", None), ("B", None), ("C", None)],
            &[(0, 1, Some("label1")), (0, 2, Some("label2"))],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let config = LabelPlacementConfig {
            offset_step: 0.5,
            max_offset: 10.0,
            ..Default::default()
        };
        let result = place_labels(&ir, &layout, &config);

        // Both labels should be placed.
        assert_eq!(result.edge_labels.len(), 2);
        // Labels should not overlap.
        let r0 = &result.edge_labels[0].rect;
        let r1 = &result.edge_labels[1].rect;
        let overlap = rects_overlap(r0, r1, 0.0);
        assert!(
            !overlap,
            "labels should not overlap after collision avoidance"
        );
    }

    #[test]
    fn label_placement_is_deterministic() {
        let ir = make_labeled_ir(
            &[("A", Some("NodeA")), ("B", Some("NodeB"))],
            &[(0, 1, Some("edge"))],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let config = LabelPlacementConfig::default();
        let r1 = place_labels(&ir, &layout, &config);
        let r2 = place_labels(&ir, &layout, &config);

        assert_eq!(r1.edge_labels.len(), r2.edge_labels.len());
        for (l1, l2) in r1.edge_labels.iter().zip(r2.edge_labels.iter()) {
            assert_eq!(l1, l2, "label placement must be deterministic");
        }
    }

    #[test]
    fn label_truncation_for_long_text() {
        let ir = make_labeled_ir(
            &[("A", None), ("B", None)],
            &[(
                0,
                1,
                Some("This is a very long label that should be truncated"),
            )],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let config = LabelPlacementConfig {
            max_label_width: 10.0,
            char_width: 1.0,
            ..Default::default()
        };
        let result = place_labels(&ir, &layout, &config);

        assert_eq!(result.edge_labels.len(), 1);
        assert!(result.edge_labels[0].was_truncated);
        assert!(result.edge_labels[0].rect.width <= 10.0 + 0.01);
    }

    #[test]
    fn edge_midpoint_calculation() {
        let wp = vec![
            LayoutPoint { x: 0.0, y: 0.0 },
            LayoutPoint { x: 10.0, y: 0.0 },
        ];
        let mid = edge_midpoint(&wp);
        assert!((mid.x - 5.0).abs() < 0.01);
        assert!((mid.y - 0.0).abs() < 0.01);
    }

    #[test]
    fn edge_midpoint_multi_segment() {
        let wp = vec![
            LayoutPoint { x: 0.0, y: 0.0 },
            LayoutPoint { x: 0.0, y: 4.0 },
            LayoutPoint { x: 3.0, y: 4.0 },
        ];
        // Total length: 4 + 3 = 7. Midpoint at 3.5 along path.
        let mid = edge_midpoint(&wp);
        // First segment covers 0..4, midpoint is at 3.5 on first segment.
        assert!((mid.x - 0.0).abs() < 0.01);
        assert!((mid.y - 3.5).abs() < 0.01);
    }

    #[test]
    fn offset_candidates_are_deterministic() {
        let offsets1 = generate_offset_candidates(1.0, 3.0);
        let offsets2 = generate_offset_candidates(1.0, 3.0);
        assert_eq!(offsets1, offsets2);
        // First offset should be (0,0).
        assert_eq!(offsets1[0], (0.0, 0.0));
        // Should include cardinal and diagonal directions.
        assert!(offsets1.len() > 8);
    }

    #[test]
    fn rects_overlap_basic() {
        let a = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 5.0,
            height: 5.0,
        };
        let b = LayoutRect {
            x: 3.0,
            y: 3.0,
            width: 5.0,
            height: 5.0,
        };
        assert!(rects_overlap(&a, &b, 0.0));

        let c = LayoutRect {
            x: 10.0,
            y: 10.0,
            width: 5.0,
            height: 5.0,
        };
        assert!(!rects_overlap(&a, &c, 0.0));
    }

    // =========================================================================
    // Property tests: Layout Invariants (bd-3g7lx)
    // =========================================================================

    /// Simple deterministic PRNG for test graphs with fixed seeds.
    struct SimpleRng {
        state: u64,
    }

    impl SimpleRng {
        fn new(seed: u64) -> Self {
            Self {
                state: seed.wrapping_add(1),
            }
        }
        fn next_u64(&mut self) -> u64 {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            self.state
        }
        fn next_usize(&mut self, max: usize) -> usize {
            (self.next_u64() as usize) % max
        }
        fn next_bool(&mut self, pct: u64) -> bool {
            self.next_u64() % 100 < pct
        }
    }

    fn make_random_ir(
        seed: u64,
        node_count: usize,
        edge_pct: u64,
        direction: GraphDirection,
    ) -> MermaidDiagramIr {
        let mut rng = SimpleRng::new(seed);
        let names: Vec<String> = (0..node_count).map(|i| format!("N{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut edges = Vec::new();
        for i in 0..node_count {
            for j in 0..node_count {
                if i != j && rng.next_bool(edge_pct) {
                    edges.push((i, j));
                }
            }
        }
        make_simple_ir(&refs, &edges, direction)
    }

    fn assert_no_same_rank_overlaps(layout: &DiagramLayout) {
        for i in 0..layout.nodes.len() {
            for j in (i + 1)..layout.nodes.len() {
                if layout.nodes[i].rank == layout.nodes[j].rank {
                    let ri = &layout.nodes[i].rect;
                    let rj = &layout.nodes[j].rect;
                    let no_h = ri.x + ri.width <= rj.x + 0.01 || rj.x + rj.width <= ri.x + 0.01;
                    let no_v = ri.y + ri.height <= rj.y + 0.01 || rj.y + rj.height <= ri.y + 0.01;
                    assert!(no_h || no_v, "overlap: node {} vs {}", i, j);
                }
            }
        }
    }

    fn assert_all_nodes_in_bounds(layout: &DiagramLayout) {
        let bb = &layout.bounding_box;
        for n in &layout.nodes {
            let r = &n.rect;
            assert!(r.x >= bb.x - 0.01, "node {} left OOB", n.node_idx);
            assert!(r.y >= bb.y - 0.01, "node {} top OOB", n.node_idx);
            assert!(
                r.x + r.width <= bb.x + bb.width + 0.01,
                "node {} right OOB",
                n.node_idx
            );
            assert!(
                r.y + r.height <= bb.y + bb.height + 0.01,
                "node {} bottom OOB",
                n.node_idx
            );
        }
    }

    fn assert_edges_in_bounds(layout: &DiagramLayout) {
        let bb = &layout.bounding_box;
        let m = 0.01;
        for e in &layout.edges {
            for (i, wp) in e.waypoints.iter().enumerate() {
                assert!(
                    wp.x >= bb.x - m && wp.x <= bb.x + bb.width + m,
                    "edge {} wp {} x OOB",
                    e.edge_idx,
                    i
                );
                assert!(
                    wp.y >= bb.y - m && wp.y <= bb.y + bb.height + m,
                    "edge {} wp {} y OOB",
                    e.edge_idx,
                    i
                );
            }
        }
    }

    #[test]
    fn prop_no_overlaps_random() {
        for seed in 0..20 {
            let ir = make_random_ir(seed, 8, 20, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            assert_no_same_rank_overlaps(&layout);
        }
    }

    #[test]
    fn prop_no_overlaps_dense() {
        for seed in 100..110 {
            let ir = make_random_ir(seed, 6, 50, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            assert_no_same_rank_overlaps(&layout);
        }
    }

    #[test]
    fn prop_no_overlaps_all_dirs() {
        for dir in [
            GraphDirection::TB,
            GraphDirection::TD,
            GraphDirection::BT,
            GraphDirection::LR,
            GraphDirection::RL,
        ] {
            for seed in 200..205 {
                let ir = make_random_ir(seed, 7, 25, dir);
                let layout = layout_diagram(&ir, &default_config());
                assert_no_same_rank_overlaps(&layout);
            }
        }
    }

    #[test]
    fn prop_nodes_in_bounds_random() {
        for seed in 300..320 {
            let ir = make_random_ir(seed, 10, 20, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            assert_all_nodes_in_bounds(&layout);
        }
    }

    #[test]
    fn prop_nodes_in_bounds_all_dirs() {
        for dir in [
            GraphDirection::TB,
            GraphDirection::BT,
            GraphDirection::LR,
            GraphDirection::RL,
        ] {
            for seed in 400..405 {
                let ir = make_random_ir(seed, 8, 30, dir);
                let layout = layout_diagram(&ir, &default_config());
                assert_all_nodes_in_bounds(&layout);
            }
        }
    }

    #[test]
    fn prop_deterministic_random() {
        for seed in 500..515 {
            let ir = make_random_ir(seed, 8, 25, GraphDirection::TB);
            let c = default_config();
            let l1 = layout_diagram(&ir, &c);
            let l2 = layout_diagram(&ir, &c);
            for (n1, n2) in l1.nodes.iter().zip(l2.nodes.iter()) {
                assert_eq!(n1.rank, n2.rank, "seed {seed}");
                assert_eq!(n1.order, n2.order, "seed {seed}");
                assert_eq!(n1.rect, n2.rect, "seed {seed}");
            }
        }
    }

    #[test]
    fn prop_edge_waypoints_in_bounds() {
        for seed in 600..615 {
            let ir = make_random_ir(seed, 6, 25, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            assert_edges_in_bounds(&layout);
        }
    }

    #[test]
    fn prop_node_count_matches_ir() {
        for seed in 700..720 {
            let n = 3 + (seed as usize % 10);
            let ir = make_random_ir(seed, n, 20, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            assert_eq!(layout.nodes.len(), ir.nodes.len(), "seed {seed}");
        }
    }

    #[test]
    fn prop_bbox_positive() {
        for seed in 800..820 {
            let ir = make_random_ir(seed, 5, 30, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            assert!(layout.bounding_box.width > 0.0, "seed {seed}");
            assert!(layout.bounding_box.height > 0.0, "seed {seed}");
        }
    }

    #[test]
    fn stress_no_panic() {
        for seed in 1000..1050 {
            let mut rng = SimpleRng::new(seed);
            let n = 2 + rng.next_usize(12);
            let d = 10 + rng.next_usize(40);
            let dir = [
                GraphDirection::TB,
                GraphDirection::BT,
                GraphDirection::LR,
                GraphDirection::RL,
            ][rng.next_usize(4)];
            let ir = make_random_ir(seed + 2000, n, d as u64, dir);
            let layout = layout_diagram(&ir, &default_config());
            assert_eq!(layout.nodes.len(), ir.nodes.len());
        }
    }

    #[test]
    fn stress_all_invariants() {
        for seed in 3000..3030 {
            let mut rng = SimpleRng::new(seed);
            let n = 3 + rng.next_usize(8);
            let d = 15 + rng.next_usize(30);
            let dir = [
                GraphDirection::TB,
                GraphDirection::BT,
                GraphDirection::LR,
                GraphDirection::RL,
            ][rng.next_usize(4)];
            let ir = make_random_ir(seed + 4000, n, d as u64, dir);
            let layout = layout_diagram(&ir, &default_config());
            assert_no_same_rank_overlaps(&layout);
            assert_all_nodes_in_bounds(&layout);
            assert_edges_in_bounds(&layout);
            assert_eq!(layout.nodes.len(), ir.nodes.len());
        }
    }

    #[test]
    fn guard_degrade_no_overlaps() {
        let ir = make_random_ir(42, 10, 30, GraphDirection::TB);
        let mut config = default_config();
        config.layout_iteration_budget = 5;
        let layout = layout_diagram(&ir, &config);
        assert_no_same_rank_overlaps(&layout);
        assert_all_nodes_in_bounds(&layout);
    }

    #[test]
    fn guard_degrade_deterministic() {
        let ir = make_random_ir(99, 8, 35, GraphDirection::LR);
        let mut config = default_config();
        config.layout_iteration_budget = 3;
        let l1 = layout_diagram(&ir, &config);
        let l2 = layout_diagram(&ir, &config);
        for (n1, n2) in l1.nodes.iter().zip(l2.nodes.iter()) {
            assert_eq!(n1.rect, n2.rect);
        }
    }

    #[test]
    fn prop_routing_deterministic() {
        for seed in 5000..5010 {
            let ir = make_random_ir(seed, 5, 30, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            let w = RoutingWeights::default();
            let (p1, _) = route_all_edges(&ir, &layout, &default_config(), &w);
            let (p2, _) = route_all_edges(&ir, &layout, &default_config(), &w);
            assert_eq!(p1.len(), p2.len(), "seed {seed}");
            for (a, b) in p1.iter().zip(p2.iter()) {
                assert_eq!(a.waypoints, b.waypoints, "seed {seed}");
            }
        }
    }

    #[test]
    fn prop_routing_report_consistent() {
        for seed in 6000..6010 {
            let ir = make_random_ir(seed, 6, 25, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            let w = RoutingWeights::default();
            let (_, rep) = route_all_edges(&ir, &layout, &default_config(), &w);
            let sc: f64 = rep.edges.iter().map(|d| d.cost).sum();
            let sb: usize = rep.edges.iter().map(|d| d.bends).sum();
            let se: usize = rep.edges.iter().map(|d| d.cells_explored).sum();
            let sf: usize = rep.edges.iter().filter(|d| d.fallback).count();
            assert!((rep.total_cost - sc).abs() < f64::EPSILON, "seed {seed}");
            assert_eq!(rep.total_bends, sb, "seed {seed}");
            assert_eq!(rep.total_cells_explored, se, "seed {seed}");
            assert_eq!(rep.fallback_count, sf, "seed {seed}");
        }
    }

    #[test]
    fn prop_label_no_overlap() {
        for seed in 7000..7010 {
            let n = 3 + (seed as usize % 4);
            let names: Vec<String> = (0..n).map(|i| format!("N{i}")).collect();
            let specs: Vec<(&str, Option<&str>)> = names
                .iter()
                .enumerate()
                .map(|(i, nm)| {
                    if (seed + i as u64).is_multiple_of(2) {
                        (nm.as_str(), Some("lbl"))
                    } else {
                        (nm.as_str(), None)
                    }
                })
                .collect();
            let mut edges = Vec::new();
            for i in 0..n.saturating_sub(1) {
                let has = !(seed + i as u64).is_multiple_of(3);
                edges.push((i, i + 1, if has { Some("edge") } else { None }));
            }
            let ir = make_labeled_ir(&specs, &edges, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            let cfg = LabelPlacementConfig {
                offset_step: 0.5,
                max_offset: 10.0,
                ..Default::default()
            };
            let res = place_labels(&ir, &layout, &cfg);
            for i in 0..res.edge_labels.len() {
                for j in (i + 1)..res.edge_labels.len() {
                    assert!(
                        !rects_overlap(&res.edge_labels[i].rect, &res.edge_labels[j].rect, 0.0),
                        "seed {seed}: labels {i} and {j} overlap"
                    );
                }
            }
        }
    }

    #[test]
    fn prop_forward_edges_higher_rank() {
        for seed in 8000..8010 {
            let n = 5 + (seed as usize % 5);
            let mut edges = Vec::new();
            let mut rng = SimpleRng::new(seed);
            for i in 0..n {
                for j in (i + 1)..n {
                    if rng.next_bool(25) {
                        edges.push((i, j));
                    }
                }
            }
            if edges.is_empty() {
                edges.push((0, 1));
            }
            let names: Vec<String> = (0..n).map(|i| format!("N{i}")).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let ir = make_simple_ir(&refs, &edges, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            let mut rm = std::collections::HashMap::new();
            for nd in &layout.nodes {
                rm.insert(nd.node_idx, nd.rank);
            }
            for edge in &ir.edges {
                if let (IrEndpoint::Node(f), IrEndpoint::Node(t)) = (&edge.from, &edge.to)
                    && f.0 != t.0
                {
                    let fr = rm.get(&f.0).copied().unwrap_or(0);
                    let tr = rm.get(&t.0).copied().unwrap_or(0);
                    assert!(fr <= tr, "seed {seed}: {}→{} backward", f.0, t.0);
                }
            }
        }
    }

    #[test]
    fn prop_objective_finite() {
        for seed in 9000..9020 {
            let ir = make_random_ir(seed, 7, 25, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            let obj = evaluate_layout(&layout);
            assert!(obj.score.is_finite(), "seed {seed}");
            assert!(obj.total_edge_length.is_finite(), "seed {seed}");
        }
    }

    #[test]
    fn prop_single_node_all_dirs() {
        for dir in [
            GraphDirection::TB,
            GraphDirection::BT,
            GraphDirection::LR,
            GraphDirection::RL,
        ] {
            let ir = make_simple_ir(&["X"], &[], dir);
            let layout = layout_diagram(&ir, &default_config());
            assert_eq!(layout.nodes.len(), 1);
            assert_all_nodes_in_bounds(&layout);
        }
    }

    #[test]
    fn prop_disconnected_nodes() {
        let ir = make_simple_ir(&["A", "B"], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        assert_eq!(layout.nodes.len(), 2);
        assert_no_same_rank_overlaps(&layout);
        assert_all_nodes_in_bounds(&layout);
    }

    #[test]
    fn prop_complete_graph_k4() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        assert_eq!(layout.nodes.len(), 4);
        assert_no_same_rank_overlaps(&layout);
        assert_all_nodes_in_bounds(&layout);
    }

    #[test]
    fn prop_long_chain() {
        let names: Vec<String> = (0..15).map(|i| format!("N{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let edges: Vec<(usize, usize)> = (0..14).map(|i| (i, i + 1)).collect();
        let ir = make_simple_ir(&refs, &edges, GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        assert_eq!(layout.nodes.len(), 15);
        assert_no_same_rank_overlaps(&layout);
        assert_all_nodes_in_bounds(&layout);
        assert_edges_in_bounds(&layout);
        let mut ranks: Vec<usize> = layout.nodes.iter().map(|n| n.rank).collect();
        ranks.sort();
        ranks.dedup();
        assert_eq!(ranks.len(), 15, "chain should have 15 distinct ranks");
    }

    // =========================================================================
    // Additional invariant tests (bd-3g7lx continued)
    // =========================================================================

    fn assert_no_edge_node_intersections(ir: &MermaidDiagramIr, layout: &DiagramLayout) {
        for edge_path in &layout.edges {
            if edge_path.edge_idx >= ir.edges.len() {
                continue;
            }
            let edge = &ir.edges[edge_path.edge_idx];
            let src = match &edge.from {
                IrEndpoint::Node(id) => Some(id.0),
                IrEndpoint::Port(_) => None,
            };
            let dst = match &edge.to {
                IrEndpoint::Node(id) => Some(id.0),
                IrEndpoint::Port(_) => None,
            };

            for wp in &edge_path.waypoints {
                for node in &layout.nodes {
                    if Some(node.node_idx) == src || Some(node.node_idx) == dst {
                        continue;
                    }
                    // A* grid resolution is 1.0 unit; shrink nodes by 1.5 to
                    // account for grid-snapping of waypoints near node edges.
                    let margin = 1.5;
                    let inner = LayoutRect {
                        x: node.rect.x + margin,
                        y: node.rect.y + margin,
                        width: (node.rect.width - 2.0 * margin).max(0.0),
                        height: (node.rect.height - 2.0 * margin).max(0.0),
                    };
                    assert!(
                        !inner.contains_point(*wp),
                        "edge {} waypoint ({:.1},{:.1}) inside node {} rect {:?}",
                        edge_path.edge_idx,
                        wp.x,
                        wp.y,
                        node.node_idx,
                        node.rect
                    );
                }
            }
        }
    }

    fn assert_clusters_contain_members(ir: &MermaidDiagramIr, layout: &DiagramLayout) {
        for cluster_box in &layout.clusters {
            if cluster_box.cluster_idx >= ir.clusters.len() {
                continue;
            }
            let cluster = &ir.clusters[cluster_box.cluster_idx];
            let cr = &cluster_box.rect;
            for member_id in &cluster.members {
                if let Some(node) = layout.nodes.iter().find(|n| n.node_idx == member_id.0) {
                    let nr = &node.rect;
                    assert!(
                        nr.x >= cr.x - 0.01
                            && nr.y >= cr.y - 0.01
                            && nr.x + nr.width <= cr.x + cr.width + 0.01
                            && nr.y + nr.height <= cr.y + cr.height + 0.01,
                        "cluster {} doesn't contain member node {} ({:?} vs {:?})",
                        cluster_box.cluster_idx,
                        member_id.0,
                        nr,
                        cr
                    );
                }
            }
        }
    }

    fn make_clustered_ir(
        nodes: &[&str],
        edges: &[(usize, usize)],
        cluster_members: &[usize],
        direction: GraphDirection,
    ) -> MermaidDiagramIr {
        let mut ir = make_simple_ir(nodes, edges, direction);
        ir.clusters.push(IrCluster {
            id: IrClusterId(0),
            title: None,
            members: cluster_members.iter().map(|&i| IrNodeId(i)).collect(),
            span: empty_span(),
        });
        ir
    }

    #[test]
    fn prop_no_same_rank_overlaps_random() {
        for seed in 10_000..10_020 {
            let ir = make_random_ir(seed, 8, 25, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            assert_no_same_rank_overlaps(&layout);
        }
    }

    #[test]
    fn prop_no_same_rank_overlaps_all_dirs() {
        for dir in [
            GraphDirection::TB,
            GraphDirection::BT,
            GraphDirection::LR,
            GraphDirection::RL,
        ] {
            for seed in 10_100..10_105 {
                let ir = make_random_ir(seed, 6, 30, dir);
                let layout = layout_diagram(&ir, &default_config());
                assert_no_same_rank_overlaps(&layout);
            }
        }
    }

    #[test]
    fn prop_no_edge_node_intersections_random() {
        let w = RoutingWeights::default();
        for seed in 11_000..11_015 {
            let ir = make_random_ir(seed, 6, 20, GraphDirection::TB);
            let mut layout = layout_diagram(&ir, &default_config());
            let (routed, _) = route_all_edges(&ir, &layout, &default_config(), &w);
            layout.edges = routed;
            assert_no_edge_node_intersections(&ir, &layout);
        }
    }

    #[test]
    fn prop_no_edge_node_intersections_dense() {
        let w = RoutingWeights::default();
        for seed in 11_100..11_108 {
            let ir = make_random_ir(seed, 5, 50, GraphDirection::TB);
            let mut layout = layout_diagram(&ir, &default_config());
            let (routed, _) = route_all_edges(&ir, &layout, &default_config(), &w);
            layout.edges = routed;
            assert_no_edge_node_intersections(&ir, &layout);
        }
    }

    #[test]
    fn prop_no_edge_node_intersections_all_dirs() {
        let w = RoutingWeights::default();
        for dir in [
            GraphDirection::TB,
            GraphDirection::BT,
            GraphDirection::LR,
            GraphDirection::RL,
        ] {
            for seed in 11_200..11_204 {
                let ir = make_random_ir(seed, 5, 30, dir);
                let mut layout = layout_diagram(&ir, &default_config());
                let (routed, _) = route_all_edges(&ir, &layout, &default_config(), &w);
                layout.edges = routed;
                assert_no_edge_node_intersections(&ir, &layout);
            }
        }
    }

    #[test]
    fn prop_cluster_contains_members() {
        let ir = make_clustered_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (1, 2), (2, 3)],
            &[0, 1, 2],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        assert_clusters_contain_members(&ir, &layout);
    }

    #[test]
    fn prop_cluster_bounds_positive_size() {
        let ir = make_clustered_ir(
            &["A", "B", "C"],
            &[(0, 1), (1, 2)],
            &[0, 1],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        for c in &layout.clusters {
            assert!(c.rect.width > 0.0, "cluster {} zero width", c.cluster_idx);
            assert!(c.rect.height > 0.0, "cluster {} zero height", c.cluster_idx);
        }
    }

    #[test]
    fn prop_cluster_all_directions() {
        for dir in [
            GraphDirection::TB,
            GraphDirection::BT,
            GraphDirection::LR,
            GraphDirection::RL,
        ] {
            let ir = make_clustered_ir(
                &["A", "B", "C", "D", "E"],
                &[(0, 1), (1, 2), (2, 3), (3, 4)],
                &[1, 2, 3],
                dir,
            );
            let layout = layout_diagram(&ir, &default_config());
            assert_clusters_contain_members(&ir, &layout);
            assert_no_same_rank_overlaps(&layout);
        }
    }

    #[test]
    fn guard_degrade_same_rank_no_overlap() {
        for seed in 12_000..12_010 {
            let ir = make_random_ir(seed, 8, 25, GraphDirection::TB);
            let mut config = default_config();
            config.layout_iteration_budget = 3;
            let layout = layout_diagram(&ir, &config);
            assert_no_same_rank_overlaps(&layout);
            assert_all_nodes_in_bounds(&layout);
        }
    }

    #[test]
    fn guard_degrade_all_dirs() {
        for dir in [
            GraphDirection::TB,
            GraphDirection::BT,
            GraphDirection::LR,
            GraphDirection::RL,
        ] {
            let ir = make_random_ir(42, 7, 30, dir);
            let mut config = default_config();
            config.layout_iteration_budget = 2;
            let layout = layout_diagram(&ir, &config);
            assert_no_same_rank_overlaps(&layout);
            assert_all_nodes_in_bounds(&layout);
            assert_edges_in_bounds(&layout);
        }
    }

    #[test]
    fn guard_degrade_extreme_budget() {
        for seed in 12_100..12_110 {
            let ir = make_random_ir(seed, 10, 20, GraphDirection::TB);
            let mut config = default_config();
            config.layout_iteration_budget = 1;
            let layout = layout_diagram(&ir, &config);
            assert_no_same_rank_overlaps(&layout);
            assert_all_nodes_in_bounds(&layout);
            assert_eq!(layout.nodes.len(), ir.nodes.len(), "seed {seed}");
        }
    }

    #[test]
    fn guard_degrade_routing_budget() {
        let ir = make_random_ir(55, 6, 35, GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        let w = RoutingWeights::default();
        let mut config = default_config();
        config.route_budget = 10;
        let (paths, report) = route_all_edges(&ir, &layout, &config, &w);
        assert_eq!(paths.len(), ir.edges.len());
        for p in &paths {
            assert!(
                p.waypoints.len() >= 2,
                "edge {} has <2 waypoints",
                p.edge_idx
            );
        }
        if ir.edges.len() > 2 {
            assert!(
                report.fallback_count > 0,
                "expected fallbacks under tight routing budget"
            );
        }
    }

    #[test]
    fn guard_degrade_preserves_node_count() {
        for budget in [1, 2, 5, 10, 50] {
            let ir = make_random_ir(77, 8, 20, GraphDirection::TB);
            let mut config = default_config();
            config.layout_iteration_budget = budget;
            let layout = layout_diagram(&ir, &config);
            assert_eq!(layout.nodes.len(), ir.nodes.len(), "budget={budget}");
        }
    }

    #[test]
    fn stress_all_invariants_extended() {
        let w = RoutingWeights::default();
        for seed in 13_000..13_025 {
            let mut rng = SimpleRng::new(seed);
            let n = 3 + rng.next_usize(8);
            let d = 10 + rng.next_usize(40);
            let dir = [
                GraphDirection::TB,
                GraphDirection::BT,
                GraphDirection::LR,
                GraphDirection::RL,
            ][rng.next_usize(4)];
            let ir = make_random_ir(seed + 20_000, n, d as u64, dir);
            let mut layout = layout_diagram(&ir, &default_config());
            let (routed, _) = route_all_edges(&ir, &layout, &default_config(), &w);
            layout.edges = routed;
            // Recompute bounding box to include new A*-routed waypoints.
            layout.bounding_box =
                compute_bounding_box(&layout.nodes, &layout.clusters, &layout.edges);

            assert_no_same_rank_overlaps(&layout);
            assert_all_nodes_in_bounds(&layout);
            assert_edges_in_bounds(&layout);
            assert_no_edge_node_intersections(&ir, &layout);
            assert_eq!(layout.nodes.len(), ir.nodes.len(), "seed {seed}");
            assert!(layout.bounding_box.width > 0.0, "seed {seed}");
            assert!(layout.bounding_box.height > 0.0, "seed {seed}");
        }
    }

    #[test]
    fn stress_degraded_all_invariants() {
        for seed in 14_000..14_015 {
            let mut rng = SimpleRng::new(seed);
            let n = 4 + rng.next_usize(6);
            let d = 15 + rng.next_usize(30);
            let dir = [
                GraphDirection::TB,
                GraphDirection::BT,
                GraphDirection::LR,
                GraphDirection::RL,
            ][rng.next_usize(4)];
            let ir = make_random_ir(seed + 30_000, n, d as u64, dir);
            let mut config = default_config();
            config.layout_iteration_budget = 2 + rng.next_usize(5);
            let layout = layout_diagram(&ir, &config);

            assert_no_same_rank_overlaps(&layout);
            assert_all_nodes_in_bounds(&layout);
            assert_edges_in_bounds(&layout);
            assert_eq!(layout.nodes.len(), ir.nodes.len(), "seed {seed}");
        }
    }

    #[test]
    fn prop_tiebreak_deterministic_dense() {
        for seed in 15_000..15_010 {
            let ir = make_random_ir(seed, 6, 60, GraphDirection::TB);
            let c = default_config();
            let l1 = layout_diagram(&ir, &c);
            let l2 = layout_diagram(&ir, &c);
            for (n1, n2) in l1.nodes.iter().zip(l2.nodes.iter()) {
                assert_eq!(n1.rank, n2.rank, "seed {seed}: rank mismatch");
                assert_eq!(n1.order, n2.order, "seed {seed}: order mismatch");
                assert_eq!(n1.rect, n2.rect, "seed {seed}: rect mismatch");
            }
            for (e1, e2) in l1.edges.iter().zip(l2.edges.iter()) {
                assert_eq!(
                    e1.waypoints, e2.waypoints,
                    "seed {seed}: edge waypoints mismatch"
                );
            }
        }
    }

    #[test]
    fn prop_tiebreak_deterministic_symmetric() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (0, 2), (1, 3), (2, 3)],
            GraphDirection::TB,
        );
        let c = default_config();
        let l1 = layout_diagram(&ir, &c);
        let l2 = layout_diagram(&ir, &c);
        for (n1, n2) in l1.nodes.iter().zip(l2.nodes.iter()) {
            assert_eq!(n1.rect, n2.rect);
            assert_eq!(n1.rank, n2.rank);
            assert_eq!(n1.order, n2.order);
        }
    }

    #[test]
    fn prop_tiebreak_deterministic_star() {
        let names: Vec<String> = (0..8).map(|i| format!("N{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let edges: Vec<(usize, usize)> = (1..8).map(|i| (0, i)).collect();
        let ir = make_simple_ir(&refs, &edges, GraphDirection::TB);
        let c = default_config();
        let l1 = layout_diagram(&ir, &c);
        let l2 = layout_diagram(&ir, &c);
        for (n1, n2) in l1.nodes.iter().zip(l2.nodes.iter()) {
            assert_eq!(n1.rect, n2.rect, "star: node {} rect", n1.node_idx);
            assert_eq!(n1.order, n2.order, "star: node {} order", n1.node_idx);
        }
    }

    // ── Content-aware node sizing tests ──────────────────────────────

    fn make_labeled_ir_with_text(
        nodes: &[(&str, &str)],
        edges: &[(usize, usize)],
        direction: GraphDirection,
    ) -> MermaidDiagramIr {
        let ir_labels: Vec<IrLabel> = nodes
            .iter()
            .map(|(_, label)| IrLabel {
                text: label.to_string(),
                span: empty_span(),
            })
            .collect();

        let ir_nodes: Vec<IrNode> = nodes
            .iter()
            .enumerate()
            .map(|(i, (id, _))| IrNode {
                id: id.to_string(),
                label: Some(IrLabelId(i)),
                shape: NodeShape::Rect,
                classes: vec![],
                style_ref: None,
                span_primary: empty_span(),
                span_all: vec![],
                implicit: false,
                members: vec![],
            })
            .collect();

        let ir_edges: Vec<IrEdge> = edges
            .iter()
            .map(|(from, to)| IrEdge {
                from: IrEndpoint::Node(IrNodeId(*from)),
                to: IrEndpoint::Node(IrNodeId(*to)),
                arrow: "-->".to_string(),
                label: None,
                style_ref: None,
                span: empty_span(),
            })
            .collect();

        MermaidDiagramIr {
            diagram_type: DiagramType::Graph,
            direction,
            nodes: ir_nodes,
            edges: ir_edges,
            ports: vec![],
            clusters: vec![],
            labels: ir_labels,
            pie_entries: vec![],
            pie_title: None,
            pie_show_data: false,
            style_refs: vec![],
            links: vec![],
            meta: MermaidDiagramMeta {
                diagram_type: DiagramType::Graph,
                direction,
                support_level: MermaidSupportLevel::Supported,
                init: empty_init_parse(),
                theme_overrides: MermaidThemeOverrides {
                    theme: None,
                    theme_variables: BTreeMap::new(),
                },
                guard: empty_guard_report(),
            },
            constraints: vec![],
        }
    }
    #[test]
    fn content_aware_wider_label_wider_node() {
        let ir = make_labeled_ir_with_text(
            &[("A", "Hi"), ("B", "This is a very long label text")],
            &[(0, 1)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        assert_eq!(layout.nodes.len(), 2);
        assert!(
            layout.nodes[1].rect.width > layout.nodes[0].rect.width,
            "node B (long label) should be wider than node A (short label): B={}, A={}",
            layout.nodes[1].rect.width,
            layout.nodes[0].rect.width,
        );
    }

    #[test]
    fn content_aware_no_label_uses_default() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let spacing = LayoutSpacing::default();
        let layout = layout_diagram_with_spacing(&ir, &default_config(), &spacing);
        // Without labels, all nodes get default width.
        for node in &layout.nodes {
            assert!(
                (node.rect.width - spacing.node_width).abs() < 0.01,
                "node without label should use default width"
            );
        }
    }

    // ── Cluster-aware crossing minimization tests ────────────────────

    fn make_named_clustered_ir(
        nodes: &[&str],
        edges: &[(usize, usize)],
        clusters: &[(&str, &[usize])],
        direction: GraphDirection,
    ) -> MermaidDiagramIr {
        let ir_nodes: Vec<IrNode> = nodes
            .iter()
            .map(|id| IrNode {
                id: id.to_string(),
                label: None,
                shape: NodeShape::Rect,
                classes: vec![],
                style_ref: None,
                span_primary: empty_span(),
                span_all: vec![],
                implicit: false,
                members: vec![],
            })
            .collect();

        let ir_edges: Vec<IrEdge> = edges
            .iter()
            .map(|(from, to)| IrEdge {
                from: IrEndpoint::Node(IrNodeId(*from)),
                to: IrEndpoint::Node(IrNodeId(*to)),
                arrow: "-->".to_string(),
                label: None,
                style_ref: None,
                span: empty_span(),
            })
            .collect();

        let ir_clusters: Vec<IrCluster> = clusters
            .iter()
            .enumerate()
            .map(|(ci, (_, members))| IrCluster {
                id: IrClusterId(ci),
                title: None,
                members: members.iter().map(|&m| IrNodeId(m)).collect(),
                span: empty_span(),
            })
            .collect();

        MermaidDiagramIr {
            diagram_type: DiagramType::Graph,
            direction,
            nodes: ir_nodes,
            edges: ir_edges,
            ports: vec![],
            clusters: ir_clusters,
            labels: vec![],
            pie_entries: vec![],
            pie_title: None,
            pie_show_data: false,
            style_refs: vec![],
            links: vec![],
            meta: MermaidDiagramMeta {
                diagram_type: DiagramType::Graph,
                direction,
                support_level: MermaidSupportLevel::Supported,
                init: empty_init_parse(),
                theme_overrides: MermaidThemeOverrides {
                    theme: None,
                    theme_variables: BTreeMap::new(),
                },
                guard: empty_guard_report(),
            },
            constraints: vec![],
        }
    }
    #[test]
    fn cluster_members_stay_contiguous() {
        // Nodes B, C, D in a cluster at rank 1. They should remain adjacent.
        // A -> B, A -> C, A -> D, A -> E, B -> F, C -> F, D -> F, E -> F
        let ir = make_named_clustered_ir(
            &["A", "B", "C", "D", "E", "F"],
            &[
                (0, 1),
                (0, 2),
                (0, 3),
                (0, 4),
                (1, 5),
                (2, 5),
                (3, 5),
                (4, 5),
            ],
            &[("cluster1", &[1, 2, 3])],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());

        // Find positions of cluster members (B=1, C=2, D=3) within rank 1.
        let rank1_nodes: Vec<&LayoutNodeBox> =
            layout.nodes.iter().filter(|n| n.rank == 1).collect();

        let mut rank1_sorted = rank1_nodes.clone();
        rank1_sorted.sort_by(|a, b| {
            a.rect
                .x
                .partial_cmp(&b.rect.x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Find indices of cluster members in sorted order.
        let cluster_positions: Vec<usize> = rank1_sorted
            .iter()
            .enumerate()
            .filter(|(_, n)| [1, 2, 3].contains(&n.node_idx))
            .map(|(i, _)| i)
            .collect();

        // Cluster members should be contiguous (adjacent indices).
        if cluster_positions.len() >= 2 {
            for w in cluster_positions.windows(2) {
                assert_eq!(
                    w[1] - w[0],
                    1,
                    "cluster members should be contiguous, found positions {:?}",
                    cluster_positions
                );
            }
        }
    }

    // ── Multi-segment edge routing tests ─────────────────────────────

    #[test]
    fn multi_rank_edge_gets_intermediate_waypoints() {
        // A -> B -> C -> D, plus A -> D (skips 3 ranks).
        let ir = make_simple_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (1, 2), (2, 3), (0, 3)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());

        // Find the edge A -> D (index 3).
        let long_edge = &layout.edges[3];
        assert!(
            long_edge.waypoints.len() > 2,
            "multi-rank edge A->D should have intermediate waypoints, got {}",
            long_edge.waypoints.len()
        );

        // Adjacent edges should have exactly 2 waypoints.
        let short_edge = &layout.edges[0]; // A -> B
        assert_eq!(
            short_edge.waypoints.len(),
            2,
            "adjacent-rank edge should have exactly 2 waypoints"
        );
    }

    #[test]
    fn multi_rank_waypoints_increase_with_distance() {
        // Chain: 0->1->2->3->4, plus long edge 0->4.
        let ir = make_simple_ir(
            &["A", "B", "C", "D", "E"],
            &[(0, 1), (1, 2), (2, 3), (3, 4), (0, 4)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());

        let long_edge = &layout.edges[4]; // A -> E
        // Should have 3 intermediate waypoints (at ranks 1, 2, 3) + 2 endpoints = 5.
        assert!(
            long_edge.waypoints.len() >= 4,
            "4-rank edge should have >= 4 waypoints, got {}",
            long_edge.waypoints.len()
        );
    }

    // ── Expanded stats tests ─────────────────────────────────────────

    #[test]
    fn stats_total_bends_reflects_routing() {
        // A -> B -> C -> D, plus A -> D (multi-rank edge with bends).
        let ir = make_simple_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (1, 2), (2, 3), (0, 3)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());

        // total_bends should count bends from all edges.
        let expected_bends: usize = layout
            .edges
            .iter()
            .map(|e| e.waypoints.len().saturating_sub(2))
            .sum();
        assert_eq!(layout.stats.total_bends, expected_bends);
    }

    #[test]
    fn stats_position_variance_is_finite() {
        let ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (0, 2)], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        assert!(
            layout.stats.position_variance.is_finite(),
            "position_variance should be finite"
        );
    }

    #[test]
    fn empty_diagram_stats_have_zero_bends() {
        let ir = make_simple_ir(&[], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir, &default_config());
        assert_eq!(layout.stats.total_bends, 0);
        assert!((layout.stats.position_variance - 0.0).abs() < f64::EPSILON);
    }

    // ── Constraint application tests ──────────────────────────────

    #[test]
    fn same_rank_constraint_equalizes_layers() {
        let mut ir = make_simple_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::TD);
        ir.constraints.push(LayoutConstraint::SameRank {
            node_ids: vec!["A".to_string(), "C".to_string()],
            span: empty_span(),
        });
        let layout = layout_diagram(&ir, &default_config());
        let rank_a = layout.nodes.iter().find(|n| n.node_idx == 0).unwrap().rank;
        let rank_c = layout.nodes.iter().find(|n| n.node_idx == 2).unwrap().rank;
        assert_eq!(rank_a, rank_c, "A and C should be on the same rank");
    }

    #[test]
    fn min_length_constraint_ensures_rank_gap() {
        let mut ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TD);
        ir.constraints.push(LayoutConstraint::MinLength {
            from_id: "A".to_string(),
            to_id: "B".to_string(),
            min_len: 3,
            span: empty_span(),
        });
        let layout = layout_diagram(&ir, &default_config());
        let rank_a = layout.nodes.iter().find(|n| n.node_idx == 0).unwrap().rank;
        let rank_b = layout.nodes.iter().find(|n| n.node_idx == 1).unwrap().rank;
        assert!(
            rank_b >= rank_a + 3,
            "rank gap should be >= 3, got {} - {} = {}",
            rank_b,
            rank_a,
            rank_b - rank_a
        );
    }

    #[test]
    fn pin_constraint_overrides_position() {
        let mut ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TD);
        ir.constraints.push(LayoutConstraint::Pin {
            node_id: "A".to_string(),
            x: 42.0,
            y: 99.0,
            span: empty_span(),
        });
        let layout = layout_diagram(&ir, &default_config());
        let node_a = layout.nodes.iter().find(|n| n.node_idx == 0).unwrap();
        assert!(
            (node_a.rect.x - 42.0).abs() < 1e-9,
            "pinned x should be 42.0, got {}",
            node_a.rect.x
        );
        assert!(
            (node_a.rect.y - 99.0).abs() < 1e-9,
            "pinned y should be 99.0, got {}",
            node_a.rect.y
        );
    }

    #[test]
    fn order_constraint_enforces_sequence() {
        let mut ir = make_simple_ir(&["A", "B", "C"], &[], GraphDirection::TD);
        ir.constraints.push(LayoutConstraint::SameRank {
            node_ids: vec!["A".to_string(), "B".to_string(), "C".to_string()],
            span: empty_span(),
        });
        ir.constraints.push(LayoutConstraint::OrderInRank {
            node_ids: vec!["C".to_string(), "A".to_string(), "B".to_string()],
            span: empty_span(),
        });
        let layout = layout_diagram(&ir, &default_config());
        let order_a = layout.nodes.iter().find(|n| n.node_idx == 0).unwrap().order;
        let order_b = layout.nodes.iter().find(|n| n.node_idx == 1).unwrap().order;
        let order_c = layout.nodes.iter().find(|n| n.node_idx == 2).unwrap().order;
        assert!(
            order_c < order_a && order_a < order_b,
            "expected C < A < B ordering, got C={order_c} A={order_a} B={order_b}"
        );
    }

    #[test]
    fn constraints_with_unknown_nodes_are_harmless() {
        let mut ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TD);
        ir.constraints.push(LayoutConstraint::SameRank {
            node_ids: vec!["X".to_string(), "Y".to_string()],
            span: empty_span(),
        });
        ir.constraints.push(LayoutConstraint::Pin {
            node_id: "Z".to_string(),
            x: 0.0,
            y: 0.0,
            span: empty_span(),
        });
        let layout = layout_diagram(&ir, &default_config());
        assert_eq!(layout.nodes.len(), 2);
    }

    #[test]
    fn full_pipeline_with_constraints() {
        let src = "graph TD\n%%{ ftui:rank=same B, D }%%\nA --> B\nA --> C\nC --> D\n";
        let ast = crate::mermaid::parse(src).expect("parse");
        let config = MermaidConfig::default();
        let ir_parse = crate::mermaid::normalize_ast_to_ir(
            &ast,
            &config,
            &crate::mermaid::MermaidCompatibilityMatrix::default(),
            &crate::mermaid::MermaidFallbackPolicy::default(),
        );
        assert_eq!(ir_parse.ir.constraints.len(), 1);
        let layout = layout_diagram(&ir_parse.ir, &config);
        let b_idx = ir_parse.ir.nodes.iter().position(|n| n.id == "B").unwrap();
        let d_idx = ir_parse.ir.nodes.iter().position(|n| n.id == "D").unwrap();
        let rank_b = layout
            .nodes
            .iter()
            .find(|n| n.node_idx == b_idx)
            .unwrap()
            .rank;
        let rank_d = layout
            .nodes
            .iter()
            .find(|n| n.node_idx == d_idx)
            .unwrap()
            .rank;
        assert_eq!(rank_b, rank_d, "B and D should be on same rank");
    }

    #[test]
    fn edge_bundling_disabled_leaves_edges_unchanged() {
        let ir = make_simple_ir(
            &["A", "B"],
            &[(0, 1), (0, 1), (0, 1), (0, 1), (0, 1)],
            GraphDirection::TB,
        );
        let config = MermaidConfig {
            edge_bundling: false,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);
        assert_eq!(
            layout.edges.len(),
            5,
            "with bundling disabled, all edges should remain"
        );
        for edge in &layout.edges {
            assert_eq!(edge.bundle_count, 1);
            assert!(edge.bundle_members.is_empty());
        }
    }

    #[test]
    fn edge_bundling_single_edge_no_change() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 2,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);
        assert_eq!(layout.edges.len(), 1);
        assert_eq!(layout.edges[0].bundle_count, 1);
        assert!(layout.edges[0].bundle_members.is_empty());
    }

    #[test]
    fn edge_bundling_exact_min_threshold_bundles() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1), (0, 1), (0, 1)], GraphDirection::TB);
        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 3,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);
        assert_eq!(
            layout.edges.len(),
            1,
            "3 edges at min_count=3 should bundle"
        );
        assert_eq!(layout.edges[0].bundle_count, 3);
        assert_eq!(layout.edges[0].bundle_members.len(), 3);
    }

    #[test]
    fn edge_bundling_opposite_direction_not_bundled() {
        let ir = make_simple_ir(
            &["A", "B"],
            &[(0, 1), (0, 1), (0, 1), (1, 0), (1, 0), (1, 0)],
            GraphDirection::TB,
        );
        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 3,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);
        assert_eq!(
            layout.edges.len(),
            2,
            "opposite-direction edges should form separate bundles"
        );
        for edge in &layout.edges {
            assert_eq!(edge.bundle_count, 3);
        }
    }

    #[test]
    fn edge_bundling_different_arrows_not_bundled() {
        let mut ir = make_simple_ir(
            &["A", "B"],
            &[(0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1)],
            GraphDirection::TB,
        );
        for edge in ir.edges.iter_mut().skip(3) {
            edge.arrow = "-.->".to_string();
        }

        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 3,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);
        assert_eq!(
            layout.edges.len(),
            2,
            "different arrow styles should not be bundled together"
        );
    }

    #[test]
    fn edge_bundling_preserves_non_parallel_edges() {
        let ir = make_simple_ir(
            &["A", "B", "C"],
            &[(0, 1), (0, 1), (0, 1), (0, 2), (1, 2)],
            GraphDirection::TB,
        );
        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 3,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);
        assert_eq!(
            layout.edges.len(),
            3,
            "should have 1 bundled + 2 non-bundled edges"
        );

        let bundled: Vec<_> = layout.edges.iter().filter(|e| e.bundle_count > 1).collect();
        assert_eq!(bundled.len(), 1);
        assert_eq!(bundled[0].bundle_count, 3);

        let unbundled: Vec<_> = layout
            .edges
            .iter()
            .filter(|e| e.bundle_count == 1)
            .collect();
        assert_eq!(unbundled.len(), 2);
    }

    #[test]
    fn edge_bundling_canonical_uses_lowest_edge_idx() {
        let ir = make_simple_ir(
            &["A", "B"],
            &[(0, 1), (0, 1), (0, 1), (0, 1)],
            GraphDirection::TB,
        );
        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 2,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);
        assert_eq!(layout.edges.len(), 1);
        assert_eq!(
            layout.edges[0].edge_idx, 0,
            "canonical edge should always be the lowest index"
        );
    }

    #[test]
    fn edge_bundling_lr_direction_offsets_y() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1), (0, 1)], GraphDirection::LR);
        let spacing = LayoutSpacing::default();

        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 3,
            ..MermaidConfig::default()
        };

        let layout = layout_diagram_with_spacing(&ir, &config, &spacing);
        if layout.edges.len() == 2 {
            let dy = (layout.edges[1].waypoints[0].y - layout.edges[0].waypoints[0].y).abs();
            let dx = (layout.edges[1].waypoints[0].x - layout.edges[0].waypoints[0].x).abs();
            assert!(
                dy > dx || dy > 0.5,
                "LR direction should offset in Y, got dx={dx} dy={dy}"
            );
        }
    }

    #[test]
    fn edge_bundling_min_count_clamped_to_two() {
        let ir = make_simple_ir(&["A", "B"], &[(0, 1), (0, 1)], GraphDirection::TB);
        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 1,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);
        assert_eq!(
            layout.edges.len(),
            1,
            "min_count=1 clamped to 2 should bundle 2 parallel edges"
        );
        assert_eq!(layout.edges[0].bundle_count, 2);
    }

    #[test]
    fn edge_bundling_bundle_members_sorted() {
        let ir = make_simple_ir(
            &["A", "B"],
            &[(0, 1), (0, 1), (0, 1), (0, 1)],
            GraphDirection::TB,
        );
        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 2,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);
        assert_eq!(layout.edges.len(), 1);
        let members = &layout.edges[0].bundle_members;
        assert!(!members.is_empty());
        let mut sorted = members.clone();
        sorted.sort();
        assert_eq!(members, &sorted, "bundle_members should be sorted");
    }

    #[test]
    fn edge_bundling_self_loop_edges_no_crash() {
        let ir = make_simple_ir(
            &["A", "B"],
            &[(0, 0), (0, 0), (0, 0), (0, 1)],
            GraphDirection::TB,
        );
        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 3,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);
        assert!(!layout.edges.is_empty(), "should have at least one edge");
    }

    #[test]
    fn edge_bundling_non_graph_diagram_skipped() {
        let mut ir = make_simple_ir(&["A", "B"], &[(0, 1), (0, 1), (0, 1)], GraphDirection::TB);
        ir.diagram_type = DiagramType::Sequence;
        ir.meta.diagram_type = DiagramType::Sequence;

        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 3,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);
        for edge in &layout.edges {
            assert_eq!(
                edge.bundle_count, 1,
                "non-graph diagrams should not have bundled edges"
            );
        }
    }

    #[test]
    fn edge_bundling_multi_group_correct_counts() {
        let ir = make_simple_ir(
            &["A", "B", "C", "D"],
            &[
                (0, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (2, 3),
                (2, 3),
                (2, 3),
                (0, 3),
                (0, 3),
            ],
            GraphDirection::TB,
        );
        let config = MermaidConfig {
            edge_bundling: true,
            edge_bundle_min_count: 3,
            ..MermaidConfig::default()
        };
        let layout = layout_diagram(&ir, &config);

        let bundled: Vec<_> = layout.edges.iter().filter(|e| e.bundle_count > 1).collect();
        let unbundled: Vec<_> = layout
            .edges
            .iter()
            .filter(|e| e.bundle_count == 1)
            .collect();

        assert_eq!(bundled.len(), 2, "should have 2 bundle groups");
        assert_eq!(unbundled.len(), 2, "should have 2 unbundled offset edges");

        let mut bundle_counts: Vec<usize> = bundled.iter().map(|e| e.bundle_count).collect();
        bundle_counts.sort();
        assert_eq!(bundle_counts, vec![3, 4]);
    }

    #[test]
    fn requirement_layout_empty() {
        let mut ir = make_simple_ir(&[], &[], GraphDirection::TB);
        ir.diagram_type = DiagramType::Requirement;
        let config = MermaidConfig::default();
        let layout = layout_diagram(&ir, &config);
        assert!(layout.nodes.is_empty());
        assert!(layout.edges.is_empty());
    }

    #[test]
    fn requirement_layout_single_entity() {
        let mut ir = make_simple_ir(&["req1"], &[], GraphDirection::TB);
        ir.diagram_type = DiagramType::Requirement;
        let config = MermaidConfig::default();
        let layout = layout_diagram(&ir, &config);
        assert_eq!(layout.nodes.len(), 1);
        assert!(
            layout.nodes[0].rect.width >= 10.0,
            "entity box should be wide"
        );
    }

    #[test]
    fn requirement_layout_with_relations() {
        let mut ir = make_simple_ir(
            &["req1", "elem1", "req2"],
            &[(0, 1), (1, 2)],
            GraphDirection::TB,
        );
        ir.diagram_type = DiagramType::Requirement;
        let config = MermaidConfig::default();
        let layout = layout_diagram(&ir, &config);
        assert_eq!(layout.nodes.len(), 3);
        assert_eq!(layout.edges.len(), 2);
        let y0 = layout.nodes[0].rect.y;
        let y1 = layout.nodes[1].rect.y;
        assert!(y0 < y1 || y1 < y0, "nodes at different ranks");
    }

    #[test]
    fn requirement_layout_deterministic() {
        let mut ir = make_simple_ir(
            &["r1", "r2", "e1", "e2"],
            &[(0, 2), (1, 3), (2, 3)],
            GraphDirection::TB,
        );
        ir.diagram_type = DiagramType::Requirement;
        let config = MermaidConfig::default();
        let layout1 = layout_diagram(&ir, &config);
        let layout2 = layout_diagram(&ir, &config);
        for (a, b) in layout1.nodes.iter().zip(layout2.nodes.iter()) {
            assert_eq!(a.rect.x, b.rect.x);
            assert_eq!(a.rect.y, b.rect.y);
        }
    }
}

// ── Label placement & collision avoidance tests ─────────────────────
#[cfg(test)]
mod label_tests {
    use super::*;
    use crate::mermaid::*;

    fn default_config() -> MermaidConfig {
        MermaidConfig::default()
    }

    fn empty_span() -> Span {
        Span {
            start: Position {
                line: 0,
                col: 0,
                byte: 0,
            },
            end: Position {
                line: 0,
                col: 0,
                byte: 0,
            },
        }
    }

    fn make_labeled_ir(
        nodes: &[(&str, Option<&str>)],
        edges: &[(usize, usize, Option<&str>)],
        direction: GraphDirection,
    ) -> MermaidDiagramIr {
        let mut labels = Vec::new();

        let ir_nodes: Vec<IrNode> = nodes
            .iter()
            .map(|(id, label_text)| {
                let label = label_text.map(|t| {
                    let idx = labels.len();
                    labels.push(IrLabel {
                        text: t.to_string(),
                        span: empty_span(),
                    });
                    IrLabelId(idx)
                });
                IrNode {
                    id: id.to_string(),
                    label,
                    shape: NodeShape::Rect,
                    classes: vec![],
                    style_ref: None,
                    span_primary: empty_span(),
                    span_all: vec![],
                    implicit: false,
                    members: vec![],
                }
            })
            .collect();

        let ir_edges: Vec<IrEdge> = edges
            .iter()
            .map(|(from, to, label_text)| {
                let label = label_text.map(|t| {
                    let idx = labels.len();
                    labels.push(IrLabel {
                        text: t.to_string(),
                        span: empty_span(),
                    });
                    IrLabelId(idx)
                });
                IrEdge {
                    from: IrEndpoint::Node(IrNodeId(*from)),
                    to: IrEndpoint::Node(IrNodeId(*to)),
                    arrow: "-->".to_string(),
                    label,
                    style_ref: None,
                    span: empty_span(),
                }
            })
            .collect();

        MermaidDiagramIr {
            diagram_type: DiagramType::Graph,
            direction,
            nodes: ir_nodes,
            edges: ir_edges,
            ports: vec![],
            clusters: vec![],
            labels,
            pie_entries: vec![],
            pie_title: None,
            pie_show_data: false,
            style_refs: vec![],
            links: vec![],
            meta: MermaidDiagramMeta {
                diagram_type: DiagramType::Graph,
                direction,
                support_level: MermaidSupportLevel::Supported,
                init: MermaidInitParse::default(),
                theme_overrides: MermaidThemeOverrides::default(),
                guard: MermaidGuardReport::default(),
            },
            constraints: vec![],
        }
    }

    // ── Collision primitives ──────────────────────────────────────────

    #[test]
    fn rects_overlap_with_margin() {
        let a = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 5.0,
            height: 5.0,
        };
        let c = LayoutRect {
            x: 5.5,
            y: 0.0,
            width: 5.0,
            height: 5.0,
        };
        assert!(!rects_overlap(&a, &c, 0.0));
        assert!(rects_overlap(&a, &c, 1.0));
    }

    // ── Edge midpoint edge cases ────────────────────────────────────

    #[test]
    fn edge_midpoint_empty() {
        let mid = edge_midpoint(&[]);
        assert!((mid.x).abs() < f64::EPSILON);
        assert!((mid.y).abs() < f64::EPSILON);
    }

    #[test]
    fn edge_midpoint_single() {
        let mid = edge_midpoint(&[LayoutPoint { x: 3.0, y: 7.0 }]);
        assert!((mid.x - 3.0).abs() < f64::EPSILON);
        assert!((mid.y - 7.0).abs() < f64::EPSILON);
    }

    // ── Multi-line text measurement ─────────────────────────────────

    #[test]
    fn measure_text_empty_returns_zero() {
        let cfg = LabelPlacementConfig::default();
        let (w, h, tr) = measure_text("", &cfg);
        assert!((w).abs() < f64::EPSILON);
        assert!((h).abs() < f64::EPSILON);
        assert!(!tr);
    }

    #[test]
    fn measure_text_single_line_fits() {
        let cfg = LabelPlacementConfig {
            max_label_width: 20.0,
            char_width: 1.0,
            line_height: 1.0,
            max_lines: 3,
            ..Default::default()
        };
        let (w, h, tr) = measure_text("hello", &cfg);
        assert!((w - 5.0).abs() < f64::EPSILON);
        assert!((h - 1.0).abs() < f64::EPSILON);
        assert!(!tr);
    }

    #[test]
    fn measure_text_wraps_long_line() {
        let cfg = LabelPlacementConfig {
            max_label_width: 5.0,
            char_width: 1.0,
            line_height: 1.0,
            max_lines: 5,
            max_label_height: 10.0,
            ..Default::default()
        };
        let (w, h, tr) = measure_text("abcdefghij", &cfg);
        assert!((w - 5.0).abs() < f64::EPSILON);
        assert!((h - 2.0).abs() < f64::EPSILON);
        assert!(!tr);
    }

    #[test]
    fn measure_text_truncates_vertically() {
        let cfg = LabelPlacementConfig {
            max_label_width: 3.0,
            char_width: 1.0,
            line_height: 1.0,
            max_lines: 2,
            max_label_height: 10.0,
            ..Default::default()
        };
        let (_w, h, tr) = measure_text("abcdefghi", &cfg);
        assert!((h - 2.0).abs() < f64::EPSILON);
        assert!(tr, "should be truncated vertically");
    }

    #[test]
    fn measure_text_with_newlines() {
        let cfg = LabelPlacementConfig {
            max_label_width: 20.0,
            char_width: 1.0,
            line_height: 1.0,
            max_lines: 5,
            max_label_height: 10.0,
            ..Default::default()
        };
        let (w, h, _) = measure_text("abc\nde\nfghij", &cfg);
        assert!((w - 5.0).abs() < f64::EPSILON);
        assert!((h - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn measure_text_cjk_display_width() {
        let cfg = LabelPlacementConfig {
            max_label_width: 20.0,
            char_width: 1.0,
            line_height: 1.0,
            max_lines: 5,
            max_label_height: 10.0,
            ..Default::default()
        };
        // Each CJK character is 2 display columns wide
        let (w, _h, _trunc) = measure_text("漢字", &cfg);
        assert!(
            (w - 4.0).abs() < f64::EPSILON,
            "'漢字' should be 4 cols, got {w}"
        );
    }

    #[test]
    fn measure_text_cjk_wrapping() {
        let cfg = LabelPlacementConfig {
            max_label_width: 5.0,
            char_width: 1.0,
            line_height: 1.0,
            max_lines: 10,
            max_label_height: 20.0,
            ..Default::default()
        };
        // "漢字テスト" = 10 display cols, wraps into multiple lines at width 5
        let (_w, h, _trunc) = measure_text("漢字テスト", &cfg);
        assert!(
            h > 1.0,
            "CJK text should wrap to multiple lines at width 5, got height {h}"
        );
    }

    #[test]
    fn measure_text_mixed_ascii_cjk() {
        let cfg = LabelPlacementConfig {
            max_label_width: 20.0,
            char_width: 1.0,
            line_height: 1.0,
            max_lines: 5,
            max_label_height: 10.0,
            ..Default::default()
        };
        // "Hi" = 2 cols, "漢字" = 4 cols, total = 6
        let (w, _h, _trunc) = measure_text("Hi漢字", &cfg);
        assert!(
            (w - 6.0).abs() < f64::EPSILON,
            "'Hi漢字' should be 6 cols, got {w}"
        );
    }

    // ── Edge segment rects ──────────────────────────────────────────

    #[test]
    fn edge_segment_rects_horizontal() {
        let wps = vec![
            LayoutPoint { x: 0.0, y: 0.0 },
            LayoutPoint { x: 10.0, y: 0.0 },
        ];
        let rects = edge_segment_rects(&wps, 1.0);
        assert_eq!(rects.len(), 1);
        assert!((rects[0].x - (-0.5)).abs() < f64::EPSILON);
        assert!((rects[0].width - 11.0).abs() < f64::EPSILON);
    }

    #[test]
    fn edge_segment_rects_empty() {
        assert!(edge_segment_rects(&[], 1.0).is_empty());
    }

    #[test]
    fn edge_segment_rects_single_point() {
        assert!(edge_segment_rects(&[LayoutPoint { x: 5.0, y: 5.0 }], 1.0).is_empty());
    }

    // ── Full label placement scenarios ──────────────────────────────

    #[test]
    fn labels_avoid_edge_paths() {
        let ir = make_labeled_ir(
            &[("A", None), ("B", None), ("C", None)],
            &[(0, 1, Some("lbl1")), (1, 2, Some("lbl2"))],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let cfg = LabelPlacementConfig {
            offset_step: 0.5,
            max_offset: 10.0,
            ..Default::default()
        };
        let result = place_labels(&ir, &layout, &cfg);
        assert_eq!(result.edge_labels.len(), 2);
        assert!(
            !rects_overlap(
                &result.edge_labels[0].rect,
                &result.edge_labels[1].rect,
                0.0
            ),
            "edge labels should not overlap"
        );
    }

    #[test]
    fn collider_variants_are_valid() {
        let ir = make_labeled_ir(
            &[("A", None), ("B", None)],
            &[(0, 1, Some("test"))],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let result = place_labels(&ir, &layout, &LabelPlacementConfig::default());
        for c in &result.collisions {
            match &c.collider {
                LabelCollider::Edge(_) | LabelCollider::Node(_) | LabelCollider::Label(_) => {}
            }
        }
        assert_eq!(result.edge_labels.len(), 1);
    }

    #[test]
    fn dense_labels_no_pairwise_overlaps() {
        let ir = make_labeled_ir(
            &[("A", None), ("B", None), ("C", None), ("D", None)],
            &[
                (0, 1, Some("AB")),
                (0, 2, Some("AC")),
                (1, 3, Some("BD")),
                (2, 3, Some("CD")),
            ],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let cfg = LabelPlacementConfig {
            offset_step: 0.5,
            max_offset: 20.0,
            ..Default::default()
        };
        let result = place_labels(&ir, &layout, &cfg);
        assert_eq!(result.edge_labels.len(), 4);
        for i in 0..result.edge_labels.len() {
            for j in (i + 1)..result.edge_labels.len() {
                assert!(
                    !rects_overlap(
                        &result.edge_labels[i].rect,
                        &result.edge_labels[j].rect,
                        0.0
                    ),
                    "labels {i} and {j} should not overlap"
                );
            }
        }
    }

    #[test]
    fn node_and_edge_labels_no_overlap() {
        let ir = make_labeled_ir(
            &[("A", Some("Node A")), ("B", Some("Node B"))],
            &[(0, 1, Some("edge"))],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let result = place_labels(&ir, &layout, &LabelPlacementConfig::default());
        assert_eq!(result.node_labels.len(), 2);
        assert_eq!(result.edge_labels.len(), 1);
        for nl in &result.node_labels {
            assert!(
                !rects_overlap(&result.edge_labels[0].rect, &nl.rect, 0.0),
                "edge label should not overlap node label"
            );
        }
    }

    // ── Leader lines ────────────────────────────────────────────────

    #[test]
    fn leader_line_for_large_offset() {
        let ir = make_labeled_ir(
            &[("A", Some("Wide Node Label")), ("B", None)],
            &[(0, 1, Some("edge label"))],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let cfg = LabelPlacementConfig {
            leader_line_threshold: 1.0,
            offset_step: 0.5,
            max_offset: 10.0,
            ..Default::default()
        };
        let result = place_labels(&ir, &layout, &cfg);
        for label in &result.edge_labels {
            if label.was_offset
                && let Some((anchor, target)) = &label.leader_line
            {
                assert!(anchor.x.is_finite());
                assert!(target.x.is_finite());
            }
        }
    }

    // ── Legend spillover ─────────────────────────────────────────────

    #[test]
    fn legend_spillover_when_enabled() {
        let ir = make_labeled_ir(
            &[("A", None), ("B", None)],
            &[(0, 1, Some("label"))],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let cfg = LabelPlacementConfig {
            max_offset: 0.0,
            legend_enabled: true,
            ..Default::default()
        };
        let result = place_labels(&ir, &layout, &cfg);
        let total = result.edge_labels.len() + result.legend_labels.len();
        assert_eq!(total, 1);
        for legend in &result.legend_labels {
            assert!(legend.spilled_to_legend);
            assert!(legend.leader_line.is_some());
        }
    }

    #[test]
    fn legend_disabled_by_default() {
        let ir = make_labeled_ir(
            &[("A", None), ("B", None)],
            &[(0, 1, Some("label"))],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let result = place_labels(&ir, &layout, &LabelPlacementConfig::default());
        assert!(result.legend_labels.is_empty());
    }

    // ── Label reservation rects ─────────────────────────────────────

    #[test]
    fn reservation_rects_include_all_labels() {
        let ir = make_labeled_ir(
            &[("A", Some("Node")), ("B", None)],
            &[(0, 1, Some("edge"))],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let result = place_labels(&ir, &layout, &LabelPlacementConfig::default());
        let rects = label_reservation_rects(&result);
        assert_eq!(
            rects.len(),
            result.node_labels.len() + result.edge_labels.len()
        );
    }

    // ── Direction consistency ────────────────────────────────────────

    #[test]
    fn labels_placed_in_all_directions() {
        for dir in [
            GraphDirection::TB,
            GraphDirection::BT,
            GraphDirection::LR,
            GraphDirection::RL,
        ] {
            let ir = make_labeled_ir(&[("A", None), ("B", None)], &[(0, 1, Some("lbl"))], dir);
            let layout = layout_diagram(&ir, &default_config());
            let result = place_labels(&ir, &layout, &LabelPlacementConfig::default());
            assert_eq!(result.edge_labels.len(), 1, "direction {dir:?}");
            assert!(result.edge_labels[0].rect.width > 0.0);
        }
    }

    // ── Determinism under collisions ────────────────────────────────

    #[test]
    fn dense_collision_deterministic() {
        let ir = make_labeled_ir(
            &[("A", None), ("B", None), ("C", None), ("D", None)],
            &[
                (0, 1, Some("AB")),
                (0, 2, Some("AC")),
                (1, 3, Some("BD")),
                (2, 3, Some("CD")),
            ],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir, &default_config());
        let cfg = LabelPlacementConfig {
            offset_step: 0.5,
            max_offset: 20.0,
            ..Default::default()
        };
        let r1 = place_labels(&ir, &layout, &cfg);
        let r2 = place_labels(&ir, &layout, &cfg);
        assert_eq!(r1.edge_labels.len(), r2.edge_labels.len());
        for (l1, l2) in r1.edge_labels.iter().zip(r2.edge_labels.iter()) {
            assert_eq!(l1, l2, "dense placement must be deterministic");
        }
        assert_eq!(r1.collisions.len(), r2.collisions.len());
    }

    #[test]
    fn offset_candidates_complete_set() {
        let offsets = generate_offset_candidates(1.0, 1.0);
        assert_eq!(offsets.len(), 9);
        assert_eq!(offsets[0], (0.0, 0.0));
        assert_eq!(offsets[1], (0.0, -1.0));
        assert_eq!(offsets[2], (1.0, 0.0));
    }

    // --- Legend / Footnote layout tests (bd-1oa1y) ---

    #[test]
    fn legend_empty_input_returns_empty() {
        let bbox = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 20.0,
        };
        let legend = compute_legend_layout(&bbox, &[], &LegendConfig::default());
        assert!(legend.is_empty());
        assert_eq!(legend.entries.len(), 0);
        assert_eq!(legend.overflow_count, 0);
    }

    #[test]
    fn legend_below_placement_basic() {
        let bbox = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 20.0,
        };
        let footnotes = vec![
            "[1] https://example.com (A)".to_string(),
            "[2] https://other.com (B)".to_string(),
        ];
        let config = LegendConfig::default();
        let legend = compute_legend_layout(&bbox, &footnotes, &config);

        assert!(!legend.is_empty());
        assert_eq!(legend.entries.len(), 2);
        assert_eq!(legend.placement, LegendPlacement::Below);
        assert_eq!(legend.overflow_count, 0);
        // Legend should be below the diagram.
        assert!(legend.region.y >= bbox.y + bbox.height);
    }

    #[test]
    fn legend_right_placement_basic() {
        let bbox = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 20.0,
        };
        let footnotes = vec!["[1] https://example.com (A)".to_string()];
        let config = LegendConfig {
            placement: LegendPlacement::Right,
            ..LegendConfig::default()
        };
        let legend = compute_legend_layout(&bbox, &footnotes, &config);

        assert_eq!(legend.placement, LegendPlacement::Right);
        // Legend should be to the right of the diagram.
        assert!(legend.region.x >= bbox.x + bbox.width);
    }

    #[test]
    fn legend_no_overlap_with_diagram() {
        let bbox = LayoutRect {
            x: 5.0,
            y: 5.0,
            width: 40.0,
            height: 20.0,
        };
        let footnotes: Vec<String> = (0..5)
            .map(|i| format!("[{}] https://example.com/page{} (Node{})", i + 1, i, i))
            .collect();

        for placement in [LegendPlacement::Below, LegendPlacement::Right] {
            let config = LegendConfig {
                placement,
                ..LegendConfig::default()
            };
            let legend = compute_legend_layout(&bbox, &footnotes, &config);

            // No overlap: legend region must not intersect diagram bbox.
            let no_overlap = legend.region.x + legend.region.width <= bbox.x
                || legend.region.x >= bbox.x + bbox.width
                || legend.region.y + legend.region.height <= bbox.y
                || legend.region.y >= bbox.y + bbox.height;
            assert!(no_overlap, "legend {:?} overlaps diagram", placement);
        }
    }

    #[test]
    fn legend_max_height_truncates_entries() {
        let bbox = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 20.0,
        };
        // 20 footnotes but max_height only allows ~3 lines.
        let footnotes: Vec<String> = (0..20)
            .map(|i| format!("[{}] https://example.com/{}", i + 1, i))
            .collect();
        let config = LegendConfig {
            max_height: 3.5, // 0.5 padding + 3 lines of 1.0
            ..LegendConfig::default()
        };
        let legend = compute_legend_layout(&bbox, &footnotes, &config);

        assert!(legend.entries.len() < 20);
        assert!(legend.overflow_count > 0);
        assert_eq!(legend.entries.len() + legend.overflow_count, 20);
    }

    #[test]
    fn legend_entry_truncation() {
        let bbox = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 30.0,
            height: 10.0,
        };
        let long_url =
            "[1] https://very-long-domain-name.example.com/this/is/a/very/long/path (LongNodeName)"
                .to_string();
        let footnotes = vec![long_url.clone()];
        let config = LegendConfig {
            max_entry_chars: 30,
            ..LegendConfig::default()
        };
        let legend = compute_legend_layout(&bbox, &footnotes, &config);

        assert_eq!(legend.entries.len(), 1);
        assert!(legend.entries[0].was_truncated);
        assert!(legend.entries[0].text.ends_with('…'));
        assert!(visual_width(&legend.entries[0].text) <= 30);
    }

    #[test]
    fn legend_entries_are_vertically_stacked() {
        let bbox = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 20.0,
        };
        let footnotes = vec![
            "[1] https://a.com (A)".to_string(),
            "[2] https://b.com (B)".to_string(),
            "[3] https://c.com (C)".to_string(),
        ];
        let config = LegendConfig::default();
        let legend = compute_legend_layout(&bbox, &footnotes, &config);

        assert_eq!(legend.entries.len(), 3);
        // Each entry should be below the previous one.
        for i in 1..legend.entries.len() {
            assert!(
                legend.entries[i].rect.y > legend.entries[i - 1].rect.y,
                "entry {} not below entry {}",
                i,
                i - 1
            );
        }
    }

    #[test]
    fn legend_deterministic() {
        let bbox = LayoutRect {
            x: 3.0,
            y: 5.0,
            width: 50.0,
            height: 30.0,
        };
        let footnotes: Vec<String> = (0..8)
            .map(|i| format!("[{}] https://example.com/{}", i + 1, i))
            .collect();
        let config = LegendConfig::default();

        let l1 = compute_legend_layout(&bbox, &footnotes, &config);
        let l2 = compute_legend_layout(&bbox, &footnotes, &config);

        assert_eq!(l1, l2, "legend layout must be deterministic");
    }

    #[test]
    fn truncate_legend_text_short() {
        let (text, truncated) = truncate_legend_text("hello", 10);
        assert_eq!(text, "hello");
        assert!(!truncated);
    }

    #[test]
    fn truncate_legend_text_exact() {
        let (text, truncated) = truncate_legend_text("hello", 5);
        assert_eq!(text, "hello");
        assert!(!truncated);
    }

    #[test]
    fn truncate_legend_text_long() {
        let (text, truncated) = truncate_legend_text("hello world!", 8);
        assert_eq!(text, "hello w…");
        assert!(truncated);
        assert_eq!(visual_width(&text), 8);
    }

    #[test]
    fn truncate_legend_text_very_short_max() {
        let (text, truncated) = truncate_legend_text("hello", 2);
        assert_eq!(text, "h…");
        assert!(truncated);
        assert_eq!(visual_width(&text), 2);
    }

    #[test]
    fn truncate_legend_text_unicode_safe() {
        // CJK characters are width 2 each. max_cols=4, budget=3, so 1 CJK char (w=2) fits + ellipsis
        let (text, truncated) = truncate_legend_text("猫の手も借りたい", 4);
        assert!(truncated);
        assert!(visual_width(&text) <= 4);
    }

    #[test]
    fn build_link_footnotes_basic() {
        let links = vec![
            IrLink {
                kind: LinkKind::Click,
                target: IrNodeId(0),
                url: "https://example.com".to_string(),
                tooltip: Some("Go here".to_string()),
                sanitize_outcome: LinkSanitizeOutcome::Allowed,
                span: empty_span(),
            },
            IrLink {
                kind: LinkKind::Link,
                target: IrNodeId(1),
                url: "https://other.com".to_string(),
                tooltip: None,
                sanitize_outcome: LinkSanitizeOutcome::Allowed,
                span: empty_span(),
            },
        ];
        let nodes = vec![
            IrNode {
                id: "A".to_string(),
                label: None,
                shape: NodeShape::Rect,
                classes: vec![],
                style_ref: None,
                span_primary: empty_span(),
                span_all: vec![],
                implicit: false,
                members: vec![],
            },
            IrNode {
                id: "B".to_string(),
                label: None,
                shape: NodeShape::Rect,
                classes: vec![],
                style_ref: None,
                span_primary: empty_span(),
                span_all: vec![],
                implicit: false,
                members: vec![],
            },
        ];

        let footnotes = build_link_footnotes(&links, &nodes);
        assert_eq!(footnotes.len(), 2);
        assert_eq!(footnotes[0], "[1] https://example.com (A - Go here)");
        assert_eq!(footnotes[1], "[2] https://other.com (B)");
    }

    #[test]
    fn build_link_footnotes_skips_blocked() {
        let links = vec![
            IrLink {
                kind: LinkKind::Click,
                target: IrNodeId(0),
                url: "https://safe.com".to_string(),
                tooltip: None,
                sanitize_outcome: LinkSanitizeOutcome::Allowed,
                span: empty_span(),
            },
            IrLink {
                kind: LinkKind::Click,
                target: IrNodeId(1),
                url: "javascript:xss".to_string(),
                tooltip: None,
                sanitize_outcome: LinkSanitizeOutcome::Blocked,
                span: empty_span(),
            },
        ];
        let nodes = vec![
            IrNode {
                id: "A".to_string(),
                label: None,
                shape: NodeShape::Rect,
                classes: vec![],
                style_ref: None,
                span_primary: empty_span(),
                span_all: vec![],
                implicit: false,
                members: vec![],
            },
            IrNode {
                id: "B".to_string(),
                label: None,
                shape: NodeShape::Rect,
                classes: vec![],
                style_ref: None,
                span_primary: empty_span(),
                span_all: vec![],
                implicit: false,
                members: vec![],
            },
        ];

        let footnotes = build_link_footnotes(&links, &nodes);
        assert_eq!(footnotes.len(), 1);
        assert_eq!(footnotes[0], "[1] https://safe.com (A)");
    }

    #[test]
    fn legend_gap_respected() {
        let bbox = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 20.0,
        };
        let footnotes = vec!["[1] https://example.com (A)".to_string()];
        let config = LegendConfig {
            gap: 3.0,
            ..LegendConfig::default()
        };
        let legend = compute_legend_layout(&bbox, &footnotes, &config);

        // Legend should be at least gap distance from diagram.
        assert!(
            legend.region.y >= bbox.y + bbox.height + config.gap - 0.01,
            "legend gap not respected"
        );
    }
}

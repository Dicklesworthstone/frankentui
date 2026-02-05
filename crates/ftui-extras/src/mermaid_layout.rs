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

use crate::diagram::visual_width;
use crate::mermaid::{
    DiagramType, GraphDirection, IrEndpoint, IrNodeId, MermaidConfig, MermaidDegradationPlan,
    MermaidDiagramIr, MermaidFidelity, append_jsonl_line,
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

/// Compute barycenter of a node relative to the previous rank.
fn barycenter(_node: usize, prev_order: &[usize], neighbors: &[usize]) -> f64 {
    if neighbors.is_empty() {
        return f64::MAX;
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    for &nb in neighbors {
        if let Some(pos) = prev_order.iter().position(|&x| x == nb) {
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
) {
    if r == 0 || r >= rank_order.len() {
        return;
    }
    let prev = rank_order[r - 1].clone();

    let scored: Vec<(usize, f64)> = rank_order[r]
        .iter()
        .map(|&v| {
            let bc = barycenter(v, &prev, &graph.rev[v]);
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
) {
    if r + 1 >= rank_order.len() {
        return;
    }
    let next = rank_order[r + 1].clone();

    let scored: Vec<(usize, f64)> = rank_order[r]
        .iter()
        .map(|&v| {
            let bc = barycenter(v, &next, &graph.adj[v]);
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

/// Count edge crossings between two adjacent ranks.
fn count_crossings(rank_a: &[usize], rank_b: &[usize], graph: &LayoutGraph) -> usize {
    // Build position maps.
    let mut pos_b = vec![0usize; graph.n];
    for (i, &v) in rank_b.iter().enumerate() {
        pos_b[v] = i;
    }

    // Collect all edges between rank_a and rank_b as (pos_a, pos_b) pairs.
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (i, &u) in rank_a.iter().enumerate() {
        for &v in &graph.adj[u] {
            if rank_b.contains(&v) {
                edges.push((i, pos_b[v]));
            }
        }
    }

    // Count inversions (crossings) by brute force for small sizes,
    // which is sufficient for terminal diagrams.
    let mut crossings = 0;
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

/// Total crossings across all adjacent rank pairs.
fn total_crossings(rank_order: &[Vec<usize>], graph: &LayoutGraph) -> usize {
    let mut total = 0;
    for r in 0..rank_order.len().saturating_sub(1) {
        total += count_crossings(&rank_order[r], &rank_order[r + 1], graph);
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

    let mut best_crossings = total_crossings(rank_order, graph);
    let mut best_order = rank_order.clone();
    let mut iterations_used = 0;

    for _iter in 0..max_iterations {
        iterations_used += 1;

        // Forward sweep.
        for r in 1..rank_order.len() {
            barycenter_sweep_forward(rank_order, graph, r, cluster_map);
        }

        // Backward sweep.
        for r in (0..rank_order.len().saturating_sub(1)).rev() {
            barycenter_sweep_backward(rank_order, graph, r, cluster_map);
        }

        let crossings = total_crossings(rank_order, graph);
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
            }
        })
        .collect()
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

    for mid_rank in (lo_rank + 1)..hi_rank {
        let t = (mid_rank - lo_rank) as f64 / total_steps;

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
    emit_layout_metrics_jsonl(config, &layout, &obj);

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

/// Compute layout with custom spacing parameters.
#[must_use]
pub fn layout_diagram_with_spacing(
    ir: &MermaidDiagramIr,
    config: &MermaidConfig,
    spacing: &LayoutSpacing,
) -> DiagramLayout {
    if ir.diagram_type == DiagramType::Sequence {
        return layout_sequence_diagram(ir, config, spacing);
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
    let ranks = assign_ranks(&graph);

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
    let edges = route_edges(ir, &node_rects, &ranks, &rank_order, ir.direction, spacing);

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
    emit_layout_metrics_jsonl(config, &layout, &obj);

    layout
}

fn compute_bounding_box(
    nodes: &[LayoutNodeBox],
    clusters: &[LayoutClusterBox],
    edges: &[LayoutEdgePath],
) -> LayoutRect {
    let mut rects: Vec<&LayoutRect> = nodes.iter().map(|n| &n.rect).collect();
    rects.extend(clusters.iter().map(|c| &c.rect));

    if rects.is_empty() {
        return LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
    }

    let mut bounds = *rects[0];
    for &r in &rects[1..] {
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
) {
    let Some(path) = config.log_path.as_deref() else {
        return;
    };
    let score_normal = obj.compute_score_with(&AestheticWeights::normal());
    let score_compact = obj.compute_score_with(&AestheticWeights::compact());
    let score_rich = obj.compute_score_with(&AestheticWeights::rich());
    let json = serde_json::json!({
        "event": "layout_metrics",
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

    for r in 0..=max_rank {
        let mut xs: Vec<f64> = nodes
            .iter()
            .filter(|n| n.rank == r)
            .map(|n| n.rect.center().x)
            .collect();
        if xs.is_empty() {
            continue;
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = xs[xs.len() / 2];
        for &x in &xs {
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
    for _pass in 0..max_passes {
        let mut moved = false;

        for rank_nodes in rank_order {
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
                        neighbor_sum += match direction {
                            GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => c.x,
                            GraphDirection::LR | GraphDirection::RL => c.y,
                        };
                        neighbor_count += 1;
                    }
                }
                for &succ in &graph.adj[node] {
                    if succ < node_rects.len() {
                        let c = node_rects[succ].center();
                        neighbor_sum += match direction {
                            GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => c.x,
                            GraphDirection::LR | GraphDirection::RL => c.y,
                        };
                        neighbor_count += 1;
                    }
                }

                if neighbor_count == 0 {
                    continue;
                }

                let ideal = neighbor_sum / neighbor_count as f64;
                let current = match direction {
                    GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => {
                        node_rects[node].center().x
                    }
                    GraphDirection::LR | GraphDirection::RL => node_rects[node].center().y,
                };

                let delta = ideal - current;
                if delta.abs() < 0.5 {
                    continue;
                }

                // Check if moving wouldn't overlap with rank neighbors.
                let min_gap = spacing.node_gap;

                let can_move = rank_nodes.iter().all(|&other| {
                    if other == node || other >= node_rects.len() {
                        return true;
                    }
                    match direction {
                        GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => {
                            let new_x = node_rects[node].x + delta;
                            let other_x = node_rects[other].x;
                            let half_size = node_rects[node].width / 2.0
                                + node_rects[other].width / 2.0
                                + min_gap;
                            let new_center = new_x + node_rects[node].width / 2.0;
                            let other_center = other_x + node_rects[other].width / 2.0;
                            (new_center - other_center).abs() >= half_size - 0.01
                        }
                        GraphDirection::LR | GraphDirection::RL => {
                            let new_y = node_rects[node].y + delta;
                            let other_y = node_rects[other].y;
                            let half_size = node_rects[node].height / 2.0
                                + node_rects[other].height / 2.0
                                + min_gap;
                            let new_center = new_y + node_rects[node].height / 2.0;
                            let other_center = other_y + node_rects[other].height / 2.0;
                            (new_center - other_center).abs() >= half_size - 0.01
                        }
                    }
                });

                if can_move {
                    match direction {
                        GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => {
                            node_rects[node].x += delta;
                        }
                        GraphDirection::LR | GraphDirection::RL => {
                            node_rects[node].y += delta;
                        }
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
        // Mark cluster boundary cells as occupied.
        for cluster in clusters {
            mark_rect_occupied(&mut occupied, cols, rows, cell_size, &origin, &cluster.rect);
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
        let col = ((p.x - self.origin.x) / self.cell_size).round().max(0.0) as usize;
        let row = ((p.y - self.origin.y) / self.cell_size).round().max(0.0) as usize;
        (
            col.min(self.cols.saturating_sub(1)),
            row.min(self.rows.saturating_sub(1)),
        )
    }

    /// Convert grid coordinates back to world space (center of cell).
    fn to_world(&self, col: usize, row: usize) -> LayoutPoint {
        LayoutPoint {
            x: self.origin.x + col as f64 * self.cell_size,
            y: self.origin.y + row as f64 * self.cell_size,
        }
    }

    /// Check if a cell is free for routing.
    fn is_free(&self, col: usize, row: usize) -> bool {
        if col >= self.cols || row >= self.rows {
            return false;
        }
        !self.occupied[row * self.cols + col]
    }

    /// Find a path between two world-space points using BFS.
    ///
    /// Returns waypoints in world space, or a direct line if no path found.
    #[must_use]
    pub fn find_path(&self, from: LayoutPoint, to: LayoutPoint) -> Vec<LayoutPoint> {
        let (sc, sr) = self.to_grid(from);
        let (ec, er) = self.to_grid(to);

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
        self.f_cost == other.f_cost
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

    let grid_size = grid.cols * grid.rows;
    let mut occupied_routes = vec![false; grid_size];
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

                let (waypoints, diag) =
                    grid.find_path_astar(from_pt, to_pt, weights, &occupied_routes);

                ops_used += diag.cells_explored;

                // Mark route cells as occupied for crossing penalty.
                for wp in &waypoints {
                    let (c, r) = grid.to_grid(*wp);
                    let gi = r * grid.cols + c;
                    if gi < occupied_routes.len() {
                        occupied_routes[gi] = true;
                    }
                }

                all_diags.push(diag);
                all_paths.push(LayoutEdgePath {
                    edge_idx: idx,
                    waypoints,
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
    let max_chars_per_line = (config.max_label_width / config.char_width)
        .floor()
        .max(1.0) as usize;

    // Split on explicit newlines, then wrap each line.
    let mut lines: Vec<usize> = Vec::new(); // char count per wrapped line
    for raw_line in text.lines() {
        if raw_line.is_empty() {
            lines.push(0);
        } else {
            let chars: Vec<char> = raw_line.chars().collect();
            for chunk in chars.chunks(max_chars_per_line) {
                lines.push(chunk.len());
            }
        }
    }

    // Handle text that doesn't end with a newline but has content.
    if lines.is_empty() {
        lines.push(text.len().min(max_chars_per_line));
    }

    let was_truncated = lines.len() > config.max_lines
        || text.chars().count() > max_chars_per_line * config.max_lines;

    // Truncate vertically.
    let visible_lines = lines.len().min(config.max_lines);
    let max_line_chars = lines[..visible_lines].iter().copied().max().unwrap_or(0);

    let width = (max_line_chars as f64 * config.char_width).min(config.max_label_width);
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
            for (occ_idx, occ) in occupied.iter().enumerate() {
                if rects_overlap(&candidate, occ, config.label_margin) {
                    if collider.is_none() {
                        // Classify collider: node, edge segment, or label.
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
            width: display_text.len() as f64 * config.char_width,
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

/// Truncate a legend entry to fit within max_chars, adding ellipsis if needed.
fn truncate_legend_text(text: &str, max_chars: usize) -> (String, bool) {
    if max_chars == 0 {
        return (String::new(), !text.is_empty());
    }
    let char_count = text.chars().count();
    if char_count <= max_chars {
        (text.to_string(), false)
    } else if max_chars <= 3 {
        (text.chars().take(max_chars).collect(), true)
    } else {
        let mut truncated: String = text.chars().take(max_chars - 3).collect();
        truncated.push_str("...");
        (truncated, true)
    }
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

    fn make_simple_ir(
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
        }
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
        let m = 10.0;
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
    fn prop_no_pairwise_overlaps_random() {
        for seed in 10_000..10_020 {
            let ir = make_random_ir(seed, 8, 25, GraphDirection::TB);
            let layout = layout_diagram(&ir, &default_config());
            assert_no_same_rank_overlaps(&layout);
        }
    }

    #[test]
    fn prop_no_pairwise_overlaps_all_dirs() {
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
    fn guard_degrade_pairwise_no_overlap() {
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
        assert!(legend.entries[0].text.ends_with("..."));
        assert!(legend.entries[0].text.len() <= 30);
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
        assert_eq!(text, "hello...");
        assert!(truncated);
        assert_eq!(text.len(), 8);
    }

    #[test]
    fn truncate_legend_text_very_short_max() {
        let (text, truncated) = truncate_legend_text("hello", 2);
        assert_eq!(text, "he");
        assert!(truncated);
    }

    #[test]
    fn truncate_legend_text_unicode_safe() {
        let (text, truncated) = truncate_legend_text("猫の手も借りたい", 4);
        assert_eq!(text.chars().count(), 4);
        assert!(truncated);
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

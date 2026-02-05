//! Sugiyama-based graph layout engine for Mermaid diagrams.
//!
//! Produces positioned nodes, edges, clusters, and ports in world-unit f64
//! coordinates. The engine is fully deterministic: same input always produces
//! identical output with no RNG or floating-point non-determinism.
//!
//! # Pipeline
//! 1. Cycle removal (greedy source/sink peeling)
//! 2. Layer assignment (longest-path via topological sort)
//! 3. Crossing minimization (barycenter heuristic)
//! 4. Coordinate assignment (median refinement)
//! 5. Post-processing: cluster boxes, port resolution, edge routing, quality scoring

use crate::mermaid::{
    GraphDirection, IrClusterId, IrEndpoint, IrNodeId, IrPortId, IrPortSideHint, MermaidDiagramIr,
};

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Which side of a node a port attaches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortSide {
    Top,
    Bottom,
    Left,
    Right,
}

/// A positioned node in world coordinates.
#[derive(Debug, Clone)]
pub struct NodeBox {
    pub ir_node: IrNodeId,
    pub cx: f64,
    pub cy: f64,
    pub width: f64,
    pub height: f64,
}

impl NodeBox {
    #[must_use]
    pub fn left(&self) -> f64 {
        self.cx - self.width / 2.0
    }
    #[must_use]
    pub fn right(&self) -> f64 {
        self.cx + self.width / 2.0
    }
    #[must_use]
    pub fn top(&self) -> f64 {
        self.cy - self.height / 2.0
    }
    #[must_use]
    pub fn bottom(&self) -> f64 {
        self.cy + self.height / 2.0
    }

    #[cfg(test)]
    fn overlaps(&self, other: &NodeBox) -> bool {
        self.left() < other.right()
            && self.right() > other.left()
            && self.top() < other.bottom()
            && self.bottom() > other.top()
    }
}

/// A positioned subgraph bounding box.
#[derive(Debug, Clone)]
pub struct ClusterBox {
    pub ir_cluster: IrClusterId,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub padding: f64,
}

/// A positioned port attachment point.
#[derive(Debug, Clone)]
pub struct PortPoint {
    pub ir_port: IrPortId,
    pub ir_node: IrNodeId,
    pub x: f64,
    pub y: f64,
    pub side: PortSide,
}

/// A routed edge as a polyline.
#[derive(Debug, Clone)]
pub struct RoutedEdge {
    pub ir_edge_idx: usize,
    pub from: IrNodeId,
    pub to: IrNodeId,
    pub waypoints: Vec<(f64, f64)>,
    pub reversed: bool,
}

/// Routing channel information for future orthogonal routing.
#[derive(Debug, Clone, Default)]
pub struct RouteGrid {
    pub vertical_channels: Vec<f64>,
    pub horizontal_channels: Vec<f64>,
}

/// Quality metrics for a layout.
#[derive(Debug, Clone)]
pub struct LayoutQuality {
    pub crossings: usize,
    pub bends: usize,
    pub variance: f64,
    pub asymmetry: f64,
    pub total_score: f64,
}

/// Complete layout result.
#[derive(Debug, Clone)]
pub struct DiagramLayout {
    pub nodes: Vec<NodeBox>,
    pub clusters: Vec<ClusterBox>,
    pub ports: Vec<PortPoint>,
    pub edges: Vec<RoutedEdge>,
    pub route_grid: RouteGrid,
    pub bounds: (f64, f64, f64, f64),
    pub quality: LayoutQuality,
    pub degraded: bool,
}

/// Configuration knobs for the layout engine.
#[derive(Debug, Clone)]
pub struct LayoutConfig {
    pub node_width: f64,
    pub node_height: f64,
    pub node_spacing: f64,
    pub layer_spacing: f64,
    pub cluster_padding: f64,
    pub max_crossing_iterations: usize,
    pub iteration_budget: usize,
    pub collapse_clusters: bool,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            node_width: 80.0,
            node_height: 40.0,
            node_spacing: 30.0,
            layer_spacing: 60.0,
            cluster_padding: 10.0,
            max_crossing_iterations: 24,
            iteration_budget: 10_000,
            collapse_clusters: false,
        }
    }
}

impl LayoutConfig {
    /// Build config from IR guard report, respecting degradation hints.
    #[must_use]
    pub fn from_ir(ir: &MermaidDiagramIr) -> Self {
        let mut config = Self::default();
        let guard = &ir.meta.guard;
        if guard.layout_budget_exceeded {
            config.max_crossing_iterations = 4;
            config.iteration_budget = 2_000;
        }
        if guard.degradation.collapse_clusters {
            config.collapse_clusters = true;
        }
        config
    }
}

// ---------------------------------------------------------------------------
// Internal graph representation
// ---------------------------------------------------------------------------

struct LayoutGraph {
    num_nodes: usize,
    adj: Vec<Vec<usize>>,
    radj: Vec<Vec<usize>>,
    node_widths: Vec<f64>,
    node_heights: Vec<f64>,
    cluster_members: Vec<(IrClusterId, Vec<usize>)>,
}

impl LayoutGraph {
    fn from_ir(ir: &MermaidDiagramIr, config: &LayoutConfig) -> Self {
        let num_nodes = ir.nodes.len();
        let mut adj = vec![Vec::new(); num_nodes];
        let mut radj = vec![Vec::new(); num_nodes];

        for edge in &ir.edges {
            let from = resolve_node(ir, edge.from);
            let to = resolve_node(ir, edge.to);
            adj[from].push(to);
            radj[to].push(from);
        }

        let node_widths = vec![config.node_width; num_nodes];
        let node_heights = vec![config.node_height; num_nodes];

        let cluster_members: Vec<(IrClusterId, Vec<usize>)> = ir
            .clusters
            .iter()
            .map(|c| (c.id, c.members.iter().map(|id| id.0).collect()))
            .collect();

        Self {
            num_nodes,
            adj,
            radj,
            node_widths,
            node_heights,
            cluster_members,
        }
    }
}

fn resolve_node(ir: &MermaidDiagramIr, endpoint: IrEndpoint) -> usize {
    match endpoint {
        IrEndpoint::Node(id) => id.0,
        IrEndpoint::Port(pid) => ir.ports[pid.0].node.0,
    }
}

// ---------------------------------------------------------------------------
// Cycle removal tracking
// ---------------------------------------------------------------------------

struct CycleRemoval {
    reversed: Vec<(usize, usize)>,
}

// ---------------------------------------------------------------------------
// Phase 1: Cycle removal — greedy source/sink peeling
// ---------------------------------------------------------------------------

fn remove_cycles(graph: &mut LayoutGraph, budget: &mut usize) -> CycleRemoval {
    let n = graph.num_nodes;
    let mut in_deg = vec![0usize; n];
    let mut out_deg = vec![0usize; n];
    let mut removed = vec![false; n];

    for (u, adj) in graph.adj.iter().enumerate() {
        for &v in adj {
            if u != v {
                out_deg[u] += 1;
                in_deg[v] += 1;
            }
        }
    }

    let mut left_order: Vec<usize> = Vec::new();
    let mut right_order: Vec<usize> = Vec::new();
    let mut remaining = n;

    while remaining > 0 && *budget > 0 {
        *budget = budget.saturating_sub(1);
        let mut progress = false;

        // Remove sinks (out_deg == 0)
        for v in 0..n {
            if !removed[v] && out_deg[v] == 0 {
                removed[v] = true;
                remaining -= 1;
                right_order.push(v);
                for &u in &graph.radj[v] {
                    if !removed[u] && u != v {
                        out_deg[u] = out_deg[u].saturating_sub(1);
                    }
                }
                progress = true;
            }
        }

        // Remove sources (in_deg == 0)
        for v in 0..n {
            if !removed[v] && in_deg[v] == 0 {
                removed[v] = true;
                remaining -= 1;
                left_order.push(v);
                for &w in &graph.adj[v] {
                    if !removed[w] && w != v {
                        in_deg[w] = in_deg[w].saturating_sub(1);
                    }
                }
                progress = true;
            }
        }

        if !progress && remaining > 0 {
            // Pick node with max (out_deg - in_deg), tiebreak by index
            let best = (0..n).filter(|&v| !removed[v]).max_by(|&a, &b| {
                let da = out_deg[a] as isize - in_deg[a] as isize;
                let db = out_deg[b] as isize - in_deg[b] as isize;
                da.cmp(&db).then_with(|| b.cmp(&a))
            });
            if let Some(v) = best {
                removed[v] = true;
                remaining -= 1;
                left_order.push(v);
                for &w in &graph.adj[v] {
                    if !removed[w] && w != v {
                        in_deg[w] = in_deg[w].saturating_sub(1);
                    }
                }
                for &u in &graph.radj[v] {
                    if !removed[u] && u != v {
                        out_deg[u] = out_deg[u].saturating_sub(1);
                    }
                }
            }
        }
    }

    // Build final ordering (must include all nodes for a total order).
    right_order.reverse();
    left_order.extend(right_order);
    if left_order.len() < n {
        for (v, is_removed) in removed.iter().enumerate().take(n) {
            if !is_removed {
                left_order.push(v);
            }
        }
    }
    let order = left_order;

    let mut pos = vec![0usize; n];
    for (i, &v) in order.iter().enumerate() {
        pos[v] = i;
    }

    // Collect original edges, classify as kept or reversed, rebuild adj/radj
    let mut reversed = Vec::new();
    let mut new_adj = vec![Vec::new(); n];
    let mut new_radj = vec![Vec::new(); n];

    for u in 0..n {
        for &v in &graph.adj[u] {
            if u == v {
                continue; // remove self-loops
            }
            if pos[u] > pos[v] {
                // Reverse this edge: was u->v, now v->u
                reversed.push((u, v));
                new_adj[v].push(u);
                new_radj[u].push(v);
            } else {
                new_adj[u].push(v);
                new_radj[v].push(u);
            }
        }
    }

    graph.adj = new_adj;
    graph.radj = new_radj;

    CycleRemoval { reversed }
}

// ---------------------------------------------------------------------------
// Phase 2: Layer assignment — longest-path via topological sort
// ---------------------------------------------------------------------------

fn assign_layers(graph: &LayoutGraph, budget: &mut usize) -> Vec<usize> {
    let n = graph.num_nodes;
    if n == 0 {
        return Vec::new();
    }

    let mut in_deg = vec![0usize; n];
    for u in 0..n {
        for &v in &graph.adj[u] {
            in_deg[v] += 1;
        }
    }

    // Topological sort (Kahn's algorithm)
    let mut queue: Vec<usize> = (0..n).filter(|&v| in_deg[v] == 0).collect();
    queue.sort_unstable(); // deterministic ordering
    let mut topo = Vec::with_capacity(n);
    let mut visited = vec![false; n];

    while let Some(u) = queue.first().copied() {
        queue.remove(0);
        *budget = budget.saturating_sub(1);
        if *budget == 0 {
            break;
        }
        topo.push(u);
        visited[u] = true;
        for &v in &graph.adj[u] {
            in_deg[v] -= 1;
            if in_deg[v] == 0 {
                // Insert in sorted position for determinism
                let pos = queue.partition_point(|&x| x < v);
                queue.insert(pos, v);
            }
        }
    }

    // Add any remaining nodes not yet visited (disconnected or budget-cut)
    for (v, &vis) in visited.iter().enumerate() {
        if !vis {
            topo.push(v);
        }
    }

    // Longest-path layering with convergence loop.
    // A single forward pass suffices for a correct topo order, but if
    // budget exhaustion produced an incomplete sort, iterate until stable.
    let mut layer = vec![0usize; n];
    let mut changed = true;
    while changed {
        changed = false;
        for &u in &topo {
            for &v in &graph.adj[u] {
                if layer[v] <= layer[u] {
                    layer[v] = layer[u] + 1;
                    changed = true;
                }
            }
        }
    }

    layer
}

// ---------------------------------------------------------------------------
// Phase 3: Crossing minimization — barycenter heuristic
// ---------------------------------------------------------------------------

fn count_crossings(layers: &[Vec<usize>], adj: &[Vec<usize>]) -> usize {
    let mut crossings = 0;
    for i in 0..layers.len().saturating_sub(1) {
        let layer_a = &layers[i];
        let layer_b = &layers[i + 1];

        // Position map for layer_b (usize::MAX => not present)
        let max_node = layer_b.iter().copied().max().unwrap_or(0) + 1;
        let mut pos_b = vec![usize::MAX; max_node];
        for (p, &v) in layer_b.iter().enumerate() {
            if v < max_node {
                pos_b[v] = p;
            }
        }

        // Collect all edges between layer_a and layer_b as (pos_in_a, pos_in_b)
        let mut edge_pairs: Vec<(usize, usize)> = Vec::new();
        for (pa, &u) in layer_a.iter().enumerate() {
            for &v in &adj[u] {
                if v < max_node {
                    let pos = pos_b[v];
                    if pos != usize::MAX {
                        edge_pairs.push((pa, pos));
                    }
                }
            }
        }

        // Count inversions
        for i_idx in 0..edge_pairs.len() {
            for j_idx in (i_idx + 1)..edge_pairs.len() {
                let (a1, b1) = edge_pairs[i_idx];
                let (a2, b2) = edge_pairs[j_idx];
                if (a1 < a2 && b1 > b2) || (a1 > a2 && b1 < b2) {
                    crossings += 1;
                }
            }
        }
    }
    crossings
}

fn minimize_crossings(
    layer_assignment: &[usize],
    adj: &[Vec<usize>],
    radj: &[Vec<usize>],
    num_nodes: usize,
    max_iterations: usize,
    budget: &mut usize,
) -> Vec<Vec<usize>> {
    if num_nodes == 0 {
        return Vec::new();
    }

    let num_layers = layer_assignment.iter().copied().max().unwrap_or(0) + 1;
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); num_layers];
    for v in 0..num_nodes {
        layers[layer_assignment[v]].push(v);
    }

    // Initial ordering: sort by node index within each layer
    for layer in &mut layers {
        layer.sort_unstable();
    }

    let mut best_crossings = count_crossings(&layers, adj);
    let mut best_layers = layers.clone();

    for iter in 0..max_iterations {
        if *budget == 0 {
            break;
        }
        *budget = budget.saturating_sub(1);

        if iter % 2 == 0 {
            // Forward sweep
            for i in 1..num_layers {
                barycenter_sort(&mut layers, i, adj, radj, true, budget);
            }
        } else {
            // Backward sweep
            for i in (0..num_layers.saturating_sub(1)).rev() {
                barycenter_sort(&mut layers, i, adj, radj, false, budget);
            }
        }

        let c = count_crossings(&layers, adj);
        if c < best_crossings {
            best_crossings = c;
            best_layers = layers.clone();
        }

        if best_crossings == 0 {
            break;
        }
    }

    best_layers
}

fn barycenter_sort(
    layers: &mut [Vec<usize>],
    layer_idx: usize,
    adj: &[Vec<usize>],
    radj: &[Vec<usize>],
    forward: bool,
    budget: &mut usize,
) {
    if layers.is_empty() || layer_idx >= layers.len() {
        return;
    }

    let ref_layer_idx = if forward {
        layer_idx.checked_sub(1)
    } else {
        if layer_idx + 1 < layers.len() {
            Some(layer_idx + 1)
        } else {
            None
        }
    };

    let ref_layer_idx = match ref_layer_idx {
        Some(i) => i,
        None => return,
    };

    // Build position map for reference layer
    let ref_layer = &layers[ref_layer_idx];
    let max_ref = ref_layer.iter().copied().max().unwrap_or(0) + 1;
    let mut ref_pos = vec![0usize; max_ref];
    for (p, &v) in ref_layer.iter().enumerate() {
        if v < max_ref {
            ref_pos[v] = p;
        }
    }

    // Compute barycenters for nodes in current layer
    let layer = &layers[layer_idx];
    let mut bary: Vec<(usize, f64)> = Vec::with_capacity(layer.len());

    for &v in layer {
        *budget = budget.saturating_sub(1);
        let neighbors: &Vec<usize> = if forward { &radj[v] } else { &adj[v] };
        let relevant: Vec<usize> = neighbors
            .iter()
            .copied()
            .filter(|&u| u < max_ref && ref_layer.contains(&u))
            .collect();

        if relevant.is_empty() {
            bary.push((v, f64::MAX));
        } else {
            let sum: f64 = relevant.iter().map(|&u| ref_pos[u] as f64).sum();
            let avg = sum / relevant.len() as f64;
            bary.push((v, avg));
        }
    }

    // Sort by barycenter, tiebreak by node index
    bary.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

    layers[layer_idx] = bary.into_iter().map(|(v, _)| v).collect();
}

// ---------------------------------------------------------------------------
// Phase 4: Coordinate assignment — centering + median refinement
// ---------------------------------------------------------------------------

fn assign_coordinates(
    layers: &[Vec<usize>],
    graph: &LayoutGraph,
    config: &LayoutConfig,
    budget: &mut usize,
) -> (Vec<f64>, Vec<f64>) {
    let n = graph.num_nodes;
    let mut x = vec![0.0f64; n];
    let mut y = vec![0.0f64; n];

    // Place nodes in each layer
    for (layer_idx, layer) in layers.iter().enumerate() {
        let layer_y = layer_idx as f64 * (config.node_height + config.layer_spacing);
        let total_width: f64 = layer.iter().map(|&v| graph.node_widths[v]).sum::<f64>()
            + (layer.len().saturating_sub(1)) as f64 * config.node_spacing;
        let mut cx = -total_width / 2.0;

        for &v in layer {
            let w = graph.node_widths[v];
            x[v] = cx + w / 2.0;
            y[v] = layer_y;
            cx += w + config.node_spacing;
        }
    }

    // Median refinement passes
    let passes = 4.min(config.max_crossing_iterations);
    for _ in 0..passes {
        if *budget == 0 {
            break;
        }
        *budget = budget.saturating_sub(1);

        for layer in layers {
            for &v in layer {
                let neighbors: Vec<f64> = graph.adj[v]
                    .iter()
                    .chain(graph.radj[v].iter())
                    .map(|&u| x[u])
                    .collect();
                if !neighbors.is_empty() {
                    let mut sorted = neighbors;
                    sorted.sort_by(|a, b| a.total_cmp(b));
                    let median = sorted[sorted.len() / 2];
                    x[v] = (x[v] + median) / 2.0;
                }
            }
        }

        // Resolve overlaps within layers
        for layer in layers {
            let mut sorted_layer: Vec<usize> = layer.clone();
            sorted_layer.sort_by(|&a, &b| x[a].total_cmp(&x[b]).then_with(|| a.cmp(&b)));

            for i in 1..sorted_layer.len() {
                let prev = sorted_layer[i - 1];
                let curr = sorted_layer[i];
                let min_gap =
                    (graph.node_widths[prev] + graph.node_widths[curr]) / 2.0 + config.node_spacing;
                if x[curr] - x[prev] < min_gap {
                    x[curr] = x[prev] + min_gap;
                }
            }
        }
    }

    (x, y)
}

fn remap_for_direction(x: &mut [f64], y: &mut [f64], direction: GraphDirection) {
    match direction {
        GraphDirection::TB | GraphDirection::TD => {}
        GraphDirection::BT => {
            let max_y = y.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            for yi in y.iter_mut() {
                *yi = max_y - *yi;
            }
        }
        GraphDirection::LR => {
            for i in 0..x.len() {
                std::mem::swap(&mut x[i], &mut y[i]);
            }
        }
        GraphDirection::RL => {
            let max_y = y.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            for i in 0..x.len() {
                std::mem::swap(&mut x[i], &mut y[i]);
                x[i] = max_y - x[i];
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Post-processing: cluster boxes
// ---------------------------------------------------------------------------

fn compute_cluster_boxes(
    graph: &LayoutGraph,
    x: &[f64],
    y: &[f64],
    display_widths: &[f64],
    display_heights: &[f64],
    config: &LayoutConfig,
) -> Vec<ClusterBox> {
    if config.collapse_clusters {
        return Vec::new();
    }

    graph
        .cluster_members
        .iter()
        .filter_map(|(cluster_id, members)| {
            if members.is_empty() {
                return None;
            }

            let mut min_x = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_y = f64::NEG_INFINITY;

            for &v in members {
                let hw = display_widths[v] / 2.0;
                let hh = display_heights[v] / 2.0;
                min_x = min_x.min(x[v] - hw);
                max_x = max_x.max(x[v] + hw);
                min_y = min_y.min(y[v] - hh);
                max_y = max_y.max(y[v] + hh);
            }

            let pad = config.cluster_padding;
            Some(ClusterBox {
                ir_cluster: *cluster_id,
                x: min_x - pad,
                y: min_y - pad,
                width: (max_x - min_x) + 2.0 * pad,
                height: (max_y - min_y) + 2.0 * pad,
                padding: pad,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Post-processing: port resolution
// ---------------------------------------------------------------------------

fn compute_ports(
    ir: &MermaidDiagramIr,
    x: &[f64],
    y: &[f64],
    node_widths: &[f64],
    node_heights: &[f64],
    direction: GraphDirection,
) -> Vec<PortPoint> {
    ir.ports
        .iter()
        .enumerate()
        .map(|(i, port)| {
            let nid = port.node.0;
            let (px, py, side) = match port.side_hint {
                IrPortSideHint::Horizontal => match direction {
                    GraphDirection::LR => {
                        (x[nid] + node_widths[nid] / 2.0, y[nid], PortSide::Right)
                    }
                    GraphDirection::RL => (x[nid] - node_widths[nid] / 2.0, y[nid], PortSide::Left),
                    _ => (x[nid] + node_widths[nid] / 2.0, y[nid], PortSide::Right),
                },
                IrPortSideHint::Vertical => match direction {
                    GraphDirection::BT => (x[nid], y[nid] - node_heights[nid] / 2.0, PortSide::Top),
                    _ => (x[nid], y[nid] + node_heights[nid] / 2.0, PortSide::Bottom),
                },
                IrPortSideHint::Auto => match direction {
                    GraphDirection::LR => {
                        (x[nid] + node_widths[nid] / 2.0, y[nid], PortSide::Right)
                    }
                    GraphDirection::RL => (x[nid] - node_widths[nid] / 2.0, y[nid], PortSide::Left),
                    GraphDirection::BT => (x[nid], y[nid] - node_heights[nid] / 2.0, PortSide::Top),
                    _ => (x[nid], y[nid] + node_heights[nid] / 2.0, PortSide::Bottom),
                },
            };

            PortPoint {
                ir_port: IrPortId(i),
                ir_node: port.node,
                x: px,
                y: py,
                side,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Post-processing: edge routing (straight-line boundary-to-boundary)
// ---------------------------------------------------------------------------

fn route_edges_simple(
    ir: &MermaidDiagramIr,
    x: &[f64],
    y: &[f64],
    node_widths: &[f64],
    node_heights: &[f64],
    reversed_edges: &[(usize, usize)],
) -> (Vec<RoutedEdge>, RouteGrid) {
    let reversed_set: std::collections::HashSet<(usize, usize)> =
        reversed_edges.iter().copied().collect();

    let mut edges = Vec::new();
    let mut v_channels: Vec<f64> = Vec::new();
    let mut h_channels: Vec<f64> = Vec::new();

    for (idx, edge) in ir.edges.iter().enumerate() {
        let from_id = resolve_node(ir, edge.from);
        let to_id = resolve_node(ir, edge.to);
        let is_reversed = reversed_set.contains(&(from_id, to_id));

        // Always clip from→to in the original IR direction so waypoints
        // match the `from`/`to` fields consumers will use for rendering.
        let (sx, sy) = clip_to_boundary(
            x[from_id],
            y[from_id],
            node_widths[from_id],
            node_heights[from_id],
            x[to_id],
            y[to_id],
        );
        let (dx, dy) = clip_to_boundary(
            x[to_id],
            y[to_id],
            node_widths[to_id],
            node_heights[to_id],
            x[from_id],
            y[from_id],
        );

        v_channels.push((sx + dx) / 2.0);
        h_channels.push((sy + dy) / 2.0);

        edges.push(RoutedEdge {
            ir_edge_idx: idx,
            from: IrNodeId(from_id),
            to: IrNodeId(to_id),
            waypoints: vec![(sx, sy), (dx, dy)],
            reversed: is_reversed,
        });
    }

    v_channels.sort_by(|a, b| a.total_cmp(b));
    v_channels.dedup();
    h_channels.sort_by(|a, b| a.total_cmp(b));
    h_channels.dedup();

    let grid = RouteGrid {
        vertical_channels: v_channels,
        horizontal_channels: h_channels,
    };

    (edges, grid)
}

fn clip_to_boundary(cx: f64, cy: f64, w: f64, h: f64, target_x: f64, target_y: f64) -> (f64, f64) {
    let dx = target_x - cx;
    let dy = target_y - cy;

    if dx.abs() < 1e-12 && dy.abs() < 1e-12 {
        return (cx, cy);
    }

    let hw = w / 2.0;
    let hh = h / 2.0;

    // Scale factor to hit the rectangle boundary
    let sx = if dx.abs() > 1e-12 {
        hw / dx.abs()
    } else {
        f64::INFINITY
    };
    let sy = if dy.abs() > 1e-12 {
        hh / dy.abs()
    } else {
        f64::INFINITY
    };
    let s = sx.min(sy);

    (cx + dx * s, cy + dy * s)
}

// ---------------------------------------------------------------------------
// Quality scoring
// ---------------------------------------------------------------------------

impl LayoutQuality {
    fn compute_from(
        layers: &[Vec<usize>],
        adj: &[Vec<usize>],
        edges: &[RoutedEdge],
        x: &[f64],
    ) -> Self {
        let crossings = count_crossings(layers, adj);

        let bends: usize = edges
            .iter()
            .map(|e| e.waypoints.len().saturating_sub(2))
            .sum();

        // Variance of x-positions within layers
        let mut variance = 0.0;
        let mut layer_count = 0;
        for layer in layers {
            if layer.len() <= 1 {
                continue;
            }
            let mean: f64 = layer.iter().map(|&v| x[v]).sum::<f64>() / layer.len() as f64;
            let var: f64 = layer
                .iter()
                .map(|&v| (x[v] - mean) * (x[v] - mean))
                .sum::<f64>()
                / layer.len() as f64;
            variance += var;
            layer_count += 1;
        }
        if layer_count > 0 {
            variance /= layer_count as f64;
        }

        // Asymmetry: mean absolute deviation from center
        let mut asymmetry = 0.0;
        let mut asym_count = 0;
        for layer in layers {
            if layer.is_empty() {
                continue;
            }
            let mean: f64 = layer.iter().map(|&v| x[v]).sum::<f64>() / layer.len() as f64;
            for &v in layer {
                asymmetry += (x[v] - mean).abs();
                asym_count += 1;
            }
        }
        if asym_count > 0 {
            asymmetry /= asym_count as f64;
        }

        let total_score =
            crossings as f64 * 10.0 + bends as f64 * 2.0 + variance * 0.1 + asymmetry * 0.5;

        Self {
            crossings,
            bends,
            variance,
            asymmetry,
            total_score,
        }
    }
}

// ---------------------------------------------------------------------------
// Bounds computation
// ---------------------------------------------------------------------------

fn compute_bounds(nodes: &[NodeBox]) -> (f64, f64, f64, f64) {
    if nodes.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }

    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for n in nodes {
        min_x = min_x.min(n.left());
        max_x = max_x.max(n.right());
        min_y = min_y.min(n.top());
        max_y = max_y.max(n.bottom());
    }

    (min_x, min_y, max_x, max_y)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Layout a diagram using default configuration.
#[must_use]
pub fn layout_diagram(ir: &MermaidDiagramIr) -> DiagramLayout {
    let config = LayoutConfig::from_ir(ir);
    layout_diagram_with_config(ir, &config)
}

/// Layout a diagram with explicit configuration.
#[must_use]
pub fn layout_diagram_with_config(ir: &MermaidDiagramIr, config: &LayoutConfig) -> DiagramLayout {
    if ir.nodes.is_empty() {
        return DiagramLayout {
            nodes: Vec::new(),
            clusters: Vec::new(),
            ports: Vec::new(),
            edges: Vec::new(),
            route_grid: RouteGrid::default(),
            bounds: (0.0, 0.0, 0.0, 0.0),
            quality: LayoutQuality {
                crossings: 0,
                bends: 0,
                variance: 0.0,
                asymmetry: 0.0,
                total_score: 0.0,
            },
            degraded: false,
        };
    }

    let mut budget = config.iteration_budget;
    let mut graph = LayoutGraph::from_ir(ir, config);

    // Phase 1: Cycle removal
    let cycle_info = remove_cycles(&mut graph, &mut budget);

    // Phase 2: Layer assignment
    let layer_assignment = assign_layers(&graph, &mut budget);

    // Phase 3: Crossing minimization
    let layers = minimize_crossings(
        &layer_assignment,
        &graph.adj,
        &graph.radj,
        graph.num_nodes,
        config.max_crossing_iterations,
        &mut budget,
    );

    // Phase 4: Coordinate assignment
    let (mut x, mut y) = assign_coordinates(&layers, &graph, config, &mut budget);

    // Direction remapping
    remap_for_direction(&mut x, &mut y, ir.direction);

    // Compute display dimensions (swapped for LR/RL since axes are swapped)
    let (display_widths, display_heights) = match ir.direction {
        GraphDirection::LR | GraphDirection::RL => {
            (graph.node_heights.clone(), graph.node_widths.clone())
        }
        _ => (graph.node_widths.clone(), graph.node_heights.clone()),
    };

    // Build node boxes
    let nodes: Vec<NodeBox> = (0..graph.num_nodes)
        .map(|v| NodeBox {
            ir_node: IrNodeId(v),
            cx: x[v],
            cy: y[v],
            width: display_widths[v],
            height: display_heights[v],
        })
        .collect();

    // Post-processing (use display dimensions, not internal TB dimensions)
    let clusters = compute_cluster_boxes(&graph, &x, &y, &display_widths, &display_heights, config);
    let ports = compute_ports(ir, &x, &y, &display_widths, &display_heights, ir.direction);
    let (edges, route_grid) = route_edges_simple(
        ir,
        &x,
        &y,
        &display_widths,
        &display_heights,
        &cycle_info.reversed,
    );
    let quality = LayoutQuality::compute_from(&layers, &graph.adj, &edges, &x);
    let bounds = compute_bounds(&nodes);
    let degraded = budget == 0;

    DiagramLayout {
        nodes,
        clusters,
        ports,
        edges,
        route_grid,
        bounds,
        quality,
        degraded,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mermaid::*;
    use std::collections::BTreeMap;

    // -- Test helper --

    fn dummy_span() -> Span {
        Span {
            start: Position {
                line: 1,
                col: 1,
                byte: 0,
            },
            end: Position {
                line: 1,
                col: 1,
                byte: 0,
            },
        }
    }

    fn make_test_ir(
        node_labels: &[&str],
        edges: &[(usize, usize)],
        direction: GraphDirection,
    ) -> MermaidDiagramIr {
        let nodes: Vec<IrNode> = node_labels
            .iter()
            .map(|label| IrNode {
                id: label.to_string(),
                label: None,
                shape: NodeShape::Rect,
                classes: Vec::new(),
                style_ref: None,
                span_primary: dummy_span(),
                span_all: Vec::new(),
                implicit: false,
                members: Vec::new(),
            })
            .collect();

        let ir_edges: Vec<IrEdge> = edges
            .iter()
            .map(|&(from, to)| IrEdge {
                from: IrEndpoint::Node(IrNodeId(from)),
                to: IrEndpoint::Node(IrNodeId(to)),
                arrow: "-->".to_string(),
                label: None,
                style_ref: None,
                span: dummy_span(),
            })
            .collect();

        MermaidDiagramIr {
            diagram_type: DiagramType::Graph,
            direction,
            nodes,
            edges: ir_edges,
            ports: Vec::new(),
            clusters: Vec::new(),
            labels: Vec::new(),
            style_refs: Vec::new(),
            links: Vec::new(),
            meta: MermaidDiagramMeta {
                diagram_type: DiagramType::Graph,
                direction,
                support_level: MermaidSupportLevel::Supported,
                init: MermaidInitParse {
                    config: MermaidInitConfig {
                        theme: None,
                        theme_variables: BTreeMap::new(),
                        flowchart_direction: None,
                    },
                    warnings: Vec::new(),
                    errors: Vec::new(),
                },
                theme_overrides: MermaidThemeOverrides {
                    theme: None,
                    theme_variables: BTreeMap::new(),
                },
                guard: MermaidGuardReport::default(),
            },
        }
    }

    fn make_test_ir_with_clusters(
        node_labels: &[&str],
        edges: &[(usize, usize)],
        clusters: &[(usize, &[usize])],
        direction: GraphDirection,
    ) -> MermaidDiagramIr {
        let mut ir = make_test_ir(node_labels, edges, direction);
        ir.clusters = clusters
            .iter()
            .map(|&(id, members)| IrCluster {
                id: IrClusterId(id),
                title: None,
                members: members.iter().map(|&m| IrNodeId(m)).collect(),
                span: dummy_span(),
            })
            .collect();
        ir
    }

    // -- Primitive tests --

    #[test]
    fn node_box_accessors() {
        let nb = NodeBox {
            ir_node: IrNodeId(0),
            cx: 100.0,
            cy: 50.0,
            width: 80.0,
            height: 40.0,
        };
        assert!((nb.left() - 60.0).abs() < 1e-9);
        assert!((nb.right() - 140.0).abs() < 1e-9);
        assert!((nb.top() - 30.0).abs() < 1e-9);
        assert!((nb.bottom() - 70.0).abs() < 1e-9);
    }

    #[test]
    fn layout_padding_default() {
        let config = LayoutConfig::default();
        assert!((config.cluster_padding - 10.0).abs() < 1e-9);
        assert!((config.node_spacing - 30.0).abs() < 1e-9);
    }

    #[test]
    fn config_from_ir_respects_degradation() {
        let mut ir = make_test_ir(&["A"], &[], GraphDirection::TB);
        ir.meta.guard.layout_budget_exceeded = true;
        ir.meta.guard.degradation.collapse_clusters = true;
        let config = LayoutConfig::from_ir(&ir);
        assert_eq!(config.max_crossing_iterations, 4);
        assert!(config.collapse_clusters);
    }

    // -- Cycle removal tests --

    #[test]
    fn cycle_removal_acyclic_unchanged() {
        let ir = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::TB);
        let config = LayoutConfig::default();
        let mut graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 10_000;
        let info = remove_cycles(&mut graph, &mut budget);
        assert!(info.reversed.is_empty());
    }

    #[test]
    fn cycle_removal_simple_cycle() {
        let ir = make_test_ir(
            &["A", "B", "C"],
            &[(0, 1), (1, 2), (2, 0)],
            GraphDirection::TB,
        );
        let config = LayoutConfig::default();
        let mut graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 10_000;
        let info = remove_cycles(&mut graph, &mut budget);
        assert!(!info.reversed.is_empty());
    }

    #[test]
    fn cycle_removal_self_loop() {
        let ir = make_test_ir(&["A", "B"], &[(0, 0), (0, 1)], GraphDirection::TB);
        let config = LayoutConfig::default();
        let mut graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 10_000;
        let _info = remove_cycles(&mut graph, &mut budget);
        // Self-loops removed; layout still works
        let layers = assign_layers(&graph, &mut budget);
        assert_eq!(layers.len(), 2);
    }

    #[test]
    fn cycle_removal_multi_cycle() {
        let ir = make_test_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (1, 0), (2, 3), (3, 2)],
            GraphDirection::TB,
        );
        let config = LayoutConfig::default();
        let mut graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 10_000;
        let info = remove_cycles(&mut graph, &mut budget);
        assert!(!info.reversed.is_empty());
    }

    #[test]
    fn cycle_removal_deterministic() {
        let ir = make_test_ir(
            &["A", "B", "C"],
            &[(0, 1), (1, 2), (2, 0)],
            GraphDirection::TB,
        );
        let config = LayoutConfig::default();

        let mut g1 = LayoutGraph::from_ir(&ir, &config);
        let mut b1 = 10_000;
        let r1 = remove_cycles(&mut g1, &mut b1);

        let mut g2 = LayoutGraph::from_ir(&ir, &config);
        let mut b2 = 10_000;
        let r2 = remove_cycles(&mut g2, &mut b2);

        assert_eq!(r1.reversed.len(), r2.reversed.len());
    }

    #[test]
    fn cycle_removal_budget_exhaustion_still_orders_all_nodes() {
        let ir = make_test_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (1, 2), (2, 0), (2, 3)],
            GraphDirection::TB,
        );
        let config = LayoutConfig::default();
        let mut graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 0;
        let _info = remove_cycles(&mut graph, &mut budget);

        let mut layer_budget = 10_000;
        let layers = assign_layers(&graph, &mut layer_budget);
        assert_eq!(layers.len(), ir.nodes.len());
        let max_layer = layers.iter().copied().max().unwrap_or(0);
        assert!(max_layer <= ir.nodes.len().saturating_sub(1));
    }

    // -- Layering tests --

    #[test]
    fn layering_linear_chain() {
        let ir = make_test_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (1, 2), (2, 3)],
            GraphDirection::TB,
        );
        let config = LayoutConfig::default();
        let graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 10_000;
        let layers = assign_layers(&graph, &mut budget);
        assert_eq!(layers, vec![0, 1, 2, 3]);
    }

    #[test]
    fn layering_diamond() {
        // A -> B, A -> C, B -> D, C -> D
        let ir = make_test_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (0, 2), (1, 3), (2, 3)],
            GraphDirection::TB,
        );
        let config = LayoutConfig::default();
        let graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 10_000;
        let layers = assign_layers(&graph, &mut budget);
        assert_eq!(layers[0], 0); // A at top
        assert_eq!(layers[1], 1); // B
        assert_eq!(layers[2], 1); // C same layer as B
        assert_eq!(layers[3], 2); // D at bottom
    }

    #[test]
    fn layering_single_node() {
        let ir = make_test_ir(&["A"], &[], GraphDirection::TB);
        let config = LayoutConfig::default();
        let graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 10_000;
        let layers = assign_layers(&graph, &mut budget);
        assert_eq!(layers, vec![0]);
    }

    #[test]
    fn layering_disconnected() {
        let ir = make_test_ir(&["A", "B", "C"], &[], GraphDirection::TB);
        let config = LayoutConfig::default();
        let graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 10_000;
        let layers = assign_layers(&graph, &mut budget);
        // All disconnected nodes on layer 0
        assert_eq!(layers, vec![0, 0, 0]);
    }

    // -- Crossing minimization tests --

    #[test]
    fn crossings_no_crossing_stable() {
        // A -> C, B -> D — no crossing possible
        let ir = make_test_ir(&["A", "B", "C", "D"], &[(0, 2), (1, 3)], GraphDirection::TB);
        let config = LayoutConfig::default();
        let graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 10_000;
        let layer_assignment = assign_layers(&graph, &mut budget);
        let layers = minimize_crossings(
            &layer_assignment,
            &graph.adj,
            &graph.radj,
            graph.num_nodes,
            config.max_crossing_iterations,
            &mut budget,
        );
        let c = count_crossings(&layers, &graph.adj);
        assert_eq!(c, 0);
    }

    #[test]
    fn crossings_swap_reduces() {
        // A -> D, B -> C — initial ordering has crossing, barycenter should fix
        let ir = make_test_ir(&["A", "B", "C", "D"], &[(0, 3), (1, 2)], GraphDirection::TB);
        let config = LayoutConfig::default();
        let graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 10_000;
        let layer_assignment = assign_layers(&graph, &mut budget);
        let layers = minimize_crossings(
            &layer_assignment,
            &graph.adj,
            &graph.radj,
            graph.num_nodes,
            config.max_crossing_iterations,
            &mut budget,
        );
        let c = count_crossings(&layers, &graph.adj);
        assert_eq!(c, 0);
    }

    #[test]
    fn crossings_bounded_iterations() {
        let ir = make_test_ir(&["A", "B", "C", "D"], &[(0, 3), (1, 2)], GraphDirection::TB);
        let config = LayoutConfig {
            max_crossing_iterations: 1,
            ..LayoutConfig::default()
        };
        let graph = LayoutGraph::from_ir(&ir, &config);
        let mut budget = 10_000;
        let layer_assignment = assign_layers(&graph, &mut budget);
        let layers = minimize_crossings(
            &layer_assignment,
            &graph.adj,
            &graph.radj,
            graph.num_nodes,
            config.max_crossing_iterations,
            &mut budget,
        );
        // Should still produce valid layers
        assert!(!layers.is_empty());
    }

    #[test]
    fn crossings_deterministic() {
        let ir = make_test_ir(
            &["A", "B", "C", "D", "E"],
            &[(0, 3), (1, 4), (2, 3), (0, 4)],
            GraphDirection::TB,
        );
        let config = LayoutConfig::default();

        let result1 = layout_diagram_with_config(&ir, &config);
        let result2 = layout_diagram_with_config(&ir, &config);

        for (n1, n2) in result1.nodes.iter().zip(result2.nodes.iter()) {
            assert!((n1.cx - n2.cx).abs() < 1e-12);
            assert!((n1.cy - n2.cy).abs() < 1e-12);
        }
    }

    // -- Coordinate tests --

    #[test]
    fn coordinates_single_layer_centered() {
        let ir = make_test_ir(&["A", "B", "C"], &[], GraphDirection::TB);
        let config = LayoutConfig::default();
        let layout = layout_diagram_with_config(&ir, &config);
        // Center of mass should be near zero
        let mean_x: f64 =
            layout.nodes.iter().map(|n| n.cx).sum::<f64>() / layout.nodes.len() as f64;
        assert!(mean_x.abs() < config.node_width);
    }

    #[test]
    fn coordinates_two_layers_no_overlap() {
        let ir = make_test_ir(&["A", "B", "C", "D"], &[(0, 2), (1, 3)], GraphDirection::TB);
        let config = LayoutConfig::default();
        let layout = layout_diagram_with_config(&ir, &config);

        // No overlaps
        for i in 0..layout.nodes.len() {
            for j in (i + 1)..layout.nodes.len() {
                assert!(
                    !layout.nodes[i].overlaps(&layout.nodes[j]),
                    "Nodes {} and {} overlap",
                    i,
                    j
                );
            }
        }
    }

    #[test]
    fn coordinates_direction_tb() {
        let ir = make_test_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let layout = layout_diagram(&ir);
        // In TB mode, A should be above B (smaller y)
        assert!(layout.nodes[0].cy < layout.nodes[1].cy);
    }

    #[test]
    fn coordinates_direction_bt() {
        let ir = make_test_ir(&["A", "B"], &[(0, 1)], GraphDirection::BT);
        let layout = layout_diagram(&ir);
        // In BT mode, A should be below B (larger y)
        assert!(layout.nodes[0].cy > layout.nodes[1].cy);
    }

    #[test]
    fn coordinates_direction_lr() {
        let ir = make_test_ir(&["A", "B"], &[(0, 1)], GraphDirection::LR);
        let layout = layout_diagram(&ir);
        // In LR mode, A should be left of B (smaller x)
        assert!(layout.nodes[0].cx < layout.nodes[1].cx);
    }

    #[test]
    fn coordinates_direction_rl() {
        let ir = make_test_ir(&["A", "B"], &[(0, 1)], GraphDirection::RL);
        let layout = layout_diagram(&ir);
        // In RL mode, A should be right of B (larger x)
        assert!(layout.nodes[0].cx > layout.nodes[1].cx);
    }

    #[test]
    fn coordinates_direction_td() {
        let ir = make_test_ir(&["A", "B"], &[(0, 1)], GraphDirection::TD);
        let layout = layout_diagram(&ir);
        // TD is alias for TB
        assert!(layout.nodes[0].cy < layout.nodes[1].cy);
    }

    // -- Cluster tests --

    #[test]
    fn cluster_encloses_members() {
        let ir = make_test_ir_with_clusters(
            &["A", "B", "C"],
            &[(0, 1), (1, 2)],
            &[(0, &[0, 1])],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir);
        assert_eq!(layout.clusters.len(), 1);
        let cluster = &layout.clusters[0];

        // Cluster should enclose nodes 0 and 1
        for &idx in &[0usize, 1] {
            let node = &layout.nodes[idx];
            assert!(
                node.left() >= cluster.x,
                "Node {} left outside cluster",
                idx
            );
            assert!(
                node.right() <= cluster.x + cluster.width,
                "Node {} right outside cluster",
                idx
            );
            assert!(node.top() >= cluster.y, "Node {} top outside cluster", idx);
            assert!(
                node.bottom() <= cluster.y + cluster.height,
                "Node {} bottom outside cluster",
                idx
            );
        }
    }

    #[test]
    fn cluster_padding_applied() {
        let ir =
            make_test_ir_with_clusters(&["A", "B"], &[(0, 1)], &[(0, &[0, 1])], GraphDirection::TB);
        let config = LayoutConfig {
            cluster_padding: 20.0,
            ..LayoutConfig::default()
        };
        let layout = layout_diagram_with_config(&ir, &config);
        assert_eq!(layout.clusters.len(), 1);
        assert!((layout.clusters[0].padding - 20.0).abs() < 1e-9);
    }

    #[test]
    fn cluster_collapsed_empty() {
        let ir =
            make_test_ir_with_clusters(&["A", "B"], &[(0, 1)], &[(0, &[0, 1])], GraphDirection::TB);
        let config = LayoutConfig {
            collapse_clusters: true,
            ..LayoutConfig::default()
        };
        let layout = layout_diagram_with_config(&ir, &config);
        assert!(layout.clusters.is_empty());
    }

    // -- No-overlap invariant --

    #[test]
    fn no_overlap_invariant() {
        let ir = make_test_ir(
            &["A", "B", "C", "D", "E"],
            &[(0, 1), (0, 2), (1, 3), (2, 4), (3, 4)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir);
        for i in 0..layout.nodes.len() {
            for j in (i + 1)..layout.nodes.len() {
                assert!(
                    !layout.nodes[i].overlaps(&layout.nodes[j]),
                    "Nodes {} and {} overlap",
                    i,
                    j
                );
            }
        }
    }

    // -- Integration tests --

    #[test]
    fn integration_empty_ir() {
        let ir = make_test_ir(&[], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir);
        assert!(layout.nodes.is_empty());
        assert!(layout.edges.is_empty());
        assert!(!layout.degraded);
    }

    #[test]
    fn integration_single_node() {
        let ir = make_test_ir(&["A"], &[], GraphDirection::TB);
        let layout = layout_diagram(&ir);
        assert_eq!(layout.nodes.len(), 1);
        assert_eq!(layout.edges.len(), 0);
        assert!(!layout.degraded);
    }

    #[test]
    fn integration_linear_chain() {
        let ir = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::TB);
        let layout = layout_diagram(&ir);
        assert_eq!(layout.nodes.len(), 3);
        assert_eq!(layout.edges.len(), 2);
        // Nodes should be ordered top-to-bottom
        assert!(layout.nodes[0].cy < layout.nodes[1].cy);
        assert!(layout.nodes[1].cy < layout.nodes[2].cy);
    }

    #[test]
    fn integration_diamond() {
        let ir = make_test_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (0, 2), (1, 3), (2, 3)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir);
        assert_eq!(layout.nodes.len(), 4);
        assert_eq!(layout.edges.len(), 4);
        // A at top, D at bottom, B and C in middle
        assert!(layout.nodes[0].cy < layout.nodes[1].cy);
        assert!((layout.nodes[1].cy - layout.nodes[2].cy).abs() < 1e-9);
        assert!(layout.nodes[1].cy < layout.nodes[3].cy);
    }

    #[test]
    fn integration_with_cluster() {
        let ir = make_test_ir_with_clusters(
            &["A", "B", "C"],
            &[(0, 1), (1, 2)],
            &[(0, &[0, 1])],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir);
        assert_eq!(layout.clusters.len(), 1);
        assert_eq!(layout.nodes.len(), 3);
    }

    #[test]
    fn integration_determinism() {
        let ir = make_test_ir(
            &["A", "B", "C", "D", "E"],
            &[(0, 1), (0, 2), (1, 3), (2, 4), (3, 4)],
            GraphDirection::TB,
        );

        let l1 = layout_diagram(&ir);
        let l2 = layout_diagram(&ir);

        assert_eq!(l1.nodes.len(), l2.nodes.len());
        for (n1, n2) in l1.nodes.iter().zip(l2.nodes.iter()) {
            assert!((n1.cx - n2.cx).abs() < 1e-12);
            assert!((n1.cy - n2.cy).abs() < 1e-12);
            assert!((n1.width - n2.width).abs() < 1e-12);
            assert!((n1.height - n2.height).abs() < 1e-12);
        }
        assert!((l1.quality.total_score - l2.quality.total_score).abs() < 1e-12);
    }

    #[test]
    fn integration_budget_respected() {
        let ir = make_test_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (1, 2), (2, 3), (3, 0)],
            GraphDirection::TB,
        );
        let config = LayoutConfig {
            iteration_budget: 5,
            max_crossing_iterations: 100,
            ..LayoutConfig::default()
        };
        let layout = layout_diagram_with_config(&ir, &config);
        // Should still produce a valid layout even if degraded
        assert_eq!(layout.nodes.len(), 4);
        // May or may not be degraded depending on how fast budget runs out
    }

    #[test]
    fn edge_routing_produces_waypoints() {
        let ir = make_test_ir(&["A", "B"], &[(0, 1)], GraphDirection::TB);
        let layout = layout_diagram(&ir);
        assert_eq!(layout.edges.len(), 1);
        assert!(layout.edges[0].waypoints.len() >= 2);
    }

    #[test]
    fn quality_score_computed() {
        let ir = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)], GraphDirection::TB);
        let layout = layout_diagram(&ir);
        // Linear chain should have 0 crossings
        assert_eq!(layout.quality.crossings, 0);
        assert!(layout.quality.total_score >= 0.0);
    }

    #[test]
    fn bounds_encompass_all_nodes() {
        let ir = make_test_ir(
            &["A", "B", "C", "D"],
            &[(0, 1), (0, 2), (1, 3), (2, 3)],
            GraphDirection::TB,
        );
        let layout = layout_diagram(&ir);
        let (bx0, by0, bx1, by1) = layout.bounds;
        for n in &layout.nodes {
            assert!(n.left() >= bx0 - 1e-9);
            assert!(n.right() <= bx1 + 1e-9);
            assert!(n.top() >= by0 - 1e-9);
            assert!(n.bottom() <= by1 + 1e-9);
        }
    }
}

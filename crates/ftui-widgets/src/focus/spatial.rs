#![forbid(unsafe_code)]

//! Spatial navigation: arrow-key focus movement based on widget geometry.
//!
//! Given a set of focusable nodes with bounding rectangles, finds the best
//! candidate in a given direction using a quadrant-based search with
//! weighted distance scoring.
//!
//! # Algorithm
//!
//! 1. Check for explicit edge override in the graph (takes precedence).
//! 2. Filter candidates to the correct quadrant relative to the current node.
//! 3. Score each candidate: `primary_distance + 0.3 × orthogonal_distance`.
//! 4. Return the candidate with the lowest score.
//!
//! # Invariants
//!
//! - Explicit graph edges always take precedence over spatial search.
//! - Only focusable nodes are considered.
//! - If no candidate exists in the direction, `None` is returned.
//! - The algorithm is deterministic: same layout → same navigation path.

use super::graph::{FocusGraph, FocusId, NavDirection};
use ftui_core::geometry::Rect;

/// Find the best spatial navigation target from `origin` in `dir`.
///
/// Returns `None` if no valid candidate exists. Explicit graph edges
/// take precedence over spatial search.
#[must_use]
pub fn spatial_navigate(graph: &FocusGraph, origin: FocusId, dir: NavDirection) -> Option<FocusId> {
    // 1. Check explicit edge first.
    if let Some(target) = graph.navigate(origin, dir)
        && graph.get(target).is_some_and(|n| n.is_focusable)
    {
        return Some(target);
    }

    // Only spatial directions make sense.
    if !matches!(
        dir,
        NavDirection::Up | NavDirection::Down | NavDirection::Left | NavDirection::Right
    ) {
        return None;
    }

    let origin_node = graph.get(origin)?;
    let oc = center_i32(&origin_node.bounds);

    let mut best: Option<(FocusId, i64)> = None;

    for candidate_id in graph.node_ids() {
        if candidate_id == origin {
            continue;
        }
        let candidate = match graph.get(candidate_id) {
            Some(n) if n.is_focusable => n,
            _ => continue,
        };

        let cc = center_i32(&candidate.bounds);

        if !in_quadrant_i32(oc, cc, dir) {
            continue;
        }

        let score = distance_score_i32(oc, cc, dir);

        if !best.is_some_and(|(_, best_score)| score >= best_score) {
            best = Some((candidate_id, score));
        }
    }

    best.map(|(id, _)| id)
}

/// Build spatial edges for all nodes in the graph.
///
/// For each node and each spatial direction (Up/Down/Left/Right),
/// computes the best spatial target and inserts the edge if no
/// explicit edge already exists.
pub fn build_spatial_edges(graph: &mut FocusGraph) {
    let ids: Vec<FocusId> = graph.node_ids().collect();
    let dirs = [
        NavDirection::Up,
        NavDirection::Down,
        NavDirection::Left,
        NavDirection::Right,
    ];

    // Collect edges to add (can't mutate graph while iterating).
    let mut edges_to_add = Vec::new();

    for &id in &ids {
        for dir in dirs {
            // Skip if explicit edge already exists.
            if graph.navigate(id, dir).is_some() {
                continue;
            }
            // Use a read-only spatial search.
            if let Some(target) = spatial_navigate(graph, id, dir) {
                edges_to_add.push((id, dir, target));
            }
        }
    }

    for (from, dir, to) in edges_to_add {
        graph.connect(from, dir, to);
    }
}

// --- Geometry helpers (integer arithmetic for debug-mode performance) ---

/// Doubled center coordinates to avoid division: `(2*x + w, 2*y + h)`.
///
/// Using doubled coords preserves exact ordering without floating point.
fn center_i32(r: &Rect) -> (i32, i32) {
    (
        2 * r.x as i32 + r.width as i32,
        2 * r.y as i32 + r.height as i32,
    )
}

/// Check if `candidate` center is in the correct quadrant relative to `origin`.
fn in_quadrant_i32(origin: (i32, i32), candidate: (i32, i32), dir: NavDirection) -> bool {
    let (ox, oy) = origin;
    let (cx, cy) = candidate;
    match dir {
        NavDirection::Up => cy < oy,
        NavDirection::Down => cy > oy,
        NavDirection::Left => cx < ox,
        NavDirection::Right => cx > ox,
        _ => false,
    }
}

/// Score a candidate: `10 × primary + 3 × orthogonal`.
///
/// This is equivalent to `primary + 0.3 × ortho` scaled by 10 to stay in
/// integer space. The relative ordering is preserved.
fn distance_score_i32(origin: (i32, i32), candidate: (i32, i32), dir: NavDirection) -> i64 {
    let (ox, oy) = origin;
    let (cx, cy) = candidate;

    let (primary, ortho) = match dir {
        NavDirection::Up => (i64::from(oy - cy), i64::from((ox - cx).abs())),
        NavDirection::Down => (i64::from(cy - oy), i64::from((ox - cx).abs())),
        NavDirection::Left => (i64::from(ox - cx), i64::from((oy - cy).abs())),
        NavDirection::Right => (i64::from(cx - ox), i64::from((oy - cy).abs())),
        _ => (i64::MAX / 2, 0),
    };

    10 * primary + 3 * ortho
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::focus::FocusNode;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    fn node_at(id: FocusId, x: u16, y: u16, w: u16, h: u16) -> FocusNode {
        FocusNode::new(id, rect(x, y, w, h))
    }

    /// Layout:
    /// ```text
    ///   [1]  [2]  [3]
    ///   [4]  [5]  [6]
    ///   [7]  [8]  [9]
    /// ```
    fn grid_3x3() -> FocusGraph {
        let mut g = FocusGraph::new();
        for row in 0..3u16 {
            for col in 0..3u16 {
                let id = (row * 3 + col + 1) as FocusId;
                g.insert(node_at(id, col * 12, row * 4, 10, 3));
            }
        }
        g
    }

    // --- Basic spatial navigation ---

    #[test]
    fn navigate_right_in_row() {
        let g = grid_3x3();
        let target = spatial_navigate(&g, 1, NavDirection::Right);
        assert_eq!(target, Some(2));
    }

    #[test]
    fn navigate_left_in_row() {
        let g = grid_3x3();
        let target = spatial_navigate(&g, 3, NavDirection::Left);
        assert_eq!(target, Some(2));
    }

    #[test]
    fn navigate_down_in_column() {
        let g = grid_3x3();
        let target = spatial_navigate(&g, 1, NavDirection::Down);
        assert_eq!(target, Some(4));
    }

    #[test]
    fn navigate_up_in_column() {
        let g = grid_3x3();
        let target = spatial_navigate(&g, 7, NavDirection::Up);
        assert_eq!(target, Some(4));
    }

    // --- Edge of grid ---

    #[test]
    fn no_target_left_at_edge() {
        let g = grid_3x3();
        let target = spatial_navigate(&g, 1, NavDirection::Left);
        assert_eq!(target, None);
    }

    #[test]
    fn no_target_up_at_edge() {
        let g = grid_3x3();
        let target = spatial_navigate(&g, 1, NavDirection::Up);
        assert_eq!(target, None);
    }

    #[test]
    fn no_target_right_at_edge() {
        let g = grid_3x3();
        let target = spatial_navigate(&g, 3, NavDirection::Right);
        assert_eq!(target, None);
    }

    #[test]
    fn no_target_down_at_edge() {
        let g = grid_3x3();
        let target = spatial_navigate(&g, 9, NavDirection::Down);
        assert_eq!(target, None);
    }

    // --- Diagonal preference ---

    #[test]
    fn prefers_aligned_over_diagonal() {
        // Node 5 (center) going right should prefer 6 (same row) over 3 (above-right).
        let g = grid_3x3();
        let target = spatial_navigate(&g, 5, NavDirection::Right);
        assert_eq!(target, Some(6));
    }

    // --- Explicit edge override ---

    #[test]
    fn explicit_edge_takes_precedence() {
        let mut g = grid_3x3();
        // Override: right from 1 goes to 9 (instead of spatial 2).
        g.connect(1, NavDirection::Right, 9);

        let target = spatial_navigate(&g, 1, NavDirection::Right);
        assert_eq!(target, Some(9));
    }

    // --- Unfocusable nodes ---

    #[test]
    fn skips_unfocusable_candidates() {
        let mut g = FocusGraph::new();
        g.insert(node_at(1, 0, 0, 10, 3));
        g.insert(node_at(2, 12, 0, 10, 3).with_focusable(false));
        g.insert(node_at(3, 24, 0, 10, 3));

        let target = spatial_navigate(&g, 1, NavDirection::Right);
        assert_eq!(target, Some(3)); // Skips 2.
    }

    // --- Next/Prev return None ---

    #[test]
    fn next_prev_not_spatial() {
        let g = grid_3x3();
        assert_eq!(spatial_navigate(&g, 1, NavDirection::Next), None);
        assert_eq!(spatial_navigate(&g, 1, NavDirection::Prev), None);
    }

    // --- build_spatial_edges ---

    #[test]
    fn build_spatial_edges_populates_grid() {
        let mut g = grid_3x3();
        build_spatial_edges(&mut g);

        // 1 should now have Right→2 and Down→4.
        assert_eq!(g.navigate(1, NavDirection::Right), Some(2));
        assert_eq!(g.navigate(1, NavDirection::Down), Some(4));

        // 5 (center) should have all four.
        assert!(g.navigate(5, NavDirection::Up).is_some());
        assert!(g.navigate(5, NavDirection::Down).is_some());
        assert!(g.navigate(5, NavDirection::Left).is_some());
        assert!(g.navigate(5, NavDirection::Right).is_some());
    }

    #[test]
    fn build_spatial_preserves_explicit_edges() {
        let mut g = grid_3x3();
        g.connect(1, NavDirection::Right, 9); // Override.
        build_spatial_edges(&mut g);

        // Explicit edge should be preserved.
        assert_eq!(g.navigate(1, NavDirection::Right), Some(9));
    }

    // --- Irregular layout ---

    #[test]
    fn navigate_irregular_layout() {
        // Wide button spanning columns:
        // [1] [2]
        // [  3  ]  (wide, centered)
        let mut g = FocusGraph::new();
        g.insert(node_at(1, 0, 0, 10, 3));
        g.insert(node_at(2, 12, 0, 10, 3));
        g.insert(node_at(3, 0, 4, 22, 3)); // Full width.

        // Down from 1 should go to 3 (directly below).
        let target = spatial_navigate(&g, 1, NavDirection::Down);
        assert_eq!(target, Some(3));

        // Down from 2 should also go to 3.
        let target = spatial_navigate(&g, 2, NavDirection::Down);
        assert_eq!(target, Some(3));
    }

    #[test]
    fn navigate_overlapping_widgets() {
        // Two overlapping nodes: pick the one in the direction.
        let mut g = FocusGraph::new();
        g.insert(node_at(1, 0, 0, 10, 5));
        g.insert(node_at(2, 5, 3, 10, 5)); // Overlaps partially.

        // Right from 1 → 2 (center of 2 is to the right).
        let target = spatial_navigate(&g, 1, NavDirection::Right);
        assert_eq!(target, Some(2));
    }

    // --- Single node ---

    #[test]
    fn single_node_no_target() {
        let mut g = FocusGraph::new();
        g.insert(node_at(1, 0, 0, 10, 3));

        for dir in NavDirection::ALL {
            assert_eq!(spatial_navigate(&g, 1, dir), None);
        }
    }

    // --- Empty graph ---

    #[test]
    fn empty_graph_returns_none() {
        let g = FocusGraph::new();
        assert_eq!(spatial_navigate(&g, 1, NavDirection::Right), None);
    }

    // --- Distance scoring ---

    #[test]
    fn closer_target_wins() {
        let mut g = FocusGraph::new();
        g.insert(node_at(1, 0, 0, 10, 3));
        g.insert(node_at(2, 12, 0, 10, 3)); // Close.
        g.insert(node_at(3, 50, 0, 10, 3)); // Far.

        let target = spatial_navigate(&g, 1, NavDirection::Right);
        assert_eq!(target, Some(2));
    }

    // --- Property: determinism ---

    #[test]
    fn property_deterministic() {
        let g = grid_3x3();
        let dirs = [
            NavDirection::Up,
            NavDirection::Down,
            NavDirection::Left,
            NavDirection::Right,
        ];

        for _ in 0..100 {
            for id in 1..=9 {
                for dir in dirs {
                    let a = spatial_navigate(&g, id, dir);
                    let b = spatial_navigate(&g, id, dir);
                    assert_eq!(a, b, "Non-deterministic for id={id}, dir={dir:?}");
                }
            }
        }
    }

    // --- Perf ---

    #[test]
    fn perf_spatial_navigate_100_nodes() {
        let mut g = FocusGraph::new();
        for row in 0..10u16 {
            for col in 0..10u16 {
                let id = (row * 10 + col + 1) as FocusId;
                g.insert(node_at(id, col * 12, row * 4, 10, 3));
            }
        }

        let start = std::time::Instant::now();
        for id in 1..=100 {
            for dir in [
                NavDirection::Up,
                NavDirection::Down,
                NavDirection::Left,
                NavDirection::Right,
            ] {
                let _ = spatial_navigate(&g, id, dir);
            }
        }
        let elapsed = start.elapsed();
        // 400 spatial navigations across 100 nodes.
        assert!(
            elapsed.as_micros() < 10_000,
            "400 spatial navigations took {}μs (budget: 10000μs)",
            elapsed.as_micros()
        );
    }

    #[test]
    fn perf_build_spatial_edges_100() {
        let mut g = FocusGraph::new();
        for row in 0..10u16 {
            for col in 0..10u16 {
                let id = (row * 10 + col + 1) as FocusId;
                g.insert(node_at(id, col * 12, row * 4, 10, 3));
            }
        }

        let start = std::time::Instant::now();
        build_spatial_edges(&mut g);
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_micros() < 50_000,
            "build_spatial_edges(100 nodes) took {}μs (budget: 50000μs)",
            elapsed.as_micros()
        );
    }
}

//! E2E test suite for incremental layout computation (bd-3p4y1.7).
//!
//! Every test emits structured JSONL records for post-hoc analysis,
//! regression triage, and evidence-ledger integration.
//!
//! Run with: `cargo test -p ftui-layout --test e2e_incremental -- --nocapture`
//!
//! JSONL schema per record:
//! ```json
//! { "test": "<name>", "phase": "<setup|execute|verify|teardown>",
//!   ...<phase-specific fields> }
//! ```

use ftui_core::geometry::Rect;
use ftui_layout::dep_graph::{CycleError, DepGraph, InputKind, NodeId};
use ftui_layout::incremental::IncrementalLayout;
use serde_json::json;
use std::collections::HashSet;
use std::io::Write as _;
use std::sync::Mutex;
use std::time::Instant;

// ============================================================================
// JSONL logging infrastructure
// ============================================================================

/// Thread-safe JSONL log buffer. Flushed to stderr at test end for capture.
struct JsonlLog {
    entries: Mutex<Vec<serde_json::Value>>,
}

impl JsonlLog {
    fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    fn emit(&self, entry: serde_json::Value) {
        self.entries.lock().unwrap().push(entry);
    }

    fn flush(&self, test_name: &str) {
        let entries = self.entries.lock().unwrap();
        let mut stderr = std::io::stderr().lock();
        for entry in entries.iter() {
            let _ = writeln!(stderr, "[JSONL] {test_name}: {entry}");
        }
    }

    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

/// Nanosecond timestamp relative to a start instant.
fn elapsed_ns(start: &Instant) -> u64 {
    start.elapsed().as_nanos() as u64
}

// ============================================================================
// Helpers
// ============================================================================

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

/// Deterministic pseudo-random from seed (xorshift32).
fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Walk a 3-level tree (root → children → grandchildren) computing layout.
fn walk_tree(inc: &mut IncrementalLayout, root: NodeId, root_area: Rect) {
    let child_count = inc.graph().dependents(root).len();
    let root_rects = inc.get_or_compute(root, root_area, |a| split_equal(a, child_count.max(1)));
    let children: Vec<_> = inc.graph().dependents(root).to_vec();
    for (i, child) in children.iter().enumerate() {
        let child_area = if i < root_rects.len() {
            root_rects[i]
        } else {
            Rect::default()
        };
        let gc_count = inc.graph().dependents(*child).len();
        let child_rects =
            inc.get_or_compute(*child, child_area, |a| split_equal(a, gc_count.max(1)));
        let grandchildren: Vec<_> = inc.graph().dependents(*child).to_vec();
        for (j, gc) in grandchildren.iter().enumerate() {
            let gc_area = if j < child_rects.len() {
                child_rects[j]
            } else {
                Rect::default()
            };
            inc.get_or_compute(*gc, gc_area, |a| vec![a]);
        }
    }
}

/// Build a 3-level tree: root → `children` children → `gc_per` grandchildren each.
fn build_tree(
    children: usize,
    gc_per: usize,
) -> (IncrementalLayout, NodeId, Vec<NodeId>, Vec<NodeId>) {
    let total = 1 + children + children * gc_per;
    let mut inc = IncrementalLayout::with_capacity(total);
    let root = inc.add_node(None);
    let mut child_ids = Vec::with_capacity(children);
    let mut gc_ids = Vec::with_capacity(children * gc_per);

    for _ in 0..children {
        let child = inc.add_node(Some(root));
        child_ids.push(child);
        for _ in 0..gc_per {
            let gc = inc.add_node(Some(child));
            gc_ids.push(gc);
        }
    }
    (inc, root, child_ids, gc_ids)
}

/// Collect all result hashes across the tree for isomorphism comparison.
fn collect_hashes(inc: &IncrementalLayout, root: NodeId) -> Vec<(NodeId, Option<u64>)> {
    let mut result = vec![(root, inc.result_hash(root))];
    let children: Vec<_> = inc.graph().dependents(root).to_vec();
    for child in &children {
        result.push((*child, inc.result_hash(*child)));
        let gcs: Vec<_> = inc.graph().dependents(*child).to_vec();
        for gc in &gcs {
            result.push((*gc, inc.result_hash(*gc)));
        }
    }
    result
}

// ============================================================================
// Unit Tests: Dependency Graph Construction
// ============================================================================

#[test]
fn e2e_dep_graph_construction_widget_tree() {
    let log = JsonlLog::new();
    let start = Instant::now();

    log.emit(json!({
        "test": "dep_graph_construction_widget_tree",
        "phase": "setup",
        "timestamp_ns": elapsed_ns(&start),
    }));

    let mut g = DepGraph::new();
    let root = g.add_node();
    let sidebar = g.add_node();
    let content = g.add_node();
    let header = g.add_node();
    let body = g.add_node();
    let footer = g.add_node();

    // sidebar and content depend on root
    g.add_edge(sidebar, root).unwrap();
    g.add_edge(content, root).unwrap();
    g.set_parent(sidebar, root);
    g.set_parent(content, root);

    // header, body, footer depend on content
    g.add_edge(header, content).unwrap();
    g.add_edge(body, content).unwrap();
    g.add_edge(footer, content).unwrap();
    g.set_parent(header, content);
    g.set_parent(body, content);
    g.set_parent(footer, content);

    log.emit(json!({
        "test": "dep_graph_construction_widget_tree",
        "phase": "execute",
        "node_count": g.node_count(),
        "edge_count": g.edge_count(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    // Verify structure.
    assert_eq!(g.node_count(), 6);
    assert_eq!(g.edge_count(), 5);
    assert_eq!(g.dependents(root).len(), 2);
    assert_eq!(g.dependents(content).len(), 3);
    assert_eq!(g.dependents(sidebar).len(), 0);
    assert_eq!(g.dependencies(sidebar), &[root]);
    assert_eq!(g.dependencies(header), &[content]);
    assert_eq!(g.parent(sidebar), Some(root));
    assert_eq!(g.parent(header), Some(content));
    assert_eq!(g.parent(root), None);

    log.emit(json!({
        "test": "dep_graph_construction_widget_tree",
        "phase": "verify",
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("dep_graph_construction_widget_tree");
}

#[test]
fn e2e_dep_graph_construction_flat_100() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut g = DepGraph::with_capacity(101, 100);
    let root = g.add_node();
    for _ in 0..100 {
        let child = g.add_node();
        g.add_edge(child, root).unwrap();
        g.set_parent(child, root);
    }

    log.emit(json!({
        "test": "dep_graph_construction_flat_100",
        "phase": "execute",
        "node_count": g.node_count(),
        "edge_count": g.edge_count(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(g.node_count(), 101);
    assert_eq!(g.edge_count(), 100);
    assert_eq!(g.dependents(root).len(), 100);

    log.emit(json!({
        "test": "dep_graph_construction_flat_100",
        "phase": "verify",
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("dep_graph_construction_flat_100");
}

// ============================================================================
// Unit Tests: Dirty Propagation
// ============================================================================

#[test]
fn e2e_dirty_propagation_exact_transitive_closure() {
    let log = JsonlLog::new();
    let start = Instant::now();

    // Tree: R → (A, B), A → (C, D), B → E
    let mut g = DepGraph::new();
    let r = g.add_node();
    let a = g.add_node();
    let b = g.add_node();
    let c = g.add_node();
    let d = g.add_node();
    let e = g.add_node();

    g.add_edge(a, r).unwrap();
    g.add_edge(b, r).unwrap();
    g.add_edge(c, a).unwrap();
    g.add_edge(d, a).unwrap();
    g.add_edge(e, b).unwrap();
    g.set_parent(a, r);
    g.set_parent(b, r);
    g.set_parent(c, a);
    g.set_parent(d, a);
    g.set_parent(e, b);

    // Dirty A → should dirty C and D, but NOT B or E.
    g.mark_dirty(a);
    let dirty = g.propagate();
    let dirty_set: HashSet<_> = dirty.iter().copied().collect();

    log.emit(json!({
        "test": "dirty_propagation_exact_transitive_closure",
        "phase": "execute",
        "dirty_count": dirty.len(),
        "dirty_ids": dirty.iter().map(|n| n.raw()).collect::<Vec<_>>(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    // A, C, D should be dirty.
    assert!(dirty_set.contains(&a));
    assert!(dirty_set.contains(&c));
    assert!(dirty_set.contains(&d));
    // R, B, E should NOT be dirty.
    assert!(!dirty_set.contains(&r));
    assert!(!dirty_set.contains(&b));
    assert!(!dirty_set.contains(&e));
    assert_eq!(dirty.len(), 3, "no over-propagation");

    log.emit(json!({
        "test": "dirty_propagation_exact_transitive_closure",
        "phase": "verify",
        "pass": true,
        "no_over_propagation": true,
        "no_under_propagation": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("dirty_propagation_exact_transitive_closure");
}

#[test]
fn e2e_dirty_propagation_root_dirties_all() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut g = DepGraph::with_capacity(111, 110);
    let root = g.add_node();
    for _ in 0..10 {
        let child = g.add_node();
        g.add_edge(child, root).unwrap();
        g.set_parent(child, root);
        for _ in 0..10 {
            let gc = g.add_node();
            g.add_edge(gc, child).unwrap();
            g.set_parent(gc, child);
        }
    }

    g.mark_dirty(root);
    let dirty = g.propagate();

    log.emit(json!({
        "test": "dirty_propagation_root_dirties_all",
        "phase": "execute",
        "total_nodes": g.node_count(),
        "dirty_count": dirty.len(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(
        dirty.len(),
        111,
        "root dirty should propagate to all descendants"
    );

    log.emit(json!({
        "test": "dirty_propagation_root_dirties_all",
        "phase": "verify",
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("dirty_propagation_root_dirties_all");
}

#[test]
fn e2e_dirty_propagation_leaf_only() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut g = DepGraph::with_capacity(111, 110);
    let root = g.add_node();
    let mut leaves = Vec::new();
    for _ in 0..10 {
        let child = g.add_node();
        g.add_edge(child, root).unwrap();
        g.set_parent(child, root);
        for _ in 0..10 {
            let gc = g.add_node();
            g.add_edge(gc, child).unwrap();
            g.set_parent(gc, child);
            leaves.push(gc);
        }
    }

    // Dirty one leaf → only that leaf in dirty set (no dependents).
    g.mark_dirty(leaves[42]);
    let dirty = g.propagate();

    log.emit(json!({
        "test": "dirty_propagation_leaf_only",
        "phase": "execute",
        "dirty_count": dirty.len(),
        "leaf_id": leaves[42].raw(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0], leaves[42]);

    log.emit(json!({
        "test": "dirty_propagation_leaf_only",
        "phase": "verify",
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("dirty_propagation_leaf_only");
}

#[test]
fn e2e_dirty_propagation_diamond_no_duplicate() {
    let log = JsonlLog::new();
    let start = Instant::now();

    // Diamond: A → B, A → C, B → D, C → D
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

    log.emit(json!({
        "test": "dirty_propagation_diamond_no_duplicate",
        "phase": "execute",
        "dirty_count": dirty.len(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(dirty.len(), 4);
    let unique: HashSet<_> = dirty.iter().collect();
    assert_eq!(unique.len(), 4, "each node appears exactly once in diamond");

    log.flush("dirty_propagation_diamond_no_duplicate");
}

// ============================================================================
// Unit Tests: Cycle Detection
// ============================================================================

#[test]
fn e2e_cycle_detection_self_loop() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut g = DepGraph::new();
    let a = g.add_node();
    let result = g.add_edge(a, a);

    log.emit(json!({
        "test": "cycle_detection_self_loop",
        "phase": "execute",
        "cycle_detected": result.is_err(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), CycleError { from: a, to: a });

    log.flush("cycle_detection_self_loop");
}

#[test]
fn e2e_cycle_detection_two_node() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut g = DepGraph::new();
    let a = g.add_node();
    let b = g.add_node();
    g.add_edge(a, b).unwrap();
    let result = g.add_edge(b, a);

    log.emit(json!({
        "test": "cycle_detection_two_node",
        "phase": "execute",
        "cycle_detected": result.is_err(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), CycleError { from: b, to: a });

    log.flush("cycle_detection_two_node");
}

#[test]
fn e2e_cycle_detection_long_chain() {
    let log = JsonlLog::new();
    let start = Instant::now();

    // Chain: A → B → C → D → E. Trying E → A creates cycle.
    let mut g = DepGraph::new();
    let mut nodes = Vec::new();
    for _ in 0..5 {
        nodes.push(g.add_node());
    }
    for i in 0..4 {
        g.add_edge(nodes[i], nodes[i + 1]).unwrap();
    }

    let result = g.add_edge(nodes[4], nodes[0]);

    log.emit(json!({
        "test": "cycle_detection_long_chain",
        "phase": "execute",
        "chain_length": 5,
        "cycle_detected": result.is_err(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert!(result.is_err());

    log.flush("cycle_detection_long_chain");
}

#[test]
fn e2e_cycle_detection_diamond_no_false_positive() {
    let log = JsonlLog::new();
    let start = Instant::now();

    // Diamond is not a cycle: A → B, A → C, B → D, C → D.
    let mut g = DepGraph::new();
    let a = g.add_node();
    let b = g.add_node();
    let c = g.add_node();
    let d = g.add_node();
    g.add_edge(a, b).unwrap();
    g.add_edge(a, c).unwrap();
    g.add_edge(b, d).unwrap();
    let result = g.add_edge(c, d);

    log.emit(json!({
        "test": "cycle_detection_diamond_no_false_positive",
        "phase": "execute",
        "false_positive": result.is_err(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert!(result.is_ok(), "diamond should not be detected as cycle");

    log.flush("cycle_detection_diamond_no_false_positive");
}

// ============================================================================
// Unit Tests: Cache Invalidation
// ============================================================================

#[test]
fn e2e_cache_invalidation_exactly_right_nodes() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let (mut inc, root, _children, grandchildren) = build_tree(5, 3);
    inc.propagate();

    let a = area(200, 60);

    // Full first pass.
    walk_tree(&mut inc, root, a);

    let s = inc.stats();
    let first_pass_recomputed = s.recomputed;

    log.emit(json!({
        "test": "cache_invalidation_exactly_right_nodes",
        "phase": "setup",
        "total_nodes": inc.node_count(),
        "first_pass_recomputed": first_pass_recomputed,
        "timestamp_ns": elapsed_ns(&start),
    }));

    inc.reset_stats();

    // Dirty grandchild[7] (child of children[2]).
    inc.mark_dirty(grandchildren[7]);
    inc.propagate();

    walk_tree(&mut inc, root, a);

    let s = inc.stats();

    log.emit(json!({
        "test": "cache_invalidation_exactly_right_nodes",
        "phase": "execute",
        "recomputed": s.recomputed,
        "cached": s.cached,
        "total": s.total,
        "hit_rate": s.hit_rate(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    // Only grandchild[7] should recompute (1 node).
    assert_eq!(s.recomputed, 1, "exactly 1 node should recompute");
    assert_eq!(s.cached, inc.node_count() - 1, "all other nodes cached");

    log.emit(json!({
        "test": "cache_invalidation_exactly_right_nodes",
        "phase": "verify",
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("cache_invalidation_exactly_right_nodes");
}

#[test]
fn e2e_cache_invalidation_hash_dedup() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut inc = IncrementalLayout::new();
    let n = inc.add_node(None);
    inc.propagate();

    let a = area(80, 24);
    inc.get_or_compute(n, a, |a| split_equal(a, 2));

    // Set hash = 42.
    inc.mark_changed(n, InputKind::Constraint, 42);
    inc.propagate();
    inc.get_or_compute(n, a, |a| split_equal(a, 2));

    inc.reset_stats();

    // Same hash again → should NOT dirty.
    inc.mark_changed(n, InputKind::Constraint, 42);
    inc.propagate();
    inc.get_or_compute(n, a, |a| split_equal(a, 2));

    let s = inc.stats();

    log.emit(json!({
        "test": "cache_invalidation_hash_dedup",
        "phase": "execute",
        "recomputed": s.recomputed,
        "cached": s.cached,
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(s.cached, 1, "same hash should not trigger recompute");
    assert_eq!(s.recomputed, 0);

    log.flush("cache_invalidation_hash_dedup");
}

#[test]
fn e2e_cache_invalidation_area_change() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut inc = IncrementalLayout::new();
    let n = inc.add_node(None);
    inc.propagate();

    inc.get_or_compute(n, area(80, 24), |a| split_equal(a, 2));
    inc.reset_stats();

    // Same node, different area → must recompute.
    inc.get_or_compute(n, area(120, 40), |a| split_equal(a, 2));

    let s = inc.stats();

    log.emit(json!({
        "test": "cache_invalidation_area_change",
        "phase": "execute",
        "recomputed": s.recomputed,
        "cached": s.cached,
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(s.recomputed, 1, "area change should trigger recompute");

    log.flush("cache_invalidation_area_change");
}

// ============================================================================
// Unit Tests: Generation Counter
// ============================================================================

#[test]
fn e2e_generation_counter_o1_dirty_check() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut g = DepGraph::with_capacity(10_000, 0);
    let mut nodes = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        nodes.push(g.add_node());
    }

    // All should be clean (no dirty marking).
    let check_start = Instant::now();
    let mut dirty_count = 0;
    for &n in &nodes {
        if g.is_dirty(n) {
            dirty_count += 1;
        }
    }
    let check_ns = elapsed_ns(&check_start);

    log.emit(json!({
        "test": "generation_counter_o1_dirty_check",
        "phase": "execute",
        "nodes_checked": 10_000,
        "dirty_found": dirty_count,
        "check_elapsed_ns": check_ns,
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(dirty_count, 0);
    // Sanity: 10K checks should be well under 1ms.
    assert!(
        check_ns < 10_000_000,
        "10K dirty checks took {check_ns}ns, expected < 10ms"
    );

    log.flush("generation_counter_o1_dirty_check");
}

#[test]
fn e2e_generation_counter_clean_all_o1() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut g = DepGraph::with_capacity(10_000, 0);
    for _ in 0..10_000 {
        let n = g.add_node();
        g.mark_dirty(n);
    }
    g.propagate();

    assert_eq!(g.dirty_count(), 10_000);

    let clean_start = Instant::now();
    g.clean_all();
    let clean_ns = elapsed_ns(&clean_start);

    log.emit(json!({
        "test": "generation_counter_clean_all_o1",
        "phase": "execute",
        "nodes_cleaned": 10_000,
        "clean_elapsed_ns": clean_ns,
        "dirty_after_clean": g.dirty_count(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(g.dirty_count(), 0);

    log.flush("generation_counter_clean_all_o1");
}

#[test]
fn e2e_generation_counter_wrap_around() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut g = DepGraph::new();
    let a = g.add_node();
    let b = g.add_node();
    g.add_edge(b, a).unwrap();

    // Simulate many clean_all cycles to approach generation wrap.
    // (We can't easily hit u32::MAX, but we can verify the mechanism works.)
    for _ in 0..1000 {
        g.mark_dirty(a);
        g.propagate();
        g.clean_all();
    }

    // After 1000 cycles, should still function correctly.
    g.mark_dirty(a);
    let dirty = g.propagate();
    assert!(dirty.contains(&a));
    assert!(dirty.contains(&b));

    g.clean_all();
    assert!(!g.is_dirty(a));
    assert!(!g.is_dirty(b));

    log.emit(json!({
        "test": "generation_counter_wrap_around",
        "phase": "verify",
        "cycles": 1000,
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("generation_counter_wrap_around");
}

// ============================================================================
// Unit Tests: Edge Cases
// ============================================================================

#[test]
fn e2e_edge_case_empty_tree() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let inc = IncrementalLayout::new();
    assert_eq!(inc.node_count(), 0);
    assert_eq!(inc.cache_len(), 0);

    let s = inc.stats();
    assert_eq!(s.total, 0);
    assert_eq!(s.recomputed, 0);
    assert_eq!(s.cached, 0);

    log.emit(json!({
        "test": "edge_case_empty_tree",
        "phase": "verify",
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("edge_case_empty_tree");
}

#[test]
fn e2e_edge_case_single_node() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut inc = IncrementalLayout::new();
    let n = inc.add_node(None);
    inc.propagate();

    let rects = inc.get_or_compute(n, area(80, 24), |_| vec![]);
    assert!(rects.is_empty());

    let s = inc.stats();
    assert_eq!(s.total, 1);
    assert_eq!(s.recomputed, 1);

    log.emit(json!({
        "test": "edge_case_single_node",
        "phase": "verify",
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("edge_case_single_node");
}

#[test]
fn e2e_edge_case_10k_flat_nodes() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut inc = IncrementalLayout::with_capacity(10_001);
    let root = inc.add_node(None);
    let mut children = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        children.push(inc.add_node(Some(root)));
    }
    inc.propagate();

    log.emit(json!({
        "test": "edge_case_10k_flat_nodes",
        "phase": "setup",
        "node_count": inc.node_count(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    let a = area(200, 60);

    // Full pass.
    let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 1));
    for &c in &children {
        inc.get_or_compute(c, root_rects[0], |a| vec![a]);
    }

    let s = inc.stats();
    assert_eq!(s.recomputed, 10_001);

    inc.reset_stats();

    // Incremental: dirty 1 child.
    inc.mark_dirty(children[5000]);
    inc.propagate();

    let root_rects = inc.get_or_compute(root, a, |a| split_equal(a, 1));
    for &c in &children {
        inc.get_or_compute(c, root_rects[0], |a| vec![a]);
    }

    let s = inc.stats();

    log.emit(json!({
        "test": "edge_case_10k_flat_nodes",
        "phase": "execute",
        "recomputed": s.recomputed,
        "cached": s.cached,
        "hit_rate": s.hit_rate(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(s.recomputed, 1, "only 1 of 10K nodes should recompute");
    assert_eq!(s.cached, 10_000);

    log.flush("edge_case_10k_flat_nodes");
}

#[test]
fn e2e_edge_case_add_remove_during_layout() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut inc = IncrementalLayout::new();
    let root = inc.add_node(None);
    let _c1 = inc.add_node(Some(root));
    let c2 = inc.add_node(Some(root));
    inc.propagate();

    let a = area(120, 24);

    // Pass 1: root + 2 children.
    walk_tree(&mut inc, root, a);
    assert_eq!(inc.cache_len(), 3);

    // Remove c2 mid-layout.
    inc.remove_node(c2);
    assert!(
        inc.is_dirty(root),
        "parent should be dirty after child removal"
    );
    assert_eq!(inc.cache_len(), 2); // c2 evicted

    // Add new child.
    let c3 = inc.add_node(Some(root));
    assert!(inc.is_dirty(c3));

    inc.propagate();

    // Pass 2: root + c1 + c3 (c2 gone).
    inc.reset_stats();
    walk_tree(&mut inc, root, a);

    let s = inc.stats();

    log.emit(json!({
        "test": "edge_case_add_remove_during_layout",
        "phase": "execute",
        "nodes_after": inc.node_count(),
        "recomputed": s.recomputed,
        "cached": s.cached,
        "timestamp_ns": elapsed_ns(&start),
    }));

    // root and c3 must recompute; c1 may or may not depending on area changes.
    assert!(s.recomputed >= 2, "root + c3 should recompute at minimum");

    log.flush("edge_case_add_remove_during_layout");
}

#[test]
fn e2e_edge_case_zero_size_area() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut inc = IncrementalLayout::new();
    let n = inc.add_node(None);
    inc.propagate();

    let zero = Rect::default();
    inc.get_or_compute(n, zero, |_| vec![]);

    // Same zero area → cache hit.
    inc.reset_stats();
    inc.get_or_compute(n, zero, |_| vec![]);
    assert_eq!(inc.stats().cached, 1);

    log.emit(json!({
        "test": "edge_case_zero_size_area",
        "phase": "verify",
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("edge_case_zero_size_area");
}

#[test]
fn e2e_edge_case_multiple_input_kinds() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut inc = IncrementalLayout::new();
    let n = inc.add_node(None);
    inc.propagate();

    let a = area(80, 24);
    inc.get_or_compute(n, a, |a| split_equal(a, 2));

    // Change constraint → dirty.
    inc.mark_changed(n, InputKind::Constraint, 10);
    inc.propagate();
    inc.get_or_compute(n, a, |a| split_equal(a, 2));

    // Change content with SAME hash → no dirty.
    inc.reset_stats();
    inc.mark_changed(n, InputKind::Content, 0); // Hash 0 is the initial value
    inc.propagate();
    inc.get_or_compute(n, a, |a| split_equal(a, 2));
    assert_eq!(inc.stats().cached, 1, "same content hash should not dirty");

    // Change style → dirty.
    inc.mark_changed(n, InputKind::Style, 99);
    inc.propagate();
    inc.reset_stats();
    inc.get_or_compute(n, a, |a| split_equal(a, 2));
    assert_eq!(inc.stats().recomputed, 1, "new style hash should dirty");

    log.emit(json!({
        "test": "edge_case_multiple_input_kinds",
        "phase": "verify",
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("edge_case_multiple_input_kinds");
}

// ============================================================================
// Unit Tests: DFS Pre-Order Determinism
// ============================================================================

#[test]
fn e2e_propagation_order_is_deterministic() {
    let log = JsonlLog::new();
    let start = Instant::now();

    // Run propagation 100 times, verify order is identical each time.
    let mut g = DepGraph::new();
    let r = g.add_node();
    let a = g.add_node();
    let b = g.add_node();
    let c = g.add_node();
    let d = g.add_node();
    g.add_edge(a, r).unwrap();
    g.add_edge(b, r).unwrap();
    g.add_edge(c, a).unwrap();
    g.add_edge(d, b).unwrap();
    g.set_parent(a, r);
    g.set_parent(b, r);
    g.set_parent(c, a);
    g.set_parent(d, b);

    let mut reference: Option<Vec<NodeId>> = None;
    for i in 0..100 {
        g.clean_all();
        g.mark_dirty(r);
        let dirty = g.propagate();
        if let Some(ref expected) = reference {
            assert_eq!(
                &dirty, expected,
                "propagation order diverged at iteration {i}"
            );
        } else {
            reference = Some(dirty);
        }
    }

    log.emit(json!({
        "test": "propagation_order_is_deterministic",
        "phase": "verify",
        "iterations": 100,
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("propagation_order_is_deterministic");
}

// ============================================================================
// Unit Tests: Node Removal and Slot Recycling
// ============================================================================

#[test]
fn e2e_node_removal_and_recycling() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let mut g = DepGraph::new();
    let a = g.add_node();
    let b = g.add_node();
    let c = g.add_node();
    assert_eq!(g.node_count(), 3);

    g.remove_node(b);
    assert_eq!(g.node_count(), 2);

    // New node should reuse b's slot.
    let d = g.add_node();
    assert_eq!(d.raw(), b.raw(), "slot should be recycled");
    assert_eq!(g.node_count(), 3);

    // b was removed, so should not be dirty.
    assert!(!g.is_dirty(b));
    // d is fresh, not yet marked dirty.
    assert!(!g.is_dirty(d));

    log.emit(json!({
        "test": "node_removal_and_recycling",
        "phase": "verify",
        "recycled_slot": d.raw(),
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    // Suppress unused warnings.
    let _ = (a, c);

    log.flush("node_removal_and_recycling");
}

// ============================================================================
// E2E Tests: Full Isomorphism
// ============================================================================

#[test]
fn e2e_isomorphism_incremental_equals_full() {
    let log = JsonlLog::new();
    let start = Instant::now();

    // Test with multiple tree sizes.
    for &(children, gc_per) in &[(3, 2), (5, 3), (10, 10), (20, 5)] {
        let total = 1 + children + children * gc_per;
        let (mut inc, root, _child_ids, _gc_ids) = build_tree(children, gc_per);
        inc.propagate();

        let a = area(200, 60);

        // Incremental pass (cold cache = full computation).
        walk_tree(&mut inc, root, a);
        let incr_hashes = collect_hashes(&inc, root);
        let incr_root_hash = inc.result_hash(root);

        // Force-full pass.
        inc.set_force_full(true);
        inc.reset_stats();
        walk_tree(&mut inc, root, a);
        let full_hashes = collect_hashes(&inc, root);
        let full_root_hash = inc.result_hash(root);
        inc.set_force_full(false);

        log.emit(json!({
            "test": "isomorphism_incremental_equals_full",
            "phase": "verify",
            "tree_size": total,
            "children": children,
            "gc_per": gc_per,
            "incr_root_hash": incr_root_hash,
            "full_root_hash": full_root_hash,
            "checksum_match": incr_root_hash == full_root_hash,
            "timestamp_ns": elapsed_ns(&start),
        }));

        assert_eq!(
            incr_root_hash, full_root_hash,
            "root hash mismatch for {children}x{gc_per} tree"
        );

        // Verify every node hash matches.
        for (i, ((id_i, h_i), (id_f, h_f))) in
            incr_hashes.iter().zip(full_hashes.iter()).enumerate()
        {
            assert_eq!(id_i, id_f, "node order mismatch at position {i}");
            assert_eq!(
                h_i, h_f,
                "hash mismatch at node {id_i} (position {i}) for {children}x{gc_per} tree"
            );
        }
    }

    log.flush("isomorphism_incremental_equals_full");
}

#[test]
fn e2e_isomorphism_after_partial_dirty() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let (mut inc, root, _children, grandchildren) = build_tree(10, 10);
    inc.propagate();

    let a = area(200, 60);

    // Full first pass.
    walk_tree(&mut inc, root, a);

    // Dirty some nodes, then verify incremental == full.
    let dirty_indices = [0, 15, 42, 77, 99];
    for &idx in &dirty_indices {
        inc.mark_dirty(grandchildren[idx]);
    }
    inc.propagate();
    walk_tree(&mut inc, root, a);

    let incr_hashes = collect_hashes(&inc, root);

    // Force-full recomputation.
    inc.invalidate_all();
    inc.propagate();
    inc.set_force_full(true);
    walk_tree(&mut inc, root, a);
    let full_hashes = collect_hashes(&inc, root);
    inc.set_force_full(false);

    log.emit(json!({
        "test": "isomorphism_after_partial_dirty",
        "phase": "verify",
        "dirty_count": dirty_indices.len(),
        "total_nodes": inc.node_count(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    for (i, ((id_i, h_i), (id_f, h_f))) in incr_hashes.iter().zip(full_hashes.iter()).enumerate() {
        assert_eq!(id_i, id_f, "node order mismatch at {i}");
        assert_eq!(
            h_i, h_f,
            "hash mismatch at node {id_i} (position {i}): incr={h_i:?} full={h_f:?}"
        );
    }

    log.flush("isomorphism_after_partial_dirty");
}

// ============================================================================
// E2E Tests: Mutation Stress
// ============================================================================

#[test]
fn e2e_mutation_stress_seeded_rng() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let (mut inc, root, _children, grandchildren) = build_tree(10, 10);
    inc.propagate();

    let a = area(200, 60);

    // Initial full pass.
    walk_tree(&mut inc, root, a);

    let num_frames = 200;
    let mut rng_state = 0xDEAD_BEEFu32; // Deterministic seed.

    for frame in 0..num_frames {
        // Randomly dirty 1-5 grandchildren.
        let num_dirty = (xorshift32(&mut rng_state) % 5 + 1) as usize;
        for _ in 0..num_dirty {
            let idx = (xorshift32(&mut rng_state) as usize) % grandchildren.len();
            inc.mark_dirty(grandchildren[idx]);
        }
        inc.propagate();
        inc.reset_stats();

        // Incremental pass.
        walk_tree(&mut inc, root, a);
        let incr_hash = inc.result_hash(root);

        // Force-full pass.
        inc.invalidate_all();
        inc.propagate();
        inc.set_force_full(true);
        walk_tree(&mut inc, root, a);
        let full_hash = inc.result_hash(root);
        inc.set_force_full(false);

        if frame % 50 == 0 {
            log.emit(json!({
                "test": "mutation_stress_seeded_rng",
                "phase": "execute",
                "frame": frame,
                "num_dirty": num_dirty,
                "incr_hash": incr_hash,
                "full_hash": full_hash,
                "checksum_match": incr_hash == full_hash,
                "timestamp_ns": elapsed_ns(&start),
            }));
        }

        assert_eq!(
            incr_hash, full_hash,
            "frame {frame}: incremental != full (dirty={num_dirty})"
        );
    }

    log.emit(json!({
        "test": "mutation_stress_seeded_rng",
        "phase": "verify",
        "frames_tested": num_frames,
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("mutation_stress_seeded_rng");
}

// ============================================================================
// E2E Tests: Resize During Incremental
// ============================================================================

#[test]
fn e2e_resize_during_incremental() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let (mut inc, root, _children, grandchildren) = build_tree(5, 4);
    inc.propagate();

    // Start at 200x60.
    let areas = [
        area(200, 60),
        area(120, 40),
        area(80, 24),
        area(300, 80),
        area(200, 60), // Back to original.
    ];

    // Full pass at initial size.
    walk_tree(&mut inc, root, areas[0]);

    for (i, &a) in areas.iter().enumerate().skip(1) {
        // Dirty a node to simulate concurrent content change + resize.
        if i < grandchildren.len() {
            inc.mark_dirty(grandchildren[i]);
        }
        inc.propagate();
        inc.reset_stats();

        // Incremental pass at new size.
        walk_tree(&mut inc, root, a);
        let incr_hash = inc.result_hash(root);

        // Full recomputation at same size.
        inc.invalidate_all();
        inc.propagate();
        inc.set_force_full(true);
        walk_tree(&mut inc, root, a);
        let full_hash = inc.result_hash(root);
        inc.set_force_full(false);

        log.emit(json!({
            "test": "resize_during_incremental",
            "phase": "execute",
            "resize_step": i,
            "area_w": a.width,
            "area_h": a.height,
            "incr_hash": incr_hash,
            "full_hash": full_hash,
            "checksum_match": incr_hash == full_hash,
            "timestamp_ns": elapsed_ns(&start),
        }));

        assert_eq!(
            incr_hash, full_hash,
            "resize step {i} ({}x{}): incremental != full",
            a.width, a.height
        );
    }

    log.emit(json!({
        "test": "resize_during_incremental",
        "phase": "verify",
        "resize_steps": areas.len() - 1,
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("resize_during_incremental");
}

// ============================================================================
// E2E Tests: Concurrent Dirty Marking
// ============================================================================

#[test]
fn e2e_concurrent_dirty_marking() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let (mut inc, root, children, grandchildren) = build_tree(10, 10);
    inc.propagate();

    let a = area(200, 60);

    // Full first pass.
    walk_tree(&mut inc, root, a);

    // Simulate multiple input sources marking dirty simultaneously:
    // Source 1: constraint change on grandchild[0].
    inc.mark_changed(grandchildren[0], InputKind::Constraint, 42);
    // Source 2: content change on grandchild[50].
    inc.mark_changed(grandchildren[50], InputKind::Content, 99);
    // Source 3: style change on child[3].
    inc.mark_changed(children[3], InputKind::Style, 77);
    // Source 4: force-dirty on grandchild[99].
    inc.mark_dirty(grandchildren[99]);

    inc.propagate();
    inc.reset_stats();

    // Incremental pass.
    walk_tree(&mut inc, root, a);
    let incr_hash = inc.result_hash(root);
    let incr_stats = inc.stats();

    // Full pass.
    inc.invalidate_all();
    inc.propagate();
    inc.set_force_full(true);
    walk_tree(&mut inc, root, a);
    let full_hash = inc.result_hash(root);
    inc.set_force_full(false);

    log.emit(json!({
        "test": "concurrent_dirty_marking",
        "phase": "execute",
        "dirty_sources": 4,
        "recomputed": incr_stats.recomputed,
        "cached": incr_stats.cached,
        "hit_rate": incr_stats.hit_rate(),
        "incr_hash": incr_hash,
        "full_hash": full_hash,
        "checksum_match": incr_hash == full_hash,
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(
        incr_hash, full_hash,
        "concurrent dirty: incremental != full"
    );

    // Verify that recomputed count is reasonable (not all nodes).
    assert!(
        incr_stats.recomputed < inc.node_count(),
        "should not recompute all {} nodes, got {}",
        inc.node_count(),
        incr_stats.recomputed
    );

    log.emit(json!({
        "test": "concurrent_dirty_marking",
        "phase": "verify",
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("concurrent_dirty_marking");
}

// ============================================================================
// E2E Tests: Mark Dirty With Ancestors (Flex Sibling Pattern)
// ============================================================================

#[test]
fn e2e_flex_sibling_isomorphism() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let (mut inc, root, children, _grandchildren) = build_tree(5, 3);
    inc.propagate();

    let a = area(200, 60);

    // Full first pass.
    walk_tree(&mut inc, root, a);

    // Use mark_dirty_with_ancestors (the flex sibling pattern).
    inc.mark_dirty_with_ancestors(children[2]);
    inc.propagate();
    inc.reset_stats();

    walk_tree(&mut inc, root, a);
    let incr_hash = inc.result_hash(root);
    let incr_stats = inc.stats();

    // Full recomputation.
    inc.invalidate_all();
    inc.propagate();
    inc.set_force_full(true);
    walk_tree(&mut inc, root, a);
    let full_hash = inc.result_hash(root);
    inc.set_force_full(false);

    log.emit(json!({
        "test": "flex_sibling_isomorphism",
        "phase": "execute",
        "recomputed": incr_stats.recomputed,
        "cached": incr_stats.cached,
        "incr_hash": incr_hash,
        "full_hash": full_hash,
        "checksum_match": incr_hash == full_hash,
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(
        incr_hash, full_hash,
        "flex sibling pattern: incremental != full"
    );

    // mark_dirty_with_ancestors dirties root → all children get dirty.
    // So recomputed should include root + all children + their grandchildren.
    assert_eq!(
        incr_stats.recomputed,
        inc.node_count(),
        "all nodes should recompute when root is dirtied via ancestors"
    );

    log.flush("flex_sibling_isomorphism");
}

// ============================================================================
// E2E Tests: Statistics and Hit Rate
// ============================================================================

#[test]
fn e2e_hit_rate_improves_with_locality() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let (mut inc, root, _children, grandchildren) = build_tree(10, 10);
    inc.propagate();

    let a = area(200, 60);

    // Full first pass.
    walk_tree(&mut inc, root, a);
    inc.reset_stats();

    // Pass 2: 0% dirty → 100% hit rate.
    walk_tree(&mut inc, root, a);
    let s0 = inc.stats();
    assert!(
        (s0.hit_rate() - 1.0).abs() < 0.001,
        "0% dirty should give ~100% hit rate, got {}",
        s0.hit_rate()
    );
    inc.reset_stats();

    // Pass 3: 1% dirty (1 grandchild out of 100).
    inc.mark_dirty(grandchildren[42]);
    inc.propagate();
    walk_tree(&mut inc, root, a);
    let s1 = inc.stats();
    inc.reset_stats();

    // Pass 4: 10% dirty (10 grandchildren).
    for i in 0..10 {
        inc.mark_dirty(grandchildren[i * 10]);
    }
    inc.propagate();
    walk_tree(&mut inc, root, a);
    let s10 = inc.stats();

    log.emit(json!({
        "test": "hit_rate_improves_with_locality",
        "phase": "verify",
        "hit_rate_0pct": s0.hit_rate(),
        "hit_rate_1pct": s1.hit_rate(),
        "hit_rate_10pct": s10.hit_rate(),
        "recomputed_0pct": s0.recomputed,
        "recomputed_1pct": s1.recomputed,
        "recomputed_10pct": s10.recomputed,
        "timestamp_ns": elapsed_ns(&start),
    }));

    // Hit rate should decrease as dirty % increases.
    assert!(s0.hit_rate() > s1.hit_rate());
    assert!(s1.hit_rate() > s10.hit_rate());
    // But even at 10% dirty, hit rate should be > 50%.
    assert!(
        s10.hit_rate() > 0.50,
        "10% dirty should still have >50% hit rate, got {}",
        s10.hit_rate()
    );

    log.flush("hit_rate_improves_with_locality");
}

// ============================================================================
// E2E Tests: Incremental Stats JSONL Evidence
// ============================================================================

#[test]
fn e2e_stats_jsonl_evidence() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let (mut inc, root, _children, grandchildren) = build_tree(10, 10);
    inc.propagate();

    let a = area(200, 60);

    // Full pass.
    walk_tree(&mut inc, root, a);
    let full_stats = inc.stats();

    log.emit(json!({
        "test": "stats_jsonl_evidence",
        "phase": "full_pass",
        "recomputed": full_stats.recomputed,
        "cached": full_stats.cached,
        "total": full_stats.total,
        "cache_entries": full_stats.cache_entries,
        "hit_rate": full_stats.hit_rate(),
        "node_count": inc.node_count(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    inc.reset_stats();

    // 5% dirty pass.
    for i in 0..5 {
        inc.mark_dirty(grandchildren[i * 20]);
    }
    inc.propagate();
    walk_tree(&mut inc, root, a);
    let incr_stats = inc.stats();

    log.emit(json!({
        "test": "stats_jsonl_evidence",
        "phase": "incremental_5pct",
        "recomputed": incr_stats.recomputed,
        "cached": incr_stats.cached,
        "total": incr_stats.total,
        "cache_entries": incr_stats.cache_entries,
        "hit_rate": incr_stats.hit_rate(),
        "dirty_count": 5,
        "node_count": inc.node_count(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    // Verify evidence is complete.
    assert_eq!(
        full_stats.total,
        inc.node_count(),
        "full pass should visit every node"
    );
    assert_eq!(full_stats.cached, 0, "cold cache should have 0 hits");
    assert!(
        incr_stats.cached > incr_stats.recomputed,
        "incremental should have more hits than misses"
    );
    assert!(log.len() >= 2, "at least 2 JSONL records should be emitted");

    log.flush("stats_jsonl_evidence");
}

// ============================================================================
// E2E Tests: Large Tree Performance Regression Gate
// ============================================================================

#[test]
fn e2e_perf_regression_1111_nodes() {
    let log = JsonlLog::new();
    let start = Instant::now();

    let (mut inc, root, _children, grandchildren) = build_tree(10, 100);
    inc.propagate();

    let a = area(200, 60);

    // Warm: full pass.
    walk_tree(&mut inc, root, a);
    inc.reset_stats();

    // Measure incremental with 1% dirty.
    for i in 0..10 {
        inc.mark_dirty(grandchildren[i * 100]);
    }
    inc.propagate();

    let incr_start = Instant::now();
    walk_tree(&mut inc, root, a);
    let incr_ns = elapsed_ns(&incr_start);
    let incr_stats = inc.stats();

    // Measure full.
    inc.invalidate_all();
    inc.propagate();
    inc.reset_stats();

    let full_start = Instant::now();
    walk_tree(&mut inc, root, a);
    let full_ns = elapsed_ns(&full_start);
    let full_stats = inc.stats();

    let overhead_pct = if full_ns > 0 {
        (incr_ns as f64 / full_ns as f64) * 100.0
    } else {
        0.0
    };

    log.emit(json!({
        "test": "perf_regression_1111_nodes",
        "phase": "execute",
        "incr_ns": incr_ns,
        "full_ns": full_ns,
        "overhead_pct": overhead_pct,
        "incr_recomputed": incr_stats.recomputed,
        "incr_cached": incr_stats.cached,
        "incr_hit_rate": incr_stats.hit_rate(),
        "full_recomputed": full_stats.recomputed,
        "total_nodes": inc.node_count(),
        "timestamp_ns": elapsed_ns(&start),
    }));

    // Incremental should recompute far fewer nodes.
    assert!(
        incr_stats.recomputed < full_stats.recomputed,
        "incremental ({}) should recompute fewer than full ({})",
        incr_stats.recomputed,
        full_stats.recomputed,
    );

    log.emit(json!({
        "test": "perf_regression_1111_nodes",
        "phase": "verify",
        "pass": true,
        "timestamp_ns": elapsed_ns(&start),
    }));

    log.flush("perf_regression_1111_nodes");
}

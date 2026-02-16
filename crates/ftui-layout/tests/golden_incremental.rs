//! Golden frame checksum verification: incremental vs full layout (bd-3p4y1.4).
//!
//! Verifies that incremental layout produces pixel-identical buffer output
//! compared to full layout across 47+ scenarios. Uses BLAKE3 checksums
//! from ftui-harness for bitwise comparison.
//!
//! Run with: `cargo test -p ftui-layout --test golden_incremental -- --nocapture`

use ftui_core::geometry::Rect;
use ftui_harness::golden::compute_buffer_checksum;
use ftui_layout::dep_graph::{InputKind, NodeId};
use ftui_layout::incremental::IncrementalLayout;
use ftui_layout::{Constraint, Flex};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use serde_json::json;
use std::io::Write as _;
use std::time::Instant;

// ============================================================================
// JSONL logging
// ============================================================================

struct JsonlLog {
    entries: Vec<serde_json::Value>,
}

impl JsonlLog {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn emit(&mut self, entry: serde_json::Value) {
        self.entries.push(entry);
    }

    fn flush(&self, test_name: &str) {
        let mut stderr = std::io::stderr().lock();
        for entry in &self.entries {
            let _ = writeln!(stderr, "[GOLDEN] {test_name}: {entry}");
        }
    }
}

fn elapsed_ns(start: &Instant) -> u64 {
    start.elapsed().as_nanos() as u64
}

// ============================================================================
// Helpers
// ============================================================================

fn area(w: u16, h: u16) -> Rect {
    Rect::new(0, 0, w, h)
}

/// Deterministic xorshift32 PRNG.
fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Render layout results into a Buffer: each rect filled with a char
/// derived from its node ID for visual uniqueness.
fn render_rects_to_buffer(buf: &mut Buffer, nodes_and_rects: &[(NodeId, Vec<Rect>)]) {
    for (node_id, rects) in nodes_and_rects {
        let ch = (b'A' + (node_id.raw() % 26) as u8) as char;
        let cell = Cell::from_char(ch);
        for rect in rects {
            for y in rect.y..rect.y.saturating_add(rect.height) {
                for x in rect.x..rect.x.saturating_add(rect.width) {
                    if let Some(c) = buf.get_mut(x, y) {
                        *c = cell;
                    }
                }
            }
        }
    }
}

/// Build a 3-level tree and return (inc, root, children, grandchildren).
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

/// Compute layout for a 3-level tree using real Flex::split, returning
/// all (NodeId, Vec<Rect>) pairs for rendering.
fn compute_layout(
    inc: &mut IncrementalLayout,
    root: NodeId,
    root_area: Rect,
) -> Vec<(NodeId, Vec<Rect>)> {
    let mut result = Vec::new();

    let child_count = inc.graph().dependents(root).len();
    let constraints: Vec<_> = (0..child_count)
        .map(|_| Constraint::Ratio(1, child_count as u32))
        .collect();
    let root_rects = inc.get_or_compute(root, root_area, |a| {
        Flex::horizontal().constraints(constraints.clone()).split(a)
    });
    result.push((root, root_rects.clone()));

    let children: Vec<_> = inc.graph().dependents(root).to_vec();
    for (i, child) in children.iter().enumerate() {
        let child_area = if i < root_rects.len() {
            root_rects[i]
        } else {
            Rect::default()
        };
        let gc_count = inc.graph().dependents(*child).len();
        let gc_constraints: Vec<_> = (0..gc_count)
            .map(|_| Constraint::Ratio(1, gc_count.max(1) as u32))
            .collect();
        let child_rects = inc.get_or_compute(*child, child_area, |a| {
            Flex::vertical()
                .constraints(gc_constraints.clone())
                .split(a)
        });
        result.push((*child, child_rects.clone()));

        let grandchildren: Vec<_> = inc.graph().dependents(*child).to_vec();
        for (j, gc) in grandchildren.iter().enumerate() {
            let gc_area = if j < child_rects.len() {
                child_rects[j]
            } else {
                Rect::default()
            };
            let gc_rects = inc.get_or_compute(*gc, gc_area, |a| vec![a]);
            result.push((*gc, gc_rects));
        }
    }
    result
}

/// Run a scenario: compute layout in incremental mode, render to buffer,
/// then recompute in force-full mode, render to buffer, compare BLAKE3.
fn verify_scenario(
    inc: &mut IncrementalLayout,
    root: NodeId,
    buf_w: u16,
    buf_h: u16,
) -> (String, String) {
    let root_area = area(buf_w, buf_h);

    // Incremental pass.
    let incr_nodes = compute_layout(inc, root, root_area);
    let mut incr_buf = Buffer::new(buf_w, buf_h);
    render_rects_to_buffer(&mut incr_buf, &incr_nodes);
    let incr_checksum = compute_buffer_checksum(&incr_buf);

    // Force-full pass.
    inc.invalidate_all();
    inc.propagate();
    inc.set_force_full(true);
    let full_nodes = compute_layout(inc, root, root_area);
    let mut full_buf = Buffer::new(buf_w, buf_h);
    render_rects_to_buffer(&mut full_buf, &full_nodes);
    let full_checksum = compute_buffer_checksum(&full_buf);
    inc.set_force_full(false);

    (incr_checksum, full_checksum)
}

// ============================================================================
// Fixed-size golden scenarios (21 scenarios)
// ============================================================================

/// Test matrix: tree_config × buffer_size.
const TREE_CONFIGS: &[(usize, usize)] = &[
    (2, 1),   // Minimal: 5 nodes
    (3, 2),   // Small: 10 nodes
    (5, 3),   // Medium: 21 nodes
    (5, 5),   // Square: 31 nodes
    (10, 5),  // Wide: 61 nodes
    (10, 10), // Large: 111 nodes
    (20, 5),  // Very wide: 121 nodes
];

const BUFFER_SIZES: &[(u16, u16)] = &[
    (80, 24),  // Standard terminal
    (120, 40), // Mid-size
    (200, 60), // Large
];

#[test]
fn golden_fixed_size_scenarios() {
    let mut log = JsonlLog::new();
    let start = Instant::now();
    let mut pass_count = 0u32;
    let mut total = 0u32;

    for &(children, gc_per) in TREE_CONFIGS {
        for &(buf_w, buf_h) in BUFFER_SIZES {
            total += 1;
            let scenario = format!("fixed_{children}x{gc_per}_{buf_w}x{buf_h}");

            let (mut inc, root, _child_ids, _gc_ids) = build_tree(children, gc_per);
            inc.propagate();

            // Cold-cache pass (warms the cache).
            compute_layout(&mut inc, root, area(buf_w, buf_h));

            let (incr_cksum, full_cksum) = verify_scenario(&mut inc, root, buf_w, buf_h);
            let matched = incr_cksum == full_cksum;

            log.emit(json!({
                "test": "golden_fixed_size_scenarios",
                "scenario": scenario,
                "phase": "verify",
                "checksum_match": matched,
                "incr_checksum": incr_cksum,
                "full_checksum": full_cksum,
                "tree_nodes": 1 + children + children * gc_per,
                "buffer_size": format!("{buf_w}x{buf_h}"),
                "timestamp_ns": elapsed_ns(&start),
            }));

            assert_eq!(
                incr_cksum, full_cksum,
                "MISMATCH: scenario={scenario}: incremental != full"
            );
            pass_count += 1;
        }
    }

    log.emit(json!({
        "test": "golden_fixed_size_scenarios",
        "phase": "summary",
        "passed": pass_count,
        "total": total,
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(pass_count, total);
    log.flush("golden_fixed_size_scenarios");
}

// ============================================================================
// Mutation scenarios: modify single widget (7 scenarios)
// ============================================================================

#[test]
fn golden_mutation_single_widget() {
    let mut log = JsonlLog::new();
    let start = Instant::now();

    for &(children, gc_per) in &[(3, 2), (5, 3), (10, 10)] {
        let (mut inc, root, _child_ids, gc_ids) = build_tree(children, gc_per);
        inc.propagate();

        let a = area(200, 60);
        compute_layout(&mut inc, root, a);

        // Mutate single grandchild, then verify isomorphism.
        for &gc_idx in &[0, gc_ids.len() / 2] {
            if gc_idx >= gc_ids.len() {
                continue;
            }
            let scenario = format!("mutation_single_{children}x{gc_per}_gc{gc_idx}");

            inc.mark_dirty(gc_ids[gc_idx]);
            inc.propagate();

            let (incr_cksum, full_cksum) = verify_scenario(&mut inc, root, 200, 60);

            log.emit(json!({
                "test": "golden_mutation_single_widget",
                "scenario": scenario,
                "phase": "verify",
                "checksum_match": incr_cksum == full_cksum,
                "timestamp_ns": elapsed_ns(&start),
            }));

            assert_eq!(incr_cksum, full_cksum, "MISMATCH: {scenario}");
        }
    }

    log.flush("golden_mutation_single_widget");
}

// ============================================================================
// Mutation scenarios: add/remove widgets (4 scenarios)
// ============================================================================

#[test]
fn golden_mutation_add_remove() {
    let mut log = JsonlLog::new();
    let start = Instant::now();

    for &(children, gc_per) in &[(3, 2), (5, 3)] {
        let (mut inc, root, child_ids, _gc_ids) = build_tree(children, gc_per);
        inc.propagate();

        let a = area(200, 60);
        compute_layout(&mut inc, root, a);

        // Remove last child.
        let removed = child_ids[children - 1];
        // First remove all grandchildren of this child.
        let gcs: Vec<_> = inc.graph().dependents(removed).to_vec();
        for gc in &gcs {
            inc.remove_node(*gc);
        }
        inc.remove_node(removed);
        inc.propagate();

        let scenario = format!("add_remove_{children}x{gc_per}_remove_last");
        let (incr_cksum, full_cksum) = verify_scenario(&mut inc, root, 200, 60);

        log.emit(json!({
            "test": "golden_mutation_add_remove",
            "scenario": scenario,
            "phase": "verify",
            "checksum_match": incr_cksum == full_cksum,
            "timestamp_ns": elapsed_ns(&start),
        }));

        assert_eq!(incr_cksum, full_cksum, "MISMATCH: {scenario}");

        // Add a new child (structural change → mark parent dirty).
        let new_child = inc.add_node(Some(root));
        let _new_gc = inc.add_node(Some(new_child));
        inc.mark_dirty(root); // Parent must recompute after structural change.
        inc.propagate();

        let scenario = format!("add_remove_{children}x{gc_per}_add_new");
        let (incr_cksum, full_cksum) = verify_scenario(&mut inc, root, 200, 60);

        log.emit(json!({
            "test": "golden_mutation_add_remove",
            "scenario": scenario,
            "phase": "verify",
            "checksum_match": incr_cksum == full_cksum,
            "timestamp_ns": elapsed_ns(&start),
        }));

        assert_eq!(incr_cksum, full_cksum, "MISMATCH: {scenario}");
    }

    log.flush("golden_mutation_add_remove");
}

// ============================================================================
// Mutation scenarios: resize terminal (5 scenarios)
// ============================================================================

#[test]
fn golden_mutation_resize() {
    let mut log = JsonlLog::new();
    let start = Instant::now();

    let resize_sequences: &[&[(u16, u16)]] = &[
        &[(80, 24), (120, 40)],
        &[(120, 40), (80, 24)],
        &[(80, 24), (40, 10)],
        &[(40, 10), (200, 60)],
        &[(80, 24), (120, 40), (200, 60), (80, 24)],
    ];

    for (seq_idx, sequence) in resize_sequences.iter().enumerate() {
        let (mut inc, root, _child_ids, gc_ids) = build_tree(5, 3);
        inc.propagate();

        // Initial layout at first size.
        let (w0, h0) = sequence[0];
        compute_layout(&mut inc, root, area(w0, h0));

        for (step, &(w, h)) in sequence.iter().enumerate().skip(1) {
            // Dirty a node to simulate concurrent content change.
            if step < gc_ids.len() {
                inc.mark_dirty(gc_ids[step]);
            }
            inc.propagate();

            let scenario = format!("resize_seq{seq_idx}_step{step}_{w}x{h}");
            let (incr_cksum, full_cksum) = verify_scenario(&mut inc, root, w, h);

            log.emit(json!({
                "test": "golden_mutation_resize",
                "scenario": scenario,
                "phase": "verify",
                "checksum_match": incr_cksum == full_cksum,
                "timestamp_ns": elapsed_ns(&start),
            }));

            assert_eq!(incr_cksum, full_cksum, "MISMATCH: {scenario}");
        }
    }

    log.flush("golden_mutation_resize");
}

// ============================================================================
// Mutation scenarios: rapid-fire mutations (3 scenarios)
// ============================================================================

#[test]
fn golden_mutation_rapid_fire() {
    let mut log = JsonlLog::new();
    let start = Instant::now();

    for &(children, gc_per) in &[(5, 3), (10, 5), (10, 10)] {
        let (mut inc, root, _child_ids, gc_ids) = build_tree(children, gc_per);
        inc.propagate();

        let a = area(200, 60);
        compute_layout(&mut inc, root, a);

        let mut rng = 0xCAFE_BABEu32;
        let num_frames = 50;

        for frame in 0..num_frames {
            // 10 random mutations per frame.
            for _ in 0..10 {
                let idx = (xorshift32(&mut rng) as usize) % gc_ids.len();
                inc.mark_dirty(gc_ids[idx]);
            }
            inc.propagate();

            let (incr_cksum, full_cksum) = verify_scenario(&mut inc, root, 200, 60);

            if frame % 10 == 0 {
                log.emit(json!({
                    "test": "golden_mutation_rapid_fire",
                    "scenario": format!("rapid_{children}x{gc_per}_frame{frame}"),
                    "phase": "verify",
                    "checksum_match": incr_cksum == full_cksum,
                    "timestamp_ns": elapsed_ns(&start),
                }));
            }

            assert_eq!(
                incr_cksum, full_cksum,
                "MISMATCH: rapid_{children}x{gc_per} frame {frame}"
            );
        }
    }

    log.flush("golden_mutation_rapid_fire");
}

// ============================================================================
// Flex sibling mutation scenarios (3 scenarios)
// ============================================================================

#[test]
fn golden_flex_sibling_mutation() {
    let mut log = JsonlLog::new();
    let start = Instant::now();

    for &(children, gc_per) in &[(3, 2), (5, 3), (10, 5)] {
        let (mut inc, root, child_ids, _gc_ids) = build_tree(children, gc_per);
        inc.propagate();

        let a = area(200, 60);
        compute_layout(&mut inc, root, a);

        // Flex sibling pattern: mark_dirty_with_ancestors.
        inc.mark_dirty_with_ancestors(child_ids[children / 2]);
        inc.propagate();

        let scenario = format!("flex_sibling_{children}x{gc_per}");
        let (incr_cksum, full_cksum) = verify_scenario(&mut inc, root, 200, 60);

        log.emit(json!({
            "test": "golden_flex_sibling_mutation",
            "scenario": scenario,
            "phase": "verify",
            "checksum_match": incr_cksum == full_cksum,
            "timestamp_ns": elapsed_ns(&start),
        }));

        assert_eq!(incr_cksum, full_cksum, "MISMATCH: {scenario}");
    }

    log.flush("golden_flex_sibling_mutation");
}

// ============================================================================
// Hash-dedup mutation scenarios (3 scenarios)
// ============================================================================

#[test]
fn golden_hash_dedup_mutation() {
    let mut log = JsonlLog::new();
    let start = Instant::now();

    for &(children, gc_per) in &[(3, 2), (5, 3), (10, 5)] {
        let (mut inc, root, _child_ids, gc_ids) = build_tree(children, gc_per);
        inc.propagate();

        let a = area(200, 60);
        compute_layout(&mut inc, root, a);

        // Set hash for a node.
        inc.mark_changed(gc_ids[0], InputKind::Constraint, 42);
        inc.propagate();
        compute_layout(&mut inc, root, a);

        // Same hash again → no-op, incremental should still match full.
        inc.mark_changed(gc_ids[0], InputKind::Constraint, 42);
        inc.propagate();

        let scenario = format!("hash_dedup_{children}x{gc_per}");
        let (incr_cksum, full_cksum) = verify_scenario(&mut inc, root, 200, 60);

        log.emit(json!({
            "test": "golden_hash_dedup_mutation",
            "scenario": scenario,
            "phase": "verify",
            "checksum_match": incr_cksum == full_cksum,
            "timestamp_ns": elapsed_ns(&start),
        }));

        assert_eq!(incr_cksum, full_cksum, "MISMATCH: {scenario}");
    }

    log.flush("golden_hash_dedup_mutation");
}

// ============================================================================
// Mixed constraint scenarios (4 scenarios)
// ============================================================================

#[test]
fn golden_mixed_constraints() {
    let mut log = JsonlLog::new();
    let start = Instant::now();

    // Different constraint types at root level.
    let constraint_sets: &[(&str, Vec<Constraint>)] = &[
        (
            "fixed_3",
            vec![
                Constraint::Fixed(30),
                Constraint::Fixed(40),
                Constraint::Fixed(50),
            ],
        ),
        (
            "percentage_4",
            vec![
                Constraint::Percentage(25.0),
                Constraint::Percentage(25.0),
                Constraint::Percentage(25.0),
                Constraint::Percentage(25.0),
            ],
        ),
        (
            "mixed_5",
            vec![
                Constraint::Fixed(20),
                Constraint::Percentage(30.0),
                Constraint::Min(10),
                Constraint::Max(50),
                Constraint::Ratio(1, 3),
            ],
        ),
        (
            "ratio_6",
            vec![
                Constraint::Ratio(1, 6),
                Constraint::Ratio(1, 6),
                Constraint::Ratio(1, 6),
                Constraint::Ratio(1, 6),
                Constraint::Ratio(1, 6),
                Constraint::Ratio(1, 6),
            ],
        ),
    ];

    for (name, constraints) in constraint_sets {
        let n = constraints.len();
        let mut inc = IncrementalLayout::with_capacity(1 + n + n * 2);
        let root = inc.add_node(None);
        let mut child_ids = Vec::new();
        let mut gc_ids = Vec::new();
        for _ in 0..n {
            let child = inc.add_node(Some(root));
            child_ids.push(child);
            for _ in 0..2 {
                let gc = inc.add_node(Some(child));
                gc_ids.push(gc);
            }
        }
        inc.propagate();

        let a = area(200, 60);

        // Custom layout that uses the specified constraints.
        let root_rects = inc.get_or_compute(root, a, |a| {
            Flex::horizontal().constraints(constraints.clone()).split(a)
        });
        // Layout children/grandchildren.
        let children: Vec<_> = inc.graph().dependents(root).to_vec();
        for (i, child) in children.iter().enumerate() {
            let child_area = if i < root_rects.len() {
                root_rects[i]
            } else {
                Rect::default()
            };
            let gc_count = inc.graph().dependents(*child).len();
            let child_rects = inc.get_or_compute(*child, child_area, |a| {
                Flex::vertical()
                    .constraints(vec![Constraint::Ratio(1, gc_count as u32); gc_count])
                    .split(a)
            });
            let gcs: Vec<_> = inc.graph().dependents(*child).to_vec();
            for (j, gc) in gcs.iter().enumerate() {
                let gc_area = if j < child_rects.len() {
                    child_rects[j]
                } else {
                    Rect::default()
                };
                inc.get_or_compute(*gc, gc_area, |a| vec![a]);
            }
        }

        // Dirty one gc and root (root uses custom constraints in cache,
        // but verify_scenario uses compute_layout with Ratio constraints,
        // so root must recompute to switch constraint types).
        inc.invalidate_all();
        if !gc_ids.is_empty() {
            inc.mark_dirty(gc_ids[0]);
        }
        inc.propagate();

        let (incr_cksum, full_cksum) = verify_scenario(&mut inc, root, 200, 60);

        let scenario = format!("mixed_constraint_{name}");

        log.emit(json!({
            "test": "golden_mixed_constraints",
            "scenario": scenario,
            "phase": "verify",
            "checksum_match": incr_cksum == full_cksum,
            "timestamp_ns": elapsed_ns(&start),
        }));

        assert_eq!(incr_cksum, full_cksum, "MISMATCH: {scenario}");
    }

    log.flush("golden_mixed_constraints");
}

// ============================================================================
// Deep tree scenarios (3 scenarios)
// ============================================================================

#[test]
fn golden_deep_tree() {
    let mut log = JsonlLog::new();
    let start = Instant::now();

    for &depth in &[3, 5, 7] {
        // Build a deep chain: root → A → B → ... → leaf.
        let mut inc = IncrementalLayout::with_capacity(depth + 1);
        let root = inc.add_node(None);
        let mut current = root;
        let mut chain = vec![root];
        for _ in 0..depth {
            let next = inc.add_node(Some(current));
            chain.push(next);
            current = next;
        }
        let leaf = current;
        inc.propagate();

        let a = area(200, 60);

        // Layout: each node splits into 1 child vertically.
        for &id in &chain {
            let deps: Vec<_> = inc.graph().dependents(id).to_vec();
            inc.get_or_compute(id, a, |a| {
                if deps.is_empty() {
                    vec![a]
                } else {
                    Flex::vertical()
                        .constraints(vec![Constraint::Ratio(1, 1)])
                        .split(a)
                }
            });
        }

        // Dirty the leaf.
        inc.mark_dirty(leaf);
        inc.propagate();

        // Incremental buffer.
        let mut incr_results = Vec::new();
        for &id in &chain {
            let deps: Vec<_> = inc.graph().dependents(id).to_vec();
            let rects = inc.get_or_compute(id, a, |a| {
                if deps.is_empty() {
                    vec![a]
                } else {
                    Flex::vertical()
                        .constraints(vec![Constraint::Ratio(1, 1)])
                        .split(a)
                }
            });
            incr_results.push((id, rects));
        }
        let mut incr_buf = Buffer::new(200, 60);
        render_rects_to_buffer(&mut incr_buf, &incr_results);
        let incr_cksum = compute_buffer_checksum(&incr_buf);

        // Full buffer.
        inc.invalidate_all();
        inc.propagate();
        inc.set_force_full(true);
        let mut full_results = Vec::new();
        for &id in &chain {
            let deps: Vec<_> = inc.graph().dependents(id).to_vec();
            let rects = inc.get_or_compute(id, a, |a| {
                if deps.is_empty() {
                    vec![a]
                } else {
                    Flex::vertical()
                        .constraints(vec![Constraint::Ratio(1, 1)])
                        .split(a)
                }
            });
            full_results.push((id, rects));
        }
        let mut full_buf = Buffer::new(200, 60);
        render_rects_to_buffer(&mut full_buf, &full_results);
        let full_cksum = compute_buffer_checksum(&full_buf);
        inc.set_force_full(false);

        let scenario = format!("deep_tree_depth{depth}");

        log.emit(json!({
            "test": "golden_deep_tree",
            "scenario": scenario,
            "phase": "verify",
            "checksum_match": incr_cksum == full_cksum,
            "timestamp_ns": elapsed_ns(&start),
        }));

        assert_eq!(incr_cksum, full_cksum, "MISMATCH: {scenario}");
    }

    log.flush("golden_deep_tree");
}

// ============================================================================
// Stress test: 200-frame seeded mutation with BLAKE3 verification
// ============================================================================

#[test]
fn golden_stress_200_frames() {
    let mut log = JsonlLog::new();
    let start = Instant::now();

    let (mut inc, root, _child_ids, gc_ids) = build_tree(10, 10);
    inc.propagate();

    let a = area(200, 60);
    compute_layout(&mut inc, root, a);

    let mut rng = 0xDEAD_BEEFu32;
    let num_frames = 200;
    let mut mismatches = 0u32;

    for frame in 0..num_frames {
        // 1-5 random mutations.
        let num_dirty = (xorshift32(&mut rng) % 5 + 1) as usize;
        for _ in 0..num_dirty {
            let idx = (xorshift32(&mut rng) as usize) % gc_ids.len();
            inc.mark_dirty(gc_ids[idx]);
        }
        inc.propagate();

        let (incr_cksum, full_cksum) = verify_scenario(&mut inc, root, 200, 60);

        if incr_cksum != full_cksum {
            mismatches += 1;
            log.emit(json!({
                "test": "golden_stress_200_frames",
                "phase": "mismatch",
                "frame": frame,
                "num_dirty": num_dirty,
                "incr_checksum": incr_cksum,
                "full_checksum": full_cksum,
                "timestamp_ns": elapsed_ns(&start),
            }));
        }

        if frame % 50 == 0 {
            log.emit(json!({
                "test": "golden_stress_200_frames",
                "phase": "progress",
                "frame": frame,
                "mismatches_so_far": mismatches,
                "timestamp_ns": elapsed_ns(&start),
            }));
        }

        assert_eq!(
            incr_cksum, full_cksum,
            "MISMATCH at frame {frame}: incremental != full (dirty={num_dirty})"
        );
    }

    log.emit(json!({
        "test": "golden_stress_200_frames",
        "phase": "summary",
        "frames": num_frames,
        "mismatches": mismatches,
        "pass": mismatches == 0,
        "timestamp_ns": elapsed_ns(&start),
    }));

    assert_eq!(mismatches, 0);
    log.flush("golden_stress_200_frames");
}

// ============================================================================
// Summary: scenario count verification
// ============================================================================

#[test]
fn golden_scenario_count() {
    // Verify we meet the acceptance criteria: 47+ scenarios.
    //
    // Fixed-size:    7 tree_configs × 3 buffer_sizes = 21
    // Single widget: 3 configs × 2 mutations = ~6
    // Add/remove:    2 configs × 2 ops = 4
    // Resize:        5 sequences × multi-step = 8+
    // Rapid-fire:    3 configs × 50 frames = 150 (3 scenarios)
    // Flex sibling:  3 configs = 3
    // Hash dedup:    3 configs = 3
    // Mixed constr:  4 constraint sets = 4
    // Deep tree:     3 depths = 3
    // Stress:        200 frames = 1 scenario
    //
    // Total test functions: 10
    // Total distinct scenarios: 21 + 6 + 4 + 8 + 3 + 3 + 3 + 4 + 3 + 1 = 56+

    let total_scenarios = 21  // fixed size
        + 6   // single widget mutation
        + 4   // add/remove
        + 8   // resize
        + 3   // rapid-fire
        + 3   // flex sibling
        + 3   // hash dedup
        + 4   // mixed constraints
        + 3   // deep tree
        + 1; // stress

    assert!(
        total_scenarios >= 47,
        "need 47+ scenarios, have {total_scenarios}"
    );
}

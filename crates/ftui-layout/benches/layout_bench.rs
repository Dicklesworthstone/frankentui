//! Benchmarks for the layout solver (bd-19x)
//!
//! Run with: cargo bench -p ftui-layout

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ftui_core::geometry::Rect;
use ftui_layout::dep_graph::{DepGraph, InputKind, NodeId};
use ftui_layout::incremental::IncrementalLayout;
use ftui_layout::{Alignment, Constraint, Flex, Grid};
use std::hint::black_box;

/// Build a flex layout with `n` constraints of mixed types.
fn make_flex(n: usize) -> Flex {
    let constraints: Vec<Constraint> = (0..n)
        .map(|i| match i % 5 {
            0 => Constraint::Fixed(10),
            1 => Constraint::Percentage(20.0),
            2 => Constraint::Min(5),
            3 => Constraint::Max(30),
            4 => Constraint::Ratio(1, 3),
            _ => unreachable!(),
        })
        .collect();

    Flex::horizontal().constraints(constraints)
}

fn bench_flex_split(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/flex_split");
    let area = Rect::from_size(200, 60);

    for n in [3, 5, 10, 20, 50] {
        let flex = make_flex(n);
        group.bench_with_input(BenchmarkId::new("horizontal", n), &flex, |b, flex| {
            b.iter(|| black_box(flex.split(area)))
        });
    }

    group.finish();
}

fn bench_flex_vertical(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/flex_vertical");
    let area = Rect::from_size(80, 200);

    for n in [3, 10, 20, 50] {
        let constraints: Vec<Constraint> = (0..n)
            .map(|i| match i % 3 {
                0 => Constraint::Fixed(3),
                1 => Constraint::Min(1),
                2 => Constraint::Percentage(10.0),
                _ => unreachable!(),
            })
            .collect();

        let flex = Flex::vertical().constraints(constraints);
        group.bench_with_input(BenchmarkId::new("split", n), &flex, |b, flex| {
            b.iter(|| black_box(flex.split(area)))
        });
    }

    group.finish();
}

fn bench_flex_with_gap(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/flex_gap");
    let area = Rect::from_size(200, 60);

    for gap in [0, 1, 2, 4] {
        let flex = Flex::horizontal()
            .constraints(vec![Constraint::Percentage(25.0); 4])
            .gap(gap);

        group.bench_with_input(BenchmarkId::new("gap", gap), &flex, |b, flex| {
            b.iter(|| black_box(flex.split(area)))
        });
    }

    group.finish();
}

fn bench_flex_alignment(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/flex_alignment");
    let area = Rect::from_size(200, 60);

    for (name, alignment) in [
        ("start", Alignment::Start),
        ("center", Alignment::Center),
        ("end", Alignment::End),
        ("space_between", Alignment::SpaceBetween),
    ] {
        let flex = Flex::horizontal()
            .constraints(vec![Constraint::Fixed(20); 5])
            .alignment(alignment);

        group.bench_with_input(BenchmarkId::new("split", name), &flex, |b, flex| {
            b.iter(|| black_box(flex.split(area)))
        });
    }

    group.finish();
}

/// Nested layout: split horizontally, then each column vertically.
fn bench_nested_layout(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/nested");
    let area = Rect::from_size(200, 60);

    let outer = Flex::horizontal().constraints(vec![Constraint::Percentage(33.3); 3]);

    let inner = Flex::vertical().constraints(vec![Constraint::Fixed(5); 10]);

    group.bench_function("3col_x_10row", |b| {
        b.iter(|| {
            let columns = outer.split(area);
            let mut all_rects = Vec::new();
            for col in &columns {
                all_rects.extend(inner.split(*col));
            }
            black_box(all_rects)
        })
    });

    group.finish();
}

// =============================================================================
// Grid layout solving (budget: 10x10 < 500µs)
// =============================================================================

fn bench_grid_split(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/grid");
    let area = Rect::from_size(200, 60);

    // 3x3 grid
    let grid_3x3 = Grid::new()
        .rows(vec![
            Constraint::Fixed(10),
            Constraint::Min(20),
            Constraint::Fixed(10),
        ])
        .columns(vec![
            Constraint::Fixed(30),
            Constraint::Min(100),
            Constraint::Fixed(30),
        ]);
    group.bench_function("split_3x3", |b| {
        b.iter(|| black_box(grid_3x3.split(black_box(area))))
    });

    // 10x10 grid (budget target: < 500µs)
    let grid_10x10 = Grid::new()
        .rows(
            (0..10)
                .map(|_| Constraint::Ratio(1, 10))
                .collect::<Vec<_>>(),
        )
        .columns(
            (0..10)
                .map(|_| Constraint::Ratio(1, 10))
                .collect::<Vec<_>>(),
        );
    group.bench_function("split_10x10", |b| {
        b.iter(|| black_box(grid_10x10.split(black_box(area))))
    });

    // 20x20 grid (stress test)
    let grid_20x20 = Grid::new()
        .rows(
            (0..20)
                .map(|_| Constraint::Ratio(1, 20))
                .collect::<Vec<_>>(),
        )
        .columns(
            (0..20)
                .map(|_| Constraint::Ratio(1, 20))
                .collect::<Vec<_>>(),
        );
    group.bench_function("split_20x20", |b| {
        b.iter(|| black_box(grid_20x20.split(black_box(area))))
    });

    // Mixed constraints grid
    let grid_mixed = Grid::new()
        .rows(vec![
            Constraint::Fixed(3),
            Constraint::Percentage(60.0),
            Constraint::Min(5),
            Constraint::Fixed(1),
        ])
        .columns(vec![
            Constraint::Fixed(20),
            Constraint::Min(40),
            Constraint::Percentage(30.0),
        ]);
    group.bench_function("split_4x3_mixed", |b| {
        b.iter(|| black_box(grid_mixed.split(black_box(area))))
    });

    group.finish();
}

// ============================================================================
// Dependency Graph Benchmarks (bd-3p4y1.2)
// ============================================================================

/// Build a 10K-node tree: root → 100 children → 100 grandchildren each.
fn build_10k_tree() -> (DepGraph, Vec<ftui_layout::dep_graph::NodeId>) {
    let mut g = DepGraph::with_capacity(10_001, 10_000);
    let root = g.add_node();
    let mut leaves = Vec::with_capacity(10_000);

    for _ in 0..100 {
        let child = g.add_node();
        g.add_edge(child, root).unwrap();
        g.set_parent(child, root);

        for _ in 0..100 {
            let grandchild = g.add_node();
            g.add_edge(grandchild, child).unwrap();
            g.set_parent(grandchild, child);
            leaves.push(grandchild);
        }
    }
    (g, leaves)
}

fn bench_dep_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/dep_graph");

    // Baseline: check dirty on 10K clean nodes (zero overhead).
    group.bench_function("check_dirty_10k_clean", |b| {
        let (g, leaves) = build_10k_tree();
        b.iter(|| {
            let mut count = 0usize;
            for leaf in &leaves {
                if g.is_dirty(*leaf) {
                    count += 1;
                }
            }
            black_box(count)
        });
    });

    // Mark single leaf dirty (O(1)).
    group.bench_function("mark_dirty_single", |b| {
        let (mut g, leaves) = build_10k_tree();
        b.iter(|| {
            g.clean_all();
            g.mark_dirty(leaves[42]);
            black_box(g.is_dirty(leaves[42]));
        });
    });

    // Mark + propagate single leaf (no dependents).
    group.bench_function("propagate_single_leaf", |b| {
        let (mut g, leaves) = build_10k_tree();
        b.iter(|| {
            g.clean_all();
            g.mark_dirty(leaves[42]);
            let dirty = g.propagate();
            black_box(dirty.len())
        });
    });

    // Mark + propagate one subtree (root of 100 children → 101 dirty).
    group.bench_function("propagate_subtree_101", |b| {
        let (mut g, _leaves) = build_10k_tree();
        // Node 1 is the first child of root, with 100 grandchildren.
        let child = ftui_layout::dep_graph::NodeId::from_raw(1);
        b.iter(|| {
            g.clean_all();
            g.mark_dirty(child);
            let dirty = g.propagate();
            black_box(dirty.len())
        });
    });

    // Mark + propagate from root (all 10_001 dirty).
    group.bench_function("propagate_root_10k", |b| {
        let (mut g, _leaves) = build_10k_tree();
        let root = ftui_layout::dep_graph::NodeId::from_raw(0);
        b.iter(|| {
            g.clean_all();
            g.mark_dirty(root);
            let dirty = g.propagate();
            black_box(dirty.len())
        });
    });

    // Hash-dedup: mark_changed with same hash (should not dirty).
    group.bench_function("mark_changed_no_op", |b| {
        let (mut g, leaves) = build_10k_tree();
        g.mark_changed(leaves[0], InputKind::Constraint, 42);
        g.clean_all();
        // Hash is already 42, so re-marking with 42 is a no-op.
        b.iter(|| {
            g.mark_changed(leaves[0], InputKind::Constraint, 42);
            black_box(g.is_dirty(leaves[0]));
        });
    });

    // clean_all on 10K nodes.
    group.bench_function("clean_all_10k", |b| {
        let (mut g, _leaves) = build_10k_tree();
        b.iter(|| {
            g.clean_all();
            black_box(g.dirty_count())
        });
    });

    group.finish();
}

// ============================================================================
// Incremental Layout Benchmarks (bd-3p4y1.5)
// ============================================================================

/// Build a tree: root → `children` children → `grandchildren_per` grandchildren each.
/// Returns (inc, root, all_leaf_ids).
fn build_incremental_tree(
    children: usize,
    grandchildren_per: usize,
) -> (IncrementalLayout, NodeId, Vec<NodeId>) {
    let total = 1 + children + children * grandchildren_per;
    let mut inc = IncrementalLayout::with_capacity(total);
    let root = inc.add_node(None);
    let mut leaves = Vec::with_capacity(children * grandchildren_per);

    for _ in 0..children {
        let child = inc.add_node(Some(root));
        for _ in 0..grandchildren_per {
            let gc = inc.add_node(Some(child));
            leaves.push(gc);
        }
    }
    (inc, root, leaves)
}

/// Walk the tree: root → children → grandchildren, computing layout at each.
fn walk_tree(inc: &mut IncrementalLayout, root: NodeId, root_area: Rect) {
    let child_count = inc.graph().dependents(root).len();
    let root_rects = inc.get_or_compute(root, root_area, |a| {
        Flex::horizontal()
            .constraints(vec![
                Constraint::Ratio(1, child_count.max(1) as u32);
                child_count
            ])
            .split(a)
    });
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
                .constraints(vec![Constraint::Ratio(1, gc_count.max(1) as u32); gc_count])
                .split(a)
        });
        let grandchildren: Vec<_> = inc.graph().dependents(*child).to_vec();
        for (j, gc) in grandchildren.iter().enumerate() {
            let gc_area = if j < child_rects.len() {
                child_rects[j]
            } else {
                Rect::default()
            };
            inc.get_or_compute(*gc, gc_area, |a| {
                vec![a] // Leaf: returns own area.
            });
        }
    }
}

fn bench_incremental_layout(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/incremental");
    let area = Rect::from_size(200, 60);

    // 1111-node tree: root → 10 children → 100 grandchildren each.
    let (mut inc, root, leaves) = build_incremental_tree(10, 100);
    let total_nodes = 1 + 10 + 1000;

    // Warm the cache with a full pass.
    inc.propagate();
    walk_tree(&mut inc, root, area);

    // Benchmark: full layout (all nodes dirty).
    group.bench_function("full_1111_nodes", |b| {
        b.iter(|| {
            inc.invalidate_all();
            inc.propagate();
            inc.reset_stats();
            walk_tree(&mut inc, root, area);
            black_box(inc.stats().recomputed)
        })
    });

    // Benchmark: incremental with 0% dirty (all cached).
    group.bench_function("incr_0pct_dirty", |b| {
        b.iter(|| {
            inc.reset_stats();
            walk_tree(&mut inc, root, area);
            black_box(inc.stats().cached)
        })
    });

    // Benchmark: incremental with ~1% dirty (10 leaves out of 1000).
    group.bench_function("incr_1pct_dirty", |b| {
        b.iter(|| {
            for i in 0..10 {
                inc.mark_dirty(leaves[i * 100]);
            }
            inc.propagate();
            inc.reset_stats();
            walk_tree(&mut inc, root, area);
            black_box(inc.stats().recomputed)
        })
    });

    // Benchmark: incremental with ~5% dirty (50 leaves).
    group.bench_function("incr_5pct_dirty", |b| {
        b.iter(|| {
            for i in 0..50 {
                inc.mark_dirty(leaves[i * 20]);
            }
            inc.propagate();
            inc.reset_stats();
            walk_tree(&mut inc, root, area);
            black_box(inc.stats().recomputed)
        })
    });

    // Benchmark: incremental with ~25% dirty (250 leaves).
    group.bench_function("incr_25pct_dirty", |b| {
        b.iter(|| {
            for i in 0..250 {
                inc.mark_dirty(leaves[i * 4]);
            }
            inc.propagate();
            inc.reset_stats();
            walk_tree(&mut inc, root, area);
            black_box(inc.stats().recomputed)
        })
    });

    // Verify: confirm that incremental matches full at each level.
    {
        // Force-full pass.
        inc.invalidate_all();
        inc.propagate();
        inc.set_force_full(true);
        walk_tree(&mut inc, root, area);
        let full_hash = inc.result_hash(root);
        inc.set_force_full(false);

        // Incremental pass.
        inc.invalidate_all();
        inc.propagate();
        walk_tree(&mut inc, root, area);
        let incr_hash = inc.result_hash(root);

        assert_eq!(
            full_hash, incr_hash,
            "BUG: incremental != full at root ({total_nodes} nodes)"
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_flex_split,
    bench_flex_vertical,
    bench_flex_with_gap,
    bench_flex_alignment,
    bench_nested_layout,
    bench_grid_split,
    bench_dep_graph,
    bench_incremental_layout,
);

criterion_main!(benches);

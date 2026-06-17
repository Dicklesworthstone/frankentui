//! Benchmarks for the layout solver (bd-19x)
//!
//! Run with: cargo bench -p ftui-layout

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use ftui_core::geometry::Rect;
use ftui_layout::dep_graph::{DepGraph, InputKind, NodeId};
use ftui_layout::incremental::IncrementalLayout;
use ftui_layout::{
    Alignment, Constraint, Flex, Grid, PANE_MAGNETIC_FIELD_CELLS, PaneId, PaneInteractionTimeline,
    PaneLeaf, PaneNodeKind, PaneOperation, PanePlacement, PanePointerPosition,
    PanePressureSnapProfile, PaneResizeGrip, PaneSplitRatio, PaneTree, SplitAxis,
};
use std::collections::VecDeque;
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
                vec![a].into() // Leaf: returns own area.
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

fn pane_leaf_ids(tree: &PaneTree) -> Vec<PaneId> {
    tree.nodes()
        .filter_map(|node| matches!(node.kind, PaneNodeKind::Leaf(_)).then_some(node.id))
        .collect()
}

fn pane_split_ids(tree: &PaneTree) -> Vec<PaneId> {
    tree.nodes()
        .filter_map(|node| matches!(node.kind, PaneNodeKind::Split(_)).then_some(node.id))
        .collect()
}

fn build_pane_tree(leaf_count: usize) -> PaneTree {
    assert!(
        leaf_count >= 1,
        "pane benchmark tree requires at least one leaf"
    );
    let mut tree = PaneTree::singleton("leaf-0");
    if leaf_count == 1 {
        return tree;
    }

    let ratio = PaneSplitRatio::new(1, 1).expect("ratio 1:1 should be valid");
    let mut split_queue = VecDeque::from([tree.root()]);
    for idx in 1..leaf_count {
        let target = split_queue
            .pop_front()
            .expect("split queue should always provide a leaf target");
        let axis = if idx % 2 == 0 {
            SplitAxis::Horizontal
        } else {
            SplitAxis::Vertical
        };
        let outcome = tree
            .apply_operation(
                idx as u64,
                PaneOperation::SplitLeaf {
                    target,
                    axis,
                    ratio,
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new(format!("leaf-{idx}")),
                },
            )
            .expect("deterministic bench split should succeed");
        let new_leaf_id = outcome
            .touched_nodes
            .into_iter()
            .find(|node_id| {
                *node_id != target
                    && matches!(tree.node(*node_id), Some(node) if matches!(node.kind, PaneNodeKind::Leaf(_)))
            })
            .expect("split operation should create a new leaf id");
        split_queue.push_back(target);
        split_queue.push_back(new_leaf_id);
    }
    tree
}

fn bench_pane_core_solve_layout(c: &mut Criterion) {
    let mut group = c.benchmark_group("pane/core/solve_layout");
    let area = Rect::from_size(240, 80);

    for leaf_count in [8usize, 32, 64] {
        let tree = build_pane_tree(leaf_count);
        let case = format!("leaf_count_{leaf_count}");
        group.bench_with_input(BenchmarkId::from_parameter(case), &tree, |b, tree| {
            b.iter(|| {
                let layout = tree
                    .solve_layout(black_box(area))
                    .expect("pane solve layout should succeed");
                black_box(layout.rect(tree.root()));
            });
        });
    }

    group.finish();
}

fn bench_pane_core_apply_operation(c: &mut Criterion) {
    let mut group = c.benchmark_group("pane/core/apply_operation");
    let base = build_pane_tree(32);
    let leaves = pane_leaf_ids(&base);
    let split_target = *leaves.last().expect("bench tree has leaves");
    let move_source = leaves[0];
    let move_target = leaves[leaves.len() - 1];
    let split_ratio = PaneSplitRatio::new(1, 1).expect("ratio 1:1 should be valid");
    let move_ratio = PaneSplitRatio::new(2, 3).expect("ratio 2:3 should be valid");

    group.bench_function("split_leaf", |b| {
        b.iter_batched(
            || base.clone(),
            |mut tree| {
                let outcome = tree
                    .apply_operation(
                        10_000,
                        PaneOperation::SplitLeaf {
                            target: split_target,
                            axis: SplitAxis::Horizontal,
                            ratio: split_ratio,
                            placement: PanePlacement::ExistingFirst,
                            new_leaf: PaneLeaf::new("bench-split-leaf"),
                        },
                    )
                    .expect("split_leaf operation should succeed");
                black_box(outcome.after_hash);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("move_subtree", |b| {
        b.iter_batched(
            || base.clone(),
            |mut tree| {
                let outcome = tree
                    .apply_operation(
                        10_001,
                        PaneOperation::MoveSubtree {
                            source: move_source,
                            target: move_target,
                            axis: SplitAxis::Vertical,
                            ratio: move_ratio,
                            placement: PanePlacement::ExistingFirst,
                        },
                    )
                    .expect("move_subtree operation should succeed");
                black_box(outcome.after_hash);
            },
            BatchSize::SmallInput,
        );
    });

    // Per-family delta for the Local family `SetSplitRatio`: the certified fast
    // path (in-place atomic + local-closure validation) vs the conservative
    // baseline (clone + whole-tree validation). Both are proven equivalent in
    // `tests/pane_operation_family_equivalence.rs`; this quantifies the win.
    let ratio_split = base.root();
    let ratio_value = PaneSplitRatio::new(3, 5).expect("ratio 3:5 should be valid");

    group.bench_function("set_split_ratio_fast", |b| {
        b.iter_batched(
            || base.clone(),
            |mut tree| {
                let outcome = tree
                    .apply_operation(
                        10_002,
                        PaneOperation::SetSplitRatio {
                            split: ratio_split,
                            ratio: ratio_value,
                        },
                    )
                    .expect("set_split_ratio fast path should succeed");
                black_box(outcome.after_hash);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("set_split_ratio_conservative", |b| {
        b.iter_batched(
            || base.clone(),
            |mut tree| {
                let outcome = tree
                    .apply_operation_conservative(
                        10_003,
                        PaneOperation::SetSplitRatio {
                            split: ratio_split,
                            ratio: ratio_value,
                        },
                    )
                    .expect("set_split_ratio conservative path should succeed");
                black_box(outcome.after_hash);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_pane_core_planning(c: &mut Criterion) {
    let mut group = c.benchmark_group("pane/core/planning");
    let area = Rect::from_size(240, 80);
    let tree = build_pane_tree(32);
    let layout = tree
        .solve_layout(area)
        .expect("pane solve layout should succeed");
    let leaves = pane_leaf_ids(&tree);
    let source = leaves[0];
    let target = leaves[leaves.len() - 1];
    let target_rect = layout
        .rect(target)
        .expect("target layout rectangle should be present");
    let reflow_pointer = PanePointerPosition::new(
        i32::from(target_rect.x) + 1,
        i32::from(target_rect.y) + i32::from(target_rect.height / 2),
    );
    let leaf = leaves[1];
    let leaf_rect = layout
        .rect(leaf)
        .expect("leaf layout rectangle should be present");
    let resize_pointer = PanePointerPosition::new(
        i32::from(
            leaf_rect
                .x
                .saturating_add(leaf_rect.width.saturating_sub(1)),
        ),
        i32::from(leaf_rect.y) + i32::from(leaf_rect.height / 2),
    );

    group.bench_function("plan_reflow_move", |b| {
        b.iter(|| {
            let plan = tree
                .plan_reflow_move_with_preview(
                    source,
                    &layout,
                    black_box(reflow_pointer),
                    black_box(ftui_layout::PaneMotionVector::from_delta(24, 2, 32, 0)),
                    None,
                    PANE_MAGNETIC_FIELD_CELLS,
                )
                .expect("reflow planning should succeed");
            black_box(plan.operations.len());
        });
    });

    group.bench_function("plan_edge_resize", |b| {
        b.iter(|| {
            let plan = tree
                .plan_edge_resize(
                    leaf,
                    &layout,
                    PaneResizeGrip::Right,
                    black_box(resize_pointer),
                    black_box(PanePressureSnapProfile::from_motion(
                        ftui_layout::PaneMotionVector::from_delta(18, 1, 24, 0),
                    )),
                )
                .expect("edge resize planning should succeed");
            black_box(plan.operations.len());
        });
    });

    group.finish();
}

fn bench_pane_core_timeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("pane/core/timeline");
    let base = build_pane_tree(32);
    let split_ids = pane_split_ids(&base);
    let ratios = [
        PaneSplitRatio::new(3, 2).expect("ratio 3:2 should be valid"),
        PaneSplitRatio::new(2, 3).expect("ratio 2:3 should be valid"),
        PaneSplitRatio::new(5, 4).expect("ratio 5:4 should be valid"),
        PaneSplitRatio::new(4, 5).expect("ratio 4:5 should be valid"),
    ];

    for &(name, operation_count) in &[
        ("apply_and_replay_8_ops", 8usize),
        ("apply_and_replay_32_ops", 32usize),
        ("apply_and_replay_256_ops", 256usize),
    ] {
        group.bench_function(name, |b| {
            b.iter_batched(
                || (base.clone(), PaneInteractionTimeline::with_baseline(&base)),
                |(mut tree, mut timeline)| {
                    for idx in 0..operation_count {
                        let split = split_ids[idx % split_ids.len()];
                        let ratio = ratios[idx % ratios.len()];
                        timeline
                            .apply_and_record(
                                &mut tree,
                                idx as u64,
                                80_000 + idx as u64,
                                PaneOperation::SetSplitRatio { split, ratio },
                            )
                            .expect("timeline set_split_ratio should succeed");
                    }
                    let replayed = timeline.replay().expect("timeline replay should succeed");
                    black_box(replayed.state_hash());
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

// ============================================================================
// E-graph Layout Benchmarks (bd-14g59.4)
// ============================================================================

use ftui_layout::egraph::{self, SaturationConfig};

/// Standard Flex.split for comparison baseline.
fn flex_solve(constraints: &[Constraint], total: u16) -> Vec<u16> {
    let area = Rect::from_size(total, 1);
    let flex = Flex::horizontal().constraints(constraints.to_vec());
    flex.split(area).iter().map(|r| r.width).collect()
}

fn bench_egraph_vs_standard(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/egraph_vs_standard");

    // Scenario 1: typical sidebar + content + footer
    let typical: Vec<Constraint> = vec![
        Constraint::Fixed(30),
        Constraint::Min(100),
        Constraint::Fixed(20),
    ];

    // Scenario 2: dense grid-like 50 widgets
    let dense: Vec<Constraint> = (0..50)
        .map(|i| match i % 5 {
            0 => Constraint::Fixed(10),
            1 => Constraint::Percentage(20.0),
            2 => Constraint::Min(5),
            3 => Constraint::Max(30),
            4 => Constraint::Ratio(1, 3),
            _ => unreachable!(),
        })
        .collect();

    // Scenario 3: deeply nested (simulated as 100 constraints)
    let deep: Vec<Constraint> = (0..100)
        .map(|i| {
            if i % 2 == 0 {
                Constraint::Ratio(1, 100)
            } else {
                Constraint::Min(1)
            }
        })
        .collect();

    // Scenario 4: pathological mixed (200 widgets)
    let pathological: Vec<Constraint> = (0..200)
        .map(|i| match i % 7 {
            0 => Constraint::Fixed(i as u16 % 50),
            1 => Constraint::Percentage((i as f32 * 0.5) % 100.0),
            2 => Constraint::Min(i as u16 % 30),
            3 => Constraint::Max(i as u16 % 80),
            4 => Constraint::Ratio((i as u32 % 10) + 1, 10),
            5 => Constraint::Fill,
            _ => Constraint::FitContentBounded { min: 5, max: 50 },
        })
        .collect();

    // Scenario 5: 500 widgets (scale test)
    let large: Vec<Constraint> = (0..500)
        .map(|i| Constraint::Fixed(i as u16 % 100))
        .collect();

    let config = SaturationConfig::default();
    let scenarios: &[(&str, &[Constraint], u16)] = &[
        ("typical_3w", &typical, 200),
        ("dense_50w", &dense, 500),
        ("deep_100w", &deep, 800),
        ("pathological_200w", &pathological, 1000),
        ("large_500w", &large, 2000),
    ];

    for &(name, constraints, total) in scenarios {
        // Benchmark: Standard Flex.split
        group.bench_with_input(
            BenchmarkId::new("flex_split", name),
            &(constraints, total),
            |b, &(constraints, total)| b.iter(|| black_box(flex_solve(constraints, total))),
        );

        // Benchmark: E-graph solve_layout
        group.bench_with_input(
            BenchmarkId::new("egraph_solve", name),
            &(constraints, total),
            |b, &(constraints, total)| {
                b.iter(|| black_box(egraph::solve_layout(constraints, total, &config)))
            },
        );
    }

    group.finish();
}

fn bench_egraph_saturation_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/egraph_saturation");

    for n in [10, 50, 100, 200, 500] {
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

        let config = SaturationConfig::default();

        group.bench_with_input(
            BenchmarkId::new("solve", n),
            &constraints,
            |b, constraints| b.iter(|| black_box(egraph::solve_layout(constraints, 1000, &config))),
        );
    }

    group.finish();
}

fn bench_egraph_budget_impact(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/egraph_budget");

    let constraints: Vec<Constraint> = (0..100)
        .map(|i| match i % 5 {
            0 => Constraint::Fixed(10),
            1 => Constraint::Percentage(20.0),
            2 => Constraint::Min(5),
            3 => Constraint::Max(30),
            4 => Constraint::Ratio(1, 3),
            _ => unreachable!(),
        })
        .collect();

    for budget in [100, 500, 1_000, 5_000, 10_000] {
        let config = SaturationConfig {
            node_budget: budget,
            iteration_limit: 100,
            time_limit_us: 50_000,
            memory_limit: 10 * 1024 * 1024,
        };

        group.bench_with_input(
            BenchmarkId::new("budget", budget),
            &constraints,
            |b, constraints| b.iter(|| black_box(egraph::solve_layout(constraints, 1000, &config))),
        );
    }

    group.finish();
}

// ============================================================================
// van Emde Boas Tree Layout Benchmarks (bd-xwkon)
// ============================================================================

use ftui_layout::veb_tree::{TreeNode, VebTree};

/// Build a binary tree of given depth as `TreeNode` list.
fn make_binary_tree_nodes(depth: u16) -> Vec<TreeNode<u32>> {
    let mut nodes = Vec::new();
    let mut next_id = 1u32;
    fn build(id: u32, remaining: u16, next_id: &mut u32, nodes: &mut Vec<TreeNode<u32>>) {
        if remaining == 0 {
            nodes.push(TreeNode::new(id, id, vec![]));
            return;
        }
        let left = *next_id;
        *next_id += 1;
        let right = *next_id;
        *next_id += 1;
        nodes.push(TreeNode::new(id, id, vec![left, right]));
        build(left, remaining - 1, next_id, nodes);
        build(right, remaining - 1, next_id, nodes);
    }
    build(0, depth, &mut next_id, &mut nodes);
    nodes
}

/// Build a wide tree: root → N children (each a leaf).
fn make_wide_tree_nodes(n: u32) -> Vec<TreeNode<u32>> {
    let mut nodes = vec![TreeNode::new(0, 0, (1..=n).collect())];
    for i in 1..=n {
        nodes.push(TreeNode::new(i, i, vec![]));
    }
    nodes
}

fn bench_veb_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("veb/build");

    // Binary trees at different sizes.
    for depth in [6u16, 9, 13] {
        // depth 6 → 127 nodes, 9 → 1023, 13 → 16383
        let nodes = make_binary_tree_nodes(depth);
        let count = nodes.len();
        group.bench_with_input(BenchmarkId::new("binary", count), &nodes, |b, nodes| {
            b.iter(|| black_box(VebTree::build(nodes.clone())))
        });
    }

    // Wide trees.
    for n in [100u32, 1000, 5000] {
        let nodes = make_wide_tree_nodes(n);
        let count = nodes.len();
        group.bench_with_input(BenchmarkId::new("wide", count), &nodes, |b, nodes| {
            b.iter(|| black_box(VebTree::build(nodes.clone())))
        });
    }

    group.finish();
}

fn bench_veb_traverse(c: &mut Criterion) {
    let mut group = c.benchmark_group("veb/traverse");

    for depth in [6u16, 9, 13] {
        let nodes = make_binary_tree_nodes(depth);
        let count = nodes.len();
        let tree = VebTree::build(nodes);

        // vEB-order traversal (sequential memory access).
        group.bench_with_input(BenchmarkId::new("veb_order", count), &tree, |b, tree| {
            b.iter(|| {
                let mut sum = 0u64;
                for entry in tree.iter() {
                    sum = sum.wrapping_add(entry.id as u64);
                }
                black_box(sum)
            })
        });

        // DFS traversal (pointer-chasing via child indices).
        group.bench_with_input(BenchmarkId::new("dfs_order", count), &tree, |b, tree| {
            b.iter(|| {
                let mut sum = 0u64;
                for entry in tree.iter_dfs() {
                    sum = sum.wrapping_add(entry.id as u64);
                }
                black_box(sum)
            })
        });
    }

    group.finish();
}

fn bench_veb_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("veb/lookup");

    for depth in [6u16, 9, 13] {
        let nodes = make_binary_tree_nodes(depth);
        let count = nodes.len();
        let tree = VebTree::build(nodes);
        let ids: Vec<u32> = tree.iter().map(|e| e.id).collect();

        group.bench_with_input(
            BenchmarkId::new("by_id", count),
            &(&tree, &ids),
            |b, (tree, ids)| {
                b.iter(|| {
                    let mut sum = 0u64;
                    for &id in *ids {
                        if let Some(e) = tree.get(id) {
                            sum = sum.wrapping_add(e.depth as u64);
                        }
                    }
                    black_box(sum)
                })
            },
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
    bench_pane_core_solve_layout,
    bench_pane_core_apply_operation,
    bench_pane_core_planning,
    bench_pane_core_timeline,
    bench_egraph_vs_standard,
    bench_egraph_saturation_scaling,
    bench_egraph_budget_impact,
    bench_veb_build,
    bench_veb_traverse,
    bench_veb_lookup,
);

criterion_main!(benches);

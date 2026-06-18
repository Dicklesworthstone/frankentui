//! Persistent/versioned pane-tree adoption benchmark (`bd-1k7ek.5`).
//!
//! Compares the prototype [`PaneVersionStore`] (structural sharing, replay-free
//! navigation) against the production checkpointed
//! [`PaneInteractionTimeline`] across two workloads:
//!
//! * `resize_storm` — a pure `SetSplitRatio` drag-resize workload on a fixed
//!   tree (the hot path), where structural sharing is maximal.
//! * `mixed_session` — a realistic editing session mixing split/close/swap/move
//!   /ratio operations on an evolving tree.
//!
//! It emits memory-retention, version/checkpoint counts, and replay/undo timing
//! so the adoption decision is evidence-backed. Run with:
//!
//! ```text
//! cargo bench -p ftui-layout --bench pane_persistent_bench
//! cargo bench -p ftui-layout --bench pane_persistent_bench -- --out /tmp/persistent.json
//! ```

use std::alloc::System;
use std::env;
use std::fs;
use std::time::Instant;

use ftui_layout::{
    PaneId, PaneInteractionTimeline, PaneLeaf, PaneNodeKind, PaneOperation, PanePlacement,
    PaneSplitRatio, PaneTree, PaneVersionStore, PaneVersioningReport, SplitAxis, VersionedPaneTree,
};
use serde::Serialize;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

// --- deterministic generation ------------------------------------------------

struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn range(&mut self, min: u32, max: u32) -> u32 {
        min + (self.next_u64() % u64::from(max - min)) as u32
    }

    fn index(&mut self, len: usize) -> usize {
        (self.next_u64() % len as u64) as usize
    }

    fn boolean(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}

fn leaf_ids(tree: &PaneTree) -> Vec<PaneId> {
    tree.nodes()
        .filter_map(|n| match n.kind {
            PaneNodeKind::Leaf(_) => Some(n.id),
            PaneNodeKind::Split(_) => None,
        })
        .collect()
}

fn split_ids(tree: &PaneTree) -> Vec<PaneId> {
    tree.nodes()
        .filter_map(|n| match n.kind {
            PaneNodeKind::Split(_) => Some(n.id),
            PaneNodeKind::Leaf(_) => None,
        })
        .collect()
}

fn ratio(rng: &mut Lcg) -> PaneSplitRatio {
    PaneSplitRatio::new(rng.range(1, 32), rng.range(1, 32)).expect("ratio")
}

fn axis(rng: &mut Lcg) -> SplitAxis {
    if rng.boolean() {
        SplitAxis::Horizontal
    } else {
        SplitAxis::Vertical
    }
}

fn placement(rng: &mut Lcg) -> PanePlacement {
    if rng.boolean() {
        PanePlacement::ExistingFirst
    } else {
        PanePlacement::IncomingFirst
    }
}

/// Grow a balanced-ish tree to roughly `leaf_count` leaves by splitting the
/// smallest-id leaf each step (deterministic).
fn grow_tree(leaf_count: usize) -> PaneTree {
    let mut tree = PaneTree::singleton("root");
    let mut id = 1u64;
    while leaf_ids(&tree).len() < leaf_count {
        let target = *leaf_ids(&tree).iter().min().expect("leaf");
        tree.apply_operation(
            id,
            PaneOperation::SplitLeaf {
                target,
                axis: if id.is_multiple_of(2) {
                    SplitAxis::Horizontal
                } else {
                    SplitAxis::Vertical
                },
                ratio: PaneSplitRatio::new(1, 1).expect("ratio"),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new(format!("leaf-{id}")),
            },
        )
        .expect("grow split");
        id += 1;
    }
    tree
}

/// A pure `SetSplitRatio` workload over the fixed `base` tree's splits.
fn resize_storm_ops(base: &PaneTree, op_count: usize, seed: u64) -> Vec<PaneOperation> {
    let splits = split_ids(base);
    let mut rng = Lcg::new(seed);
    (0..op_count)
        .map(|_| PaneOperation::SetSplitRatio {
            split: splits[rng.index(splits.len())],
            ratio: ratio(&mut rng),
        })
        .collect()
}

/// A mixed editing workload generated against an evolving clone of `base`.
fn mixed_session_ops(base: &PaneTree, op_count: usize, seed: u64) -> Vec<PaneOperation> {
    let mut tree = base.clone();
    let mut rng = Lcg::new(seed);
    let mut ops = Vec::with_capacity(op_count);
    for step in 0..op_count {
        let leaves = leaf_ids(&tree);
        let splits = split_ids(&tree);
        // Weight toward SetSplitRatio (the hot path); keep the tree from
        // collapsing by biasing splits when leaves get scarce.
        let mut bag: Vec<u8> = Vec::new();
        if !splits.is_empty() {
            bag.extend([5, 5, 5, 5]); // SetSplitRatio (heavy)
        }
        if leaves.len() < 6 || splits.is_empty() {
            bag.push(1); // SplitLeaf
            bag.push(1);
        } else {
            bag.push(1);
        }
        if leaves.len() > 2 {
            bag.push(2); // CloseNode
            bag.push(4); // SwapNodes
            bag.push(3); // MoveSubtree
        }
        let op = match bag[rng.index(bag.len())] {
            1 => PaneOperation::SplitLeaf {
                target: leaves[rng.index(leaves.len())],
                axis: axis(&mut rng),
                ratio: ratio(&mut rng),
                placement: placement(&mut rng),
                new_leaf: PaneLeaf::new(format!("m{step}")),
            },
            2 => PaneOperation::CloseNode {
                target: leaves[rng.index(leaves.len())],
            },
            3 => {
                let s = rng.index(leaves.len());
                let mut t = rng.index(leaves.len());
                while t == s {
                    t = rng.index(leaves.len());
                }
                PaneOperation::MoveSubtree {
                    source: leaves[s],
                    target: leaves[t],
                    axis: axis(&mut rng),
                    ratio: ratio(&mut rng),
                    placement: placement(&mut rng),
                }
            }
            4 => {
                let f = rng.index(leaves.len());
                let mut s = rng.index(leaves.len());
                while s == f {
                    s = rng.index(leaves.len());
                }
                PaneOperation::SwapNodes {
                    first: leaves[f],
                    second: leaves[s],
                }
            }
            _ => PaneOperation::SetSplitRatio {
                split: splits[rng.index(splits.len())],
                ratio: ratio(&mut rng),
            },
        };
        tree.apply_operation((step as u64) + 1, op.clone())
            .expect("mixed op applies");
        ops.push(op);
    }
    ops
}

// --- measurement -------------------------------------------------------------

fn net_bytes(region: &Region<System>) -> i64 {
    let stats = region.change();
    stats.bytes_allocated as i64 - stats.bytes_deallocated as i64
}

#[derive(Debug, Serialize)]
struct ScenarioResult {
    scenario: &'static str,
    leaf_count: usize,
    op_count: usize,
    // Build (apply) timing
    persistent_apply_ns: u128,
    checkpointed_apply_ns: u128,
    // Memory retained after building the full history
    persistent_bytes_retained: i64,
    checkpointed_bytes_retained: i64,
    memory_ratio_persistent_over_checkpointed: f64,
    // Structural sharing (unbounded retention)
    sharing: PaneVersioningReport,
    checkpoint_count: usize,
    checkpoint_interval: usize,
    // Worst-case replay depth a single timeline navigation pays (bounded by the
    // checkpoint interval); every navigation ALSO pays one whole-tree
    // snapshot-restore validation, which dominates for large trees.
    replay_depth_bound: usize,
    // Bounded-retention persistent store (the recommended adoption shape).
    bounded_window: usize,
    bounded_bytes_retained: i64,
    bounded_sharing: PaneVersioningReport,
    bounded_memory_ratio_over_checkpointed: f64,
    // Navigation: full undo→redo sweep over the whole history
    timeline_full_sweep_ns: u128,
    persistent_pure_sweep_ns: u128,
    persistent_flatten_sweep_ns: u128,
    pure_nav_speedup_x: f64,
    flatten_nav_speedup_x: f64,
    // Random access to scattered versions (replay vs O(1))
    timeline_random_access_ns: u128,
    persistent_random_access_ns: u128,
    random_access_speedup_x: f64,
}

fn run_scenario(
    scenario: &'static str,
    leaf_count: usize,
    op_count: usize,
    seed: u64,
) -> ScenarioResult {
    let base = grow_tree(leaf_count);
    let ops = if scenario == "resize_storm" {
        resize_storm_ops(&base, op_count, seed)
    } else {
        mixed_session_ops(&base, op_count, seed)
    };

    // --- PERSISTENT: build, measure apply time and retained memory ----------
    let initial = VersionedPaneTree::from_pane_tree(&base);
    let persistent_region = Region::new(GLOBAL);
    let mut store = PaneVersionStore::new(initial);
    let t = Instant::now();
    for op in &ops {
        store.apply(op).expect("persistent apply");
    }
    let persistent_apply_ns = t.elapsed().as_nanos();
    let persistent_bytes_retained = net_bytes(&persistent_region);
    let sharing = store.report();

    // --- CHECKPOINTED: build, measure apply time and retained memory --------
    let checkpoint_region = Region::new(GLOBAL);
    let mut tree = base.clone();
    let mut timeline = PaneInteractionTimeline::with_baseline(&tree);
    let t = Instant::now();
    for (idx, op) in ops.iter().enumerate() {
        timeline
            .apply_and_record(&mut tree, idx as u64, (idx as u64) + 1, op.clone())
            .expect("timeline apply");
    }
    let checkpointed_apply_ns = t.elapsed().as_nanos();
    let checkpointed_bytes_retained = net_bytes(&checkpoint_region);
    let diag = timeline.replay_diagnostics();
    let replay_depth_bound = diag.checkpoint_interval.saturating_sub(1);

    // --- BOUNDED-WINDOW PERSISTENT: the recommended adoption shape ----------
    // Same workload, but cap retained versions so memory stays bounded while
    // navigation within the window remains O(1).
    let bounded_window = 64usize;
    let bounded_region = Region::new(GLOBAL);
    let mut bounded = PaneVersionStore::with_max_versions(
        VersionedPaneTree::from_pane_tree(&base),
        bounded_window,
    );
    for op in &ops {
        bounded.apply(op).expect("bounded apply");
    }
    let bounded_bytes_retained = net_bytes(&bounded_region);
    let bounded_sharing = bounded.report();
    let bounded_memory_ratio_over_checkpointed = if checkpointed_bytes_retained > 0 {
        bounded_bytes_retained as f64 / checkpointed_bytes_retained as f64
    } else {
        0.0
    };

    // --- NAVIGATION: full undo→redo sweep -----------------------------------
    // Timeline: replay-based.
    let t = Instant::now();
    while timeline.undo(&mut tree).expect("undo") {}
    while timeline.redo(&mut tree).expect("redo") {}
    let timeline_full_sweep_ns = t.elapsed().as_nanos();

    // Persistent pure navigation (cursor only).
    let t = Instant::now();
    while store.undo() {}
    while store.redo() {}
    let persistent_pure_sweep_ns = t.elapsed().as_nanos();

    // Persistent navigation + flatten to a usable canonical tree each step.
    let t = Instant::now();
    while store.undo() {
        let _ = store.current().to_pane_tree().expect("flatten");
    }
    while store.redo() {
        let _ = store.current().to_pane_tree().expect("flatten");
    }
    let persistent_flatten_sweep_ns = t.elapsed().as_nanos();

    // --- RANDOM ACCESS: jump to scattered versions --------------------------
    let mut rng = Lcg::new(seed ^ 0xCA75);
    let targets: Vec<usize> = (0..64).map(|_| rng.index(op_count + 1)).collect();

    // Timeline random access via replay from baseline.
    let t = Instant::now();
    let mut acc_hash = 0u64;
    for &target in &targets {
        // Reconstruct by undo/redo to the target cursor.
        while timeline.applied_len() > target {
            timeline.undo(&mut tree).expect("undo");
        }
        while timeline.applied_len() < target {
            timeline.redo(&mut tree).expect("redo");
        }
        acc_hash ^= tree.state_hash();
    }
    let timeline_random_access_ns = t.elapsed().as_nanos();

    // Persistent random access: set cursor and read (O(1)) + flatten for parity.
    let t = Instant::now();
    let mut acc_hash2 = 0u64;
    for &target in &targets {
        while store.cursor() > target {
            store.undo();
        }
        while store.cursor() < target {
            store.redo();
        }
        acc_hash2 ^= store.current().state_hash().expect("flatten");
    }
    let persistent_random_access_ns = t.elapsed().as_nanos();
    assert_eq!(acc_hash, acc_hash2, "random-access hashes must match");

    let memory_ratio = if checkpointed_bytes_retained > 0 {
        persistent_bytes_retained as f64 / checkpointed_bytes_retained as f64
    } else {
        0.0
    };
    let pure_speedup = ratio_speedup(timeline_full_sweep_ns, persistent_pure_sweep_ns);
    let flatten_speedup = ratio_speedup(timeline_full_sweep_ns, persistent_flatten_sweep_ns);
    let random_speedup = ratio_speedup(timeline_random_access_ns, persistent_random_access_ns);

    ScenarioResult {
        scenario,
        leaf_count,
        op_count,
        persistent_apply_ns,
        checkpointed_apply_ns,
        persistent_bytes_retained,
        checkpointed_bytes_retained,
        memory_ratio_persistent_over_checkpointed: memory_ratio,
        sharing,
        checkpoint_count: diag.checkpoint_count,
        checkpoint_interval: diag.checkpoint_interval,
        replay_depth_bound,
        bounded_window,
        bounded_bytes_retained,
        bounded_sharing,
        bounded_memory_ratio_over_checkpointed,
        timeline_full_sweep_ns,
        persistent_pure_sweep_ns,
        persistent_flatten_sweep_ns,
        pure_nav_speedup_x: pure_speedup,
        flatten_nav_speedup_x: flatten_speedup,
        timeline_random_access_ns,
        persistent_random_access_ns,
        random_access_speedup_x: random_speedup,
    }
}

fn ratio_speedup(baseline_ns: u128, candidate_ns: u128) -> f64 {
    if candidate_ns == 0 {
        f64::INFINITY
    } else {
        baseline_ns as f64 / candidate_ns as f64
    }
}

fn print_result(r: &ScenarioResult) {
    println!("== scenario: {} ==", r.scenario);
    println!("  leaves={} ops={}", r.leaf_count, r.op_count);
    println!(
        "  apply: persistent={}us checkpointed={}us",
        r.persistent_apply_ns / 1000,
        r.checkpointed_apply_ns / 1000
    );
    println!(
        "  memory retained: persistent={}B checkpointed={}B (ratio={:.2}x)",
        r.persistent_bytes_retained,
        r.checkpointed_bytes_retained,
        r.memory_ratio_persistent_over_checkpointed
    );
    println!(
        "  sharing: versions={} distinct_nodes={} logical_nodes={} shared={} ratio={:.3}",
        r.sharing.version_count,
        r.sharing.distinct_nodes,
        r.sharing.total_logical_nodes,
        r.sharing.shared_nodes,
        r.sharing.sharing_ratio
    );
    println!(
        "  checkpoints={} interval={} replay_depth_bound={}",
        r.checkpoint_count, r.checkpoint_interval, r.replay_depth_bound
    );
    println!(
        "  bounded(window={}): retained={}B vs checkpointed={}B (ratio={:.2}x) distinct_nodes={} sharing={:.3}",
        r.bounded_window,
        r.bounded_bytes_retained,
        r.checkpointed_bytes_retained,
        r.bounded_memory_ratio_over_checkpointed,
        r.bounded_sharing.distinct_nodes,
        r.bounded_sharing.sharing_ratio
    );
    println!(
        "  full undo->redo sweep: timeline={}us persistent_pure={}us persistent_flatten={}us",
        r.timeline_full_sweep_ns / 1000,
        r.persistent_pure_sweep_ns / 1000,
        r.persistent_flatten_sweep_ns / 1000
    );
    println!(
        "    speedup: pure={:.1}x flatten={:.1}x",
        r.pure_nav_speedup_x, r.flatten_nav_speedup_x
    );
    println!(
        "  random access (64 jumps): timeline={}us persistent={}us speedup={:.1}x",
        r.timeline_random_access_ns / 1000,
        r.persistent_random_access_ns / 1000,
        r.random_access_speedup_x
    );
    println!();
}

fn main() {
    let mut out_path: Option<String> = None;
    let args: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--out" && i + 1 < args.len() {
            out_path = Some(args[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }
    if out_path.is_none() {
        out_path = env::var("PANE_PERSISTENT_BENCH_OUT").ok();
    }

    let results = vec![
        run_scenario("resize_storm", 63, 512, 0x1111),
        run_scenario("resize_storm", 255, 512, 0x2222),
        run_scenario("mixed_session", 32, 256, 0x3333),
    ];

    println!("pane_persistent_bench — persistent vs checkpointed replay\n");
    for r in &results {
        print_result(r);
    }

    if let Some(path) = out_path {
        match serde_json::to_string_pretty(&results) {
            Ok(json) => {
                if let Err(err) = fs::write(&path, json) {
                    eprintln!("failed to write {path}: {err}");
                } else {
                    println!("wrote manifest: {path}");
                }
            }
            Err(err) => eprintln!("failed to serialize results: {err}"),
        }
    }
}

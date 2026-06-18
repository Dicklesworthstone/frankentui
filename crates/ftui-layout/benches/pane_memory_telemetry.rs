//! Cross-strategy pane-memory telemetry artifact (`bd-25wj7.1`).
//!
//! Emits a deterministic JSON manifest comparing the **baseline** (no history),
//! **checkpointed** ([`PaneInteractionTimeline`]), and **persistent**
//! ([`PaneVersionStore`]) strategies across two workloads, capturing both:
//!
//! * **transient allocations** — measured with `stats_alloc` while each strategy
//!   builds the full operation history (allocation count + bytes), and
//! * **retained-state economics** — the modeled [`PaneMemoryComparison`] with a
//!   per-class byte breakdown and the dominant memory driver per strategy.
//!
//! The retained model is deterministic and reproducible (it is the library's
//! pure [`pane_memory_comparison`]); only the transient counts depend on the
//! allocator. Run with:
//!
//! ```text
//! cargo bench -p ftui-layout --bench pane_memory_telemetry
//! cargo bench -p ftui-layout --bench pane_memory_telemetry -- --out /tmp/pane_memory.json
//! ```
//!
//! Downstream beads consume this artifact as a concrete baseline: bounded
//! retention/pruning (`bd-25wj7.2`) and churn reduction (`bd-25wj7.3`).

use std::alloc::System;
use std::env;
use std::fs;

use ftui_layout::{
    PANE_MEMORY_TELEMETRY_SCHEMA_VERSION, PaneExecutionDecision, PaneExecutionPolicy, PaneId,
    PaneInteractionTimeline, PaneLeaf, PaneMemoryComparison, PaneNodeKind, PaneOperation,
    PanePlacement, PaneRetentionDecision, PaneRetentionPolicy, PaneSplitRatio, PaneTree,
    PaneVersionStore, PaneWorkloadProfile, SplitAxis, VersionedPaneTree,
    apply_retention_to_timeline, apply_retention_to_version_store, pane_memory_comparison,
};
use serde::Serialize;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

// --- deterministic generation (shared with pane_persistent_bench) ------------

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

/// Grow a balanced-ish tree to roughly `leaf_count` leaves (deterministic).
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
        let mut bag: Vec<u8> = Vec::new();
        if !splits.is_empty() {
            bag.extend([5, 5, 5, 5]); // SetSplitRatio (heavy)
        }
        if leaves.len() < 6 || splits.is_empty() {
            bag.push(1);
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

/// Transient (build-time) allocation telemetry for one strategy.
#[derive(Debug, Serialize)]
struct StrategyTransient {
    strategy: &'static str,
    transient_allocations: u64,
    transient_bytes_allocated: u64,
    transient_bytes_deallocated: u64,
    /// `bytes_allocated - bytes_deallocated`: the measured live retained bytes
    /// after building the full history (complements the modeled estimate).
    net_retained_bytes_measured: i64,
}

fn transient(strategy: &'static str, region: &Region<System>) -> StrategyTransient {
    let s = region.change();
    StrategyTransient {
        strategy,
        transient_allocations: s.allocations as u64,
        transient_bytes_allocated: s.bytes_allocated as u64,
        transient_bytes_deallocated: s.bytes_deallocated as u64,
        net_retained_bytes_measured: s.bytes_allocated as i64 - s.bytes_deallocated as i64,
    }
}

#[derive(Debug, Serialize)]
struct ScenarioMemoryReport {
    scenario: &'static str,
    leaf_count: usize,
    op_count: usize,
    baseline_transient: StrategyTransient,
    checkpointed_transient: StrategyTransient,
    persistent_transient: StrategyTransient,
    /// Modeled retained-state comparison (deterministic, per-class breakdown).
    comparison: PaneMemoryComparison,
    /// Before/after evidence: a representative byte-budget retention policy
    /// applied to the persistent version store (budget = 1/4 unbounded footprint).
    version_retention: PaneRetentionDecision,
    /// Before/after evidence: the same policy class applied to the checkpointed
    /// timeline (budget = 1/2 unbounded footprint).
    timeline_retention: PaneRetentionDecision,
    /// The deterministic execution-strategy the selector picks for this workload
    /// shape (resize-dominated bursts route to persistent; mixed falls back to
    /// the checkpointed default).
    selector_decision: PaneExecutionDecision,
}

fn run_scenario(
    scenario: &'static str,
    base: &PaneTree,
    ops: &[PaneOperation],
) -> ScenarioMemoryReport {
    // BASELINE: no history — mutate one live tree in place.
    let baseline_region = Region::new(GLOBAL);
    let mut live = base.clone();
    for (i, op) in ops.iter().enumerate() {
        live.apply_operation_conservative(i as u64 + 1, op.clone())
            .expect("baseline apply");
    }
    let baseline_transient = transient("baseline", &baseline_region);

    // CHECKPOINTED: production timeline (snapshots + op log + replay).
    let checkpoint_region = Region::new(GLOBAL);
    let mut tree = base.clone();
    let mut timeline = PaneInteractionTimeline::default();
    for (i, op) in ops.iter().enumerate() {
        let id = i as u64;
        timeline
            .apply_and_record(&mut tree, id, id, op.clone())
            .expect("timeline apply");
    }
    let checkpointed_transient = transient("checkpointed", &checkpoint_region);

    // PERSISTENT: structurally shared version store.
    let persistent_region = Region::new(GLOBAL);
    let mut store = PaneVersionStore::new(VersionedPaneTree::from_pane_tree(base));
    for op in ops {
        store.apply(op).expect("store apply");
    }
    let persistent_transient = transient("persistent", &persistent_region);

    let comparison = pane_memory_comparison(&live, &timeline, &store);

    // Before/after evidence: apply a representative byte-budget retention policy
    // to clones of both substrates (the originals stay intact for the comparison).
    let version_budget = comparison.persistent.total_retained_bytes / 4;
    let mut store_for_policy = store.clone();
    let version_retention = apply_retention_to_version_store(
        &mut store_for_policy,
        &PaneRetentionPolicy::bounded(version_budget, 0),
    );
    let timeline_budget = comparison.checkpointed.total_retained_bytes / 2;
    let mut timeline_for_policy = timeline.clone();
    let timeline_retention = apply_retention_to_timeline(
        &mut timeline_for_policy,
        &PaneRetentionPolicy::bounded(timeline_budget, 0),
    );

    // Deterministic strategy selection for this workload shape, carrying the
    // representative version-store retention budget.
    let selector_decision =
        PaneExecutionPolicy::adaptive(PaneRetentionPolicy::bounded(version_budget, 0))
            .select(PaneWorkloadProfile::observe(ops, 240, true));

    ScenarioMemoryReport {
        scenario,
        leaf_count: leaf_ids(base).len(),
        op_count: ops.len(),
        baseline_transient,
        checkpointed_transient,
        persistent_transient,
        comparison,
        version_retention,
        timeline_retention,
        selector_decision,
    }
}

/// Transient allocation churn of the `SetSplitRatio` fast path (the resize hot
/// path) applied to a plain tree — no timeline, no store, no checkpoints — so it
/// isolates per-op allocations (bd-25wj7.3 targets driving this to zero).
#[derive(Debug, Serialize)]
struct FastPathAllocs {
    op_count: usize,
    total_allocations: u64,
    total_bytes_allocated: u64,
    allocations_per_op: f64,
}

fn measure_fast_path_resize(base: &PaneTree, op_count: usize) -> FastPathAllocs {
    let ops = resize_storm_ops(base, op_count, 0xF00D);
    let mut tree = base.clone();
    // Measure only the apply loop (the clone and op generation are excluded).
    let region = Region::new(GLOBAL);
    for (i, op) in ops.iter().enumerate() {
        tree.apply_operation(i as u64 + 1, op.clone())
            .expect("resize apply");
    }
    let s = region.change();
    FastPathAllocs {
        op_count: ops.len(),
        total_allocations: s.allocations as u64,
        total_bytes_allocated: s.bytes_allocated as u64,
        allocations_per_op: s.allocations as f64 / ops.len() as f64,
    }
}

#[derive(Debug, Serialize)]
struct Manifest {
    artifact: &'static str,
    schema_version: u16,
    bead: &'static str,
    scenarios: Vec<ScenarioMemoryReport>,
    fast_path_resize: FastPathAllocs,
}

fn out_path() -> Option<String> {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--out" {
            return args.next();
        }
    }
    None
}

fn print_scenario(report: &ScenarioMemoryReport) {
    println!("== scenario: {} ==", report.scenario);
    println!("  leaves={} ops={}", report.leaf_count, report.op_count);
    for t in [
        &report.baseline_transient,
        &report.checkpointed_transient,
        &report.persistent_transient,
    ] {
        println!(
            "  transient[{}]: allocs={} alloc_bytes={} net_retained_measured={}B",
            t.strategy,
            t.transient_allocations,
            t.transient_bytes_allocated,
            t.net_retained_bytes_measured
        );
    }
    let c = &report.comparison;
    for f in [&c.baseline, &c.checkpointed, &c.persistent] {
        println!(
            "  modeled[{}]: total={}B nodes={}B leaf={}B ext={}B oplog={}B hashes={}B meta={}B driver={}",
            f.strategy.as_str(),
            f.total_retained_bytes,
            f.node_struct_bytes,
            f.leaf_payload_bytes,
            f.extension_payload_bytes,
            f.operation_payload_bytes,
            f.state_hash_overhead_bytes,
            f.container_and_metadata_bytes,
            f.dominant_driver.as_str(),
        );
    }
    println!("  {}", c.summary);
    println!("  policy/{}", report.version_retention.log);
    println!("  policy/{}", report.timeline_retention.log);
    println!("  selector/{}", report.selector_decision.log);
}

fn main() {
    let resize_base = grow_tree(64);
    let resize_ops = resize_storm_ops(&resize_base, 512, 0xABCD);
    let mixed_base = grow_tree(32);
    let mixed_ops = mixed_session_ops(&mixed_base, 384, 0x1234);

    let scenarios = vec![
        run_scenario("resize_storm", &resize_base, &resize_ops),
        run_scenario("mixed_session", &mixed_base, &mixed_ops),
    ];

    let manifest = Manifest {
        artifact: "pane_memory_telemetry",
        schema_version: PANE_MEMORY_TELEMETRY_SCHEMA_VERSION,
        bead: "bd-25wj7.1",
        scenarios,
        fast_path_resize: measure_fast_path_resize(&resize_base, 512),
    };

    for report in &manifest.scenarios {
        print_scenario(report);
    }
    let fp = &manifest.fast_path_resize;
    println!(
        "== fast_path_resize (bd-25wj7.3) ==\n  ops={} allocations={} ({:.2}/op) bytes={}",
        fp.op_count, fp.total_allocations, fp.allocations_per_op, fp.total_bytes_allocated
    );

    if let Some(path) = out_path() {
        let json = serde_json::to_string_pretty(&manifest).expect("serialize manifest");
        fs::write(&path, json).expect("write manifest");
        println!("wrote {path}");
    }
}

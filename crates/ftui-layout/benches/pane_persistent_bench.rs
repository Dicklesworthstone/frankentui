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
//!
//! Add `--live` for paired execution-engine measurements, including measured
//! observation/maintenance and solved leaf rectangles. It uses 20 measured
//! repetitions plus one excluded warmup; `--live-repetitions 1` is an explicit
//! diagnostic run. Live JSON reports raw timing samples, p50/p95/p99, allocator
//! traffic, solve errors and executed-path counters. It does not measure the
//! showcase renderer, terminal IO or browser execution.
//! Observation tapes preserve the exact samples supplied to each engine. Their
//! shared pair clock includes control and benchmark overhead: adaptive decisions
//! describe coupled-harness observations, not production input cadence.

use std::alloc::System;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::process::Command;
use std::time::Instant;

use ftui_layout::{
    PaneExecutionEngine, PaneExecutionPolicy, PaneExecutionSample, PaneExecutionStatus,
    PaneHistory, PaneId, PaneInteractionTimeline, PaneLeaf, PaneMemoryStrategy, PaneModelError,
    PaneNodeKind, PaneOperation, PaneOperationFamily, PanePlacement, PaneRetentionPolicy,
    PaneSplitRatio, PaneTree, PaneVersionStore, PaneVersioningReport, Rect, SplitAxis,
    VersionedPaneTree,
};
use serde::Serialize;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, Stats, StatsAlloc};

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

// Full-wrapper measurements are opt-in. The original substrate benchmark and
// its JSON schema remain unchanged. These measurements stop at solved leaf
// rectangles; they do not execute the showcase renderer or terminal/browser IO.
type LeafProjection = Result<Vec<(PaneId, Rect)>, PaneModelError>;

fn leaf_projection(tree: &PaneTree) -> LeafProjection {
    let layout = tree.solve_layout(Rect::new(0, 0, 240, 80))?;
    Ok(tree
        .nodes()
        .filter(|node| matches!(node.kind, PaneNodeKind::Leaf(_)))
        .map(|node| (node.id, layout.visual_rect(node.id).expect("solved leaf")))
        .collect())
}

#[derive(Default, Serialize)]
struct LivePhase {
    // Preserve acquisition order; warmup, oracle checks, and sample bookkeeping
    // are excluded from both timing and allocator regions.
    elapsed_ns: Vec<u128>,
    allocations: usize,
    deallocations: usize,
    reallocations: usize,
    bytes_allocated: usize,
    bytes_deallocated: usize,
    bytes_reallocated: isize,
    solve_successes: usize,
    solve_errors: BTreeMap<String, usize>,
}

impl LivePhase {
    fn record(&mut self, ns: u128, stats: Stats, projection: &LeafProjection) {
        self.elapsed_ns.push(ns);
        self.allocations += stats.allocations;
        self.deallocations += stats.deallocations;
        self.reallocations += stats.reallocations;
        self.bytes_allocated += stats.bytes_allocated;
        self.bytes_deallocated += stats.bytes_deallocated;
        self.bytes_reallocated += stats.bytes_reallocated;
        match projection {
            Ok(_) => self.solve_successes += 1,
            Err(error) => *self.solve_errors.entry(error.to_string()).or_default() += 1,
        }
    }

    fn report(self) -> LivePhaseReport {
        let mut sorted = self.elapsed_ns.clone();
        sorted.sort_unstable();
        let percentile = |percent: usize| {
            (!sorted.is_empty()).then(|| sorted[(sorted.len() * percent).div_ceil(100) - 1])
        };
        LivePhaseReport {
            samples: sorted.len(),
            p50_ns: percentile(50),
            p95_ns: percentile(95),
            p99_ns: percentile(99),
            total_ns: sorted.iter().sum(),
            measured: self,
        }
    }
}

#[derive(Serialize)]
struct LivePhaseReport {
    samples: usize,
    p50_ns: Option<u128>,
    p95_ns: Option<u128>,
    p99_ns: Option<u128>,
    total_ns: u128,
    measured: LivePhase,
}

#[derive(Default)]
struct LiveRun {
    apply: LivePhase,
    undo: LivePhase,
    redo: LivePhase,
    observation_tapes: Vec<LiveObservationTape>,
}

#[derive(Serialize)]
struct LiveObservationTape {
    repetition: usize,
    excluded_warmup: bool,
    samples: Vec<PaneExecutionSample>,
}

impl LiveRun {
    fn phase(&mut self, action: &LiveAction<'_>) -> &mut LivePhase {
        match action {
            LiveAction::Apply(..) => &mut self.apply,
            LiveAction::Undo => &mut self.undo,
            LiveAction::Redo => &mut self.redo,
        }
    }

    fn report(self) -> LiveRunReport {
        LiveRunReport {
            apply: self.apply.report(),
            undo: self.undo.report(),
            redo: self.redo.report(),
            observation_tapes: self.observation_tapes,
        }
    }
}

#[derive(Serialize)]
struct LiveRunReport {
    apply: LivePhaseReport,
    undo: LivePhaseReport,
    redo: LivePhaseReport,
    observation_tapes: Vec<LiveObservationTape>,
}

enum LiveDriver {
    Timeline(PaneInteractionTimeline),
    Engine(Box<PaneExecutionEngine>),
}

impl LiveDriver {
    fn history(&mut self) -> &mut dyn PaneHistory {
        match self {
            Self::Timeline(timeline) => timeline,
            Self::Engine(engine) => engine.as_mut(),
        }
    }

    fn timeline(&self) -> &PaneInteractionTimeline {
        match self {
            Self::Timeline(timeline) => timeline,
            Self::Engine(engine) => engine.timeline(),
        }
    }
}

enum LiveAction<'a> {
    Apply(u64, &'a PaneOperation),
    Undo,
    Redo,
}

fn measure_live_step(
    driver: &mut LiveDriver,
    tree: &mut PaneTree,
    action: &LiveAction<'_>,
    retention: PaneRetentionPolicy,
    origin: Instant,
) -> (u128, Stats, LeafProjection, Option<PaneExecutionSample>) {
    let mut observation = None;
    let region = Region::new(GLOBAL);
    let start = Instant::now();
    let local = match action {
        LiveAction::Apply(id, operation) => {
            driver
                .history()
                .apply_and_record(tree, *id, *id, (*operation).clone())
                .expect("full-wrapper apply");
            operation.family() == PaneOperationFamily::Local
        }
        LiveAction::Undo => {
            assert!(driver.history().undo(tree).expect("full-wrapper undo"));
            false
        }
        LiveAction::Redo => {
            assert!(driver.history().redo(tree).expect("full-wrapper redo"));
            false
        }
    };
    // The actual terminal/browser adapters observe accepted applies only.
    // Navigation must not inject fabricated operations into the selector.
    if matches!(action, LiveAction::Apply(..)) {
        let elapsed_ns =
            u64::try_from(start.elapsed().as_nanos()).expect("operation duration fits u64");
        match driver {
            LiveDriver::Timeline(timeline) => {
                ftui_layout::pane_retention::apply_to_timeline(timeline, &retention);
            }
            LiveDriver::Engine(engine) => {
                let sample = PaneExecutionSample {
                    timestamp_ns: u64::try_from(origin.elapsed().as_nanos()).expect("timestamp"),
                    elapsed_ns,
                    local,
                };
                engine
                    .observe(tree, sample)
                    .expect("full-wrapper observation and maintenance");
                observation = Some(sample);
            }
        }
    }
    let projection = std::hint::black_box(leaf_projection(tree));
    let ns = start.elapsed().as_nanos();
    let stats = region.change();
    (ns, stats, projection, observation)
}

fn assert_live_equivalence(
    canonical: &PaneTree,
    timeline: &PaneInteractionTimeline,
    tree: &PaneTree,
    engine_timeline: &PaneInteractionTimeline,
) {
    assert_eq!(
        canonical.to_snapshot(),
        tree.to_snapshot(),
        "full live snapshot"
    );
    assert_eq!(canonical.next_id(), tree.next_id(), "live allocator state");
    assert_eq!(
        timeline.baseline, engine_timeline.baseline,
        "retained baseline"
    );
    assert_eq!(
        timeline.entries, engine_timeline.entries,
        "retained operations"
    );
    assert_eq!(
        timeline.cursor, engine_timeline.cursor,
        "retained redo cursor"
    );
}

fn measured_pair_step(
    canonical: (&mut LiveDriver, &mut PaneTree, &mut LiveRun),
    candidate: (&mut LiveDriver, &mut PaneTree, &mut LiveRun),
    action: LiveAction<'_>,
    retention: PaneRetentionPolicy,
    origin: Instant,
    repetition: usize,
) {
    let mut projections = [None, None];
    let order = if repetition.is_multiple_of(2) {
        [0, 1]
    } else {
        [1, 0]
    };
    for index in order {
        let (driver, tree, run) = if index == 0 {
            (&mut *canonical.0, &mut *canonical.1, &mut *canonical.2)
        } else {
            (&mut *candidate.0, &mut *candidate.1, &mut *candidate.2)
        };
        let (ns, stats, projection, observation) =
            measure_live_step(driver, tree, &action, retention, origin);
        // Store the exact value passed to observe, after both measurements
        // have ended. No second clock read or reconstructed interval is used.
        if let Some(sample) = observation {
            assert_eq!(index, 1, "only the engine receives observations");
            assert!(matches!(action, LiveAction::Apply(..)));
            run.observation_tapes
                .last_mut()
                .expect("engine repetition tape")
                .samples
                .push(sample);
        }
        if repetition != 0 {
            run.phase(&action).record(ns, stats, &projection);
        }
        projections[index] = Some(projection);
    }
    assert_eq!(
        projections[0], projections[1],
        "solve and leaf projection parity"
    );
    assert_live_equivalence(
        canonical.1,
        canonical.0.timeline(),
        candidate.1,
        candidate.0.timeline(),
    );
}

fn live_benchmark_policy(mode: &str, retention: PaneRetentionPolicy) -> PaneExecutionPolicy {
    let policy = PaneExecutionPolicy::adaptive(retention);
    match mode {
        "persistent" => policy.forcing(PaneMemoryStrategy::Persistent),
        "checkpointed" => policy.forcing(PaneMemoryStrategy::Checkpointed),
        "conservative" => policy.conservative(),
        "adaptive" => policy,
        _ => unreachable!("declared live benchmark mode"),
    }
}

#[derive(Serialize)]
struct LivePairResult {
    scenario: &'static str,
    requested_mode: &'static str,
    seed: u64,
    leaf_count: usize,
    operation_count: usize,
    retention: PaneRetentionPolicy,
    canonical: LiveRunReport,
    engine: LiveRunReport,
    // No canonical migration ratio: an unchanged Timeline has no equivalent
    // substrate transition. Each direction below performs a real conversion.
    migration_to_persistent: LivePhaseReport,
    migration_to_checkpointed: LivePhaseReport,
    execution_status_by_repetition: Vec<PaneExecutionStatus>,
    final_state_hash: u64,
}

fn measure_live_migrations(
    tree: &PaneTree,
    timeline: &PaneInteractionTimeline,
    retention: PaneRetentionPolicy,
    phases: (&mut LivePhase, &mut LivePhase),
    measured: bool,
) {
    // Migration setup/import is separate from the two measured transitions;
    // the actual conversion, maintenance and solved projection are timed.
    let mut engine =
        PaneExecutionEngine::from_timeline(tree, timeline.clone()).expect("migration import");
    engine
        .set_policy(tree, live_benchmark_policy("checkpointed", retention))
        .expect("migration setup");
    let expected_projection = leaf_projection(tree);
    for (strategy, phase) in [
        (PaneMemoryStrategy::Persistent, phases.0),
        (PaneMemoryStrategy::Checkpointed, phases.1),
    ] {
        assert_ne!(
            engine.strategy(),
            strategy,
            "migration must change substrate"
        );
        let region = Region::new(GLOBAL);
        let start = Instant::now();
        engine
            .set_policy(
                tree,
                PaneExecutionPolicy::adaptive(retention).forcing(strategy),
            )
            .expect("measured migration");
        let projection = std::hint::black_box(leaf_projection(tree));
        let ns = start.elapsed().as_nanos();
        let stats = region.change();
        if measured {
            phase.record(ns, stats, &projection);
        }
        assert_eq!(engine.strategy(), strategy, "requested migration executed");
        assert_eq!(projection, expected_projection);
        assert_live_equivalence(tree, timeline, tree, engine.timeline());
        assert_eq!(
            engine.replay().expect("migrated replay").to_snapshot(),
            tree.to_snapshot()
        );
    }
    // Verify that both conversions preserved the complete retained redo tail.
    let mut expected = timeline.clone();
    let mut canonical = tree.clone();
    let mut projected = tree.clone();
    while expected
        .redo(&mut canonical)
        .expect("migration oracle redo")
    {
        assert!(engine.redo(&mut projected).expect("migrated redo"));
        assert_live_equivalence(&canonical, &expected, &projected, engine.timeline());
    }
    assert!(!engine.redo(&mut projected).expect("migrated head"));
}

fn run_live_pair(
    spec: (&'static str, usize, usize, u64),
    mode: &'static str,
    retention: PaneRetentionPolicy,
    repetitions: usize,
) -> LivePairResult {
    let (scenario, leaf_count, operation_count, seed) = spec;
    let base = grow_tree(leaf_count);
    let operations = if scenario == "mixed_session" {
        mixed_session_ops(&base, operation_count, seed)
    } else {
        resize_storm_ops(&base, operation_count, seed)
    };
    let mut canonical_metrics = LiveRun::default();
    let mut engine_metrics = LiveRun::default();
    let mut to_persistent = LivePhase::default();
    let mut to_checkpointed = LivePhase::default();
    let mut statuses = Vec::with_capacity(repetitions);
    let mut final_state_hash = None;
    for repetition in 0..=repetitions {
        let origin = Instant::now();
        engine_metrics.observation_tapes.push(LiveObservationTape {
            repetition,
            excluded_warmup: repetition == 0,
            samples: Vec::with_capacity(operation_count),
        });
        let mut canonical = base.clone();
        let mut tree = base.clone();
        let mut timeline = LiveDriver::Timeline(
            PaneInteractionTimeline::with_baseline(&canonical).with_max_entries(0),
        );
        let mut engine = PaneExecutionEngine::new(&tree);
        engine
            .set_policy(&tree, live_benchmark_policy(mode, retention))
            .expect("initial live policy");
        let mut candidate = LiveDriver::Engine(Box::new(engine));
        for (index, operation) in operations.iter().enumerate() {
            measured_pair_step(
                (&mut timeline, &mut canonical, &mut canonical_metrics),
                (&mut candidate, &mut tree, &mut engine_metrics),
                LiveAction::Apply(index as u64 + 1, operation),
                retention,
                origin,
                repetition,
            );
        }
        let head = tree.state_hash();
        if let Some(expected) = final_state_hash {
            assert_eq!(head, expected, "same history across repetitions");
        }
        final_state_hash = Some(head);
        let retained = timeline.timeline().cursor;
        assert!(retained > 0, "navigation must execute retained history");
        for index in 0..retained {
            measured_pair_step(
                (&mut timeline, &mut canonical, &mut canonical_metrics),
                (&mut candidate, &mut tree, &mut engine_metrics),
                LiveAction::Undo,
                retention,
                origin,
                repetition,
            );
            if index == retained / 2 {
                measure_live_migrations(
                    &tree,
                    candidate.timeline(),
                    retention,
                    (&mut to_persistent, &mut to_checkpointed),
                    repetition != 0,
                );
            }
        }
        assert!(!candidate.history().undo(&mut tree).expect("live baseline"));
        for _ in 0..retained {
            measured_pair_step(
                (&mut timeline, &mut canonical, &mut canonical_metrics),
                (&mut candidate, &mut tree, &mut engine_metrics),
                LiveAction::Redo,
                retention,
                origin,
                repetition,
            );
        }
        assert!(!candidate.history().redo(&mut tree).expect("live head"));
        assert_eq!(tree.state_hash(), head);
        assert_eq!(
            candidate
                .history()
                .replay()
                .expect("live replay")
                .to_snapshot(),
            canonical.to_snapshot()
        );
        let tape = engine_metrics
            .observation_tapes
            .last()
            .expect("completed engine tape");
        assert_eq!(
            tape.samples.len(),
            operation_count,
            "one sample per accepted apply"
        );
        assert!(
            tape.samples
                .windows(2)
                .all(|pair| pair[0].timestamp_ns <= pair[1].timestamp_ns),
            "recorded observation timestamps are monotonic"
        );
        for (sample, operation) in tape.samples.iter().zip(&operations) {
            assert_eq!(
                sample.local,
                operation.family() == PaneOperationFamily::Local
            );
        }
        if repetition != 0 {
            let LiveDriver::Engine(engine) = &candidate else {
                unreachable!("engine candidate")
            };
            assert_eq!(engine.status().applies, operation_count as u64);
            statuses.push(engine.status().clone());
        }
    }
    assert!(canonical_metrics.observation_tapes.is_empty());
    assert_eq!(engine_metrics.observation_tapes.len(), repetitions + 1);
    if scenario == "resize_solve_control" {
        assert_eq!(
            engine_metrics.apply.solve_successes,
            operation_count * repetitions
        );
        assert!(engine_metrics.apply.solve_errors.is_empty());
    }
    LivePairResult {
        scenario,
        requested_mode: mode,
        seed,
        leaf_count,
        operation_count,
        retention,
        canonical: canonical_metrics.report(),
        engine: engine_metrics.report(),
        migration_to_persistent: to_persistent.report(),
        migration_to_checkpointed: to_checkpointed.report(),
        execution_status_by_repetition: statuses,
        final_state_hash: final_state_hash.expect("warmup and measured histories executed"),
    }
}

#[derive(Serialize)]
struct LiveManifest {
    boundary: &'static str,
    exclusions: &'static str,
    percentile_method: &'static str,
    clock_profile_provenance: &'static str,
    repetitions: usize,
    excluded_warmup_repetitions: usize,
    viewport: (u16, u16),
    latency_envelope_ns: u64,
    os: &'static str,
    architecture: &'static str,
    host: Option<String>,
    available_parallelism: Option<usize>,
    executable: String,
    declared_build_profile: Option<String>,
    rustc_version: Option<String>,
    rustc_stderr: Option<String>,
    rustc_exit: Option<i32>,
    rustc_spawn_error: Option<String>,
    results: Vec<LivePairResult>,
}

fn run_live_benchmark(
    repetitions: usize,
    out_path: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let rustc = Command::new("rustc").arg("-Vv").output();
    let mut results = Vec::new();
    for spec in [
        ("resize_solve_control", 2, 128, 0x4444),
        ("resize_storm", 63, 512, 0x1111),
        ("resize_storm", 255, 512, 0x2222),
        ("mixed_session", 32, 256, 0x3333),
    ] {
        for units in [0, 64] {
            for mode in ["persistent", "checkpointed", "conservative", "adaptive"] {
                let result = run_live_pair(
                    spec,
                    mode,
                    PaneRetentionPolicy::bounded(0, units),
                    repetitions,
                );
                eprintln!(
                    "live scenario={} leaves={} mode={} retained_edits={} repetitions={} engine_apply_p50/p95/p99={:?}/{:?}/{:?}ns timeline={:?}/{:?}/{:?}ns solve_success={} solve_error={}",
                    spec.0,
                    spec.1,
                    mode,
                    units,
                    repetitions,
                    result.engine.apply.p50_ns,
                    result.engine.apply.p95_ns,
                    result.engine.apply.p99_ns,
                    result.canonical.apply.p50_ns,
                    result.canonical.apply.p95_ns,
                    result.canonical.apply.p99_ns,
                    result.engine.apply.measured.solve_successes,
                    result
                        .engine
                        .apply
                        .measured
                        .solve_errors
                        .values()
                        .sum::<usize>(),
                );
                results.push(result);
            }
        }
    }
    let manifest = LiveManifest {
        boundary: "owned apply + canonical projection + measured observe/maintenance + solve_layout + visual leaf rectangles; undo/redo + projection without observe, matching host callers; migration includes set_policy reconstruction/maintenance and projection",
        exclusions: "not showcase rendering, terminal IO, browser or allocator-peak proof; initial construction, migration setup/import, parity assertions, sample storage, serialization and one warmup excluded; bytes_allocated/deallocated are measured traffic, not peak memory; raw Timeline has no migration counterpart; byte budget is unbounded in this comparison; actual counters identify executed paths after monitor fallback",
        percentile_method: "nearest rank over all measured phase operations, raw samples in acquisition order; paired candidate/control order alternates by repetition",
        clock_profile_provenance: "coupled-harness observations, not production cadence: timestamp_ns uses the shared pair Instant origin for each repetition, so intervals include control execution, parity checks, sample bookkeeping and scheduling; elapsed_ns measures engine apply/record before observe/maintenance/projection. Engine tapes contain the exact samples passed to observe, in operation order, including explicitly tagged warmup; Timeline and migration helper engines receive no observations. Replaying the seeded history/policy/envelope with a tape can reproduce semantic selector decisions, not wall-clock performance",
        repetitions,
        excluded_warmup_repetitions: 1,
        viewport: (240, 80),
        latency_envelope_ns: 8_000_000,
        os: env::consts::OS,
        architecture: env::consts::ARCH,
        host: env::var("HOSTNAME").ok(),
        available_parallelism: std::thread::available_parallelism().ok().map(usize::from),
        executable: env::current_exe()?.display().to_string(),
        declared_build_profile: env::var("PANE_BENCH_BUILD_PROFILE").ok(),
        rustc_version: rustc
            .as_ref()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned()),
        rustc_stderr: rustc
            .as_ref()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stderr).into_owned()),
        rustc_exit: rustc.as_ref().ok().and_then(|output| output.status.code()),
        rustc_spawn_error: rustc.err().map(|error| error.to_string()),
        results,
    };
    let json = serde_json::to_string_pretty(&manifest)?;
    if let Some(path) = out_path {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?
            .write_all(json.as_bytes())?;
        eprintln!("wrote live benchmark: {path}");
    } else {
        println!("{json}");
    }
    Ok(())
}

fn main() {
    let mut out_path: Option<String> = None;
    let mut live = false;
    let mut live_repetitions = 20usize;
    let args: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--live" {
            live = true;
            i += 1;
        } else if args[i] == "--live-repetitions" {
            live_repetitions = args
                .get(i + 1)
                .and_then(|value| value.parse().ok())
                .filter(|value| *value > 0)
                .unwrap_or_else(|| {
                    eprintln!("--live-repetitions requires a positive integer");
                    std::process::exit(2);
                });
            i += 2;
        } else if args[i] == "--out" && i + 1 < args.len() {
            out_path = Some(args[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }
    if out_path.is_none() {
        out_path = env::var("PANE_PERSISTENT_BENCH_OUT").ok();
    }
    if live {
        if let Err(error) = run_live_benchmark(live_repetitions, out_path.as_deref()) {
            eprintln!("live benchmark failed: {error}");
            std::process::exit(1);
        }
        return;
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

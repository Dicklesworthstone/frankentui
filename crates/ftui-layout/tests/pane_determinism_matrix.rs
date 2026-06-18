//! Comprehensive cross-strategy determinism matrix for pane execution (bd-1pvzq.4).
//!
//! The pane lane ships four genuinely different ways to execute an operation
//! history, all of which must be *observationally identical*:
//!
//! 1. **Adaptive baseline** — [`PaneTree::apply_operation`] with the certified
//!    `SetSplitRatio` fast path + local-closure validation.
//! 2. **Conservative oracle** — [`PaneTree::apply_operation_conservative`], which
//!    always clones and whole-tree-validates (the ground truth fast paths must
//!    match).
//! 3. **Checkpointed replay** — [`PaneInteractionTimeline`] that records the
//!    history and rebuilds state from the nearest checkpoint via `replay()`,
//!    `undo()`, and `redo()`.
//! 4. **Persistent versions** — [`PaneVersionStore`] path-copying with `O(1)`
//!    undo/redo over structurally shared [`VersionedPaneTree`] versions.
//!
//! Where the existing `pane_operation_family_equivalence` and
//! `pane_persistent_equivalence` suites prove pairwise parity with bare
//! `assert_eq!`, this matrix adds the thing CI triage actually needs: a
//! **structured divergence diagnostic**. [`first_divergence`] compares two hash
//! traces and, on the first mismatch, yields a [`DivergenceReport`] naming the
//! offending operation index, both state hashes, the cursor, the active
//! strategy, and the operation family — emitted as a single structured JSON log
//! line so a failed CI run points straight at the first bad step
//! (bd-1pvzq.4 AC3).
//!
//! The suite is deterministic (seeded SplitMix64) and bounded so it stays cheap
//! in CI while exercising representative *and* adversarial histories.

use ftui_layout::{
    PaneExecutionPolicy, PaneId, PaneInteractionTimeline, PaneLeaf, PaneMemoryStrategy,
    PaneNodeKind, PaneOperation, PaneOperationFamily, PanePlacement, PaneRetentionOutcome,
    PaneRetentionPolicy, PaneSplitRatio, PaneStrategyReason, PaneTree, PaneVersionStore,
    PaneWorkloadProfile, SplitAxis, VersionedPaneTree, apply_retention_to_timeline,
    apply_retention_to_version_store,
};

// ===========================================================================
// Deterministic generator (mirrors the sibling equivalence suites)
// ===========================================================================

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

    fn next_u32_range(&mut self, min: u32, max: u32) -> u32 {
        let span = u64::from(max - min);
        min + (self.next_u64() % span) as u32
    }

    fn choose_index(&mut self, len: usize) -> usize {
        (self.next_u64() % len as u64) as usize
    }

    fn choose_bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}

fn leaf_ids(tree: &PaneTree) -> Vec<PaneId> {
    tree.nodes()
        .filter_map(|node| match node.kind {
            PaneNodeKind::Leaf(_) => Some(node.id),
            PaneNodeKind::Split(_) => None,
        })
        .collect()
}

fn split_ids(tree: &PaneTree) -> Vec<PaneId> {
    tree.nodes()
        .filter_map(|node| match node.kind {
            PaneNodeKind::Split(_) => Some(node.id),
            PaneNodeKind::Leaf(_) => None,
        })
        .collect()
}

fn random_ratio(rng: &mut Lcg) -> PaneSplitRatio {
    PaneSplitRatio::new(rng.next_u32_range(1, 32), rng.next_u32_range(1, 32))
        .expect("ratio bounds ensure validity")
}

fn random_axis(rng: &mut Lcg) -> SplitAxis {
    if rng.choose_bool() {
        SplitAxis::Horizontal
    } else {
        SplitAxis::Vertical
    }
}

fn random_placement(rng: &mut Lcg) -> PanePlacement {
    if rng.choose_bool() {
        PanePlacement::ExistingFirst
    } else {
        PanePlacement::IncomingFirst
    }
}

/// Bias toward `SetSplitRatio` to exercise the Local fast path, while still
/// drawing structural ops to keep the tree evolving. `resize_bias` in `[0, 100]`
/// is the probability of forcing a resize when a split exists.
fn random_operation(
    tree: &PaneTree,
    rng: &mut Lcg,
    sequence: usize,
    resize_bias: u32,
) -> PaneOperation {
    let leaves = leaf_ids(tree);
    let splits = split_ids(tree);

    if !splits.is_empty() && rng.next_u32_range(0, 100) < resize_bias {
        return PaneOperation::SetSplitRatio {
            split: splits[rng.choose_index(splits.len())],
            ratio: random_ratio(rng),
        };
    }

    let mut candidates = vec![0usize]; // NormalizeRatios
    if !leaves.is_empty() {
        candidates.push(1); // SplitLeaf
    }
    if leaves.len() > 1 {
        candidates.push(2); // CloseNode
    }
    if leaves.len() > 2 {
        candidates.push(3); // MoveSubtree
        candidates.push(4); // SwapNodes
    }
    if !splits.is_empty() {
        candidates.push(5); // SetSplitRatio
    }

    match candidates[rng.choose_index(candidates.len())] {
        1 => PaneOperation::SplitLeaf {
            target: leaves[rng.choose_index(leaves.len())],
            axis: random_axis(rng),
            ratio: random_ratio(rng),
            placement: random_placement(rng),
            new_leaf: PaneLeaf::new(format!("leaf-{sequence}")),
        },
        2 => PaneOperation::CloseNode {
            target: leaves[rng.choose_index(leaves.len())],
        },
        3 => {
            let source_idx = rng.choose_index(leaves.len());
            let mut target_idx = rng.choose_index(leaves.len());
            while target_idx == source_idx {
                target_idx = rng.choose_index(leaves.len());
            }
            PaneOperation::MoveSubtree {
                source: leaves[source_idx],
                target: leaves[target_idx],
                axis: random_axis(rng),
                ratio: random_ratio(rng),
                placement: random_placement(rng),
            }
        }
        4 => {
            let first_idx = rng.choose_index(leaves.len());
            let mut second_idx = rng.choose_index(leaves.len());
            while second_idx == first_idx {
                second_idx = rng.choose_index(leaves.len());
            }
            PaneOperation::SwapNodes {
                first: leaves[first_idx],
                second: leaves[second_idx],
            }
        }
        5 => PaneOperation::SetSplitRatio {
            split: splits[rng.choose_index(splits.len())],
            ratio: random_ratio(rng),
        },
        _ => PaneOperation::NormalizeRatios,
    }
}

fn family_label(op: &PaneOperation) -> &'static str {
    match op.family() {
        PaneOperationFamily::Local => "Local",
        PaneOperationFamily::Structural => "Structural",
    }
}

// ===========================================================================
// Structured divergence diagnostic (bd-1pvzq.4 AC3)
// ===========================================================================

/// First point at which a strategy's hash trace departs from the canonical
/// (adaptive baseline) trace. Carries everything a triage step needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DivergenceReport {
    seed: u64,
    phase: &'static str,
    strategy: &'static str,
    op_index: usize,
    cursor: usize,
    operation: String,
    family: &'static str,
    canonical_hash: u64,
    strategy_hash: u64,
}

impl DivergenceReport {
    /// Single-line structured log so CI failure output is machine-greppable.
    fn to_json_line(&self) -> String {
        format!(
            "{{\"event\":\"pane_determinism_divergence\",\"seed\":{},\"phase\":\"{}\",\
\"strategy\":\"{}\",\"op_index\":{},\"cursor\":{},\"family\":\"{}\",\
\"canonical_hash\":{},\"strategy_hash\":{},\"operation\":\"{}\"}}",
            self.seed,
            self.phase,
            self.strategy,
            self.op_index,
            self.cursor,
            self.family,
            self.canonical_hash,
            self.strategy_hash,
            json_escape(&self.operation),
        )
    }
}

impl std::fmt::Display for DivergenceReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pane determinism divergence: strategy={} phase={} seed={} op_index={} cursor={} \
family={}\n  canonical_hash={}\n  strategy_hash={}\n  operation={}",
            self.strategy,
            self.phase,
            self.seed,
            self.op_index,
            self.cursor,
            self.family,
            self.canonical_hash,
            self.strategy_hash,
            self.operation,
        )
    }
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

/// Compare a strategy's per-step hash trace against the canonical trace and
/// return the first divergence, if any. Pure and unit-tested directly so the
/// detection logic itself is covered (AC3 machinery cannot silently no-op).
///
/// `cursors[i]` is the logical cursor (applied length) the hash was sampled at;
/// `op_for_index(i)` yields a `(debug, family)` description for the step that
/// produced `hashes[i]` (or `None` for index 0 / navigation positions).
fn first_divergence<F>(
    seed: u64,
    phase: &'static str,
    strategy: &'static str,
    canonical: &[u64],
    hashes: &[u64],
    cursors: &[usize],
    op_for_index: F,
) -> Option<DivergenceReport>
where
    F: Fn(usize) -> (String, &'static str),
{
    let len = canonical.len().min(hashes.len());
    for i in 0..len {
        if canonical[i] != hashes[i] {
            let (operation, family) = op_for_index(i);
            return Some(DivergenceReport {
                seed,
                phase,
                strategy,
                op_index: i,
                cursor: cursors.get(i).copied().unwrap_or(i),
                operation,
                family,
                canonical_hash: canonical[i],
                strategy_hash: hashes[i],
            });
        }
    }
    if canonical.len() != hashes.len() {
        let i = len;
        let (operation, family) = op_for_index(i.saturating_sub(1));
        return Some(DivergenceReport {
            seed,
            phase,
            strategy,
            op_index: i,
            cursor: cursors.get(i).copied().unwrap_or(i),
            operation: format!(
                "<length mismatch: canonical={} strategy={}> {operation}",
                canonical.len(),
                hashes.len()
            ),
            family,
            canonical_hash: canonical.get(i).copied().unwrap_or(0),
            strategy_hash: hashes.get(i).copied().unwrap_or(0),
        });
    }
    None
}

/// Assert no divergence, printing the structured log line first so the JSON is
/// always visible in CI output before the panic.
fn assert_no_divergence(report: Option<DivergenceReport>) {
    if let Some(report) = report {
        eprintln!("{}", report.to_json_line());
        panic!("{report}");
    }
}

// ===========================================================================
// Lockstep execution across all four substrates
// ===========================================================================

struct Lockstep {
    seed: u64,
    operations: Vec<PaneOperation>,
    /// Hash after each applied prefix; index 0 is the empty baseline.
    baseline_hashes: Vec<u64>,
    conservative_hashes: Vec<u64>,
    persistent_hashes: Vec<u64>,
    cursors: Vec<usize>,
}

impl Lockstep {
    fn op_for_index(&self, i: usize) -> (String, &'static str) {
        if i == 0 || i > self.operations.len() {
            ("<baseline>".to_string(), "-")
        } else {
            let op = &self.operations[i - 1];
            (format!("{op:?}"), family_label(op))
        }
    }
}

/// Drive the adaptive baseline, conservative oracle, and persistent store
/// through one random history in lockstep, capturing per-step hashes.
fn run_lockstep(seed: u64, steps: usize, resize_bias: u32) -> Lockstep {
    let mut baseline = PaneTree::singleton("root");
    let mut conservative = PaneTree::singleton("root");
    let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
    let mut rng = Lcg::new(seed);

    let mut operations = Vec::with_capacity(steps);
    let mut baseline_hashes = vec![baseline.state_hash()];
    let mut conservative_hashes = vec![conservative.state_hash()];
    let mut persistent_hashes = vec![store.current().state_hash().expect("flatten baseline")];
    let mut cursors = vec![0usize];

    for step in 0..steps {
        let op = random_operation(&baseline, &mut rng, step, resize_bias);
        baseline
            .apply_operation((step as u64) + 1, op.clone())
            .expect("baseline apply");
        conservative
            .apply_operation_conservative((step as u64) + 1, op.clone())
            .expect("conservative apply");
        store.apply(&op).expect("persistent apply");

        operations.push(op);
        baseline_hashes.push(baseline.state_hash());
        conservative_hashes.push(conservative.state_hash());
        persistent_hashes.push(store.current().state_hash().expect("flatten"));
        cursors.push(step + 1);
    }

    Lockstep {
        seed,
        operations,
        baseline_hashes,
        conservative_hashes,
        persistent_hashes,
        cursors,
    }
}

// ===========================================================================
// Integration matrix: apply-phase equivalence (AC2)
// ===========================================================================

#[test]
fn matrix_apply_equivalence_across_strategies_and_families() {
    // Representative histories with a moderate resize bias so every family and
    // both the Local fast path and structural rebuild paths are exercised.
    for seed in 0..64u64 {
        let run = run_lockstep(seed, 40, 45);

        assert_no_divergence(first_divergence(
            seed,
            "apply",
            "conservative",
            &run.baseline_hashes,
            &run.conservative_hashes,
            &run.cursors,
            |i| run.op_for_index(i),
        ));
        assert_no_divergence(first_divergence(
            seed,
            "apply",
            "persistent",
            &run.baseline_hashes,
            &run.persistent_hashes,
            &run.cursors,
            |i| run.op_for_index(i),
        ));
    }
}

#[test]
fn matrix_resize_dominated_histories_keep_fast_path_parity() {
    // Resize-storm histories (90% Local) stress the certified fast path against
    // the conservative oracle and the persistent path.
    for seed in 0..48u64 {
        let run = run_lockstep(seed ^ 0x5151_5151, 64, 90);
        // Confirm the history really is resize-dominated.
        let local = run
            .operations
            .iter()
            .filter(|op| op.family() == PaneOperationFamily::Local)
            .count();
        assert!(
            local * 100 >= run.operations.len() * 60,
            "seed={seed}: expected resize-dominated history, got {local}/{} Local",
            run.operations.len()
        );

        assert_no_divergence(first_divergence(
            run.seed,
            "apply",
            "conservative",
            &run.baseline_hashes,
            &run.conservative_hashes,
            &run.cursors,
            |i| run.op_for_index(i),
        ));
        assert_no_divergence(first_divergence(
            run.seed,
            "apply",
            "persistent",
            &run.baseline_hashes,
            &run.persistent_hashes,
            &run.cursors,
            |i| run.op_for_index(i),
        ));
    }
}

// ===========================================================================
// Integration matrix: checkpointed replay + undo/redo navigation (AC2)
// ===========================================================================

#[test]
fn matrix_navigation_equivalence_undo_redo_across_strategies() {
    for seed in 0..32u64 {
        let mut canonical = PaneTree::singleton("root");
        let mut timeline = PaneInteractionTimeline::with_baseline(&canonical);
        let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
        let mut rng = Lcg::new(seed ^ 0xC0FF_EE00);

        let steps = 40usize;
        let mut operations = Vec::with_capacity(steps);
        // expected[applied_len] == canonical state hash at that cursor.
        let mut expected = vec![canonical.state_hash()];

        for step in 0..steps {
            let op = random_operation(&canonical, &mut rng, step, 40);
            timeline
                .apply_and_record(&mut canonical, step as u64, (step as u64) + 1, op.clone())
                .expect("timeline apply");
            store.apply(&op).expect("store apply");
            operations.push(op);
            expected.push(canonical.state_hash());
        }

        // Checkpointed replay from the nearest checkpoint must rebuild the head.
        let replayed = timeline.replay().expect("timeline replay");
        assert_eq!(
            replayed.state_hash(),
            *expected.last().expect("head present"),
            "checkpointed replay head diverged at seed={seed}"
        );

        // Walk undo to baseline, then redo to head, capturing both strategies'
        // hash traces at every cursor and diffing against the canonical trace.
        let mut timeline_undo_trace = vec![*expected.last().expect("head")];
        let mut store_undo_trace = vec![store.current().state_hash().expect("flatten head")];
        let mut undo_cursors = vec![steps];
        let mut applied = steps;
        while applied > 0 {
            let timeline_moved = timeline.undo(&mut canonical).expect("timeline undo");
            let store_moved = store.undo();
            assert_eq!(
                timeline_moved, store_moved,
                "undo availability diverged seed={seed}"
            );
            applied -= 1;
            timeline_undo_trace.push(canonical.state_hash());
            store_undo_trace.push(store.current().state_hash().expect("flatten"));
            undo_cursors.push(applied);
        }
        assert!(!timeline.undo(&mut canonical).expect("no-op undo"));
        assert!(!store.undo());

        // Canonical trace for the undo walk is expected[] read high→low.
        let undo_canonical: Vec<u64> = (0..=steps).rev().map(|i| expected[i]).collect();
        let nav_ops = |_i: usize| ("<undo>".to_string(), "-");
        assert_no_divergence(first_divergence(
            seed,
            "undo",
            "checkpointed",
            &undo_canonical,
            &timeline_undo_trace,
            &undo_cursors,
            nav_ops,
        ));
        assert_no_divergence(first_divergence(
            seed,
            "undo",
            "persistent",
            &undo_canonical,
            &store_undo_trace,
            &undo_cursors,
            nav_ops,
        ));

        // Redo back to head.
        let mut timeline_redo_trace = vec![canonical.state_hash()];
        let mut store_redo_trace = vec![store.current().state_hash().expect("flatten base")];
        let mut redo_cursors = vec![0usize];
        while applied < steps {
            assert!(timeline.redo(&mut canonical).expect("timeline redo"));
            assert!(store.redo());
            applied += 1;
            timeline_redo_trace.push(canonical.state_hash());
            store_redo_trace.push(store.current().state_hash().expect("flatten"));
            redo_cursors.push(applied);
        }
        assert!(!store.redo());

        let redo_canonical: Vec<u64> = (0..=steps).map(|i| expected[i]).collect();
        let redo_ops = |_i: usize| ("<redo>".to_string(), "-");
        assert_no_divergence(first_divergence(
            seed,
            "redo",
            "checkpointed",
            &redo_canonical,
            &timeline_redo_trace,
            &redo_cursors,
            redo_ops,
        ));
        assert_no_divergence(first_divergence(
            seed,
            "redo",
            "persistent",
            &redo_canonical,
            &store_redo_trace,
            &redo_cursors,
            redo_ops,
        ));
    }
}

// ===========================================================================
// Integration matrix: selector-routed execution never changes behavior (AC2)
// ===========================================================================

#[test]
fn matrix_selector_routed_execution_never_diverges() {
    // Whatever strategy the execution policy picks for a workload, executing the
    // history under that strategy must equal the adaptive baseline. We verify
    // this across the adaptive policy and every forced policy, on both mixed and
    // resize-dominated histories.
    let policies = [
        (
            "adaptive",
            PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded()),
        ),
        (
            "conservative",
            PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded()).conservative(),
        ),
        (
            "force-baseline",
            PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded())
                .forcing(PaneMemoryStrategy::Baseline),
        ),
        (
            "force-persistent",
            PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded())
                .forcing(PaneMemoryStrategy::Persistent),
        ),
    ];

    for (history_label, resize_bias) in [("mixed", 45u32), ("resize", 90u32)] {
        for seed in 0..24u64 {
            let run = run_lockstep(seed ^ 0xDEC1_5104, 48, resize_bias);
            let profile = PaneWorkloadProfile::observe(&run.operations, 120, true);

            for (policy_label, policy) in &policies {
                let decision = policy.select(profile);
                // The selected strategy's trace must equal the baseline trace.
                let strategy_trace = match decision.strategy {
                    PaneMemoryStrategy::Baseline => &run.baseline_hashes,
                    PaneMemoryStrategy::Checkpointed => &run.conservative_hashes,
                    PaneMemoryStrategy::Persistent => &run.persistent_hashes,
                };
                assert_no_divergence(first_divergence(
                    run.seed,
                    "selector",
                    policy_label,
                    &run.baseline_hashes,
                    strategy_trace,
                    &run.cursors,
                    |i| run.op_for_index(i),
                ));
                // Selection is deterministic for a fixed profile + policy.
                assert_eq!(
                    decision.strategy,
                    policy.select(profile).strategy,
                    "selector non-deterministic: policy={policy_label} history={history_label} seed={seed}"
                );
            }
        }
    }
}

// ===========================================================================
// Integration matrix: retention preserves head state across strategies (AC1)
// ===========================================================================

#[test]
fn matrix_retention_preserves_head_state_across_strategies() {
    // Apply a history, then prune both the persistent store and the checkpointed
    // timeline under a tight budget. Retention may drop *history*, but the head
    // (current) state must survive byte-for-byte and still equal the canonical
    // tree.
    for seed in 0..32u64 {
        let mut canonical = PaneTree::singleton("root");
        let mut timeline = PaneInteractionTimeline::with_baseline(&canonical);
        let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
        let mut rng = Lcg::new(seed ^ 0x12AB_34CD);

        for step in 0..48usize {
            let op = random_operation(&canonical, &mut rng, step, 50);
            timeline
                .apply_and_record(&mut canonical, step as u64, (step as u64) + 1, op.clone())
                .expect("timeline apply");
            store.apply(&op).expect("store apply");
        }

        let head_hash = canonical.state_hash();
        assert_eq!(store.current().state_hash().expect("flatten"), head_hash);

        // Tight budgets: a handful of units / a few KiB. These force pruning.
        let policy = PaneRetentionPolicy::bounded(4096, 4);

        let store_decision = apply_retention_to_version_store(&mut store, &policy);
        assert_eq!(
            store_decision.current_state_hash, head_hash,
            "persistent retention changed head hash at seed={seed}: {}",
            store_decision.log
        );
        assert_eq!(
            store.current().state_hash().expect("flatten after prune"),
            head_hash,
            "persistent head state lost after pruning at seed={seed}"
        );
        assert!(
            matches!(
                store_decision.outcome,
                PaneRetentionOutcome::WithinBudget
                    | PaneRetentionOutcome::PrunedToFit
                    | PaneRetentionOutcome::ConservativeHold
                    | PaneRetentionOutcome::FloorReached
            ),
            "unexpected store retention outcome at seed={seed}: {:?}",
            store_decision.outcome
        );

        let timeline_decision = apply_retention_to_timeline(&mut timeline, &policy);
        assert_eq!(
            timeline_decision.current_state_hash, head_hash,
            "timeline retention changed head hash at seed={seed}: {}",
            timeline_decision.log
        );
        // After pruning, replay must still rebuild the canonical head.
        let replayed = timeline.replay().expect("replay after prune");
        assert_eq!(
            replayed.state_hash(),
            head_hash,
            "timeline replay head lost after pruning at seed={seed}"
        );
    }
}

// ===========================================================================
// Adversarial histories (AC2)
// ===========================================================================

#[test]
fn matrix_rebuild_fallback_heavy_histories_keep_parity() {
    // MoveSubtree + NormalizeRatios take the persistent rebuild-fallback path
    // (no structural sharing). Hammer them and confirm parity still holds.
    for seed in 0..32u64 {
        let mut baseline = PaneTree::singleton("root");
        let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
        let mut rng = Lcg::new(seed ^ 0xFEED_BEEF);

        let mut operations = Vec::new();
        let mut baseline_hashes = vec![baseline.state_hash()];
        let mut persistent_hashes = vec![store.current().state_hash().expect("flatten")];
        let mut cursors = vec![0usize];

        // Seed a few leaves so MoveSubtree/SwapNodes are available.
        for step in 0..48usize {
            let leaves = leaf_ids(&baseline);
            let op = if leaves.len() < 4 {
                // Grow first.
                PaneOperation::SplitLeaf {
                    target: leaves[rng.choose_index(leaves.len())],
                    axis: random_axis(&mut rng),
                    ratio: random_ratio(&mut rng),
                    placement: random_placement(&mut rng),
                    new_leaf: PaneLeaf::new(format!("leaf-{step}")),
                }
            } else if rng.choose_bool() {
                PaneOperation::NormalizeRatios
            } else {
                let source_idx = rng.choose_index(leaves.len());
                let mut target_idx = rng.choose_index(leaves.len());
                while target_idx == source_idx {
                    target_idx = rng.choose_index(leaves.len());
                }
                PaneOperation::MoveSubtree {
                    source: leaves[source_idx],
                    target: leaves[target_idx],
                    axis: random_axis(&mut rng),
                    ratio: random_ratio(&mut rng),
                    placement: random_placement(&mut rng),
                }
            };
            baseline
                .apply_operation((step as u64) + 1, op.clone())
                .expect("baseline apply");
            store.apply(&op).expect("store apply");
            operations.push(op);
            baseline_hashes.push(baseline.state_hash());
            persistent_hashes.push(store.current().state_hash().expect("flatten"));
            cursors.push(step + 1);
        }

        assert_no_divergence(first_divergence(
            seed,
            "apply",
            "persistent",
            &baseline_hashes,
            &persistent_hashes,
            &cursors,
            |i| {
                if i == 0 || i > operations.len() {
                    ("<baseline>".to_string(), "-")
                } else {
                    let op = &operations[i - 1];
                    (format!("{op:?}"), family_label(op))
                }
            },
        ));
    }
}

#[test]
fn matrix_illegal_operations_reject_identically() {
    // Both the adaptive and conservative paths and the persistent store must
    // reject the same illegal operations without mutating state.
    let mut baseline = PaneTree::singleton("root");
    let mut conservative = PaneTree::singleton("root");
    let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
    let mut rng = Lcg::new(0x0BAD_F00D);
    for step in 0..24usize {
        let op = random_operation(&baseline, &mut rng, step, 40);
        baseline
            .apply_operation((step as u64) + 1, op.clone())
            .expect("apply");
        conservative
            .apply_operation_conservative((step as u64) + 1, op.clone())
            .expect("apply");
        store.apply(&op).expect("apply");
    }

    let head_hash = baseline.state_hash();
    let missing = baseline.to_snapshot().next_id;
    let leaf = leaf_ids(&baseline)[0];
    let ratio = PaneSplitRatio::new(2, 1).expect("ratio");
    let illegal = [
        PaneOperation::SetSplitRatio {
            split: missing,
            ratio,
        },
        PaneOperation::SetSplitRatio { split: leaf, ratio },
        PaneOperation::CloseNode { target: missing },
    ];

    for (idx, op) in illegal.iter().enumerate() {
        let baseline_err = baseline
            .apply_operation(1000 + idx as u64, op.clone())
            .is_err();
        let conservative_err = conservative
            .apply_operation_conservative(1000 + idx as u64, op.clone())
            .is_err();
        let store_err = store.apply(op).is_err();
        assert!(
            baseline_err && conservative_err && store_err,
            "reject parity broke for illegal op {idx}: baseline_err={baseline_err} \
conservative_err={conservative_err} store_err={store_err}"
        );
        // State unchanged on every substrate.
        assert_eq!(
            baseline.state_hash(),
            head_hash,
            "baseline mutated on reject {idx}"
        );
        assert_eq!(
            conservative.state_hash(),
            head_hash,
            "conservative mutated on reject {idx}"
        );
        assert_eq!(
            store.current().state_hash().expect("flatten"),
            head_hash,
            "store mutated on reject {idx}"
        );
    }
}

// ===========================================================================
// Unit tests: classifier / selector / checkpoint spacing / fallbacks (AC1)
// ===========================================================================

#[test]
fn unit_operation_family_classifier_is_correct() {
    let ratio = PaneSplitRatio::new(1, 1).expect("ratio");
    let id = PaneTree::singleton("root").root();
    assert_eq!(
        PaneOperation::SetSplitRatio { split: id, ratio }.family(),
        PaneOperationFamily::Local
    );
    for structural in [
        PaneOperation::SplitLeaf {
            target: id,
            axis: SplitAxis::Horizontal,
            ratio,
            placement: PanePlacement::ExistingFirst,
            new_leaf: PaneLeaf::new("x"),
        },
        PaneOperation::CloseNode { target: id },
        PaneOperation::MoveSubtree {
            source: id,
            target: id,
            axis: SplitAxis::Vertical,
            ratio,
            placement: PanePlacement::IncomingFirst,
        },
        PaneOperation::SwapNodes {
            first: id,
            second: id,
        },
        PaneOperation::NormalizeRatios,
    ] {
        assert_eq!(
            structural.family(),
            PaneOperationFamily::Structural,
            "expected Structural for {structural:?}"
        );
    }
}

#[test]
fn unit_selector_is_deterministic_with_fallback_triggers() {
    let policy = PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded());

    // No history required -> baseline fallback.
    let no_history = PaneWorkloadProfile::observe(&[], 0, false);
    let d = policy.select(no_history);
    assert_eq!(d.strategy, PaneMemoryStrategy::Baseline);
    assert_eq!(d.reason, PaneStrategyReason::NoHistoryRequired);
    // Determinism.
    assert_eq!(policy.select(no_history).strategy, d.strategy);

    // Forced override beats adaptation.
    let forced = policy.forcing(PaneMemoryStrategy::Persistent);
    let mixed = PaneWorkloadProfile::observe(&[PaneOperation::NormalizeRatios], 10, true);
    let fd = forced.select(mixed);
    assert_eq!(fd.strategy, PaneMemoryStrategy::Persistent);
    assert!(fd.forced);
    assert_eq!(fd.reason, PaneStrategyReason::ForcedOverride);

    // Conservative override forces the checkpointed certified path.
    let conservative = policy.conservative();
    let cd = conservative.select(mixed);
    assert_eq!(cd.strategy, PaneMemoryStrategy::Checkpointed);
    assert_eq!(cd.reason, PaneStrategyReason::ConservativeFallback);

    // A resize-dominated bursty deep history favors the persistent path. The
    // split id only needs to classify as Local (SetSplitRatio); validity is
    // irrelevant to PaneWorkloadProfile::observe.
    let any_id = PaneTree::singleton("x").root();
    let resize_ops: Vec<PaneOperation> = (0..128)
        .map(|i| PaneOperation::SetSplitRatio {
            split: any_id,
            ratio: PaneSplitRatio::new(1 + (i % 3) as u32, 2).expect("ratio"),
        })
        .collect();
    let storm = PaneWorkloadProfile::observe(&resize_ops, 240, true);
    let sd = policy.select(storm);
    assert_eq!(sd.strategy, PaneMemoryStrategy::Persistent);
    assert_eq!(
        policy.select(storm).strategy,
        sd.strategy,
        "selector non-deterministic"
    );
}

#[test]
fn unit_checkpoint_spacing_responds_to_cost_ratio() {
    // Cheaper snapshots relative to replay steps -> checkpoint more often
    // (smaller interval); expensive snapshots -> larger interval. Deterministic.
    let frequent = PaneInteractionTimeline::checkpoint_decision(1_000, 10_000);
    let sparse = PaneInteractionTimeline::checkpoint_decision(1_000_000, 100);
    assert!(frequent.checkpoint_interval >= 1, "interval must be >= 1");
    assert!(
        frequent.checkpoint_interval <= sparse.checkpoint_interval,
        "expected cheaper snapshots to checkpoint at least as often: frequent={} sparse={}",
        frequent.checkpoint_interval,
        sparse.checkpoint_interval
    );
    assert_eq!(
        frequent.checkpoint_interval,
        PaneInteractionTimeline::checkpoint_decision(1_000, 10_000).checkpoint_interval,
        "checkpoint decision must be deterministic"
    );
}

// ===========================================================================
// Meta-test: the divergence detector itself must work (AC3)
// ===========================================================================

#[test]
fn divergence_detector_reports_first_mismatch() {
    // No divergence on equal traces.
    let canonical = [1u64, 2, 3, 4];
    let same = [1u64, 2, 3, 4];
    assert!(
        first_divergence(7, "apply", "s", &canonical, &same, &[0, 1, 2, 3], |_| (
            "op".into(),
            "Local"
        ))
        .is_none()
    );

    // First mismatch at index 2 is reported with full context.
    let diverging = [1u64, 2, 99, 4];
    let report = first_divergence(
        7,
        "apply",
        "persistent",
        &canonical,
        &diverging,
        &[0, 1, 2, 3],
        |i| {
            (
                format!("op#{i}"),
                if i == 2 { "Structural" } else { "Local" },
            )
        },
    )
    .expect("divergence at index 2 must be detected");
    assert_eq!(report.op_index, 2);
    assert_eq!(report.cursor, 2);
    assert_eq!(report.strategy, "persistent");
    assert_eq!(report.canonical_hash, 3);
    assert_eq!(report.strategy_hash, 99);
    assert_eq!(report.family, "Structural");

    // Structured log line is valid, greppable JSON carrying the key fields.
    let line = report.to_json_line();
    assert!(line.starts_with('{') && line.ends_with('}'));
    assert!(line.contains("\"event\":\"pane_determinism_divergence\""));
    assert!(line.contains("\"op_index\":2"));
    assert!(line.contains("\"strategy\":\"persistent\""));
    assert!(line.contains("\"canonical_hash\":3"));
    assert!(line.contains("\"strategy_hash\":99"));

    // Length mismatch is also flagged.
    let short = [1u64, 2];
    let report = first_divergence(
        7,
        "apply",
        "persistent",
        &canonical,
        &short,
        &[0, 1, 2, 3],
        |_| ("op".into(), "-"),
    )
    .expect("length mismatch must be detected");
    assert_eq!(report.op_index, 2);
    assert!(report.operation.contains("length mismatch"));

    // JSON escaping survives quotes/backslashes in the operation debug string.
    let escaped = json_escape("leaf \"a\\b\"");
    assert_eq!(escaped, "leaf \\\"a\\\\b\\\"");
}

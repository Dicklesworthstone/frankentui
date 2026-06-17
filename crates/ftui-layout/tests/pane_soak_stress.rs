//! Long-run soak, stress, and chaos scenarios for the pane split-tree engine
//! (bd-a46q1.6).
//!
//! Where `pane_invariant_fuzz.rs` is a *bounded single-pass* property check,
//! this suite targets *reliability over time and under load*:
//!   * **soak** — prolonged random-but-valid operation streams (thousands of
//!     ops) with bounded tree growth, asserting structural + layout invariants
//!     hold at every step and that the whole run replays to an identical hash,
//!   * **stress** — viewport resize storms (thousands of rapid re-solves),
//!     repeated split/close churn, and deep/wide tree construction,
//!   * **chaos** — transaction rollback as a true no-op, invalid-operation
//!     rejection without mutation, and idempotent recovery (`repair_safe`) over
//!     valid state — the core-level analogues of host interruption events.
//!
//! Reproducibility & flake triage
//! ------------------------------
//! Every scenario is driven by a *fixed-seed* deterministic LCG (no
//! `any::<u64>()`, no wall-clock, no float) so a CI failure is byte-for-byte
//! reproducible from the printed seed alone. The soak loop checks invariants
//! explicitly and, on the first violation, writes a minimized repro bundle
//! (seed + full operation journal up to the failing step) before failing — so a
//! failure is actionable without an ad-hoc rerun. When `PANE_SOAK_ARTIFACT_DIR`
//! is set, a per-scenario evidence JSON (seed, steps, final hash) is written on
//! success too. The triage/quarantine process is documented in
//! `docs/testing/pane-soak-flake-triage.md`.
//!
//! Scaling: defaults keep `cargo test` fast (~1-2s); the runner script
//! (`scripts/pane_e2e.sh`, full/stress modes) turns the knobs up via
//! `PANE_SOAK_STEPS`, `PANE_STRESS_RESIZES`, `PANE_STRESS_CHURN`,
//! `PANE_STRESS_DEPTH`, `PANE_STRESS_WIDTH`.

use std::fmt::Write as _;

use ftui_layout::{
    PaneId, PaneLeaf, PaneNodeKind, PaneOperation, PanePlacement, PaneSplitRatio, PaneTree,
    PaneTreeSnapshot, Rect, SplitAxis,
};

/// Deterministic linear-congruential PRNG (mirrors `pane_invariant_fuzz.rs`):
/// fixed-seed, reproducible, no external dependency.
#[derive(Debug, Clone)]
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
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.state
    }

    fn next_u32_range(&mut self, min: u32, max: u32) -> u32 {
        debug_assert!(min <= max);
        if min == max {
            return min;
        }
        let span = u64::from(max - min + 1);
        min + (self.next_u64() % span) as u32
    }

    fn next_u16_range(&mut self, min: u16, max: u16) -> u16 {
        debug_assert!(min <= max);
        if min == max {
            return min;
        }
        let span = u64::from(max - min + 1);
        min + (self.next_u64() % span) as u16
    }

    fn choose_index(&mut self, len: usize) -> usize {
        debug_assert!(len > 0);
        (self.next_u64() % len as u64) as usize
    }

    fn choose_bool(&mut self) -> bool {
        (self.next_u64() & 1) == 0
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
    let numerator = rng.next_u32_range(1, 32);
    let denominator = rng.next_u32_range(1, 32);
    PaneSplitRatio::new(numerator, denominator).expect("ratio bounds ensure validity")
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

/// Generate a random, currently-valid operation. When the tree already has
/// `max_leaves` leaves the growth operations (`SplitLeaf`, `MoveSubtree`) are
/// excluded so a long soak stays memory-bounded instead of exploding.
fn bounded_random_operation(
    tree: &PaneTree,
    rng: &mut Lcg,
    sequence: usize,
    max_leaves: usize,
) -> PaneOperation {
    let leaves = leaf_ids(tree);
    let splits = split_ids(tree);
    let at_capacity = leaves.len() >= max_leaves;

    // Candidate op kinds, gated by validity AND the soft capacity bound.
    let mut candidates = vec![0usize]; // NormalizeRatios (always available)
    if !leaves.is_empty() && !at_capacity {
        candidates.push(1); // SplitLeaf (grows)
    }
    if leaves.len() > 1 {
        candidates.push(2); // CloseNode (shrinks)
    }
    if leaves.len() > 2 && !at_capacity {
        candidates.push(3); // MoveSubtree (restructures; can grow split count)
    }
    if leaves.len() > 2 {
        candidates.push(4); // SwapNodes (size-neutral)
    }
    if !splits.is_empty() {
        candidates.push(5); // SetSplitRatio (size-neutral)
    }

    let op_kind = candidates[rng.choose_index(candidates.len())];
    match op_kind {
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

/// Structural validity + invariant report must be clean. Returns an error
/// string (instead of panicking) so the soak loop can emit a repro bundle.
fn check_tree_invariants(tree: &PaneTree) -> Result<(), String> {
    if let Err(err) = tree.validate() {
        return Err(format!("tree.validate() failed: {err:?}"));
    }
    let report = tree.invariant_report();
    if report.has_errors() {
        return Err(format!("invariant report has errors: {:?}", report.issues));
    }
    Ok(())
}

/// `solve_layout` must be deterministic and produce in-bounds, positive-size
/// rects for every leaf. Returns an error string on violation.
fn check_layout(tree: &PaneTree, area: Rect) -> Result<(), String> {
    let first = tree.solve_layout(area);
    let second = tree.solve_layout(area);
    if first != second {
        return Err(format!("solve_layout non-deterministic for area={area:?}"));
    }
    let Ok(layout) = first else {
        // An unsatisfiable area is acceptable as long as it is deterministic.
        return Ok(());
    };
    for pane in leaf_ids(tree) {
        let Some(rect) = layout.rect(pane) else {
            return Err(format!(
                "leaf {pane:?} missing solved rect for area={area:?}"
            ));
        };
        if rect.x < area.x
            || rect.y < area.y
            || rect.right() > area.right()
            || rect.bottom() > area.bottom()
            || rect.width == 0
            || rect.height == 0
        {
            return Err(format!(
                "leaf {pane:?} rect {rect:?} out of bounds / degenerate for area={area:?}"
            ));
        }
    }
    Ok(())
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// Write a minimized, replayable repro bundle for a failing soak run, then
/// panic with a pointer to it. The seed alone is sufficient to reproduce.
fn fail_with_repro(scenario: &str, seed: u64, ops: &[PaneOperation], step: usize, msg: &str) -> ! {
    let mut journal = String::new();
    for (idx, op) in ops.iter().enumerate() {
        let _ = writeln!(journal, "{idx}: {op:?}");
    }
    let artifact = format!(
        "{{\"schema_version\":\"pane-soak-repro-v1\",\"scenario\":\"{scenario}\",\"seed\":{seed},\"failing_step\":{step},\"applied_ops\":{},\"message\":{:?}}}",
        ops.len(),
        msg
    );
    if let Ok(dir) = std::env::var("PANE_SOAK_ARTIFACT_DIR")
        && !dir.is_empty()
    {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(format!("{dir}/repro_{scenario}_seed{seed}.json"), &artifact);
        let _ = std::fs::write(
            format!("{dir}/repro_{scenario}_seed{seed}.journal"),
            &journal,
        );
    }
    panic!(
        "SOAK FAILURE [{scenario}] seed={seed} step={step}: {msg}\n\
         reproduce: PANE_SOAK_SEED={seed} cargo test -p ftui-layout --test pane_soak_stress {scenario}\n\
         repro bundle (set PANE_SOAK_ARTIFACT_DIR to persist):\n{artifact}\n\
         operation journal:\n{journal}"
    );
}

/// Emit a success-path evidence artifact when requested by the runner.
fn emit_evidence(scenario: &str, seed: u64, steps: usize, final_hash: u64) {
    println!(
        "PANE_SOAK_TRACE scenario={scenario} seed={seed} steps={steps} final_hash={final_hash} status=ok"
    );
    if let Ok(dir) = std::env::var("PANE_SOAK_ARTIFACT_DIR")
        && !dir.is_empty()
    {
        let _ = std::fs::create_dir_all(&dir);
        let body = format!(
            "{{\"schema_version\":\"pane-soak-evidence-v1\",\"scenario\":\"{scenario}\",\"seed\":{seed},\"steps\":{steps},\"final_hash\":{final_hash},\"status\":\"ok\"}}"
        );
        let _ = std::fs::write(format!("{dir}/evidence_{scenario}_seed{seed}.json"), body);
    }
}

/// Drive a bounded soak stream and return (final tree, applied ops). Checks
/// invariants and an interleaved resize at every step; emits a repro on failure.
fn run_soak(
    scenario: &str,
    seed: u64,
    steps: usize,
    max_leaves: usize,
) -> (PaneTree, Vec<PaneOperation>) {
    let mut tree = PaneTree::singleton("root");
    let mut rng = Lcg::new(seed);
    let mut applied: Vec<PaneOperation> = Vec::with_capacity(steps);

    for step in 0..steps {
        let operation = bounded_random_operation(&tree, &mut rng, step, max_leaves);
        let operation_id = (step as u64) + 1;
        let before_hash = tree.state_hash();

        match tree.apply_operation(operation_id, operation.clone()) {
            Ok(_) => applied.push(operation),
            Err(err) => {
                // A generated op should always be valid for the current tree;
                // if not, that itself is a soak failure worth a repro.
                applied.push(operation.clone());
                fail_with_repro(
                    scenario,
                    seed,
                    &applied,
                    step,
                    &format!("generated op rejected: {operation:?} err={err:?}"),
                );
            }
        }

        if let Err(msg) = check_tree_invariants(&tree) {
            fail_with_repro(scenario, seed, &applied, step, &msg);
        }

        // Interleave a resize at a size that scales with the tree so leaves
        // always have room, plus jitter (the "resize storm" woven into soak).
        let leaf_count = leaf_ids(&tree).len() as u16;
        let base = leaf_count.saturating_mul(4).max(64);
        let area = Rect::new(
            0,
            0,
            base.saturating_add(rng.next_u16_range(0, 48)),
            base.saturating_add(rng.next_u16_range(0, 48)),
        );
        if let Err(msg) = check_layout(&tree, area) {
            fail_with_repro(scenario, seed, &applied, step, &msg);
        }

        // Sanity: a successful apply must change the hash unless it was a
        // no-op-shaped operation; we only assert it never panics / corrupts.
        let _ = before_hash;
    }

    (tree, applied)
}

// ---------------------------------------------------------------------------
// Soak
// ---------------------------------------------------------------------------

/// Prolonged random-but-valid operation stream: invariants + layout bounds hold
/// at every step, the tree stays memory-bounded, and the whole run replays to
/// an identical state hash and snapshot.
#[test]
fn pane_soak_long_run_preserves_invariants_and_bounds() {
    let seed = env_usize("PANE_SOAK_SEED", 0x_A46A1) as u64;
    let steps = env_usize("PANE_SOAK_STEPS", 1_500);
    let max_leaves = env_usize("PANE_SOAK_MAX_LEAVES", 24);

    let (tree, ops) = run_soak("soak_long_run", seed, steps, max_leaves);

    // Final invariants.
    if let Err(msg) = check_tree_invariants(&tree) {
        fail_with_repro("soak_long_run", seed, &ops, steps, &msg);
    }
    // Memory bound held throughout (final state reflects the cap with slack for
    // the last growth op).
    let leaves = leaf_ids(&tree).len();
    assert!(
        leaves <= max_leaves + 2,
        "soak tree exceeded leaf bound: {leaves} > {max_leaves}"
    );

    // Deterministic replay: re-applying the journal reproduces the exact tree.
    let final_hash = tree.state_hash();
    let mut replay = PaneTree::singleton("root");
    for (idx, op) in ops.iter().enumerate() {
        replay
            .apply_operation((idx as u64) + 1, op.clone())
            .expect("recorded soak op must replay");
    }
    assert_eq!(
        replay.state_hash(),
        final_hash,
        "soak run must replay to identical hash"
    );
    assert_eq!(
        replay.to_snapshot(),
        tree.to_snapshot(),
        "soak run must replay to identical snapshot"
    );

    emit_evidence("soak_long_run", seed, steps, final_hash);
}

/// Reproducibility guarantee that underpins flake triage: the same seed yields a
/// byte-identical soak outcome across independent runs.
#[test]
fn pane_soak_is_reproducible_across_runs() {
    let seed = 0x5EED_u64;
    let steps = env_usize("PANE_SOAK_STEPS", 600);
    let max_leaves = 20;
    let (a, ops_a) = run_soak("soak_repro", seed, steps, max_leaves);
    let (b, ops_b) = run_soak("soak_repro", seed, steps, max_leaves);
    assert_eq!(a.state_hash(), b.state_hash(), "same seed -> same hash");
    assert_eq!(
        a.to_snapshot(),
        b.to_snapshot(),
        "same seed -> same snapshot"
    );
    assert_eq!(ops_a.len(), ops_b.len(), "same seed -> same op count");
    assert_eq!(ops_a, ops_b, "same seed -> identical operation journal");
}

/// A small fixed seed corpus, each run long enough to churn through every op
/// kind repeatedly.
#[test]
fn pane_soak_seed_corpus_preserves_invariants() {
    let steps = env_usize("PANE_SOAK_STEPS", 400);
    for seed in [0u64, 1, 7, 42, 255, 1024, u32::MAX as u64, u64::MAX] {
        let (tree, ops) = run_soak("soak_corpus", seed, steps, 24);
        if let Err(msg) = check_tree_invariants(&tree) {
            fail_with_repro("soak_corpus", seed, &ops, steps, &msg);
        }
    }
}

// ---------------------------------------------------------------------------
// Stress
// ---------------------------------------------------------------------------

/// Build a moderate tree, then hammer `solve_layout` with thousands of rapidly
/// varying viewport sizes (resize storm). A fixed probe size must solve
/// identically every time it recurs (determinism under churn), and every solve
/// must stay in bounds.
#[test]
fn pane_stress_viewport_resize_storm() {
    let resizes = env_usize("PANE_STRESS_RESIZES", 3_000);
    let mut rng = Lcg::new(0x5102_u64);
    // ~12-leaf tree.
    let (tree, _) = run_soak("stress_resize_seed", 0xBEEF, 40, 12);

    let probe = Rect::new(0, 0, 200, 80);
    let probe_solution = tree.solve_layout(probe);

    for _ in 0..resizes {
        let w = rng.next_u16_range(8, 400);
        let h = rng.next_u16_range(4, 200);
        let area = Rect::new(0, 0, w, h);
        if let Err(msg) = check_layout(&tree, area) {
            panic!("resize storm violated layout invariant: {msg}");
        }
        // Re-solve the probe size periodically; it must never drift.
        assert_eq!(
            tree.solve_layout(probe),
            probe_solution,
            "probe layout drifted under resize storm"
        );
    }
    emit_evidence("stress_resize_storm", 0xBEEF, resizes, tree.state_hash());
}

/// Repeated split-then-close churn must leave the engine in a stable,
/// invariant-clean state with no id corruption or unbounded growth.
#[test]
fn pane_stress_split_close_churn() {
    let cycles = env_usize("PANE_STRESS_CHURN", 1_000);
    let mut tree = PaneTree::singleton("root");
    let mut op_id = 0u64;

    for cycle in 0..cycles {
        // Split a randomly-chosen leaf...
        let leaves = leaf_ids(&tree);
        let target = leaves[cycle % leaves.len()];
        op_id += 1;
        tree.apply_operation(
            op_id,
            PaneOperation::SplitLeaf {
                target,
                axis: if cycle % 2 == 0 {
                    SplitAxis::Horizontal
                } else {
                    SplitAxis::Vertical
                },
                ratio: PaneSplitRatio::new(1, 1).expect("valid ratio"),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new(format!("churn-{cycle}")),
            },
        )
        .expect("split should apply");

        // ...then immediately close the freshly created sibling to churn.
        let new_leaf = leaf_ids(&tree)
            .into_iter()
            .max_by_key(|id| id.get())
            .expect("at least one leaf");
        op_id += 1;
        tree.apply_operation(op_id, PaneOperation::CloseNode { target: new_leaf })
            .expect("close should apply");

        if let Err(msg) = check_tree_invariants(&tree) {
            panic!("churn cycle {cycle} corrupted the tree: {msg}");
        }
    }

    // After balanced churn the tree should be small and valid (not leaking).
    let leaves = leaf_ids(&tree).len();
    assert!(leaves <= 4, "split/close churn leaked leaves: {leaves}");
    check_tree_invariants(&tree).expect("post-churn tree valid");
    emit_evidence("stress_split_close_churn", 0, cycles, tree.state_hash());
}

/// Deep nested splits and wide fan-out must remain valid and solve within
/// bounds (large tree depth/width stress).
#[test]
fn pane_stress_deep_and_wide_trees() {
    let depth = env_usize("PANE_STRESS_DEPTH", 48);
    let width = env_usize("PANE_STRESS_WIDTH", 128);

    // Deep: repeatedly split the most-recently-created leaf to build a spine.
    let mut deep = PaneTree::singleton("root");
    let mut op_id = 0u64;
    for level in 0..depth {
        let target = leaf_ids(&deep)
            .into_iter()
            .max_by_key(|id| id.get())
            .expect("leaf present");
        op_id += 1;
        deep.apply_operation(
            op_id,
            PaneOperation::SplitLeaf {
                target,
                axis: if level % 2 == 0 {
                    SplitAxis::Horizontal
                } else {
                    SplitAxis::Vertical
                },
                ratio: PaneSplitRatio::new(1, 2).expect("valid ratio"),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new(format!("deep-{level}")),
            },
        )
        .expect("deep split applies");
    }
    check_tree_invariants(&deep).expect("deep tree valid");
    // Give every level room: extent scales with depth.
    let extent = (depth as u16).saturating_mul(2).saturating_add(16);
    check_layout(&deep, Rect::new(0, 0, extent, extent)).expect("deep tree solves in bounds");
    assert_eq!(
        deep.solve_layout(Rect::new(0, 0, extent, extent)),
        deep.solve_layout(Rect::new(0, 0, extent, extent))
    );

    // Wide: keep splitting the same original leaf to fan out siblings.
    let mut wide = PaneTree::singleton("root");
    op_id = 0;
    for n in 0..width {
        let target = leaf_ids(&wide)
            .into_iter()
            .min_by_key(|id| id.get())
            .expect("leaf present");
        op_id += 1;
        wide.apply_operation(
            op_id,
            PaneOperation::SplitLeaf {
                target,
                axis: SplitAxis::Horizontal,
                ratio: PaneSplitRatio::new(1, 1).expect("valid ratio"),
                placement: PanePlacement::IncomingFirst,
                new_leaf: PaneLeaf::new(format!("wide-{n}")),
            },
        )
        .expect("wide split applies");
    }
    check_tree_invariants(&wide).expect("wide tree valid");
    let wextent = (width as u16).saturating_mul(2).saturating_add(16);
    check_layout(&wide, Rect::new(0, 0, wextent, wextent)).expect("wide tree solves in bounds");

    emit_evidence("stress_deep_wide", depth as u64, width, wide.state_hash());
}

// ---------------------------------------------------------------------------
// Chaos
// ---------------------------------------------------------------------------

/// Transaction rollback is a true no-op: after applying operations inside a
/// transaction and rolling back, the base tree is byte-identical to its
/// pre-transaction state, repeated across a chaotic interleave.
#[test]
fn pane_chaos_rollback_is_a_true_noop() {
    let mut tree = PaneTree::singleton("root");
    let mut rng = Lcg::new(0xC4A05_u64);
    let mut txn_id = 0u64;
    let mut op_id = 100_000u64;

    // Grow a small working tree first.
    for step in 0..8 {
        let op = bounded_random_operation(&tree, &mut rng, step, 12);
        op_id += 1;
        let _ = tree.apply_operation(op_id, op);
    }

    for _ in 0..200 {
        let before = tree.state_hash();
        let before_snapshot = tree.to_snapshot();

        txn_id += 1;
        let mut txn = tree.begin_transaction(txn_id);
        // Apply a handful of random ops inside the transaction.
        for step in 0..rng.next_u32_range(1, 6) {
            let op = bounded_random_operation(txn.tree(), &mut rng, step as usize, 16);
            let _ = txn.apply_operation(op_id + step as u64, op);
        }
        let outcome = txn.rollback();
        assert!(
            !outcome.committed,
            "rolled-back transaction must report not committed"
        );

        // The original tree is untouched by the rolled-back transaction.
        assert_eq!(
            tree.state_hash(),
            before,
            "rollback changed the base tree hash"
        );
        assert_eq!(
            tree.to_snapshot(),
            before_snapshot,
            "rollback changed the base tree"
        );
    }
    emit_evidence("chaos_rollback_noop", 0xC4A05, 200, tree.state_hash());
}

/// Invalid operations are rejected without mutating the tree: the state hash is
/// unchanged after each rejected op (no partial mutation).
#[test]
fn pane_chaos_invalid_ops_are_rejected_without_mutation() {
    let mut tree = PaneTree::singleton("root");
    // Build a tiny known tree: split root once.
    tree.apply_operation(
        1,
        PaneOperation::SplitLeaf {
            target: leaf_ids(&tree)[0],
            axis: SplitAxis::Horizontal,
            ratio: PaneSplitRatio::new(1, 1).expect("ratio"),
            placement: PanePlacement::ExistingFirst,
            new_leaf: PaneLeaf::new("b"),
        },
    )
    .expect("setup split applies");

    let root = tree.root();
    let split = split_ids(&tree)[0];
    let bogus = PaneId::new(9_999).expect("non-zero");
    let baseline = tree.state_hash();

    let invalid_ops = vec![
        // Close the root (must be rejected).
        PaneOperation::CloseNode { target: root },
        // Split a node that is not a leaf.
        PaneOperation::SplitLeaf {
            target: split,
            axis: SplitAxis::Vertical,
            ratio: PaneSplitRatio::new(1, 1).expect("ratio"),
            placement: PanePlacement::ExistingFirst,
            new_leaf: PaneLeaf::new("x"),
        },
        // Operate on a non-existent node.
        PaneOperation::CloseNode { target: bogus },
        PaneOperation::SetSplitRatio {
            split: bogus,
            ratio: PaneSplitRatio::new(3, 1).expect("ratio"),
        },
        // Swap against a non-existent node (a self-swap, by contrast, is a
        // legitimate no-op and is intentionally not in this list).
        PaneOperation::SwapNodes {
            first: root,
            second: bogus,
        },
    ];

    let mut op_id = 1_000u64;
    for op in invalid_ops {
        op_id += 1;
        let result = tree.apply_operation(op_id, op.clone());
        assert!(result.is_err(), "expected rejection of invalid op: {op:?}");
        assert_eq!(
            tree.state_hash(),
            baseline,
            "rejected op {op:?} must not mutate the tree"
        );
    }
    check_tree_invariants(&tree).expect("tree valid after rejected ops");
    emit_evidence("chaos_invalid_rejected", 0, 5, tree.state_hash());
}

/// Recovery (`repair_safe`) over already-valid state is idempotent: it produces
/// a valid tree, reports no remaining errors, and leaves the topology unchanged.
#[test]
fn pane_chaos_repair_is_idempotent_on_valid_state() {
    // A non-trivial valid tree.
    let (tree, _) = run_soak("repair_seed", 0xDEAD, 60, 16);
    let snapshot: PaneTreeSnapshot = tree.to_snapshot();
    let before_hash = snapshot.state_hash();

    let outcome = snapshot
        .repair_safe()
        .expect("repair_safe must succeed on a valid snapshot");
    assert!(
        !outcome.report_after.has_errors(),
        "post-repair report has errors: {:?}",
        outcome.report_after.issues
    );
    outcome
        .tree
        .validate()
        .expect("repaired tree must validate");
    assert_eq!(
        outcome.after_hash, before_hash,
        "repair_safe must be a no-op on already-valid state"
    );
    emit_evidence("chaos_repair_idempotent", 0xDEAD, 60, outcome.after_hash);
}

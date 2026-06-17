//! Certified-rewrite equivalence proofs for pane operation families.
//!
//! The pane engine partitions operations into a [`PaneOperationFamily`]:
//!
//! * `Local` (`SetSplitRatio`) takes a certified fast path — an in-place atomic
//!   mutation validated by a local invariant closure over the touched node.
//! * `Structural` (everything else) uses the conservative clone-and-whole-tree
//!   validate baseline.
//!
//! The optimization is only sound if the fast path is *observationally
//! equivalent* to the conservative baseline. `PaneTree::apply_operation_conservative`
//! is the forced-baseline oracle: it always clones and runs the whole-tree
//! validator regardless of family. These tests prove that for every operation —
//! accepted or rejected, across randomized histories and adversarial
//! counterexamples — the adaptive path (`apply_operation`, with fast paths) and
//! the conservative path produce byte-identical results and trees.
//!
//! See `docs/perf/pane_operation_families.md` for the isomorphism rationale.

use ftui_layout::{
    PaneId, PaneLeaf, PaneNodeKind, PaneOperation, PaneOperationFailure, PanePlacement,
    PaneSplitRatio, PaneTree, SplitAxis,
};

/// Small deterministic LCG so the corpus is reproducible without proptest
/// shrinking noise. Mirrors the generator used by `pane_invariant_fuzz.rs`.
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
        // SplitMix64 step — good distribution, fully deterministic.
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_u32_range(&mut self, min: u32, max: u32) -> u32 {
        debug_assert!(min < max);
        let span = u64::from(max - min);
        min + (self.next_u64() % span) as u32
    }

    fn choose_index(&mut self, len: usize) -> usize {
        debug_assert!(len > 0);
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

/// Generate a structurally valid random operation for the current tree, drawing
/// from every operation family so the equivalence proof spans both `Local` and
/// `Structural` paths.
fn random_operation(tree: &PaneTree, rng: &mut Lcg, sequence: usize) -> PaneOperation {
    let leaves = leaf_ids(tree);
    let splits = split_ids(tree);

    let mut candidates = vec![0usize]; // NormalizeRatios (always available)
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

/// Build a representative valid tree by replaying `steps` random operations
/// through the adaptive path.
fn build_random_tree(seed: u64, steps: usize) -> PaneTree {
    let mut tree = PaneTree::singleton("root");
    let mut rng = Lcg::new(seed);
    for step in 0..steps {
        let op = random_operation(&tree, &mut rng, step);
        // Generators only emit structurally valid operations.
        tree.apply_operation((step as u64) + 1, op)
            .expect("generated operation should apply");
    }
    tree
}

#[test]
fn adaptive_and_conservative_paths_agree_across_random_histories() {
    // End-to-end: drive two trees through the *same* random history in lockstep,
    // one with fast paths enabled (`apply_operation`) and one forced onto the
    // conservative baseline (`apply_operation_conservative`). Every step must
    // produce identical outcomes and identical resulting trees across all
    // operation families.
    for seed in 0..48u64 {
        let mut adaptive = PaneTree::singleton("root");
        let mut conservative = PaneTree::singleton("root");
        let mut rng = Lcg::new(seed);

        for step in 0..40usize {
            // Both trees are identical here, so generating from one is sound.
            let op = random_operation(&adaptive, &mut rng, step);
            let operation_id = (step as u64) + 1;

            let adaptive_result = adaptive.apply_operation(operation_id, op.clone());
            let conservative_result =
                conservative.apply_operation_conservative(operation_id, op.clone());

            assert_eq!(
                adaptive_result, conservative_result,
                "outcome diverged at seed={seed} step={step} op={op:?}"
            );
            assert_eq!(
                adaptive.to_snapshot(),
                conservative.to_snapshot(),
                "tree diverged at seed={seed} step={step} op={op:?}"
            );
            assert_eq!(
                adaptive, conservative,
                "tree value diverged at seed={seed} step={step} op={op:?}"
            );
        }
    }
}

#[test]
fn set_split_ratio_fast_path_matches_conservative_baseline_for_every_split() {
    // Focused Local-family proof: for many trees and many ratios, applying
    // `SetSplitRatio` via the certified fast path is identical to the
    // conservative baseline — both in the returned outcome and in the resulting
    // tree. This is the certified-rewrite obligation for the fast path.
    for seed in 0..64u64 {
        let tree = build_random_tree(seed, 24);
        let splits = split_ids(&tree);
        if splits.is_empty() {
            continue;
        }

        let mut rng = Lcg::new(seed ^ 0xDEAD_BEEF);
        for split in splits {
            for _ in 0..4 {
                let ratio = random_ratio(&mut rng);
                let op = PaneOperation::SetSplitRatio { split, ratio };

                let mut fast = tree.clone();
                let mut conservative = tree.clone();

                let fast_result = fast.apply_operation(1, op.clone());
                let conservative_result = conservative.apply_operation_conservative(1, op.clone());

                assert_eq!(
                    fast_result, conservative_result,
                    "outcome mismatch seed={seed} op={op:?}"
                );
                assert_eq!(fast, conservative, "tree mismatch seed={seed} op={op:?}");

                // Accepted operations preserve global validity; the fast path's
                // local-closure validation never lets an invalid tree through.
                if fast_result.is_ok() {
                    fast.validate()
                        .expect("accepted fast-path mutation must keep tree valid");
                } else {
                    assert_eq!(fast, tree, "rejected op must leave the tree unchanged");
                }
            }
        }
    }
}

#[test]
fn set_split_ratio_counterexamples_reject_identically_on_both_paths() {
    // Adversarial fixtures: invalid fast-path entry must be rejected exactly as
    // the conservative baseline rejects it, with the same failure reason and an
    // unchanged tree.
    let tree = build_random_tree(7, 24);
    assert!(
        !split_ids(&tree).is_empty(),
        "fixture tree must contain at least one split"
    );
    let ratio = PaneSplitRatio::new(2, 1).expect("valid ratio");

    // Counterexample 1: a node id that was never allocated.
    let missing = tree.to_snapshot().next_id;
    let missing_op = PaneOperation::SetSplitRatio {
        split: missing,
        ratio,
    };
    assert_reject_equivalence(
        &tree,
        &missing_op,
        |reason| matches!(reason, PaneOperationFailure::MissingNode { node_id } if *node_id == missing),
    );

    // Counterexample 2: targeting a leaf (not a split).
    let leaf = leaf_ids(&tree)[0];
    let leaf_op = PaneOperation::SetSplitRatio { split: leaf, ratio };
    assert_reject_equivalence(
        &tree,
        &leaf_op,
        |reason| matches!(reason, PaneOperationFailure::ParentNotSplit { node_id } if *node_id == leaf),
    );
}

/// Assert both paths reject `op`, with identical error payloads, a matching
/// reason, and an unchanged tree.
fn assert_reject_equivalence(
    tree: &PaneTree,
    op: &PaneOperation,
    reason_matches: impl Fn(&PaneOperationFailure) -> bool,
) {
    let mut fast = tree.clone();
    let mut conservative = tree.clone();

    let fast_result = fast.apply_operation(1, op.clone());
    let conservative_result = conservative.apply_operation_conservative(1, op.clone());

    assert_eq!(
        fast_result, conservative_result,
        "rejection outcome mismatch for {op:?}"
    );
    let err = fast_result.expect_err("fast path must reject the invalid operation");
    assert!(
        reason_matches(&err.reason),
        "unexpected rejection reason for {op:?}: {:?}",
        err.reason
    );
    assert_eq!(fast, *tree, "fast-path rejection must not mutate the tree");
    assert_eq!(
        conservative, *tree,
        "conservative rejection must not mutate the tree"
    );
}

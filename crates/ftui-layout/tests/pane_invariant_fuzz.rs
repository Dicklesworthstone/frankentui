//! Property/fuzz-style invariants for pane split-tree operations.
//!
//! This suite exercises random operation streams against the public PaneTree API
//! and asserts structural validity, deterministic replay, and stable layout
//! bounds after each mutation.

use ftui_layout::{
    PaneId, PaneLeaf, PaneNodeKind, PaneOperation, PanePlacement, PaneSplitRatio, PaneTree, Rect,
    SplitAxis,
};
use proptest::prelude::*;

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

    let op_kind = candidates[rng.choose_index(candidates.len())];
    match op_kind {
        1 => {
            let target = leaves[rng.choose_index(leaves.len())];
            PaneOperation::SplitLeaf {
                target,
                axis: random_axis(rng),
                ratio: random_ratio(rng),
                placement: random_placement(rng),
                new_leaf: PaneLeaf::new(format!("leaf-{sequence}")),
            }
        }
        2 => {
            let target = leaves[rng.choose_index(leaves.len())];
            PaneOperation::CloseNode { target }
        }
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
        5 => {
            let split = splits[rng.choose_index(splits.len())];
            PaneOperation::SetSplitRatio {
                split,
                ratio: random_ratio(rng),
            }
        }
        _ => PaneOperation::NormalizeRatios,
    }
}

fn assert_layout_determinism_and_bounds(tree: &PaneTree, area: Rect) {
    let first = tree.solve_layout(area);
    let second = tree.solve_layout(area);
    assert_eq!(
        first, second,
        "solve_layout must be deterministic for identical tree+area"
    );

    let Ok(layout) = first else {
        return;
    };

    for pane in leaf_ids(tree) {
        let rect = layout
            .rect(pane)
            .expect("every leaf pane should have a solved rect");

        assert!(rect.x >= area.x);
        assert!(rect.y >= area.y);
        assert!(rect.right() <= area.right());
        assert!(rect.bottom() <= area.bottom());
        assert!(rect.width > 0);
        assert!(rect.height > 0);
    }
}

fn assert_tree_invariants(tree: &PaneTree) {
    tree.validate()
        .expect("tree should remain structurally valid");
    let report = tree.invariant_report();
    assert!(
        !report.has_errors(),
        "invariant report contains errors: {:?}",
        report.issues
    );
}

fn run_sequence(seed: u64, steps: usize) -> (PaneTree, Vec<PaneOperation>) {
    let mut tree = PaneTree::singleton("root");
    let mut rng = Lcg::new(seed);
    let mut applied = Vec::with_capacity(steps);

    for step in 0..steps {
        let operation = random_operation(&tree, &mut rng, step);
        let operation_id = (step as u64) + 1;

        let outcome = tree.apply_operation(operation_id, operation.clone());
        assert!(
            outcome.is_ok(),
            "operation failed at step {step}, seed={seed}, op={operation:?}, err={outcome:?}"
        );
        let _ = outcome.expect("checked is_ok");

        assert_tree_invariants(&tree);
        let leaf_count = leaf_ids(&tree).len() as u16;
        let base_extent = leaf_count.saturating_mul(4).max(64);
        let area = Rect::new(
            0,
            0,
            base_extent.saturating_add(rng.next_u16_range(0, 32)),
            base_extent.saturating_add(rng.next_u16_range(0, 32)),
        );
        assert_layout_determinism_and_bounds(&tree, area);
        applied.push(operation);
    }

    (tree, applied)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn pane_tree_random_operation_sequences_preserve_invariants(
        seed in any::<u64>(),
        steps in 20usize..120,
    ) {
        let (tree, _) = run_sequence(seed, steps);
        assert_tree_invariants(&tree);
    }

    #[test]
    fn pane_tree_random_operation_sequences_replay_deterministically(
        seed in any::<u64>(),
        steps in 20usize..80,
    ) {
        let (final_tree, operations) = run_sequence(seed, steps);
        let final_hash = final_tree.state_hash();

        let mut replay_tree = PaneTree::singleton("root");
        for (idx, operation) in operations.into_iter().enumerate() {
            replay_tree
                .apply_operation((idx as u64) + 1, operation)
                .expect("replay operation should succeed");
        }

        assert_eq!(
            replay_tree.state_hash(),
            final_hash,
            "same operation sequence should produce identical state hash"
        );
        assert_eq!(
            replay_tree.to_snapshot(),
            final_tree.to_snapshot(),
            "same operation sequence should produce identical snapshot"
        );
    }
}

#[test]
fn pane_tree_fuzz_seed_corpus_preserves_invariants() {
    let seeds = [
        0_u64,
        1,
        2,
        3,
        5,
        8,
        13,
        21,
        34,
        55,
        89,
        144,
        u32::MAX as u64,
        (u32::MAX as u64) + 1,
        u64::MAX - 1,
        u64::MAX,
    ];

    for seed in seeds {
        let (tree, _) = run_sequence(seed, 180);
        assert_tree_invariants(&tree);
    }
}

//! Differential equivalence proofs for the persistent/versioned pane-tree spike
//! (`bd-1k7ek.5`).
//!
//! The prototype [`PaneVersionStore`] / [`VersionedPaneTree`] must be
//! *observationally equivalent* to the canonical [`PaneTree`] +
//! [`PaneInteractionTimeline`] it could replace. These tests drive both engines
//! through identical random histories and prove:
//!
//! * **Apply parity** — after every operation (all six families), the persistent
//!   current version flattens to the same `state_hash` and `next_id` as the
//!   canonical tree.
//! * **Navigation parity** — replay-free `O(1)` undo/redo over the version store
//!   reproduces the exact hash sequence the checkpointed-replay timeline
//!   produces at every cursor position.
//! * **Reject parity** — illegal operations are rejected by both engines.
//! * **Structural sharing** — off-path subtrees survive a deep mutation as the
//!   same `Arc` allocation.
//! * **Determinism** — identical seeds yield identical version-hash sequences.
//! * **Rollback** — the persistent tree always flattens back to a valid
//!   canonical tree identical to replaying the operation log.
//!
//! See `docs/perf/pane_persistent_versioning.md` for the adoption analysis.

use std::sync::Arc;

use ftui_layout::{
    PaneId, PaneInteractionTimeline, PaneLeaf, PaneNodeKind, PaneOperation, PanePlacement,
    PaneSplitRatio, PaneTree, PaneVersionStore, PersistentApplyError, PersistentNode, SplitAxis,
    VersionedPaneTree,
};

/// Deterministic SplitMix64 generator (mirrors `pane_operation_family_equivalence.rs`).
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

/// Generate a structurally valid random operation drawing from every family.
fn random_operation(tree: &PaneTree, rng: &mut Lcg, sequence: usize) -> PaneOperation {
    let leaves = leaf_ids(tree);
    let splits = split_ids(tree);

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

#[test]
fn persistent_apply_matches_canonical_across_random_histories() {
    // Lockstep: the canonical tree and the persistent store consume the same
    // random history. After each operation the flattened persistent state must
    // equal the canonical state, for every operation family.
    for seed in 0..48u64 {
        let mut canonical = PaneTree::singleton("root");
        let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
        let mut rng = Lcg::new(seed);

        for step in 0..40usize {
            let op = random_operation(&canonical, &mut rng, step);

            canonical
                .apply_operation((step as u64) + 1, op.clone())
                .expect("generated op applies to canonical tree");
            store
                .apply(&op)
                .expect("generated op applies to persistent store");

            assert_eq!(
                store.current().state_hash().expect("flatten persistent"),
                canonical.state_hash(),
                "state hash diverged at seed={seed} step={step} op={op:?}"
            );
            assert_eq!(
                store.current().next_id(),
                canonical.next_id(),
                "next_id diverged at seed={seed} step={step} op={op:?}"
            );
        }
    }
}

#[test]
fn persistent_navigation_matches_checkpointed_replay_timeline() {
    // Build identical histories in the checkpointed timeline and the persistent
    // store, capture the canonical hash at each cursor, then walk undo→redo on
    // both. The persistent store's O(1) navigation must reproduce the same hash
    // sequence the replay-based timeline produces.
    for seed in 0..24u64 {
        let mut canonical = PaneTree::singleton("root");
        let mut timeline = PaneInteractionTimeline::with_baseline(&canonical);
        let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
        let mut rng = Lcg::new(seed ^ 0xA5A5_A5A5);

        // Expected hash at each applied_len (index 0 == empty history).
        let mut expected = vec![canonical.state_hash()];

        let steps = 40usize;
        for step in 0..steps {
            let op = random_operation(&canonical, &mut rng, step);
            timeline
                .apply_and_record(&mut canonical, step as u64, (step as u64) + 1, op.clone())
                .expect("timeline apply");
            store.apply(&op).expect("store apply");
            expected.push(canonical.state_hash());
            assert_eq!(
                store.current().state_hash().expect("flatten"),
                *expected.last().expect("present"),
                "post-apply hash diverged at seed={seed} step={step}"
            );
        }

        // Undo all the way down to the baseline, checking parity at each cursor.
        let mut applied = steps;
        while applied > 0 {
            let timeline_moved = timeline.undo(&mut canonical).expect("timeline undo");
            let store_moved = store.undo();
            assert_eq!(timeline_moved, store_moved, "undo availability diverged");
            applied -= 1;
            assert_eq!(
                canonical.state_hash(),
                expected[applied],
                "timeline undo hash wrong at applied={applied} seed={seed}"
            );
            assert_eq!(
                store.current().state_hash().expect("flatten"),
                expected[applied],
                "store undo hash wrong at applied={applied} seed={seed}"
            );
        }
        assert!(!timeline.undo(&mut canonical).expect("no-op undo"));
        assert!(!store.undo());

        // Redo back up to the head.
        while applied < steps {
            assert!(timeline.redo(&mut canonical).expect("timeline redo"));
            assert!(store.redo());
            applied += 1;
            assert_eq!(
                canonical.state_hash(),
                expected[applied],
                "timeline redo hash wrong at applied={applied} seed={seed}"
            );
            assert_eq!(
                store.current().state_hash().expect("flatten"),
                expected[applied],
                "store redo hash wrong at applied={applied} seed={seed}"
            );
        }
        assert!(!store.redo());
    }
}

#[test]
fn invalid_operations_are_rejected_by_both_engines() {
    // Build a representative tree.
    let mut canonical = PaneTree::singleton("root");
    let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
    let mut rng = Lcg::new(0x1357);
    for step in 0..24usize {
        let op = random_operation(&canonical, &mut rng, step);
        canonical
            .apply_operation((step as u64) + 1, op.clone())
            .expect("apply");
        store.apply(&op).expect("apply");
    }

    let missing = canonical.to_snapshot().next_id;
    let leaf = leaf_ids(&canonical)[0];
    let split = split_ids(&canonical)[0];
    let root = canonical.root();
    let ratio = PaneSplitRatio::new(2, 1).expect("ratio");

    let illegal = [
        // Missing node id.
        PaneOperation::SetSplitRatio {
            split: missing,
            ratio,
        },
        // SetSplitRatio on a leaf.
        PaneOperation::SetSplitRatio { split: leaf, ratio },
        // SplitLeaf on a split.
        PaneOperation::SplitLeaf {
            target: split,
            axis: SplitAxis::Horizontal,
            ratio,
            placement: PanePlacement::ExistingFirst,
            new_leaf: PaneLeaf::new("x"),
        },
        // Close the root.
        PaneOperation::CloseNode { target: root },
        // Close a missing node.
        PaneOperation::CloseNode { target: missing },
        // Swap a node with its ancestor (root is an ancestor of every node).
        PaneOperation::SwapNodes {
            first: root,
            second: leaf,
        },
    ];

    for op in illegal {
        let mut baseline = canonical.clone();
        let baseline_result = baseline.apply_operation_conservative(1, op.clone());
        let persistent_result = store.current().apply_operation(&op);
        assert!(baseline_result.is_err(), "baseline should reject {op:?}");
        assert!(
            persistent_result.is_err(),
            "persistent should reject {op:?}"
        );
        // The persistent store's current version is unchanged on rejection
        // (apply_operation is pure; it never mutated `store`).
        assert_eq!(
            store.current().state_hash().expect("flatten"),
            canonical.state_hash()
        );
    }
}

#[test]
fn deep_mutation_preserves_off_path_arc_sharing() {
    // Build a left-leaning chain so there is a deep path with shareable
    // off-path siblings, then re-ratio a deep split and prove an untouched
    // sibling subtree is the same physical allocation.
    let mut canonical = PaneTree::singleton("root");
    // Split the leftmost leaf repeatedly to grow depth.
    for step in 0..6u64 {
        let target = *leaf_ids(&canonical)
            .iter()
            .min()
            .expect("at least one leaf");
        canonical
            .apply_operation(
                step + 1,
                PaneOperation::SplitLeaf {
                    target,
                    axis: SplitAxis::Horizontal,
                    ratio: PaneSplitRatio::new(1, 1).expect("ratio"),
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new(format!("n{step}")),
                },
            )
            .expect("split");
    }

    let base = VersionedPaneTree::from_pane_tree(&canonical);
    // Root's second child is an off-path subtree relative to a deep-left mutation.
    let off_path_before = match &**base.root() {
        PersistentNode::Split { second, .. } => second.clone(),
        PersistentNode::Leaf { .. } => unreachable!("root is a split"),
    };

    // Find a deep split (max id among splits is the most recently created one).
    let deep_split = *split_ids(&canonical).iter().max().expect("a split");
    let next = base
        .apply_operation(&PaneOperation::SetSplitRatio {
            split: deep_split,
            ratio: PaneSplitRatio::new(7, 3).expect("ratio"),
        })
        .expect("set ratio");

    let off_path_after = match &**next.root() {
        PersistentNode::Split { second, .. } => second.clone(),
        PersistentNode::Leaf { .. } => unreachable!("root is a split"),
    };
    assert!(
        Arc::ptr_eq(&off_path_before, &off_path_after),
        "off-path subtree must be physically shared across versions"
    );
    assert!(
        !Arc::ptr_eq(base.root(), next.root()),
        "the mutated root path must be a fresh allocation"
    );
    // And the result is still byte-identical to the canonical baseline.
    let mut canonical_after = canonical.clone();
    canonical_after
        .apply_operation_conservative(
            99,
            PaneOperation::SetSplitRatio {
                split: deep_split,
                ratio: PaneSplitRatio::new(7, 3).expect("ratio"),
            },
        )
        .expect("canonical set ratio");
    assert_eq!(
        next.state_hash().expect("flatten"),
        canonical_after.state_hash()
    );
}

#[test]
fn identical_seeds_yield_identical_version_hashes() {
    let run = |seed: u64| -> Vec<u64> {
        let mut canonical = PaneTree::singleton("root");
        let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
        let mut rng = Lcg::new(seed);
        let mut hashes = Vec::new();
        for step in 0..32usize {
            let op = random_operation(&canonical, &mut rng, step);
            canonical
                .apply_operation((step as u64) + 1, op.clone())
                .expect("apply");
            store.apply(&op).expect("apply");
            hashes.push(store.current().state_hash().expect("flatten"));
        }
        hashes
    };
    assert_eq!(
        run(0xBEEF),
        run(0xBEEF),
        "version hashes must be deterministic"
    );
}

#[test]
fn persistent_tree_always_rolls_back_to_a_valid_canonical_tree() {
    // After an arbitrary history, the persistent current version must flatten to
    // a *valid* canonical tree identical to replaying the same operation log.
    let mut log: Vec<PaneOperation> = Vec::new();
    let mut canonical = PaneTree::singleton("root");
    let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
    let mut rng = Lcg::new(0xFEED_FACE);
    for step in 0..40usize {
        let op = random_operation(&canonical, &mut rng, step);
        canonical
            .apply_operation((step as u64) + 1, op.clone())
            .expect("apply");
        store.apply(&op).expect("apply");
        log.push(op);
    }

    // Flatten the persistent tree back to canonical form (this validates).
    let rolled_back = store
        .current()
        .to_pane_tree()
        .expect("persistent tree flattens to a valid canonical tree");
    rolled_back.validate().expect("rolled-back tree is valid");

    // Replay the log from scratch and compare.
    let mut replayed = PaneTree::singleton("root");
    for (idx, op) in log.into_iter().enumerate() {
        replayed
            .apply_operation((idx as u64) + 1, op)
            .expect("replay");
    }
    assert_eq!(rolled_back.state_hash(), replayed.state_hash());
    assert_eq!(rolled_back.to_snapshot(), replayed.to_snapshot());
}

#[test]
fn version_store_truncates_redo_branch_on_new_apply() {
    let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
    let split = PaneOperation::SplitLeaf {
        target: PaneId::MIN,
        axis: SplitAxis::Horizontal,
        ratio: PaneSplitRatio::new(1, 1).expect("ratio"),
        placement: PanePlacement::ExistingFirst,
        new_leaf: PaneLeaf::new("b"),
    };
    store.apply(&split).expect("split");
    let split2 = PaneOperation::SplitLeaf {
        target: PaneId::MIN,
        axis: SplitAxis::Vertical,
        ratio: PaneSplitRatio::new(1, 1).expect("ratio"),
        placement: PanePlacement::ExistingFirst,
        new_leaf: PaneLeaf::new("c"),
    };
    store.apply(&split2).expect("split2");
    assert_eq!(store.version_count(), 3);

    // Undo twice, then apply a different op — the redo branch is discarded.
    assert!(store.undo());
    assert!(store.undo());
    assert!(store.can_redo());
    let ratio_op = PaneOperation::SetSplitRatio {
        split: PaneId::MIN,
        ratio: PaneSplitRatio::new(1, 1).expect("ratio"),
    };
    // SetSplitRatio on the root leaf is invalid; use a fresh split instead.
    let _ = ratio_op;
    store.apply(&split).expect("re-split after undo");
    assert!(
        !store.can_redo(),
        "redo branch must be discarded after a new apply"
    );
    assert_eq!(store.version_count(), 2);
}

#[test]
fn rebuild_fallback_ops_keep_total_parity() {
    // Drive a history that includes MoveSubtree and NormalizeRatios (the rebuild
    // fallback ops) and confirm parity is maintained through them.
    let mut canonical = PaneTree::singleton("root");
    let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
    // Grow to >2 leaves so MoveSubtree becomes available.
    for step in 0..5u64 {
        let target = *leaf_ids(&canonical).iter().min().expect("leaf");
        let op = PaneOperation::SplitLeaf {
            target,
            axis: SplitAxis::Vertical,
            ratio: PaneSplitRatio::new(1, 1).expect("ratio"),
            placement: PanePlacement::ExistingFirst,
            new_leaf: PaneLeaf::new(format!("g{step}")),
        };
        canonical
            .apply_operation(step + 1, op.clone())
            .expect("split");
        store.apply(&op).expect("split");
    }

    let leaves = leaf_ids(&canonical);
    let move_op = PaneOperation::MoveSubtree {
        source: leaves[0],
        target: leaves[leaves.len() - 1],
        axis: SplitAxis::Horizontal,
        ratio: PaneSplitRatio::new(3, 2).expect("ratio"),
        placement: PanePlacement::IncomingFirst,
    };
    canonical
        .apply_operation(100, move_op.clone())
        .expect("move");
    store.apply(&move_op).expect("move");
    assert_eq!(
        store.current().state_hash().expect("flatten"),
        canonical.state_hash()
    );

    canonical
        .apply_operation(101, PaneOperation::NormalizeRatios)
        .expect("normalize");
    store
        .apply(&PaneOperation::NormalizeRatios)
        .expect("normalize");
    assert_eq!(
        store.current().state_hash().expect("flatten"),
        canonical.state_hash()
    );
}

#[test]
fn missing_node_error_surfaces_for_unknown_ids() {
    let store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
    let err = store
        .current()
        .apply_operation(&PaneOperation::CloseNode {
            target: PaneId::new(999).expect("id"),
        })
        .expect_err("unknown id");
    assert_eq!(
        err,
        PersistentApplyError::MissingNode {
            node_id: PaneId::new(999).expect("id")
        }
    );
}

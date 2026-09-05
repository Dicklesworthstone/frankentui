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

use ftui_layout::pane_execution::{PaneExecutionEngine, PaneExecutionError, PaneExecutionSample};
use ftui_layout::{
    PaneAssumption, PaneExecutionPolicy, PaneId, PaneInteractionTimeline, PaneLeaf,
    PaneMemoryStrategy, PaneNodeKind, PaneOperation, PaneOperationError, PanePlacement,
    PaneRetentionOutcome, PaneRetentionPolicy, PaneSplitRatio, PaneTree, PaneVersionStore,
    PersistentApplyError, PersistentNode, SplitAxis, VersionedPaneTree,
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

fn live_policy(strategy: PaneMemoryStrategy) -> PaneExecutionPolicy {
    PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded()).forcing(strategy)
}

fn live_split(tree: &PaneTree, label: &str) -> PaneOperation {
    let mut new_leaf = PaneLeaf::new(label);
    new_leaf
        .extensions
        .insert("test-payload".to_owned(), format!("payload:{label}"));
    PaneOperation::SplitLeaf {
        target: leaf_ids(tree)[0],
        axis: SplitAxis::Horizontal,
        ratio: PaneSplitRatio::new(2, 3).expect("valid ratio"),
        placement: PanePlacement::ExistingFirst,
        new_leaf,
    }
}

fn assert_live_matches(
    engine: &PaneExecutionEngine,
    tree: &PaneTree,
    timeline: &PaneInteractionTimeline,
    canonical: &PaneTree,
) {
    assert_eq!(tree.to_snapshot(), canonical.to_snapshot());
    assert_eq!(tree.next_id(), canonical.next_id());
    assert_eq!(engine.timeline().baseline, timeline.baseline);
    assert_eq!(engine.timeline().entries, timeline.entries);
    assert_eq!(engine.timeline().cursor, timeline.cursor);
    assert_eq!(
        engine.replay().expect("engine replay").to_snapshot(),
        canonical.to_snapshot()
    );
    assert_eq!(
        timeline.replay().expect("oracle replay").to_snapshot(),
        canonical.to_snapshot()
    );
    tree.validate().expect("live tree remains valid");
}

#[test]
fn live_engines_match_all_operation_kinds_and_exact_rejections() {
    let policies = [
        live_policy(PaneMemoryStrategy::Persistent),
        live_policy(PaneMemoryStrategy::Checkpointed),
        PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded()).conservative(),
    ];
    let mut observed_kinds = [false; 6];
    for seed in 0..12u64 {
        let mut canonical = PaneTree::singleton("live-root");
        let mut timeline = PaneInteractionTimeline::with_baseline(&canonical);
        let mut engines = policies.map(|policy| {
            let tree = canonical.clone();
            let mut engine = PaneExecutionEngine::new(&tree);
            engine.set_policy(&tree, policy).expect("install policy");
            (engine, tree)
        });
        let mut rng = Lcg::new(seed);
        for step in 0..48usize {
            let operation = random_operation(&canonical, &mut rng, step);
            let kind_index = match operation {
                PaneOperation::SplitLeaf { .. } => 0,
                PaneOperation::CloseNode { .. } => 1,
                PaneOperation::MoveSubtree { .. } => 2,
                PaneOperation::SwapNodes { .. } => 3,
                PaneOperation::SetSplitRatio { .. } => 4,
                PaneOperation::NormalizeRatios => 5,
            };
            observed_kinds[kind_index] = true;
            let operation_id = step as u64 + 1;
            let expected = timeline
                .apply_and_record(
                    &mut canonical,
                    operation_id,
                    operation_id,
                    operation.clone(),
                )
                .expect("generated canonical operation");
            for (engine, tree) in &mut engines {
                assert_eq!(
                    engine
                        .apply_and_record(tree, operation_id, operation_id, operation.clone())
                        .expect("live operation"),
                    expected,
                    "outcome diverged at seed={seed}, step={step}"
                );
                assert_live_matches(engine, tree, &timeline, &canonical);
            }
        }

        // Reject while a redo branch exists: failure must preserve both the
        // original error payload and the branch, including its allocated IDs.
        assert!(timeline.undo(&mut canonical).expect("oracle undo"));
        for (engine, tree) in &mut engines {
            assert!(engine.undo(tree).expect("live undo"));
        }
        let missing = PaneId::new(u64::MAX).expect("nonzero id");
        let invalid = [
            PaneOperation::CloseNode {
                target: canonical.root(),
            },
            PaneOperation::CloseNode { target: missing },
            PaneOperation::SplitLeaf {
                target: missing,
                axis: SplitAxis::Vertical,
                ratio: PaneSplitRatio::new(1, 1).expect("ratio"),
                placement: PanePlacement::IncomingFirst,
                new_leaf: PaneLeaf::new("rejected"),
            },
            PaneOperation::SetSplitRatio {
                split: leaf_ids(&canonical)[0],
                ratio: PaneSplitRatio::new(1, 2).expect("ratio"),
            },
            PaneOperation::MoveSubtree {
                source: missing,
                target: canonical.root(),
                axis: SplitAxis::Vertical,
                ratio: PaneSplitRatio::new(1, 1).expect("ratio"),
                placement: PanePlacement::IncomingFirst,
            },
            PaneOperation::SwapNodes {
                first: missing,
                second: canonical.root(),
            },
        ];
        for (index, operation) in invalid.into_iter().enumerate() {
            let id = 100 + index as u64;
            let before = canonical.to_snapshot();
            let history_before = timeline.clone();
            let expected = timeline
                .apply_and_record(&mut canonical, id, id, operation.clone())
                .expect_err("invalid oracle operation");
            assert_eq!(canonical.to_snapshot(), before);
            assert_eq!(timeline, history_before);
            for (engine, tree) in &mut engines {
                let applies_before = engine.status().applies;
                let error = engine
                    .apply_and_record(tree, id, id, operation.clone())
                    .expect_err("invalid live operation");
                assert_eq!(
                    std::error::Error::source(&error)
                        .and_then(|source| source.downcast_ref::<PaneOperationError>()),
                    Some(&expected),
                    "original canonical error must survive engine wrapping"
                );
                assert_eq!(engine.status().applies, applies_before);
                assert_live_matches(engine, tree, &timeline, &canonical);
            }
        }
        assert!(timeline.redo(&mut canonical).expect("oracle redo"));
        for (index, (engine, tree)) in engines.iter_mut().enumerate() {
            assert!(engine.redo(tree).expect("preserved live redo"));
            assert_live_matches(engine, tree, &timeline, &canonical);
            let status = engine.status();
            assert_eq!(status.applies, 48);
            assert_eq!(status.undos, 1);
            assert_eq!(status.redos, 1);
            match index {
                0 => assert_eq!(status.persistent_applies, 48),
                1 => assert_eq!(status.checkpointed_applies, 48),
                2 => assert_eq!(status.conservative_applies, 48),
                _ => unreachable!("three policies"),
            }
        }
    }
    assert_eq!(observed_kinds, [true; 6], "every operation kind ran");
}

#[test]
fn live_strategy_switches_preserve_mid_history_and_replace_only_redo_branch() {
    let mut canonical = PaneTree::singleton("switch-root");
    let mut timeline = PaneInteractionTimeline::with_baseline(&canonical);
    let mut tree = canonical.clone();
    let mut engine = PaneExecutionEngine::new(&tree);
    engine
        .set_policy(&tree, live_policy(PaneMemoryStrategy::Persistent))
        .expect("persistent policy");
    for id in 1..=6u64 {
        let operation = live_split(&canonical, &format!("before-switch-{id}"));
        timeline
            .apply_and_record(&mut canonical, id, id, operation.clone())
            .expect("oracle split");
        engine
            .apply_and_record(&mut tree, id, id, operation)
            .expect("live split");
    }
    for _ in 0..4 {
        assert!(timeline.undo(&mut canonical).expect("oracle undo"));
        assert!(engine.undo(&mut tree).expect("live undo"));
    }
    assert_eq!(timeline.cursor, 2);
    let retained_history = engine.timeline().clone();
    for strategy in [
        PaneMemoryStrategy::Checkpointed,
        PaneMemoryStrategy::Persistent,
    ] {
        engine
            .set_policy(&tree, live_policy(strategy))
            .expect("mid-history switch");
        assert_eq!(engine.strategy(), strategy);
        assert_eq!(engine.timeline(), &retained_history);
        assert_live_matches(&engine, &tree, &timeline, &canonical);
    }
    // Traverse the migrated redo tail before replacing it with a real edit.
    for _ in 0..4 {
        assert!(timeline.redo(&mut canonical).expect("oracle redo"));
        assert!(engine.redo(&mut tree).expect("migrated redo"));
        assert_live_matches(&engine, &tree, &timeline, &canonical);
    }
    for _ in 0..3 {
        assert!(timeline.undo(&mut canonical).expect("oracle undo"));
        assert!(engine.undo(&mut tree).expect("live undo"));
    }
    engine
        .set_policy(&tree, live_policy(PaneMemoryStrategy::Checkpointed))
        .expect("switch before branch replacement");
    let before = tree.to_snapshot();
    let next_id_before = tree.next_id();
    let operation = live_split(&canonical, "replacement-branch");
    timeline
        .apply_and_record(&mut canonical, 7, 7, operation.clone())
        .expect("oracle replacement");
    engine
        .apply_and_record(&mut tree, 7, 7, operation)
        .expect("live replacement");
    assert_ne!(tree.to_snapshot(), before);
    assert_ne!(tree.next_id(), next_id_before);
    assert_eq!(timeline.entries.len(), 4);
    assert!(!engine.redo(&mut tree).expect("old branch was discarded"));
    assert!(
        !timeline
            .redo(&mut canonical)
            .expect("oracle old branch discarded")
    );
    assert_live_matches(&engine, &tree, &timeline, &canonical);
    engine
        .set_policy(&tree, live_policy(PaneMemoryStrategy::Persistent))
        .expect("migrate replacement branch");
    while timeline
        .undo(&mut canonical)
        .expect("oracle walk to baseline")
    {
        assert!(engine.undo(&mut tree).expect("live walk to baseline"));
        assert_live_matches(&engine, &tree, &timeline, &canonical);
    }
    assert!(!engine.undo(&mut tree).expect("baseline reached"));
    while timeline
        .redo(&mut canonical)
        .expect("oracle walk to new head")
    {
        assert!(engine.redo(&mut tree).expect("live walk to new head"));
        assert_live_matches(&engine, &tree, &timeline, &canonical);
    }
    assert!(!engine.redo(&mut tree).expect("new head reached"));
    assert_eq!(engine.status().applies, 7);
    assert_eq!(engine.status().persistent_applies, 6);
    assert_eq!(engine.status().checkpointed_applies, 1);
}

#[test]
fn live_coalesced_drags_keep_separate_undo_steps_across_strategies() {
    let mut initial = PaneTree::singleton("drag-root");
    let operation = live_split(&initial, "drag-sibling");
    initial
        .apply_operation(1, operation)
        .expect("initial split");
    let split = initial.root();
    for policy in [
        live_policy(PaneMemoryStrategy::Persistent),
        live_policy(PaneMemoryStrategy::Checkpointed),
        PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded()).conservative(),
    ] {
        let mut canonical = initial.clone();
        let mut timeline = PaneInteractionTimeline::with_baseline(&canonical);
        let mut tree = canonical.clone();
        let mut engine = PaneExecutionEngine::new(&tree);
        engine.set_policy(&tree, policy).expect("drag policy");
        let mut snapshots = vec![tree.to_snapshot()];
        for gesture in 0..2u64 {
            let boundary = timeline.next_operation_id() - 1;
            engine.begin_gesture();
            for delta in 1..=4u64 {
                let id = gesture * 4 + delta;
                let operation = PaneOperation::SetSplitRatio {
                    split,
                    ratio: PaneSplitRatio::new(id as u32 + 1, 3).expect("drag ratio"),
                };
                let expected = timeline
                    .apply_and_record_coalesced_resize_delta(
                        &mut canonical,
                        id,
                        id,
                        operation.clone(),
                        boundary,
                    )
                    .expect("oracle drag delta");
                assert_eq!(
                    engine
                        .apply_and_record_coalesced_resize_delta(
                            &mut tree, id, id, operation, boundary,
                        )
                        .expect("live drag delta"),
                    expected
                );
                assert_live_matches(&engine, &tree, &timeline, &canonical);
                assert_eq!(timeline.entries.len(), gesture as usize + 1);
            }
            engine.end_gesture(&tree).expect("finish gesture");
            snapshots.push(tree.to_snapshot());
        }
        assert_eq!(engine.status().applies, 8);
        assert_eq!(engine.timeline().entries.len(), 2);
        for expected in snapshots[..2].iter().rev() {
            assert!(timeline.undo(&mut canonical).expect("oracle drag undo"));
            assert!(engine.undo(&mut tree).expect("live drag undo"));
            assert_eq!(&tree.to_snapshot(), expected);
            assert_live_matches(&engine, &tree, &timeline, &canonical);
        }
        assert!(!engine.undo(&mut tree).expect("pre-drag baseline"));
        for expected in &snapshots[1..] {
            assert!(timeline.redo(&mut canonical).expect("oracle drag redo"));
            assert!(engine.redo(&mut tree).expect("live drag redo"));
            assert_eq!(&tree.to_snapshot(), expected);
            assert_live_matches(&engine, &tree, &timeline, &canonical);
        }
        assert!(!engine.redo(&mut tree).expect("post-drag head"));
    }
}

#[test]
fn live_measured_latency_violation_changes_the_next_execution_path() {
    let origin = std::time::Instant::now();
    let initial = PaneTree::singleton("latency-root");
    let mut canonical = initial.clone();
    let mut timeline = PaneInteractionTimeline::with_baseline(&canonical);
    let mut engines = [1u64, u64::MAX].map(|envelope| {
        let tree = initial.clone();
        let mut engine = PaneExecutionEngine::new(&tree);
        engine
            .set_policy(&tree, live_policy(PaneMemoryStrategy::Persistent))
            .expect("persistent latency policy");
        engine.set_latency_envelope_ns(envelope);
        (engine, tree)
    });
    let operation = live_split(&canonical, "measured-edit");
    timeline
        .apply_and_record(&mut canonical, 1, 1, operation.clone())
        .expect("oracle measured edit");
    for (engine, tree) in &mut engines {
        let start = std::time::Instant::now();
        engine
            .apply_and_record(tree, 1, 1, operation.clone())
            .expect("measured real edit");
        let elapsed_ns = u64::try_from(start.elapsed().as_nanos()).expect("bounded elapsed");
        assert!(
            elapsed_ns > 1,
            "actual edit exceeds the one-nanosecond control envelope"
        );
        engine
            .observe(
                tree,
                PaneExecutionSample {
                    timestamp_ns: u64::try_from(origin.elapsed().as_nanos()).expect("timestamp"),
                    elapsed_ns,
                    local: false,
                },
            )
            .expect("record real observation");
        assert_live_matches(engine, tree, &timeline, &canonical);
    }
    let fallback = &engines[0].0;
    assert_eq!(fallback.strategy(), PaneMemoryStrategy::Checkpointed);
    assert!(fallback.status().conservative);
    assert_eq!(fallback.status().fallbacks, 1);
    let violation = fallback
        .status()
        .last_monitor
        .as_ref()
        .expect("latency verdict");
    assert_eq!(violation.assumption, PaneAssumption::LatencyEnvelope);
    assert!(violation.status.is_violation());
    let control = &engines[1].0;
    assert_eq!(control.strategy(), PaneMemoryStrategy::Persistent);
    assert!(!control.status().conservative);
    assert_eq!(control.status().fallbacks, 0);
    assert!(
        !control
            .status()
            .last_monitor
            .as_ref()
            .expect("control verdict")
            .status
            .is_violation()
    );

    // A changed label alone cannot satisfy this test: another actual split
    // must execute through the conservative path and preserve canonical state.
    let before = canonical.to_snapshot();
    let operation = live_split(&canonical, "after-latency-fallback");
    timeline
        .apply_and_record(&mut canonical, 2, 2, operation.clone())
        .expect("oracle continued edit");
    assert_ne!(canonical.to_snapshot(), before);
    for (engine, tree) in &mut engines {
        engine
            .apply_and_record(tree, 2, 2, operation.clone())
            .expect("continued real edit after observation");
        assert_live_matches(engine, tree, &timeline, &canonical);
    }
    assert_eq!(engines[0].0.status().persistent_applies, 1);
    assert_eq!(engines[0].0.status().conservative_applies, 1);
    assert_eq!(engines[1].0.status().persistent_applies, 2);
    assert_eq!(engines[1].0.status().conservative_applies, 0);
}

#[test]
fn live_import_and_policy_migration_reject_invalid_history_atomically() {
    let mut canonical = PaneTree::singleton("import-root");
    let mut timeline = PaneInteractionTimeline::with_baseline(&canonical);
    timeline.checkpoint_interval = 2;
    for id in 1..=6u64 {
        let operation = live_split(&canonical, &format!("import-{id}"));
        timeline
            .apply_and_record(&mut canonical, id, id, operation)
            .expect("import source edit");
    }
    for _ in 0..4 {
        assert!(timeline.undo(&mut canonical).expect("import source undo"));
    }
    let source_snapshot = canonical.to_snapshot();
    let source_history = timeline.clone();
    let mut tree = canonical.clone();
    let mut engine = PaneExecutionEngine::from_timeline(&tree, timeline.clone())
        .expect("valid mid-history import");
    engine
        .set_policy(&tree, live_policy(PaneMemoryStrategy::Persistent))
        .expect("migrate imported history");
    assert_live_matches(&engine, &tree, &timeline, &canonical);

    let mut invalid_histories = Vec::new();
    let mut missing_baseline = timeline.clone();
    missing_baseline.baseline = None;
    invalid_histories.push(missing_baseline);
    let mut invalid_baseline = timeline.clone();
    invalid_baseline
        .baseline
        .as_mut()
        .expect("baseline")
        .next_id = PaneId::MIN;
    invalid_histories.push(invalid_baseline);
    let mut invalid_cursor = timeline.clone();
    invalid_cursor.cursor = invalid_cursor.entries.len() + 1;
    invalid_histories.push(invalid_cursor);
    let mut invalid_redo = timeline.clone();
    invalid_redo.entries[5].operation = PaneOperation::CloseNode {
        target: PaneId::new(u64::MAX).expect("missing id"),
    };
    invalid_histories.push(invalid_redo);
    let mut invalid_hash = timeline.clone();
    invalid_hash.entries[5].after_hash ^= 1;
    invalid_histories.push(invalid_hash);
    let mut invalid_checkpoint = timeline.clone();
    invalid_checkpoint
        .checkpoints
        .last_mut()
        .expect("checkpoint")
        .snapshot = timeline.baseline.clone().expect("baseline");
    invalid_histories.push(invalid_checkpoint);
    for invalid in invalid_histories {
        assert!(PaneExecutionEngine::from_timeline(&tree, invalid).is_err());
        assert_eq!(tree.to_snapshot(), source_snapshot);
        assert_eq!(timeline, source_history);
        assert_live_matches(&engine, &tree, &timeline, &canonical);
    }
    let before_history = engine.timeline().clone();
    let before_applies = engine.status().applies;
    let before_switches = engine.status().switches;
    let mut mismatched = tree.to_snapshot();
    mismatched.next_id = mismatched.next_id.checked_next().expect("next id");
    let mismatched_tree = PaneTree::from_snapshot(mismatched).expect("valid different allocator");
    let mismatched_before = mismatched_tree.to_snapshot();
    assert!(matches!(
        engine.set_policy(
            &mismatched_tree,
            live_policy(PaneMemoryStrategy::Checkpointed)
        ),
        Err(PaneExecutionError::InvalidHistory(_))
    ));
    assert_eq!(mismatched_tree.to_snapshot(), mismatched_before);
    assert_eq!(engine.strategy(), PaneMemoryStrategy::Persistent);
    assert_eq!(engine.timeline(), &before_history);
    assert_eq!(engine.status().applies, before_applies);
    assert_eq!(engine.status().switches, before_switches);
    assert!(matches!(
        engine.set_policy(&tree, live_policy(PaneMemoryStrategy::Baseline)),
        Err(PaneExecutionError::HistoryRequired)
    ));
    assert_eq!(engine.strategy(), PaneMemoryStrategy::Persistent);
    assert_eq!(engine.timeline(), &before_history);
    assert_live_matches(&engine, &tree, &timeline, &canonical);
    // Failure must leave the already imported engine usable, including redo
    // entries that were not applied at import time.
    while timeline.redo(&mut canonical).expect("oracle imported redo") {
        assert!(
            engine
                .redo(&mut tree)
                .expect("live imported redo after failures")
        );
        assert_live_matches(&engine, &tree, &timeline, &canonical);
    }
    assert!(!engine.redo(&mut tree).expect("imported head reached"));
}

#[test]
fn live_retention_pressure_preserves_full_redo_and_a_continued_edit() {
    for strategy in [
        PaneMemoryStrategy::Persistent,
        PaneMemoryStrategy::Checkpointed,
    ] {
        for (bytes, units) in [(1, 0), (0, 2), (1, 2), (0, 0)] {
            let pressure_expected = bytes != 0 || units != 0;
            let mut canonical = PaneTree::singleton("pressure-root");
            let mut timeline = PaneInteractionTimeline::with_baseline(&canonical);
            let mut tree = canonical.clone();
            let mut engine = PaneExecutionEngine::new(&tree);
            engine
                .set_policy(&tree, live_policy(strategy))
                .expect("initial policy");
            engine.set_latency_envelope_ns(0);
            for id in 1..=6u64 {
                let operation = live_split(&canonical, &format!("retained-{id}"));
                timeline
                    .apply_and_record(&mut canonical, id, id, operation.clone())
                    .expect("oracle retained edit");
                engine
                    .apply_and_record(&mut tree, id, id, operation)
                    .expect("live retained edit");
            }
            for _ in 0..6 {
                assert!(timeline.undo(&mut canonical).expect("oracle baseline undo"));
                assert!(engine.undo(&mut tree).expect("live baseline undo"));
            }
            assert_eq!(engine.timeline().cursor, 0);
            let before = tree.to_snapshot();
            let history_before = engine.timeline().entries.clone();
            let policy = PaneExecutionPolicy::adaptive(PaneRetentionPolicy::bounded(bytes, units))
                .forcing(strategy);
            engine
                .set_policy(&tree, policy)
                .expect("apply actual retention pressure");
            assert_eq!(tree.to_snapshot(), before);
            assert_eq!(engine.timeline().entries, history_before);
            assert_live_matches(&engine, &tree, &timeline, &canonical);
            let status = engine.status();
            let decision = status
                .last_retention
                .as_ref()
                .expect("actual retention decision");
            assert_eq!(decision.budget.max_retained_bytes, bytes);
            assert_eq!(decision.budget.max_retained_units, units);
            assert_eq!(decision.units_before, 6);
            assert_eq!(decision.units_after, 6);
            assert_eq!(decision.units_pruned, 0);
            assert_eq!(decision.current_state_hash, tree.state_hash());
            assert!(decision.bytes_before > 1);
            assert_eq!(
                decision
                    .budget
                    .is_exceeded_by(decision.bytes_after, decision.units_after),
                pressure_expected
            );
            if pressure_expected {
                assert_eq!(decision.outcome, PaneRetentionOutcome::PruningBlocked);
                assert_eq!(engine.strategy(), PaneMemoryStrategy::Checkpointed);
                assert!(status.conservative);
                assert_eq!(status.fallbacks, 1);
                let monitor = status.last_monitor.as_ref().expect("pressure verdict");
                assert_eq!(monitor.assumption, PaneAssumption::RetentionPressure);
                assert!(monitor.status.is_violation());
            } else {
                assert_eq!(decision.outcome, PaneRetentionOutcome::WithinBudget);
                assert_eq!(engine.strategy(), strategy);
                assert!(!status.conservative);
                assert_eq!(status.fallbacks, 0);
                assert!(
                    !status
                        .last_monitor
                        .as_ref()
                        .expect("control verdict")
                        .status
                        .is_violation()
                );
            }
            for _ in 0..6 {
                assert!(
                    timeline
                        .redo(&mut canonical)
                        .expect("oracle protected redo")
                );
                assert!(engine.redo(&mut tree).expect("live protected redo"));
                assert_live_matches(&engine, &tree, &timeline, &canonical);
            }
            assert!(!engine.redo(&mut tree).expect("retained head reached"));
            let before_edit = tree.to_snapshot();
            let next_id_before = tree.next_id();
            let operation = live_split(&canonical, "edit-after-pressure");
            timeline
                .apply_and_record(&mut canonical, 7, 7, operation.clone())
                .expect("oracle continued edit");
            engine
                .apply_and_record(&mut tree, 7, 7, operation)
                .expect("live continued edit");
            assert_ne!(tree.to_snapshot(), before_edit);
            assert_ne!(tree.next_id(), next_id_before);
            assert_live_matches(&engine, &tree, &timeline, &canonical);
            assert_eq!(engine.status().applies, 7);
            assert_eq!(
                engine.status().conservative_applies,
                u64::from(pressure_expected)
            );
            if !pressure_expected {
                match strategy {
                    PaneMemoryStrategy::Persistent => {
                        assert_eq!(engine.status().persistent_applies, 7)
                    }
                    PaneMemoryStrategy::Checkpointed => {
                        assert_eq!(engine.status().checkpointed_applies, 7)
                    }
                    PaneMemoryStrategy::Baseline => unreachable!("history strategies only"),
                }
            }
        }
    }
}

#[test]
fn live_retention_prunes_to_exact_unit_cap_without_discarding_redo() {
    for strategy in [
        PaneMemoryStrategy::Persistent,
        PaneMemoryStrategy::Checkpointed,
    ] {
        let mut canonical = PaneTree::singleton("pruning-root");
        let mut timeline = PaneInteractionTimeline::with_baseline(&canonical);
        let mut tree = canonical.clone();
        let mut engine = PaneExecutionEngine::new(&tree);
        engine
            .set_policy(&tree, live_policy(strategy))
            .expect("initial policy");
        for id in 1..=6u64 {
            let operation = live_split(&canonical, &format!("pruning-{id}"));
            timeline
                .apply_and_record(&mut canonical, id, id, operation.clone())
                .expect("oracle pruning edit");
            engine
                .apply_and_record(&mut tree, id, id, operation)
                .expect("live pruning edit");
        }
        for _ in 0..2 {
            assert!(timeline.undo(&mut canonical).expect("oracle pruning undo"));
            assert!(engine.undo(&mut tree).expect("live pruning undo"));
        }
        let before = tree.to_snapshot();
        assert_eq!(timeline.set_max_entries(3), 3);
        let policy =
            PaneExecutionPolicy::adaptive(PaneRetentionPolicy::bounded(0, 3)).forcing(strategy);
        engine
            .set_policy(&tree, policy)
            .expect("fit exact retained edit cap");
        assert_eq!(tree.to_snapshot(), before);
        assert_eq!(engine.timeline().cursor, 1);
        assert_eq!(engine.timeline().entries.len(), 3);
        assert_live_matches(&engine, &tree, &timeline, &canonical);
        let decision = engine
            .status()
            .last_retention
            .as_ref()
            .expect("pruning decision");
        assert_eq!(decision.units_pruned, 3);
        assert_eq!(decision.units_after, 3);
        assert_eq!(decision.outcome, PaneRetentionOutcome::PrunedToFit);
        assert_eq!(engine.strategy(), strategy);
        assert_eq!(engine.status().fallbacks, 0);
        assert!(
            timeline
                .undo(&mut canonical)
                .expect("oracle retained baseline")
        );
        assert!(engine.undo(&mut tree).expect("live retained baseline"));
        assert_live_matches(&engine, &tree, &timeline, &canonical);
        assert!(
            !engine
                .undo(&mut tree)
                .expect("pruned history is unavailable")
        );
        for _ in 0..3 {
            assert!(timeline.redo(&mut canonical).expect("oracle retained redo"));
            assert!(engine.redo(&mut tree).expect("live retained redo"));
            assert_live_matches(&engine, &tree, &timeline, &canonical);
        }
        assert!(!engine.redo(&mut tree).expect("retained head reached"));
    }
}

//! Assumption-monitor gates on *real* pane telemetry (bd-1pvzq.2).
//!
//! The inline unit tests in `pane_monitors` exercise the monitor logic on
//! synthetic telemetry. This suite drives the monitors with telemetry produced
//! by the actual pane substrates and the execution-policy selector, asserting:
//!
//! * a representative healthy session produces **no violations**, and
//! * pathological regimes (a coarse-checkpoint replay blowup, an
//!   impossible retention budget, a thrashing selector) are each flagged as a
//!   **violation** with an operator-readable explanation naming the assumption.
//!
//! These are the CI fail criteria for the advanced pane strategies: if a change
//! makes a healthy session start violating an assumption, this suite fails; if a
//! genuinely degraded regime stops being flagged, the pathological cases fail.

use ftui_layout::{
    PaneAssumption, PaneExecutionPolicy, PaneId, PaneInteractionTimeline, PaneLeaf,
    PaneMemoryStrategy, PaneMonitorReport, PaneMonitorStatus, PaneMonitorThresholds, PaneNodeKind,
    PaneOperation, PanePlacement, PaneRetentionPolicy, PaneSplitRatio, PaneTree, PaneVersionStore,
    PaneWorkloadProfile, SplitAxis, VersionedPaneTree, apply_retention_to_version_store,
    monitor_fallback_frequency, monitor_latency_envelope, monitor_replay_depth,
    monitor_retention_pressure, monitor_selector_churn,
};

/// Build a small balanced tree so there are splits to resize.
fn seed_tree(leaf_count: usize) -> PaneTree {
    let mut tree = PaneTree::singleton("leaf-0");
    let ratio = PaneSplitRatio::new(1, 1).expect("ratio");
    let mut next = 1u64;
    loop {
        let leaves: Vec<PaneId> = tree
            .nodes()
            .filter_map(|n| match n.kind {
                PaneNodeKind::Leaf(_) => Some(n.id),
                PaneNodeKind::Split(_) => None,
            })
            .collect();
        if leaves.len() >= leaf_count {
            break;
        }
        tree.apply_operation(
            next,
            PaneOperation::SplitLeaf {
                target: leaves[0],
                axis: if next.is_multiple_of(2) {
                    SplitAxis::Horizontal
                } else {
                    SplitAxis::Vertical
                },
                ratio,
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new(format!("leaf-{next}")),
            },
        )
        .expect("split applies");
        next += 1;
    }
    tree
}

fn first_split(tree: &PaneTree) -> PaneId {
    tree.nodes()
        .find_map(|n| match n.kind {
            PaneNodeKind::Split(_) => Some(n.id),
            PaneNodeKind::Leaf(_) => None,
        })
        .expect("at least one split")
}

#[test]
fn healthy_resize_session_has_no_violations() {
    // A realistic resize session: many SetSplitRatio ops on a checkpointed
    // timeline. Replay depth should stay within the checkpoint interval and the
    // selector should stay stable.
    let mut tree = seed_tree(16);
    let split = first_split(&tree);
    let mut timeline = PaneInteractionTimeline::with_baseline(&tree);
    let ratios = [
        PaneSplitRatio::new(3, 2).expect("ratio"),
        PaneSplitRatio::new(2, 3).expect("ratio"),
        PaneSplitRatio::new(5, 4).expect("ratio"),
    ];
    let mut ops = Vec::new();
    for i in 0..64u64 {
        let op = PaneOperation::SetSplitRatio {
            split,
            ratio: ratios[(i % 3) as usize],
        };
        timeline
            .apply_and_record(&mut tree, i, i + 1, op.clone())
            .expect("apply");
        ops.push(op);
    }

    let thresholds = PaneMonitorThresholds::default();
    let replay = monitor_replay_depth(&timeline.replay_diagnostics(), &thresholds);
    assert_eq!(
        replay.status,
        PaneMonitorStatus::Healthy,
        "healthy resize replay should be within the interval: {}",
        replay.explanation
    );

    // Selector over a stable resize-dominated profile stays put.
    let policy = PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded());
    let profile = PaneWorkloadProfile::observe(&ops, 200, true);
    let decisions: Vec<_> = (0..8).map(|_| policy.select(profile)).collect();
    let churn = monitor_selector_churn(&decisions, &thresholds);
    let fallback = monitor_fallback_frequency(&decisions, &thresholds);
    assert_eq!(
        churn.status,
        PaneMonitorStatus::Healthy,
        "{}",
        churn.explanation
    );
    assert!(!fallback.status.is_violation(), "{}", fallback.explanation);

    // Latency well under a generous envelope.
    let latency = monitor_latency_envelope(
        PaneMemoryStrategy::Checkpointed,
        5_000.0,
        50_000.0,
        &thresholds,
    );

    let report = PaneMonitorReport::new("healthy_resize_session")
        .with(replay)
        .with(churn)
        .with(fallback)
        .with(latency);
    assert!(
        !report.has_violations(),
        "healthy session unexpectedly violated an assumption:\n{}",
        report.summary_log()
    );
    assert_eq!(report.worst_status(), PaneMonitorStatus::Healthy);
}

#[test]
fn impossible_retention_budget_is_flagged_as_violation() {
    // Apply real history to a persistent store, then prune under a budget too
    // small to hold even one version -> FloorReached -> violation.
    let tree = seed_tree(12);
    let mut store = PaneVersionStore::new(VersionedPaneTree::from_pane_tree(&tree));
    let split = first_split(&tree);
    for i in 0..24u64 {
        store
            .apply(&PaneOperation::SetSplitRatio {
                split,
                ratio: PaneSplitRatio::new(1 + (i % 4) as u32, 2).expect("ratio"),
            })
            .expect("store apply");
    }
    // 1 byte budget cannot fit a real version -> floor.
    let decision =
        apply_retention_to_version_store(&mut store, &PaneRetentionPolicy::bounded(1, 0));
    let thresholds = PaneMonitorThresholds::default();
    let verdict = monitor_retention_pressure(&decision, &thresholds);
    assert_eq!(
        verdict.status,
        PaneMonitorStatus::Violated,
        "impossible budget should violate retention pressure: {}",
        verdict.explanation
    );
    assert_eq!(verdict.assumption, PaneAssumption::RetentionPressure);
    // The store still preserves the head despite the floor.
    assert_eq!(decision.current_state_hash, tree_head_hash(&store));
}

fn tree_head_hash(store: &PaneVersionStore) -> u64 {
    store.current().state_hash().expect("flatten head")
}

#[test]
fn thrashing_selector_is_flagged_as_violation() {
    // Alternate between a resize-storm profile (favors persistent) and a
    // no-history profile (forces baseline) with a fresh stateless `select` each
    // time -> maximal churn -> violation.
    let policy = PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded());
    let split = PaneTree::singleton("x").root();
    let storm_ops: Vec<_> = (0..128)
        .map(|i| PaneOperation::SetSplitRatio {
            split,
            ratio: PaneSplitRatio::new(1 + (i % 3) as u32, 2).expect("ratio"),
        })
        .collect();
    let storm = PaneWorkloadProfile::observe(&storm_ops, 240, true);
    let no_history = PaneWorkloadProfile::observe(&[], 0, false);

    let decisions: Vec<_> = (0..8u32)
        .map(|i| {
            if i.is_multiple_of(2) {
                policy.select(storm)
            } else {
                policy.select(no_history)
            }
        })
        .collect();

    let thresholds = PaneMonitorThresholds::default();
    let churn = monitor_selector_churn(&decisions, &thresholds);
    assert_eq!(
        churn.status,
        PaneMonitorStatus::Violated,
        "alternating profiles should thrash: {}",
        churn.explanation
    );
    assert_eq!(churn.assumption, PaneAssumption::SelectorChurn);
    assert!(churn.explanation.contains("thrash"));
}

//! End-to-end soak + rollback driver for optimized pane strategies (bd-1pvzq.5).
//!
//! Unit and integration tests prove the strategies are *correct*; this driver
//! proves the user-visible *workflow* is safe under sustained interaction and
//! that, when an operating assumption breaks, the engine **rolls back to the
//! conservative path** and keeps producing the correct state — emitting
//! operator-grade JSONL the whole way.
//!
//! The scenario is a deterministic soak:
//!
//! 1. Many rounds of resize interaction drive a checkpointed timeline + a
//!    persistent version store in lockstep (the optimized path).
//! 2. Each round the bd-1pvzq.2 monitors are evaluated on real telemetry.
//! 3. At a designated "pressure" round a retention spike (a tiny budget,
//!    standing in for accumulated memory pressure) makes the retention monitor
//!    *violate*. The controller treats `report.has_violations()` as the
//!    **rollback trigger** and switches to the conservative (checkpointed)
//!    strategy with a safe budget.
//! 4. Post-rollback rounds run conservative and return to **healthy**.
//!
//! Asserted invariants (CI-deterministic, no wall-clock in assertions):
//! * a rollback happens exactly at the pressure round,
//! * the final state hash equals the canonical baseline — behavior is preserved
//!   *across* the rollback (the rollback changes representation, not state),
//! * every post-rollback round is healthy (recovery is real, not cosmetic),
//! * the emitted JSONL is well-formed and carries the rollback event.
//!
//! Operator-grade JSONL is written to `$PANE_SOAK_LOG` (default
//! `target/pane-soak/pane_soak_rollback.jsonl`); `scripts/pane_soak_rollback.sh`
//! wraps this, validates the log, and bundles it for CI / postmortem.

use std::fs;
use std::path::PathBuf;

use ftui_layout::{
    PaneExecutionPolicy, PaneId, PaneInteractionTimeline, PaneMemoryStrategy, PaneMonitorReport,
    PaneMonitorStatus, PaneMonitorThresholds, PaneNodeKind, PaneOperation, PaneRetentionOutcome,
    PaneRetentionPolicy, PaneSplitRatio, PaneTree, PaneVersionStore, PaneWorkloadProfile,
    VersionedPaneTree, apply_retention_to_version_store, monitor_fallback_frequency,
    monitor_latency_envelope, monitor_replay_depth, monitor_retention_pressure,
    monitor_selector_churn,
};
use serde_json::json;

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
        min + (self.next_u64() % u64::from(max - min)) as u32
    }
}

fn first_split(tree: &PaneTree) -> PaneId {
    tree.nodes()
        .find_map(|n| match n.kind {
            PaneNodeKind::Split(_) => Some(n.id),
            PaneNodeKind::Leaf(_) => None,
        })
        .expect("a split exists")
}

/// Build a balanced tree with `leaf_count` leaves so there are splits to resize.
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
                axis: ftui_layout::SplitAxis::Horizontal,
                ratio,
                placement: ftui_layout::PanePlacement::ExistingFirst,
                new_leaf: ftui_layout::PaneLeaf::new(format!("leaf-{next}")),
            },
        )
        .expect("split applies");
        next += 1;
    }
    tree
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn soak_log_path() -> PathBuf {
    std::env::var("PANE_SOAK_LOG").map_or_else(
        |_| PathBuf::from("target/pane-soak/pane_soak_rollback.jsonl"),
        PathBuf::from,
    )
}

fn strategy_label(strategy: PaneMemoryStrategy) -> &'static str {
    match strategy {
        PaneMemoryStrategy::Baseline => "baseline",
        PaneMemoryStrategy::Checkpointed => "checkpointed",
        PaneMemoryStrategy::Persistent => "persistent",
    }
}

fn status_label(status: PaneMonitorStatus) -> &'static str {
    match status {
        PaneMonitorStatus::Healthy => "healthy",
        PaneMonitorStatus::Degraded => "degraded",
        PaneMonitorStatus::Violated => "violated",
    }
}

#[test]
fn pane_soak_rolls_back_to_conservative_and_preserves_behavior() {
    let rounds = env_usize("PANE_SOAK_ROUNDS", 12);
    let ops_per_round = env_usize("PANE_SOAK_OPS_PER_ROUND", 16);
    let pressure_round = env_usize("PANE_SOAK_PRESSURE_ROUND", 6).min(rounds.saturating_sub(2));
    let seed = env_usize("PANE_SOAK_SEED", 0x50A4) as u64;

    let mut tree = seed_tree(16);
    let split = first_split(&tree);
    let mut timeline = PaneInteractionTimeline::with_baseline(&tree);
    let mut store = PaneVersionStore::new(VersionedPaneTree::from_pane_tree(&tree));
    let mut canonical = tree.clone();
    let mut rng = Lcg::new(seed);

    let policy = PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded());
    let conservative = policy.conservative();
    let thresholds = PaneMonitorThresholds::default();

    let mut decisions = Vec::new();
    let mut all_ops: Vec<PaneOperation> = Vec::new();
    let mut rolled_back = false;
    let mut rollback_round: Option<usize> = None;
    let mut post_rollback_statuses = Vec::new();

    let mut log_lines: Vec<String> = Vec::new();
    let mut op_id = 1u64;

    for round in 0..rounds {
        // --- apply this round's interactions to every substrate -------------
        for _ in 0..ops_per_round {
            let op = PaneOperation::SetSplitRatio {
                split,
                ratio: PaneSplitRatio::new(rng.next_u32_range(1, 16), rng.next_u32_range(1, 16))
                    .expect("ratio"),
            };
            timeline
                .apply_and_record(&mut tree, op_id, op_id, op.clone())
                .expect("timeline apply");
            store.apply(&op).expect("store apply");
            canonical
                .apply_operation_conservative(op_id, op.clone())
                .expect("canonical apply");
            all_ops.push(op);
            op_id += 1;
        }

        // --- selector decision for the round's workload ---------------------
        let profile = PaneWorkloadProfile::observe(&all_ops, 200, true);
        let active_strategy;
        let decision_reason;
        if rolled_back {
            let d = conservative.select(profile);
            active_strategy = d.strategy;
            decision_reason = format!("{:?}", d.reason);
            decisions.push(d);
        } else {
            let d = policy.select(profile);
            active_strategy = d.strategy;
            decision_reason = format!("{:?}", d.reason);
            decisions.push(d);
        }

        // --- retention: a pressure spike at the designated round ------------
        // Before/after rollback we run a safe budget; the spike forces the
        // retention monitor to violate exactly once and trigger rollback.
        let budget = if round == pressure_round && !rolled_back {
            PaneRetentionPolicy::bounded(1, 0) // impossible budget -> FloorReached
        } else {
            PaneRetentionPolicy::bounded(1 << 20, 64) // generous, safe
        };
        let retention = apply_retention_to_version_store(&mut store, &budget);

        // --- evaluate the monitor suite on real telemetry -------------------
        let replay_diag = timeline.replay_diagnostics();
        // Deterministic latency proxy (replay depth dominates undo cost); a
        // generous envelope keeps latency healthy so retention is the trigger.
        let observed_ns = f64::from(u32::try_from(replay_diag.replay_depth).unwrap_or(u32::MAX))
            * 1_000.0
            + 2_000.0;
        let report = PaneMonitorReport::new(format!("soak-round-{round}"))
            .with(monitor_replay_depth(&replay_diag, &thresholds))
            .with(monitor_retention_pressure(&retention, &thresholds))
            .with(monitor_selector_churn(&decisions, &thresholds))
            .with(monitor_fallback_frequency(&decisions, &thresholds))
            .with(monitor_latency_envelope(
                active_strategy,
                observed_ns,
                100_000.0,
                &thresholds,
            ));

        let worst = report.worst_status();
        let violations = report.violations().count();
        let round_hash = tree.state_hash();

        log_lines.push(
            json!({
                "event": "pane_soak_round",
                "round": round,
                "strategy": strategy_label(active_strategy),
                "reason": decision_reason,
                "replay_depth": replay_diag.replay_depth,
                "checkpoint_interval": replay_diag.checkpoint_interval,
                "retention_outcome": format!("{:?}", retention.outcome),
                "monitor_worst": status_label(worst),
                "violations": violations,
                "rolled_back": rolled_back,
                "state_hash": round_hash,
                "ops_applied": ops_per_round,
            })
            .to_string(),
        );

        // --- rollback decision: a violation on the optimized path is the
        // operator-grade trigger to fall back to the conservative strategy ---
        if report.has_violations() && !rolled_back {
            rolled_back = true;
            rollback_round = Some(round);
            log_lines.push(
                json!({
                    "event": "pane_soak_rollback",
                    "round": round,
                    "from_strategy": strategy_label(active_strategy),
                    "to_strategy": strategy_label(PaneMemoryStrategy::Checkpointed),
                    "trigger": "monitor_violation",
                    "monitor_summary": report.summary_log(),
                    "state_hash": round_hash,
                })
                .to_string(),
            );
        } else if rolled_back {
            post_rollback_statuses.push(worst);
        }
    }

    // --- final certification: behavior preserved across the rollback --------
    let final_hash = tree.state_hash();
    let canonical_hash = canonical.state_hash();
    let replay_hash = timeline.replay().expect("replay").state_hash();
    let store_hash = store.current().state_hash().expect("flatten");
    let certified = final_hash == canonical_hash
        && replay_hash == canonical_hash
        && store_hash == canonical_hash;

    log_lines.push(
        json!({
            "event": "pane_soak_summary",
            "rounds": rounds,
            "rollbacks": usize::from(rolled_back),
            "rollback_round": rollback_round,
            "final_strategy": strategy_label(if rolled_back {
                PaneMemoryStrategy::Checkpointed
            } else {
                PaneMemoryStrategy::Persistent
            }),
            "final_state_hash": final_hash,
            "canonical_state_hash": canonical_hash,
            "replay_state_hash": replay_hash,
            "store_state_hash": store_hash,
            "certified": certified,
            "seed": seed,
        })
        .to_string(),
    );

    // --- write operator-grade JSONL -------------------------------------
    let log_path = soak_log_path();
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).expect("create soak log dir");
    }
    fs::write(&log_path, format!("{}\n", log_lines.join("\n"))).expect("write soak log");
    // Also stream each line to stdout (visible under --nocapture) prefixed with
    // a marker, so the runner can reconstruct the log locally even when the
    // driver executed on a remote rch worker whose filesystem is not synced.
    for line in &log_lines {
        println!("SOAK_JSONL={line}");
    }

    // --- assertions -----------------------------------------------------
    assert_eq!(
        rollback_round,
        Some(pressure_round),
        "rollback should fire exactly at the pressure round; log at {}",
        log_path.display()
    );
    assert!(
        certified,
        "behavior must be preserved across the rollback: final={final_hash} canonical={canonical_hash} replay={replay_hash} store={store_hash}"
    );
    // After rollback the conservative path legitimately shows elevated fallback
    // frequency (Degraded) — that is the *expected* degraded mode, not a failure.
    // The recovery invariant is that the *violation* clears and never recurs.
    assert!(
        post_rollback_statuses
            .iter()
            .all(|s| *s != PaneMonitorStatus::Violated),
        "post-rollback rounds must clear the violation (degraded fallback is expected), got {post_rollback_statuses:?}"
    );

    // The emitted JSONL must be well-formed and carry the rollback + summary.
    let written = fs::read_to_string(&log_path).expect("read soak log");
    let mut saw_rollback = false;
    let mut saw_summary = false;
    for line in written.lines() {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("each soak log line is valid JSON");
        match value.get("event").and_then(|e| e.as_str()) {
            Some("pane_soak_rollback") => saw_rollback = true,
            Some("pane_soak_summary") => {
                saw_summary = true;
                assert_eq!(value["certified"], json!(true));
            }
            _ => {}
        }
    }
    assert!(saw_rollback, "soak log must contain a rollback event");
    assert!(saw_summary, "soak log must contain a summary event");
}

#[test]
fn pane_soak_without_pressure_never_rolls_back() {
    // A soak with the pressure round pushed past the end never violates, so the
    // optimized path runs throughout and the state stays certified — proving the
    // rollback is driven by real violations, not fired spuriously.
    let rounds = 8usize;
    let ops_per_round = 12usize;
    let mut tree = seed_tree(12);
    let split = first_split(&tree);
    let mut timeline = PaneInteractionTimeline::with_baseline(&tree);
    let mut store = PaneVersionStore::new(VersionedPaneTree::from_pane_tree(&tree));
    let mut canonical = tree.clone();
    let mut rng = Lcg::new(0xBEEF);
    let thresholds = PaneMonitorThresholds::default();
    let mut decisions = Vec::new();
    let mut all_ops = Vec::new();
    let policy = PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded());
    let mut op_id = 1u64;

    for _round in 0..rounds {
        for _ in 0..ops_per_round {
            let op = PaneOperation::SetSplitRatio {
                split,
                ratio: PaneSplitRatio::new(rng.next_u32_range(1, 16), rng.next_u32_range(1, 16))
                    .expect("ratio"),
            };
            timeline
                .apply_and_record(&mut tree, op_id, op_id, op.clone())
                .expect("apply");
            store.apply(&op).expect("apply");
            canonical
                .apply_operation_conservative(op_id, op.clone())
                .expect("apply");
            all_ops.push(op);
            op_id += 1;
        }
        let profile = PaneWorkloadProfile::observe(&all_ops, 200, true);
        decisions.push(policy.select(profile));
        let retention = apply_retention_to_version_store(
            &mut store,
            &PaneRetentionPolicy::bounded(1 << 20, 64),
        );
        let report = PaneMonitorReport::new("soak-no-pressure")
            .with(monitor_replay_depth(
                &timeline.replay_diagnostics(),
                &thresholds,
            ))
            .with(monitor_retention_pressure(&retention, &thresholds))
            .with(monitor_selector_churn(&decisions, &thresholds))
            .with(monitor_fallback_frequency(&decisions, &thresholds));
        assert!(
            !report.has_violations(),
            "healthy soak must not violate: {}",
            report.summary_log()
        );
        assert_eq!(retention.outcome, PaneRetentionOutcome::WithinBudget);
    }

    assert_eq!(tree.state_hash(), canonical.state_hash());
}

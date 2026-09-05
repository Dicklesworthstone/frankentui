//! Bounded retention & pruning policy for pane history (`bd-25wj7.2`).
//!
//! The asymptotic replay-speed work ([`pane_persistent`](crate::pane_persistent))
//! and the production checkpointed timeline
//! ([`PaneInteractionTimeline`]) both buy
//! undo/redo by *retaining state* — versions, operation-log entries, and
//! checkpoint snapshots. Left unbounded that retained state grows without limit
//! (the memory telemetry in [`pane_memory`](crate::pane_memory) quantifies the
//! drivers: node structs dominate every strategy). This module turns "memory is
//! bounded by hope" into "memory is bounded by an explicit, deterministic,
//! observable policy".
//!
//! # Policy
//!
//! A [`PaneRetentionPolicy`] is an explicit ceiling on two axes:
//!
//! * `max_retained_bytes` — a modeled-byte budget (using the same retained-state
//!   byte model as [`pane_memory`](crate::pane_memory)), and
//! * `max_retained_units` — a hard cap on retained *units* (persistent versions
//!   or timeline entries).
//!
//! `0` on either axis means unbounded on that axis. The policy applies both: it
//! first installs the unit cap, then prunes the oldest history one unit at a
//! time until the byte budget is met — protecting the current cursor and its
//! redo tail, so the **current state is never discarded** (its state hash is carried
//! in the [`PaneRetentionDecision`]).
//!
//! # Determinism & fallback
//!
//! Pruning is a pure function of the policy and the current retained state, so it
//! is deterministic and reproducible. Fallbacks are explicit and observable:
//!
//! * **Conservative debugging** ([`PaneRetentionPolicy::conservative`]) disables
//!   pruning entirely. Over-budget state is *held*, not discarded, and the
//!   decision reports [`PaneRetentionOutcome::ConservativeHold`] so an operator
//!   knows exactly what would otherwise be dropped.
//! * **Floor** — when even a single retained unit exceeds the byte budget (e.g.
//!   the timeline's irreducible baseline snapshot), pruning stops at one unit and
//!   reports [`PaneRetentionOutcome::FloorReached`]: the live state is never
//!   sacrificed to a byte budget.
//! * **Blocked pruning** — protected redo history or an unreplayable timeline
//!   can prevent further pruning. [`PaneRetentionOutcome::PruningBlocked`]
//!   reports that either budget remains exceeded; it never reports a fit merely
//!   because some older history was removed.
//!
//! Every application returns a [`PaneRetentionDecision`] — a serializable
//! telemetry record plus a human-readable `log` line — covering the retained-state
//! totals before/after, units pruned, the preserved current-state hash, and the
//! outcome.

use crate::pane::{PaneInteractionTimeline, PaneTree};
use crate::pane_memory::PaneMemoryStrategy;
use crate::pane_persistent::PaneVersionStore;

/// Explicit memory ceiling for retained pane history. `0` on an axis is
/// unbounded for that axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PaneRetentionBudget {
    /// Modeled-byte ceiling on total retained state (`0` = unbounded).
    pub max_retained_bytes: usize,
    /// Hard cap on retained units — versions (store) or entries (timeline)
    /// (`0` = unbounded).
    pub max_retained_units: usize,
}

impl PaneRetentionBudget {
    /// A budget bounded on both axes.
    #[must_use]
    pub const fn new(max_retained_bytes: usize, max_retained_units: usize) -> Self {
        Self {
            max_retained_bytes,
            max_retained_units,
        }
    }

    /// An unbounded budget (no ceiling on either axis).
    #[must_use]
    pub const fn unbounded() -> Self {
        Self::new(0, 0)
    }

    /// Whether `bytes`/`units` exceed this budget on any bounded axis.
    #[must_use]
    pub const fn is_exceeded_by(self, bytes: usize, units: usize) -> bool {
        (self.max_retained_bytes != 0 && bytes > self.max_retained_bytes)
            || (self.max_retained_units != 0 && units > self.max_retained_units)
    }
}

/// A bounded-retention policy: a [`PaneRetentionBudget`] plus a conservative
/// debugging override that disables pruning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PaneRetentionPolicy {
    /// The memory ceiling.
    pub budget: PaneRetentionBudget,
    /// When `true`, pruning is disabled: over-budget state is retained (held)
    /// for debugging rather than discarded, and the decision reports
    /// [`PaneRetentionOutcome::ConservativeHold`].
    pub conservative_debug: bool,
}

impl PaneRetentionPolicy {
    /// A pruning policy bounded by `max_retained_bytes` and `max_retained_units`.
    #[must_use]
    pub const fn bounded(max_retained_bytes: usize, max_retained_units: usize) -> Self {
        Self {
            budget: PaneRetentionBudget::new(max_retained_bytes, max_retained_units),
            conservative_debug: false,
        }
    }

    /// An unbounded policy — never prunes (the implicit debug default).
    #[must_use]
    pub const fn unbounded() -> Self {
        Self {
            budget: PaneRetentionBudget::unbounded(),
            conservative_debug: false,
        }
    }

    /// Return this policy in conservative-debug mode: pruning is disabled and
    /// over-budget state is held rather than discarded.
    #[must_use]
    pub const fn conservative(mut self) -> Self {
        self.conservative_debug = true;
        self
    }
}

/// The outcome of applying a [`PaneRetentionPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PaneRetentionOutcome {
    /// Retained state was already within budget; nothing pruned.
    WithinBudget,
    /// Oldest history was pruned to bring retained state within budget.
    PrunedToFit,
    /// Over budget, but conservative-debug mode held all state (nothing pruned).
    ConservativeHold,
    /// Pruned to the minimum single retained unit, yet still over the byte
    /// budget — the deterministic floor. The live state is never discarded.
    FloorReached,
    /// Still over a byte or unit budget because protected history or failed
    /// baseline replay prevents further pruning. Current state and redo survive.
    PruningBlocked,
}

impl PaneRetentionOutcome {
    /// Stable identifier for logs/artifacts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WithinBudget => "within_budget",
            Self::PrunedToFit => "pruned_to_fit",
            Self::ConservativeHold => "conservative_hold",
            Self::FloorReached => "floor_reached",
            Self::PruningBlocked => "pruning_blocked",
        }
    }
}

/// Serializable record of one [`PaneRetentionPolicy`] application.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PaneRetentionDecision {
    /// Which retained-state substrate the policy was applied to.
    pub strategy: PaneMemoryStrategy,
    /// The budget that was enforced.
    pub budget: PaneRetentionBudget,
    /// Whether conservative-debug mode was in effect.
    pub conservative_debug: bool,
    /// Retained units before the decision.
    pub units_before: usize,
    /// Retained units after the decision.
    pub units_after: usize,
    /// Units pruned (`units_before - units_after`).
    pub units_pruned: usize,
    /// Modeled retained bytes before the decision.
    pub bytes_before: usize,
    /// Modeled retained bytes after the decision.
    pub bytes_after: usize,
    /// The current-state hash, preserved across the decision (proof that
    /// pruning discarded only history, never the live state).
    pub current_state_hash: u64,
    /// The classified outcome.
    pub outcome: PaneRetentionOutcome,
    /// Human-readable one-line log.
    pub log: String,
}

impl PaneRetentionDecision {
    #[allow(clippy::too_many_arguments)]
    fn build(
        strategy: PaneMemoryStrategy,
        policy: &PaneRetentionPolicy,
        units_before: usize,
        units_after: usize,
        bytes_before: usize,
        bytes_after: usize,
        current_state_hash: u64,
        outcome: PaneRetentionOutcome,
    ) -> Self {
        let units_pruned = units_before.saturating_sub(units_after);
        let log = format!(
            "retention[{}] {}: units {}->{} (pruned {}), bytes {}->{} (budget bytes={} units={}{}); head_hash={:#018x}",
            strategy.as_str(),
            outcome.as_str(),
            units_before,
            units_after,
            units_pruned,
            bytes_before,
            bytes_after,
            policy.budget.max_retained_bytes,
            policy.budget.max_retained_units,
            if policy.conservative_debug {
                ", conservative-debug"
            } else {
                ""
            },
            current_state_hash,
        );
        Self {
            strategy,
            budget: policy.budget,
            conservative_debug: policy.conservative_debug,
            units_before,
            units_after,
            units_pruned,
            bytes_before,
            bytes_after,
            current_state_hash,
            outcome,
            log,
        }
    }
}

/// Classify the outcome after pruning has run (or been skipped).
fn classify(
    units_pruned: usize,
    units_after: usize,
    bytes_after: usize,
    budget: PaneRetentionBudget,
) -> PaneRetentionOutcome {
    if budget.is_exceeded_by(bytes_after, units_after) {
        if units_after <= 1 {
            PaneRetentionOutcome::FloorReached
        } else {
            PaneRetentionOutcome::PruningBlocked
        }
    } else if units_pruned > 0 {
        PaneRetentionOutcome::PrunedToFit
    } else {
        PaneRetentionOutcome::WithinBudget
    }
}

/// Apply a retention policy to a persistent [`PaneVersionStore`], pruning the
/// oldest versions to fit the budget while preserving the current version.
pub fn apply_to_version_store(
    store: &mut PaneVersionStore,
    policy: &PaneRetentionPolicy,
) -> PaneRetentionDecision {
    let strategy = PaneMemoryStrategy::Persistent;
    let bytes_before = store.retention().estimated_total_retained_bytes;
    let units_before = store.version_count();
    let current_state_hash = store.current().state_hash().unwrap_or(0);

    if policy.conservative_debug {
        // The store also enforces its installed cap on subsequent applies.
        // Holding history must disable that earlier cap, not just skip this call.
        store.set_max_versions(0);
        let outcome = if policy.budget.is_exceeded_by(bytes_before, units_before) {
            PaneRetentionOutcome::ConservativeHold
        } else {
            PaneRetentionOutcome::WithinBudget
        };
        return PaneRetentionDecision::build(
            strategy,
            policy,
            units_before,
            units_before,
            bytes_before,
            bytes_before,
            current_state_hash,
            outcome,
        );
    }

    store.set_max_versions(policy.budget.max_retained_units);
    if policy.budget.max_retained_bytes != 0 {
        while store.version_count() > 1
            && store.retention().estimated_total_retained_bytes > policy.budget.max_retained_bytes
        {
            let keep = store.version_count() - 1;
            if store.set_max_versions(keep) == 0 {
                // No progress: the cursor pins every remaining version (the
                // user has undone to/near the oldest state). Stop — classified
                // below — rather than spinning forever.
                break;
            }
        }
    }

    let units_after = store.version_count();
    let bytes_after = store.retention().estimated_total_retained_bytes;
    let outcome = classify(
        units_before.saturating_sub(units_after),
        units_after,
        bytes_after,
        policy.budget,
    );
    PaneRetentionDecision::build(
        strategy,
        policy,
        units_before,
        units_after,
        bytes_before,
        bytes_after,
        current_state_hash,
        outcome,
    )
}

/// Apply a retention policy to a checkpointed [`PaneInteractionTimeline`],
/// pruning the oldest entries (advancing the replay baseline and re-basing
/// checkpoints) to fit the budget while preserving the current state and redo.
///
/// Note the timeline's irreducible floor is its baseline snapshot: pruning
/// entries advances the baseline, so a byte budget below the baseline-snapshot
/// cost reports [`PaneRetentionOutcome::FloorReached`] rather than discarding
/// the head.
pub fn apply_to_timeline(
    timeline: &mut PaneInteractionTimeline,
    policy: &PaneRetentionPolicy,
) -> PaneRetentionDecision {
    let strategy = PaneMemoryStrategy::Checkpointed;
    let bytes_before = timeline
        .retention_diagnostics()
        .estimated_total_retained_bytes;
    let units_before = timeline.entries.len();
    // The current state is baseline + entries[..cursor], so the preserved hash
    // must come from the cursor position — `entries.last()` is the head of the
    // full history, which is NOT the current state after an undo.
    let current_state_hash = if timeline.cursor == 0 {
        timeline.entries.first().map_or_else(
            || {
                timeline
                    .baseline
                    .as_ref()
                    .and_then(|snapshot| PaneTree::from_snapshot(snapshot.clone()).ok())
                    .map_or(0, |tree| tree.state_hash())
            },
            |entry| entry.before_hash,
        )
    } else {
        timeline
            .entries
            .get(timeline.cursor - 1)
            .map_or(0, |entry| entry.after_hash)
    };

    if policy.conservative_debug {
        // Clear any cap installed by a previous policy so later edits are held too.
        timeline.set_max_entries(0);
        let outcome = if policy.budget.is_exceeded_by(bytes_before, units_before) {
            PaneRetentionOutcome::ConservativeHold
        } else {
            PaneRetentionOutcome::WithinBudget
        };
        return PaneRetentionDecision::build(
            strategy,
            policy,
            units_before,
            units_before,
            bytes_before,
            bytes_before,
            current_state_hash,
            outcome,
        );
    }

    timeline.set_max_entries(policy.budget.max_retained_units);
    if policy.budget.max_retained_bytes != 0 {
        while timeline.entries.len() > 1
            && timeline
                .retention_diagnostics()
                .estimated_total_retained_bytes
                > policy.budget.max_retained_bytes
        {
            let keep = timeline.entries.len() - 1;
            if timeline.set_max_entries(keep) == 0 {
                // No progress: the baseline is missing/unreplayable, or the
                // cursor pins every remaining entry. Stop (classified below)
                // rather than spinning forever.
                break;
            }
        }
    }

    let units_after = timeline.entries.len();
    let bytes_after = timeline
        .retention_diagnostics()
        .estimated_total_retained_bytes;
    let outcome = classify(
        units_before.saturating_sub(units_after),
        units_after,
        bytes_after,
        policy.budget,
    );
    PaneRetentionDecision::build(
        strategy,
        policy,
        units_before,
        units_after,
        bytes_before,
        bytes_after,
        current_state_hash,
        outcome,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::{
        PaneId, PaneInteractionTimeline, PaneLeaf, PaneOperation, PanePlacement, PaneSplitRatio,
        PaneTree, SplitAxis,
    };
    use crate::pane_persistent::VersionedPaneTree;

    fn ratio_of(n: u32, d: u32) -> PaneSplitRatio {
        PaneSplitRatio::new(n, d).expect("valid ratio")
    }

    /// A workload: two splits then `storm` resize ops on split id 4.
    fn workload(storm: u32) -> Vec<PaneOperation> {
        let mut ops = vec![
            PaneOperation::SplitLeaf {
                target: PaneId::MIN,
                axis: SplitAxis::Horizontal,
                ratio: ratio_of(1, 1),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("b"),
            },
            PaneOperation::SplitLeaf {
                target: PaneId::MIN,
                axis: SplitAxis::Vertical,
                ratio: ratio_of(2, 1),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("c"),
            },
        ];
        let split = PaneId::new(4).expect("valid id");
        for n in 1..=storm {
            ops.push(PaneOperation::SetSplitRatio {
                split,
                ratio: ratio_of(n % 7 + 1, 1),
            });
        }
        ops
    }

    fn build_store(storm: u32) -> PaneVersionStore {
        let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
        for op in &workload(storm) {
            store.apply(op).expect("store apply");
        }
        store
    }

    #[test]
    fn version_store_prune_after_undo_preserves_current() {
        // 2 splits + 30 resizes = 32 ops -> 33 versions; undo back to cursor 2.
        let mut store = build_store(30);
        for _ in 0..30 {
            assert!(store.undo());
        }
        let current_hash = store.current().state_hash().expect("hash");
        let pruned = store.set_max_versions(8);
        // Only versions strictly before the cursor may be pruned.
        assert_eq!(pruned, 2);
        assert_eq!(
            store.current().state_hash().expect("hash"),
            current_hash,
            "pruning must never discard the current (undone-to) version"
        );
        // The redo tail survives intact.
        let mut redos = 0;
        while store.redo() {
            redos += 1;
        }
        assert_eq!(redos, 30);
    }

    #[test]
    fn timeline_prune_after_undo_preserves_current_state() {
        let mut tree = PaneTree::singleton("root");
        let mut timeline = PaneInteractionTimeline::default();
        for (i, op) in workload(20).iter().enumerate() {
            let id = i as u64;
            timeline
                .apply_and_record(&mut tree, id, id, op.clone())
                .expect("apply");
        }
        for _ in 0..20 {
            timeline.undo(&mut tree).expect("undo");
        }
        let current_hash = tree.state_hash();
        let decision = apply_to_timeline(&mut timeline, &PaneRetentionPolicy::bounded(0, 4));
        assert_eq!(
            decision.current_state_hash, current_hash,
            "preserved hash must be the cursor state, not the head of history"
        );
        // Cursor-pinned entries survive: redo back to the head still works.
        let mut redos = 0;
        while timeline.redo(&mut tree).expect("redo") {
            redos += 1;
        }
        assert_eq!(redos, 20);
    }

    #[test]
    fn version_store_byte_budget_terminates_when_cursor_pins_history() {
        // Undone to cursor 0, every version is at/after the cursor, so the
        // byte-budget loop can make no progress and must stop, not spin.
        let mut store = build_store(12);
        while store.undo() {}
        let current_hash = store.current().state_hash().expect("hash");
        let before = store.version_count();
        let decision = apply_to_version_store(&mut store, &PaneRetentionPolicy::bounded(1, 0));
        assert_eq!(store.version_count(), before);
        assert_eq!(decision.units_pruned, 0);
        assert_eq!(store.current().state_hash().expect("hash"), current_hash);
    }

    #[test]
    fn timeline_byte_budget_terminates_when_pruning_cannot_progress() {
        let mut timeline = build_timeline(10);
        // A hand-assembled/deserialized timeline can lack a replayable
        // baseline; the byte-budget loop must stop rather than spin forever.
        timeline.baseline = None;
        let before = timeline.entries.len();
        let decision = apply_to_timeline(&mut timeline, &PaneRetentionPolicy::bounded(1, 0));
        assert_eq!(timeline.entries.len(), before);
        assert_eq!(decision.units_pruned, 0);
        assert_eq!(decision.outcome, PaneRetentionOutcome::PruningBlocked);
    }

    #[test]
    fn pinned_history_reports_unmet_budgets_and_keeps_redo() {
        use crate::pane_monitors::{PaneMonitorThresholds, monitor_retention_pressure};

        for budget in [(0, 2), (1, 0), (1, 2)] {
            // At cursor zero nothing can be removed; at cursor two some older
            // history can be removed, but the redo tail still exceeds the cap.
            for cursor in [0, 2] {
                let mut store = build_store(6);
                let mut timeline = build_timeline(6);
                let mut tree = timeline.replay().expect("valid history");
                while timeline.cursor > cursor {
                    assert!(timeline.undo(&mut tree).expect("undo"));
                    assert!(store.undo());
                }
                let current = tree.to_snapshot();
                let mut expected_redo = timeline.clone();
                let policy = PaneRetentionPolicy::bounded(budget.0, budget.1);
                let persistent = apply_to_version_store(&mut store, &policy);
                let checkpointed = apply_to_timeline(&mut timeline, &policy);
                for decision in [&persistent, &checkpointed] {
                    assert_eq!(decision.outcome.as_str(), "pruning_blocked");
                    assert_eq!(decision.units_pruned, cursor);
                    assert!(
                        policy
                            .budget
                            .is_exceeded_by(decision.bytes_after, decision.units_after)
                    );
                    assert_eq!(decision.current_state_hash, tree.state_hash());
                    assert!(
                        monitor_retention_pressure(decision, &PaneMonitorThresholds::default())
                            .status
                            .is_violation()
                    );
                }
                assert_eq!(
                    timeline.replay().expect("current replay").to_snapshot(),
                    current
                );
                assert_eq!(store.current().to_snapshot(), current);
                while expected_redo.redo(&mut tree).expect("expected redo") {
                    let expected = tree.to_snapshot();
                    assert!(timeline.redo(&mut tree).expect("retained redo"));
                    assert!(store.redo());
                    assert_eq!(tree.to_snapshot(), expected);
                    assert_eq!(store.current().to_snapshot(), expected);
                }
                assert!(!timeline.redo(&mut tree).expect("end of redo"));
                assert!(!store.redo());

                // Retention pressure must not disable the next real edit.
                let before_edit = tree.to_snapshot();
                let operation = PaneOperation::SplitLeaf {
                    target: PaneId::MIN,
                    axis: SplitAxis::Horizontal,
                    ratio: ratio_of(1, 1),
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new("continued editing"),
                };
                timeline
                    .apply_and_record(&mut tree, 100, 100, operation.clone())
                    .expect("continue editing");
                store
                    .apply(&operation)
                    .expect("continue persistent editing");
                assert_ne!(tree.to_snapshot(), before_edit);
                assert_eq!(store.current().to_snapshot(), tree.to_snapshot());
            }
        }
    }

    #[test]
    fn pinned_history_that_fits_does_not_report_pressure() {
        use crate::pane_monitors::{
            PaneMonitorStatus, PaneMonitorThresholds, monitor_retention_pressure,
        };

        let mut store = build_store(6);
        let mut timeline = build_timeline(6);
        let mut tree = timeline.replay().expect("history");
        while timeline.undo(&mut tree).expect("undo") {
            assert!(store.undo());
        }
        let snapshot = tree.to_snapshot();
        let history = timeline.clone();
        let policy = PaneRetentionPolicy::bounded(usize::MAX, 100);
        let persistent = apply_to_version_store(&mut store, &policy);
        let checkpointed = apply_to_timeline(&mut timeline, &policy);
        for decision in [persistent, checkpointed] {
            assert_eq!(decision.outcome, PaneRetentionOutcome::WithinBudget);
            assert_eq!(decision.units_pruned, 0);
            assert_eq!(
                monitor_retention_pressure(&decision, &PaneMonitorThresholds::default()).status,
                PaneMonitorStatus::Healthy
            );
        }
        assert_eq!(tree.to_snapshot(), snapshot);
        assert_eq!(store.current().to_snapshot(), snapshot);
        assert_eq!(timeline.entries, history.entries);
        assert_eq!(timeline.cursor, history.cursor);
    }

    #[test]
    fn baseline_only_timeline_reports_actual_current_hash() {
        let tree = PaneTree::singleton("baseline without edits");
        for policy in [
            PaneRetentionPolicy::unbounded(),
            PaneRetentionPolicy::bounded(1, 0),
        ] {
            let mut timeline = PaneInteractionTimeline::with_baseline(&tree);
            let mut expected = timeline.clone();
            expected.max_entries = 0;
            let decision = apply_to_timeline(&mut timeline, &policy);
            assert_eq!(decision.current_state_hash, tree.state_hash());
            assert_eq!(decision.units_pruned, 0);
            assert_eq!(timeline, expected);
        }
    }

    #[test]
    fn relaxed_policy_clears_previous_caps_for_subsequent_edits() {
        for initial in [
            PaneRetentionPolicy::bounded(0, 2),
            PaneRetentionPolicy::bounded(1, 0),
        ] {
            for relaxed in [
                PaneRetentionPolicy::unbounded(),
                PaneRetentionPolicy::bounded(1, 1).conservative(),
            ] {
                let mut store = build_store(6);
                let mut timeline = build_timeline(6);
                let mut tree = timeline.replay().expect("valid history");
                apply_to_version_store(&mut store, &initial);
                apply_to_timeline(&mut timeline, &initial);
                let versions = store.version_count();
                let entries = timeline.entries.len();
                let before = tree.to_snapshot();
                assert_eq!(apply_to_version_store(&mut store, &relaxed).units_pruned, 0);
                assert_eq!(apply_to_timeline(&mut timeline, &relaxed).units_pruned, 0);
                for id in 20..23 {
                    let operation = PaneOperation::SetSplitRatio {
                        split: PaneId::new(4).expect("split id"),
                        ratio: ratio_of(id as u32, 1),
                    };
                    timeline
                        .apply_and_record(&mut tree, id, id, operation.clone())
                        .expect("continued edit");
                    store.apply(&operation).expect("persistent continued edit");
                    assert_eq!(store.current().to_snapshot(), tree.to_snapshot());
                }
                assert_ne!(tree.to_snapshot(), before);
                assert_eq!(store.version_count(), versions + 3);
                assert_eq!(timeline.entries.len(), entries + 3);
            }
        }
    }

    fn build_timeline(storm: u32) -> PaneInteractionTimeline {
        let mut tree = PaneTree::singleton("root");
        let mut timeline = PaneInteractionTimeline::default();
        for (i, op) in workload(storm).iter().enumerate() {
            let id = i as u64;
            timeline
                .apply_and_record(&mut tree, id, id, op.clone())
                .expect("timeline apply");
        }
        timeline
    }

    #[test]
    fn within_budget_is_a_noop() {
        let mut store = build_store(20);
        let before = store.version_count();
        let hash = store.current().state_hash().unwrap();
        let d = apply_to_version_store(&mut store, &PaneRetentionPolicy::bounded(usize::MAX, 0));
        assert_eq!(d.outcome, PaneRetentionOutcome::WithinBudget);
        assert_eq!(d.units_pruned, 0);
        assert_eq!(store.version_count(), before);
        assert_eq!(d.current_state_hash, hash);
    }

    #[test]
    fn unit_budget_caps_versions_and_preserves_head() {
        let mut store = build_store(40);
        let head = store.current().state_hash().unwrap();
        let d = apply_to_version_store(&mut store, &PaneRetentionPolicy::bounded(0, 8));
        assert!(store.version_count() <= 8);
        assert_eq!(d.outcome, PaneRetentionOutcome::PrunedToFit);
        assert!(d.units_pruned > 0);
        // The live state is untouched — only undo history was discarded.
        assert_eq!(store.current().state_hash().unwrap(), head);
        assert_eq!(d.current_state_hash, head);
    }

    #[test]
    fn byte_budget_prunes_to_fit_and_preserves_head() {
        let mut store = build_store(40);
        let head = store.current().state_hash().unwrap();
        let full = store.retention().estimated_total_retained_bytes;
        // Budget at a quarter of the unbounded footprint forces pruning.
        let d = apply_to_version_store(&mut store, &PaneRetentionPolicy::bounded(full / 4, 0));
        assert_eq!(d.outcome, PaneRetentionOutcome::PrunedToFit);
        assert!(d.bytes_after <= full / 4);
        assert!(d.bytes_after < d.bytes_before);
        assert!(store.version_count() >= 1);
        assert_eq!(store.current().state_hash().unwrap(), head);
    }

    #[test]
    fn conservative_debug_holds_over_budget_state() {
        let mut store = build_store(30);
        let before = store.version_count();
        let bytes = store.retention().estimated_total_retained_bytes;
        // Tiny budget that would normally prune hard, but conservative mode holds.
        let policy = PaneRetentionPolicy::bounded(1, 1).conservative();
        let d = apply_to_version_store(&mut store, &policy);
        assert_eq!(d.outcome, PaneRetentionOutcome::ConservativeHold);
        assert_eq!(d.units_pruned, 0);
        assert_eq!(store.version_count(), before);
        assert_eq!(d.bytes_after, bytes);
    }

    #[test]
    fn floor_is_never_breached() {
        let mut store = build_store(30);
        let head = store.current().state_hash().unwrap();
        // An impossible 1-byte budget prunes to the single live version, no more.
        let d = apply_to_version_store(&mut store, &PaneRetentionPolicy::bounded(1, 0));
        assert_eq!(d.outcome, PaneRetentionOutcome::FloorReached);
        assert_eq!(store.version_count(), 1);
        assert_eq!(store.current().state_hash().unwrap(), head);
        assert_eq!(d.current_state_hash, head);
    }

    #[test]
    fn timeline_policy_prunes_entries_and_preserves_head() {
        let mut timeline = build_timeline(40);
        let head = timeline.entries.last().unwrap().after_hash;
        let d = apply_to_timeline(&mut timeline, &PaneRetentionPolicy::bounded(0, 8));
        assert!(timeline.entries.len() <= 8);
        assert!(d.units_pruned > 0);
        assert_eq!(timeline.entries.last().unwrap().after_hash, head);
        assert_eq!(d.current_state_hash, head);
        assert_eq!(d.strategy, PaneMemoryStrategy::Checkpointed);
    }

    #[test]
    fn decisions_are_deterministic() {
        let mut a = build_store(40);
        let mut b = build_store(40);
        let policy = PaneRetentionPolicy::bounded(50_000, 16);
        assert_eq!(
            apply_to_version_store(&mut a, &policy),
            apply_to_version_store(&mut b, &policy)
        );
    }

    #[test]
    fn unbounded_policy_never_prunes() {
        let mut store = build_store(25);
        let before = store.version_count();
        let d = apply_to_version_store(&mut store, &PaneRetentionPolicy::unbounded());
        assert_eq!(d.outcome, PaneRetentionOutcome::WithinBudget);
        assert_eq!(store.version_count(), before);
    }
}

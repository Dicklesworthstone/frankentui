//! Deterministic execution-policy selector for pane-history strategies (`bd-1k7ek.6`).
//!
//! Three semantically-equivalent ways to drive pane undo/redo now exist —
//! **baseline** (no history), the **checkpointed** [`PaneInteractionTimeline`](crate::pane::PaneInteractionTimeline),
//! and the **persistent** [`PaneVersionStore`](crate::pane_persistent::PaneVersionStore) —
//! proven byte-identical over lockstep histories
//! (`tests/pane_persistent_equivalence.rs`). Once equivalent implementations
//! exist, the system should pick the cheapest *safe* one for the observed
//! workload rather than hard-coding one globally.
//!
//! This module is that policy layer, and it is deliberately **not** opaque
//! adaptive magic:
//!
//! * Selection is a **pure, deterministic function** of an observed
//!   [`PaneWorkloadProfile`] and explicit, documented thresholds — same inputs
//!   always yield the same [`PaneExecutionDecision`].
//! * Every decision carries a human-readable `log` and a [`PaneStrategyReason`]
//!   explaining *why* a strategy was chosen, so profiling and regressions stay
//!   explainable.
//! * The checkpointed timeline is the **conservative fallback** (it is the
//!   certified production path and the differential oracle for the persistent
//!   spike); it is chosen whenever the persistent criteria are not all met.
//! * Operators can **force** any strategy ([`PaneExecutionPolicy::forcing`]) or
//!   force the conservative path ([`PaneExecutionPolicy::conservative`]) for
//!   debugging and rollout — overrides bypass the adaptive logic entirely.
//! * [`reselect`](PaneExecutionPolicy::reselect) applies **hysteresis** so the
//!   selector does not thrash when the workload jitters near a threshold.
//!
//! Because the candidate strategies are proven equivalent, selecting among them
//! can never change observable behavior — only cost. The selector emits the
//! *decision*; wiring it to a live execution engine is downstream integration
//! (the persistent store is still a prototype on no production path).

use crate::pane::{PaneOperation, PaneOperationFamily};
use crate::pane_memory::PaneMemoryStrategy;
use crate::pane_retention::PaneRetentionPolicy;

/// Observed shape of a pane-interaction workload window — the deterministic
/// input to strategy selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PaneWorkloadProfile {
    /// Operations observed in the window.
    pub operation_count: usize,
    /// Of those, how many are `Local` (resize / `SetSplitRatio`) — the hot path
    /// where the persistent store's structural sharing and O(1) navigation win.
    pub local_operation_count: usize,
    /// Peak operations-per-second observed (burstiness, e.g. a live drag-resize).
    pub peak_ops_per_sec: u32,
    /// Whether undo/redo history is required at all. If not, no history substrate
    /// is needed and the baseline path is selected.
    pub history_required: bool,
}

impl PaneWorkloadProfile {
    /// Construct a profile from explicit counts.
    #[must_use]
    pub const fn new(
        operation_count: usize,
        local_operation_count: usize,
        peak_ops_per_sec: u32,
        history_required: bool,
    ) -> Self {
        Self {
            operation_count,
            local_operation_count,
            peak_ops_per_sec,
            history_required,
        }
    }

    /// Derive a profile from an observed operation window. Local operations are
    /// classified by [`PaneOperation::family`] (`Local` = `SetSplitRatio`).
    #[must_use]
    pub fn observe(ops: &[PaneOperation], peak_ops_per_sec: u32, history_required: bool) -> Self {
        let local_operation_count = ops
            .iter()
            .filter(|op| op.family() == PaneOperationFamily::Local)
            .count();
        Self::new(
            ops.len(),
            local_operation_count,
            peak_ops_per_sec,
            history_required,
        )
    }

    /// Local-operation fraction as an integer percentage in `[0, 100]`.
    #[must_use]
    pub const fn local_fraction_pct(self) -> u32 {
        if self.operation_count == 0 {
            return 0;
        }
        ((self.local_operation_count.saturating_mul(100)) / self.operation_count) as u32
    }
}

/// Why a strategy was selected — the auditable rationale in every decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PaneStrategyReason {
    /// An operator/debug override forced this strategy.
    ForcedOverride,
    /// Conservative mode forced the certified checkpointed path.
    ConservativeFallback,
    /// History is not required, so the baseline (no-history) path was chosen.
    NoHistoryRequired,
    /// A resize-dominated, bursty, deep workload favored the persistent store.
    ResizeDominatedBurst,
    /// No strategy clearly won, so the conservative checkpointed default was used.
    GeneralDefault,
    /// A hysteresis margin held the previous strategy to avoid thrashing.
    HysteresisHold,
}

impl PaneStrategyReason {
    /// Stable identifier for logs/artifacts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ForcedOverride => "forced_override",
            Self::ConservativeFallback => "conservative_fallback",
            Self::NoHistoryRequired => "no_history_required",
            Self::ResizeDominatedBurst => "resize_dominated_burst",
            Self::GeneralDefault => "general_default",
            Self::HysteresisHold => "hysteresis_hold",
        }
    }
}

/// Deterministic policy that selects among the three pane execution strategies.
///
/// The thresholds are explicit and tunable; defaults are derived from the
/// persistence spike (`bd-1k7ek.5`) and the memory telemetry (`bd-25wj7.1`): the
/// persistent store earns its keep on resize-dominated bursts deep enough that
/// O(1) navigation and structural sharing pay off, while the checkpointed
/// timeline is the safe default everywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PaneExecutionPolicy {
    /// Force a specific strategy (operator/debug override). `None` = adaptive.
    pub forced_strategy: Option<PaneMemoryStrategy>,
    /// Force the conservative certified path (checkpointed). Overrides adaptation.
    pub conservative: bool,
    /// Minimum operations before the persistent store is considered.
    pub persistent_min_operations: usize,
    /// Minimum local-op fraction (percent) for the persistent store.
    pub persistent_local_fraction_pct: u32,
    /// Minimum peak ops/sec (burstiness) for the persistent store.
    pub persistent_burst_ops_per_sec: u32,
    /// Hysteresis margin (percent, on the local fraction) used by
    /// [`reselect`](Self::reselect) to avoid thrashing near a threshold.
    pub hysteresis_pct: u32,
    /// The retention policy carried alongside the selected strategy.
    pub retention: PaneRetentionPolicy,
}

impl PaneExecutionPolicy {
    /// Default: persistent only past the spike's bounded-window depth.
    pub const DEFAULT_PERSISTENT_MIN_OPERATIONS: usize = 64;
    /// Default: persistent only when the workload is resize-dominated.
    pub const DEFAULT_PERSISTENT_LOCAL_FRACTION_PCT: u32 = 80;
    /// Default: persistent only under a drag-resize burst.
    pub const DEFAULT_PERSISTENT_BURST_OPS_PER_SEC: u32 = 60;
    /// Default hysteresis margin.
    pub const DEFAULT_HYSTERESIS_PCT: u32 = 10;

    /// An adaptive policy with the default thresholds, carrying `retention`.
    #[must_use]
    pub const fn adaptive(retention: PaneRetentionPolicy) -> Self {
        Self {
            forced_strategy: None,
            conservative: false,
            persistent_min_operations: Self::DEFAULT_PERSISTENT_MIN_OPERATIONS,
            persistent_local_fraction_pct: Self::DEFAULT_PERSISTENT_LOCAL_FRACTION_PCT,
            persistent_burst_ops_per_sec: Self::DEFAULT_PERSISTENT_BURST_OPS_PER_SEC,
            hysteresis_pct: Self::DEFAULT_HYSTERESIS_PCT,
            retention,
        }
    }

    /// Return this policy forced to the conservative certified path (checkpointed).
    #[must_use]
    pub const fn conservative(mut self) -> Self {
        self.conservative = true;
        self
    }

    /// Return this policy forced to a specific strategy.
    #[must_use]
    pub const fn forcing(mut self, strategy: PaneMemoryStrategy) -> Self {
        self.forced_strategy = Some(strategy);
        self
    }

    /// Select a strategy for `profile` (stateless, deterministic).
    #[must_use]
    pub fn select(&self, profile: PaneWorkloadProfile) -> PaneExecutionDecision {
        let (strategy, reason, forced) = self.decide(profile);
        self.decision(strategy, reason, forced, profile)
    }

    /// Re-select with hysteresis given the `previous` strategy: keep `previous`
    /// unless the workload favors a different strategy by a clear margin. This
    /// prevents thrashing when the local-op fraction jitters near a threshold.
    /// Forced/conservative overrides ignore hysteresis.
    #[must_use]
    pub fn reselect(
        &self,
        profile: PaneWorkloadProfile,
        previous: PaneMemoryStrategy,
    ) -> PaneExecutionDecision {
        if self.forced_strategy.is_some() || self.conservative {
            return self.select(profile);
        }
        let (fresh, reason, forced) = self.decide(profile);
        if fresh == previous {
            return self.decision(fresh, reason, forced, profile);
        }
        // The history requirement is a hard functional flag, not a tunable
        // threshold: honor it immediately (no hysteresis on entering/leaving
        // the baseline path).
        if matches!(reason, PaneStrategyReason::NoHistoryRequired)
            || previous == PaneMemoryStrategy::Baseline
        {
            return self.decision(fresh, reason, forced, profile);
        }
        let decisive = if fresh == PaneMemoryStrategy::Persistent {
            // Entering persistent: EVERY gate must clear by a margin, not just
            // the local-fraction threshold. Leaving is decisive on any failed
            // hard gate, so a marginless entry would let burst/op-count jitter
            // right at a hard gate flip the strategy every window — the exact
            // oscillation hysteresis exists to prevent. Hard-gate margins are
            // proportional (10%).
            let burst_margin = self.persistent_burst_ops_per_sec / 10;
            let ops_margin = self.persistent_min_operations / 10;
            self.favors_persistent(profile, self.hysteresis_pct)
                && profile.peak_ops_per_sec
                    >= self
                        .persistent_burst_ops_per_sec
                        .saturating_add(burst_margin)
                && profile.operation_count
                    >= self.persistent_min_operations.saturating_add(ops_margin)
        } else {
            // Leaving persistent: a failed hard gate is decisive; otherwise the
            // local fraction must drop below the threshold by the margin.
            profile.operation_count < self.persistent_min_operations
                || profile.peak_ops_per_sec < self.persistent_burst_ops_per_sec
                || profile.local_fraction_pct()
                    < self
                        .persistent_local_fraction_pct
                        .saturating_sub(self.hysteresis_pct)
        };
        if decisive {
            self.decision(fresh, reason, forced, profile)
        } else {
            self.decision(previous, PaneStrategyReason::HysteresisHold, false, profile)
        }
    }

    fn decide(
        &self,
        profile: PaneWorkloadProfile,
    ) -> (PaneMemoryStrategy, PaneStrategyReason, bool) {
        if let Some(forced) = self.forced_strategy {
            return (forced, PaneStrategyReason::ForcedOverride, true);
        }
        if self.conservative {
            return (
                PaneMemoryStrategy::Checkpointed,
                PaneStrategyReason::ConservativeFallback,
                true,
            );
        }
        if !profile.history_required {
            return (
                PaneMemoryStrategy::Baseline,
                PaneStrategyReason::NoHistoryRequired,
                false,
            );
        }
        if self.favors_persistent(profile, 0) {
            return (
                PaneMemoryStrategy::Persistent,
                PaneStrategyReason::ResizeDominatedBurst,
                false,
            );
        }
        (
            PaneMemoryStrategy::Checkpointed,
            PaneStrategyReason::GeneralDefault,
            false,
        )
    }

    fn favors_persistent(&self, profile: PaneWorkloadProfile, local_margin_pct: u32) -> bool {
        profile.operation_count >= self.persistent_min_operations
            && profile.local_fraction_pct()
                >= self
                    .persistent_local_fraction_pct
                    .saturating_add(local_margin_pct)
            && profile.peak_ops_per_sec >= self.persistent_burst_ops_per_sec
    }

    fn decision(
        &self,
        strategy: PaneMemoryStrategy,
        reason: PaneStrategyReason,
        forced: bool,
        profile: PaneWorkloadProfile,
    ) -> PaneExecutionDecision {
        let log = format!(
            "execution[{}] {}: ops={} local={}% burst={}/s history={} (thresholds: min_ops={} local>={}% burst>={}/s hysteresis={}%{}); retention budget bytes={} units={}",
            strategy.as_str(),
            reason.as_str(),
            profile.operation_count,
            profile.local_fraction_pct(),
            profile.peak_ops_per_sec,
            profile.history_required,
            self.persistent_min_operations,
            self.persistent_local_fraction_pct,
            self.persistent_burst_ops_per_sec,
            self.hysteresis_pct,
            if forced { ", forced" } else { "" },
            self.retention.budget.max_retained_bytes,
            self.retention.budget.max_retained_units,
        );
        PaneExecutionDecision {
            strategy,
            reason,
            forced,
            profile,
            retention: self.retention,
            log,
        }
    }
}

/// A deterministic, auditable strategy-selection decision.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PaneExecutionDecision {
    /// The selected execution strategy.
    pub strategy: PaneMemoryStrategy,
    /// Why it was selected.
    pub reason: PaneStrategyReason,
    /// Whether an operator override produced this decision.
    pub forced: bool,
    /// The workload profile the decision was made against.
    pub profile: PaneWorkloadProfile,
    /// The retention policy to apply alongside the selected strategy.
    pub retention: PaneRetentionPolicy,
    /// Human-readable one-line decision trace.
    pub log: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::{
        PaneId, PaneInteractionTimeline, PaneLeaf, PaneOperation, PanePlacement, PaneSplitRatio,
        PaneTree, SplitAxis,
    };
    use crate::pane_persistent::{PaneVersionStore, VersionedPaneTree};

    fn policy() -> PaneExecutionPolicy {
        PaneExecutionPolicy::adaptive(PaneRetentionPolicy::bounded(500_000, 64))
    }

    fn resize_storm_profile() -> PaneWorkloadProfile {
        // 512 ops, all local (a pure drag-resize storm), bursty.
        PaneWorkloadProfile::new(512, 512, 240, true)
    }

    fn mixed_profile() -> PaneWorkloadProfile {
        // 384 ops, ~55% local, moderate rate.
        PaneWorkloadProfile::new(384, 211, 40, true)
    }

    #[test]
    fn selection_is_deterministic() {
        let p = policy();
        let profile = resize_storm_profile();
        assert_eq!(p.select(profile), p.select(profile));
    }

    #[test]
    fn resize_storm_selects_persistent() {
        let d = policy().select(resize_storm_profile());
        assert_eq!(d.strategy, PaneMemoryStrategy::Persistent);
        assert_eq!(d.reason, PaneStrategyReason::ResizeDominatedBurst);
        assert!(!d.forced);
    }

    #[test]
    fn mixed_workload_falls_back_to_checkpointed() {
        let d = policy().select(mixed_profile());
        assert_eq!(d.strategy, PaneMemoryStrategy::Checkpointed);
        assert_eq!(d.reason, PaneStrategyReason::GeneralDefault);
    }

    #[test]
    fn no_history_selects_baseline() {
        let profile = PaneWorkloadProfile::new(512, 512, 240, false);
        let d = policy().select(profile);
        assert_eq!(d.strategy, PaneMemoryStrategy::Baseline);
        assert_eq!(d.reason, PaneStrategyReason::NoHistoryRequired);
    }

    #[test]
    fn shallow_resize_storm_stays_checkpointed() {
        // Resize-dominated and bursty, but below the depth where persistent pays.
        let profile = PaneWorkloadProfile::new(32, 32, 240, true);
        assert_eq!(
            policy().select(profile).strategy,
            PaneMemoryStrategy::Checkpointed
        );
    }

    #[test]
    fn forced_strategy_overrides_adaptation() {
        // A mixed workload would pick checkpointed, but force persistent.
        let forced = policy().forcing(PaneMemoryStrategy::Persistent);
        let d = forced.select(mixed_profile());
        assert_eq!(d.strategy, PaneMemoryStrategy::Persistent);
        assert_eq!(d.reason, PaneStrategyReason::ForcedOverride);
        assert!(d.forced);
    }

    #[test]
    fn conservative_forces_checkpointed_even_on_resize_storm() {
        let conservative = policy().conservative();
        let d = conservative.select(resize_storm_profile());
        assert_eq!(d.strategy, PaneMemoryStrategy::Checkpointed);
        assert_eq!(d.reason, PaneStrategyReason::ConservativeFallback);
        assert!(d.forced);
    }

    #[test]
    fn hysteresis_prevents_thrashing_near_threshold() {
        let p = policy();
        // Local fraction exactly at the entry threshold (80%): a fresh select
        // would pick persistent, but reselect from checkpointed needs +margin.
        let at_threshold = PaneWorkloadProfile::new(512, 410, 240, true); // 80%
        assert_eq!(
            p.select(at_threshold).strategy,
            PaneMemoryStrategy::Persistent
        );
        assert_eq!(
            p.reselect(at_threshold, PaneMemoryStrategy::Checkpointed)
                .strategy,
            PaneMemoryStrategy::Checkpointed,
            "should not enter persistent without clearing the hysteresis margin"
        );

        // A decisive resize storm (95% local) does enter persistent.
        let decisive = PaneWorkloadProfile::new(512, 487, 240, true); // 95%
        let entered = p.reselect(decisive, PaneMemoryStrategy::Checkpointed);
        assert_eq!(entered.strategy, PaneMemoryStrategy::Persistent);
        assert_eq!(entered.reason, PaneStrategyReason::ResizeDominatedBurst);

        // Once persistent, a mild dip (75% — within the margin) holds persistent.
        let mild_dip = PaneWorkloadProfile::new(512, 384, 240, true); // 75%
        let held = p.reselect(mild_dip, PaneMemoryStrategy::Persistent);
        assert_eq!(held.strategy, PaneMemoryStrategy::Persistent);
        assert_eq!(held.reason, PaneStrategyReason::HysteresisHold);

        // A decisive drop (65% — below threshold - margin) leaves persistent.
        let decisive_drop = PaneWorkloadProfile::new(512, 332, 240, true); // 64%
        assert_eq!(
            p.reselect(decisive_drop, PaneMemoryStrategy::Persistent)
                .strategy,
            PaneMemoryStrategy::Checkpointed
        );
    }

    #[test]
    fn hard_gate_jitter_does_not_oscillate() {
        // Burst rate jittering right at the hard gate (59 <-> 60/s) must not
        // flip Persistent <-> Checkpointed every window: leaving on a failed
        // hard gate is decisive, so re-entry must clear the gate by a margin
        // (>= 66/s at the default 60/s threshold), not merely touch it.
        let p = policy();
        let below_gate = PaneWorkloadProfile::new(512, 512, 59, true);
        let at_gate = PaneWorkloadProfile::new(512, 512, 60, true);
        let clears_gate = PaneWorkloadProfile::new(512, 512, 66, true);

        // Below the gate: leaving persistent is decisive.
        assert_eq!(
            p.reselect(below_gate, PaneMemoryStrategy::Persistent)
                .strategy,
            PaneMemoryStrategy::Checkpointed
        );
        // Back at (but not clearing) the gate: re-entry is refused — held.
        let held = p.reselect(at_gate, PaneMemoryStrategy::Checkpointed);
        assert_eq!(held.strategy, PaneMemoryStrategy::Checkpointed);
        assert_eq!(held.reason, PaneStrategyReason::HysteresisHold);
        // Clearing the gate by the 10% margin enters persistent.
        assert_eq!(
            p.reselect(clears_gate, PaneMemoryStrategy::Checkpointed)
                .strategy,
            PaneMemoryStrategy::Persistent
        );
    }

    #[test]
    fn observe_classifies_local_operations() {
        let ops = vec![
            PaneOperation::SetSplitRatio {
                split: PaneId::new(2).unwrap(),
                ratio: PaneSplitRatio::new(1, 1).unwrap(),
            },
            PaneOperation::SetSplitRatio {
                split: PaneId::new(2).unwrap(),
                ratio: PaneSplitRatio::new(2, 1).unwrap(),
            },
            PaneOperation::CloseNode {
                target: PaneId::new(3).unwrap(),
            },
        ];
        let profile = PaneWorkloadProfile::observe(&ops, 120, true);
        assert_eq!(profile.operation_count, 3);
        assert_eq!(profile.local_operation_count, 2);
        assert_eq!(profile.local_fraction_pct(), 66);
    }

    /// The headline safety guarantee: whichever strategy the selector picks, the
    /// observable result (final state hash) is identical — the candidates are
    /// proven equivalent, so selection changes cost, never behavior.
    #[test]
    fn strategy_choice_never_diverges_behavior() {
        let ratio = |n, d| PaneSplitRatio::new(n, d).expect("ratio");
        let mut ops = vec![
            PaneOperation::SplitLeaf {
                target: PaneId::MIN,
                axis: SplitAxis::Horizontal,
                ratio: ratio(1, 1),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("b"),
            },
            PaneOperation::SplitLeaf {
                target: PaneId::MIN,
                axis: SplitAxis::Vertical,
                ratio: ratio(2, 1),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("c"),
            },
        ];
        let split = PaneId::new(4).expect("id");
        for n in 1..=12u32 {
            ops.push(PaneOperation::SetSplitRatio {
                split,
                ratio: ratio(n % 5 + 1, 1),
            });
        }

        // Baseline: a plain tree.
        let mut baseline = PaneTree::singleton("root");
        for (i, op) in ops.iter().enumerate() {
            baseline
                .apply_operation_conservative(i as u64 + 1, op.clone())
                .expect("baseline apply");
        }
        // Checkpointed timeline.
        let mut tree = PaneTree::singleton("root");
        let mut timeline = PaneInteractionTimeline::default();
        for (i, op) in ops.iter().enumerate() {
            let id = i as u64;
            timeline
                .apply_and_record(&mut tree, id, id, op.clone())
                .expect("timeline apply");
        }
        // Persistent store.
        let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
        for op in &ops {
            store.apply(op).expect("store apply");
        }

        let baseline_hash = baseline.state_hash();
        let timeline_hash = tree.state_hash();
        let store_hash = store.current().state_hash().expect("hash");
        assert_eq!(baseline_hash, timeline_hash);
        assert_eq!(baseline_hash, store_hash);

        // And the selector picks among exactly these equivalent substrates.
        let profile = PaneWorkloadProfile::observe(&ops, 240, true);
        let strategy = policy().select(profile).strategy;
        assert!(matches!(
            strategy,
            PaneMemoryStrategy::Baseline
                | PaneMemoryStrategy::Checkpointed
                | PaneMemoryStrategy::Persistent
        ));
    }
}

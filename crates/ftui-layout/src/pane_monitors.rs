//! Latency-envelope and assumption monitors for advanced pane strategies
//! (`bd-1pvzq.2`).
//!
//! The checkpointed, persistent, and policy-selected pane execution strategies
//! each rest on operating assumptions that a single scalar perf budget cannot
//! protect:
//!
//! * **Checkpointed replay** assumes `replay()` walks at most one checkpoint
//!   interval from the nearest checkpoint — if `replay_depth` blows past the
//!   interval, undo/redo silently degrades to a long replay.
//! * **Retention** assumes the bounded budget can hold the working set — if the
//!   policy hits its floor it is keeping over-budget state (or dropping history
//!   a user expects).
//! * **The execution-policy selector** assumes hysteresis keeps the chosen
//!   strategy stable — if it thrashes, every switch pays a substrate-rebuild
//!   cost, and if it keeps falling back to the conservative path the advanced
//!   strategies are not actually being used.
//! * **Every strategy** has a per-op **latency envelope** that, once exceeded,
//!   shows up to the user as a sluggish drag/undo.
//!
//! These monitors turn each assumption into an explicit, structured, and
//! *operator-readable* verdict so a CI gate or a local triage session can say
//! exactly which assumption failed, on which scenario, under which strategy —
//! rather than leaving a regression as a mysterious slowdown.
//!
//! Everything here is pure and deterministic: the same telemetry always yields
//! the same report, byte-for-byte, so the verdicts are safe to attach to CI
//! artifacts (see `docs/perf/pane_assumption_monitors.md`).

use serde::Serialize;

use crate::pane::PaneInteractionTimelineReplayDiagnostics;
use crate::pane_execution::{PaneExecutionDecision, PaneStrategyReason};
use crate::pane_memory::PaneMemoryStrategy;
use crate::pane_retention::{PaneRetentionDecision, PaneRetentionOutcome};

/// Health of one monitored assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneMonitorStatus {
    /// The assumption holds with comfortable headroom.
    Healthy,
    /// The assumption still holds but headroom is thin — worth watching.
    Degraded,
    /// The assumption is violated — responsiveness or correctness of the
    /// optimization is at risk.
    Violated,
}

impl PaneMonitorStatus {
    /// Whether this status is a hard violation (the CI fail condition).
    #[must_use]
    pub const fn is_violation(self) -> bool {
        matches!(self, Self::Violated)
    }

    /// Whether this status is at least degraded (warn-or-worse).
    #[must_use]
    pub const fn is_degraded_or_worse(self) -> bool {
        matches!(self, Self::Degraded | Self::Violated)
    }

    const fn rank(self) -> u8 {
        match self {
            Self::Healthy => 0,
            Self::Degraded => 1,
            Self::Violated => 2,
        }
    }

    /// The worse (higher-severity) of two statuses.
    #[must_use]
    pub fn worst(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Violated => "violated",
        }
    }
}

/// Which operating assumption a verdict concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneAssumption {
    /// Checkpointed replay walks at most one checkpoint interval.
    ReplayDepthBound,
    /// Retained history fits within the configured budget.
    RetentionPressure,
    /// The execution-policy selector does not thrash between strategies.
    SelectorChurn,
    /// The workload is not constantly forced onto the conservative path.
    FallbackFrequency,
    /// Per-operation latency stays within the strategy's envelope.
    LatencyEnvelope,
}

impl PaneAssumption {
    /// Stable identifier for logs and dashboards.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReplayDepthBound => "replay_depth_bound",
            Self::RetentionPressure => "retention_pressure",
            Self::SelectorChurn => "selector_churn",
            Self::FallbackFrequency => "fallback_frequency",
            Self::LatencyEnvelope => "latency_envelope",
        }
    }
}

/// Tunable thresholds. Defaults are chosen to flag real regressions (≈2×
/// blowups, near-budget pressure, sustained thrash) without firing on the
/// normal jitter of a healthy workload.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PaneMonitorThresholds {
    /// `replay_depth / interval` above this is degraded.
    pub replay_depth_degraded_ratio: f64,
    /// `replay_depth / interval` above this is a violation.
    pub replay_depth_violated_ratio: f64,
    /// Retained-bytes utilization (% of budget) above this is degraded.
    pub retention_degraded_util_pct: f64,
    /// Strategy-switch rate (% of transitions) above this is degraded.
    pub selector_churn_degraded_pct: f64,
    /// Strategy-switch rate above this is a violation.
    pub selector_churn_violated_pct: f64,
    /// Conservative-fallback rate (% of decisions) above this is degraded.
    pub fallback_degraded_pct: f64,
    /// Conservative-fallback rate above this is a violation.
    pub fallback_violated_pct: f64,
    /// `observed / envelope` above this is degraded.
    pub latency_degraded_ratio: f64,
    /// `observed / envelope` above this is a violation.
    pub latency_violated_ratio: f64,
}

impl Default for PaneMonitorThresholds {
    fn default() -> Self {
        Self {
            replay_depth_degraded_ratio: 1.0,
            replay_depth_violated_ratio: 2.0,
            retention_degraded_util_pct: 90.0,
            selector_churn_degraded_pct: 25.0,
            selector_churn_violated_pct: 50.0,
            fallback_degraded_pct: 50.0,
            fallback_violated_pct: 80.0,
            latency_degraded_ratio: 0.8,
            latency_violated_ratio: 1.0,
        }
    }
}

/// One monitor verdict: machine-readable metrics plus an operator-readable
/// explanation of what it means for responsiveness.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaneMonitorVerdict {
    /// The assumption being judged.
    pub assumption: PaneAssumption,
    /// The strategy the verdict applies to.
    pub strategy: PaneMemoryStrategy,
    /// Overall health.
    pub status: PaneMonitorStatus,
    /// Observed metric value (units depend on the assumption: a ratio, a
    /// percentage, or nanoseconds).
    pub observed: f64,
    /// The value at which the verdict would become a violation (the budget).
    pub budget: f64,
    /// Remaining headroom as a percentage of the budget; negative once over.
    pub headroom_pct: f64,
    /// Plain-language explanation aimed at an operator, not just a developer.
    pub explanation: String,
}

/// Aggregate report over one scenario.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaneMonitorReport {
    /// Scenario label (e.g. the profiling scenario name).
    pub scenario: String,
    /// Verdicts in evaluation order.
    pub verdicts: Vec<PaneMonitorVerdict>,
}

impl PaneMonitorReport {
    /// Start an empty report for `scenario`.
    #[must_use]
    pub fn new(scenario: impl Into<String>) -> Self {
        Self {
            scenario: scenario.into(),
            verdicts: Vec::new(),
        }
    }

    /// Append a verdict (builder style).
    pub fn with(mut self, verdict: PaneMonitorVerdict) -> Self {
        self.verdicts.push(verdict);
        self
    }

    /// Append a verdict in place.
    pub fn push(&mut self, verdict: PaneMonitorVerdict) {
        self.verdicts.push(verdict);
    }

    /// The worst status across all verdicts (`Healthy` if empty).
    #[must_use]
    pub fn worst_status(&self) -> PaneMonitorStatus {
        self.verdicts
            .iter()
            .fold(PaneMonitorStatus::Healthy, |acc, v| acc.worst(v.status))
    }

    /// Whether any assumption is violated — the CI fail condition.
    #[must_use]
    pub fn has_violations(&self) -> bool {
        self.verdicts.iter().any(|v| v.status.is_violation())
    }

    /// Iterate the violated verdicts.
    pub fn violations(&self) -> impl Iterator<Item = &PaneMonitorVerdict> {
        self.verdicts.iter().filter(|v| v.status.is_violation())
    }

    /// Deterministic structured JSON log (one object). Infallible: falls back to
    /// `"{}"` on the serialization error that cannot occur for these fields.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Operator-facing multi-line summary: one line per verdict.
    #[must_use]
    pub fn summary_log(&self) -> String {
        let mut out = format!(
            "pane monitors [{}]: {}\n",
            self.scenario,
            self.worst_status().label()
        );
        for v in &self.verdicts {
            out.push_str(&format!(
                "  [{}] {}: {}\n",
                v.status.label(),
                v.assumption.as_str(),
                v.explanation
            ));
        }
        out
    }
}

// ===========================================================================
// Individual monitors
// ===========================================================================

fn classify_ratio(ratio: f64, degraded_at: f64, violated_at: f64) -> PaneMonitorStatus {
    if ratio > violated_at {
        PaneMonitorStatus::Violated
    } else if ratio > degraded_at {
        PaneMonitorStatus::Degraded
    } else {
        PaneMonitorStatus::Healthy
    }
}

fn headroom_pct(observed: f64, budget: f64) -> f64 {
    if budget <= 0.0 {
        100.0
    } else {
        (budget - observed) / budget * 100.0
    }
}

/// Monitor the checkpointed-replay depth assumption: `replay()` should walk at
/// most one checkpoint interval from the nearest checkpoint.
#[must_use]
pub fn monitor_replay_depth(
    diag: &PaneInteractionTimelineReplayDiagnostics,
    thresholds: &PaneMonitorThresholds,
) -> PaneMonitorVerdict {
    let interval = diag.checkpoint_interval.max(1) as f64;
    let depth = diag.replay_depth as f64;
    let ratio = depth / interval;
    let status = classify_ratio(
        ratio,
        thresholds.replay_depth_degraded_ratio,
        thresholds.replay_depth_violated_ratio,
    );
    let explanation = match status {
        PaneMonitorStatus::Healthy => format!(
            "Checkpointed replay walks {} step(s) from the nearest checkpoint, within the \
{}-step interval — undo/redo stays snappy.",
            diag.replay_depth, diag.checkpoint_interval
        ),
        PaneMonitorStatus::Degraded => format!(
            "Checkpointed replay walks {} step(s), {:.1}x the {}-step checkpoint interval — \
undo/redo is starting to lag; checkpoint spacing may be too coarse.",
            diag.replay_depth, ratio, diag.checkpoint_interval
        ),
        PaneMonitorStatus::Violated => format!(
            "Checkpointed replay walks {} step(s), {:.1}x the {}-step checkpoint interval \
(checkpoint_hit={}) — undo/redo will feel sluggish; the checkpoint-spacing assumption is \
violated.",
            diag.replay_depth, ratio, diag.checkpoint_interval, diag.checkpoint_hit
        ),
    };
    PaneMonitorVerdict {
        assumption: PaneAssumption::ReplayDepthBound,
        strategy: PaneMemoryStrategy::Checkpointed,
        status,
        observed: ratio,
        budget: thresholds.replay_depth_violated_ratio,
        headroom_pct: headroom_pct(ratio, thresholds.replay_depth_violated_ratio),
        explanation,
    }
}

/// Monitor retention pressure from a bounded-retention decision.
#[must_use]
pub fn monitor_retention_pressure(
    decision: &PaneRetentionDecision,
    thresholds: &PaneMonitorThresholds,
) -> PaneMonitorVerdict {
    let budget_bytes = decision.budget.max_retained_bytes;
    let after = decision.bytes_after as f64;
    // Utilization as a percentage of budget (0 budget = unbounded = no pressure).
    let util_pct = if budget_bytes == 0 {
        0.0
    } else {
        after / budget_bytes as f64 * 100.0
    };

    let status = match decision.outcome {
        PaneRetentionOutcome::FloorReached => PaneMonitorStatus::Violated,
        PaneRetentionOutcome::ConservativeHold => PaneMonitorStatus::Degraded,
        PaneRetentionOutcome::WithinBudget | PaneRetentionOutcome::PrunedToFit => {
            if budget_bytes != 0 && util_pct > thresholds.retention_degraded_util_pct {
                PaneMonitorStatus::Degraded
            } else {
                PaneMonitorStatus::Healthy
            }
        }
    };

    let explanation = match decision.outcome {
        PaneRetentionOutcome::FloorReached => format!(
            "Retention hit its floor: {} byte(s) of state cannot fit the {}-byte budget even \
after pruning to the minimum — history a user expects may be dropped, or the budget is too \
small for this workspace.",
            decision.bytes_after, budget_bytes
        ),
        PaneRetentionOutcome::ConservativeHold => format!(
            "Retention is holding {} byte(s) over its {}-byte budget in conservative-debug mode \
— memory pressure is unbounded; this should not run in production.",
            decision.bytes_after, budget_bytes
        ),
        PaneRetentionOutcome::PrunedToFit => format!(
            "Retention pruned {} unit(s) to fit the {}-byte budget ({:.0}% utilized) — undo \
history beyond the kept window is gone.",
            decision.units_pruned, budget_bytes, util_pct
        ),
        PaneRetentionOutcome::WithinBudget if budget_bytes == 0 => format!(
            "Retention is unbounded ({} byte(s) retained) — full undo history kept.",
            decision.bytes_after
        ),
        PaneRetentionOutcome::WithinBudget => format!(
            "Retention is within budget ({:.0}% of {} bytes used) — full undo history retained.",
            util_pct, budget_bytes
        ),
    };

    PaneMonitorVerdict {
        assumption: PaneAssumption::RetentionPressure,
        strategy: decision.strategy,
        status,
        observed: util_pct,
        budget: 100.0,
        headroom_pct: (100.0 - util_pct).max(-100.0),
        explanation,
    }
}

/// Monitor selector churn: how often the policy switches strategy across a
/// decision sequence (e.g. successive `reselect` calls over a session).
#[must_use]
pub fn monitor_selector_churn(
    decisions: &[PaneExecutionDecision],
    thresholds: &PaneMonitorThresholds,
) -> PaneMonitorVerdict {
    let transitions = decisions.len().saturating_sub(1);
    let switches = decisions
        .windows(2)
        .filter(|w| w[0].strategy != w[1].strategy)
        .count();
    let churn_pct = if transitions == 0 {
        0.0
    } else {
        switches as f64 / transitions as f64 * 100.0
    };
    let status = classify_ratio(
        churn_pct,
        thresholds.selector_churn_degraded_pct,
        thresholds.selector_churn_violated_pct,
    );
    let strategy = decisions
        .last()
        .map_or(PaneMemoryStrategy::Checkpointed, |d| d.strategy);
    let explanation = match status {
        PaneMonitorStatus::Healthy => format!(
            "Selector switched strategy {switches} time(s) across {transitions} transition(s) \
({churn_pct:.0}%) — hysteresis is keeping the strategy stable."
        ),
        PaneMonitorStatus::Degraded => format!(
            "Selector switched strategy {switches}/{transitions} times ({churn_pct:.0}%) — \
hysteresis is fraying; each switch pays a substrate-rebuild cost the user may feel."
        ),
        PaneMonitorStatus::Violated => format!(
            "Selector thrashed {switches}/{transitions} times ({churn_pct:.0}%) between \
strategies — the hysteresis assumption is violated and rebuild cost is dominating."
        ),
    };
    PaneMonitorVerdict {
        assumption: PaneAssumption::SelectorChurn,
        strategy,
        status,
        observed: churn_pct,
        budget: thresholds.selector_churn_violated_pct,
        headroom_pct: headroom_pct(churn_pct, thresholds.selector_churn_violated_pct),
        explanation,
    }
}

/// Monitor how often the policy is forced onto the conservative path. A
/// workload that constantly falls back is not benefiting from the advanced
/// strategies.
#[must_use]
pub fn monitor_fallback_frequency(
    decisions: &[PaneExecutionDecision],
    thresholds: &PaneMonitorThresholds,
) -> PaneMonitorVerdict {
    let total = decisions.len();
    let fallbacks = decisions
        .iter()
        .filter(|d| matches!(d.reason, PaneStrategyReason::ConservativeFallback))
        .count();
    let fallback_pct = if total == 0 {
        0.0
    } else {
        fallbacks as f64 / total as f64 * 100.0
    };
    let status = classify_ratio(
        fallback_pct,
        thresholds.fallback_degraded_pct,
        thresholds.fallback_violated_pct,
    );
    let strategy = decisions
        .last()
        .map_or(PaneMemoryStrategy::Checkpointed, |d| d.strategy);
    let explanation = match status {
        PaneMonitorStatus::Healthy => format!(
            "Conservative fallback fired {fallbacks}/{total} decision(s) ({fallback_pct:.0}%) — \
the advanced strategies are being used as intended."
        ),
        PaneMonitorStatus::Degraded => format!(
            "Conservative fallback fired {fallbacks}/{total} decision(s) ({fallback_pct:.0}%) — \
the workload is often forced onto the safe path; the optimization is only partly active."
        ),
        PaneMonitorStatus::Violated => format!(
            "Conservative fallback fired {fallbacks}/{total} decision(s) ({fallback_pct:.0}%) — \
the workload is almost always on the safe path, so the advanced strategies provide little \
benefit here."
        ),
    };
    PaneMonitorVerdict {
        assumption: PaneAssumption::FallbackFrequency,
        strategy,
        status,
        observed: fallback_pct,
        budget: thresholds.fallback_violated_pct,
        headroom_pct: headroom_pct(fallback_pct, thresholds.fallback_violated_pct),
        explanation,
    }
}

/// Monitor a strategy's per-operation latency against its envelope (both in
/// nanoseconds/op).
#[must_use]
pub fn monitor_latency_envelope(
    strategy: PaneMemoryStrategy,
    observed_ns_per_op: f64,
    envelope_ns_per_op: f64,
    thresholds: &PaneMonitorThresholds,
) -> PaneMonitorVerdict {
    let envelope = envelope_ns_per_op.max(1.0);
    let ratio = observed_ns_per_op / envelope;
    let status = classify_ratio(
        ratio,
        thresholds.latency_degraded_ratio,
        thresholds.latency_violated_ratio,
    );
    let explanation = match status {
        PaneMonitorStatus::Healthy => format!(
            "Observed {observed_ns_per_op:.0} ns/op is within the {envelope_ns_per_op:.0} ns/op \
envelope ({ratio:.2}x) — interactions stay responsive."
        ),
        PaneMonitorStatus::Degraded => format!(
            "Observed {observed_ns_per_op:.0} ns/op is {ratio:.2}x the {envelope_ns_per_op:.0} \
ns/op envelope — approaching the latency budget; responsiveness margin is thin."
        ),
        PaneMonitorStatus::Violated => format!(
            "Observed {observed_ns_per_op:.0} ns/op exceeds the {envelope_ns_per_op:.0} ns/op \
envelope ({ratio:.2}x) — interactions will feel slow; the latency budget is blown."
        ),
    };
    PaneMonitorVerdict {
        assumption: PaneAssumption::LatencyEnvelope,
        strategy,
        status,
        observed: observed_ns_per_op,
        budget: envelope_ns_per_op,
        headroom_pct: headroom_pct(observed_ns_per_op, envelope_ns_per_op),
        explanation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_retention::{PaneRetentionBudget, PaneRetentionPolicy};

    fn replay_diag(
        checkpoint_interval: usize,
        replay_depth: usize,
        checkpoint_hit: bool,
    ) -> PaneInteractionTimelineReplayDiagnostics {
        PaneInteractionTimelineReplayDiagnostics {
            entry_count: 64,
            cursor: 64,
            checkpoint_count: 4,
            checkpoint_interval,
            checkpoint_hit,
            replay_start_idx: 64usize.saturating_sub(replay_depth),
            replay_depth,
        }
    }

    fn retention_decision(
        outcome: PaneRetentionOutcome,
        budget_bytes: usize,
        bytes_after: usize,
        units_pruned: usize,
    ) -> PaneRetentionDecision {
        PaneRetentionDecision {
            strategy: PaneMemoryStrategy::Persistent,
            budget: PaneRetentionBudget {
                max_retained_bytes: budget_bytes,
                max_retained_units: 0,
            },
            conservative_debug: false,
            units_before: 16,
            units_after: 16usize.saturating_sub(units_pruned),
            units_pruned,
            bytes_before: bytes_after + units_pruned * 1024,
            bytes_after,
            current_state_hash: 0xABCD,
            outcome,
            log: String::new(),
        }
    }

    fn decision(strategy: PaneMemoryStrategy, reason: PaneStrategyReason) -> PaneExecutionDecision {
        use crate::pane_execution::PaneWorkloadProfile;
        PaneExecutionDecision {
            strategy,
            reason,
            forced: matches!(reason, PaneStrategyReason::ForcedOverride),
            profile: PaneWorkloadProfile::observe(&[], 0, false),
            retention: PaneRetentionPolicy::unbounded(),
            log: String::new(),
        }
    }

    #[test]
    fn replay_depth_within_interval_is_healthy() {
        let v = monitor_replay_depth(&replay_diag(16, 8, true), &PaneMonitorThresholds::default());
        assert_eq!(v.status, PaneMonitorStatus::Healthy);
        assert_eq!(v.assumption, PaneAssumption::ReplayDepthBound);
        assert!(v.headroom_pct > 0.0);
    }

    #[test]
    fn replay_depth_past_interval_degrades_then_violates() {
        let t = PaneMonitorThresholds::default();
        let degraded = monitor_replay_depth(&replay_diag(16, 20, true), &t);
        assert_eq!(degraded.status, PaneMonitorStatus::Degraded);
        let violated = monitor_replay_depth(&replay_diag(16, 40, false), &t);
        assert_eq!(violated.status, PaneMonitorStatus::Violated);
        assert!(violated.explanation.contains("violated"));
    }

    #[test]
    fn retention_outcomes_map_to_statuses() {
        let t = PaneMonitorThresholds::default();
        assert_eq!(
            monitor_retention_pressure(
                &retention_decision(PaneRetentionOutcome::WithinBudget, 8192, 2048, 0),
                &t
            )
            .status,
            PaneMonitorStatus::Healthy
        );
        assert_eq!(
            monitor_retention_pressure(
                &retention_decision(PaneRetentionOutcome::PrunedToFit, 8192, 8000, 4),
                &t
            )
            .status,
            PaneMonitorStatus::Degraded
        );
        assert_eq!(
            monitor_retention_pressure(
                &retention_decision(PaneRetentionOutcome::ConservativeHold, 4096, 9000, 0),
                &t
            )
            .status,
            PaneMonitorStatus::Degraded
        );
        assert_eq!(
            monitor_retention_pressure(
                &retention_decision(PaneRetentionOutcome::FloorReached, 1024, 4096, 12),
                &t
            )
            .status,
            PaneMonitorStatus::Violated
        );
    }

    #[test]
    fn unbounded_retention_has_no_pressure() {
        let v = monitor_retention_pressure(
            &retention_decision(PaneRetentionOutcome::WithinBudget, 0, 1_000_000, 0),
            &PaneMonitorThresholds::default(),
        );
        assert_eq!(v.status, PaneMonitorStatus::Healthy);
    }

    #[test]
    fn selector_churn_detects_thrash() {
        let t = PaneMonitorThresholds::default();
        let stable = vec![
            decision(
                PaneMemoryStrategy::Checkpointed,
                PaneStrategyReason::GeneralDefault,
            ),
            decision(
                PaneMemoryStrategy::Checkpointed,
                PaneStrategyReason::HysteresisHold,
            ),
            decision(
                PaneMemoryStrategy::Checkpointed,
                PaneStrategyReason::GeneralDefault,
            ),
        ];
        assert_eq!(
            monitor_selector_churn(&stable, &t).status,
            PaneMonitorStatus::Healthy
        );

        let thrash = vec![
            decision(
                PaneMemoryStrategy::Checkpointed,
                PaneStrategyReason::GeneralDefault,
            ),
            decision(
                PaneMemoryStrategy::Persistent,
                PaneStrategyReason::ResizeDominatedBurst,
            ),
            decision(
                PaneMemoryStrategy::Checkpointed,
                PaneStrategyReason::GeneralDefault,
            ),
            decision(
                PaneMemoryStrategy::Persistent,
                PaneStrategyReason::ResizeDominatedBurst,
            ),
        ];
        assert_eq!(
            monitor_selector_churn(&thrash, &t).status,
            PaneMonitorStatus::Violated
        );
    }

    #[test]
    fn fallback_frequency_flags_constant_conservative() {
        let t = PaneMonitorThresholds::default();
        let healthy = vec![
            decision(
                PaneMemoryStrategy::Persistent,
                PaneStrategyReason::ResizeDominatedBurst,
            ),
            decision(
                PaneMemoryStrategy::Persistent,
                PaneStrategyReason::ResizeDominatedBurst,
            ),
        ];
        assert_eq!(
            monitor_fallback_frequency(&healthy, &t).status,
            PaneMonitorStatus::Healthy
        );

        let always_safe = vec![
            decision(
                PaneMemoryStrategy::Checkpointed,
                PaneStrategyReason::ConservativeFallback,
            ),
            decision(
                PaneMemoryStrategy::Checkpointed,
                PaneStrategyReason::ConservativeFallback,
            ),
            decision(
                PaneMemoryStrategy::Checkpointed,
                PaneStrategyReason::ConservativeFallback,
            ),
        ];
        assert_eq!(
            monitor_fallback_frequency(&always_safe, &t).status,
            PaneMonitorStatus::Violated
        );
    }

    #[test]
    fn latency_envelope_classifies_relative_to_budget() {
        let t = PaneMonitorThresholds::default();
        assert_eq!(
            monitor_latency_envelope(PaneMemoryStrategy::Baseline, 400.0, 1000.0, &t).status,
            PaneMonitorStatus::Healthy
        );
        assert_eq!(
            monitor_latency_envelope(PaneMemoryStrategy::Baseline, 900.0, 1000.0, &t).status,
            PaneMonitorStatus::Degraded
        );
        assert_eq!(
            monitor_latency_envelope(PaneMemoryStrategy::Baseline, 1500.0, 1000.0, &t).status,
            PaneMonitorStatus::Violated
        );
    }

    #[test]
    fn report_aggregates_worst_status_and_violations() {
        let t = PaneMonitorThresholds::default();
        let report = PaneMonitorReport::new("scenario-x")
            .with(monitor_replay_depth(&replay_diag(16, 8, true), &t))
            .with(monitor_latency_envelope(
                PaneMemoryStrategy::Baseline,
                1500.0,
                1000.0,
                &t,
            ));
        assert_eq!(report.worst_status(), PaneMonitorStatus::Violated);
        assert!(report.has_violations());
        assert_eq!(report.violations().count(), 1);
    }

    #[test]
    fn report_is_deterministic_and_serializable() {
        let t = PaneMonitorThresholds::default();
        let build = || {
            PaneMonitorReport::new("scenario-y")
                .with(monitor_replay_depth(&replay_diag(16, 20, true), &t))
                .with(monitor_retention_pressure(
                    &retention_decision(PaneRetentionOutcome::PrunedToFit, 8192, 8000, 4),
                    &t,
                ))
        };
        let a = build().to_json();
        let b = build().to_json();
        assert_eq!(
            a, b,
            "monitor report must be byte-identical for identical inputs"
        );
        assert!(a.contains("\"assumption\":\"replay_depth_bound\""));
        assert!(a.contains("\"status\":\"degraded\""));
        // Operator summary names each assumption.
        let summary = build().summary_log();
        assert!(summary.contains("replay_depth_bound"));
        assert!(summary.contains("retention_pressure"));
    }

    #[test]
    fn status_worst_combinator_is_monotone() {
        use PaneMonitorStatus::{Degraded, Healthy, Violated};
        assert_eq!(Healthy.worst(Degraded), Degraded);
        assert_eq!(Degraded.worst(Healthy), Degraded);
        assert_eq!(Degraded.worst(Violated), Violated);
        assert_eq!(Violated.worst(Healthy), Violated);
        assert!(Violated.is_violation());
        assert!(!Degraded.is_violation());
        assert!(Degraded.is_degraded_or_worse());
    }
}

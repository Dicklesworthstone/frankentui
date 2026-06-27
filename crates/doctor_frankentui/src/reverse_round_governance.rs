// SPDX-License-Identifier: Apache-2.0
//! Reverse-round one-lever governance loop with baseline comparator contracts.
//!
//! Every alien-graveyard uplift change is introduced against a *declared
//! incumbent baseline*, mutating exactly *one optimization lever* per decision
//! window, and is only promoted once posterior evidence, an isomorphism verdict,
//! and a re-profile all agree. This module operationalizes that loop as a pure,
//! deterministic gate that consumes the upstream artifacts (posterior ledger,
//! golden-isomorphism proof, performance comparator deltas) and emits:
//!
//! - A canonical **reverse-round execution spec** ([`reverse_round_execution_spec`]):
//!   baseline → hotspot evidence → single lever → isomorphism proof → re-profile
//!   → decision, with the gate clause each stage answers to.
//! - A **comparator registry** keyed by `claim_id` ([`ComparatorRegistry`]): each
//!   [`BaselineComparator`] pins the incumbent baseline, metric family, per-percentile
//!   regression thresholds, and a confidence band.
//! - A **governance ledger** ([`GovernanceLedgerEntry`]) linking each `claim_id`
//!   to its posterior evidence ([`PosteriorEvidenceSummary`]), expected-loss action
//!   ([`MigrationDecision`]), and rollback policy/decision.
//!
//! # Acceptance criteria
//!
//! 1. Every uplift recommendation references a `comparator_id`, an incumbent
//!    baseline, and a one-lever scope *before* implementation. Missing or
//!    mismatched references are blocking findings ([`ReverseRoundFindingCode`]).
//! 2. The gate fails if more than one optimization lever is enabled in the same
//!    decision window without *both* an override artifact and a risk waiver.
//! 3. The gate emits a structured [`ReverseRoundDecisionLog`] per claim carrying
//!    `claim_id`, `comparator_id`, `lever_id`, `equation_id`, a Bayes-factor
//!    summary, p50/p95/p99 deltas, the isomorphism verdict, and the
//!    `rollback_decision`.
//!
//! # Determinism
//!
//! Claims and windows are processed in sorted order; findings, ledger entries,
//! and decision logs are sorted by stable keys; the report id and evidence
//! checksum are SHA-256 of the canonical serialization. Repeated runs over the
//! same request produce byte-identical reports.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::golden_isomorphism::ChecksumVerdict;
use crate::performance_diff::PerformanceMetricKind;
use crate::posterior_core::PosteriorRecord;
use crate::semantic_contract::MigrationDecision;

// ── Schema Version & Constants ───────────────────────────────────────────

/// Schema version for reverse-round governance reports.
pub const REVERSE_ROUND_GOVERNANCE_SCHEMA_VERSION: &str = "reverse-round-governance-v1";

// ── Severity ──────────────────────────────────────────────────────────────

/// Severity of a reverse-round governance finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReverseRoundSeverity {
    /// Blocking: the uplift may not be promoted while this finding stands.
    Block,
    /// Advisory: recorded for audit, does not block promotion.
    Warn,
}

impl ReverseRoundSeverity {
    /// Stable string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ReverseRoundSeverity::Block => "block",
            ReverseRoundSeverity::Warn => "warn",
        }
    }

    /// Whether this severity blocks promotion.
    #[must_use]
    pub fn is_blocking(self) -> bool {
        matches!(self, ReverseRoundSeverity::Block)
    }
}

// ── Reverse-Round Execution Spec ───────────────────────────────────────────

/// A stage in the canonical reverse-round execution loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReverseRoundStage {
    /// Capture the declared incumbent baseline.
    Baseline,
    /// Gather hotspot/change evidence justifying the lever.
    HotspotEvidence,
    /// Enable exactly one optimization lever.
    SingleLever,
    /// Prove behavior isomorphism against the golden baseline.
    IsomorphismProof,
    /// Re-profile against the comparator to obtain percentile deltas.
    Reprofile,
    /// Render the posterior/expected-loss decision and rollback readiness.
    Decision,
}

impl ReverseRoundStage {
    /// Every stage, in canonical execution order.
    pub const ORDER: &'static [ReverseRoundStage] = &[
        ReverseRoundStage::Baseline,
        ReverseRoundStage::HotspotEvidence,
        ReverseRoundStage::SingleLever,
        ReverseRoundStage::IsomorphismProof,
        ReverseRoundStage::Reprofile,
        ReverseRoundStage::Decision,
    ];

    /// Stable string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ReverseRoundStage::Baseline => "baseline",
            ReverseRoundStage::HotspotEvidence => "hotspot_evidence",
            ReverseRoundStage::SingleLever => "single_lever",
            ReverseRoundStage::IsomorphismProof => "isomorphism_proof",
            ReverseRoundStage::Reprofile => "reprofile",
            ReverseRoundStage::Decision => "decision",
        }
    }

    /// Zero-based position in the canonical order.
    #[must_use]
    pub fn order_index(self) -> usize {
        Self::ORDER
            .iter()
            .position(|stage| *stage == self)
            .unwrap_or(usize::MAX)
    }

    /// The gate clause this stage answers to.
    #[must_use]
    pub fn gate_clause(self) -> &'static str {
        match self {
            ReverseRoundStage::Baseline | ReverseRoundStage::HotspotEvidence => "RRG-REF-001",
            ReverseRoundStage::SingleLever => "RRG-LEVER-001",
            ReverseRoundStage::IsomorphismProof => "RRG-ISO-001",
            ReverseRoundStage::Reprofile => "RRG-PERF-001",
            ReverseRoundStage::Decision => "RRG-DECISION-001",
        }
    }
}

/// Specification for a single reverse-round stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseRoundStageSpec {
    /// The stage.
    pub stage: ReverseRoundStage,
    /// Zero-based order index.
    pub order_index: usize,
    /// Human-readable stage name.
    pub name: String,
    /// What the stage does and why it precedes the next.
    pub description: String,
    /// Inputs the stage requires.
    pub required_inputs: Vec<String>,
    /// Artifact produced by the stage.
    pub produced_artifact: String,
    /// Gate clause the stage answers to.
    pub gate_clause: String,
}

/// The canonical reverse-round execution spec (required output #1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseRoundExecutionSpec {
    /// Schema version.
    pub schema_version: String,
    /// Ordered stages.
    pub stages: Vec<ReverseRoundStageSpec>,
}

/// Build the canonical reverse-round execution spec.
#[must_use]
pub fn reverse_round_execution_spec() -> ReverseRoundExecutionSpec {
    let stages = ReverseRoundStage::ORDER
        .iter()
        .map(|stage| {
            let (name, description, required_inputs, produced_artifact) = match stage {
                ReverseRoundStage::Baseline => (
                    "Capture incumbent baseline",
                    "Pin the declared incumbent baseline change/version that the lever is measured against; no uplift starts without it.",
                    vec!["incumbent_baseline_id".to_string(), "comparator_id".to_string()],
                    "baseline_comparator",
                ),
                ReverseRoundStage::HotspotEvidence => (
                    "Gather hotspot evidence",
                    "Tie the proposed lever to change/hotspot evidence so the optimization is justified by a measured bottleneck, not a hunch.",
                    vec!["hotspot_refs".to_string(), "change_refs".to_string()],
                    "hotspot_evidence_bundle",
                ),
                ReverseRoundStage::SingleLever => (
                    "Enable a single lever",
                    "Enable exactly one optimization lever in the decision window; multiple concurrent levers require an override artifact and a risk waiver.",
                    vec!["lever_id".to_string(), "equation_id".to_string(), "one_lever_scope".to_string()],
                    "optimization_lever",
                ),
                ReverseRoundStage::IsomorphismProof => (
                    "Prove behavior isomorphism",
                    "Run the golden-isomorphism oracle to prove the lever preserves observable behavior (or only differs under declared invariants).",
                    vec!["golden_manifest".to_string(), "candidate_manifest".to_string()],
                    "isomorphism_proof",
                ),
                ReverseRoundStage::Reprofile => (
                    "Re-profile against comparator",
                    "Re-run the performance comparator to obtain p50/p95/p99 deltas versus the incumbent baseline within the comparator confidence band.",
                    vec!["comparator_id".to_string(), "percentile_deltas".to_string()],
                    "performance_comparison",
                ),
                ReverseRoundStage::Decision => (
                    "Render decision & rollback readiness",
                    "Combine posterior evidence, expected-loss action, and rollback policy into a governance-ledger entry and a structured decision log.",
                    vec!["posterior_record".to_string(), "rollback_policy".to_string()],
                    "governance_ledger_entry",
                ),
            };
            ReverseRoundStageSpec {
                stage: *stage,
                order_index: stage.order_index(),
                name: name.to_string(),
                description: description.to_string(),
                required_inputs,
                produced_artifact: produced_artifact.to_string(),
                gate_clause: stage.gate_clause().to_string(),
            }
        })
        .collect();
    ReverseRoundExecutionSpec {
        schema_version: REVERSE_ROUND_GOVERNANCE_SCHEMA_VERSION.to_string(),
        stages,
    }
}

// ── Metric Family ──────────────────────────────────────────────────────────

/// The metric family a comparator governs, wired to the performance comparator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricFamily {
    /// Latency percentiles (mean/p95/p99).
    Latency,
    /// Throughput (ops/sec).
    Throughput,
    /// Allocation bytes.
    Allocation,
    /// Frame timing (jitter, dropped-frame ratio).
    FrameTiming,
}

impl MetricFamily {
    /// Stable string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MetricFamily::Latency => "latency",
            MetricFamily::Throughput => "throughput",
            MetricFamily::Allocation => "allocation",
            MetricFamily::FrameTiming => "frame_timing",
        }
    }

    /// The performance-comparator metrics this family covers.
    #[must_use]
    pub fn covered_metrics(self) -> &'static [PerformanceMetricKind] {
        match self {
            MetricFamily::Latency => &[
                PerformanceMetricKind::LatencyMeanMs,
                PerformanceMetricKind::LatencyP95Ms,
                PerformanceMetricKind::LatencyP99Ms,
            ],
            MetricFamily::Throughput => &[PerformanceMetricKind::ThroughputOpsPerSec],
            MetricFamily::Allocation => &[PerformanceMetricKind::AllocationBytes],
            MetricFamily::FrameTiming => &[
                PerformanceMetricKind::FrameJitterMs,
                PerformanceMetricKind::DroppedFrameRatio,
            ],
        }
    }
}

// ── Comparator Thresholds & Confidence Band ────────────────────────────────

/// Per-percentile relative regression budgets (positive delta = regression).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ComparatorThresholds {
    /// Maximum tolerated relative regression at p50.
    pub max_p50_relative_regression: f64,
    /// Maximum tolerated relative regression at p95.
    pub max_p95_relative_regression: f64,
    /// Maximum tolerated relative regression at p99.
    pub max_p99_relative_regression: f64,
}

impl ComparatorThresholds {
    /// Construct explicit per-percentile budgets.
    #[must_use]
    pub fn new(p50: f64, p95: f64, p99: f64) -> Self {
        Self {
            max_p50_relative_regression: p50,
            max_p95_relative_regression: p95,
            max_p99_relative_regression: p99,
        }
    }

    /// Zero-tolerance budgets: any positive regression breaches.
    #[must_use]
    pub fn zero_regression() -> Self {
        Self::new(0.0, 0.0, 0.0)
    }
}

impl Default for ComparatorThresholds {
    /// Default budgets widen at higher percentiles (2% / 5% / 10%).
    fn default() -> Self {
        Self::new(0.02, 0.05, 0.10)
    }
}

/// Confidence band for the comparator (e.g. z=1.96, alpha=0.05).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConfidenceBand {
    /// Z-multiplier for the confidence interval.
    pub confidence_z: f64,
    /// Significance level alpha in `(0, 1)`.
    pub alpha: f64,
}

impl ConfidenceBand {
    /// Construct a confidence band.
    #[must_use]
    pub fn new(confidence_z: f64, alpha: f64) -> Self {
        Self {
            confidence_z,
            alpha,
        }
    }

    /// Whether the band is well-formed (`z > 0`, `0 < alpha < 1`).
    #[must_use]
    pub fn is_well_formed(self) -> bool {
        self.confidence_z.is_finite()
            && self.confidence_z > 0.0
            && self.alpha.is_finite()
            && self.alpha > 0.0
            && self.alpha < 1.0
    }
}

impl Default for ConfidenceBand {
    fn default() -> Self {
        Self::new(1.96, 0.05)
    }
}

// ── Baseline Comparator & Registry ─────────────────────────────────────────

/// An incumbent baseline comparator keyed to a single `claim_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineComparator {
    /// Stable comparator id (referenced by uplift recommendations).
    pub comparator_id: String,
    /// The claim this comparator governs.
    pub claim_id: String,
    /// The declared incumbent baseline (change/version id).
    pub incumbent_baseline_id: String,
    /// The metric family governed by this comparator.
    pub metric_family: MetricFamily,
    /// Per-percentile regression budgets.
    pub thresholds: ComparatorThresholds,
    /// Confidence band.
    pub confidence_band: ConfidenceBand,
}

impl BaselineComparator {
    /// Construct a comparator with default thresholds and confidence band.
    #[must_use]
    pub fn new(
        comparator_id: impl Into<String>,
        claim_id: impl Into<String>,
        incumbent_baseline_id: impl Into<String>,
        metric_family: MetricFamily,
    ) -> Self {
        Self {
            comparator_id: comparator_id.into(),
            claim_id: claim_id.into(),
            incumbent_baseline_id: incumbent_baseline_id.into(),
            metric_family,
            thresholds: ComparatorThresholds::default(),
            confidence_band: ConfidenceBand::default(),
        }
    }

    /// Override the thresholds.
    #[must_use]
    pub fn with_thresholds(mut self, thresholds: ComparatorThresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    /// Override the confidence band.
    #[must_use]
    pub fn with_confidence_band(mut self, band: ConfidenceBand) -> Self {
        self.confidence_band = band;
        self
    }

    /// Whether the comparator declares a non-empty incumbent baseline.
    #[must_use]
    pub fn has_incumbent_baseline(&self) -> bool {
        !self.incumbent_baseline_id.trim().is_empty()
    }
}

/// Error registering a comparator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ComparatorRegistryError {
    /// A comparator is already registered for this claim id.
    DuplicateClaim {
        /// The duplicate claim id.
        claim_id: String,
    },
    /// The comparator's `claim_id` did not match the registration key.
    ClaimKeyMismatch {
        /// The registration key.
        expected: String,
        /// The comparator's own claim id.
        found: String,
    },
    /// The comparator id was empty.
    EmptyComparatorId {
        /// The claim id whose comparator id was empty.
        claim_id: String,
    },
}

/// A comparator registry keyed by `claim_id` (required output #2).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComparatorRegistry {
    comparators: BTreeMap<String, BaselineComparator>,
}

impl ComparatorRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a comparator, keyed by its `claim_id`.
    ///
    /// # Errors
    ///
    /// Returns [`ComparatorRegistryError`] on a duplicate claim, a key/claim
    /// mismatch, or an empty comparator id.
    pub fn register(
        &mut self,
        comparator: BaselineComparator,
    ) -> Result<(), ComparatorRegistryError> {
        if comparator.comparator_id.trim().is_empty() {
            return Err(ComparatorRegistryError::EmptyComparatorId {
                claim_id: comparator.claim_id,
            });
        }
        let key = comparator.claim_id.clone();
        if self.comparators.contains_key(&key) {
            return Err(ComparatorRegistryError::DuplicateClaim { claim_id: key });
        }
        self.comparators.insert(key, comparator);
        Ok(())
    }

    /// Register a comparator under an explicit claim key, validating the match.
    ///
    /// # Errors
    ///
    /// Returns [`ComparatorRegistryError::ClaimKeyMismatch`] when the key and the
    /// comparator's own `claim_id` disagree, plus the errors of [`Self::register`].
    pub fn register_for(
        &mut self,
        claim_id: impl Into<String>,
        comparator: BaselineComparator,
    ) -> Result<(), ComparatorRegistryError> {
        let key = claim_id.into();
        if key != comparator.claim_id {
            return Err(ComparatorRegistryError::ClaimKeyMismatch {
                expected: key,
                found: comparator.claim_id,
            });
        }
        self.register(comparator)
    }

    /// Look up the comparator for a claim.
    #[must_use]
    pub fn get(&self, claim_id: &str) -> Option<&BaselineComparator> {
        self.comparators.get(claim_id)
    }

    /// The comparator id registered for a claim, if any.
    #[must_use]
    pub fn comparator_id_for(&self, claim_id: &str) -> Option<&str> {
        self.comparators
            .get(claim_id)
            .map(|comparator| comparator.comparator_id.as_str())
    }

    /// Number of registered comparators.
    #[must_use]
    pub fn len(&self) -> usize {
        self.comparators.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.comparators.is_empty()
    }

    /// All registered claim ids in sorted order.
    #[must_use]
    pub fn claim_ids(&self) -> Vec<String> {
        self.comparators.keys().cloned().collect()
    }
}

// ── Optimization Lever & Decision Window ───────────────────────────────────

/// A single optimization lever (one per uplift change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationLever {
    /// Stable lever id.
    pub lever_id: String,
    /// The equation/technique id this lever realizes.
    pub equation_id: String,
    /// The declared one-lever scope (what exactly changes).
    pub scope: String,
    /// Whether the lever is enabled in its decision window.
    pub enabled: bool,
}

impl OptimizationLever {
    /// Construct an enabled lever.
    #[must_use]
    pub fn new(
        lever_id: impl Into<String>,
        equation_id: impl Into<String>,
        scope: impl Into<String>,
    ) -> Self {
        Self {
            lever_id: lever_id.into(),
            equation_id: equation_id.into(),
            scope: scope.into(),
            enabled: true,
        }
    }

    /// Set the enabled flag.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Whether the lever declares a non-empty one-lever scope.
    #[must_use]
    pub fn has_scope(&self) -> bool {
        !self.scope.trim().is_empty()
    }
}

/// A decision window: the set of claims whose levers compete for the same change
/// budget. AC2 requires at most one enabled lever per window absent an override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionWindow {
    /// Stable window id.
    pub window_id: String,
    /// Claims active in this window.
    pub claim_ids: Vec<String>,
    /// Override artifact authorizing multiple concurrent levers, if any.
    pub override_artifact: Option<String>,
    /// Risk waiver accompanying a multi-lever override, if any.
    pub risk_waiver: Option<String>,
}

impl DecisionWindow {
    /// Construct a window over the given claims.
    #[must_use]
    pub fn new(window_id: impl Into<String>, claim_ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            window_id: window_id.into(),
            claim_ids: claim_ids.into_iter().collect(),
            override_artifact: None,
            risk_waiver: None,
        }
    }

    /// Attach an override artifact authorizing concurrent levers.
    #[must_use]
    pub fn with_override_artifact(mut self, artifact: impl Into<String>) -> Self {
        self.override_artifact = Some(artifact.into());
        self
    }

    /// Attach a risk waiver.
    #[must_use]
    pub fn with_risk_waiver(mut self, waiver: impl Into<String>) -> Self {
        self.risk_waiver = Some(waiver.into());
        self
    }
}

// ── Rollback Policy & Decision ─────────────────────────────────────────────

/// The declared rollback policy for an uplift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackPolicy {
    /// Automatically revert the lever when a regression is detected.
    AutomaticOnRegression,
    /// Stage rollback through a canary before full revert.
    StagedCanary,
    /// Manual rollback gated on operator authority.
    GuardedManual,
    /// Hold position; do not auto-rollback (escalate instead).
    HoldNoRollback,
}

impl RollbackPolicy {
    /// Stable string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RollbackPolicy::AutomaticOnRegression => "automatic_on_regression",
            RollbackPolicy::StagedCanary => "staged_canary",
            RollbackPolicy::GuardedManual => "guarded_manual",
            RollbackPolicy::HoldNoRollback => "hold_no_rollback",
        }
    }
}

/// The rollback decision resolved for a claim within its decision window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackDecision {
    /// No regression/violation; nothing to roll back.
    NoRollbackNeeded,
    /// A regression was detected; rollback is armed (staged canary).
    RollbackArmed,
    /// A regression was detected; rollback is triggered automatically.
    RollbackTriggered,
    /// A regression was detected but rollback awaits operator authority.
    RollbackBlockedNeedsAuthority,
    /// A regression was detected but policy suppresses auto-rollback (escalate).
    RollbackSuppressed,
}

impl RollbackDecision {
    /// Stable string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RollbackDecision::NoRollbackNeeded => "no_rollback_needed",
            RollbackDecision::RollbackArmed => "rollback_armed",
            RollbackDecision::RollbackTriggered => "rollback_triggered",
            RollbackDecision::RollbackBlockedNeedsAuthority => "rollback_blocked_needs_authority",
            RollbackDecision::RollbackSuppressed => "rollback_suppressed",
        }
    }

    /// Whether a regression/violation drove this decision.
    #[must_use]
    pub fn regression_present(self) -> bool {
        !matches!(self, RollbackDecision::NoRollbackNeeded)
    }
}

/// Resolve a rollback decision from policy + signals.
fn resolve_rollback_decision(policy: RollbackPolicy, regression_present: bool) -> RollbackDecision {
    if !regression_present {
        return RollbackDecision::NoRollbackNeeded;
    }
    match policy {
        RollbackPolicy::AutomaticOnRegression => RollbackDecision::RollbackTriggered,
        RollbackPolicy::StagedCanary => RollbackDecision::RollbackArmed,
        RollbackPolicy::GuardedManual => RollbackDecision::RollbackBlockedNeedsAuthority,
        RollbackPolicy::HoldNoRollback => RollbackDecision::RollbackSuppressed,
    }
}

// ── Percentile Deltas ──────────────────────────────────────────────────────

/// Relative percentile deltas versus the incumbent baseline.
///
/// Each value is a *relative regression*: positive means the candidate is worse
/// than the baseline (already normalized so that the comparator's direction is
/// applied upstream by the performance comparator).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PercentileDeltas {
    /// Relative regression at p50.
    pub p50_relative: f64,
    /// Relative regression at p95.
    pub p95_relative: f64,
    /// Relative regression at p99.
    pub p99_relative: f64,
}

impl PercentileDeltas {
    /// Construct percentile deltas.
    #[must_use]
    pub fn new(p50_relative: f64, p95_relative: f64, p99_relative: f64) -> Self {
        Self {
            p50_relative,
            p95_relative,
            p99_relative,
        }
    }

    /// All-improvement deltas (no regression at any percentile).
    #[must_use]
    pub fn improvement(magnitude: f64) -> Self {
        let mag = -magnitude.abs();
        Self::new(mag, mag, mag)
    }

    /// Percentiles whose regression exceeds the comparator budget.
    #[must_use]
    fn breaches(self, thresholds: ComparatorThresholds) -> Vec<PercentileBreach> {
        let mut out = Vec::new();
        let checks = [
            (
                "p50",
                self.p50_relative,
                thresholds.max_p50_relative_regression,
            ),
            (
                "p95",
                self.p95_relative,
                thresholds.max_p95_relative_regression,
            ),
            (
                "p99",
                self.p99_relative,
                thresholds.max_p99_relative_regression,
            ),
        ];
        for (label, delta, budget) in checks {
            if delta.is_finite() && delta > budget {
                out.push(PercentileBreach {
                    percentile: label,
                    delta,
                    budget,
                });
            } else if !delta.is_finite() {
                // A non-finite delta is treated as an unbounded breach.
                out.push(PercentileBreach {
                    percentile: label,
                    delta: f64::INFINITY,
                    budget,
                });
            }
        }
        out
    }
}

struct PercentileBreach {
    percentile: &'static str,
    delta: f64,
    budget: f64,
}

// ── Posterior Evidence & Bayes-Factor Summaries ────────────────────────────

/// Compact posterior evidence summary for the governance ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PosteriorEvidenceSummary {
    /// The claim id.
    pub claim_id: String,
    /// Posterior accept probability.
    pub posterior_prob: f64,
    /// Aggregate log Bayes factor across channels.
    pub aggregate_log_bayes_factor: f64,
    /// The expected-loss action (the posterior decision).
    pub expected_loss_action: MigrationDecision,
    /// Whether the posterior was degraded (missing channels).
    pub degraded: bool,
    /// Number of present evidence channels.
    pub present_channel_count: usize,
    /// Top contributing channel, if any.
    pub top_channel: Option<String>,
    /// Top channel's additive logit contribution.
    pub top_channel_contribution: f64,
}

impl PosteriorEvidenceSummary {
    /// Project a full posterior record into a compact summary.
    #[must_use]
    pub fn from_record(record: &PosteriorRecord) -> Self {
        let top = record.top_contributors(1);
        let (top_channel, top_channel_contribution) = top
            .first()
            .map(|contribution| {
                (
                    Some(contribution.channel.as_str().to_string()),
                    contribution.contribution,
                )
            })
            .unwrap_or((None, 0.0));
        Self {
            claim_id: record.claim_id.clone(),
            posterior_prob: record.posterior_prob,
            aggregate_log_bayes_factor: record.aggregate_log_bayes_factor,
            expected_loss_action: record.decision,
            degraded: record.degraded,
            present_channel_count: record.present_channel_count,
            top_channel,
            top_channel_contribution,
        }
    }
}

/// Bayes-factor summary for the structured decision log (AC3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BayesFactorSummary {
    /// Aggregate log Bayes factor.
    pub aggregate_log_bayes_factor: f64,
    /// Top channel (or `"n/a"`).
    pub top_channel: String,
    /// Top channel's log Bayes factor.
    pub top_channel_log_bf: f64,
    /// Number of present channels.
    pub present_channel_count: usize,
}

impl BayesFactorSummary {
    /// Build a Bayes-factor summary from a posterior record.
    #[must_use]
    pub fn from_record(record: &PosteriorRecord) -> Self {
        let top = record.top_contributors(1);
        let (top_channel, top_channel_log_bf) = top
            .first()
            .map(|contribution| {
                (
                    contribution.channel.as_str().to_string(),
                    contribution.log_bayes_factor,
                )
            })
            .unwrap_or_else(|| ("n/a".to_string(), 0.0));
        Self {
            aggregate_log_bayes_factor: record.aggregate_log_bayes_factor,
            top_channel,
            top_channel_log_bf,
            present_channel_count: record.present_channel_count,
        }
    }
}

// ── Uplift Claim (Input) ───────────────────────────────────────────────────

/// A single uplift claim presented to the reverse-round gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpliftClaim {
    /// The claim id (must match a registered comparator).
    pub claim_id: String,
    /// The comparator id the recommendation declares it is measured against.
    pub declared_comparator_id: String,
    /// The single optimization lever.
    pub lever: OptimizationLever,
    /// The posterior record (posterior evidence + expected-loss action).
    pub posterior: PosteriorRecord,
    /// The golden-isomorphism verdict for the lever.
    pub isomorphism_verdict: ChecksumVerdict,
    /// Re-profiled percentile deltas versus the incumbent baseline.
    pub percentile_deltas: PercentileDeltas,
    /// The declared rollback policy.
    pub rollback_policy: RollbackPolicy,
}

impl UpliftClaim {
    /// Construct an uplift claim.
    #[must_use]
    pub fn new(
        claim_id: impl Into<String>,
        declared_comparator_id: impl Into<String>,
        lever: OptimizationLever,
        posterior: PosteriorRecord,
        isomorphism_verdict: ChecksumVerdict,
        percentile_deltas: PercentileDeltas,
        rollback_policy: RollbackPolicy,
    ) -> Self {
        Self {
            claim_id: claim_id.into(),
            declared_comparator_id: declared_comparator_id.into(),
            lever,
            posterior,
            isomorphism_verdict,
            percentile_deltas,
            rollback_policy,
        }
    }
}

// ── Governance Ledger Entry (Output) ───────────────────────────────────────

/// A governance-ledger entry linking a claim to its evidence and rollback
/// posture (required output #3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GovernanceLedgerEntry {
    /// The claim id.
    pub claim_id: String,
    /// The resolved comparator id.
    pub comparator_id: String,
    /// The incumbent baseline id.
    pub incumbent_baseline_id: String,
    /// The metric family governed.
    pub metric_family: MetricFamily,
    /// The lever id.
    pub lever_id: String,
    /// The equation id realized by the lever.
    pub equation_id: String,
    /// Compact posterior evidence.
    pub posterior_evidence: PosteriorEvidenceSummary,
    /// The expected-loss action.
    pub expected_loss_action: MigrationDecision,
    /// The declared rollback policy.
    pub rollback_policy: RollbackPolicy,
    /// The resolved rollback decision.
    pub rollback_decision: RollbackDecision,
    /// Whether the claim was promotable (no blocking findings).
    pub promotable: bool,
}

// ── Structured Decision Log (AC3) ──────────────────────────────────────────

/// A structured per-claim decision log (AC3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReverseRoundDecisionLog {
    /// The claim id.
    pub claim_id: String,
    /// The comparator id.
    pub comparator_id: String,
    /// The lever id.
    pub lever_id: String,
    /// The equation id.
    pub equation_id: String,
    /// Bayes-factor summary.
    pub bayes_factor_summary: BayesFactorSummary,
    /// Relative regression at p50.
    pub p50_delta: f64,
    /// Relative regression at p95.
    pub p95_delta: f64,
    /// Relative regression at p99.
    pub p99_delta: f64,
    /// The isomorphism verdict (stable string).
    pub isomorphism_verdict: String,
    /// The resolved rollback decision.
    pub rollback_decision: RollbackDecision,
}

// ── Findings ───────────────────────────────────────────────────────────────

/// Finding codes for the reverse-round gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReverseRoundFindingCode {
    /// AC1: the recommendation declares no comparator id.
    MissingComparatorReference,
    /// AC1: no comparator is registered for the claim.
    UnregisteredComparator,
    /// AC1: the declared comparator id disagrees with the registry.
    ComparatorReferenceMismatch,
    /// AC1: the comparator declares no incumbent baseline.
    MissingIncumbentBaseline,
    /// AC1: the lever declares no one-lever scope.
    MissingOneLeverScope,
    /// AC2: multiple enabled levers in a window without an override artifact.
    MultiLeverWithoutOverride,
    /// AC2: multi-lever override present but no risk waiver.
    MultiLeverWithoutRiskWaiver,
    /// AC2: a multi-lever override + waiver was applied (advisory audit record).
    MultiLeverOverrideApplied,
    /// A window references a claim id with no matching uplift claim.
    UnknownClaimInWindow,
    /// The isomorphism proof shows behavior changed (mismatch).
    IsomorphismViolation,
    /// The isomorphism proof matched only under declared invariants (advisory).
    IsomorphismNormalizedMatch,
    /// A re-profiled percentile regression exceeds the comparator budget.
    PercentileRegressionBeyondBudget,
    /// The comparator declares a malformed confidence band.
    MalformedConfidenceBand,
}

impl ReverseRoundFindingCode {
    /// Stable string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ReverseRoundFindingCode::MissingComparatorReference => "missing_comparator_reference",
            ReverseRoundFindingCode::UnregisteredComparator => "unregistered_comparator",
            ReverseRoundFindingCode::ComparatorReferenceMismatch => "comparator_reference_mismatch",
            ReverseRoundFindingCode::MissingIncumbentBaseline => "missing_incumbent_baseline",
            ReverseRoundFindingCode::MissingOneLeverScope => "missing_one_lever_scope",
            ReverseRoundFindingCode::MultiLeverWithoutOverride => "multi_lever_without_override",
            ReverseRoundFindingCode::MultiLeverWithoutRiskWaiver => {
                "multi_lever_without_risk_waiver"
            }
            ReverseRoundFindingCode::MultiLeverOverrideApplied => "multi_lever_override_applied",
            ReverseRoundFindingCode::UnknownClaimInWindow => "unknown_claim_in_window",
            ReverseRoundFindingCode::IsomorphismViolation => "isomorphism_violation",
            ReverseRoundFindingCode::IsomorphismNormalizedMatch => "isomorphism_normalized_match",
            ReverseRoundFindingCode::PercentileRegressionBeyondBudget => {
                "percentile_regression_beyond_budget"
            }
            ReverseRoundFindingCode::MalformedConfidenceBand => "malformed_confidence_band",
        }
    }

    /// Stable clause id (the gate clause this code enforces).
    #[must_use]
    pub fn clause_id(self) -> &'static str {
        match self {
            ReverseRoundFindingCode::MissingComparatorReference
            | ReverseRoundFindingCode::UnregisteredComparator
            | ReverseRoundFindingCode::ComparatorReferenceMismatch
            | ReverseRoundFindingCode::MissingIncumbentBaseline => "RRG-REF-001",
            ReverseRoundFindingCode::MissingOneLeverScope => "RRG-LEVER-001",
            ReverseRoundFindingCode::MultiLeverWithoutOverride
            | ReverseRoundFindingCode::MultiLeverWithoutRiskWaiver
            | ReverseRoundFindingCode::MultiLeverOverrideApplied
            | ReverseRoundFindingCode::UnknownClaimInWindow => "RRG-LEVER-002",
            ReverseRoundFindingCode::IsomorphismViolation
            | ReverseRoundFindingCode::IsomorphismNormalizedMatch => "RRG-ISO-001",
            ReverseRoundFindingCode::PercentileRegressionBeyondBudget => "RRG-PERF-001",
            ReverseRoundFindingCode::MalformedConfidenceBand => "RRG-PERF-002",
        }
    }

    /// Default severity for this code.
    #[must_use]
    pub fn default_severity(self) -> ReverseRoundSeverity {
        match self {
            ReverseRoundFindingCode::MultiLeverOverrideApplied
            | ReverseRoundFindingCode::IsomorphismNormalizedMatch
            | ReverseRoundFindingCode::UnknownClaimInWindow => ReverseRoundSeverity::Warn,
            _ => ReverseRoundSeverity::Block,
        }
    }

    /// Upstream dependency hint (the bead/track responsible for the input).
    #[must_use]
    pub fn dependency_hint(self) -> &'static str {
        match self {
            ReverseRoundFindingCode::MissingComparatorReference
            | ReverseRoundFindingCode::UnregisteredComparator
            | ReverseRoundFindingCode::ComparatorReferenceMismatch
            | ReverseRoundFindingCode::MissingIncumbentBaseline => "bd-3bxhj.6.5",
            ReverseRoundFindingCode::MissingOneLeverScope
            | ReverseRoundFindingCode::MultiLeverWithoutOverride
            | ReverseRoundFindingCode::MultiLeverWithoutRiskWaiver
            | ReverseRoundFindingCode::MultiLeverOverrideApplied
            | ReverseRoundFindingCode::UnknownClaimInWindow => "bd-3bxhj.8.18",
            ReverseRoundFindingCode::IsomorphismViolation
            | ReverseRoundFindingCode::IsomorphismNormalizedMatch => "bd-3bxhj.10.14",
            ReverseRoundFindingCode::PercentileRegressionBeyondBudget => "bd-3bxhj.8.5",
            ReverseRoundFindingCode::MalformedConfidenceBand => "bd-3bxhj.1.4",
        }
    }
}

/// A finding emitted by the reverse-round gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseRoundFinding {
    /// The finding code.
    pub code: ReverseRoundFindingCode,
    /// Severity.
    pub severity: ReverseRoundSeverity,
    /// Clause id.
    pub clause_id: String,
    /// Claim id (or `"n/a"` for window-scoped findings).
    pub claim_id: String,
    /// Window id (or `"n/a"` for claim-scoped findings).
    pub window_id: String,
    /// Human-readable detail.
    pub detail: String,
    /// Remediation hint.
    pub remediation: String,
    /// Upstream dependency hint.
    pub dependency_hint: String,
}

impl ReverseRoundFinding {
    /// Whether this finding blocks promotion.
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.severity.is_blocking()
    }

    fn claim_scoped(
        code: ReverseRoundFindingCode,
        claim_id: impl Into<String>,
        detail: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: code.default_severity(),
            clause_id: code.clause_id().to_string(),
            claim_id: claim_id.into(),
            window_id: "n/a".to_string(),
            detail: detail.into(),
            remediation: remediation.into(),
            dependency_hint: code.dependency_hint().to_string(),
        }
    }

    fn window_scoped(
        code: ReverseRoundFindingCode,
        window_id: impl Into<String>,
        detail: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: code.default_severity(),
            clause_id: code.clause_id().to_string(),
            claim_id: "n/a".to_string(),
            window_id: window_id.into(),
            detail: detail.into(),
            remediation: remediation.into(),
            dependency_hint: code.dependency_hint().to_string(),
        }
    }
}

// ── Config & Request ───────────────────────────────────────────────────────

/// Configuration for the reverse-round gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReverseRoundConfig {
    /// Whether a normalized (invariant-only) isomorphism match blocks promotion.
    /// When `false` it is recorded as an advisory warning instead.
    pub normalized_match_blocks: bool,
    /// Whether a multi-lever override + waiver still records an advisory.
    pub record_override_audit: bool,
}

impl Default for ReverseRoundConfig {
    fn default() -> Self {
        Self {
            normalized_match_blocks: false,
            record_override_audit: true,
        }
    }
}

/// The full reverse-round governance request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReverseRoundRequest {
    /// The comparator registry.
    pub registry: ComparatorRegistry,
    /// The decision windows.
    pub windows: Vec<DecisionWindow>,
    /// The uplift claims.
    pub claims: Vec<UpliftClaim>,
}

impl ReverseRoundRequest {
    /// Construct a request.
    #[must_use]
    pub fn new(
        registry: ComparatorRegistry,
        windows: Vec<DecisionWindow>,
        claims: Vec<UpliftClaim>,
    ) -> Self {
        Self {
            registry,
            windows,
            claims,
        }
    }
}

// ── Summary & Report ───────────────────────────────────────────────────────

/// Aggregate summary for a reverse-round governance report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseRoundSummary {
    /// Total uplift claims.
    pub total_claims: usize,
    /// Total decision windows.
    pub total_windows: usize,
    /// Windows with more than one enabled lever.
    pub multi_lever_windows: usize,
    /// Total enabled levers across all claims.
    pub enabled_lever_count: usize,
    /// Blocking findings.
    pub blocking_findings: usize,
    /// Advisory findings.
    pub advisory_findings: usize,
    /// Claims with `NoRollbackNeeded`.
    pub rollback_none: usize,
    /// Claims with `RollbackArmed`.
    pub rollback_armed: usize,
    /// Claims with `RollbackTriggered`.
    pub rollback_triggered: usize,
    /// Claims with `RollbackBlockedNeedsAuthority`.
    pub rollback_blocked: usize,
    /// Claims with `RollbackSuppressed`.
    pub rollback_suppressed: usize,
    /// Claims whose isomorphism verdict was a byte-identical match.
    pub isomorphism_match: usize,
    /// Claims whose isomorphism verdict was a normalized (invariant) match.
    pub isomorphism_normalized_match: usize,
    /// Claims whose isomorphism verdict was a mismatch.
    pub isomorphism_mismatch: usize,
    /// Promotable claims (no blocking finding).
    pub promotable_claims: usize,
    /// Whether the overall gate passes.
    pub gate_passes: bool,
}

/// Self-contained JSON-stats artifact for CI consumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseRoundJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// The full reverse-round governance report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReverseRoundGovernanceReport {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// The configuration used.
    pub config: ReverseRoundConfig,
    /// The canonical execution spec.
    pub execution_spec: ReverseRoundExecutionSpec,
    /// All findings, sorted deterministically.
    pub findings: Vec<ReverseRoundFinding>,
    /// The governance ledger, one entry per claim.
    pub ledger: Vec<GovernanceLedgerEntry>,
    /// Structured decision logs, one per claim (AC3).
    pub decision_logs: Vec<ReverseRoundDecisionLog>,
    /// Aggregate summary.
    pub summary: ReverseRoundSummary,
    /// Whether the gate passes (no blocking findings).
    pub gate_passes: bool,
    /// Claim ids with at least one blocking finding.
    pub blocked_claim_ids: Vec<String>,
    /// Exported JSON-stats artifact.
    pub exported_json_stats: ReverseRoundJsonStatsArtifact,
    /// Deterministic replay command.
    pub replay_command: String,
    /// Evidence checksum over findings/ledger/logs/summary.
    pub evidence_checksum: String,
}

impl ReverseRoundGovernanceReport {
    /// All blocking findings.
    #[must_use]
    pub fn blocking(&self) -> Vec<&ReverseRoundFinding> {
        self.findings.iter().filter(|f| f.is_blocking()).collect()
    }

    /// Findings scoped to a given claim id.
    #[must_use]
    pub fn findings_for(&self, claim_id: &str) -> Vec<&ReverseRoundFinding> {
        self.findings
            .iter()
            .filter(|f| f.claim_id == claim_id)
            .collect()
    }

    /// The ledger entry for a claim, if any.
    #[must_use]
    pub fn ledger_entry(&self, claim_id: &str) -> Option<&GovernanceLedgerEntry> {
        self.ledger.iter().find(|e| e.claim_id == claim_id)
    }

    /// The decision log for a claim, if any.
    #[must_use]
    pub fn decision_log(&self, claim_id: &str) -> Option<&ReverseRoundDecisionLog> {
        self.decision_logs.iter().find(|l| l.claim_id == claim_id)
    }
}

// ── Gate ───────────────────────────────────────────────────────────────────

/// The reverse-round one-lever governance gate.
#[derive(Debug, Clone, Default)]
pub struct ReverseRoundGate {
    config: ReverseRoundConfig,
}

impl ReverseRoundGate {
    /// Construct a gate.
    #[must_use]
    pub fn new(config: ReverseRoundConfig) -> Self {
        Self { config }
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &ReverseRoundConfig {
        &self.config
    }

    /// Evaluate a reverse-round governance request into a deterministic report.
    #[must_use]
    pub fn evaluate(&self, request: &ReverseRoundRequest) -> ReverseRoundGovernanceReport {
        // Index claims by id (sorted, deterministic).
        let mut claims: Vec<&UpliftClaim> = request.claims.iter().collect();
        claims.sort_by(|a, b| a.claim_id.cmp(&b.claim_id));
        let claim_index: BTreeMap<&str, &UpliftClaim> = claims
            .iter()
            .map(|claim| (claim.claim_id.as_str(), *claim))
            .collect();

        let mut findings: Vec<ReverseRoundFinding> = Vec::new();
        let mut ledger: Vec<GovernanceLedgerEntry> = Vec::new();
        let mut decision_logs: Vec<ReverseRoundDecisionLog> = Vec::new();

        // ── Per-claim evaluation (AC1, isomorphism, perf, ledger, AC3 log) ──
        for claim in &claims {
            self.evaluate_claim(
                claim,
                &request.registry,
                &mut findings,
                &mut ledger,
                &mut decision_logs,
            );
        }

        // ── Per-window evaluation (AC2: one-lever-per-window) ──
        let mut windows: Vec<&DecisionWindow> = request.windows.iter().collect();
        windows.sort_by(|a, b| a.window_id.cmp(&b.window_id));
        let mut multi_lever_windows = 0usize;
        for window in &windows {
            if self.evaluate_window(window, &claim_index, &mut findings) {
                multi_lever_windows += 1;
            }
        }

        // ── Determinism: sort findings by (claim, window, clause, code) ──
        findings.sort_by(|a, b| {
            a.claim_id
                .cmp(&b.claim_id)
                .then_with(|| a.window_id.cmp(&b.window_id))
                .then_with(|| a.clause_id.cmp(&b.clause_id))
                .then_with(|| a.code.as_str().cmp(b.code.as_str()))
                .then_with(|| a.detail.cmp(&b.detail))
        });

        // Stamp promotability onto ledger entries now that all findings exist.
        let blocked: std::collections::BTreeSet<String> = findings
            .iter()
            .filter(|f| f.is_blocking() && f.claim_id != "n/a")
            .map(|f| f.claim_id.clone())
            .collect();
        for entry in &mut ledger {
            entry.promotable = !blocked.contains(&entry.claim_id);
        }
        ledger.sort_by(|a, b| a.claim_id.cmp(&b.claim_id));
        decision_logs.sort_by(|a, b| a.claim_id.cmp(&b.claim_id));

        let summary = build_summary(
            &claims,
            windows.len(),
            multi_lever_windows,
            &findings,
            &ledger,
        );
        let gate_passes = summary.gate_passes;
        let blocked_claim_ids: Vec<String> = blocked.into_iter().collect();

        let report_id = report_id_for(&self.config, request);
        let evidence_checksum = evidence_checksum_for(&findings, &ledger, &decision_logs, &summary);
        let replay_command = format!(
            "cargo test -p doctor_frankentui --lib reverse_round_governance -- --nocapture # report {report_id}"
        );
        let exported_json_stats = export_json_stats(
            &report_id,
            &self.config,
            &findings,
            &ledger,
            &decision_logs,
            &summary,
        );

        ReverseRoundGovernanceReport {
            schema_version: REVERSE_ROUND_GOVERNANCE_SCHEMA_VERSION.to_string(),
            report_id,
            config: self.config.clone(),
            execution_spec: reverse_round_execution_spec(),
            findings,
            ledger,
            decision_logs,
            summary,
            gate_passes,
            blocked_claim_ids,
            exported_json_stats,
            replay_command,
            evidence_checksum,
        }
    }

    /// Evaluate a single claim, appending findings/ledger/log entries.
    fn evaluate_claim(
        &self,
        claim: &UpliftClaim,
        registry: &ComparatorRegistry,
        findings: &mut Vec<ReverseRoundFinding>,
        ledger: &mut Vec<GovernanceLedgerEntry>,
        decision_logs: &mut Vec<ReverseRoundDecisionLog>,
    ) {
        let comparator = registry.get(&claim.claim_id);

        // AC1: comparator reference present.
        if claim.declared_comparator_id.trim().is_empty() {
            findings.push(ReverseRoundFinding::claim_scoped(
                ReverseRoundFindingCode::MissingComparatorReference,
                &claim.claim_id,
                format!("claim '{}' declares no comparator_id", claim.claim_id),
                "declare the comparator_id the uplift is measured against before implementation",
            ));
        }

        // AC1: comparator registered + matches + has incumbent baseline.
        let (resolved_comparator_id, incumbent_baseline_id, metric_family) = match comparator {
            None => {
                findings.push(ReverseRoundFinding::claim_scoped(
                    ReverseRoundFindingCode::UnregisteredComparator,
                    &claim.claim_id,
                    format!(
                        "no comparator registered for claim '{}' (declared '{}')",
                        claim.claim_id, claim.declared_comparator_id
                    ),
                    "register a BaselineComparator for this claim_id before promotion",
                ));
                (
                    claim.declared_comparator_id.clone(),
                    String::new(),
                    MetricFamily::Latency,
                )
            }
            Some(comparator) => {
                if !claim.declared_comparator_id.trim().is_empty()
                    && claim.declared_comparator_id != comparator.comparator_id
                {
                    findings.push(ReverseRoundFinding::claim_scoped(
                        ReverseRoundFindingCode::ComparatorReferenceMismatch,
                        &claim.claim_id,
                        format!(
                            "claim '{}' declares comparator '{}' but registry holds '{}'",
                            claim.claim_id, claim.declared_comparator_id, comparator.comparator_id
                        ),
                        "align the declared comparator_id with the registered comparator",
                    ));
                }
                if !comparator.has_incumbent_baseline() {
                    findings.push(ReverseRoundFinding::claim_scoped(
                        ReverseRoundFindingCode::MissingIncumbentBaseline,
                        &claim.claim_id,
                        format!(
                            "comparator '{}' for claim '{}' declares no incumbent baseline",
                            comparator.comparator_id, claim.claim_id
                        ),
                        "pin the incumbent baseline change/version on the comparator",
                    ));
                }
                if !comparator.confidence_band.is_well_formed() {
                    findings.push(ReverseRoundFinding::claim_scoped(
                        ReverseRoundFindingCode::MalformedConfidenceBand,
                        &claim.claim_id,
                        format!(
                            "comparator '{}' has malformed confidence band (z={}, alpha={})",
                            comparator.comparator_id,
                            comparator.confidence_band.confidence_z,
                            comparator.confidence_band.alpha
                        ),
                        "set z > 0 and 0 < alpha < 1 on the comparator confidence band",
                    ));
                }
                (
                    comparator.comparator_id.clone(),
                    comparator.incumbent_baseline_id.clone(),
                    comparator.metric_family,
                )
            }
        };

        // AC1: one-lever scope declared.
        if !claim.lever.has_scope() {
            findings.push(ReverseRoundFinding::claim_scoped(
                ReverseRoundFindingCode::MissingOneLeverScope,
                &claim.claim_id,
                format!(
                    "lever '{}' for claim '{}' declares no one-lever scope",
                    claim.lever.lever_id, claim.claim_id
                ),
                "declare exactly what the single lever changes (its scope) before enabling it",
            ));
        }

        // Isomorphism verdict.
        let mut iso_violation = false;
        match claim.isomorphism_verdict {
            ChecksumVerdict::Match => {}
            ChecksumVerdict::NormalizedMatch => {
                let mut finding = ReverseRoundFinding::claim_scoped(
                    ReverseRoundFindingCode::IsomorphismNormalizedMatch,
                    &claim.claim_id,
                    format!(
                        "claim '{}' matches the golden baseline only under declared invariants",
                        claim.claim_id
                    ),
                    "confirm the declared invariants are load-bearing and intended",
                );
                if self.config.normalized_match_blocks {
                    finding.severity = ReverseRoundSeverity::Block;
                    iso_violation = true;
                }
                findings.push(finding);
            }
            ChecksumVerdict::Mismatch => {
                iso_violation = true;
                findings.push(ReverseRoundFinding::claim_scoped(
                    ReverseRoundFindingCode::IsomorphismViolation,
                    &claim.claim_id,
                    format!(
                        "claim '{}' changed observable behavior versus the golden baseline",
                        claim.claim_id
                    ),
                    "restore behavior isomorphism or declare and justify the invariant difference",
                ));
            }
        }

        // Re-profile: percentile regression budgets.
        let thresholds = comparator
            .map(|c| c.thresholds)
            .unwrap_or_else(ComparatorThresholds::zero_regression);
        let breaches = claim.percentile_deltas.breaches(thresholds);
        let perf_regression = !breaches.is_empty();
        for breach in &breaches {
            findings.push(ReverseRoundFinding::claim_scoped(
                ReverseRoundFindingCode::PercentileRegressionBeyondBudget,
                &claim.claim_id,
                format!(
                    "claim '{}' {} regression {:.4} exceeds budget {:.4}",
                    claim.claim_id, breach.percentile, breach.delta, breach.budget
                ),
                "shrink the regression below the comparator budget or revise the budget with justification",
            ));
        }

        // Rollback decision.
        let posterior_negative = matches!(
            claim.posterior.decision,
            MigrationDecision::Reject | MigrationDecision::HardReject | MigrationDecision::Rollback
        );
        let regression_present = iso_violation || perf_regression || posterior_negative;
        let rollback_decision =
            resolve_rollback_decision(claim.rollback_policy, regression_present);

        // Ledger entry + AC3 decision log.
        ledger.push(GovernanceLedgerEntry {
            claim_id: claim.claim_id.clone(),
            comparator_id: resolved_comparator_id.clone(),
            incumbent_baseline_id,
            metric_family,
            lever_id: claim.lever.lever_id.clone(),
            equation_id: claim.lever.equation_id.clone(),
            posterior_evidence: PosteriorEvidenceSummary::from_record(&claim.posterior),
            expected_loss_action: claim.posterior.decision,
            rollback_policy: claim.rollback_policy,
            rollback_decision,
            promotable: true, // re-stamped after all findings collected
        });

        decision_logs.push(ReverseRoundDecisionLog {
            claim_id: claim.claim_id.clone(),
            comparator_id: resolved_comparator_id,
            lever_id: claim.lever.lever_id.clone(),
            equation_id: claim.lever.equation_id.clone(),
            bayes_factor_summary: BayesFactorSummary::from_record(&claim.posterior),
            p50_delta: claim.percentile_deltas.p50_relative,
            p95_delta: claim.percentile_deltas.p95_relative,
            p99_delta: claim.percentile_deltas.p99_relative,
            isomorphism_verdict: claim.isomorphism_verdict.as_str().to_string(),
            rollback_decision,
        });
    }

    /// Evaluate a window's one-lever discipline (AC2). Returns whether the window
    /// had more than one enabled lever.
    fn evaluate_window(
        &self,
        window: &DecisionWindow,
        claim_index: &BTreeMap<&str, &UpliftClaim>,
        findings: &mut Vec<ReverseRoundFinding>,
    ) -> bool {
        let mut enabled = 0usize;
        for claim_id in &window.claim_ids {
            match claim_index.get(claim_id.as_str()) {
                Some(claim) => {
                    if claim.lever.enabled {
                        enabled += 1;
                    }
                }
                None => {
                    findings.push(ReverseRoundFinding::window_scoped(
                        ReverseRoundFindingCode::UnknownClaimInWindow,
                        &window.window_id,
                        format!(
                            "window '{}' references unknown claim '{}'",
                            window.window_id, claim_id
                        ),
                        "remove the stale claim reference or add the missing uplift claim",
                    ));
                }
            }
        }

        let multi_lever = enabled > 1;
        if multi_lever {
            if window.override_artifact.is_none() {
                findings.push(ReverseRoundFinding::window_scoped(
                    ReverseRoundFindingCode::MultiLeverWithoutOverride,
                    &window.window_id,
                    format!(
                        "window '{}' enables {} levers without an override artifact",
                        window.window_id, enabled
                    ),
                    "reduce to one enabled lever, or attach an override artifact and risk waiver",
                ));
            } else if window.risk_waiver.is_none() {
                findings.push(ReverseRoundFinding::window_scoped(
                    ReverseRoundFindingCode::MultiLeverWithoutRiskWaiver,
                    &window.window_id,
                    format!(
                        "window '{}' has a multi-lever override but no risk waiver",
                        window.window_id
                    ),
                    "attach a risk waiver to accompany the multi-lever override artifact",
                ));
            } else if self.config.record_override_audit {
                findings.push(ReverseRoundFinding::window_scoped(
                    ReverseRoundFindingCode::MultiLeverOverrideApplied,
                    &window.window_id,
                    format!(
                        "window '{}' enables {} levers under override '{}' + waiver '{}'",
                        window.window_id,
                        enabled,
                        window.override_artifact.as_deref().unwrap_or("n/a"),
                        window.risk_waiver.as_deref().unwrap_or("n/a"),
                    ),
                    "ensure the override and waiver remain valid for this decision window",
                ));
            }
        }
        multi_lever
    }
}

// ── Summary & Hashing Helpers ──────────────────────────────────────────────

fn build_summary(
    claims: &[&UpliftClaim],
    total_windows: usize,
    multi_lever_windows: usize,
    findings: &[ReverseRoundFinding],
    ledger: &[GovernanceLedgerEntry],
) -> ReverseRoundSummary {
    let enabled_lever_count = claims.iter().filter(|c| c.lever.enabled).count();
    let blocking_findings = findings.iter().filter(|f| f.is_blocking()).count();
    let advisory_findings = findings.len() - blocking_findings;

    let mut rollback_none = 0;
    let mut rollback_armed = 0;
    let mut rollback_triggered = 0;
    let mut rollback_blocked = 0;
    let mut rollback_suppressed = 0;
    for entry in ledger {
        match entry.rollback_decision {
            RollbackDecision::NoRollbackNeeded => rollback_none += 1,
            RollbackDecision::RollbackArmed => rollback_armed += 1,
            RollbackDecision::RollbackTriggered => rollback_triggered += 1,
            RollbackDecision::RollbackBlockedNeedsAuthority => rollback_blocked += 1,
            RollbackDecision::RollbackSuppressed => rollback_suppressed += 1,
        }
    }

    let mut isomorphism_match = 0;
    let mut isomorphism_normalized_match = 0;
    let mut isomorphism_mismatch = 0;
    for claim in claims {
        match claim.isomorphism_verdict {
            ChecksumVerdict::Match => isomorphism_match += 1,
            ChecksumVerdict::NormalizedMatch => isomorphism_normalized_match += 1,
            ChecksumVerdict::Mismatch => isomorphism_mismatch += 1,
        }
    }

    let promotable_claims = ledger.iter().filter(|e| e.promotable).count();
    let gate_passes = blocking_findings == 0;

    ReverseRoundSummary {
        total_claims: claims.len(),
        total_windows,
        multi_lever_windows,
        enabled_lever_count,
        blocking_findings,
        advisory_findings,
        rollback_none,
        rollback_armed,
        rollback_triggered,
        rollback_blocked,
        rollback_suppressed,
        isomorphism_match,
        isomorphism_normalized_match,
        isomorphism_mismatch,
        promotable_claims,
        gate_passes,
    }
}

fn export_json_stats(
    report_id: &str,
    config: &ReverseRoundConfig,
    findings: &[ReverseRoundFinding],
    ledger: &[GovernanceLedgerEntry],
    decision_logs: &[ReverseRoundDecisionLog],
    summary: &ReverseRoundSummary,
) -> ReverseRoundJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        config: &'a ReverseRoundConfig,
        summary: &'a ReverseRoundSummary,
        findings: &'a [ReverseRoundFinding],
        ledger: &'a [GovernanceLedgerEntry],
        decision_logs: &'a [ReverseRoundDecisionLog],
    }

    let payload = Export {
        schema_version: REVERSE_ROUND_GOVERNANCE_SCHEMA_VERSION,
        report_id,
        config,
        summary,
        findings,
        ledger,
        decision_logs,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    ReverseRoundJsonStatsArtifact {
        path: format!("{report_id}/reverse_round_governance_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn report_id_for(config: &ReverseRoundConfig, request: &ReverseRoundRequest) -> String {
    // Canonicalize claim/window order so the report id is input-order-independent
    // (the registry is a BTreeMap and is already canonical).
    let mut claims: Vec<&UpliftClaim> = request.claims.iter().collect();
    claims.sort_by(|a, b| a.claim_id.cmp(&b.claim_id));
    let mut windows: Vec<&DecisionWindow> = request.windows.iter().collect();
    windows.sort_by(|a, b| a.window_id.cmp(&b.window_id));

    #[derive(Serialize)]
    struct IdInput<'a> {
        schema_version: &'a str,
        config: &'a ReverseRoundConfig,
        registry: &'a ComparatorRegistry,
        claims: Vec<&'a UpliftClaim>,
        windows: Vec<&'a DecisionWindow>,
    }
    let hash = stable_hash(&IdInput {
        schema_version: REVERSE_ROUND_GOVERNANCE_SCHEMA_VERSION,
        config,
        registry: &request.registry,
        claims,
        windows,
    });
    format!("reverse-round-governance-{}", short_hash(&hash))
}

fn evidence_checksum_for(
    findings: &[ReverseRoundFinding],
    ledger: &[GovernanceLedgerEntry],
    decision_logs: &[ReverseRoundDecisionLog],
    summary: &ReverseRoundSummary,
) -> String {
    #[derive(Serialize)]
    struct Evidence<'a> {
        findings: &'a [ReverseRoundFinding],
        ledger: &'a [GovernanceLedgerEntry],
        decision_logs: &'a [ReverseRoundDecisionLog],
        summary: &'a ReverseRoundSummary,
    }
    stable_hash(&Evidence {
        findings,
        ledger,
        decision_logs,
        summary,
    })
}

fn stable_hash<T: Serialize + ?Sized>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}

fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::posterior_core::{ChannelEvidence, EvidenceChannel, PosteriorEngine};

    // ── Fixtures ───────────────────────────────────────────────────────────

    fn strong_accept_posterior(claim_id: &str) -> PosteriorRecord {
        let engine = PosteriorEngine::default();
        let evidence = vec![
            ChannelEvidence::new(EvidenceChannel::Semantic, 3.0).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Visual, 2.5).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Performance, 2.0).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Accessibility, 1.5).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Determinism, 1.5).claim(claim_id),
        ];
        engine.infer(claim_id, &evidence)
    }

    fn reject_posterior(claim_id: &str) -> PosteriorRecord {
        let engine = PosteriorEngine::default();
        let evidence = vec![
            ChannelEvidence::new(EvidenceChannel::Semantic, -4.0).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Visual, -3.0).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Performance, -3.0).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Accessibility, -2.0).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Determinism, -2.0).claim(claim_id),
        ];
        engine.infer(claim_id, &evidence)
    }

    fn comparator(claim_id: &str) -> BaselineComparator {
        BaselineComparator::new(
            format!("cmp-{claim_id}"),
            claim_id,
            format!("baseline-{claim_id}"),
            MetricFamily::Latency,
        )
    }

    fn healthy_claim(claim_id: &str) -> UpliftClaim {
        UpliftClaim::new(
            claim_id,
            format!("cmp-{claim_id}"),
            OptimizationLever::new(
                format!("lever-{claim_id}"),
                "graveyard-eq-001",
                "swap inner loop to SAT tile-skip",
            ),
            strong_accept_posterior(claim_id),
            ChecksumVerdict::Match,
            PercentileDeltas::improvement(0.10),
            RollbackPolicy::AutomaticOnRegression,
        )
    }

    fn green_request() -> ReverseRoundRequest {
        let mut registry = ComparatorRegistry::new();
        registry.register(comparator("claim-a")).unwrap();
        let window = DecisionWindow::new("win-1", vec!["claim-a".to_string()]);
        ReverseRoundRequest::new(registry, vec![window], vec![healthy_claim("claim-a")])
    }

    fn evaluate(request: &ReverseRoundRequest) -> ReverseRoundGovernanceReport {
        ReverseRoundGate::default().evaluate(request)
    }

    fn has_code(report: &ReverseRoundGovernanceReport, code: ReverseRoundFindingCode) -> bool {
        report.findings.iter().any(|f| f.code == code)
    }

    // ── Execution spec ──────────────────────────────────────────────────────

    #[test]
    fn execution_spec_has_six_ordered_stages() {
        let spec = reverse_round_execution_spec();
        assert_eq!(spec.stages.len(), 6);
        assert_eq!(spec.schema_version, REVERSE_ROUND_GOVERNANCE_SCHEMA_VERSION);
        for (idx, stage) in spec.stages.iter().enumerate() {
            assert_eq!(stage.order_index, idx);
        }
        assert_eq!(spec.stages[0].stage, ReverseRoundStage::Baseline);
        assert_eq!(spec.stages[5].stage, ReverseRoundStage::Decision);
    }

    #[test]
    fn execution_spec_stage_order_matches_canonical() {
        let spec = reverse_round_execution_spec();
        let order: Vec<ReverseRoundStage> = spec.stages.iter().map(|s| s.stage).collect();
        assert_eq!(order, ReverseRoundStage::ORDER.to_vec());
    }

    #[test]
    fn stage_gate_clauses_are_stable() {
        assert_eq!(
            ReverseRoundStage::SingleLever.gate_clause(),
            "RRG-LEVER-001"
        );
        assert_eq!(
            ReverseRoundStage::IsomorphismProof.gate_clause(),
            "RRG-ISO-001"
        );
        assert_eq!(ReverseRoundStage::Reprofile.gate_clause(), "RRG-PERF-001");
    }

    // ── Metric family wiring ─────────────────────────────────────────────────

    #[test]
    fn metric_family_covers_performance_metrics() {
        assert_eq!(MetricFamily::Latency.covered_metrics().len(), 3);
        assert!(
            MetricFamily::Throughput
                .covered_metrics()
                .contains(&PerformanceMetricKind::ThroughputOpsPerSec)
        );
        assert!(
            MetricFamily::FrameTiming
                .covered_metrics()
                .contains(&PerformanceMetricKind::DroppedFrameRatio)
        );
    }

    // ── Comparator registry ──────────────────────────────────────────────────

    #[test]
    fn registry_keyed_by_claim_id() {
        let mut registry = ComparatorRegistry::new();
        registry.register(comparator("claim-a")).unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.comparator_id_for("claim-a"), Some("cmp-claim-a"));
        assert_eq!(registry.comparator_id_for("missing"), None);
        assert_eq!(registry.claim_ids(), vec!["claim-a".to_string()]);
    }

    #[test]
    fn registry_rejects_duplicate_claim() {
        let mut registry = ComparatorRegistry::new();
        registry.register(comparator("claim-a")).unwrap();
        let err = registry.register(comparator("claim-a")).unwrap_err();
        assert_eq!(
            err,
            ComparatorRegistryError::DuplicateClaim {
                claim_id: "claim-a".to_string()
            }
        );
    }

    #[test]
    fn registry_rejects_empty_comparator_id() {
        let mut registry = ComparatorRegistry::new();
        let mut cmp = comparator("claim-a");
        cmp.comparator_id = String::new();
        let err = registry.register(cmp).unwrap_err();
        assert_eq!(
            err,
            ComparatorRegistryError::EmptyComparatorId {
                claim_id: "claim-a".to_string()
            }
        );
    }

    #[test]
    fn register_for_detects_key_mismatch() {
        let mut registry = ComparatorRegistry::new();
        let err = registry
            .register_for("claim-b", comparator("claim-a"))
            .unwrap_err();
        assert_eq!(
            err,
            ComparatorRegistryError::ClaimKeyMismatch {
                expected: "claim-b".to_string(),
                found: "claim-a".to_string(),
            }
        );
    }

    // ── Green path ───────────────────────────────────────────────────────────

    #[test]
    fn green_request_passes_gate() {
        let report = evaluate(&green_request());
        assert!(report.gate_passes, "findings: {:?}", report.findings);
        assert!(report.blocked_claim_ids.is_empty());
        assert_eq!(report.summary.total_claims, 1);
        assert_eq!(report.summary.promotable_claims, 1);
        assert_eq!(report.ledger.len(), 1);
        assert_eq!(report.decision_logs.len(), 1);
    }

    #[test]
    fn green_path_ledger_links_claim_to_evidence_and_rollback() {
        let report = evaluate(&green_request());
        let entry = report.ledger_entry("claim-a").expect("ledger entry");
        assert_eq!(entry.comparator_id, "cmp-claim-a");
        assert_eq!(entry.incumbent_baseline_id, "baseline-claim-a");
        assert_eq!(entry.lever_id, "lever-claim-a");
        assert_eq!(entry.equation_id, "graveyard-eq-001");
        assert_eq!(entry.expected_loss_action, MigrationDecision::AutoApprove);
        assert_eq!(entry.rollback_decision, RollbackDecision::NoRollbackNeeded);
        assert!(entry.promotable);
    }

    #[test]
    fn green_path_decision_log_has_all_ac3_fields() {
        let report = evaluate(&green_request());
        let log = report.decision_log("claim-a").expect("decision log");
        assert_eq!(log.claim_id, "claim-a");
        assert_eq!(log.comparator_id, "cmp-claim-a");
        assert_eq!(log.lever_id, "lever-claim-a");
        assert_eq!(log.equation_id, "graveyard-eq-001");
        assert!(log.bayes_factor_summary.aggregate_log_bayes_factor > 0.0);
        assert_eq!(log.bayes_factor_summary.present_channel_count, 5);
        assert!(log.p50_delta < 0.0);
        assert_eq!(log.isomorphism_verdict, "match");
        assert_eq!(log.rollback_decision, RollbackDecision::NoRollbackNeeded);
    }

    // ── AC1: comparator/baseline/scope references ────────────────────────────

    #[test]
    fn ac1_missing_comparator_reference_blocks() {
        let mut request = green_request();
        request.claims[0].declared_comparator_id = String::new();
        let report = evaluate(&request);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::MissingComparatorReference
        ));
        assert!(!report.gate_passes);
    }

    #[test]
    fn ac1_unregistered_comparator_blocks() {
        let registry = ComparatorRegistry::new(); // empty
        let request = ReverseRoundRequest::new(
            registry,
            vec![DecisionWindow::new("win-1", vec!["claim-a".to_string()])],
            vec![healthy_claim("claim-a")],
        );
        let report = evaluate(&request);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::UnregisteredComparator
        ));
        assert!(!report.gate_passes);
        assert_eq!(report.blocked_claim_ids, vec!["claim-a".to_string()]);
    }

    #[test]
    fn ac1_comparator_mismatch_blocks() {
        let mut request = green_request();
        request.claims[0].declared_comparator_id = "cmp-wrong".to_string();
        let report = evaluate(&request);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::ComparatorReferenceMismatch
        ));
        assert!(!report.gate_passes);
    }

    #[test]
    fn ac1_missing_incumbent_baseline_blocks() {
        let mut registry = ComparatorRegistry::new();
        let mut cmp = comparator("claim-a");
        cmp.incumbent_baseline_id = "   ".to_string();
        registry.register(cmp).unwrap();
        let request = ReverseRoundRequest::new(
            registry,
            vec![DecisionWindow::new("win-1", vec!["claim-a".to_string()])],
            vec![healthy_claim("claim-a")],
        );
        let report = evaluate(&request);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::MissingIncumbentBaseline
        ));
        assert!(!report.gate_passes);
    }

    #[test]
    fn ac1_missing_one_lever_scope_blocks() {
        let mut request = green_request();
        request.claims[0].lever.scope = String::new();
        let report = evaluate(&request);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::MissingOneLeverScope
        ));
        assert!(!report.gate_passes);
    }

    #[test]
    fn malformed_confidence_band_blocks() {
        let mut registry = ComparatorRegistry::new();
        let cmp = comparator("claim-a").with_confidence_band(ConfidenceBand::new(-1.0, 1.5));
        registry.register(cmp).unwrap();
        let request = ReverseRoundRequest::new(
            registry,
            vec![DecisionWindow::new("win-1", vec!["claim-a".to_string()])],
            vec![healthy_claim("claim-a")],
        );
        let report = evaluate(&request);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::MalformedConfidenceBand
        ));
    }

    // ── AC2: one-lever-per-window ────────────────────────────────────────────

    fn two_lever_request(
        override_artifact: Option<&str>,
        risk_waiver: Option<&str>,
    ) -> ReverseRoundRequest {
        let mut registry = ComparatorRegistry::new();
        registry.register(comparator("claim-a")).unwrap();
        registry.register(comparator("claim-b")).unwrap();
        let mut window =
            DecisionWindow::new("win-1", vec!["claim-a".to_string(), "claim-b".to_string()]);
        if let Some(a) = override_artifact {
            window = window.with_override_artifact(a);
        }
        if let Some(w) = risk_waiver {
            window = window.with_risk_waiver(w);
        }
        ReverseRoundRequest::new(
            registry,
            vec![window],
            vec![healthy_claim("claim-a"), healthy_claim("claim-b")],
        )
    }

    #[test]
    fn ac2_two_levers_without_override_blocks() {
        let report = evaluate(&two_lever_request(None, None));
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::MultiLeverWithoutOverride
        ));
        assert!(!report.gate_passes);
        assert_eq!(report.summary.multi_lever_windows, 1);
    }

    #[test]
    fn ac2_two_levers_override_without_waiver_blocks() {
        let report = evaluate(&two_lever_request(Some("override-1"), None));
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::MultiLeverWithoutRiskWaiver
        ));
        assert!(!report.gate_passes);
    }

    #[test]
    fn ac2_two_levers_override_plus_waiver_passes_with_advisory() {
        let report = evaluate(&two_lever_request(Some("override-1"), Some("waiver-1")));
        assert!(report.gate_passes, "findings: {:?}", report.findings);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::MultiLeverOverrideApplied
        ));
        // The override audit is advisory, not blocking.
        let advisory = report
            .findings
            .iter()
            .find(|f| f.code == ReverseRoundFindingCode::MultiLeverOverrideApplied)
            .unwrap();
        assert_eq!(advisory.severity, ReverseRoundSeverity::Warn);
    }

    #[test]
    fn ac2_single_disabled_lever_not_multi() {
        let mut request = two_lever_request(None, None);
        request.claims[1].lever.enabled = false; // only one enabled lever now
        let report = evaluate(&request);
        assert!(!has_code(
            &report,
            ReverseRoundFindingCode::MultiLeverWithoutOverride
        ));
        assert_eq!(report.summary.multi_lever_windows, 0);
        assert_eq!(report.summary.enabled_lever_count, 1);
    }

    #[test]
    fn unknown_claim_in_window_is_advisory() {
        let mut registry = ComparatorRegistry::new();
        registry.register(comparator("claim-a")).unwrap();
        let window = DecisionWindow::new("win-1", vec!["claim-a".to_string(), "ghost".to_string()]);
        let request =
            ReverseRoundRequest::new(registry, vec![window], vec![healthy_claim("claim-a")]);
        let report = evaluate(&request);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::UnknownClaimInWindow
        ));
        // The unknown-claim finding alone is advisory and must not block.
        assert!(report.gate_passes, "findings: {:?}", report.findings);
    }

    // ── Isomorphism ──────────────────────────────────────────────────────────

    #[test]
    fn isomorphism_mismatch_blocks_and_arms_rollback() {
        let mut request = green_request();
        request.claims[0].isomorphism_verdict = ChecksumVerdict::Mismatch;
        let report = evaluate(&request);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::IsomorphismViolation
        ));
        assert!(!report.gate_passes);
        let entry = report.ledger_entry("claim-a").unwrap();
        assert_eq!(entry.rollback_decision, RollbackDecision::RollbackTriggered);
    }

    #[test]
    fn normalized_match_is_advisory_by_default() {
        let mut request = green_request();
        request.claims[0].isomorphism_verdict = ChecksumVerdict::NormalizedMatch;
        let report = evaluate(&request);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::IsomorphismNormalizedMatch
        ));
        assert!(report.gate_passes, "findings: {:?}", report.findings);
    }

    #[test]
    fn normalized_match_blocks_when_configured() {
        let mut request = green_request();
        request.claims[0].isomorphism_verdict = ChecksumVerdict::NormalizedMatch;
        let gate = ReverseRoundGate::new(ReverseRoundConfig {
            normalized_match_blocks: true,
            record_override_audit: true,
        });
        let report = gate.evaluate(&request);
        assert!(!report.gate_passes);
    }

    // ── Performance budgets ──────────────────────────────────────────────────

    #[test]
    fn percentile_regression_beyond_budget_blocks() {
        let mut request = green_request();
        // Default thresholds: 2% / 5% / 10%. p99 of 0.5 (50%) breaches.
        request.claims[0].percentile_deltas = PercentileDeltas::new(0.0, 0.0, 0.5);
        let report = evaluate(&request);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::PercentileRegressionBeyondBudget
        ));
        assert!(!report.gate_passes);
    }

    #[test]
    fn within_budget_regression_does_not_block() {
        let mut request = green_request();
        // 1% / 4% / 9% all within the default 2% / 5% / 10% budgets.
        request.claims[0].percentile_deltas = PercentileDeltas::new(0.01, 0.04, 0.09);
        let report = evaluate(&request);
        assert!(!has_code(
            &report,
            ReverseRoundFindingCode::PercentileRegressionBeyondBudget
        ));
        assert!(report.gate_passes);
    }

    #[test]
    fn non_finite_delta_is_treated_as_breach() {
        let mut request = green_request();
        request.claims[0].percentile_deltas = PercentileDeltas::new(f64::NAN, 0.0, 0.0);
        let report = evaluate(&request);
        assert!(has_code(
            &report,
            ReverseRoundFindingCode::PercentileRegressionBeyondBudget
        ));
    }

    // ── Rollback policy mapping ──────────────────────────────────────────────

    #[test]
    fn rollback_policy_maps_to_decision_on_regression() {
        let cases = [
            (
                RollbackPolicy::AutomaticOnRegression,
                RollbackDecision::RollbackTriggered,
            ),
            (
                RollbackPolicy::StagedCanary,
                RollbackDecision::RollbackArmed,
            ),
            (
                RollbackPolicy::GuardedManual,
                RollbackDecision::RollbackBlockedNeedsAuthority,
            ),
            (
                RollbackPolicy::HoldNoRollback,
                RollbackDecision::RollbackSuppressed,
            ),
        ];
        for (policy, expected) in cases {
            let mut request = green_request();
            request.claims[0].rollback_policy = policy;
            request.claims[0].isomorphism_verdict = ChecksumVerdict::Mismatch; // force regression
            let report = evaluate(&request);
            assert_eq!(
                report.ledger_entry("claim-a").unwrap().rollback_decision,
                expected,
                "policy {policy:?}"
            );
        }
    }

    #[test]
    fn reject_posterior_arms_rollback_even_without_perf_regression() {
        let mut request = green_request();
        request.claims[0].posterior = reject_posterior("claim-a");
        request.claims[0].rollback_policy = RollbackPolicy::StagedCanary;
        let report = evaluate(&request);
        let entry = report.ledger_entry("claim-a").unwrap();
        assert!(matches!(
            entry.expected_loss_action,
            MigrationDecision::Reject | MigrationDecision::HardReject
        ));
        assert_eq!(entry.rollback_decision, RollbackDecision::RollbackArmed);
    }

    // ── Posterior summaries ──────────────────────────────────────────────────

    #[test]
    fn posterior_evidence_summary_projects_record() {
        let record = strong_accept_posterior("claim-a");
        let summary = PosteriorEvidenceSummary::from_record(&record);
        assert_eq!(summary.claim_id, "claim-a");
        assert_eq!(summary.present_channel_count, 5);
        assert!(summary.top_channel.is_some());
        assert_eq!(summary.expected_loss_action, record.decision);
    }

    #[test]
    fn bayes_factor_summary_handles_empty_evidence() {
        let engine = PosteriorEngine::default();
        let record = engine.infer("claim-empty", &[]);
        let summary = BayesFactorSummary::from_record(&record);
        assert_eq!(summary.top_channel, "n/a");
        assert_eq!(summary.present_channel_count, 0);
    }

    // ── Determinism ──────────────────────────────────────────────────────────

    #[test]
    fn evaluation_is_deterministic() {
        let request = green_request();
        let a = evaluate(&request);
        let b = evaluate(&request);
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.exported_json_stats.sha256, b.exported_json_stats.sha256);
    }

    #[test]
    fn claim_order_does_not_affect_report() {
        let req_ab = two_lever_request(Some("o"), Some("w"));
        let mut req_ba = two_lever_request(Some("o"), Some("w"));
        req_ba.claims.reverse();
        let a = evaluate(&req_ab);
        let b = evaluate(&req_ba);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.report_id, b.report_id);
    }

    #[test]
    fn findings_are_sorted_deterministically() {
        let report = evaluate(&two_lever_request(None, None));
        let mut sorted = report.findings.clone();
        sorted.sort_by(|a, b| {
            a.claim_id
                .cmp(&b.claim_id)
                .then_with(|| a.window_id.cmp(&b.window_id))
                .then_with(|| a.clause_id.cmp(&b.clause_id))
                .then_with(|| a.code.as_str().cmp(b.code.as_str()))
                .then_with(|| a.detail.cmp(&b.detail))
        });
        assert_eq!(report.findings, sorted);
    }

    // ── Finding code metadata ────────────────────────────────────────────────

    #[test]
    fn finding_codes_have_stable_clause_ids() {
        assert_eq!(
            ReverseRoundFindingCode::MissingIncumbentBaseline.clause_id(),
            "RRG-REF-001"
        );
        assert_eq!(
            ReverseRoundFindingCode::MultiLeverWithoutOverride.clause_id(),
            "RRG-LEVER-002"
        );
        assert_eq!(
            ReverseRoundFindingCode::IsomorphismViolation.clause_id(),
            "RRG-ISO-001"
        );
    }

    #[test]
    fn every_finding_carries_clause_and_dependency_hint() {
        let report = evaluate(&two_lever_request(None, None));
        assert!(!report.findings.is_empty());
        for finding in &report.findings {
            assert!(!finding.clause_id.is_empty());
            assert!(!finding.dependency_hint.is_empty());
            assert!(!finding.remediation.is_empty());
            assert_eq!(finding.clause_id, finding.code.clause_id());
        }
    }

    // ── Serde roundtrips ─────────────────────────────────────────────────────

    // NOTE: the posterior records carry extreme near-1.0 probabilities that can
    // shift by 1 ULP under serde_json's default best-effort float parsing, so the
    // round-trip tests assert structural and stable-string identity rather than
    // bit-exact f64 equality on the embedded posteriors.

    #[test]
    fn report_serde_roundtrips() {
        let report = evaluate(&green_request());
        let json = serde_json::to_string(&report).unwrap();
        let back: ReverseRoundGovernanceReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report.schema_version, back.schema_version);
        assert_eq!(report.report_id, back.report_id);
        assert_eq!(report.evidence_checksum, back.evidence_checksum);
        assert_eq!(report.execution_spec, back.execution_spec);
        assert_eq!(report.findings, back.findings);
        assert_eq!(report.summary, back.summary);
        assert_eq!(report.gate_passes, back.gate_passes);
        assert_eq!(report.blocked_claim_ids, back.blocked_claim_ids);
        assert_eq!(report.ledger.len(), back.ledger.len());
        assert_eq!(report.decision_logs.len(), back.decision_logs.len());
        assert_eq!(report.ledger[0].claim_id, back.ledger[0].claim_id);
        assert_eq!(
            report.ledger[0].rollback_decision,
            back.ledger[0].rollback_decision
        );
    }

    #[test]
    fn request_serde_roundtrips() {
        let request = green_request();
        let json = serde_json::to_string(&request).unwrap();
        let back: ReverseRoundRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request.registry, back.registry);
        assert_eq!(request.windows, back.windows);
        assert_eq!(request.claims.len(), back.claims.len());
        assert_eq!(request.claims[0].claim_id, back.claims[0].claim_id);
        assert_eq!(
            request.claims[0].declared_comparator_id,
            back.claims[0].declared_comparator_id
        );
        assert_eq!(request.claims[0].lever, back.claims[0].lever);
        assert_eq!(
            request.claims[0].isomorphism_verdict,
            back.claims[0].isomorphism_verdict
        );
        assert_eq!(
            request.claims[0].rollback_policy,
            back.claims[0].rollback_policy
        );
    }

    #[test]
    fn exported_json_stats_checksum_matches_content() {
        let report = evaluate(&green_request());
        let recomputed = sha256_hex(report.exported_json_stats.content.as_bytes());
        assert_eq!(report.exported_json_stats.sha256, recomputed);
    }

    // ── Summary accounting ───────────────────────────────────────────────────

    #[test]
    fn summary_counts_rollback_and_isomorphism_buckets() {
        let mut request = two_lever_request(Some("o"), Some("w"));
        request.claims[0].isomorphism_verdict = ChecksumVerdict::Mismatch;
        request.claims[1].isomorphism_verdict = ChecksumVerdict::NormalizedMatch;
        let report = evaluate(&request);
        assert_eq!(report.summary.total_claims, 2);
        assert_eq!(report.summary.isomorphism_mismatch, 1);
        assert_eq!(report.summary.isomorphism_normalized_match, 1);
        // claim-a (mismatch) triggers rollback; claim-b (normalized match) does not.
        assert_eq!(report.summary.rollback_triggered, 1);
    }

    #[test]
    fn blocked_claim_marked_not_promotable() {
        let mut request = green_request();
        request.claims[0].isomorphism_verdict = ChecksumVerdict::Mismatch;
        let report = evaluate(&request);
        let entry = report.ledger_entry("claim-a").unwrap();
        assert!(!entry.promotable);
        assert_eq!(report.summary.promotable_claims, 0);
    }

    #[test]
    fn replay_command_references_report_id() {
        let report = evaluate(&green_request());
        assert!(report.replay_command.contains(&report.report_id));
        assert!(report.replay_command.contains("reverse_round_governance"));
    }

    #[test]
    fn report_embeds_execution_spec() {
        let report = evaluate(&green_request());
        assert_eq!(report.execution_spec.stages.len(), 6);
    }
}

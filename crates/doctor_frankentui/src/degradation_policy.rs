//! Robust missing-evidence graceful-degradation policy compiler (bd-3bxhj.10.41).
//!
//! Real OpenTUI→FrankenTUI migration runs routinely face partial telemetry,
//! delayed traces, and contradictory probes (MNAR-style missingness). Brittle
//! "fail hard or trust blindly" handling either blocks safe migrations or ships
//! unsafe ones. This module compiles **explicit degradation contracts** so a
//! decision stays *safe*, *interpretable*, and *recoverable* under incomplete
//! evidence:
//!
//! 1. **Degradation DSL** — every channel carries a [`SignalStatus`]
//!    (`Present` / `Missing{class}` / `Delayed` / `Contradictory`). Missingness
//!    is typed with [`MissingnessClass`] proxies (MCAR/MAR/MNAR); MNAR is the most
//!    hazardous because the missingness depends on the unobserved value and cannot
//!    be imputed safely.
//! 2. **Robust inference fallback** — each missing channel is imputed by a
//!    deterministic, provenance-tagged [`ImputationFamily`] (carry-forward for
//!    MCAR, conservative-prior for MAR, bounded-worst-case for MNAR). Degraded
//!    evidence inflates uncertainty ([`DegradationDecision::inflation_factor`]).
//! 3. **State machine** — [`DegradationStateMachine`] gives degraded mode explicit
//!    entry/exit/cooldown semantics with *hysteresis* (`exit_threshold >
//!    enter_threshold`) so the controller never oscillates.
//! 4. **Adversarial resilience** — contradictory evidence is arbitrated toward the
//!    more conservative reading, and dissent above an anomaly threshold engages a
//!    hard clamp (force [`MigrationDecision::ConservativeFallback`] + operator
//!    escalation).
//! 5. **Recovery proofs** — promotion back to full confidence requires a
//!    [`RecoveryGuard`] whose measurable criteria are *all* satisfied; there is no
//!    implicit auto-promotion.
//!
//! ## Safety by construction (AC1)
//!
//! For *any* subset of channels missing / delayed / contradictory, the compiled
//! `final_action` is always at least as conservative as the action *required* by
//! the computed residual risk ([`required_action_for_risk`]). That property holds
//! by construction (the ceiling is the more-conservative of the tier ceiling and
//! the risk-required action), and the gate re-derives it independently so it is
//! falsifiable rather than tautological.
//!
//! The ledger is **float-free** (every numeric term is a fixed-decimal string via
//! [`fmt6`]), so it derives [`Eq`] and replays byte-identically (AC4). Numerical
//! finiteness is checked on the *raw* `f64`s before rendering (AC1), because
//! `fmt6` maps a NaN to `"0.000000"` and a post-render string check alone would be
//! tautological.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::action_str;
use crate::semantic_contract::MigrationDecision;

/// Schema version for the degradation-policy artifacts.
pub const DEGRADATION_POLICY_SCHEMA_VERSION: &str = "degradation-policy-v1";

/// Numeric epsilon for guarded ratios and comparisons.
const EPS: f64 = 1e-9;

/// Upper clamp on the uncertainty-inflation factor (keeps the ledger bounded).
const MAX_INFLATION: f64 = 4.0;

// ── Hashing / formatting helpers ─────────────────────────────────────────────

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

/// Deterministic fixed-decimal rendering. Non-finite and `-0.0` normalize to a
/// stable string so the ledger derives `Eq` and replays byte-identically.
fn fmt6(x: f64) -> String {
    if !x.is_finite() {
        return "0.000000".to_string();
    }
    let rendered = format!("{x:.6}");
    if rendered == "-0.000000" {
        "0.000000".to_string()
    } else {
        rendered
    }
}

/// A ratio guarded against a zero (or non-finite) denominator.
fn safe_div(num: f64, den: f64) -> f64 {
    if den.abs() < EPS || !den.is_finite() || !num.is_finite() {
        0.0
    } else {
        num / den
    }
}

fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

// ── Action conservatism ──────────────────────────────────────────────────────

/// Conservatism rank over the canonical action space: a *lower* rank is *more*
/// conservative. Mirrors the ordering used by the decision-loss policy engine so
/// the two modules agree on "which action is safer".
fn conservatism_rank(action: MigrationDecision) -> u8 {
    match action {
        MigrationDecision::ConservativeFallback => 0,
        MigrationDecision::Rollback => 1,
        MigrationDecision::HardReject => 2,
        MigrationDecision::Reject => 3,
        MigrationDecision::HumanReview => 4,
        MigrationDecision::AutoApprove => 5,
    }
}

/// The more conservative (lower-rank) of two actions; ties keep `a`.
fn more_conservative(a: MigrationDecision, b: MigrationDecision) -> MigrationDecision {
    if conservatism_rank(b) < conservatism_rank(a) {
        b
    } else {
        a
    }
}

/// Whether `action` is at least as conservative as `floor` (rank ≤ floor's rank).
fn at_least_as_conservative(action: MigrationDecision, floor: MigrationDecision) -> bool {
    conservatism_rank(action) <= conservatism_rank(floor)
}

/// The least-conservative action whose residual risk stays within tolerance.
/// This is the binding max-risk envelope: a higher residual risk *requires* a more
/// conservative action. Monotone in `risk`.
#[must_use]
pub fn required_action_for_risk(risk: f64) -> MigrationDecision {
    let r = clamp01(risk);
    if r <= 0.05 + EPS {
        MigrationDecision::AutoApprove
    } else if r <= 0.15 + EPS {
        MigrationDecision::HumanReview
    } else if r <= 0.35 + EPS {
        MigrationDecision::Reject
    } else if r <= 0.60 + EPS {
        MigrationDecision::HardReject
    } else {
        MigrationDecision::ConservativeFallback
    }
}

// ── Degradation DSL ──────────────────────────────────────────────────────────

/// Missingness-class proxies (Rubin's taxonomy), ordered by hazard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingnessClass {
    /// Missing completely at random — least hazardous; carry-forward is safe.
    Mcar,
    /// Missing at random (conditional on observed covariates) — moderate hazard.
    Mar,
    /// Missing not at random — most hazardous; the missingness depends on the
    /// unobserved value, so only a bounded-worst-case imputation is defensible.
    Mnar,
}

impl MissingnessClass {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcar => "mcar",
            Self::Mar => "mar",
            Self::Mnar => "mnar",
        }
    }

    /// Per-class hazard weight feeding the inflation factor.
    fn hazard(self) -> f64 {
        match self {
            Self::Mcar => 0.05,
            Self::Mar => 0.10,
            Self::Mnar => 0.20,
        }
    }

    /// The deterministic imputation family this missingness class mandates.
    fn imputation_family(self) -> ImputationFamily {
        match self {
            Self::Mcar => ImputationFamily::CarryForward,
            Self::Mar => ImputationFamily::ConservativePrior,
            Self::Mnar => ImputationFamily::BoundedWorstCase,
        }
    }
}

/// The observation status of one evidence channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SignalStatus {
    /// Observed with a quality score in `[0, 1]` (1 = pristine).
    Present { quality: f64 },
    /// Absent, tagged with its missingness class.
    Missing { class: MissingnessClass },
    /// Observed but stale; `staleness` in `[0, 1]` (1 = fully stale).
    Delayed { staleness: f64 },
    /// Observed but in conflict with peers; `dissent` in `[0, 1]`.
    Contradictory { dissent: f64 },
}

/// One evidence channel feeding a degradation decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSignal {
    /// Stable channel id (e.g. `widget_coverage`, `runtime_event`).
    pub channel_id: String,
    /// Relative importance weight (non-negative).
    pub weight: f64,
    /// The channel's observation status.
    pub status: SignalStatus,
    /// Whether this channel is load-bearing for safety. A critical channel that is
    /// MNAR-missing or contradictory cannot be safely recovered from.
    pub critical: bool,
}

impl EvidenceSignal {
    /// Construct a channel.
    #[must_use]
    pub fn new(channel_id: impl Into<String>, weight: f64, status: SignalStatus) -> Self {
        Self {
            channel_id: channel_id.into(),
            weight: weight.max(0.0),
            status,
            critical: false,
        }
    }

    /// Mark this channel as safety-critical.
    #[must_use]
    pub fn critical(mut self) -> Self {
        self.critical = true;
        self
    }

    /// The effective, usable quality of this channel in `[0, 1]`.
    fn effective_quality(&self) -> f64 {
        match &self.status {
            SignalStatus::Present { quality } => clamp01(*quality),
            // A missing channel falls back to its imputation's quality (MCAR
            // carry-forward 0.5, MAR conservative-prior 0.4, MNAR bounded-worst-case
            // 0.0). The class-appropriate imputation, not a blanket zero, is what a
            // graceful degradation contract trusts — and it still drags quality down
            // and inflates uncertainty (the imputation is provenance-tagged below).
            SignalStatus::Missing { class } => class.imputation_family().imputed_quality(),
            // Stale evidence is partially usable; halve it and decay with staleness.
            SignalStatus::Delayed { staleness } => clamp01(1.0 - clamp01(*staleness)) * 0.5,
            // Contradiction erodes usable quality proportionally to the dissent.
            SignalStatus::Contradictory { dissent } => clamp01(1.0 - clamp01(*dissent)),
        }
    }
}

// ── Imputation ───────────────────────────────────────────────────────────────

/// Deterministic imputation families with provenance, one per missingness class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImputationFamily {
    /// Carry the last-known value forward (defensible only under MCAR).
    CarryForward,
    /// Fall back to a conservative prior (MAR).
    ConservativePrior,
    /// Assume the bounded worst case (MNAR; cannot trust any point estimate).
    BoundedWorstCase,
}

impl ImputationFamily {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CarryForward => "carry_forward",
            Self::ConservativePrior => "conservative_prior",
            Self::BoundedWorstCase => "bounded_worst_case",
        }
    }

    /// The deterministic imputed quality this family assigns to a missing channel.
    fn imputed_quality(self) -> f64 {
        match self {
            Self::CarryForward => 0.50,
            Self::ConservativePrior => 0.40,
            Self::BoundedWorstCase => 0.0,
        }
    }

    /// Provenance string recorded on each imputation.
    fn provenance(self) -> &'static str {
        match self {
            Self::CarryForward => "carry_forward_last_known",
            Self::ConservativePrior => "conservative_prior_default",
            Self::BoundedWorstCase => "bounded_worst_case_assumption",
        }
    }
}

/// A provenance-tagged imputation for one missing channel (float-free).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImputationRecord {
    /// The imputed channel.
    pub channel_id: String,
    /// The missingness class that selected the family.
    pub missingness: MissingnessClass,
    /// The imputation family applied.
    pub family: ImputationFamily,
    /// The deterministic imputed quality (fixed-decimal).
    pub imputed_quality: String,
    /// Human-readable provenance tag.
    pub provenance: String,
}

// ── Evidence tiers ───────────────────────────────────────────────────────────

/// Evidence-quality tiers, from pristine to unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTier {
    /// Full evidence — no degradation.
    Full,
    /// Partial evidence — minor gaps.
    Partial,
    /// Degraded evidence — substantial gaps.
    Degraded,
    /// Critical — evidence is unusable on its own.
    Critical,
}

impl EvidenceTier {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Partial => "partial",
            Self::Degraded => "degraded",
            Self::Critical => "critical",
        }
    }
}

/// Per-tier degradation envelope: the action ceiling, the uncertainty inflation,
/// and the nominal residual-risk budget.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TierEnvelope {
    /// The most permissive action the tier allows (a ceiling on permissiveness).
    pub ceiling: MigrationDecision,
    /// The base uncertainty inflation factor (≥ 1) for the tier.
    pub inflation: f64,
    /// The nominal residual-risk budget for the tier (informational).
    pub max_residual_risk: f64,
}

/// Degradation-mode of the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradationMode {
    /// Normal, full-confidence operation.
    Normal,
    /// Degraded operation: clamped actions and inflated uncertainty.
    Degraded,
}

impl DegradationMode {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Degraded => "degraded",
        }
    }
}

/// Explicit, fieldless reason codes for a degraded decision (AC2). Fieldless so
/// the set is trivially ordered and deduplicated for a deterministic ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradationReason {
    /// An MCAR-missing channel was imputed (carry-forward).
    MissingMcar,
    /// An MAR-missing channel was imputed (conservative prior).
    MissingMar,
    /// An MNAR-missing channel was imputed (bounded worst case).
    MissingMnar,
    /// A stale/delayed channel was down-weighted.
    DelayedChannel,
    /// A contradictory channel was arbitrated conservatively.
    ContradictoryChannel,
    /// Aggregate evidence quality fell below the tier threshold.
    LowQuality,
    /// A safety-critical channel was MNAR-missing or contradictory.
    CriticalHazard,
    /// Dissent exceeded the anomaly threshold — hard clamp engaged.
    AnomalyClamp,
    /// The residual risk forced a more-conservative ceiling than the tier.
    RiskEnvelopeEscalation,
}

impl DegradationReason {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingMcar => "missing_mcar",
            Self::MissingMar => "missing_mar",
            Self::MissingMnar => "missing_mnar",
            Self::DelayedChannel => "delayed_channel",
            Self::ContradictoryChannel => "contradictory_channel",
            Self::LowQuality => "low_quality",
            Self::CriticalHazard => "critical_hazard",
            Self::AnomalyClamp => "anomaly_clamp",
            Self::RiskEnvelopeEscalation => "risk_envelope_escalation",
        }
    }
}

/// The measurable guards that gate recovery to [`DegradationMode::Normal`] (AC3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryGuard {
    /// Aggregate quality cleared the (hysteretic) exit threshold.
    pub quality_recovered: bool,
    /// The minimum degraded-mode dwell time elapsed.
    pub cooldown_elapsed: bool,
    /// No channel is currently contradictory.
    pub no_active_contradiction: bool,
    /// No safety-critical channel is currently MNAR-missing.
    pub no_critical_mnar: bool,
    /// All guards satisfied — promotion is permitted iff this is true.
    pub satisfied: bool,
}

impl RecoveryGuard {
    fn new(
        quality_recovered: bool,
        cooldown_elapsed: bool,
        no_active_contradiction: bool,
        no_critical_mnar: bool,
    ) -> Self {
        Self {
            quality_recovered,
            cooldown_elapsed,
            no_active_contradiction,
            no_critical_mnar,
            satisfied: quality_recovered
                && cooldown_elapsed
                && no_active_contradiction
                && no_critical_mnar,
        }
    }
}

// ── Policy configuration ─────────────────────────────────────────────────────

/// Tunable thresholds for the degradation policy. All defaults are conservative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DegradationConfig {
    /// Quality ≥ this is [`EvidenceTier::Full`].
    pub full_threshold: f64,
    /// Quality ≥ this (and < full) is [`EvidenceTier::Partial`].
    pub partial_threshold: f64,
    /// Quality ≥ this (and < partial) is [`EvidenceTier::Degraded`]; below is
    /// [`EvidenceTier::Critical`].
    pub degraded_threshold: f64,
    /// Dissent above this engages the anomaly hard-clamp.
    pub anomaly_threshold: f64,
    /// State machine: enter degraded mode when quality drops below this.
    pub enter_threshold: f64,
    /// State machine: only exit degraded mode when quality rises above this
    /// (`> enter_threshold` gives hysteresis).
    pub exit_threshold: f64,
    /// State machine: minimum cycles to dwell in degraded mode before exit.
    pub cooldown_cycles: u32,
}

impl Default for DegradationConfig {
    fn default() -> Self {
        Self {
            full_threshold: 0.85,
            partial_threshold: 0.60,
            degraded_threshold: 0.35,
            anomaly_threshold: 0.60,
            enter_threshold: 0.50,
            exit_threshold: 0.70,
            cooldown_cycles: 3,
        }
    }
}

impl DegradationConfig {
    fn tier_for(&self, quality: f64) -> EvidenceTier {
        let q = clamp01(quality);
        if q >= self.full_threshold - EPS {
            EvidenceTier::Full
        } else if q >= self.partial_threshold - EPS {
            EvidenceTier::Partial
        } else if q >= self.degraded_threshold - EPS {
            EvidenceTier::Degraded
        } else {
            EvidenceTier::Critical
        }
    }

    fn envelope_for(tier: EvidenceTier) -> TierEnvelope {
        match tier {
            EvidenceTier::Full => TierEnvelope {
                ceiling: MigrationDecision::AutoApprove,
                inflation: 1.00,
                max_residual_risk: 0.05,
            },
            EvidenceTier::Partial => TierEnvelope {
                ceiling: MigrationDecision::HumanReview,
                inflation: 1.25,
                max_residual_risk: 0.15,
            },
            EvidenceTier::Degraded => TierEnvelope {
                ceiling: MigrationDecision::Reject,
                inflation: 1.75,
                max_residual_risk: 0.35,
            },
            EvidenceTier::Critical => TierEnvelope {
                ceiling: MigrationDecision::ConservativeFallback,
                inflation: 2.50,
                max_residual_risk: 0.60,
            },
        }
    }
}

// ── The compiled decision (ledger entry) ─────────────────────────────────────

/// One compiled degradation decision. Float-free, so it derives `Eq` and replays
/// byte-identically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DegradationDecision {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Deterministic decision id.
    pub decision_id: String,
    /// Subject under decision (e.g. a widget/migration unit id).
    pub subject_id: String,
    /// The upstream-proposed action (before degradation clamping).
    pub proposed_action: MigrationDecision,
    /// The action ceiling imposed by the tier + risk envelope + clamps.
    pub ceiling_action: MigrationDecision,
    /// The emitted action: the more conservative of proposed and ceiling.
    pub final_action: MigrationDecision,
    /// The least-conservative action permitted by the residual risk (the binding
    /// max-risk envelope; recomputed by the gate).
    pub required_action: MigrationDecision,
    /// The evidence tier.
    pub evidence_tier: EvidenceTier,
    /// The operating mode for this decision.
    pub mode: DegradationMode,
    /// Aggregate evidence-quality score in `[0, 1]` (fixed-decimal).
    pub evidence_quality_score: String,
    /// Uncertainty inflation factor (≥ 1, fixed-decimal).
    pub inflation_factor: String,
    /// Uncertainty delta (`inflation_factor - 1`, fixed-decimal); > 0 when degraded.
    pub uncertainty_delta: String,
    /// Residual risk after degradation (fixed-decimal).
    pub residual_risk: String,
    /// The tier's nominal residual-risk budget (fixed-decimal).
    pub max_risk_envelope: String,
    /// Whether a human operator must review this decision.
    pub operator_review_required: bool,
    /// Whether every raw f64 was finite before rendering (AC1; pre-`fmt6`).
    pub numerically_finite: bool,
    /// Sorted, deduplicated reason codes (AC2).
    pub degradation_reasons: Vec<DegradationReason>,
    /// Provenance-tagged imputations, sorted by channel id.
    pub imputations: Vec<ImputationRecord>,
    /// The recovery guard snapshot for this decision (informational in one-shot).
    pub recovery_guard: RecoveryGuard,
    /// Whether the row's flags are consistent with their recorded arithmetic.
    pub clause_consistent: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command.
    pub reproduction_command: String,
}

fn decision_has_required_fields(d: &DegradationDecision) -> bool {
    !d.schema_version.is_empty()
        && !d.run_id.is_empty()
        && !d.decision_id.is_empty()
        && !d.subject_id.is_empty()
        && !d.evidence_quality_score.is_empty()
        && !d.inflation_factor.is_empty()
        && !d.uncertainty_delta.is_empty()
        && !d.residual_risk.is_empty()
        && !d.max_risk_envelope.is_empty()
        && !d.detail.is_empty()
        && !d.reproduction_command.is_empty()
        && d.imputations.iter().all(|i| {
            !i.channel_id.is_empty() && !i.imputed_quality.is_empty() && !i.provenance.is_empty()
        })
}

// ── The compiler ─────────────────────────────────────────────────────────────

/// The deterministic graceful-degradation policy compiler.
#[derive(Debug, Clone, Default)]
pub struct DegradationPolicy {
    config: DegradationConfig,
}

/// Intermediate evidence assessment, shared by the compiler and the state machine.
struct EvidenceAssessment {
    quality: f64,
    max_dissent: f64,
    has_contradiction: bool,
    has_critical_mnar: bool,
    missing_mcar: u32,
    missing_mar: u32,
    missing_mnar: u32,
    delayed: u32,
    contradictory: u32,
    imputations: Vec<ImputationRecord>,
}

impl DegradationPolicy {
    /// Construct a policy with explicit config.
    #[must_use]
    pub fn new(config: DegradationConfig) -> Self {
        Self { config }
    }

    /// The active configuration.
    #[must_use]
    pub fn config(&self) -> &DegradationConfig {
        &self.config
    }

    /// Assess an evidence bundle into aggregate quality, hazards, and imputations.
    fn assess(&self, signals: &[EvidenceSignal]) -> EvidenceAssessment {
        let mut weighted_quality = 0.0;
        let mut total_weight = 0.0;
        let mut max_dissent = 0.0_f64;
        let (mut missing_mcar, mut missing_mar, mut missing_mnar) = (0, 0, 0);
        let (mut delayed, mut contradictory) = (0, 0);
        let mut has_contradiction = false;
        let mut has_critical_mnar = false;
        let mut imputations = Vec::new();

        for signal in signals {
            let w = signal.weight.max(0.0);
            weighted_quality += w * signal.effective_quality();
            total_weight += w;
            match &signal.status {
                SignalStatus::Present { .. } => {}
                SignalStatus::Missing { class } => {
                    match class {
                        MissingnessClass::Mcar => missing_mcar += 1,
                        MissingnessClass::Mar => missing_mar += 1,
                        MissingnessClass::Mnar => missing_mnar += 1,
                    }
                    if *class == MissingnessClass::Mnar && signal.critical {
                        has_critical_mnar = true;
                    }
                    let family = class.imputation_family();
                    imputations.push(ImputationRecord {
                        channel_id: signal.channel_id.clone(),
                        missingness: *class,
                        family,
                        imputed_quality: fmt6(family.imputed_quality()),
                        provenance: family.provenance().to_string(),
                    });
                }
                SignalStatus::Delayed { .. } => delayed += 1,
                SignalStatus::Contradictory { dissent } => {
                    contradictory += 1;
                    has_contradiction = true;
                    max_dissent = max_dissent.max(clamp01(*dissent));
                }
            }
        }

        // An empty (or zero-weight) bundle is maximally uncertain, not a divide-by-
        // zero: quality 0 ⇒ Critical tier ⇒ conservative fallback.
        let quality = if total_weight <= EPS {
            0.0
        } else {
            clamp01(safe_div(weighted_quality, total_weight))
        };
        imputations.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));

        EvidenceAssessment {
            quality,
            max_dissent,
            has_contradiction,
            has_critical_mnar,
            missing_mcar,
            missing_mar,
            missing_mnar,
            delayed,
            contradictory,
            imputations,
        }
    }

    /// Compile a single degradation decision for `proposed_action` under `signals`.
    #[must_use]
    pub fn compile(
        &self,
        run_id: &str,
        subject_id: &str,
        proposed_action: MigrationDecision,
        signals: &[EvidenceSignal],
    ) -> DegradationDecision {
        let a = self.assess(signals);
        let tier = self.config.tier_for(a.quality);
        let envelope = DegradationConfig::envelope_for(tier);

        // Uncertainty inflation: start from the tier base and add per-hazard
        // increments, then clamp. Strictly ≥ the tier base, so degraded tiers
        // always carry uncertainty_delta > 0 (AC2).
        let mut inflation = envelope.inflation;
        inflation += 0.20 * f64::from(a.missing_mnar);
        inflation += 0.10 * f64::from(a.missing_mar);
        inflation += 0.05 * f64::from(a.missing_mcar);
        inflation += 0.10 * f64::from(a.contradictory);
        inflation += 0.05 * f64::from(a.delayed);
        // MNAR hazard weight (per its class hazard) for criticality.
        inflation += MissingnessClass::Mnar.hazard() * f64::from(u32::from(a.has_critical_mnar));
        let inflation = inflation.clamp(1.0, MAX_INFLATION);

        // Residual risk: the unexplained share of evidence, amplified by the
        // inflation, bounded to [0, 1].
        let residual_risk = clamp01((1.0 - a.quality) * inflation * 0.5);

        // The binding max-risk envelope: the residual risk *requires* at least this
        // action. The ceiling is the more conservative of the tier ceiling and the
        // risk-required action — so the final action is safe by construction (AC1).
        let required_action = required_action_for_risk(residual_risk);
        let mut ceiling = more_conservative(envelope.ceiling, required_action);
        let risk_escalated = !at_least_as_conservative(envelope.ceiling, required_action);

        // Adversarial resilience: dissent above the anomaly threshold or a critical
        // MNAR hazard forces a hard clamp to ConservativeFallback + operator review.
        let anomaly = a.max_dissent > self.config.anomaly_threshold + EPS;
        let hard_clamp = anomaly || a.has_critical_mnar;
        if hard_clamp {
            ceiling = more_conservative(ceiling, MigrationDecision::ConservativeFallback);
        }
        let operator_review_required = hard_clamp;

        let final_action = more_conservative(proposed_action, ceiling);
        // Degraded mode tracks *evidence incompleteness*, not the risk-based clamp:
        // full, pristine evidence whose residual still warrants a more-conservative
        // action (a pure risk-envelope clamp) stays Normal — the action is clamped
        // but uncertainty is not inflated, so it must not be flagged "degraded".
        // (Folding risk escalation into the mode would let a Full/all-present row be
        // Degraded with a zero uncertainty delta, breaking the AC2 clause.)
        let mode = if matches!(tier, EvidenceTier::Full) && !hard_clamp {
            DegradationMode::Normal
        } else {
            DegradationMode::Degraded
        };

        // Reason codes (AC2): a deterministic, deduplicated set.
        let mut reasons: BTreeSet<DegradationReason> = BTreeSet::new();
        if a.missing_mcar > 0 {
            reasons.insert(DegradationReason::MissingMcar);
        }
        if a.missing_mar > 0 {
            reasons.insert(DegradationReason::MissingMar);
        }
        if a.missing_mnar > 0 {
            reasons.insert(DegradationReason::MissingMnar);
        }
        if a.delayed > 0 {
            reasons.insert(DegradationReason::DelayedChannel);
        }
        if a.contradictory > 0 {
            reasons.insert(DegradationReason::ContradictoryChannel);
        }
        if tier != EvidenceTier::Full {
            reasons.insert(DegradationReason::LowQuality);
        }
        if a.has_critical_mnar {
            reasons.insert(DegradationReason::CriticalHazard);
        }
        if anomaly {
            reasons.insert(DegradationReason::AnomalyClamp);
        }
        if risk_escalated {
            reasons.insert(DegradationReason::RiskEnvelopeEscalation);
        }
        let degradation_reasons: Vec<DegradationReason> = reasons.into_iter().collect();

        // Recovery guard (informational in one-shot: cooldown does not apply when
        // there is no degraded-mode dwell to measure, so it reports elapsed).
        let recovery_guard = RecoveryGuard::new(
            a.quality > self.config.exit_threshold + EPS,
            true,
            !a.has_contradiction,
            !a.has_critical_mnar,
        );

        // AC1 numerical stability: check the raw f64s before fmt6 can mask a NaN.
        let numerically_finite = [
            a.quality,
            a.max_dissent,
            inflation,
            residual_risk,
            envelope.max_residual_risk,
        ]
        .iter()
        .all(|x| x.is_finite());

        // Clause consistency (falsifiable, recomputed from the decision's own data):
        //  - the final action is at least as conservative as the required action;
        //  - the final action is at least as conservative as the ceiling;
        //  - a degraded mode carries at least one reason and a positive delta;
        //  - a hard clamp ⇒ final is ConservativeFallback and operator review is on.
        let uncertainty_delta = inflation - 1.0;
        let final_safe = at_least_as_conservative(final_action, required_action)
            && at_least_as_conservative(final_action, ceiling);
        let degraded_explained = matches!(mode, DegradationMode::Normal)
            || (!degradation_reasons.is_empty() && uncertainty_delta > EPS);
        let clamp_consistent = !hard_clamp
            || (final_action == MigrationDecision::ConservativeFallback
                && operator_review_required);
        let clause_consistent = final_safe && degraded_explained && clamp_consistent;

        let decision_id = short_hash(&stable_hash(&DecisionId {
            run_id,
            subject_id,
            proposed: action_str(proposed_action),
            quality: fmt6(a.quality),
            tier: tier.as_str(),
        }));

        let detail = format!(
            "{} tier={} q={:.4} infl={:.4} risk={:.4} req={} ceil={} -> {}{}",
            subject_id,
            tier.as_str(),
            a.quality,
            inflation,
            residual_risk,
            action_str(required_action),
            action_str(ceiling),
            action_str(final_action),
            if operator_review_required {
                " [operator-review]"
            } else {
                ""
            },
        );

        DegradationDecision {
            schema_version: DEGRADATION_POLICY_SCHEMA_VERSION.to_string(),
            run_id: run_id.to_string(),
            decision_id,
            subject_id: subject_id.to_string(),
            proposed_action,
            ceiling_action: ceiling,
            final_action,
            required_action,
            evidence_tier: tier,
            mode,
            evidence_quality_score: fmt6(a.quality),
            inflation_factor: fmt6(inflation),
            uncertainty_delta: fmt6(uncertainty_delta),
            residual_risk: fmt6(residual_risk),
            max_risk_envelope: fmt6(envelope.max_residual_risk),
            operator_review_required,
            numerically_finite,
            degradation_reasons,
            imputations: a.imputations,
            recovery_guard,
            clause_consistent,
            detail,
            reproduction_command: format!(
                "cargo test -p doctor_frankentui --lib degradation_policy # subject {subject_id}"
            ),
        }
    }

    /// Run the degraded-mode state machine over a sequence of per-cycle evidence
    /// bundles, returning the mode transitions. Hysteresis (`exit > enter`) and a
    /// cooldown dwell prevent oscillation; promotion to Normal requires a satisfied
    /// [`RecoveryGuard`] (AC3 — no implicit auto-promotion).
    #[must_use]
    pub fn simulate(&self, run_id: &str, cycles: &[Vec<EvidenceSignal>]) -> Vec<ModeTransition> {
        let mut mode = DegradationMode::Normal;
        let mut cycles_in_degraded: u32 = 0;
        let mut transitions = Vec::with_capacity(cycles.len());

        for (i, signals) in cycles.iter().enumerate() {
            let a = self.assess(signals);
            let from = mode;

            // A hard hazard forces (or holds) degraded mode regardless of quality.
            let hard_hazard =
                a.has_critical_mnar || a.max_dissent > self.config.anomaly_threshold + EPS;

            let guard = match mode {
                DegradationMode::Normal => {
                    if a.quality < self.config.enter_threshold - EPS || hard_hazard {
                        mode = DegradationMode::Degraded;
                        cycles_in_degraded = 1;
                    }
                    // No recovery decision is taken from Normal.
                    RecoveryGuard::new(
                        a.quality > self.config.exit_threshold + EPS,
                        true,
                        !a.has_contradiction,
                        !a.has_critical_mnar,
                    )
                }
                DegradationMode::Degraded => {
                    cycles_in_degraded = cycles_in_degraded.saturating_add(1);
                    let guard = RecoveryGuard::new(
                        a.quality > self.config.exit_threshold + EPS,
                        cycles_in_degraded > self.config.cooldown_cycles,
                        !a.has_contradiction,
                        !a.has_critical_mnar,
                    );
                    // Promote to Normal ONLY when every measurable guard is met.
                    if guard.satisfied && !hard_hazard {
                        mode = DegradationMode::Normal;
                        cycles_in_degraded = 0;
                    }
                    guard
                }
            };

            let reason = match (from, mode) {
                (DegradationMode::Normal, DegradationMode::Degraded) => {
                    if hard_hazard {
                        "enter_degraded:hazard"
                    } else {
                        "enter_degraded:low_quality"
                    }
                }
                (DegradationMode::Degraded, DegradationMode::Normal) => "recover:guard_satisfied",
                (DegradationMode::Degraded, DegradationMode::Degraded) => {
                    if guard.satisfied {
                        // Guard satisfied but a live hazard held us — record it.
                        "hold_degraded:hazard"
                    } else {
                        "hold_degraded:guard_unmet"
                    }
                }
                (DegradationMode::Normal, DegradationMode::Normal) => "hold_normal",
            };

            transitions.push(ModeTransition {
                run_id: run_id.to_string(),
                cycle: i,
                quality: fmt6(a.quality),
                from_mode: from,
                to_mode: mode,
                recovery_guard: guard,
                reason: reason.to_string(),
            });
        }

        transitions
    }
}

#[derive(Serialize)]
struct DecisionId<'a> {
    run_id: &'a str,
    subject_id: &'a str,
    proposed: &'a str,
    quality: String,
    tier: &'a str,
}

/// One state-machine transition (float-free).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeTransition {
    /// Deterministic run id.
    pub run_id: String,
    /// Cycle index.
    pub cycle: usize,
    /// Aggregate quality at this cycle (fixed-decimal).
    pub quality: String,
    /// Mode before this cycle.
    pub from_mode: DegradationMode,
    /// Mode after this cycle.
    pub to_mode: DegradationMode,
    /// The recovery guard evaluated at this cycle.
    pub recovery_guard: RecoveryGuard,
    /// Human-readable transition reason.
    pub reason: String,
}

// ── Scenarios + report ───────────────────────────────────────────────────────

/// A named decision scenario for the report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DegradationScenario {
    /// Subject id under decision.
    pub subject_id: String,
    /// The upstream-proposed action.
    pub proposed_action: MigrationDecision,
    /// The evidence bundle.
    pub signals: Vec<EvidenceSignal>,
}

impl DegradationScenario {
    /// Construct a scenario.
    #[must_use]
    pub fn new(
        subject_id: impl Into<String>,
        proposed_action: MigrationDecision,
        signals: Vec<EvidenceSignal>,
    ) -> Self {
        Self {
            subject_id: subject_id.into(),
            proposed_action,
            signals,
        }
    }
}

/// Roll-up of a degradation report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DegradationSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the ledger + transitions.
    pub evidence_checksum: String,
    /// Total compiled decisions.
    pub total_decisions: usize,
    /// Decisions that operated in degraded mode.
    pub degraded_decisions: usize,
    /// Decisions that triggered the anomaly hard-clamp.
    pub anomaly_clamped: usize,
    /// Decisions that required operator review.
    pub operator_reviews: usize,
    /// State-machine transitions simulated.
    pub transitions: usize,
    /// Whether every row carries all mandated fields (AC4).
    pub required_fields_complete: bool,
    /// Whether every row's flags match their recorded arithmetic.
    pub clauses_consistent: bool,
    /// Whether every raw computation stayed finite (AC1).
    pub numerically_stable: bool,
    /// AC1: no decision path failed catastrophically — every final action is at
    /// least as conservative as the action its residual risk requires.
    pub no_catastrophic_failure: bool,
    /// AC2: every degraded decision emitted reason codes, a positive uncertainty
    /// delta, and (when hard-clamped) an explicit conservative clamp + escalation.
    pub degraded_explained: bool,
    /// AC3: no state-machine promotion to Normal occurred without a satisfied
    /// recovery guard.
    pub recovery_guarded: bool,
    /// AC4: the artifacts are deterministic and auditable (float-free + replayable).
    pub deterministic_auditable: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DegradationStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// A full degradation report: ledger + state-machine transitions + summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DegradationReport {
    /// The compiled decisions.
    pub ledger: Vec<DegradationDecision>,
    /// The simulated state-machine transitions.
    pub transitions: Vec<ModeTransition>,
    /// The roll-up summary + gate.
    pub summary: DegradationSummary,
    /// The deterministic JSON-stats artifact.
    pub exported_json_stats: DegradationStatsArtifact,
}

impl DegradationReport {
    /// Look up a ledger entry by subject id.
    #[must_use]
    pub fn entry(&self, subject_id: &str) -> Option<&DegradationDecision> {
        self.ledger.iter().find(|e| e.subject_id == subject_id)
    }

    /// Whether the gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

/// Compile a full degradation report over `scenarios` plus a recovery `sequence`
/// driving the state machine.
#[must_use]
pub fn run_degradation_report(
    label: &str,
    scenarios: &[DegradationScenario],
    sequence: &[Vec<EvidenceSignal>],
    policy: &DegradationPolicy,
) -> DegradationReport {
    let run_id = short_hash(&stable_hash(&format!(
        "{DEGRADATION_POLICY_SCHEMA_VERSION}|{label}"
    )));

    let ledger: Vec<DegradationDecision> = scenarios
        .iter()
        .map(|s| policy.compile(&run_id, &s.subject_id, s.proposed_action, &s.signals))
        .collect();
    let transitions = policy.simulate(&run_id, sequence);

    let evidence_checksum = stable_hash(&Checksummed {
        ledger: &ledger,
        transitions: &transitions,
    });
    let report_id = short_hash(&stable_hash(&format!("{run_id}|{evidence_checksum}")));

    let degraded_decisions = ledger
        .iter()
        .filter(|e| matches!(e.mode, DegradationMode::Degraded))
        .count();
    let anomaly_clamped = ledger
        .iter()
        .filter(|e| {
            e.degradation_reasons
                .contains(&DegradationReason::AnomalyClamp)
        })
        .count();
    let operator_reviews = ledger.iter().filter(|e| e.operator_review_required).count();

    let required_fields_complete = ledger.iter().all(decision_has_required_fields);
    let clauses_consistent = ledger.iter().all(|e| e.clause_consistent);
    // AC1: every claim's raw computation stayed finite (checked pre-render) and the
    // rendered strings parse back to finite numbers.
    let numerically_stable = ledger.iter().all(|e| {
        e.numerically_finite
            && [
                &e.evidence_quality_score,
                &e.inflation_factor,
                &e.uncertainty_delta,
                &e.residual_risk,
                &e.max_risk_envelope,
            ]
            .iter()
            .all(|s| s.parse::<f64>().is_ok_and(f64::is_finite))
    });
    // AC1: no catastrophic failure — independently re-derive the required action
    // from the recorded residual risk and confirm the final action is at least as
    // conservative (so it can never be the silent, unsafe choice).
    let no_catastrophic_failure = ledger.iter().all(|e| {
        let risk = e.residual_risk.parse::<f64>().unwrap_or(1.0);
        let required = required_action_for_risk(risk);
        at_least_as_conservative(e.final_action, required)
            && at_least_as_conservative(e.final_action, e.ceiling_action)
    });
    // AC2: every degraded decision is explained.
    let degraded_explained = ledger.iter().all(|e| {
        if matches!(e.mode, DegradationMode::Normal) {
            return true;
        }
        let delta = e.uncertainty_delta.parse::<f64>().unwrap_or(0.0);
        let clamp_ok = !e.operator_review_required
            || e.final_action == MigrationDecision::ConservativeFallback;
        !e.degradation_reasons.is_empty() && delta > EPS && clamp_ok
    });
    // AC3: no promotion to Normal without a satisfied recovery guard.
    let recovery_guarded = transitions.iter().all(|t| {
        !(matches!(t.from_mode, DegradationMode::Degraded)
            && matches!(t.to_mode, DegradationMode::Normal))
            || t.recovery_guard.satisfied
    });
    // AC4: float-free + replayable + clause-consistent + reproducible.
    let deterministic_auditable = clauses_consistent
        && numerically_stable
        && ledger
            .iter()
            .all(|e| !e.reproduction_command.is_empty() && !e.decision_id.is_empty());

    let gate_passes = required_fields_complete
        && clauses_consistent
        && numerically_stable
        && no_catastrophic_failure
        && degraded_explained
        && recovery_guarded
        && deterministic_auditable;

    let summary = DegradationSummary {
        schema_version: DEGRADATION_POLICY_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        run_id: run_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_decisions: ledger.len(),
        degraded_decisions,
        anomaly_clamped,
        operator_reviews,
        transitions: transitions.len(),
        required_fields_complete,
        clauses_consistent,
        numerically_stable,
        no_catastrophic_failure,
        degraded_explained,
        recovery_guarded,
        deterministic_auditable,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib degradation_policy # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a DegradationSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: DEGRADATION_POLICY_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let sha256 = sha256_hex(content.as_bytes());
        DegradationStatsArtifact {
            path: format!("degradation_policy/{report_id}.json"),
            sha256,
            content,
        }
    };

    DegradationReport {
        ledger,
        transitions,
        summary,
        exported_json_stats,
    }
}

#[derive(Serialize)]
struct Checksummed<'a> {
    ledger: &'a [DegradationDecision],
    transitions: &'a [ModeTransition],
}

/// A representative corpus of degradation scenarios spanning the tiers and hazards.
#[must_use]
pub fn default_degradation_scenarios() -> Vec<DegradationScenario> {
    let present =
        |id: &str, w: f64, q: f64| EvidenceSignal::new(id, w, SignalStatus::Present { quality: q });
    vec![
        // Full evidence: AutoApprove survives.
        DegradationScenario::new(
            "unit.full",
            MigrationDecision::AutoApprove,
            vec![
                present("widget_coverage", 1.0, 0.95),
                present("runtime_event", 1.0, 0.92),
                present("snapshot_parity", 1.0, 0.90),
            ],
        ),
        // Partial: one MCAR gap caps permissiveness.
        DegradationScenario::new(
            "unit.partial",
            MigrationDecision::AutoApprove,
            vec![
                present("widget_coverage", 1.0, 0.88),
                present("runtime_event", 1.0, 0.80),
                EvidenceSignal::new(
                    "snapshot_parity",
                    1.0,
                    SignalStatus::Missing {
                        class: MissingnessClass::Mcar,
                    },
                ),
            ],
        ),
        // Degraded: delayed + MAR-missing evidence.
        DegradationScenario::new(
            "unit.degraded",
            MigrationDecision::AutoApprove,
            vec![
                present("widget_coverage", 1.0, 0.55),
                EvidenceSignal::new(
                    "runtime_event",
                    1.0,
                    SignalStatus::Delayed { staleness: 0.6 },
                ),
                EvidenceSignal::new(
                    "snapshot_parity",
                    1.0,
                    SignalStatus::Missing {
                        class: MissingnessClass::Mar,
                    },
                ),
            ],
        ),
        // Contradictory anomaly: high dissent → hard clamp + operator review.
        DegradationScenario::new(
            "unit.anomaly",
            MigrationDecision::AutoApprove,
            vec![
                present("widget_coverage", 1.0, 0.70),
                EvidenceSignal::new(
                    "runtime_event",
                    1.0,
                    SignalStatus::Contradictory { dissent: 0.85 },
                ),
            ],
        ),
        // Critical MNAR hazard on a load-bearing channel → conservative fallback.
        DegradationScenario::new(
            "unit.critical_mnar",
            MigrationDecision::HumanReview,
            vec![
                present("widget_coverage", 1.0, 0.60),
                EvidenceSignal::new(
                    "safety_invariant",
                    2.0,
                    SignalStatus::Missing {
                        class: MissingnessClass::Mnar,
                    },
                )
                .critical(),
            ],
        ),
    ]
}

/// A recovery sequence that drives the state machine: drop into degraded mode,
/// then climb back out only after the hysteretic guard + cooldown are satisfied.
#[must_use]
pub fn default_recovery_sequence() -> Vec<Vec<EvidenceSignal>> {
    let q = |quality: f64| {
        vec![EvidenceSignal::new(
            "aggregate",
            1.0,
            SignalStatus::Present { quality },
        )]
    };
    vec![
        q(0.95), // normal
        q(0.30), // enter degraded (below enter_threshold 0.50)
        q(0.65), // above enter, below exit (0.70) → hold (no oscillation)
        q(0.80), // above exit but cooldown not yet elapsed → hold
        q(0.82), // cooldown elapsed + quality recovered → promote to normal
        q(0.90), // hold normal
    ]
}

/// Run the default degradation report.
#[must_use]
pub fn run_default_degradation_report(label: &str) -> DegradationReport {
    run_degradation_report(
        label,
        &default_degradation_scenarios(),
        &default_recovery_sequence(),
        &DegradationPolicy::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> DegradationPolicy {
        DegradationPolicy::default()
    }

    fn present(id: &str, w: f64, q: f64) -> EvidenceSignal {
        EvidenceSignal::new(id, w, SignalStatus::Present { quality: q })
    }

    #[test]
    fn full_evidence_keeps_proposed_action() {
        let signals = vec![present("a", 1.0, 0.95), present("b", 1.0, 0.93)];
        let d = policy().compile("run", "s.full", MigrationDecision::AutoApprove, &signals);
        assert_eq!(d.evidence_tier, EvidenceTier::Full);
        assert_eq!(d.mode, DegradationMode::Normal);
        assert_eq!(d.final_action, MigrationDecision::AutoApprove);
        assert_eq!(d.uncertainty_delta, fmt6(0.0));
        assert!(d.degradation_reasons.is_empty());
        assert!(d.clause_consistent);
    }

    #[test]
    fn partial_evidence_caps_auto_approve() {
        let signals = vec![
            present("a", 1.0, 0.88),
            present("b", 1.0, 0.80),
            EvidenceSignal::new(
                "c",
                1.0,
                SignalStatus::Missing {
                    class: MissingnessClass::Mcar,
                },
            ),
        ];
        let d = policy().compile("run", "s.partial", MigrationDecision::AutoApprove, &signals);
        assert_eq!(d.evidence_tier, EvidenceTier::Partial);
        assert_eq!(d.mode, DegradationMode::Degraded);
        // AutoApprove is no longer permitted; final is at least HumanReview.
        assert!(at_least_as_conservative(
            d.final_action,
            MigrationDecision::HumanReview
        ));
        assert!(
            d.degradation_reasons
                .contains(&DegradationReason::MissingMcar)
        );
        // The missing channel is imputed with carry-forward provenance.
        assert_eq!(d.imputations.len(), 1);
        assert_eq!(d.imputations[0].family, ImputationFamily::CarryForward);
        assert!(d.clause_consistent);
    }

    #[test]
    fn critical_evidence_forces_conservative() {
        // Barely-usable evidence: one very-low-quality present channel plus an
        // MAR gap ⇒ aggregate quality 0.25 < 0.35 ⇒ Critical tier.
        let signals = vec![
            present("a", 1.0, 0.10),
            EvidenceSignal::new(
                "b",
                1.0,
                SignalStatus::Missing {
                    class: MissingnessClass::Mar,
                },
            ),
        ];
        let d = policy().compile("run", "s.crit", MigrationDecision::AutoApprove, &signals);
        assert_eq!(d.evidence_tier, EvidenceTier::Critical);
        assert_eq!(d.final_action, MigrationDecision::ConservativeFallback);
        assert!(d.uncertainty_delta.parse::<f64>().unwrap() > 0.0);
        assert!(d.clause_consistent);
    }

    #[test]
    fn critical_mnar_hazard_forces_fallback_and_operator_review() {
        let signals = vec![
            present("widget", 1.0, 0.70),
            EvidenceSignal::new(
                "safety",
                2.0,
                SignalStatus::Missing {
                    class: MissingnessClass::Mnar,
                },
            )
            .critical(),
        ];
        let d = policy().compile("run", "s.mnar", MigrationDecision::AutoApprove, &signals);
        assert_eq!(d.final_action, MigrationDecision::ConservativeFallback);
        assert!(d.operator_review_required);
        assert!(
            d.degradation_reasons
                .contains(&DegradationReason::CriticalHazard)
        );
        assert!(
            d.degradation_reasons
                .contains(&DegradationReason::MissingMnar)
        );
        assert!(d.clause_consistent);
    }

    #[test]
    fn contradiction_anomaly_triggers_hard_clamp() {
        let signals = vec![
            present("a", 1.0, 0.80),
            EvidenceSignal::new("b", 1.0, SignalStatus::Contradictory { dissent: 0.9 }),
        ];
        let d = policy().compile("run", "s.anom", MigrationDecision::AutoApprove, &signals);
        assert!(
            d.degradation_reasons
                .contains(&DegradationReason::AnomalyClamp)
        );
        assert_eq!(d.final_action, MigrationDecision::ConservativeFallback);
        assert!(d.operator_review_required);
        assert!(d.clause_consistent);
    }

    #[test]
    fn missing_channels_carry_provenance_by_class() {
        let signals = vec![
            EvidenceSignal::new(
                "mcar",
                1.0,
                SignalStatus::Missing {
                    class: MissingnessClass::Mcar,
                },
            ),
            EvidenceSignal::new(
                "mar",
                1.0,
                SignalStatus::Missing {
                    class: MissingnessClass::Mar,
                },
            ),
            EvidenceSignal::new(
                "mnar",
                1.0,
                SignalStatus::Missing {
                    class: MissingnessClass::Mnar,
                },
            ),
        ];
        let d = policy().compile("run", "s.imp", MigrationDecision::AutoApprove, &signals);
        // Sorted by channel id: mar, mcar, mnar.
        let fams: Vec<ImputationFamily> = d.imputations.iter().map(|i| i.family).collect();
        assert_eq!(
            fams,
            vec![
                ImputationFamily::ConservativePrior,
                ImputationFamily::CarryForward,
                ImputationFamily::BoundedWorstCase,
            ]
        );
        assert_eq!(d.imputations[2].provenance, "bounded_worst_case_assumption");
    }

    #[test]
    fn final_action_is_always_safe_for_any_missingness_pattern() {
        // AC1: exhaustively combine statuses; the final action must always be at
        // least as conservative as the action its residual risk requires.
        let statuses = [
            SignalStatus::Present { quality: 0.9 },
            SignalStatus::Present { quality: 0.4 },
            SignalStatus::Missing {
                class: MissingnessClass::Mcar,
            },
            SignalStatus::Missing {
                class: MissingnessClass::Mnar,
            },
            SignalStatus::Delayed { staleness: 0.7 },
            SignalStatus::Contradictory { dissent: 0.5 },
        ];
        let p = policy();
        for (i, s0) in statuses.iter().enumerate() {
            for (j, s1) in statuses.iter().enumerate() {
                for proposed in [
                    MigrationDecision::AutoApprove,
                    MigrationDecision::HumanReview,
                    MigrationDecision::Reject,
                ] {
                    let signals = vec![
                        EvidenceSignal::new(format!("c{i}"), 1.0, s0.clone()),
                        EvidenceSignal::new(format!("d{j}"), 1.0, s1.clone()),
                    ];
                    let d = p.compile("run", "s.fuzz", proposed, &signals);
                    let risk = d.residual_risk.parse::<f64>().unwrap();
                    let required = required_action_for_risk(risk);
                    assert!(
                        at_least_as_conservative(d.final_action, required),
                        "unsafe: final={:?} required={:?} risk={}",
                        d.final_action,
                        required,
                        d.residual_risk
                    );
                    assert!(d.numerically_finite);
                    assert!(d.clause_consistent);
                }
            }
        }
    }

    #[test]
    fn empty_evidence_is_conservative_not_a_panic() {
        let d = policy().compile("run", "s.empty", MigrationDecision::AutoApprove, &[]);
        assert_eq!(d.evidence_tier, EvidenceTier::Critical);
        assert_eq!(d.final_action, MigrationDecision::ConservativeFallback);
        assert!(d.numerically_finite);
        assert!(d.clause_consistent);
    }

    #[test]
    fn state_machine_has_hysteresis_and_no_oscillation() {
        let transitions = policy().simulate("run", &default_recovery_sequence());
        let modes: Vec<DegradationMode> = transitions.iter().map(|t| t.to_mode).collect();
        // normal, degraded, degraded, degraded, normal, normal.
        assert_eq!(
            modes,
            vec![
                DegradationMode::Normal,
                DegradationMode::Degraded,
                DegradationMode::Degraded,
                DegradationMode::Degraded,
                DegradationMode::Normal,
                DegradationMode::Normal,
            ]
        );
        // The 0.65 cycle is above enter (0.50) but below exit (0.70): it must NOT
        // flip back, proving hysteresis.
        assert_eq!(transitions[2].to_mode, DegradationMode::Degraded);
    }

    #[test]
    fn no_promotion_to_normal_without_satisfied_guard() {
        // A sequence that recovers quality but keeps a live contradiction must
        // never promote back to Normal (AC3).
        let seq = vec![
            vec![present("a", 1.0, 0.95)],
            vec![present("a", 1.0, 0.20)], // enter degraded
            vec![present("a", 1.0, 0.95)], // quality fine...
            // ...but a contradiction holds the guard open every later cycle.
            vec![
                present("a", 1.0, 0.95),
                EvidenceSignal::new("b", 1.0, SignalStatus::Contradictory { dissent: 0.3 }),
            ],
            vec![
                present("a", 1.0, 0.95),
                EvidenceSignal::new("b", 1.0, SignalStatus::Contradictory { dissent: 0.3 }),
            ],
        ];
        let transitions = policy().simulate("run", &seq);
        for t in &transitions {
            if matches!(t.from_mode, DegradationMode::Degraded)
                && matches!(t.to_mode, DegradationMode::Normal)
            {
                assert!(t.recovery_guard.satisfied, "promoted without guard");
            }
        }
        // The contradiction blocks recovery, so the controller stays degraded.
        assert_eq!(
            transitions.last().unwrap().to_mode,
            DegradationMode::Degraded
        );
    }

    #[test]
    fn ledger_is_float_free_and_replay_stable() {
        let a = run_default_degradation_report("degradation/test");
        let b = run_default_degradation_report("degradation/test");
        assert_eq!(a.summary.report_id, b.summary.report_id);
        assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
        assert_eq!(a.transitions, b.transitions);
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_degradation_report("degradation/test");
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_decisions, 5);
        assert!(report.summary.degraded_decisions >= 4);
        assert!(report.summary.anomaly_clamped >= 1);
        assert!(report.summary.operator_reviews >= 2);
        assert!(report.summary.no_catastrophic_failure);
        assert!(report.summary.degraded_explained);
        assert!(report.summary.recovery_guarded);
        assert!(report.summary.deterministic_auditable);
        for e in &report.ledger {
            assert!(decision_has_required_fields(e));
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_degradation_report("degradation/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    #[test]
    fn required_action_is_monotone_in_risk() {
        let mut prev = required_action_for_risk(0.0);
        for i in 1..=100 {
            let r = f64::from(i) / 100.0;
            let cur = required_action_for_risk(r);
            // Conservatism rank must be non-increasing as risk grows (more risk ⇒
            // more conservative ⇒ lower or equal rank).
            assert!(conservatism_rank(cur) <= conservatism_rank(prev));
            prev = cur;
        }
    }
}

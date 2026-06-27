// SPDX-License-Identifier: Apache-2.0
//! Decision-theoretic loss-matrix compiler and expected-loss action policy engine.
//!
//! Threshold-style verdicts ("ship if `posterior_prob >= 0.9`") collapse a rich
//! cost structure into a single cut point. This module replaces that with
//! explicit **expected-loss minimization** over the full migration action space
//! (`accept`, `review`, `reject`, `holdback`, `rollback`, `conservative-fallback`)
//! and a versioned, auditable loss matrix.
//!
//! # Action vocabulary
//!
//! The action space is the crate-canonical [`MigrationDecision`] so every
//! decision logged here is directly comparable with the posterior ledger
//! ([`crate::posterior_core`]) and the reverse-round governance ledger
//! ([`crate::reverse_round_governance`]). The task vocabulary maps 1:1:
//!
//! | task action            | [`MigrationDecision`]                |
//! |------------------------|--------------------------------------|
//! | `accept`               | [`MigrationDecision::AutoApprove`]    |
//! | `review`               | [`MigrationDecision::HumanReview`]    |
//! | `reject`               | [`MigrationDecision::Reject`]         |
//! | `holdback` (quarantine)| [`MigrationDecision::HardReject`]     |
//! | `rollback`             | [`MigrationDecision::Rollback`]       |
//! | `conservative-fallback`| [`MigrationDecision::ConservativeFallback`] |
//!
//! # Pipeline
//!
//! 1. A [`LossPolicyManifest`] declares one [`LossMatrixSpec`] per
//!    (`PolicyProfile`, `RiskTier`) cell. Each spec assigns a loss to every
//!    (action, [`OutcomeState`]) pair.
//! 2. [`compile`] validates the manifest, applies any explicit
//!    [`LossOverrideArtifact`]s, canonicalizes ordering, and stamps a base +
//!    effective SHA-256 checksum onto every matrix — yielding a
//!    [`CompiledLossPolicy`] whose loss assumptions are versioned and auditable
//!    (AC2: loss values may only deviate from the base via a recorded override
//!    artifact).
//! 3. [`solve_expected_loss`] computes, for a [`StateDistribution`] derived from
//!    a posterior, the expected loss of every action and returns the
//!    `argmin` action with deterministic tie-breaks (AC1).
//! 4. [`simulate_sensitivity`] perturbs each loss cell within configured ranges
//!    and reports the counterfactual decision diffs that flip the verdict (AC3).
//!
//! # Determinism
//!
//! Matrices, actions, and states are processed in canonical order; every report
//! id and checksum is a SHA-256 of the canonical serialization. Repeated runs
//! over the same inputs produce byte-identical artifacts. Floating-point loss
//! values are author-provided and never round-tripped before hashing within a
//! run, so the stable ids do not depend on serde float reparse behavior.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::posterior_core::PosteriorRecord;
use crate::semantic_contract::MigrationDecision;

// ── Schema Version & Constants ───────────────────────────────────────────────

/// Schema version for decision-loss-policy artifacts.
pub const DECISION_LOSS_POLICY_SCHEMA_VERSION: &str = "decision-loss-policy-v1";

/// Numerical tolerance for "probabilities sum to one" checks.
const PROB_SUM_EPSILON: f64 = 1e-6;

/// Every migration action, in canonical (most-permissive → most-conservative)
/// order. This order defines row ordering and the final tie-break.
const ALL_ACTIONS: [MigrationDecision; 6] = [
    MigrationDecision::AutoApprove,
    MigrationDecision::HumanReview,
    MigrationDecision::Reject,
    MigrationDecision::HardReject,
    MigrationDecision::Rollback,
    MigrationDecision::ConservativeFallback,
];

/// Stable snake_case string for a [`MigrationDecision`] (matches its serde form).
#[must_use]
pub fn action_str(action: MigrationDecision) -> &'static str {
    match action {
        MigrationDecision::AutoApprove => "auto_approve",
        MigrationDecision::HumanReview => "human_review",
        MigrationDecision::Reject => "reject",
        MigrationDecision::HardReject => "hard_reject",
        MigrationDecision::Rollback => "rollback",
        MigrationDecision::ConservativeFallback => "conservative_fallback",
    }
}

/// Canonical index of an action in [`ALL_ACTIONS`] (`0..6`).
#[must_use]
pub fn action_index(action: MigrationDecision) -> usize {
    match action {
        MigrationDecision::AutoApprove => 0,
        MigrationDecision::HumanReview => 1,
        MigrationDecision::Reject => 2,
        MigrationDecision::HardReject => 3,
        MigrationDecision::Rollback => 4,
        MigrationDecision::ConservativeFallback => 5,
    }
}

/// Tie-break preference rank: **lower wins**. On a genuine tie (equal expected
/// loss and equal worst-case loss) the more conservative action is selected, so
/// the governance gate never resolves a coin-flip toward shipping.
#[must_use]
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

// ── Errors ───────────────────────────────────────────────────────────────────

/// Errors raised while compiling or solving a decision-loss policy.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DecisionLossError {
    /// The manifest declared no matrices.
    #[error("loss policy manifest is empty")]
    EmptyManifest,
    /// Two specs share the same (profile, tier) key.
    #[error("duplicate loss matrix for profile '{profile}' tier '{tier}'")]
    DuplicateEntry {
        /// Offending profile.
        profile: &'static str,
        /// Offending tier.
        tier: &'static str,
    },
    /// A requested (profile, tier) matrix is absent from the policy.
    #[error("no loss matrix for profile '{profile}' tier '{tier}'")]
    MissingEntry {
        /// Requested profile.
        profile: &'static str,
        /// Requested tier.
        tier: &'static str,
    },
    /// A matrix is missing a required (action, state) cell.
    #[error("matrix '{profile}/{tier}' missing cell for action '{action}' state '{state}'")]
    MissingCell {
        /// Profile.
        profile: &'static str,
        /// Tier.
        tier: &'static str,
        /// Missing action.
        action: &'static str,
        /// Missing state.
        state: &'static str,
    },
    /// A matrix declared the same (action, state) cell more than once.
    #[error("matrix '{profile}/{tier}' has duplicate cell for action '{action}' state '{state}'")]
    DuplicateCell {
        /// Profile.
        profile: &'static str,
        /// Tier.
        tier: &'static str,
        /// Duplicated action.
        action: &'static str,
        /// Duplicated state.
        state: &'static str,
    },
    /// A loss value is negative or non-finite.
    #[error(
        "matrix '{profile}/{tier}' cell action '{action}' state '{state}' has invalid loss {loss}"
    )]
    InvalidLoss {
        /// Profile.
        profile: &'static str,
        /// Tier.
        tier: &'static str,
        /// Action.
        action: &'static str,
        /// State.
        state: &'static str,
        /// The offending value.
        loss: f64,
    },
    /// A dominance invariant was violated (e.g. accepting a broken migration is
    /// cheaper than accepting a faithful one, or cheaper than rejecting it).
    #[error("matrix '{profile}/{tier}' violates dominance invariant: {detail}")]
    DominanceViolation {
        /// Profile.
        profile: &'static str,
        /// Tier.
        tier: &'static str,
        /// Human-readable explanation.
        detail: String,
    },
    /// An override artifact targets a (profile, tier) cell that does not exist.
    #[error("override artifact '{artifact_id}' targets unknown matrix '{profile}/{tier}'")]
    OverrideMatrixNotFound {
        /// Artifact id.
        artifact_id: String,
        /// Targeted profile.
        profile: &'static str,
        /// Targeted tier.
        tier: &'static str,
    },
    /// An override artifact carries a negative or non-finite loss value.
    #[error("override artifact '{artifact_id}' has invalid loss {loss}")]
    InvalidOverrideLoss {
        /// Artifact id.
        artifact_id: String,
        /// The offending value.
        loss: f64,
    },
    /// A state distribution contains a negative or non-finite probability.
    #[error("state distribution has invalid probability {probability} for state '{state}'")]
    InvalidProbability {
        /// State.
        state: &'static str,
        /// The offending value.
        probability: f64,
    },
    /// A state distribution does not sum to one (and renormalization is off).
    #[error("state distribution sums to {sum}, expected 1.0 (enable renormalization to allow)")]
    UnnormalizedDistribution {
        /// The observed mass.
        sum: f64,
    },
    /// A state distribution carried no positive mass at all.
    #[error("state distribution is empty (no positive mass)")]
    EmptyDistribution,
}

/// Convenience result alias for this module.
type Result<T> = std::result::Result<T, DecisionLossError>;

// ── Policy Profile & Risk Tier ───────────────────────────────────────────────

/// Risk appetite of the governing policy. The profile shifts the loss matrix:
/// conservative profiles inflate the cost of shipping a bad migration and
/// deflate the cost of holding/reviewing; aggressive profiles do the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyProfile {
    /// Minimize the chance of shipping a regression at the cost of throughput.
    Conservative,
    /// Balanced trade-off between false-accept and false-reject losses.
    Balanced,
    /// Favor throughput; tolerate more risk before holding/rejecting.
    Aggressive,
}

impl PolicyProfile {
    /// Every profile, in canonical order.
    pub const ALL: [PolicyProfile; 3] = [
        PolicyProfile::Conservative,
        PolicyProfile::Balanced,
        PolicyProfile::Aggressive,
    ];

    /// Stable string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PolicyProfile::Conservative => "conservative",
            PolicyProfile::Balanced => "balanced",
            PolicyProfile::Aggressive => "aggressive",
        }
    }

    /// Multiplier applied to the cost of shipping a bad migration.
    #[must_use]
    fn accept_penalty_mult(self) -> f64 {
        match self {
            PolicyProfile::Conservative => 1.5,
            PolicyProfile::Balanced => 1.0,
            PolicyProfile::Aggressive => 0.6,
        }
    }

    /// Multiplier applied to the friction of holding / reviewing.
    #[must_use]
    fn hold_cost_mult(self) -> f64 {
        match self {
            PolicyProfile::Conservative => 0.5,
            PolicyProfile::Balanced => 1.0,
            PolicyProfile::Aggressive => 1.6,
        }
    }

    /// Multiplier applied to the opportunity cost of blocking a good migration.
    #[must_use]
    fn opportunity_mult(self) -> f64 {
        match self {
            PolicyProfile::Conservative => 0.7,
            PolicyProfile::Balanced => 1.0,
            PolicyProfile::Aggressive => 1.4,
        }
    }
}

/// Blast radius of the change being governed. Higher tiers scale the cost of
/// both shipping a bad migration and blocking a good one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskTier {
    /// Load-bearing surface; a regression is catastrophic.
    Critical,
    /// High-traffic surface; a regression is serious.
    High,
    /// Moderate surface.
    Medium,
    /// Low-impact surface; regressions are cheap to absorb.
    Low,
}

impl RiskTier {
    /// Every tier, in canonical (most-severe → least-severe) order.
    pub const ALL: [RiskTier; 4] = [
        RiskTier::Critical,
        RiskTier::High,
        RiskTier::Medium,
        RiskTier::Low,
    ];

    /// Stable string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RiskTier::Critical => "critical",
            RiskTier::High => "high",
            RiskTier::Medium => "medium",
            RiskTier::Low => "low",
        }
    }

    /// Severity scale used by the calibrated default matrices.
    #[must_use]
    fn scale(self) -> f64 {
        match self {
            RiskTier::Critical => 8.0,
            RiskTier::High => 4.0,
            RiskTier::Medium => 2.0,
            RiskTier::Low => 1.0,
        }
    }
}

// ── Outcome States ───────────────────────────────────────────────────────────

/// A possible true state of a migration outcome. The loss matrix has one column
/// per state; the posterior supplies a probability distribution over them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeState {
    /// Behaviorally faithful; no observable regression.
    Faithful,
    /// Cosmetic / normalized drift that is acceptable in practice.
    BenignDrift,
    /// Functional or performance regression.
    Regressed,
    /// Does not build or run.
    Broken,
}

impl OutcomeState {
    /// Every state, in canonical (best → worst) order.
    pub const ALL: [OutcomeState; 4] = [
        OutcomeState::Faithful,
        OutcomeState::BenignDrift,
        OutcomeState::Regressed,
        OutcomeState::Broken,
    ];

    /// Stable string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            OutcomeState::Faithful => "faithful",
            OutcomeState::BenignDrift => "benign_drift",
            OutcomeState::Regressed => "regressed",
            OutcomeState::Broken => "broken",
        }
    }

    /// Zero-based canonical index (`0..4`), used to index compiled loss rows.
    #[must_use]
    pub fn order_index(self) -> usize {
        match self {
            OutcomeState::Faithful => 0,
            OutcomeState::BenignDrift => 1,
            OutcomeState::Regressed => 2,
            OutcomeState::Broken => 3,
        }
    }

    /// Normalized "badness if shipped" weight in `[0, 1]` (Faithful = 0).
    #[must_use]
    fn badness(self) -> f64 {
        match self {
            OutcomeState::Faithful => 0.0,
            OutcomeState::BenignDrift => 0.1,
            OutcomeState::Regressed => 0.6,
            OutcomeState::Broken => 1.0,
        }
    }

    /// Normalized "value forgone if blocked" weight in `[0, 1]` (Broken = 0).
    #[must_use]
    fn goodness(self) -> f64 {
        match self {
            OutcomeState::Faithful => 1.0,
            OutcomeState::BenignDrift => 0.8,
            OutcomeState::Regressed => 0.2,
            OutcomeState::Broken => 0.0,
        }
    }
}

// ── State Distribution ───────────────────────────────────────────────────────

/// One state probability entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateProbability {
    /// The state.
    pub state: OutcomeState,
    /// Its probability mass.
    pub probability: f64,
}

/// A probability distribution over [`OutcomeState`]s. Entries are kept in
/// canonical state order; absent states carry zero mass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateDistribution {
    /// Canonical-ordered probability entries.
    pub entries: Vec<StateProbability>,
}

/// How to map a scalar posterior into a [`StateDistribution`].
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StateMapping {
    /// `P(Faithful) = posterior_prob`, `P(Regressed) = 1 - posterior_prob`.
    #[default]
    Binary,
    /// Split the failure tail `1 - posterior_prob` across the three failure
    /// states by explicit (normalized) shares. Shares are auditable config, not
    /// inferred magic.
    Tiered {
        /// Share of the failure tail attributed to benign drift.
        benign_share: f64,
        /// Share attributed to a functional/performance regression.
        regressed_share: f64,
        /// Share attributed to a broken build/run.
        broken_share: f64,
    },
}

impl StateDistribution {
    /// Build a distribution from explicit `(state, probability)` pairs. The
    /// entries are summed per state and sorted canonically.
    ///
    /// # Errors
    ///
    /// Returns [`DecisionLossError::InvalidProbability`] if any probability is
    /// negative or non-finite, and [`DecisionLossError::EmptyDistribution`] if
    /// the total mass is not positive.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (OutcomeState, f64)>) -> Result<Self> {
        let mut mass = [0.0_f64; OutcomeState::ALL.len()];
        for (state, prob) in pairs {
            if !prob.is_finite() || prob < 0.0 {
                return Err(DecisionLossError::InvalidProbability {
                    state: state.as_str(),
                    probability: prob,
                });
            }
            mass[state.order_index()] += prob;
        }
        let total: f64 = mass.iter().sum();
        if total <= 0.0 {
            return Err(DecisionLossError::EmptyDistribution);
        }
        let entries = OutcomeState::ALL
            .iter()
            .copied()
            .filter(|s| mass[s.order_index()] > 0.0)
            .map(|s| StateProbability {
                state: s,
                probability: mass[s.order_index()],
            })
            .collect();
        Ok(Self { entries })
    }

    /// Binary distribution `{Faithful: p, Regressed: 1 - p}` from a posterior
    /// acceptance probability.
    ///
    /// # Errors
    ///
    /// Returns [`DecisionLossError::InvalidProbability`] if `p` is not a finite
    /// probability in `[0, 1]`.
    pub fn binary_from_posterior(record: &PosteriorRecord) -> Result<Self> {
        let p = record.posterior_prob;
        if !p.is_finite() || !(0.0..=1.0).contains(&p) {
            return Err(DecisionLossError::InvalidProbability {
                state: OutcomeState::Faithful.as_str(),
                probability: p,
            });
        }
        Self::from_pairs([
            (OutcomeState::Faithful, p),
            (OutcomeState::Regressed, 1.0 - p),
        ])
    }

    /// Map a posterior into a distribution using the supplied [`StateMapping`].
    ///
    /// # Errors
    ///
    /// Returns [`DecisionLossError::InvalidProbability`] for an out-of-range
    /// posterior or non-finite share, and [`DecisionLossError::EmptyDistribution`]
    /// for an all-zero tiered split with a zero faithful mass.
    pub fn from_posterior(record: &PosteriorRecord, mapping: StateMapping) -> Result<Self> {
        let p = record.posterior_prob;
        if !p.is_finite() || !(0.0..=1.0).contains(&p) {
            return Err(DecisionLossError::InvalidProbability {
                state: OutcomeState::Faithful.as_str(),
                probability: p,
            });
        }
        match mapping {
            StateMapping::Binary => Self::binary_from_posterior(record),
            StateMapping::Tiered {
                benign_share,
                regressed_share,
                broken_share,
            } => {
                for (state, share) in [
                    (OutcomeState::BenignDrift, benign_share),
                    (OutcomeState::Regressed, regressed_share),
                    (OutcomeState::Broken, broken_share),
                ] {
                    if !share.is_finite() || share < 0.0 {
                        return Err(DecisionLossError::InvalidProbability {
                            state: state.as_str(),
                            probability: share,
                        });
                    }
                }
                let share_total = benign_share + regressed_share + broken_share;
                let tail = 1.0 - p;
                let (benign, regressed, broken) = if share_total > 0.0 {
                    (
                        tail * benign_share / share_total,
                        tail * regressed_share / share_total,
                        tail * broken_share / share_total,
                    )
                } else {
                    // No declared split → attribute the whole tail to a regression.
                    (0.0, tail, 0.0)
                };
                Self::from_pairs([
                    (OutcomeState::Faithful, p),
                    (OutcomeState::BenignDrift, benign),
                    (OutcomeState::Regressed, regressed),
                    (OutcomeState::Broken, broken),
                ])
            }
        }
    }

    /// Total probability mass.
    #[must_use]
    pub fn total_mass(&self) -> f64 {
        self.entries.iter().map(|e| e.probability).sum()
    }

    /// Probability of a given state (`0.0` if absent).
    #[must_use]
    pub fn probability_of(&self, state: OutcomeState) -> f64 {
        self.entries
            .iter()
            .find(|e| e.state == state)
            .map_or(0.0, |e| e.probability)
    }

    /// A copy renormalized so the masses sum to one.
    ///
    /// # Errors
    ///
    /// Returns [`DecisionLossError::EmptyDistribution`] if there is no mass.
    pub fn normalized(&self) -> Result<Self> {
        let total = self.total_mass();
        if total <= 0.0 {
            return Err(DecisionLossError::EmptyDistribution);
        }
        Ok(Self {
            entries: self
                .entries
                .iter()
                .map(|e| StateProbability {
                    state: e.state,
                    probability: e.probability / total,
                })
                .collect(),
        })
    }

    /// Validate the distribution against a solver config, returning a possibly
    /// renormalized copy ready for the solver.
    fn validated(&self, config: &SolverConfig) -> Result<Self> {
        for entry in &self.entries {
            if !entry.probability.is_finite() || entry.probability < 0.0 {
                return Err(DecisionLossError::InvalidProbability {
                    state: entry.state.as_str(),
                    probability: entry.probability,
                });
            }
        }
        let total = self.total_mass();
        if total <= 0.0 {
            return Err(DecisionLossError::EmptyDistribution);
        }
        if config.renormalize_distribution {
            return self.normalized();
        }
        if (total - 1.0).abs() > PROB_SUM_EPSILON {
            return Err(DecisionLossError::UnnormalizedDistribution { sum: total });
        }
        Ok(self.clone())
    }
}

// ── Loss Matrix Spec & Manifest ──────────────────────────────────────────────

/// One author-declared loss cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LossCellSpec {
    /// The action.
    pub action: MigrationDecision,
    /// The true state.
    pub state: OutcomeState,
    /// The loss incurred by taking `action` when the world is in `state`.
    pub loss: f64,
}

/// A declarative loss matrix for one (profile, tier) cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LossMatrixSpec {
    /// Policy profile this matrix governs.
    pub profile: PolicyProfile,
    /// Risk tier this matrix governs.
    pub tier: RiskTier,
    /// One cell per (action, state). Must cover the full cartesian product.
    pub cells: Vec<LossCellSpec>,
}

impl LossMatrixSpec {
    /// Build a calibrated default matrix from the parametric cost family. This
    /// is the "calibrated scoring rubric": the loss of an action in a state is
    ///
    /// ```text
    /// exposure(action) · badness(state)   · ship_bad_cost(tier, profile)
    /// + missed(action) · goodness(state)  · opportunity_cost(tier, profile)
    /// + friction(action, profile)
    /// ```
    #[must_use]
    pub fn calibrated(profile: PolicyProfile, tier: RiskTier) -> Self {
        let t = tier.scale();
        let ship_bad = 100.0 * t * profile.accept_penalty_mult();
        let opportunity = 30.0 * t * profile.opportunity_mult();
        let hold = profile.hold_cost_mult();

        let mut cells = Vec::with_capacity(ALL_ACTIONS.len() * OutcomeState::ALL.len());
        for action in ALL_ACTIONS {
            let (exposure, missed, friction) = action_cost_terms(action, hold);
            for state in OutcomeState::ALL {
                let loss = exposure * state.badness() * ship_bad
                    + missed * state.goodness() * opportunity
                    + friction;
                cells.push(LossCellSpec {
                    action,
                    state,
                    loss,
                });
            }
        }
        Self {
            profile,
            tier,
            cells,
        }
    }
}

/// Per-action cost terms for the calibrated family: `(exposure, missed, friction)`.
///
/// * `exposure` — fraction of a bad migration that reaches users under this action.
/// * `missed` — fraction of a good migration's value forgone under this action.
/// * `friction` — fixed operational cost of taking the action.
#[must_use]
fn action_cost_terms(action: MigrationDecision, hold_mult: f64) -> (f64, f64, f64) {
    match action {
        MigrationDecision::AutoApprove => (1.0, 0.0, 0.0),
        MigrationDecision::HumanReview => (0.25, 0.2, 5.0 * hold_mult),
        MigrationDecision::Reject => (0.0, 1.0, 1.0),
        MigrationDecision::HardReject => (0.0, 1.0, 2.0),
        MigrationDecision::Rollback => (0.1, 1.0, 8.0),
        MigrationDecision::ConservativeFallback => (0.0, 0.7, 4.0 * hold_mult),
    }
}

/// A complete set of loss matrices, versioned for audit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LossPolicyManifest {
    /// Schema version.
    pub schema_version: String,
    /// Author-assigned policy id.
    pub policy_id: String,
    /// Author-assigned policy version (semantic or monotonic).
    pub policy_version: String,
    /// One matrix per (profile, tier).
    pub matrices: Vec<LossMatrixSpec>,
}

impl LossPolicyManifest {
    /// The calibrated standard manifest covering all 12 (profile, tier) cells.
    #[must_use]
    pub fn standard(policy_id: impl Into<String>, policy_version: impl Into<String>) -> Self {
        let mut matrices = Vec::with_capacity(PolicyProfile::ALL.len() * RiskTier::ALL.len());
        for profile in PolicyProfile::ALL {
            for tier in RiskTier::ALL {
                matrices.push(LossMatrixSpec::calibrated(profile, tier));
            }
        }
        Self {
            schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
            policy_id: policy_id.into(),
            policy_version: policy_version.into(),
            matrices,
        }
    }
}

// ── Override Artifacts (AC2) ─────────────────────────────────────────────────

/// An explicit, auditable artifact that overrides a single loss cell. Base loss
/// assumptions can be changed **only** through one of these; the compiled policy
/// records every applied override so the deviation is always traceable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LossOverrideArtifact {
    /// Unique artifact id (e.g. a ticket / change-record id).
    pub artifact_id: String,
    /// Who authored the override.
    pub author: String,
    /// Who approved it.
    pub approved_by: String,
    /// Why the base assumption is being overridden.
    pub reason: String,
    /// Targeted profile.
    pub profile: PolicyProfile,
    /// Targeted tier.
    pub tier: RiskTier,
    /// Targeted action.
    pub action: MigrationDecision,
    /// Targeted state.
    pub state: OutcomeState,
    /// The new loss value.
    pub new_loss: f64,
}

/// A recorded override that was actually applied during compilation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedOverride {
    /// Artifact id that authorized the change.
    pub artifact_id: String,
    /// Approver recorded for audit.
    pub approved_by: String,
    /// Targeted action.
    pub action: MigrationDecision,
    /// Targeted state.
    pub state: OutcomeState,
    /// The base loss before the override.
    pub base_loss: f64,
    /// The new loss after the override.
    pub new_loss: f64,
}

// ── Compiled Policy ──────────────────────────────────────────────────────────

/// One action's compiled losses, indexed by [`OutcomeState::order_index`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledActionRow {
    /// The action.
    pub action: MigrationDecision,
    /// Losses indexed by canonical state order.
    pub losses: Vec<f64>,
}

/// A validated, checksummed loss matrix for one (profile, tier) cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledLossMatrix {
    /// Policy profile.
    pub profile: PolicyProfile,
    /// Risk tier.
    pub tier: RiskTier,
    /// State columns, in canonical order.
    pub states: Vec<OutcomeState>,
    /// Action rows, in canonical order.
    pub rows: Vec<CompiledActionRow>,
    /// SHA-256 of the canonical base spec (pre-override).
    pub base_checksum: String,
    /// SHA-256 of the effective matrix (post-override).
    pub effective_checksum: String,
    /// Overrides applied to this matrix, sorted canonically.
    pub applied_overrides: Vec<AppliedOverride>,
}

impl CompiledLossMatrix {
    /// Loss of `action` in `state`.
    #[must_use]
    pub fn loss_of(&self, action: MigrationDecision, state: OutcomeState) -> f64 {
        self.rows[action_index(action)].losses[state.order_index()]
    }

    /// A clone with a single cell replaced (used by the sensitivity simulator).
    #[must_use]
    fn with_cell(&self, action: MigrationDecision, state: OutcomeState, loss: f64) -> Self {
        let mut clone = self.clone();
        clone.rows[action_index(action)].losses[state.order_index()] = loss;
        clone
    }
}

/// A fully compiled decision-loss policy: a checksummed registry of matrices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledLossPolicy {
    /// Schema version.
    pub schema_version: String,
    /// Policy id from the manifest.
    pub policy_id: String,
    /// Policy version from the manifest.
    pub policy_version: String,
    /// Compiled matrices, sorted by (profile, tier).
    pub matrices: Vec<CompiledLossMatrix>,
    /// SHA-256 over the canonical base manifest.
    pub base_checksum: String,
    /// SHA-256 over the effective (post-override) policy.
    pub effective_checksum: String,
}

impl CompiledLossPolicy {
    /// The compiled matrix for a (profile, tier) cell.
    ///
    /// # Errors
    ///
    /// Returns [`DecisionLossError::MissingEntry`] when no matrix governs the
    /// requested cell.
    pub fn matrix_for(
        &self,
        profile: PolicyProfile,
        tier: RiskTier,
    ) -> Result<&CompiledLossMatrix> {
        self.matrices
            .iter()
            .find(|m| m.profile == profile && m.tier == tier)
            .ok_or(DecisionLossError::MissingEntry {
                profile: profile.as_str(),
                tier: tier.as_str(),
            })
    }

    /// Pretty-printed JSON of the compiled policy.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|error| error.to_string())
    }
}

// ── Compiler ─────────────────────────────────────────────────────────────────

/// Compile a manifest into a [`CompiledLossPolicy`], applying any override
/// artifacts and validating every matrix.
///
/// # Errors
///
/// Returns a [`DecisionLossError`] when the manifest is empty, has duplicate or
/// malformed matrices, violates a dominance invariant, or references an override
/// target that does not exist.
pub fn compile(
    manifest: &LossPolicyManifest,
    overrides: &[LossOverrideArtifact],
) -> Result<CompiledLossPolicy> {
    if manifest.matrices.is_empty() {
        return Err(DecisionLossError::EmptyManifest);
    }

    // Reject duplicate (profile, tier) keys.
    let mut seen: BTreeSet<(usize, usize)> = BTreeSet::new();
    for spec in &manifest.matrices {
        let key = (profile_ord(spec.profile), tier_ord(spec.tier));
        if !seen.insert(key) {
            return Err(DecisionLossError::DuplicateEntry {
                profile: spec.profile.as_str(),
                tier: spec.tier.as_str(),
            });
        }
    }

    // Validate every override actually targets a declared matrix cell.
    for ov in overrides {
        if !ov.new_loss.is_finite() || ov.new_loss < 0.0 {
            return Err(DecisionLossError::InvalidOverrideLoss {
                artifact_id: ov.artifact_id.clone(),
                loss: ov.new_loss,
            });
        }
        let matrix_present = manifest
            .matrices
            .iter()
            .any(|m| m.profile == ov.profile && m.tier == ov.tier);
        if !matrix_present {
            return Err(DecisionLossError::OverrideMatrixNotFound {
                artifact_id: ov.artifact_id.clone(),
                profile: ov.profile.as_str(),
                tier: ov.tier.as_str(),
            });
        }
    }

    // Compile each spec.
    let mut matrices = Vec::with_capacity(manifest.matrices.len());
    for spec in &manifest.matrices {
        let matrix_overrides: Vec<&LossOverrideArtifact> = overrides
            .iter()
            .filter(|ov| ov.profile == spec.profile && ov.tier == spec.tier)
            .collect();
        matrices.push(compile_matrix(spec, &matrix_overrides)?);
    }

    // Sort matrices canonically for determinism.
    matrices.sort_by(|a, b| {
        profile_ord(a.profile)
            .cmp(&profile_ord(b.profile))
            .then_with(|| tier_ord(a.tier).cmp(&tier_ord(b.tier)))
    });

    let base_checksum = stable_hash(&CanonicalManifest {
        schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION,
        policy_id: &manifest.policy_id,
        policy_version: &manifest.policy_version,
        base_checksums: matrices.iter().map(|m| m.base_checksum.as_str()).collect(),
    });
    let effective_checksum = stable_hash(&CanonicalManifest {
        schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION,
        policy_id: &manifest.policy_id,
        policy_version: &manifest.policy_version,
        base_checksums: matrices
            .iter()
            .map(|m| m.effective_checksum.as_str())
            .collect(),
    });

    Ok(CompiledLossPolicy {
        schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
        policy_id: manifest.policy_id.clone(),
        policy_version: manifest.policy_version.clone(),
        matrices,
        base_checksum,
        effective_checksum,
    })
}

#[derive(Serialize)]
struct CanonicalManifest<'a> {
    schema_version: &'a str,
    policy_id: &'a str,
    policy_version: &'a str,
    base_checksums: Vec<&'a str>,
}

fn compile_matrix(
    spec: &LossMatrixSpec,
    overrides: &[&LossOverrideArtifact],
) -> Result<CompiledLossMatrix> {
    // Build a dense loss grid, checking coverage / duplicates / validity.
    let n_states = OutcomeState::ALL.len();
    let mut grid: Vec<Option<f64>> = vec![None; ALL_ACTIONS.len() * n_states];
    for cell in &spec.cells {
        let idx = action_index(cell.action) * n_states + cell.state.order_index();
        if grid[idx].is_some() {
            return Err(DecisionLossError::DuplicateCell {
                profile: spec.profile.as_str(),
                tier: spec.tier.as_str(),
                action: action_str(cell.action),
                state: cell.state.as_str(),
            });
        }
        if !cell.loss.is_finite() || cell.loss < 0.0 {
            return Err(DecisionLossError::InvalidLoss {
                profile: spec.profile.as_str(),
                tier: spec.tier.as_str(),
                action: action_str(cell.action),
                state: cell.state.as_str(),
                loss: cell.loss,
            });
        }
        grid[idx] = Some(cell.loss);
    }

    // Every (action, state) must be present.
    for action in ALL_ACTIONS {
        for state in OutcomeState::ALL {
            let idx = action_index(action) * n_states + state.order_index();
            if grid[idx].is_none() {
                return Err(DecisionLossError::MissingCell {
                    profile: spec.profile.as_str(),
                    tier: spec.tier.as_str(),
                    action: action_str(action),
                    state: state.as_str(),
                });
            }
        }
    }

    let base_rows: Vec<CompiledActionRow> = ALL_ACTIONS
        .iter()
        .copied()
        .map(|action| {
            let base = action_index(action) * n_states;
            CompiledActionRow {
                action,
                losses: (0..n_states)
                    .map(|s| grid[base + s].unwrap_or(0.0))
                    .collect(),
            }
        })
        .collect();
    let base_checksum = stable_hash(&CanonicalMatrix {
        profile: spec.profile.as_str(),
        tier: spec.tier.as_str(),
        rows: &base_rows,
    });

    // Apply overrides (canonical order) and record provenance.
    let mut rows = base_rows.clone();
    let mut sorted_overrides: Vec<&LossOverrideArtifact> = overrides.to_vec();
    sorted_overrides.sort_by(|a, b| {
        action_index(a.action)
            .cmp(&action_index(b.action))
            .then_with(|| a.state.order_index().cmp(&b.state.order_index()))
            .then_with(|| a.artifact_id.cmp(&b.artifact_id))
    });
    let mut applied = Vec::with_capacity(sorted_overrides.len());
    for ov in sorted_overrides {
        let slot = &mut rows[action_index(ov.action)].losses[ov.state.order_index()];
        let base_loss = *slot;
        *slot = ov.new_loss;
        applied.push(AppliedOverride {
            artifact_id: ov.artifact_id.clone(),
            approved_by: ov.approved_by.clone(),
            action: ov.action,
            state: ov.state,
            base_loss,
            new_loss: ov.new_loss,
        });
    }

    let effective_checksum = stable_hash(&CanonicalMatrix {
        profile: spec.profile.as_str(),
        tier: spec.tier.as_str(),
        rows: &rows,
    });

    let matrix = CompiledLossMatrix {
        profile: spec.profile,
        tier: spec.tier,
        states: OutcomeState::ALL.to_vec(),
        rows,
        base_checksum,
        effective_checksum,
        applied_overrides: applied,
    };
    validate_dominance(&matrix)?;
    Ok(matrix)
}

#[derive(Serialize)]
struct CanonicalMatrix<'a> {
    profile: &'a str,
    tier: &'a str,
    rows: &'a [CompiledActionRow],
}

/// Two defensible dominance invariants every sane loss matrix must honor.
fn validate_dominance(matrix: &CompiledLossMatrix) -> Result<()> {
    let accept_broken = matrix.loss_of(MigrationDecision::AutoApprove, OutcomeState::Broken);
    let accept_faithful = matrix.loss_of(MigrationDecision::AutoApprove, OutcomeState::Faithful);
    let reject_broken = matrix.loss_of(MigrationDecision::Reject, OutcomeState::Broken);

    if accept_broken <= accept_faithful {
        return Err(DecisionLossError::DominanceViolation {
            profile: matrix.profile.as_str(),
            tier: matrix.tier.as_str(),
            detail: format!(
                "accepting a broken migration ({accept_broken}) must cost strictly more than accepting a faithful one ({accept_faithful})"
            ),
        });
    }
    if accept_broken < reject_broken {
        return Err(DecisionLossError::DominanceViolation {
            profile: matrix.profile.as_str(),
            tier: matrix.tier.as_str(),
            detail: format!(
                "accepting a broken migration ({accept_broken}) must cost at least as much as rejecting it ({reject_broken})"
            ),
        });
    }
    Ok(())
}

// ── Solver (AC1) ─────────────────────────────────────────────────────────────

/// Solver tuning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SolverConfig {
    /// Expected losses within this absolute tolerance are treated as tied and
    /// resolved by the deterministic tie-break.
    pub tie_epsilon: f64,
    /// If true, an unnormalized input distribution is renormalized rather than
    /// rejected.
    pub renormalize_distribution: bool,
}

impl Default for SolverConfig {
    fn default() -> Self {
        Self {
            tie_epsilon: 1e-9,
            renormalize_distribution: true,
        }
    }
}

/// Per-action expected and worst-case loss.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionExpectedLoss {
    /// The action.
    pub action: MigrationDecision,
    /// Expected loss over the state distribution.
    pub expected_loss: f64,
    /// Worst-case loss over states with positive mass (minimax tie-break key).
    pub worst_case_loss: f64,
}

/// The result of an expected-loss solve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionDecision {
    /// The selected `argmin`-expected-loss action.
    pub selected: MigrationDecision,
    /// Every action's expected/worst-case loss, in canonical action order.
    pub per_action: Vec<ActionExpectedLoss>,
    /// The minimum expected loss.
    pub min_expected_loss: f64,
    /// The next-best distinct action, if any.
    pub runner_up: Option<MigrationDecision>,
    /// `runner_up.expected_loss - min_expected_loss` (`0.0` if no runner-up).
    pub margin: f64,
    /// Whether the tie-break decided between epsilon-equal expected losses.
    pub tie_break_applied: bool,
    /// All actions that were within `tie_epsilon` of the minimum.
    pub tie_candidates: Vec<MigrationDecision>,
    /// Human-readable rationale.
    pub rationale: String,
}

/// Solve for the `argmin`-expected-loss action over a state distribution.
///
/// Action selection is `argmin_a Σ_s P(s)·loss(a, s)`. Ties (expected losses
/// within `tie_epsilon`) are resolved lexicographically: smaller worst-case
/// loss first (minimax), then more conservative action (lower
/// [`conservatism_rank`]), then canonical action order — fully deterministic.
///
/// # Errors
///
/// Returns a [`DecisionLossError`] when the distribution is malformed or (with
/// renormalization off) does not sum to one.
pub fn solve_expected_loss(
    matrix: &CompiledLossMatrix,
    distribution: &StateDistribution,
    config: &SolverConfig,
) -> Result<ActionDecision> {
    let dist = distribution.validated(config)?;

    let per_action: Vec<ActionExpectedLoss> = ALL_ACTIONS
        .iter()
        .copied()
        .map(|action| {
            let mut expected = 0.0;
            let mut worst = f64::NEG_INFINITY;
            for entry in &dist.entries {
                let loss = matrix.loss_of(action, entry.state);
                expected += entry.probability * loss;
                if loss > worst {
                    worst = loss;
                }
            }
            ActionExpectedLoss {
                action,
                expected_loss: expected,
                worst_case_loss: if worst.is_finite() { worst } else { 0.0 },
            }
        })
        .collect();

    // Minimum expected loss.
    let min_expected = per_action
        .iter()
        .map(|a| a.expected_loss)
        .fold(f64::INFINITY, f64::min);

    // Tie candidates (within epsilon of the minimum).
    let mut tie_candidates: Vec<&ActionExpectedLoss> = per_action
        .iter()
        .filter(|a| (a.expected_loss - min_expected).abs() <= config.tie_epsilon)
        .collect();
    // Deterministic order: worst-case, then conservatism, then canonical index.
    tie_candidates.sort_by(|a, b| {
        a.worst_case_loss
            .total_cmp(&b.worst_case_loss)
            .then_with(|| conservatism_rank(a.action).cmp(&conservatism_rank(b.action)))
            .then_with(|| action_index(a.action).cmp(&action_index(b.action)))
    });
    let selected = tie_candidates
        .first()
        .map_or(MigrationDecision::ConservativeFallback, |a| a.action);
    let tie_break_applied = tie_candidates.len() > 1;
    // Materialize the candidate actions before `per_action` is moved below.
    let tie_candidate_actions: Vec<MigrationDecision> =
        tie_candidates.iter().map(|a| a.action).collect();

    // Runner-up: the cheapest action strictly worse than the selected one.
    let runner_up_entry = per_action
        .iter()
        .filter(|a| a.action != selected && a.expected_loss > min_expected + config.tie_epsilon)
        .min_by(|a, b| a.expected_loss.total_cmp(&b.expected_loss));
    let runner_up = runner_up_entry.map(|a| a.action);
    let margin = runner_up_entry.map_or(0.0, |a| a.expected_loss - min_expected);

    let rationale = format!(
        "argmin expected loss: {} (EL={:.4}); runner_up={}; margin={:.4}; tie_break={}",
        action_str(selected),
        min_expected,
        runner_up.map_or("none", action_str),
        margin,
        tie_break_applied,
    );

    Ok(ActionDecision {
        selected,
        per_action,
        min_expected_loss: min_expected,
        runner_up,
        margin,
        tie_break_applied,
        tie_candidates: tie_candidate_actions,
        rationale,
    })
}

// ── Decision Log (AC1) ───────────────────────────────────────────────────────

/// A structured, per-decision audit log entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionLogEntry {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic decision id (SHA-256 of the inputs + result).
    pub decision_id: String,
    /// Claim this decision pertains to.
    pub claim_id: String,
    /// Policy profile used.
    pub profile: PolicyProfile,
    /// Risk tier used.
    pub tier: RiskTier,
    /// Policy id from the compiled policy.
    pub policy_id: String,
    /// Policy version from the compiled policy.
    pub policy_version: String,
    /// Effective (post-override) checksum of the matrix used.
    pub matrix_effective_checksum: String,
    /// The state distribution the decision was computed over.
    pub state_distribution: StateDistribution,
    /// The expected-loss solve result.
    pub decision: ActionDecision,
    /// The posterior acceptance probability, when derived from a posterior.
    pub posterior_prob: Option<f64>,
    /// Deterministic replay command.
    pub replay_command: String,
}

impl DecisionLogEntry {
    /// The selected action.
    #[must_use]
    pub fn selected(&self) -> MigrationDecision {
        self.decision.selected
    }

    /// Pretty-printed JSON of the log entry.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|error| error.to_string())
    }
}

// ── Sensitivity Simulator (AC3) ──────────────────────────────────────────────

/// How to perturb loss cells during sensitivity analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerturbationConfig {
    /// Relative multipliers applied to each cell (e.g. `-0.5` ⇒ ×0.5,
    /// `0.5` ⇒ ×1.5). `0.0` is ignored.
    pub relative_steps: Vec<f64>,
    /// Clamp perturbed losses to be non-negative.
    pub clamp_nonnegative: bool,
}

impl Default for PerturbationConfig {
    fn default() -> Self {
        Self {
            relative_steps: vec![-0.5, -0.25, 0.25, 0.5],
            clamp_nonnegative: true,
        }
    }
}

/// A counterfactual: a single-cell perturbation that flips the decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CounterfactualDecisionDiff {
    /// The perturbed cell's action.
    pub action: MigrationDecision,
    /// The perturbed cell's state.
    pub state: OutcomeState,
    /// The relative delta applied (`new = base · (1 + relative_delta)`).
    pub relative_delta: f64,
    /// The cell's loss before perturbation.
    pub baseline_loss: f64,
    /// The cell's loss after perturbation.
    pub perturbed_loss: f64,
    /// The baseline (unperturbed) decision.
    pub baseline_decision: MigrationDecision,
    /// The decision after perturbation.
    pub flipped_decision: MigrationDecision,
    /// The baseline decision margin.
    pub margin_before: f64,
    /// The perturbed decision margin.
    pub margin_after: f64,
}

/// JSON-stats artifact (path / checksum / content), matching the crate idiom.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonStatsArtifact {
    /// Suggested artifact path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// The serialized JSON content.
    pub content: String,
}

/// The result of a sensitivity sweep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensitivityReport {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Profile analyzed.
    pub profile: PolicyProfile,
    /// Tier analyzed.
    pub tier: RiskTier,
    /// The baseline decision before any perturbation.
    pub baseline_decision: MigrationDecision,
    /// The baseline decision margin.
    pub baseline_margin: f64,
    /// Number of (cell × step) perturbations evaluated.
    pub perturbations_evaluated: usize,
    /// The perturbations that flipped the decision, sorted canonically.
    pub flips: Vec<CounterfactualDecisionDiff>,
    /// Fraction of perturbations that did **not** flip the decision (`[0, 1]`).
    pub stability: f64,
    /// The cell with the most flips, if any.
    pub most_sensitive_cell: Option<SensitiveCell>,
    /// The smallest `|relative_delta|` that flips the decision, if any.
    pub min_flip_relative_distance: Option<f64>,
    /// Exported JSON-stats artifact.
    pub exported_json_stats: JsonStatsArtifact,
    /// Deterministic replay command.
    pub replay_command: String,
    /// Evidence checksum over the flips + summary.
    pub evidence_checksum: String,
}

/// Identifies the most sensitive loss cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensitiveCell {
    /// Cell action.
    pub action: MigrationDecision,
    /// Cell state.
    pub state: OutcomeState,
    /// Number of perturbation steps on this cell that flipped the decision.
    pub flip_count: usize,
}

/// Run a single-cell sensitivity sweep, returning every counterfactual that
/// flips the decision.
///
/// # Errors
///
/// Returns a [`DecisionLossError`] when the baseline solve or any perturbed
/// solve fails (e.g. a malformed distribution).
pub fn simulate_sensitivity(
    matrix: &CompiledLossMatrix,
    distribution: &StateDistribution,
    solver: &SolverConfig,
    perturbation: &PerturbationConfig,
) -> Result<SensitivityReport> {
    let baseline = solve_expected_loss(matrix, distribution, solver)?;

    let mut flips: Vec<CounterfactualDecisionDiff> = Vec::new();
    let mut flip_counts: Vec<usize> = vec![0; ALL_ACTIONS.len() * OutcomeState::ALL.len()];
    let mut evaluated = 0usize;

    for action in ALL_ACTIONS {
        for state in OutcomeState::ALL {
            let base_loss = matrix.loss_of(action, state);
            for &step in &perturbation.relative_steps {
                if step == 0.0 {
                    continue;
                }
                evaluated += 1;
                let mut perturbed_loss = base_loss * (1.0 + step);
                if perturbation.clamp_nonnegative && perturbed_loss < 0.0 {
                    perturbed_loss = 0.0;
                }
                let perturbed_matrix = matrix.with_cell(action, state, perturbed_loss);
                let perturbed = solve_expected_loss(&perturbed_matrix, distribution, solver)?;
                if perturbed.selected != baseline.selected {
                    let cell_idx =
                        action_index(action) * OutcomeState::ALL.len() + state.order_index();
                    flip_counts[cell_idx] += 1;
                    flips.push(CounterfactualDecisionDiff {
                        action,
                        state,
                        relative_delta: step,
                        baseline_loss: base_loss,
                        perturbed_loss,
                        baseline_decision: baseline.selected,
                        flipped_decision: perturbed.selected,
                        margin_before: baseline.margin,
                        margin_after: perturbed.margin,
                    });
                }
            }
        }
    }

    // Canonical flip order: cell (action, state), then |delta|, then signed delta.
    flips.sort_by(|a, b| {
        action_index(a.action)
            .cmp(&action_index(b.action))
            .then_with(|| a.state.order_index().cmp(&b.state.order_index()))
            .then_with(|| a.relative_delta.abs().total_cmp(&b.relative_delta.abs()))
            .then_with(|| a.relative_delta.total_cmp(&b.relative_delta))
    });

    let stability = if evaluated == 0 {
        1.0
    } else {
        1.0 - (flips.len() as f64) / (evaluated as f64)
    };

    // Most sensitive cell: highest flip count, canonical tie-break.
    let mut most_sensitive_cell: Option<SensitiveCell> = None;
    for action in ALL_ACTIONS {
        for state in OutcomeState::ALL {
            let idx = action_index(action) * OutcomeState::ALL.len() + state.order_index();
            let count = flip_counts[idx];
            if count > 0
                && most_sensitive_cell
                    .as_ref()
                    .is_none_or(|c| count > c.flip_count)
            {
                most_sensitive_cell = Some(SensitiveCell {
                    action,
                    state,
                    flip_count: count,
                });
            }
        }
    }

    let min_flip_relative_distance = flips
        .iter()
        .map(|f| f.relative_delta.abs())
        .min_by(f64::total_cmp);

    let report_id = format!(
        "decision-loss-sensitivity-{}",
        short_hash(&stable_hash(&CanonicalSensitivityId {
            schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION,
            matrix_effective_checksum: &matrix.effective_checksum,
            distribution,
            relative_steps: &perturbation.relative_steps,
        }))
    );
    let evidence_checksum = stable_hash(&CanonicalSensitivityEvidence {
        baseline_decision: action_str(baseline.selected),
        baseline_margin: baseline.margin,
        flips: &flips,
        stability,
    });
    let replay_command = format!(
        "cargo test -p doctor_frankentui --lib decision_loss_policy -- --nocapture # report {report_id}"
    );
    let exported_json_stats = export_sensitivity_json(
        &report_id,
        matrix.profile,
        matrix.tier,
        baseline.selected,
        &flips,
        stability,
    );

    Ok(SensitivityReport {
        schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
        report_id,
        profile: matrix.profile,
        tier: matrix.tier,
        baseline_decision: baseline.selected,
        baseline_margin: baseline.margin,
        perturbations_evaluated: evaluated,
        flips,
        stability,
        most_sensitive_cell,
        min_flip_relative_distance,
        exported_json_stats,
        replay_command,
        evidence_checksum,
    })
}

#[derive(Serialize)]
struct CanonicalSensitivityId<'a> {
    schema_version: &'a str,
    matrix_effective_checksum: &'a str,
    distribution: &'a StateDistribution,
    relative_steps: &'a [f64],
}

#[derive(Serialize)]
struct CanonicalSensitivityEvidence<'a> {
    baseline_decision: &'a str,
    baseline_margin: f64,
    flips: &'a [CounterfactualDecisionDiff],
    stability: f64,
}

fn export_sensitivity_json(
    report_id: &str,
    profile: PolicyProfile,
    tier: RiskTier,
    baseline_decision: MigrationDecision,
    flips: &[CounterfactualDecisionDiff],
    stability: f64,
) -> JsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        profile: &'a str,
        tier: &'a str,
        baseline_decision: &'a str,
        stability: f64,
        flip_count: usize,
        flips: &'a [CounterfactualDecisionDiff],
    }
    let payload = Export {
        schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION,
        report_id,
        profile: profile.as_str(),
        tier: tier.as_str(),
        baseline_decision: action_str(baseline_decision),
        stability,
        flip_count: flips.len(),
        flips,
    };
    let content = serde_json::to_string_pretty(&payload).unwrap_or_else(|error| error.to_string());
    JsonStatsArtifact {
        path: format!("{report_id}/decision_loss_sensitivity_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// The decision-loss policy engine: a compiled policy plus solver + posterior
/// mapping config. This is the operator-facing entry point that produces
/// auditable [`DecisionLogEntry`]s and [`SensitivityReport`]s.
#[derive(Debug, Clone)]
pub struct DecisionPolicyEngine {
    policy: CompiledLossPolicy,
    solver: SolverConfig,
    state_mapping: StateMapping,
}

impl DecisionPolicyEngine {
    /// Build an engine from a compiled policy (default solver + binary mapping).
    #[must_use]
    pub fn new(policy: CompiledLossPolicy) -> Self {
        Self {
            policy,
            solver: SolverConfig::default(),
            state_mapping: StateMapping::default(),
        }
    }

    /// Override the solver config.
    #[must_use]
    pub fn with_solver(mut self, solver: SolverConfig) -> Self {
        self.solver = solver;
        self
    }

    /// Override the posterior → distribution mapping.
    #[must_use]
    pub fn with_state_mapping(mut self, mapping: StateMapping) -> Self {
        self.state_mapping = mapping;
        self
    }

    /// Borrow the compiled policy.
    #[must_use]
    pub fn policy(&self) -> &CompiledLossPolicy {
        &self.policy
    }

    /// Borrow the solver config.
    #[must_use]
    pub fn solver(&self) -> &SolverConfig {
        &self.solver
    }

    /// Decide over an explicit state distribution, returning an audit log entry.
    ///
    /// # Errors
    ///
    /// Returns a [`DecisionLossError`] when no matrix governs (profile, tier) or
    /// the solve fails.
    pub fn decide(
        &self,
        claim_id: impl Into<String>,
        profile: PolicyProfile,
        tier: RiskTier,
        distribution: &StateDistribution,
    ) -> Result<DecisionLogEntry> {
        self.decide_inner(claim_id.into(), profile, tier, distribution, None)
    }

    /// Decide directly from a posterior record using the engine's state mapping.
    ///
    /// # Errors
    ///
    /// Returns a [`DecisionLossError`] when the posterior is out of range, no
    /// matrix governs (profile, tier), or the solve fails.
    pub fn decide_from_posterior(
        &self,
        record: &PosteriorRecord,
        profile: PolicyProfile,
        tier: RiskTier,
    ) -> Result<DecisionLogEntry> {
        let distribution = StateDistribution::from_posterior(record, self.state_mapping)?;
        self.decide_inner(
            record.claim_id.clone(),
            profile,
            tier,
            &distribution,
            Some(record.posterior_prob),
        )
    }

    fn decide_inner(
        &self,
        claim_id: String,
        profile: PolicyProfile,
        tier: RiskTier,
        distribution: &StateDistribution,
        posterior_prob: Option<f64>,
    ) -> Result<DecisionLogEntry> {
        let matrix = self.policy.matrix_for(profile, tier)?;
        let decision = solve_expected_loss(matrix, distribution, &self.solver)?;
        let decision_id = format!(
            "decision-loss-{}",
            short_hash(&stable_hash(&CanonicalDecisionId {
                schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION,
                claim_id: &claim_id,
                profile: profile.as_str(),
                tier: tier.as_str(),
                matrix_effective_checksum: &matrix.effective_checksum,
                selected: action_str(decision.selected),
                distribution,
            }))
        );
        let replay_command = format!(
            "cargo test -p doctor_frankentui --lib decision_loss_policy -- --nocapture # decision {decision_id}"
        );
        Ok(DecisionLogEntry {
            schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
            decision_id,
            claim_id,
            profile,
            tier,
            policy_id: self.policy.policy_id.clone(),
            policy_version: self.policy.policy_version.clone(),
            matrix_effective_checksum: matrix.effective_checksum.clone(),
            state_distribution: distribution.clone(),
            decision,
            posterior_prob,
            replay_command,
        })
    }

    /// Run a sensitivity sweep over the (profile, tier) matrix for a distribution.
    ///
    /// # Errors
    ///
    /// Returns a [`DecisionLossError`] when no matrix governs (profile, tier) or
    /// a solve fails.
    pub fn simulate(
        &self,
        profile: PolicyProfile,
        tier: RiskTier,
        distribution: &StateDistribution,
        perturbation: &PerturbationConfig,
    ) -> Result<SensitivityReport> {
        let matrix = self.policy.matrix_for(profile, tier)?;
        simulate_sensitivity(matrix, distribution, &self.solver, perturbation)
    }
}

#[derive(Serialize)]
struct CanonicalDecisionId<'a> {
    schema_version: &'a str,
    claim_id: &'a str,
    profile: &'a str,
    tier: &'a str,
    matrix_effective_checksum: &'a str,
    selected: &'a str,
    distribution: &'a StateDistribution,
}

// ── Ordering helpers ─────────────────────────────────────────────────────────

#[must_use]
fn profile_ord(profile: PolicyProfile) -> usize {
    match profile {
        PolicyProfile::Conservative => 0,
        PolicyProfile::Balanced => 1,
        PolicyProfile::Aggressive => 2,
    }
}

#[must_use]
fn tier_ord(tier: RiskTier) -> usize {
    match tier {
        RiskTier::Critical => 0,
        RiskTier::High => 1,
        RiskTier::Medium => 2,
        RiskTier::Low => 3,
    }
}

// ── Hashing helpers ──────────────────────────────────────────────────────────

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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::posterior_core::{ChannelEvidence, EvidenceChannel, PosteriorEngine};

    // ── Fixtures ─────────────────────────────────────────────────────────────

    fn standard_policy() -> CompiledLossPolicy {
        compile(
            &LossPolicyManifest::standard("opentui-import", "1.0.0"),
            &[],
        )
        .expect("standard manifest compiles")
    }

    fn matrix(policy: &CompiledLossPolicy, p: PolicyProfile, t: RiskTier) -> &CompiledLossMatrix {
        policy.matrix_for(p, t).expect("matrix present")
    }

    fn dist(faithful: f64, broken: f64) -> StateDistribution {
        StateDistribution::from_pairs([
            (OutcomeState::Faithful, faithful),
            (OutcomeState::Broken, broken),
        ])
        .expect("valid distribution")
    }

    /// A custom matrix where one action is forced to be cheapest in some state.
    fn custom_spec(
        profile: PolicyProfile,
        tier: RiskTier,
        mutate: impl Fn(&mut LossCellSpec),
    ) -> LossMatrixSpec {
        let mut spec = LossMatrixSpec::calibrated(profile, tier);
        for cell in &mut spec.cells {
            mutate(cell);
        }
        spec
    }

    fn posterior(claim: &str, accept_prob: f64) -> PosteriorRecord {
        // Drive the real posterior engine so we consume the genuine type.
        let engine = PosteriorEngine::default();
        // Translate a target accept probability into a strong single-channel signal.
        let log_bf = if accept_prob >= 0.999 {
            8.0
        } else if accept_prob <= 0.001 {
            -8.0
        } else {
            (accept_prob / (1.0 - accept_prob)).ln()
        };
        let evidence: Vec<ChannelEvidence> = EvidenceChannel::ALL
            .iter()
            .map(|ch| ChannelEvidence::new(*ch, log_bf).claim(claim))
            .collect();
        engine.infer(claim, &evidence)
    }

    // ── Compiler ─────────────────────────────────────────────────────────────

    #[test]
    fn standard_manifest_covers_all_twelve_cells() {
        let policy = standard_policy();
        assert_eq!(policy.matrices.len(), 12);
        for profile in PolicyProfile::ALL {
            for tier in RiskTier::ALL {
                let m = matrix(&policy, profile, tier);
                assert_eq!(m.rows.len(), 6, "six action rows");
                assert_eq!(m.states.len(), 4, "four state columns");
                for row in &m.rows {
                    assert_eq!(row.losses.len(), 4);
                    assert!(row.losses.iter().all(|l| l.is_finite() && *l >= 0.0));
                }
            }
        }
    }

    #[test]
    fn empty_manifest_is_rejected() {
        let manifest = LossPolicyManifest {
            schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
            policy_id: "x".into(),
            policy_version: "1".into(),
            matrices: vec![],
        };
        assert_eq!(
            compile(&manifest, &[]),
            Err(DecisionLossError::EmptyManifest)
        );
    }

    #[test]
    fn duplicate_matrix_key_is_rejected() {
        let mut manifest = LossPolicyManifest::standard("x", "1");
        manifest.matrices.push(LossMatrixSpec::calibrated(
            PolicyProfile::Balanced,
            RiskTier::Low,
        ));
        let err = compile(&manifest, &[]).unwrap_err();
        assert!(matches!(err, DecisionLossError::DuplicateEntry { .. }));
    }

    #[test]
    fn missing_cell_is_rejected() {
        let mut spec = LossMatrixSpec::calibrated(PolicyProfile::Balanced, RiskTier::Low);
        spec.cells.pop(); // drop one (action, state) cell
        let manifest = LossPolicyManifest {
            schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
            policy_id: "x".into(),
            policy_version: "1".into(),
            matrices: vec![spec],
        };
        let err = compile(&manifest, &[]).unwrap_err();
        assert!(matches!(err, DecisionLossError::MissingCell { .. }));
    }

    #[test]
    fn duplicate_cell_is_rejected() {
        let mut spec = LossMatrixSpec::calibrated(PolicyProfile::Balanced, RiskTier::Low);
        let dup = spec.cells[0].clone();
        spec.cells.push(dup);
        let manifest = LossPolicyManifest {
            schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
            policy_id: "x".into(),
            policy_version: "1".into(),
            matrices: vec![spec],
        };
        let err = compile(&manifest, &[]).unwrap_err();
        assert!(matches!(err, DecisionLossError::DuplicateCell { .. }));
    }

    #[test]
    fn negative_loss_is_rejected() {
        let spec = custom_spec(PolicyProfile::Balanced, RiskTier::Low, |cell| {
            if cell.action == MigrationDecision::Reject && cell.state == OutcomeState::Faithful {
                cell.loss = -1.0;
            }
        });
        let manifest = LossPolicyManifest {
            schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
            policy_id: "x".into(),
            policy_version: "1".into(),
            matrices: vec![spec],
        };
        let err = compile(&manifest, &[]).unwrap_err();
        assert!(matches!(err, DecisionLossError::InvalidLoss { .. }));
    }

    #[test]
    fn dominance_violation_is_rejected() {
        // Force accept-broken to be cheaper than accept-faithful.
        let spec = custom_spec(PolicyProfile::Balanced, RiskTier::Low, |cell| {
            if cell.action == MigrationDecision::AutoApprove {
                cell.loss = match cell.state {
                    OutcomeState::Broken => 0.0,
                    OutcomeState::Faithful => 5.0,
                    _ => cell.loss,
                };
            }
        });
        let manifest = LossPolicyManifest {
            schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
            policy_id: "x".into(),
            policy_version: "1".into(),
            matrices: vec![spec],
        };
        let err = compile(&manifest, &[]).unwrap_err();
        assert!(matches!(err, DecisionLossError::DominanceViolation { .. }));
    }

    // ── Solver (AC1: argmin over posterior state distribution) ────────────────

    #[test]
    fn high_confidence_accepts() {
        let policy = standard_policy();
        let m = matrix(&policy, PolicyProfile::Balanced, RiskTier::Medium);
        let decision = solve_expected_loss(m, &dist(0.97, 0.03), &SolverConfig::default()).unwrap();
        assert_eq!(decision.selected, MigrationDecision::AutoApprove);
    }

    #[test]
    fn mid_confidence_reviews() {
        let policy = standard_policy();
        let m = matrix(&policy, PolicyProfile::Balanced, RiskTier::Medium);
        let decision = solve_expected_loss(m, &dist(0.7, 0.3), &SolverConfig::default()).unwrap();
        assert_eq!(decision.selected, MigrationDecision::HumanReview);
    }

    #[test]
    fn very_low_confidence_rejects() {
        let policy = standard_policy();
        let m = matrix(&policy, PolicyProfile::Balanced, RiskTier::Medium);
        // Mostly-broken posterior: outright reject is the cheapest no-ship action.
        let decision = solve_expected_loss(m, &dist(0.1, 0.9), &SolverConfig::default()).unwrap();
        assert_eq!(decision.selected, MigrationDecision::Reject);
    }

    #[test]
    fn max_uncertainty_prefers_conservative_fallback() {
        // This is the key threshold-vs-decision-theory distinction: at maximum
        // uncertainty a threshold rule would reject, but the expected-loss argmin
        // is the conservative fallback — it retains partial value at low friction
        // instead of paying the full opportunity cost of an outright reject.
        let policy = standard_policy();
        let m = matrix(&policy, PolicyProfile::Balanced, RiskTier::Medium);
        let decision = solve_expected_loss(m, &dist(0.5, 0.5), &SolverConfig::default()).unwrap();
        assert_eq!(decision.selected, MigrationDecision::ConservativeFallback);
        // The decision is a true argmin, not a boundary lookup.
        let sel = decision
            .per_action
            .iter()
            .find(|a| a.action == decision.selected)
            .unwrap();
        assert!((sel.expected_loss - decision.min_expected_loss).abs() < 1e-9);
    }

    #[test]
    fn argmin_matches_explicit_expected_loss() {
        let policy = standard_policy();
        let m = matrix(&policy, PolicyProfile::Balanced, RiskTier::High);
        let d = dist(0.8, 0.2);
        let decision = solve_expected_loss(m, &d, &SolverConfig::default()).unwrap();
        // Recompute the minimum independently.
        let mut best = f64::INFINITY;
        for a in ALL_ACTIONS {
            let el = 0.8 * m.loss_of(a, OutcomeState::Faithful)
                + 0.2 * m.loss_of(a, OutcomeState::Broken);
            best = best.min(el);
        }
        assert!((decision.min_expected_loss - best).abs() < 1e-9);
        // Selected action's expected loss equals the minimum.
        let sel = decision
            .per_action
            .iter()
            .find(|a| a.action == decision.selected)
            .unwrap();
        assert!((sel.expected_loss - best).abs() < 1e-9);
    }

    #[test]
    fn conservative_profile_is_stricter_than_aggressive() {
        let policy = standard_policy();
        let d = dist(0.85, 0.15);
        let cons = solve_expected_loss(
            matrix(&policy, PolicyProfile::Conservative, RiskTier::Critical),
            &d,
            &SolverConfig::default(),
        )
        .unwrap();
        let aggr = solve_expected_loss(
            matrix(&policy, PolicyProfile::Aggressive, RiskTier::Critical),
            &d,
            &SolverConfig::default(),
        )
        .unwrap();
        // The conservative profile should never be more permissive (lower
        // conservatism_rank ⇒ more conservative).
        assert!(
            conservatism_rank(cons.selected) <= conservatism_rank(aggr.selected),
            "conservative={:?} aggressive={:?}",
            cons.selected,
            aggr.selected
        );
    }

    #[test]
    fn tie_break_is_deterministic_and_conservative() {
        // Construct a matrix where two actions have identical expected loss.
        let spec = custom_spec(PolicyProfile::Balanced, RiskTier::Low, |cell| {
            // Make Reject and HardReject identical across all states.
            if cell.action == MigrationDecision::HardReject {
                cell.loss = match cell.state {
                    OutcomeState::Faithful => 31.0,
                    OutcomeState::BenignDrift => 25.0,
                    OutcomeState::Regressed => 7.0,
                    OutcomeState::Broken => 1.0,
                };
            }
            if cell.action == MigrationDecision::Reject {
                cell.loss = match cell.state {
                    OutcomeState::Faithful => 31.0,
                    OutcomeState::BenignDrift => 25.0,
                    OutcomeState::Regressed => 7.0,
                    OutcomeState::Broken => 1.0,
                };
            }
        });
        let policy = compile(
            &LossPolicyManifest {
                schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
                policy_id: "tie".into(),
                policy_version: "1".into(),
                matrices: vec![spec],
            },
            &[],
        )
        .unwrap();
        let m = matrix(&policy, PolicyProfile::Balanced, RiskTier::Low);
        // A distribution that makes Reject/HardReject the joint minimum.
        let d = StateDistribution::from_pairs([
            (OutcomeState::Faithful, 0.0),
            (OutcomeState::Broken, 1.0),
        ])
        .unwrap();
        let decision = solve_expected_loss(m, &d, &SolverConfig::default()).unwrap();
        // Both have worst-case 31; HardReject is more conservative ⇒ wins.
        if decision.tie_break_applied {
            assert_eq!(decision.selected, MigrationDecision::HardReject);
        }
        // Determinism: re-solving yields the same selection.
        let again = solve_expected_loss(m, &d, &SolverConfig::default()).unwrap();
        assert_eq!(decision.selected, again.selected);
    }

    #[test]
    fn custom_matrix_can_select_rollback() {
        // Make Rollback cheapest everywhere except keep dominance invariants.
        let spec = custom_spec(PolicyProfile::Balanced, RiskTier::Low, |cell| {
            if cell.action == MigrationDecision::Rollback {
                cell.loss = 0.5;
            }
        });
        let policy = compile(
            &LossPolicyManifest {
                schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
                policy_id: "rb".into(),
                policy_version: "1".into(),
                matrices: vec![spec],
            },
            &[],
        )
        .unwrap();
        let m = matrix(&policy, PolicyProfile::Balanced, RiskTier::Low);
        let decision = solve_expected_loss(m, &dist(0.5, 0.5), &SolverConfig::default()).unwrap();
        assert_eq!(decision.selected, MigrationDecision::Rollback);
    }

    // ── Distribution / posterior plumbing ─────────────────────────────────────

    #[test]
    fn binary_from_posterior_matches_accept_prob() {
        let record = posterior("claim-a", 0.9);
        let d = StateDistribution::binary_from_posterior(&record).unwrap();
        let p = record.posterior_prob;
        assert!((d.probability_of(OutcomeState::Faithful) - p).abs() < 1e-12);
        assert!((d.probability_of(OutcomeState::Regressed) - (1.0 - p)).abs() < 1e-12);
        assert!((d.total_mass() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn tiered_mapping_splits_tail_by_shares() {
        let record = posterior("claim-b", 0.6);
        let p = record.posterior_prob;
        let mapping = StateMapping::Tiered {
            benign_share: 1.0,
            regressed_share: 2.0,
            broken_share: 1.0,
        };
        let d = StateDistribution::from_posterior(&record, mapping).unwrap();
        let tail = 1.0 - p;
        assert!((d.probability_of(OutcomeState::Faithful) - p).abs() < 1e-12);
        assert!((d.probability_of(OutcomeState::BenignDrift) - tail * 0.25).abs() < 1e-9);
        assert!((d.probability_of(OutcomeState::Regressed) - tail * 0.5).abs() < 1e-9);
        assert!((d.probability_of(OutcomeState::Broken) - tail * 0.25).abs() < 1e-9);
        assert!((d.total_mass() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn unnormalized_distribution_rejected_without_renorm() {
        let policy = standard_policy();
        let m = matrix(&policy, PolicyProfile::Balanced, RiskTier::Low);
        let d = StateDistribution::from_pairs([
            (OutcomeState::Faithful, 0.4),
            (OutcomeState::Broken, 0.2),
        ])
        .unwrap();
        let config = SolverConfig {
            tie_epsilon: 1e-9,
            renormalize_distribution: false,
        };
        let err = solve_expected_loss(m, &d, &config).unwrap_err();
        assert!(matches!(
            err,
            DecisionLossError::UnnormalizedDistribution { .. }
        ));
        // With renormalization on it succeeds.
        assert!(solve_expected_loss(m, &d, &SolverConfig::default()).is_ok());
    }

    #[test]
    fn invalid_posterior_probability_rejected() {
        let mut record = posterior("claim-c", 0.5);
        record.posterior_prob = 1.5;
        let err = StateDistribution::binary_from_posterior(&record).unwrap_err();
        assert!(matches!(err, DecisionLossError::InvalidProbability { .. }));
    }

    // ── Overrides (AC2) ───────────────────────────────────────────────────────

    #[test]
    fn override_changes_effective_matrix_and_is_recorded() {
        let manifest = LossPolicyManifest::standard("ov", "1");
        let base = compile(&manifest, &[]).unwrap();
        let base_m = matrix(&base, PolicyProfile::Balanced, RiskTier::Low);
        let base_loss = base_m.loss_of(MigrationDecision::HumanReview, OutcomeState::Faithful);

        let ov = LossOverrideArtifact {
            artifact_id: "CR-42".into(),
            author: "alice".into(),
            approved_by: "bob".into(),
            reason: "review is cheaper for this surface".into(),
            profile: PolicyProfile::Balanced,
            tier: RiskTier::Low,
            action: MigrationDecision::HumanReview,
            state: OutcomeState::Faithful,
            new_loss: base_loss + 17.0,
        };
        let compiled = compile(&manifest, std::slice::from_ref(&ov)).unwrap();
        let m = matrix(&compiled, PolicyProfile::Balanced, RiskTier::Low);

        assert!(
            (m.loss_of(MigrationDecision::HumanReview, OutcomeState::Faithful)
                - (base_loss + 17.0))
                .abs()
                < 1e-9
        );
        assert_ne!(m.base_checksum, m.effective_checksum);
        assert_eq!(m.applied_overrides.len(), 1);
        assert_eq!(m.applied_overrides[0].artifact_id, "CR-42");
        assert_eq!(m.applied_overrides[0].approved_by, "bob");
        assert!((m.applied_overrides[0].base_loss - base_loss).abs() < 1e-9);
        // The base checksum is unchanged by the override (auditability).
        assert_eq!(m.base_checksum, base_m.base_checksum);
    }

    #[test]
    fn override_targeting_unknown_matrix_is_rejected() {
        let mut manifest = LossPolicyManifest::standard("ov", "1");
        // Keep only one matrix so the override target is absent.
        manifest
            .matrices
            .retain(|m| m.profile == PolicyProfile::Balanced && m.tier == RiskTier::Low);
        let ov = LossOverrideArtifact {
            artifact_id: "CR-99".into(),
            author: "a".into(),
            approved_by: "b".into(),
            reason: "x".into(),
            profile: PolicyProfile::Aggressive,
            tier: RiskTier::Critical,
            action: MigrationDecision::AutoApprove,
            state: OutcomeState::Faithful,
            new_loss: 1.0,
        };
        let err = compile(&manifest, &[ov]).unwrap_err();
        assert!(matches!(
            err,
            DecisionLossError::OverrideMatrixNotFound { .. }
        ));
    }

    #[test]
    fn invalid_override_loss_is_rejected() {
        let manifest = LossPolicyManifest::standard("ov", "1");
        let ov = LossOverrideArtifact {
            artifact_id: "CR-1".into(),
            author: "a".into(),
            approved_by: "b".into(),
            reason: "x".into(),
            profile: PolicyProfile::Balanced,
            tier: RiskTier::Low,
            action: MigrationDecision::AutoApprove,
            state: OutcomeState::Faithful,
            new_loss: f64::NAN,
        };
        let err = compile(&manifest, &[ov]).unwrap_err();
        assert!(matches!(err, DecisionLossError::InvalidOverrideLoss { .. }));
    }

    // ── Engine + decision log (AC1) ───────────────────────────────────────────

    #[test]
    fn engine_decision_log_is_populated() {
        let engine = DecisionPolicyEngine::new(standard_policy());
        let record = posterior("claim-x", 0.95);
        let log = engine
            .decide_from_posterior(&record, PolicyProfile::Balanced, RiskTier::Medium)
            .unwrap();
        assert_eq!(log.claim_id, "claim-x");
        assert_eq!(log.profile, PolicyProfile::Balanced);
        assert_eq!(log.tier, RiskTier::Medium);
        assert_eq!(log.selected(), MigrationDecision::AutoApprove);
        assert_eq!(log.decision.per_action.len(), 6);
        assert!(log.posterior_prob.is_some());
        assert!(log.decision_id.starts_with("decision-loss-"));
        assert!(log.replay_command.contains("decision_loss_policy"));
        // The log round-trips through JSON structurally.
        let json = log.to_json();
        let parsed: DecisionLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.selected(), log.selected());
        assert_eq!(parsed.decision_id, log.decision_id);
    }

    #[test]
    fn engine_decision_is_deterministic() {
        let engine = DecisionPolicyEngine::new(standard_policy());
        let record = posterior("claim-det", 0.72);
        let a = engine
            .decide_from_posterior(&record, PolicyProfile::Conservative, RiskTier::High)
            .unwrap();
        let b = engine
            .decide_from_posterior(&record, PolicyProfile::Conservative, RiskTier::High)
            .unwrap();
        assert_eq!(a.decision_id, b.decision_id);
        assert_eq!(a.selected(), b.selected());
        assert_eq!(a.matrix_effective_checksum, b.matrix_effective_checksum);
    }

    #[test]
    fn missing_matrix_entry_errors() {
        // Compile a one-matrix policy and ask for a different cell.
        let spec = LossMatrixSpec::calibrated(PolicyProfile::Balanced, RiskTier::Low);
        let policy = compile(
            &LossPolicyManifest {
                schema_version: DECISION_LOSS_POLICY_SCHEMA_VERSION.to_string(),
                policy_id: "one".into(),
                policy_version: "1".into(),
                matrices: vec![spec],
            },
            &[],
        )
        .unwrap();
        let engine = DecisionPolicyEngine::new(policy);
        let err = engine
            .decide(
                "c",
                PolicyProfile::Aggressive,
                RiskTier::Critical,
                &dist(0.5, 0.5),
            )
            .unwrap_err();
        assert!(matches!(err, DecisionLossError::MissingEntry { .. }));
    }

    // ── Sensitivity (AC3) ─────────────────────────────────────────────────────

    #[test]
    fn sensitivity_finds_flips_near_a_boundary() {
        let policy = standard_policy();
        let m = matrix(&policy, PolicyProfile::Balanced, RiskTier::Medium);
        // A distribution near the accept/review boundary should be flip-prone.
        let d = dist(0.9, 0.1);
        let report = simulate_sensitivity(
            m,
            &d,
            &SolverConfig::default(),
            &PerturbationConfig::default(),
        )
        .unwrap();
        assert!(report.perturbations_evaluated > 0);
        assert!(
            !report.flips.is_empty(),
            "expected at least one flip near boundary"
        );
        assert!(report.stability >= 0.0 && report.stability <= 1.0);
        assert!(report.most_sensitive_cell.is_some());
        assert!(report.min_flip_relative_distance.is_some());
        // Every flip actually changes the decision.
        for flip in &report.flips {
            assert_ne!(flip.baseline_decision, flip.flipped_decision);
        }
    }

    #[test]
    fn deep_confidence_is_stable() {
        let policy = standard_policy();
        let m = matrix(&policy, PolicyProfile::Balanced, RiskTier::Low);
        // Overwhelmingly faithful ⇒ accept decision should be robust.
        let d = dist(0.999, 0.001);
        let report = simulate_sensitivity(
            m,
            &d,
            &SolverConfig::default(),
            &PerturbationConfig::default(),
        )
        .unwrap();
        assert_eq!(report.baseline_decision, MigrationDecision::AutoApprove);
        assert!(report.stability > 0.5, "stability={}", report.stability);
    }

    #[test]
    fn sensitivity_report_is_deterministic() {
        let policy = standard_policy();
        let m = matrix(&policy, PolicyProfile::Conservative, RiskTier::Critical);
        let d = dist(0.8, 0.2);
        let a = simulate_sensitivity(
            m,
            &d,
            &SolverConfig::default(),
            &PerturbationConfig::default(),
        )
        .unwrap();
        let b = simulate_sensitivity(
            m,
            &d,
            &SolverConfig::default(),
            &PerturbationConfig::default(),
        )
        .unwrap();
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.flips.len(), b.flips.len());
        assert_eq!(a.exported_json_stats.sha256, b.exported_json_stats.sha256);
    }

    #[test]
    fn engine_simulate_matches_free_function() {
        let policy = standard_policy();
        let engine = DecisionPolicyEngine::new(policy.clone());
        let d = dist(0.88, 0.12);
        let via_engine = engine
            .simulate(
                PolicyProfile::Balanced,
                RiskTier::High,
                &d,
                &PerturbationConfig::default(),
            )
            .unwrap();
        let m = matrix(&policy, PolicyProfile::Balanced, RiskTier::High);
        let via_free = simulate_sensitivity(
            m,
            &d,
            &SolverConfig::default(),
            &PerturbationConfig::default(),
        )
        .unwrap();
        assert_eq!(via_engine.report_id, via_free.report_id);
        assert_eq!(via_engine.evidence_checksum, via_free.evidence_checksum);
    }

    // ── Determinism of compilation ─────────────────────────────────────────────

    #[test]
    fn compilation_is_deterministic() {
        let manifest = LossPolicyManifest::standard("opentui-import", "2.1.0");
        let a = compile(&manifest, &[]).unwrap();
        let b = compile(&manifest, &[]).unwrap();
        assert_eq!(a.base_checksum, b.base_checksum);
        assert_eq!(a.effective_checksum, b.effective_checksum);
        // Without overrides, base == effective.
        assert_eq!(a.base_checksum, a.effective_checksum);
        for (ma, mb) in a.matrices.iter().zip(&b.matrices) {
            assert_eq!(ma.effective_checksum, mb.effective_checksum);
        }
    }

    #[test]
    fn matrices_are_sorted_canonically() {
        let policy = standard_policy();
        let keys: Vec<(usize, usize)> = policy
            .matrices
            .iter()
            .map(|m| (profile_ord(m.profile), tier_ord(m.tier)))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn action_index_round_trips() {
        for (i, a) in ALL_ACTIONS.iter().enumerate() {
            assert_eq!(action_index(*a), i);
        }
    }
}

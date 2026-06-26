// SPDX-License-Identifier: Apache-2.0
//! Closed-form Bayesian posterior core and Bayes-factor evidence ledger.
//!
//! The migration verdict is inferred in the log-odds (logit) domain: a prior
//! log-odds is updated by an additive sum of per-channel log Bayes factors,
//! one channel per evidence type (semantic, visual, performance, accessibility,
//! determinism). Accumulation in the log domain is closed-form, numerically
//! stable, and exactly additive, so every decision decomposes into per-signal
//! contributions with artifact linkage.
//!
//! # Outputs
//!
//! - A [`PosteriorRecord`] per `claim_id` carrying the prior/posterior logit,
//!   a posterior probability with a credible interval, a moment-matched Beta
//!   posterior ([`crate::semantic_contract::BayesianPosterior`]) for downstream
//!   expected-loss consumers, and a [`MigrationDecision`].
//! - A Bayes-factor ledger ([`ChannelContribution`]) ranked by absolute
//!   contribution (top-contributor first).
//! - A counterfactual flip ([`CounterfactualFlip`]): the smallest single-channel
//!   evidence change that would move the verdict across the decision boundary.
//!
//! # Graceful degradation
//!
//! Missing required channels do not hard-fail. Each absent channel applies a
//! conservative log-odds penalty (biasing toward rejection) and inflates the
//! posterior variance (widening uncertainty). When too few channels are present
//! an otherwise-accepting verdict degrades to
//! [`MigrationDecision::ConservativeFallback`] instead of auto-approving.
//!
//! # Determinism
//!
//! Channels are aggregated in a fixed order ([`EvidenceChannel::ALL`]) and the
//! ledger is sorted deterministically, so repeated runs produce identical
//! records, ids, and checksums.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::semantic_contract::{BayesianPosterior, MigrationDecision};

// ── Schema Version & Constants ───────────────────────────────────────────

/// Schema version for posterior-core reports.
pub const POSTERIOR_CORE_SCHEMA_VERSION: &str = "posterior-core-v1";

/// Clamp probabilities into `(PROB_EPS, 1 - PROB_EPS)` to keep logits finite.
const PROB_EPS: f64 = 1e-9;

// ── Evidence Channels ────────────────────────────────────────────────────

/// An evidence channel feeding the posterior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceChannel {
    /// Semantic differential (state transitions, side effects).
    Semantic,
    /// Visual / terminal-output comparison.
    Visual,
    /// Performance comparison.
    Performance,
    /// Accessibility parity.
    Accessibility,
    /// Determinism / replay stability.
    Determinism,
}

impl EvidenceChannel {
    /// Every channel, in canonical aggregation order.
    pub const ALL: &'static [EvidenceChannel] = &[
        EvidenceChannel::Semantic,
        EvidenceChannel::Visual,
        EvidenceChannel::Performance,
        EvidenceChannel::Accessibility,
        EvidenceChannel::Determinism,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::Visual => "visual",
            Self::Performance => "performance",
            Self::Accessibility => "accessibility",
            Self::Determinism => "determinism",
        }
    }

    /// Canonical aggregation index (for deterministic tie-breaks).
    #[must_use]
    pub fn order_index(self) -> usize {
        match self {
            Self::Semantic => 0,
            Self::Visual => 1,
            Self::Performance => 2,
            Self::Accessibility => 3,
            Self::Determinism => 4,
        }
    }
}

/// A single evidence signal on one channel.
///
/// `log_bayes_factor` is `ln(P(observation | accept) / P(observation | reject))`:
/// positive supports acceptance, negative supports rejection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelEvidence {
    /// Which channel this signal belongs to.
    pub channel: EvidenceChannel,
    /// Log Bayes factor (natural log).
    pub log_bayes_factor: f64,
    /// Non-negative influence weight (default `1.0`).
    pub weight: f64,
    /// Non-negative variance of the log Bayes factor (uncertainty).
    pub log_bf_variance: f64,
    /// Claim this evidence attaches to.
    pub claim_id: Option<String>,
    /// Source artifact id for provenance.
    pub artifact_id: Option<String>,
}

impl ChannelEvidence {
    /// Build a signal with unit weight and zero variance.
    #[must_use]
    pub fn new(channel: EvidenceChannel, log_bayes_factor: f64) -> Self {
        Self {
            channel,
            log_bayes_factor,
            weight: 1.0,
            log_bf_variance: 0.0,
            claim_id: None,
            artifact_id: None,
        }
    }

    /// Set the influence weight (clamped to be non-negative).
    #[must_use]
    pub fn weight(mut self, weight: f64) -> Self {
        self.weight = if weight.is_finite() && weight > 0.0 {
            weight
        } else {
            0.0
        };
        self
    }

    /// Set the log-BF variance (clamped to be non-negative).
    #[must_use]
    pub fn variance(mut self, variance: f64) -> Self {
        self.log_bf_variance = if variance.is_finite() && variance > 0.0 {
            variance
        } else {
            0.0
        };
        self
    }

    /// Attach a claim id.
    #[must_use]
    pub fn claim(mut self, claim_id: impl Into<String>) -> Self {
        self.claim_id = Some(claim_id.into());
        self
    }

    /// Attach a source artifact id.
    #[must_use]
    pub fn artifact(mut self, artifact_id: impl Into<String>) -> Self {
        self.artifact_id = Some(artifact_id.into());
        self
    }

    /// Effective additive contribution to the posterior logit.
    #[must_use]
    fn contribution(&self) -> f64 {
        sanitize(self.log_bayes_factor) * self.weight
    }

    /// Effective additive contribution to the posterior logit variance.
    #[must_use]
    fn variance_contribution(&self) -> f64 {
        self.weight * self.weight * self.log_bf_variance
    }
}

// ── Prior & Config ───────────────────────────────────────────────────────

/// Prior beliefs and degradation policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PosteriorPrior {
    /// Prior probability of acceptance, in `(0, 1)`.
    pub prior_accept_prob: f64,
    /// Prior variance on the logit (`> 0`).
    pub prior_logit_variance: f64,
    /// Conservative logit penalty subtracted per missing required channel.
    pub missing_channel_penalty: f64,
    /// Variance added per missing required channel (widens uncertainty).
    pub missing_channel_variance_inflation: f64,
}

impl Default for PosteriorPrior {
    fn default() -> Self {
        Self {
            prior_accept_prob: 0.5,
            prior_logit_variance: 1.0,
            missing_channel_penalty: 0.5,
            missing_channel_variance_inflation: 1.0,
        }
    }
}

/// Posterior-core configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PosteriorCoreConfig {
    /// Prior + degradation policy.
    pub prior: PosteriorPrior,
    /// Channels that should be present (missing ones degrade gracefully).
    pub required_channels: Vec<EvidenceChannel>,
    /// z-multiplier for the credible interval (e.g. `1.96` ≈ 95%).
    pub credible_z: f64,
    /// Probability at/above which (with a clearing lower bound) we auto-approve.
    pub auto_approve_prob: f64,
    /// Probability below which the verdict is a (soft) reject.
    pub reject_prob: f64,
    /// Probability below which the verdict is a hard reject.
    pub hard_reject_prob: f64,
    /// Minimum present channels before an accepting verdict is trusted; below
    /// this an otherwise-accepting verdict degrades to a conservative fallback.
    pub min_present_for_accept: usize,
}

impl Default for PosteriorCoreConfig {
    fn default() -> Self {
        Self {
            prior: PosteriorPrior::default(),
            required_channels: EvidenceChannel::ALL.to_vec(),
            credible_z: 1.96,
            auto_approve_prob: 0.9,
            reject_prob: 0.5,
            hard_reject_prob: 0.1,
            min_present_for_accept: 3,
        }
    }
}

impl PosteriorCoreConfig {
    /// Override the prior.
    #[must_use]
    pub fn with_prior(mut self, prior: PosteriorPrior) -> Self {
        self.prior = prior;
        self
    }

    /// Override the required channels.
    #[must_use]
    pub fn with_required_channels(
        mut self,
        channels: impl IntoIterator<Item = EvidenceChannel>,
    ) -> Self {
        let mut channels: Vec<EvidenceChannel> = channels.into_iter().collect();
        channels.sort_by_key(|c| c.order_index());
        channels.dedup();
        self.required_channels = channels;
        self
    }

    /// Override the minimum present-channel count for an accepting verdict.
    #[must_use]
    pub fn with_min_present_for_accept(mut self, min: usize) -> Self {
        self.min_present_for_accept = min;
        self
    }
}

// ── Ledger & Records ─────────────────────────────────────────────────────

/// One channel's aggregated contribution to the posterior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelContribution {
    /// The channel.
    pub channel: EvidenceChannel,
    /// Aggregate log Bayes factor across this channel's signals.
    pub log_bayes_factor: f64,
    /// Effective weight (sum of signal weights).
    pub weight: f64,
    /// Additive logit contribution (`Σ weight·log_bf`).
    pub contribution: f64,
    /// Additive variance contribution.
    pub variance_contribution: f64,
    /// Number of signals aggregated.
    pub signal_count: usize,
    /// Claim ids referenced by this channel's signals.
    pub claim_ids: Vec<String>,
    /// Source artifact ids referenced by this channel's signals.
    pub artifact_ids: Vec<String>,
    /// Whether the channel was present at all.
    pub present: bool,
}

/// The smallest single-channel evidence change that flips the verdict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CounterfactualFlip {
    /// The current decision.
    pub current_decision: MigrationDecision,
    /// The decision the flip would produce.
    pub flips_to: MigrationDecision,
    /// Probability boundary being crossed.
    pub boundary_prob: f64,
    /// Signed logit delta required to reach the boundary.
    pub required_logit_delta: f64,
    /// Cheapest channel to change (highest weight), if any present.
    pub nearest_channel: Option<EvidenceChannel>,
    /// That channel's current aggregate log Bayes factor.
    pub nearest_channel_current_log_bf: Option<f64>,
    /// The log Bayes factor that channel would need to flip the verdict.
    pub nearest_channel_required_log_bf: Option<f64>,
    /// Human-readable explanation.
    pub explanation: String,
}

/// A full posterior inference record for one claim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PosteriorRecord {
    /// Claim id.
    pub claim_id: String,
    /// Prior log-odds.
    pub prior_logit: f64,
    /// Posterior log-odds.
    pub posterior_logit: f64,
    /// Posterior variance on the logit.
    pub posterior_logit_variance: f64,
    /// Posterior standard deviation on the logit.
    pub posterior_logit_std: f64,
    /// Posterior acceptance probability.
    pub posterior_prob: f64,
    /// Lower credible bound on the probability.
    pub credible_lower_prob: f64,
    /// Upper credible bound on the probability.
    pub credible_upper_prob: f64,
    /// Aggregate log Bayes factor (present channels only).
    pub aggregate_log_bayes_factor: f64,
    /// Migration decision.
    pub decision: MigrationDecision,
    /// Moment-matched Beta posterior for downstream expected-loss consumers.
    pub beta_posterior: BayesianPosterior,
    /// Per-channel contribution ledger (ranked, top contributor first).
    pub contributions: Vec<ChannelContribution>,
    /// Channels that were required but absent.
    pub missing_channels: Vec<EvidenceChannel>,
    /// Number of present channels.
    pub present_channel_count: usize,
    /// Whether the verdict was produced under degraded/sparse evidence.
    pub degraded: bool,
    /// Counterfactual flip analysis.
    pub counterfactual: CounterfactualFlip,
}

impl PosteriorRecord {
    /// The top-`n` contributing channels by absolute contribution.
    #[must_use]
    pub fn top_contributors(&self, n: usize) -> Vec<&ChannelContribution> {
        self.contributions.iter().take(n).collect()
    }
}

// ── Engine ───────────────────────────────────────────────────────────────

/// Computes posterior records from channel evidence.
#[derive(Debug, Clone, Default)]
pub struct PosteriorEngine {
    config: PosteriorCoreConfig,
}

impl PosteriorEngine {
    /// Build an engine from configuration.
    #[must_use]
    pub fn new(config: PosteriorCoreConfig) -> Self {
        Self { config }
    }

    /// Borrow the configuration.
    #[must_use]
    pub fn config(&self) -> &PosteriorCoreConfig {
        &self.config
    }

    /// Infer the posterior for a single claim from its evidence signals.
    #[must_use]
    pub fn infer(
        &self,
        claim_id: impl Into<String>,
        evidence: &[ChannelEvidence],
    ) -> PosteriorRecord {
        let claim_id = claim_id.into();
        let prior_logit = logit(self.config.prior.prior_accept_prob);

        // Aggregate evidence per channel in canonical order.
        let mut contributions: Vec<ChannelContribution> = Vec::new();
        let mut aggregate_log_bf = 0.0;
        let mut variance = self.config.prior.prior_logit_variance.max(PROB_EPS);
        let mut present_channel_count = 0;

        for channel in EvidenceChannel::ALL.iter().copied() {
            let signals: Vec<&ChannelEvidence> =
                evidence.iter().filter(|e| e.channel == channel).collect();
            if signals.is_empty() {
                continue;
            }
            present_channel_count += 1;
            let mut log_bf_sum = 0.0;
            let mut weight_sum = 0.0;
            let mut contribution = 0.0;
            let mut variance_contribution = 0.0;
            let mut claim_ids: Vec<String> = Vec::new();
            let mut artifact_ids: Vec<String> = Vec::new();
            for signal in &signals {
                log_bf_sum += sanitize(signal.log_bayes_factor);
                weight_sum += signal.weight;
                contribution += signal.contribution();
                variance_contribution += signal.variance_contribution();
                if let Some(id) = &signal.claim_id {
                    push_unique(&mut claim_ids, id);
                }
                if let Some(id) = &signal.artifact_id {
                    push_unique(&mut artifact_ids, id);
                }
            }
            aggregate_log_bf += contribution;
            variance += variance_contribution;
            contributions.push(ChannelContribution {
                channel,
                log_bayes_factor: log_bf_sum,
                weight: weight_sum,
                contribution,
                variance_contribution,
                signal_count: signals.len(),
                claim_ids,
                artifact_ids,
                present: true,
            });
        }

        // Graceful degradation for missing required channels.
        let missing_channels: Vec<EvidenceChannel> = self
            .config
            .required_channels
            .iter()
            .copied()
            .filter(|c| !contributions.iter().any(|ct| ct.channel == *c))
            .collect();
        let missing_penalty =
            usize_to_f64(missing_channels.len()) * self.config.prior.missing_channel_penalty;
        variance += usize_to_f64(missing_channels.len())
            * self
                .config
                .prior
                .missing_channel_variance_inflation
                .max(0.0);

        let posterior_logit = prior_logit + aggregate_log_bf - missing_penalty;

        // Rank ledger by absolute contribution (deterministic tie-break).
        contributions.sort_by(|a, b| {
            b.contribution
                .abs()
                .total_cmp(&a.contribution.abs())
                .then_with(|| a.channel.order_index().cmp(&b.channel.order_index()))
        });

        let std = variance.max(0.0).sqrt();
        let z = self.config.credible_z.max(0.0);
        let posterior_prob = stable_sigmoid(posterior_logit);
        let credible_lower_prob = stable_sigmoid(posterior_logit - z * std);
        let credible_upper_prob = stable_sigmoid(posterior_logit + z * std);

        let degraded = !missing_channels.is_empty()
            || present_channel_count < self.config.min_present_for_accept;

        let decision = self.decide(
            posterior_prob,
            credible_lower_prob,
            credible_upper_prob,
            present_channel_count,
        );

        let beta_posterior = moment_matched_beta(
            posterior_prob,
            posterior_logit,
            variance,
            credible_lower_prob,
            credible_upper_prob,
        );

        let counterfactual = self.counterfactual(posterior_logit, &contributions, decision);

        PosteriorRecord {
            claim_id,
            prior_logit,
            posterior_logit,
            posterior_logit_variance: variance,
            posterior_logit_std: std,
            posterior_prob,
            credible_lower_prob,
            credible_upper_prob,
            aggregate_log_bayes_factor: aggregate_log_bf,
            decision,
            beta_posterior,
            contributions,
            missing_channels,
            present_channel_count,
            degraded,
            counterfactual,
        }
    }

    /// Map posterior summaries onto a migration decision.
    fn decide(
        &self,
        prob: f64,
        lower: f64,
        upper: f64,
        present_channel_count: usize,
    ) -> MigrationDecision {
        if present_channel_count == 0 {
            return MigrationDecision::ConservativeFallback;
        }
        let accepting = prob >= self.config.reject_prob;
        if accepting && present_channel_count < self.config.min_present_for_accept {
            // Enough to lean accept, but too sparse to trust → be conservative.
            return MigrationDecision::ConservativeFallback;
        }
        if prob >= self.config.auto_approve_prob && lower >= self.config.reject_prob {
            MigrationDecision::AutoApprove
        } else if accepting {
            MigrationDecision::HumanReview
        } else if prob < self.config.hard_reject_prob || upper < self.config.reject_prob {
            MigrationDecision::HardReject
        } else {
            MigrationDecision::Reject
        }
    }

    /// Compute the cheapest single-channel evidence change that flips the
    /// verdict across the nearest accept/reject boundary.
    fn counterfactual(
        &self,
        posterior_logit: f64,
        contributions: &[ChannelContribution],
        current_decision: MigrationDecision,
    ) -> CounterfactualFlip {
        let boundary_prob = self.config.reject_prob;
        let boundary_logit = logit(boundary_prob);
        let required_logit_delta = boundary_logit - posterior_logit;
        let accepting = posterior_logit >= boundary_logit;
        let flips_to = if accepting {
            MigrationDecision::Reject
        } else {
            MigrationDecision::HumanReview
        };

        // Cheapest channel to adjust = the present channel with the largest
        // weight (smallest required change in its log Bayes factor).
        let nearest = contributions
            .iter()
            .filter(|c| c.present && c.weight > 0.0)
            .max_by(|a, b| a.weight.total_cmp(&b.weight));

        let (nearest_channel, current_log_bf, required_log_bf, explanation) = match nearest {
            Some(c) => {
                let required = c.log_bayes_factor + required_logit_delta / c.weight;
                (
                    Some(c.channel),
                    Some(c.log_bayes_factor),
                    Some(required),
                    format!(
                        "shift {} channel log-BF from {:.4} to {:.4} (Δlogit {:.4}) to cross p={:.3}",
                        c.channel.as_str(),
                        c.log_bayes_factor,
                        required,
                        required_logit_delta,
                        boundary_prob
                    ),
                )
            }
            None => (
                None,
                None,
                None,
                format!(
                    "no present channel to adjust; verdict is prior-driven (Δlogit {required_logit_delta:.4} needed)"
                ),
            ),
        };

        CounterfactualFlip {
            current_decision,
            flips_to,
            boundary_prob,
            required_logit_delta,
            nearest_channel,
            nearest_channel_current_log_bf: current_log_bf,
            nearest_channel_required_log_bf: required_log_bf,
            explanation,
        }
    }

    /// Infer a batch of claims into a deterministic ledger report.
    #[must_use]
    pub fn evaluate(&self, claims: &[(String, Vec<ChannelEvidence>)]) -> PosteriorLedgerReport {
        let records: Vec<PosteriorRecord> = claims
            .iter()
            .map(|(claim_id, evidence)| self.infer(claim_id.clone(), evidence))
            .collect();

        let summary = summarize(&records);
        let report_id = report_id_for(&self.config, claims);
        let exported_json_stats = export_json_stats(&report_id, &self.config, &records, &summary);
        let replay_command = format!(
            "doctor_frankentui posterior-core --report-id {report_id} \
             --prior-accept {:.3} --credible-z {:.3}",
            self.config.prior.prior_accept_prob, self.config.credible_z
        );

        PosteriorLedgerReport {
            schema_version: POSTERIOR_CORE_SCHEMA_VERSION.to_string(),
            report_id,
            config: self.config.clone(),
            records,
            summary,
            exported_json_stats,
            replay_command,
        }
    }
}

// ── Report ───────────────────────────────────────────────────────────────

/// Aggregate counts for a posterior-ledger report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PosteriorLedgerSummary {
    /// Total claims evaluated.
    pub total_claims: usize,
    /// Auto-approve verdicts.
    pub auto_approve_count: usize,
    /// Human-review verdicts.
    pub human_review_count: usize,
    /// Reject verdicts.
    pub reject_count: usize,
    /// Hard-reject verdicts.
    pub hard_reject_count: usize,
    /// Conservative-fallback verdicts.
    pub conservative_fallback_count: usize,
    /// Records produced under degraded evidence.
    pub degraded_count: usize,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PosteriorJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// Full posterior-ledger report over a batch of claims.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PosteriorLedgerReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Configuration used.
    pub config: PosteriorCoreConfig,
    /// Per-claim posterior records (input order preserved).
    pub records: Vec<PosteriorRecord>,
    /// Aggregate summary.
    pub summary: PosteriorLedgerSummary,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: PosteriorJsonStatsArtifact,
    /// Replay command for the report.
    pub replay_command: String,
}

// ── Numerical Helpers ────────────────────────────────────────────────────

/// Numerically stable logistic sigmoid, clamped to `(PROB_EPS, 1 - PROB_EPS)`.
#[must_use]
pub fn stable_sigmoid(x: f64) -> f64 {
    let p = if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    };
    p.clamp(PROB_EPS, 1.0 - PROB_EPS)
}

/// Logit of a probability, clamped to keep the result finite.
#[must_use]
pub fn logit(p: f64) -> f64 {
    let p = p.clamp(PROB_EPS, 1.0 - PROB_EPS);
    (p / (1.0 - p)).ln()
}

/// Replace non-finite values with `0.0` (neutral evidence).
fn sanitize(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

fn push_unique(target: &mut Vec<String>, value: &str) {
    if !target.iter().any(|existing| existing == value) {
        target.push(value.to_string());
    }
}

fn usize_to_f64(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::from(u32::MAX), f64::from)
}

/// Moment-match a Beta posterior from the probability mean and logit variance.
///
/// The probability-scale variance is obtained by the delta method
/// (`var_p ≈ (p(1-p))² · var_logit`) and converted to Beta `(α, β)` via the
/// method of moments, clamped to remain a valid distribution.
fn moment_matched_beta(
    prob: f64,
    _posterior_logit: f64,
    logit_variance: f64,
    credible_lower: f64,
    credible_upper: f64,
) -> BayesianPosterior {
    let p = prob.clamp(PROB_EPS, 1.0 - PROB_EPS);
    let slope = p * (1.0 - p);
    let var_p = (slope * slope * logit_variance).clamp(PROB_EPS, slope - PROB_EPS);
    // κ = m(1-m)/v - 1 is the Beta concentration; clamp to stay positive.
    let concentration = (slope / var_p - 1.0).max(PROB_EPS);
    let alpha = (p * concentration).max(PROB_EPS);
    let beta = ((1.0 - p) * concentration).max(PROB_EPS);
    BayesianPosterior {
        alpha,
        beta,
        mean: p,
        variance: var_p,
        credible_lower,
        credible_upper,
    }
}

fn summarize(records: &[PosteriorRecord]) -> PosteriorLedgerSummary {
    let mut summary = PosteriorLedgerSummary {
        total_claims: records.len(),
        auto_approve_count: 0,
        human_review_count: 0,
        reject_count: 0,
        hard_reject_count: 0,
        conservative_fallback_count: 0,
        degraded_count: 0,
    };
    for record in records {
        match record.decision {
            MigrationDecision::AutoApprove => summary.auto_approve_count += 1,
            MigrationDecision::HumanReview => summary.human_review_count += 1,
            MigrationDecision::Reject => summary.reject_count += 1,
            MigrationDecision::HardReject => summary.hard_reject_count += 1,
            MigrationDecision::ConservativeFallback | MigrationDecision::Rollback => {
                summary.conservative_fallback_count += 1;
            }
        }
        if record.degraded {
            summary.degraded_count += 1;
        }
    }
    summary
}

fn export_json_stats(
    report_id: &str,
    config: &PosteriorCoreConfig,
    records: &[PosteriorRecord],
    summary: &PosteriorLedgerSummary,
) -> PosteriorJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        config: &'a PosteriorCoreConfig,
        summary: &'a PosteriorLedgerSummary,
        records: &'a [PosteriorRecord],
    }

    let payload = Export {
        schema_version: POSTERIOR_CORE_SCHEMA_VERSION,
        report_id,
        config,
        summary,
        records,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    PosteriorJsonStatsArtifact {
        path: format!("{report_id}/posterior_core_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn report_id_for(
    config: &PosteriorCoreConfig,
    claims: &[(String, Vec<ChannelEvidence>)],
) -> String {
    #[derive(Serialize)]
    struct IdInput<'a> {
        config: &'a PosteriorCoreConfig,
        claims: &'a [(String, Vec<ChannelEvidence>)],
    }
    let hash = stable_hash(&IdInput { config, claims });
    format!("posterior-core-{}", short_hash(&hash))
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

    const TOL: f64 = 1e-9;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() <= TOL
    }

    fn strong_accept_evidence() -> Vec<ChannelEvidence> {
        EvidenceChannel::ALL
            .iter()
            .copied()
            .map(|channel| {
                ChannelEvidence::new(channel, 2.0)
                    .claim("claim-x")
                    .artifact("art")
            })
            .collect()
    }

    #[test]
    fn sigmoid_is_stable_and_monotone() {
        assert!(approx(stable_sigmoid(0.0), 0.5));
        assert!(stable_sigmoid(40.0) > 0.999_999);
        assert!(stable_sigmoid(-40.0) < 0.000_001);
        // No NaN/inf at extremes.
        assert!(stable_sigmoid(1e9).is_finite());
        assert!(stable_sigmoid(-1e9).is_finite());
        assert!(stable_sigmoid(1.0) > stable_sigmoid(0.0));
    }

    #[test]
    fn logit_sigmoid_roundtrip() {
        for p in [0.05, 0.25, 0.5, 0.75, 0.95] {
            assert!(approx(stable_sigmoid(logit(p)), p));
        }
    }

    #[test]
    fn strong_accept_yields_auto_approve() {
        let record = PosteriorEngine::default().infer("claim-x", &strong_accept_evidence());
        assert_eq!(record.decision, MigrationDecision::AutoApprove);
        assert!(record.posterior_prob > 0.9);
        assert!(record.credible_lower_prob >= 0.5);
        assert!(!record.degraded);
        assert_eq!(record.present_channel_count, 5);
        assert!(record.missing_channels.is_empty());
    }

    #[test]
    fn strong_reject_yields_hard_reject() {
        let evidence: Vec<ChannelEvidence> = EvidenceChannel::ALL
            .iter()
            .copied()
            .map(|c| ChannelEvidence::new(c, -3.0))
            .collect();
        let record = PosteriorEngine::default().infer("claim-r", &evidence);
        assert!(matches!(
            record.decision,
            MigrationDecision::HardReject | MigrationDecision::Reject
        ));
        assert!(record.posterior_prob < 0.5);
    }

    #[test]
    fn aggregate_log_bf_is_additive() {
        let evidence = vec![
            ChannelEvidence::new(EvidenceChannel::Semantic, 1.0),
            ChannelEvidence::new(EvidenceChannel::Visual, 0.5),
            ChannelEvidence::new(EvidenceChannel::Performance, -0.25),
        ];
        let record = PosteriorEngine::default().infer("c", &evidence);
        assert!(approx(record.aggregate_log_bayes_factor, 1.25));
        // posterior = prior(0.0 at p=0.5) + aggregate - missing_penalty.
        // Two required channels missing (accessibility, determinism).
        assert_eq!(record.missing_channels.len(), 2);
        let expected = 0.0 + 1.25 - 2.0 * 0.5;
        assert!(approx(record.posterior_logit, expected));
    }

    #[test]
    fn weights_scale_contribution() {
        let evidence = vec![ChannelEvidence::new(EvidenceChannel::Semantic, 1.0).weight(3.0)];
        let record = PosteriorEngine::default().infer("c", &evidence);
        let semantic = record
            .contributions
            .iter()
            .find(|c| c.channel == EvidenceChannel::Semantic)
            .unwrap();
        assert!(approx(semantic.contribution, 3.0));
        assert!(approx(semantic.weight, 3.0));
    }

    #[test]
    fn ledger_is_ranked_by_absolute_contribution() {
        let evidence = vec![
            ChannelEvidence::new(EvidenceChannel::Semantic, 0.2),
            ChannelEvidence::new(EvidenceChannel::Visual, -3.0),
            ChannelEvidence::new(EvidenceChannel::Performance, 1.0),
        ];
        let record = PosteriorEngine::default().infer("c", &evidence);
        // Visual has the largest |contribution|, so it ranks first.
        assert_eq!(record.contributions[0].channel, EvidenceChannel::Visual);
        let top = record.top_contributors(1);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].channel, EvidenceChannel::Visual);
    }

    #[test]
    fn multiple_signals_per_channel_aggregate() {
        let evidence = vec![
            ChannelEvidence::new(EvidenceChannel::Semantic, 0.5).claim("a"),
            ChannelEvidence::new(EvidenceChannel::Semantic, 0.5).claim("b"),
        ];
        let record = PosteriorEngine::default().infer("c", &evidence);
        let semantic = record
            .contributions
            .iter()
            .find(|c| c.channel == EvidenceChannel::Semantic)
            .unwrap();
        assert_eq!(semantic.signal_count, 2);
        assert!(approx(semantic.contribution, 1.0));
        assert_eq!(semantic.claim_ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn missing_evidence_degrades_gracefully() {
        // Only one channel present, strongly positive.
        let evidence = vec![ChannelEvidence::new(EvidenceChannel::Semantic, 5.0)];
        let record = PosteriorEngine::default().infer("c", &evidence);
        // Sparse evidence → conservative fallback, not auto-approve.
        assert_eq!(record.decision, MigrationDecision::ConservativeFallback);
        assert!(record.degraded);
        assert_eq!(record.missing_channels.len(), 4);
        // Variance is inflated by the missing channels.
        assert!(record.posterior_logit_variance > 1.0);
    }

    #[test]
    fn no_evidence_is_conservative_fallback() {
        let record = PosteriorEngine::default().infer("c", &[]);
        assert_eq!(record.decision, MigrationDecision::ConservativeFallback);
        assert_eq!(record.present_channel_count, 0);
        assert!(record.degraded);
        assert!(record.counterfactual.nearest_channel.is_none());
    }

    #[test]
    fn non_finite_log_bf_is_neutralized() {
        let evidence = vec![
            ChannelEvidence::new(EvidenceChannel::Semantic, f64::INFINITY),
            ChannelEvidence::new(EvidenceChannel::Visual, f64::NAN),
            ChannelEvidence::new(EvidenceChannel::Performance, 1.0),
            ChannelEvidence::new(EvidenceChannel::Accessibility, 0.0),
            ChannelEvidence::new(EvidenceChannel::Determinism, 0.0),
        ];
        let record = PosteriorEngine::default().infer("c", &evidence);
        assert!(record.posterior_logit.is_finite());
        assert!(record.posterior_prob.is_finite());
        // Only the performance signal contributes.
        assert!(approx(record.aggregate_log_bayes_factor, 1.0));
    }

    #[test]
    fn credible_interval_brackets_the_mean() {
        let record = PosteriorEngine::default().infer("c", &strong_accept_evidence());
        assert!(record.credible_lower_prob <= record.posterior_prob);
        assert!(record.posterior_prob <= record.credible_upper_prob);
        assert!(record.credible_lower_prob > 0.0);
        assert!(record.credible_upper_prob < 1.0);
    }

    #[test]
    fn beta_posterior_is_valid_and_matches_mean() {
        let record = PosteriorEngine::default().infer("c", &strong_accept_evidence());
        let beta = &record.beta_posterior;
        assert!(beta.alpha > 0.0);
        assert!(beta.beta > 0.0);
        assert!(approx(beta.mean, record.posterior_prob));
        assert!(beta.variance > 0.0);
        // Beta mean = α/(α+β) should match the reported mean.
        assert!((beta.alpha / (beta.alpha + beta.beta) - beta.mean).abs() < 1e-6);
    }

    #[test]
    fn counterfactual_flips_an_accept_to_reject() {
        let record = PosteriorEngine::default().infer("c", &strong_accept_evidence());
        let cf = &record.counterfactual;
        assert_eq!(cf.current_decision, record.decision);
        assert_eq!(cf.flips_to, MigrationDecision::Reject);
        // Accept currently → need a negative logit delta to cross down.
        assert!(cf.required_logit_delta < 0.0);
        assert!(cf.nearest_channel.is_some());
        assert!(cf.nearest_channel_required_log_bf.is_some());
    }

    #[test]
    fn counterfactual_required_change_actually_flips() {
        let engine = PosteriorEngine::default();
        let mut evidence = strong_accept_evidence();
        let record = engine.infer("c", &evidence);
        let cf = &record.counterfactual;
        let channel = cf.nearest_channel.unwrap();
        let required = cf.nearest_channel_required_log_bf.unwrap();
        // Apply the counterfactual: set that channel's log-BF to `required`.
        for e in &mut evidence {
            if e.channel == channel {
                e.log_bayes_factor = required;
            }
        }
        let flipped = engine.infer("c", &evidence);
        // The posterior should now sit at/below the reject boundary.
        assert!(flipped.posterior_prob <= engine.config().reject_prob + 1e-6);
    }

    #[test]
    fn report_is_deterministic() {
        let claims = vec![("claim-x".to_string(), strong_accept_evidence())];
        let first = PosteriorEngine::default().evaluate(&claims);
        let second = PosteriorEngine::default().evaluate(&claims);
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        assert_eq!(first.records, second.records);
    }

    #[test]
    fn report_roundtrips_through_serde() {
        let claims = vec![
            ("claim-x".to_string(), strong_accept_evidence()),
            (
                "claim-y".to_string(),
                vec![ChannelEvidence::new(EvidenceChannel::Semantic, -1.0)],
            ),
        ];
        let report = PosteriorEngine::default().evaluate(&claims);
        let json = serde_json::to_string(&report).unwrap();
        let restored: PosteriorLedgerReport = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.report_id, report.report_id);
        assert_eq!(restored.schema_version, report.schema_version);
        assert_eq!(restored.summary, report.summary);
        // serde_json (without the `float_roundtrip` feature) can drift f64 by a
        // ULP on reparse, so assert structural identity rather than exact float
        // equality of the records.
        assert_eq!(restored.records.len(), report.records.len());
        for (a, b) in restored.records.iter().zip(&report.records) {
            assert_eq!(a.claim_id, b.claim_id);
            assert_eq!(a.decision, b.decision);
            assert_eq!(a.present_channel_count, b.present_channel_count);
            assert_eq!(a.missing_channels, b.missing_channels);
            assert_eq!(a.contributions.len(), b.contributions.len());
        }
    }

    #[test]
    fn json_stats_checksum_is_self_consistent() {
        let claims = vec![("c".to_string(), strong_accept_evidence())];
        let report = PosteriorEngine::default().evaluate(&claims);
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    #[test]
    fn summary_counts_decisions() {
        let claims = vec![
            ("a".to_string(), strong_accept_evidence()),
            (
                "b".to_string(),
                EvidenceChannel::ALL
                    .iter()
                    .copied()
                    .map(|c| ChannelEvidence::new(c, -3.0))
                    .collect(),
            ),
            (
                "c".to_string(),
                vec![ChannelEvidence::new(EvidenceChannel::Semantic, 5.0)],
            ),
        ];
        let report = PosteriorEngine::default().evaluate(&claims);
        assert_eq!(report.summary.total_claims, 3);
        let total = report.summary.auto_approve_count
            + report.summary.human_review_count
            + report.summary.reject_count
            + report.summary.hard_reject_count
            + report.summary.conservative_fallback_count;
        assert_eq!(total, 3);
        assert!(report.summary.degraded_count >= 1);
    }

    #[test]
    fn replay_command_references_report() {
        let claims = vec![("c".to_string(), strong_accept_evidence())];
        let report = PosteriorEngine::default().evaluate(&claims);
        assert!(report.replay_command.contains("posterior-core"));
        assert!(report.replay_command.contains(&report.report_id));
    }

    #[test]
    fn prior_shifts_the_posterior() {
        let evidence = vec![ChannelEvidence::new(EvidenceChannel::Semantic, 0.0).claim("c")];
        let optimistic = PosteriorCoreConfig::default()
            .with_prior(PosteriorPrior {
                prior_accept_prob: 0.8,
                ..PosteriorPrior::default()
            })
            .with_required_channels([EvidenceChannel::Semantic]);
        let pessimistic = PosteriorCoreConfig::default()
            .with_prior(PosteriorPrior {
                prior_accept_prob: 0.2,
                ..PosteriorPrior::default()
            })
            .with_required_channels([EvidenceChannel::Semantic]);
        let opt = PosteriorEngine::new(optimistic).infer("c", &evidence);
        let pess = PosteriorEngine::new(pessimistic).infer("c", &evidence);
        assert!(opt.posterior_prob > pess.posterior_prob);
    }

    #[test]
    fn config_roundtrips_and_required_channels_dedup() {
        let config = PosteriorCoreConfig::default().with_required_channels([
            EvidenceChannel::Determinism,
            EvidenceChannel::Semantic,
            EvidenceChannel::Semantic,
        ]);
        assert_eq!(config.required_channels.len(), 2);
        // Sorted by canonical order: semantic (0) before determinism (4).
        assert_eq!(config.required_channels[0], EvidenceChannel::Semantic);
        let json = serde_json::to_string(&config).unwrap();
        let restored: PosteriorCoreConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, config);
    }
}

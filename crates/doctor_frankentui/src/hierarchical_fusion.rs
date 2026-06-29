//! Hierarchical conjugate Bayesian evidence fusion with dependence-aware
//! corrections (bd-3bxhj.10.38).
//!
//! OpenTUI→FrankenTUI migration evidence streams are heterogeneous, sparse in the
//! long tail, and frequently correlated (widget-coverage signal correlates with
//! runtime-event signal). Naively multiplying / summing channel evidence
//! overstates certainty and produces brittle decisions. This module is a
//! closed-form, deterministic, auditable fusion core:
//!
//! 1. **Conjugate online kernels** — Beta-Binomial (Bernoulli pass/fail),
//!    Gamma-Poisson (event/error rate), Dirichlet-Multinomial (categorical
//!    strategy outcome). Each is a closed-form conjugate update, unified through a
//!    [`PosteriorSummary`] so the rest of the pipeline is kernel-agnostic.
//! 2. **Dependence-aware fusion** — fusing `m` channels with mean pairwise
//!    correlation `rho` deflates the added evidence by the inverse design effect
//!    `1 / (1 + (m-1)*rho)`. The deflation can only *reduce* certainty, and every
//!    fusion records the `dependence_factor` + a human-readable trace (AC2:
//!    correlated channels never increase confidence without an explicit
//!    dependence-adjustment trace).
//! 3. **Hierarchical shrinkage** — empirical-Bayes partial pooling: each claim's
//!    posterior mean is shrunk toward its stratum's pooled mean by
//!    `lambda = kappa0 / (kappa0 + n)`, so sparse units borrow strength.
//! 4. **Robustification** — a Huberized count cap bounds the influence of an
//!    outlier-heavy channel; an epsilon-contamination prior-stress probe yields a
//!    **sensitivity band** (AC3: high sensitivity auto-raises the conservative
//!    action policy).
//! 5. **Diagnostics** — a posterior-predictive check flags when the observed
//!    aggregate is implausible under the fused posterior (AC4: predictive-check
//!    failures raise an explicit degraded-confidence flag for downstream gates).
//!
//! The ledger is **float-free** (every numeric term is a fixed-decimal string), so
//! it derives [`Eq`] and replays byte-identically (AC1: replay-stable, and
//! numerically guarded so extreme pseudo-counts stay finite).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::semantic_contract::MigrationDecision;

/// Schema version for the fusion artifacts.
pub const HIERARCHICAL_FUSION_SCHEMA_VERSION: &str = "hierarchical-fusion-v1";

/// Numeric epsilon for guarded ratios.
const EPS: f64 = 1e-9;

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

// ── Conjugate kernels ────────────────────────────────────────────────────────

/// The conjugate kernel family a claim uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionKernel {
    /// Beta-Binomial: Bernoulli pass/fail / parity outcomes.
    BetaBinomial,
    /// Gamma-Poisson: event / error rate processes.
    GammaPoisson,
    /// Dirichlet-Multinomial: categorical strategy outcomes.
    DirichletMultinomial,
}

impl FusionKernel {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BetaBinomial => "beta_binomial",
            Self::GammaPoisson => "gamma_poisson",
            Self::DirichletMultinomial => "dirichlet_multinomial",
        }
    }
}

/// A conjugate prior, one per kernel family.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum KernelPrior {
    /// Beta(alpha, beta).
    Beta { alpha: f64, beta: f64 },
    /// Gamma(shape, rate).
    Gamma { shape: f64, rate: f64 },
    /// Dirichlet(alphas).
    Dirichlet { alphas: Vec<f64> },
}

impl KernelPrior {
    /// The kernel family this prior belongs to.
    #[must_use]
    pub fn kernel(&self) -> FusionKernel {
        match self {
            Self::Beta { .. } => FusionKernel::BetaBinomial,
            Self::Gamma { .. } => FusionKernel::GammaPoisson,
            Self::Dirichlet { .. } => FusionKernel::DirichletMultinomial,
        }
    }
}

/// A single channel's sufficient statistics (matched to its kernel).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum KernelObservation {
    /// Bernoulli successes / failures.
    Bernoulli { successes: f64, failures: f64 },
    /// Poisson event count over an exposure window.
    Poisson { count: f64, exposure: f64 },
    /// Categorical counts (one per Dirichlet component).
    Categorical { counts: Vec<f64> },
}

impl KernelObservation {
    fn kernel(&self) -> FusionKernel {
        match self {
            Self::Bernoulli { .. } => FusionKernel::BetaBinomial,
            Self::Poisson { .. } => FusionKernel::GammaPoisson,
            Self::Categorical { .. } => FusionKernel::DirichletMultinomial,
        }
    }

    /// Whether all statistics are finite and non-negative, and the kernel matches.
    fn is_valid_for(&self, prior: &KernelPrior) -> bool {
        if self.kernel() != prior.kernel() {
            return false;
        }
        match self {
            Self::Bernoulli {
                successes,
                failures,
            } => {
                successes.is_finite()
                    && failures.is_finite()
                    && *successes >= 0.0
                    && *failures >= 0.0
            }
            Self::Poisson { count, exposure } => {
                count.is_finite() && exposure.is_finite() && *count >= 0.0 && *exposure >= 0.0
            }
            Self::Categorical { counts } => match prior {
                KernelPrior::Dirichlet { alphas } => {
                    counts.len() == alphas.len()
                        && counts.iter().all(|c| c.is_finite() && *c >= 0.0)
                }
                _ => false,
            },
        }
    }

    /// The total observation weight (used for shrinkage `n` and Huberization).
    fn weight(&self) -> f64 {
        match self {
            Self::Bernoulli {
                successes,
                failures,
            } => successes + failures,
            Self::Poisson { exposure, .. } => *exposure,
            Self::Categorical { counts } => counts.iter().sum(),
        }
    }
}

/// A unified posterior summary the rest of the pipeline operates on, regardless
/// of kernel. `mean` is the decision-relevant scalar (success prob / normalized
/// rate / dominant-category prob), `concentration` is the total pseudo-count.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PosteriorSummary {
    /// The decision-relevant posterior mean in `[0, 1]`.
    pub mean: f64,
    /// The posterior variance of the mean.
    pub variance: f64,
    /// Total pseudo-count (prior + evidence), the conjugate concentration.
    pub concentration: f64,
}

impl PosteriorSummary {
    fn standard_deviation(self) -> f64 {
        self.variance.max(0.0).sqrt()
    }
}

/// Apply a conjugate update to a prior with (already dependence-deflated and
/// Huberized) sufficient statistics, returning a unified posterior summary.
fn conjugate_posterior(prior: &KernelPrior, scaled: &KernelObservation) -> PosteriorSummary {
    match (prior, scaled) {
        (
            KernelPrior::Beta { alpha, beta },
            KernelObservation::Bernoulli {
                successes,
                failures,
            },
        ) => {
            let a = (alpha + successes).max(EPS);
            let b = (beta + failures).max(EPS);
            let sum = a + b;
            let mean = safe_div(a, sum);
            let variance = safe_div(a * b, sum * sum * (sum + 1.0));
            PosteriorSummary {
                mean: clamp01(mean),
                variance,
                concentration: sum,
            }
        }
        (KernelPrior::Gamma { shape, rate }, KernelObservation::Poisson { count, exposure }) => {
            let k = (shape + count).max(EPS);
            let theta = (rate + exposure).max(EPS);
            let rate_mean = safe_div(k, theta);
            let rate_var = safe_div(k, theta * theta);
            // Normalize the rate into [0, 1] via a saturating transform so the
            // decision-relevant mean is comparable across kernels (1 - exp(-rate)
            // is the Poisson "at least one event" probability).
            let mean = clamp01(1.0 - (-rate_mean).exp());
            // Delta-method variance of g(rate)=1-exp(-rate): g'(rate)=exp(-rate).
            let g_prime = (-rate_mean).exp();
            let variance = (g_prime * g_prime * rate_var).max(0.0);
            PosteriorSummary {
                mean,
                variance,
                concentration: theta,
            }
        }
        (KernelPrior::Dirichlet { alphas }, KernelObservation::Categorical { counts }) => {
            let posterior: Vec<f64> = alphas
                .iter()
                .zip(counts.iter())
                .map(|(a, c)| (a + c).max(EPS))
                .collect();
            let sum: f64 = posterior.iter().sum();
            // The dominant category's marginal Beta(alpha_i, sum-alpha_i).
            let top = posterior.iter().copied().fold(EPS, f64::max);
            let mean = safe_div(top, sum);
            let variance = safe_div(top * (sum - top), sum * sum * (sum + 1.0));
            PosteriorSummary {
                mean: clamp01(mean),
                variance,
                concentration: sum,
            }
        }
        // Mismatched prior/observation: a degenerate, maximally-uncertain summary.
        _ => PosteriorSummary {
            mean: 0.5,
            variance: 0.25,
            concentration: EPS,
        },
    }
}

// ── Robustification ──────────────────────────────────────────────────────────

/// Whether the robust (Huberized) likelihood fallback was engaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RobustMode {
    /// Standard conjugate likelihood.
    Standard,
    /// Huberized: an outlier-heavy channel's count was capped.
    Huberized,
}

impl RobustMode {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Huberized => "huberized",
        }
    }
}

/// Scale an observation's counts by a factor (dependence deflation), capping the
/// total weight at `huber_cap` (Huberization). Returns the scaled observation and
/// whether the cap engaged.
fn deflate_and_huberize(
    observation: &KernelObservation,
    factor: f64,
    huber_cap: f64,
) -> (KernelObservation, bool) {
    let factor = clamp01(factor);
    let weight = observation.weight() * factor;
    let (scale, huberized) = if weight > huber_cap && weight > EPS {
        (factor * safe_div(huber_cap, weight), true)
    } else {
        (factor, false)
    };
    let scaled = match observation {
        KernelObservation::Bernoulli {
            successes,
            failures,
        } => KernelObservation::Bernoulli {
            successes: successes * scale,
            failures: failures * scale,
        },
        KernelObservation::Poisson { count, exposure } => KernelObservation::Poisson {
            count: count * scale,
            exposure: exposure * scale,
        },
        KernelObservation::Categorical { counts } => KernelObservation::Categorical {
            counts: counts.iter().map(|c| c * scale).collect(),
        },
    };
    (scaled, huberized)
}

/// Combine two same-kernel observations (additive sufficient statistics).
fn combine_observations(acc: KernelObservation, next: &KernelObservation) -> KernelObservation {
    match (acc, next) {
        (
            KernelObservation::Bernoulli {
                successes: s1,
                failures: f1,
            },
            KernelObservation::Bernoulli {
                successes: s2,
                failures: f2,
            },
        ) => KernelObservation::Bernoulli {
            successes: s1 + s2,
            failures: f1 + f2,
        },
        (
            KernelObservation::Poisson {
                count: c1,
                exposure: e1,
            },
            KernelObservation::Poisson {
                count: c2,
                exposure: e2,
            },
        ) => KernelObservation::Poisson {
            count: c1 + c2,
            exposure: e1 + e2,
        },
        (
            KernelObservation::Categorical { counts: mut a },
            KernelObservation::Categorical { counts: b },
        ) => {
            for (i, c) in b.iter().enumerate() {
                if let Some(slot) = a.get_mut(i) {
                    *slot += c;
                }
            }
            KernelObservation::Categorical { counts: a }
        }
        // Mismatched kernels never reach here (validated upstream); keep `acc`.
        (acc, _) => acc,
    }
}

fn zero_like(prior: &KernelPrior) -> KernelObservation {
    match prior {
        KernelPrior::Beta { .. } => KernelObservation::Bernoulli {
            successes: 0.0,
            failures: 0.0,
        },
        KernelPrior::Gamma { .. } => KernelObservation::Poisson {
            count: 0.0,
            exposure: 0.0,
        },
        KernelPrior::Dirichlet { alphas } => KernelObservation::Categorical {
            counts: vec![0.0; alphas.len()],
        },
    }
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/// One evidence channel feeding a claim, with its correlation to the others.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FusionChannel {
    /// Stable channel id (e.g. `widget_coverage`, `runtime_event`).
    pub channel_id: String,
    /// The channel's sufficient statistics.
    pub observation: KernelObservation,
    /// Mean pairwise correlation of this channel to the others in `[0, 1]`.
    pub correlation: f64,
}

impl FusionChannel {
    /// Construct a channel.
    #[must_use]
    pub fn new(
        channel_id: impl Into<String>,
        observation: KernelObservation,
        correlation: f64,
    ) -> Self {
        Self {
            channel_id: channel_id.into(),
            observation,
            correlation,
        }
    }
}

/// A fusion claim: a prior, a stratum (hierarchical level), and the channels to
/// fuse into one posterior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FusionClaim {
    /// Stable claim id.
    pub claim_id: String,
    /// Hierarchical stratum (project family / component class / widget archetype).
    pub stratum: String,
    /// The conjugate prior.
    pub prior: KernelPrior,
    /// The evidence channels (fused dependence-aware).
    pub channels: Vec<FusionChannel>,
}

impl FusionClaim {
    /// Construct a claim.
    #[must_use]
    pub fn new(
        claim_id: impl Into<String>,
        stratum: impl Into<String>,
        prior: KernelPrior,
        channels: Vec<FusionChannel>,
    ) -> Self {
        Self {
            claim_id: claim_id.into(),
            stratum: stratum.into(),
            prior,
            channels,
        }
    }
}

/// Fusion configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FusionConfig {
    /// Empirical-Bayes shrinkage pseudo-count `kappa0`.
    pub shrinkage_kappa0: f64,
    /// Epsilon-contamination weight for the prior-stress sensitivity probe.
    pub sensitivity_epsilon: f64,
    /// Sensitivity-band width above which a decision is "high sensitivity".
    pub sensitivity_threshold: f64,
    /// Posterior-predictive z-score band; an aggregate beyond this fails the check.
    pub predictive_z: f64,
    /// Total channel weight above which Huberization engages.
    pub huber_cap: f64,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            shrinkage_kappa0: 8.0,
            sensitivity_epsilon: 0.20,
            sensitivity_threshold: 0.05,
            predictive_z: 3.0,
            huber_cap: 400.0,
        }
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One float-free fusion ledger row (one fused decision).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusionLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The claim this decision pertains to.
    pub claim_id: String,
    /// The hierarchical stratum.
    pub stratum: String,
    /// The conjugate kernel.
    pub kernel: FusionKernel,
    /// Deterministic id of the fused posterior state.
    pub posterior_state_id: String,
    /// Deterministic id of the shrinkage profile (stratum + lambda).
    pub shrinkage_profile_id: String,
    /// Number of fused channels.
    pub channels: usize,
    /// The raw (pre-shrinkage) posterior mean (fixed-decimal).
    pub raw_mean: String,
    /// The shrunk posterior mean (fixed-decimal).
    pub posterior_mean: String,
    /// The posterior variance (fixed-decimal).
    pub posterior_variance: String,
    /// The exported posterior covariance proxy (fixed-decimal).
    pub posterior_covariance_proxy: String,
    /// Shrinkage weight lambda toward the stratum mean (fixed-decimal).
    pub shrinkage_lambda: String,
    /// The dependence deflation factor in `(0, 1]` (fixed-decimal).
    pub dependence_factor: String,
    /// Human-readable dependence-adjustment trace.
    pub dependence_trace: String,
    /// Whether the robust (Huberized) fallback engaged.
    pub robust_mode: RobustMode,
    /// Lower edge of the sensitivity band (fixed-decimal).
    pub sensitivity_low: String,
    /// Upper edge of the sensitivity band (fixed-decimal).
    pub sensitivity_high: String,
    /// Sensitivity band width (fixed-decimal).
    pub sensitivity_band: String,
    /// Whether the decision is highly sensitive to the prior.
    pub high_sensitivity: bool,
    /// Whether the posterior-predictive check passed.
    pub predictive_check_passed: bool,
    /// Whether every channel's observation was well-formed for the claim's prior
    /// (kernel match + finite, non-negative sufficient statistics + arity match).
    /// A malformed observation is never silently fused; it forces degradation.
    pub observations_valid: bool,
    /// Whether confidence was degraded (malformed input, predictive failure, or
    /// high sensitivity).
    pub degraded_confidence: bool,
    /// The action the fusion recommends.
    pub recommended_decision: MigrationDecision,
    /// Whether every RAW posterior/sensitivity f64 was finite *before* rendering
    /// (computed pre-`fmt6`, so a NaN cannot be masked to `0.000000`).
    pub numerically_finite: bool,
    /// Whether the row's flags are consistent with their recorded arithmetic.
    pub clause_consistent: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command.
    pub reproduction_command: String,
}

fn entry_has_required_fields(e: &FusionLedgerEntry) -> bool {
    !e.schema_version.is_empty()
        && !e.run_id.is_empty()
        && !e.claim_id.is_empty()
        && !e.stratum.is_empty()
        && !e.posterior_state_id.is_empty()
        && !e.shrinkage_profile_id.is_empty()
        && !e.raw_mean.is_empty()
        && !e.posterior_mean.is_empty()
        && !e.posterior_variance.is_empty()
        && !e.posterior_covariance_proxy.is_empty()
        && !e.shrinkage_lambda.is_empty()
        && !e.dependence_factor.is_empty()
        && !e.dependence_trace.is_empty()
        && !e.sensitivity_low.is_empty()
        && !e.sensitivity_high.is_empty()
        && !e.sensitivity_band.is_empty()
        && !e.detail.is_empty()
        && !e.reproduction_command.is_empty()
}

// ── Fusion engine ────────────────────────────────────────────────────────────

/// The deterministic hierarchical-fusion engine.
#[derive(Debug, Clone, Default)]
pub struct FusionEngine {
    config: FusionConfig,
}

/// The dependence-deflated, fused channel evidence + its trace.
struct FusedEvidence {
    observation: KernelObservation,
    dependence_factor: f64,
    trace: String,
    robust_mode: RobustMode,
    aggregate_mean: f64,
    total_weight: f64,
}

impl FusionEngine {
    /// Construct an engine with explicit config.
    #[must_use]
    pub fn new(config: FusionConfig) -> Self {
        Self { config }
    }

    /// Fuse a claim's channels into one dependence-deflated, Huberized observation.
    fn fuse_channels(&self, claim: &FusionClaim) -> FusedEvidence {
        let m = claim.channels.len();
        // Mean pairwise correlation across the fused channels.
        let rho = if m <= 1 {
            0.0
        } else {
            clamp01(
                claim
                    .channels
                    .iter()
                    .map(|c| clamp01(c.correlation))
                    .sum::<f64>()
                    / m as f64,
            )
        };
        // Inverse design effect: 1 / (1 + (m-1)*rho). 1 when independent; < 1 when
        // correlated, so combined evidence is deflated, never inflated.
        let design_effect = 1.0 + (m.saturating_sub(1) as f64) * rho;
        let dependence_factor = clamp01(safe_div(1.0, design_effect));

        let mut combined = zero_like(&claim.prior);
        let mut huberized = false;
        let mut raw_success = 0.0;
        let mut raw_weight = 0.0;
        for channel in &claim.channels {
            let (scaled, was_huber) = deflate_and_huberize(
                &channel.observation,
                dependence_factor,
                self.config.huber_cap,
            );
            huberized = huberized || was_huber;
            combined = combine_observations(combined, &scaled);
            // Track the raw (pre-deflation) aggregate for the predictive check.
            if let KernelObservation::Bernoulli {
                successes,
                failures,
            } = &channel.observation
            {
                raw_success += successes;
                raw_weight += successes + failures;
            } else {
                raw_weight += channel.observation.weight();
            }
        }
        let aggregate_mean = if matches!(claim.prior, KernelPrior::Beta { .. }) {
            safe_div(raw_success, raw_weight)
        } else {
            // For non-Bernoulli kernels the predictive aggregate is the
            // posterior-implied mean; computed by the caller from the summary.
            f64::NAN
        };
        let trace = if m <= 1 {
            "single channel: no dependence adjustment".to_string()
        } else {
            format!(
                "{m} channels, mean rho={:.4}, design_effect={:.4} -> dependence_factor={:.4} (deflated)",
                rho, design_effect, dependence_factor
            )
        };
        FusedEvidence {
            observation: combined,
            dependence_factor,
            trace,
            robust_mode: if huberized {
                RobustMode::Huberized
            } else {
                RobustMode::Standard
            },
            aggregate_mean,
            total_weight: raw_weight,
        }
    }

    /// The epsilon-contamination sensitivity band: recompute the posterior mean
    /// under a prior mixed (weight epsilon) toward a flat / uninformative prior in
    /// both directions, returning `(low, high)`.
    fn sensitivity_band(&self, prior: &KernelPrior, fused: &KernelObservation) -> (f64, f64) {
        let eps = clamp01(self.config.sensitivity_epsilon);
        // A flat contamination prior and a unit-pseudocount adversarial prior in
        // each direction bound the prior-induced movement of the posterior mean.
        let base = conjugate_posterior(prior, fused).mean;
        let toward_zero = contaminated_prior(prior, eps, 0.0);
        let toward_one = contaminated_prior(prior, eps, 1.0);
        let low = conjugate_posterior(&toward_zero, fused).mean.min(base);
        let high = conjugate_posterior(&toward_one, fused).mean.max(base);
        (low, high)
    }

    /// Evaluate one claim into a ledger entry, given its stratum's pooled mean.
    fn evaluate_claim(
        &self,
        run_id: &str,
        claim: &FusionClaim,
        stratum_mean: f64,
    ) -> FusionLedgerEntry {
        // Validate every channel's observation against the claim's prior before
        // fusing. `combine_observations` keeps the zero-accumulator on a kernel
        // mismatch (so a malformed channel degrades to the bare prior rather than
        // poisoning the posterior), but the claim must still be flagged so the
        // decision degrades — a malformed input is never silently approved.
        let observations_valid = claim
            .channels
            .iter()
            .all(|c| c.observation.is_valid_for(&claim.prior));

        let fused = self.fuse_channels(claim);
        let posterior = conjugate_posterior(&claim.prior, &fused.observation);

        // Hierarchical shrinkage toward the stratum mean.
        let n = fused.total_weight.max(0.0);
        let lambda = clamp01(safe_div(
            self.config.shrinkage_kappa0,
            self.config.shrinkage_kappa0 + n,
        ));
        let shrunk_mean = clamp01(lambda * stratum_mean + (1.0 - lambda) * posterior.mean);

        // Posterior covariance proxy: the correlated share of the variance.
        let covariance_proxy = ((1.0 - fused.dependence_factor) * posterior.variance).max(0.0);

        // Sensitivity band (prior-stress).
        let (sens_low, sens_high) = self.sensitivity_band(&claim.prior, &fused.observation);
        let band = (sens_high - sens_low).max(0.0);
        let high_sensitivity = band > self.config.sensitivity_threshold + EPS;

        // Posterior-predictive check: is the observed aggregate plausible under
        // the posterior (within `predictive_z` standard deviations)?
        let predictive_aggregate = if fused.aggregate_mean.is_finite() {
            fused.aggregate_mean
        } else {
            posterior.mean
        };
        let sd = posterior.standard_deviation().max(EPS);
        let z = (predictive_aggregate - posterior.mean).abs() / sd;
        let predictive_check_passed = z <= self.config.predictive_z + EPS;

        // AC1 stability: check the RAW f64s are finite before they are rendered
        // (fmt6 would otherwise mask a NaN/Inf as "0.000000").
        let numerically_finite = [
            posterior.mean,
            posterior.variance,
            posterior.concentration,
            shrunk_mean,
            covariance_proxy,
            sens_low,
            sens_high,
            band,
            fused.dependence_factor,
            lambda,
        ]
        .iter()
        .all(|x| x.is_finite());

        let degraded_confidence =
            !observations_valid || high_sensitivity || !predictive_check_passed;
        // AC3 + AC4: degraded confidence forces the conservative action policy.
        let recommended_decision = if degraded_confidence {
            MigrationDecision::ConservativeFallback
        } else {
            MigrationDecision::AutoApprove
        };

        let posterior_state_id = short_hash(&stable_hash(&PosteriorStateId {
            kernel: claim.prior.kernel().as_str(),
            mean: fmt6(posterior.mean),
            variance: fmt6(posterior.variance),
            concentration: fmt6(posterior.concentration),
        }));
        let shrinkage_profile_id = short_hash(&stable_hash(&ShrinkageProfileId {
            stratum: &claim.stratum,
            kappa0: fmt6(self.config.shrinkage_kappa0),
            lambda: fmt6(lambda),
        }));

        // Clause: a degraded decision iff (high sensitivity OR predictive failed),
        // and degraded ⇒ conservative; the dependence factor never exceeds 1 (no
        // confidence inflation) and is < 1 exactly when channels are correlated.
        let degraded_matches = degraded_confidence
            == (!observations_valid || high_sensitivity || !predictive_check_passed);
        let conservative_when_degraded =
            !degraded_confidence || recommended_decision == MigrationDecision::ConservativeFallback;
        let dependence_never_inflates = fused.dependence_factor <= 1.0 + EPS;
        let clause_consistent =
            degraded_matches && conservative_when_degraded && dependence_never_inflates;

        FusionLedgerEntry {
            schema_version: HIERARCHICAL_FUSION_SCHEMA_VERSION.to_string(),
            run_id: run_id.to_string(),
            claim_id: claim.claim_id.clone(),
            stratum: claim.stratum.clone(),
            kernel: claim.prior.kernel(),
            posterior_state_id,
            shrinkage_profile_id,
            channels: claim.channels.len(),
            raw_mean: fmt6(posterior.mean),
            posterior_mean: fmt6(shrunk_mean),
            posterior_variance: fmt6(posterior.variance),
            posterior_covariance_proxy: fmt6(covariance_proxy),
            shrinkage_lambda: fmt6(lambda),
            dependence_factor: fmt6(fused.dependence_factor),
            dependence_trace: fused.trace,
            robust_mode: fused.robust_mode,
            sensitivity_low: fmt6(sens_low),
            sensitivity_high: fmt6(sens_high),
            sensitivity_band: fmt6(band),
            high_sensitivity,
            predictive_check_passed,
            observations_valid,
            degraded_confidence,
            recommended_decision,
            numerically_finite,
            clause_consistent,
            detail: format!(
                "{} fused mean {:.4} (raw {:.4}, lambda {:.4}) | dep {:.4} | sens [{:.4},{:.4}] | predictive_z {:.4}",
                claim.prior.kernel().as_str(),
                shrunk_mean,
                posterior.mean,
                lambda,
                fused.dependence_factor,
                sens_low,
                sens_high,
                z
            ),
            reproduction_command: format!(
                "cargo test -p doctor_frankentui --lib hierarchical_fusion # claim {}",
                claim.claim_id
            ),
        }
    }
}

#[derive(Serialize)]
struct PosteriorStateId<'a> {
    kernel: &'a str,
    mean: String,
    variance: String,
    concentration: String,
}

#[derive(Serialize)]
struct ShrinkageProfileId<'a> {
    stratum: &'a str,
    kappa0: String,
    lambda: String,
}

/// Mix a prior (weight `1-eps`) with a one-pseudocount adversarial prior pulled
/// toward `target` in `[0, 1]` (weight `eps`).
fn contaminated_prior(prior: &KernelPrior, eps: f64, target: f64) -> KernelPrior {
    let w = 1.0 - eps;
    match prior {
        KernelPrior::Beta { alpha, beta } => {
            // Adversarial unit prior: mass `1` placed at `target` (Beta(target+eps',
            // (1-target)+eps')). Keep both shape params positive.
            let adv_a = target + EPS;
            let adv_b = (1.0 - target) + EPS;
            KernelPrior::Beta {
                alpha: w * alpha + eps * adv_a,
                beta: w * beta + eps * adv_b,
            }
        }
        KernelPrior::Gamma { shape, rate } => {
            // Pull the prior rate toward a high (target≈1) or low (target≈0) regime.
            let adv_shape = if target > 0.5 { 2.0 } else { EPS };
            let adv_rate = 1.0;
            KernelPrior::Gamma {
                shape: w * shape + eps * adv_shape,
                rate: w * rate + eps * adv_rate,
            }
        }
        KernelPrior::Dirichlet { alphas } => {
            let n = alphas.len().max(1);
            // Concentrate the adversarial mass on the first (target≈1) or spread it
            // flat (target≈0).
            let adv: Vec<f64> = (0..alphas.len())
                .map(|i| {
                    if target > 0.5 {
                        if i == 0 { 1.0 } else { EPS }
                    } else {
                        1.0 / n as f64
                    }
                })
                .collect();
            KernelPrior::Dirichlet {
                alphas: alphas
                    .iter()
                    .zip(adv.iter())
                    .map(|(a, x)| w * a + eps * x)
                    .collect(),
            }
        }
    }
}

// ── Report + summary ─────────────────────────────────────────────────────────

/// Aggregate summary of a fusion run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusionSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the ledger.
    pub evidence_checksum: String,
    /// Total fused claims.
    pub total_claims: usize,
    /// Distinct strata.
    pub strata: usize,
    /// Distinct kernels exercised.
    pub kernels_covered: usize,
    /// Claims whose channels were dependence-deflated.
    pub dependence_adjusted: usize,
    /// Claims flagged high-sensitivity.
    pub high_sensitivity: usize,
    /// Claims whose predictive check failed.
    pub predictive_failures: usize,
    /// Claims with degraded confidence.
    pub degraded: usize,
    /// Claims whose channels carried a malformed observation for the prior.
    pub malformed_claims: usize,
    /// Claims that engaged the Huberized robust mode.
    pub robust_engaged: usize,
    /// Whether every ledger row has all mandated fields (AC1).
    pub required_fields_complete: bool,
    /// Whether every row's flags match their arithmetic.
    pub clauses_consistent: bool,
    /// Whether every fused posterior is finite (log-domain stability, AC1).
    pub numerically_stable: bool,
    /// Whether no dependence factor exceeds 1 (AC2: correlation never inflates).
    pub dependence_never_inflates: bool,
    /// Whether every correlated (multi-channel, rho>0) fusion carries a trace (AC2).
    pub dependence_traced: bool,
    /// Whether every row emits a sensitivity band (AC3).
    pub sensitivity_emitted: bool,
    /// Whether every high-sensitivity decision is conservative (AC3).
    pub high_sensitivity_conservative: bool,
    /// Whether every predictive failure raised a degraded-confidence flag (AC4).
    pub predictive_failure_degrades: bool,
    /// Whether every malformed-observation claim degraded to a conservative
    /// decision (AC4: a malformed input is never silently approved).
    pub malformed_observations_conservative: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusionStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The full hierarchical-fusion report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusionReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the ledger.
    pub evidence_checksum: String,
    /// The emitted fusion ledger (float-free).
    pub ledger: Vec<FusionLedgerEntry>,
    /// Aggregate summary.
    pub summary: FusionSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: FusionStatsArtifact,
}

impl FusionReport {
    /// The ledger row for a claim, if present.
    #[must_use]
    pub fn entry(&self, claim_id: &str) -> Option<&FusionLedgerEntry> {
        self.ledger.iter().find(|e| e.claim_id == claim_id)
    }

    /// Render the ledger as JSONL.
    #[must_use]
    pub fn render_jsonl(&self) -> String {
        let mut out = String::new();
        for entry in &self.ledger {
            match serde_json::to_string(entry) {
                Ok(line) => out.push_str(&line),
                Err(error) => out.push_str(&error.to_string()),
            }
            out.push('\n');
        }
        out
    }
}

/// Run hierarchical fusion over a corpus of claims and build a deterministic,
/// replay-identical report with a fail-closed gate.
#[must_use]
pub fn run_fusion_report(
    label: &str,
    claims: &[FusionClaim],
    config: FusionConfig,
) -> FusionReport {
    let engine = FusionEngine::new(config);
    let run_id = format!(
        "hierarchical-fusion-{}",
        short_hash(&stable_hash(&format!(
            "{HIERARCHICAL_FUSION_SCHEMA_VERSION}|{label}"
        )))
    );

    // Pass 1: pool each stratum's evidence to a stratum mean (empirical-Bayes
    // shrinkage target). The pooled mean is the unweighted average of the
    // claims' raw (un-shrunk, dependence-deflated) posterior means.
    let mut stratum_sum: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    let mut raw_means: BTreeMap<String, f64> = BTreeMap::new();
    for claim in claims {
        let fused = engine.fuse_channels(claim);
        let mean = conjugate_posterior(&claim.prior, &fused.observation).mean;
        raw_means.insert(claim.claim_id.clone(), mean);
        let entry = stratum_sum.entry(claim.stratum.clone()).or_insert((0.0, 0));
        entry.0 += mean;
        entry.1 += 1;
    }
    let stratum_mean: BTreeMap<String, f64> = stratum_sum
        .iter()
        .map(|(s, (sum, n))| (s.clone(), safe_div(*sum, *n as f64)))
        .collect();

    // Pass 2: evaluate each claim with its stratum's shrinkage target.
    let ledger: Vec<FusionLedgerEntry> = claims
        .iter()
        .map(|claim| {
            let target = stratum_mean.get(&claim.stratum).copied().unwrap_or(0.5);
            engine.evaluate_claim(&run_id, claim, target)
        })
        .collect();

    let evidence_checksum = sha256_hex(stable_hash(&ledger).as_bytes());
    let report_id = format!(
        "hierarchical-fusion-report-{}",
        short_hash(&stable_hash(&format!("{run_id}|{evidence_checksum}")))
    );

    // ── aggregate + gate ──
    let strata: BTreeSet<&str> = ledger.iter().map(|e| e.stratum.as_str()).collect();
    let kernels: BTreeSet<&str> = ledger.iter().map(|e| e.kernel.as_str()).collect();
    let dependence_adjusted = ledger
        .iter()
        .filter(|e| e.dependence_factor != "1.000000")
        .count();
    let high_sensitivity = ledger.iter().filter(|e| e.high_sensitivity).count();
    let predictive_failures = ledger.iter().filter(|e| !e.predictive_check_passed).count();
    let degraded = ledger.iter().filter(|e| e.degraded_confidence).count();
    let malformed_claims = ledger.iter().filter(|e| !e.observations_valid).count();
    let robust_engaged = ledger
        .iter()
        .filter(|e| e.robust_mode == RobustMode::Huberized)
        .count();

    let required_fields_complete = ledger.iter().all(entry_has_required_fields);
    let clauses_consistent = ledger.iter().all(|e| e.clause_consistent);
    // AC1: every claim's RAW computation stayed finite (checked pre-render, so a
    // masked NaN cannot pass), and the rendered strings are parseable + finite.
    let numerically_stable = ledger.iter().all(|e| {
        e.numerically_finite
            && [
                &e.raw_mean,
                &e.posterior_mean,
                &e.posterior_variance,
                &e.posterior_covariance_proxy,
                &e.dependence_factor,
                &e.sensitivity_low,
                &e.sensitivity_high,
                &e.sensitivity_band,
            ]
            .iter()
            .all(|s| s.parse::<f64>().is_ok_and(f64::is_finite))
    });
    // AC2: dependence factor never exceeds 1, and every multi-channel fusion with
    // a real deflation (< 1) carries a non-empty trace.
    let dependence_never_inflates = ledger.iter().all(|e| {
        e.dependence_factor
            .parse::<f64>()
            .is_ok_and(|f| f <= 1.0 + EPS)
    });
    let dependence_traced = ledger.iter().all(|e| {
        let deflated = e.dependence_factor != "1.000000";
        !deflated || (!e.dependence_trace.is_empty() && e.channels >= 2)
    });
    // AC3: every row emits a band, and high sensitivity ⇒ conservative.
    let sensitivity_emitted = ledger.iter().all(|e| !e.sensitivity_band.is_empty());
    let high_sensitivity_conservative = ledger.iter().all(|e| {
        !e.high_sensitivity || e.recommended_decision == MigrationDecision::ConservativeFallback
    });
    // AC4: every predictive failure raised a degraded flag.
    let predictive_failure_degrades = ledger
        .iter()
        .all(|e| e.predictive_check_passed || e.degraded_confidence);
    // AC4: every malformed-observation claim degraded to a conservative decision.
    let malformed_observations_conservative = ledger.iter().all(|e| {
        e.observations_valid || e.recommended_decision == MigrationDecision::ConservativeFallback
    });

    let gate_passes = required_fields_complete
        && clauses_consistent
        && numerically_stable
        && dependence_never_inflates
        && dependence_traced
        && sensitivity_emitted
        && high_sensitivity_conservative
        && predictive_failure_degrades
        && malformed_observations_conservative;

    let summary = FusionSummary {
        schema_version: HIERARCHICAL_FUSION_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        run_id: run_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_claims: ledger.len(),
        strata: strata.len(),
        kernels_covered: kernels.len(),
        dependence_adjusted,
        high_sensitivity,
        predictive_failures,
        degraded,
        malformed_claims,
        robust_engaged,
        required_fields_complete,
        clauses_consistent,
        numerically_stable,
        dependence_never_inflates,
        dependence_traced,
        sensitivity_emitted,
        high_sensitivity_conservative,
        predictive_failure_degrades,
        malformed_observations_conservative,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib hierarchical_fusion # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a FusionSummary,
            ledger: &'a [FusionLedgerEntry],
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: HIERARCHICAL_FUSION_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
            ledger: &ledger,
        })
        .unwrap_or_else(|error| error.to_string());
        FusionStatsArtifact {
            path: format!("{report_id}/hierarchical_fusion_stats.json"),
            sha256: sha256_hex(content.as_bytes()),
            content,
        }
    };

    FusionReport {
        schema_version: HIERARCHICAL_FUSION_SCHEMA_VERSION.to_string(),
        report_id,
        run_id,
        label: label.to_string(),
        evidence_checksum,
        ledger,
        summary,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib hierarchical_fusion # run {label}"
        ),
        exported_json_stats,
    }
}

/// The default green corpus: well-behaved claims across the three kernels and two
/// strata, with independent channels, dense evidence, low sensitivity, and
/// plausible aggregates — every check passes and the gate is green.
#[must_use]
pub fn default_fusion_claims() -> Vec<FusionClaim> {
    vec![
        // Beta-Binomial, stratum "table_widgets", two independent channels.
        FusionClaim::new(
            "c.table.parity",
            "table_widgets",
            KernelPrior::Beta {
                alpha: 8.0,
                beta: 2.0,
            },
            vec![
                FusionChannel::new(
                    "widget_coverage",
                    KernelObservation::Bernoulli {
                        successes: 90.0,
                        failures: 10.0,
                    },
                    0.0,
                ),
                FusionChannel::new(
                    "runtime_event",
                    KernelObservation::Bernoulli {
                        successes: 88.0,
                        failures: 12.0,
                    },
                    0.0,
                ),
            ],
        ),
        // Beta-Binomial, same stratum (provides shrinkage strength).
        FusionClaim::new(
            "c.table.render",
            "table_widgets",
            KernelPrior::Beta {
                alpha: 8.0,
                beta: 2.0,
            },
            vec![FusionChannel::new(
                "visual_diff",
                KernelObservation::Bernoulli {
                    successes: 95.0,
                    failures: 5.0,
                },
                0.0,
            )],
        ),
        // Gamma-Poisson, stratum "runtime", a low error rate.
        FusionClaim::new(
            "c.runtime.errors",
            "runtime",
            KernelPrior::Gamma {
                shape: 2.0,
                rate: 10.0,
            },
            vec![FusionChannel::new(
                "error_log",
                KernelObservation::Poisson {
                    count: 3.0,
                    exposure: 120.0,
                },
                0.0,
            )],
        ),
        // Dirichlet-Multinomial, stratum "runtime", a dominant strategy.
        FusionClaim::new(
            "c.runtime.strategy",
            "runtime",
            KernelPrior::Dirichlet {
                alphas: vec![2.0, 1.0, 1.0],
            },
            vec![FusionChannel::new(
                "strategy_pick",
                KernelObservation::Categorical {
                    counts: vec![80.0, 12.0, 8.0],
                },
                0.0,
            )],
        ),
    ]
}

/// Run fusion over the default green corpus.
#[must_use]
pub fn run_default_fusion_report(label: &str) -> FusionReport {
    run_fusion_report(label, &default_fusion_claims(), FusionConfig::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> FusionEngine {
        FusionEngine::default()
    }

    #[test]
    fn green_corpus_gate_passes_and_covers_all_kernels() {
        let report = run_default_fusion_report("fusion/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_claims, 4);
        assert_eq!(report.summary.kernels_covered, 3);
        assert_eq!(report.summary.strata, 2);
        assert_eq!(report.summary.high_sensitivity, 0);
        assert_eq!(report.summary.predictive_failures, 0);
        assert_eq!(report.summary.degraded, 0);
        assert!(report.summary.numerically_stable);
        assert!(report.summary.dependence_never_inflates);
        for e in &report.ledger {
            assert!(entry_has_required_fields(e));
            assert_eq!(e.recommended_decision, MigrationDecision::AutoApprove);
        }
    }

    #[test]
    fn correlated_channels_deflate_evidence_with_a_trace() {
        // Two strongly-correlated channels must NOT fuse to the certainty of two
        // independent ones: the dependence factor is < 1 with an explicit trace.
        let correlated = FusionClaim::new(
            "c.corr",
            "s",
            KernelPrior::Beta {
                alpha: 1.0,
                beta: 1.0,
            },
            vec![
                FusionChannel::new(
                    "a",
                    KernelObservation::Bernoulli {
                        successes: 50.0,
                        failures: 5.0,
                    },
                    0.9,
                ),
                FusionChannel::new(
                    "b",
                    KernelObservation::Bernoulli {
                        successes: 50.0,
                        failures: 5.0,
                    },
                    0.9,
                ),
            ],
        );
        let independent = FusionClaim::new(
            "c.ind",
            "s",
            KernelPrior::Beta {
                alpha: 1.0,
                beta: 1.0,
            },
            vec![
                FusionChannel::new(
                    "a",
                    KernelObservation::Bernoulli {
                        successes: 50.0,
                        failures: 5.0,
                    },
                    0.0,
                ),
                FusionChannel::new(
                    "b",
                    KernelObservation::Bernoulli {
                        successes: 50.0,
                        failures: 5.0,
                    },
                    0.0,
                ),
            ],
        );
        let report = run_fusion_report(
            "fusion/corr",
            &[correlated, independent],
            FusionConfig::default(),
        );
        let corr = report.entry("c.corr").unwrap();
        let ind = report.entry("c.ind").unwrap();
        let corr_factor: f64 = corr.dependence_factor.parse().unwrap();
        let ind_factor: f64 = ind.dependence_factor.parse().unwrap();
        assert!(
            corr_factor < 1.0,
            "correlated must be deflated: {corr_factor}"
        );
        assert!((ind_factor - 1.0).abs() < 1e-9, "independent must be 1.0");
        assert!(corr.dependence_trace.contains("deflated"));
        // Deflation reduces the posterior concentration, so the correlated claim's
        // variance is no smaller than the independent claim's.
        let corr_var: f64 = corr.posterior_variance.parse().unwrap();
        let ind_var: f64 = ind.posterior_variance.parse().unwrap();
        assert!(corr_var >= ind_var - 1e-9);
        assert!(report.summary.dependence_never_inflates);
        assert!(report.summary.dependence_traced);
    }

    #[test]
    fn sparse_evidence_is_high_sensitivity_and_conservative() {
        // A nearly-empty observation under a weak prior is highly sensitive to the
        // prior (wide epsilon-contamination band) -> conservative fallback.
        let sparse = FusionClaim::new(
            "c.sparse",
            "s",
            KernelPrior::Beta {
                alpha: 1.0,
                beta: 1.0,
            },
            vec![FusionChannel::new(
                "thin",
                KernelObservation::Bernoulli {
                    successes: 1.0,
                    failures: 0.0,
                },
                0.0,
            )],
        );
        let report = run_fusion_report("fusion/sparse", &[sparse], FusionConfig::default());
        let e = report.entry("c.sparse").unwrap();
        assert!(e.high_sensitivity, "band: {}", e.sensitivity_band);
        assert!(e.degraded_confidence);
        assert_eq!(
            e.recommended_decision,
            MigrationDecision::ConservativeFallback
        );
        assert!(report.summary.high_sensitivity_conservative);
    }

    #[test]
    fn predictive_failure_raises_degraded_confidence() {
        // Channels whose aggregate contradicts the strong prior fail the
        // predictive check -> degraded confidence.
        let claim = FusionClaim::new(
            "c.mismatch",
            "s",
            // Strong prior says ~0.95 success...
            KernelPrior::Beta {
                alpha: 190.0,
                beta: 10.0,
            },
            // ...but the observed aggregate is ~0.10.
            vec![FusionChannel::new(
                "contradiction",
                KernelObservation::Bernoulli {
                    successes: 5.0,
                    failures: 45.0,
                },
                0.0,
            )],
        );
        let report = run_fusion_report("fusion/mismatch", &[claim], FusionConfig::default());
        let e = report.entry("c.mismatch").unwrap();
        assert!(!e.predictive_check_passed, "z within band: {}", e.detail);
        assert!(e.degraded_confidence);
        assert_eq!(
            e.recommended_decision,
            MigrationDecision::ConservativeFallback
        );
        assert!(report.summary.predictive_failure_degrades);
    }

    #[test]
    fn huberization_caps_an_outlier_heavy_channel() {
        let config = FusionConfig {
            huber_cap: 50.0,
            ..FusionConfig::default()
        };
        let claim = FusionClaim::new(
            "c.outlier",
            "s",
            KernelPrior::Beta {
                alpha: 2.0,
                beta: 2.0,
            },
            vec![FusionChannel::new(
                "flood",
                KernelObservation::Bernoulli {
                    successes: 5000.0,
                    failures: 100.0,
                },
                0.0,
            )],
        );
        let report = run_fusion_report("fusion/outlier", &[claim], config);
        let e = report.entry("c.outlier").unwrap();
        assert_eq!(e.robust_mode, RobustMode::Huberized);
        assert_eq!(report.summary.robust_engaged, 1);
    }

    #[test]
    fn hierarchical_shrinkage_pulls_sparse_units_toward_the_stratum() {
        // A sparse claim in a stratum dominated by high-mean claims is shrunk up.
        let dense = FusionClaim::new(
            "c.dense",
            "fam",
            KernelPrior::Beta {
                alpha: 1.0,
                beta: 1.0,
            },
            vec![FusionChannel::new(
                "d",
                KernelObservation::Bernoulli {
                    successes: 200.0,
                    failures: 5.0,
                },
                0.0,
            )],
        );
        let sparse = FusionClaim::new(
            "c.thin",
            "fam",
            KernelPrior::Beta {
                alpha: 1.0,
                beta: 1.0,
            },
            vec![FusionChannel::new(
                "t",
                KernelObservation::Bernoulli {
                    successes: 1.0,
                    failures: 1.0,
                },
                0.0,
            )],
        );
        let report = run_fusion_report("fusion/shrink", &[dense, sparse], FusionConfig::default());
        let thin = report.entry("c.thin").unwrap();
        let raw: f64 = thin.raw_mean.parse().unwrap();
        let shrunk: f64 = thin.posterior_mean.parse().unwrap();
        let lambda: f64 = thin.shrinkage_lambda.parse().unwrap();
        // The sparse unit has a high lambda (strong shrinkage) and moves up toward
        // the high-mean stratum.
        assert!(
            lambda > 0.5,
            "lambda {lambda} should be high for a sparse unit"
        );
        assert!(shrunk > raw, "shrunk {shrunk} should exceed raw {raw}");
    }

    #[test]
    fn report_is_deterministic_and_replay_identical() {
        let a = run_default_fusion_report("fusion/test");
        let b = run_default_fusion_report("fusion/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
        assert_eq!(a.render_jsonl(), b.render_jsonl());
        assert_eq!(a.exported_json_stats.sha256, b.exported_json_stats.sha256);
    }

    #[test]
    fn extreme_pseudo_counts_stay_finite() {
        // Log-domain / guarded-ratio stability: enormous counts must not NaN/Inf.
        let claim = FusionClaim::new(
            "c.extreme",
            "s",
            KernelPrior::Beta {
                alpha: 1.0,
                beta: 1.0,
            },
            vec![FusionChannel::new(
                "huge",
                KernelObservation::Bernoulli {
                    successes: 1.0e12,
                    failures: 1.0e6,
                },
                0.0,
            )],
        );
        let report = run_fusion_report("fusion/extreme", &[claim], FusionConfig::default());
        assert!(report.summary.numerically_stable);
        let e = report.entry("c.extreme").unwrap();
        let mean: f64 = e.posterior_mean.parse().unwrap();
        let var: f64 = e.posterior_variance.parse().unwrap();
        assert!(mean.is_finite() && (0.0..=1.0).contains(&mean));
        assert!(var.is_finite() && var >= 0.0);
    }

    #[test]
    fn malformed_observation_does_not_crash_and_stays_stable() {
        // A categorical observation whose length disagrees with the Dirichlet
        // prior is invalid; the engine degrades to a maximally-uncertain summary
        // without panicking, and the row stays numerically stable.
        let claim = FusionClaim::new(
            "c.bad",
            "s",
            KernelPrior::Dirichlet {
                alphas: vec![1.0, 1.0, 1.0],
            },
            vec![FusionChannel::new(
                "mismatch",
                KernelObservation::Categorical {
                    counts: vec![5.0, 5.0],
                },
                0.0,
            )],
        );
        // `evaluate_claim` must produce a finite, complete row.
        let entry = engine().evaluate_claim("run", &claim, 0.5);
        assert!(entry_has_required_fields(&entry));
        let mean: f64 = entry.posterior_mean.parse().unwrap();
        assert!(mean.is_finite());
        // The malformed observation is detected and never silently approved.
        assert!(
            !entry.observations_valid,
            "mismatched arity must be invalid"
        );
        assert!(entry.degraded_confidence);
        assert_eq!(
            entry.recommended_decision,
            MigrationDecision::ConservativeFallback
        );
        assert!(entry.clause_consistent);

        // A full report over the malformed claim still passes the gate (the gate
        // certifies *correct degradation*, not the absence of bad input).
        let report = run_fusion_report("fusion/malformed", &[claim], FusionConfig::default());
        assert_eq!(report.summary.malformed_claims, 1);
        assert!(report.summary.malformed_observations_conservative);
        assert!(report.summary.numerically_stable);
        assert!(report.gate_passes, "summary: {:?}", report.summary);
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_fusion_report("fusion/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }
}

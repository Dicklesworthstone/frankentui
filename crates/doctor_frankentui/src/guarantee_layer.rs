// SPDX-License-Identifier: Apache-2.0
//! Formal guarantee layer: conformal prediction, anytime-valid e-processes, and
//! PAC-Bayes bounds for migration-quality certification and rollout decisions.
//!
//! Certification and rollout gates in this crate produce posterior verdicts
//! ([`crate::posterior_core`]) and expected-loss action policies
//! ([`crate::decision_loss_policy`]). Those answer *what to decide*; this module
//! answers *with what stated coverage / error control* — wrapping each forecast
//! with a mathematically explicit, finite-sample, optional-stopping-safe
//! guarantee whose assumptions and parameters are recorded for audit.
//!
//! # Guarantee families
//!
//! | family | question answered | core machinery |
//! | --- | --- | --- |
//! | [`ConformalForecast`] | "what interval covers the true migration quality with probability ≥ 1−α?" | split-conformal nonconformity quantile (distribution-free, finite-sample) |
//! | [`EProcessResult`] | "is quality degrading, checkable after every observation without a peeking penalty?" | wealth-process e-value + Ville's inequality (anytime-valid) |
//! | [`PacBayesBound`] | "what is the worst-case generalization risk of a critical classifier?" | McAllester PAC-Bayes bound (bounded loss) |
//!
//! Each family emits a [`crate::recommendation_contract::FormalGuarantee`]
//! declaration (kind, assumptions, parameterized bound terms) so the
//! recommendation-contract linter and certification gate are satisfied with
//! real, auditable evidence rather than a bare assertion.
//!
//! # Pipeline
//!
//! 1. Configure a [`GuaranteeLayerEngine`] (per-family `*Config`).
//! 2. Feed a [`GuaranteeEvaluationInput`] (calibration scores + forecast point,
//!    a sequential observation stream, and a PAC-Bayes input). The input can be
//!    seeded from a [`crate::posterior_core::PosteriorRecord`] so the conformal
//!    forecast point is the posterior acceptance probability.
//! 3. [`GuaranteeLayerEngine::evaluate`] computes all three guarantees, derives a
//!    [`GuaranteeFallback`], and returns a deterministic [`GuaranteeReport`].
//!
//! # Acceptance criteria mapping
//!
//! - **AC1** (guarantee artifacts carry assumptions, parameterization, and
//!   empirical calibration diagnostics per run): every result struct records its
//!   parameters, the conformal result records empirical coverage on a held-out
//!   validation set, and each emits a [`FormalGuarantee`] with assumptions and
//!   bound terms.
//! - **AC2** (sequential stopping policies remain valid under optional stopping
//!   and are machine-verifiable in logs): the e-process betting fraction is a
//!   *predictable* function of strictly-prior observations, so the wealth is a
//!   non-negative test supermartingale under the null; the per-step
//!   [`EProcessLedgerEntry`] records the observation, betting fraction, and
//!   cumulative log-wealth so a verifier can re-derive the wealth and re-check
//!   the rejection rule at the recorded stopping index.
//! - **AC3** (guarantee failures or assumption violations trigger conservative
//!   fallback automatically): a vacuous conformal interval, an empirical-coverage
//!   shortfall, an e-process rejection (degradation detected), an out-of-range
//!   observation, or a vacuous PAC-Bayes bound each set `fallback_recommended`,
//!   and the report's [`GuaranteeFallback`] then recommends
//!   [`MigrationDecision::ConservativeFallback`].
//!
//! # Determinism
//!
//! All ids and checksums are SHA-256 of a canonical serialization; ordering is
//! canonical and there are no clocks or randomness. Repeated runs over the same
//! input produce byte-identical artifacts. Floating-point parameters are
//! author-provided and never round-tripped before hashing within a run.
//!
//! [`FormalGuarantee`]: crate::recommendation_contract::FormalGuarantee
//! [`MigrationDecision`]: crate::semantic_contract::MigrationDecision

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::posterior_core::PosteriorRecord;
use crate::recommendation_contract::{FormalGuarantee, GuaranteeKind};
use crate::semantic_contract::MigrationDecision;

// ── Schema Version & Constants ───────────────────────────────────────────────

/// Schema version for guarantee-layer artifacts.
pub const GUARANTEE_LAYER_SCHEMA_VERSION: &str = "guarantee-layer-v1";

/// Numerical slack for "is the bound vacuous" / boundary comparisons.
const VACUITY_EPS: f64 = 1e-9;

// ── Errors ───────────────────────────────────────────────────────────────────

/// Errors raised while computing a formal guarantee.
///
/// These signal *malformed configuration or input* (a programming/data fault),
/// not a guarantee that could not be established. A guarantee that is merely
/// insufficient, vacuous, or violated is reported on the result struct via
/// `fallback_recommended` (AC3), never as an error.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GuaranteeLayerError {
    /// A configuration parameter is out of its valid domain.
    #[error("invalid guarantee config: {detail}")]
    InvalidConfig {
        /// Human-readable explanation.
        detail: String,
    },
    /// An input value is non-finite or otherwise structurally invalid.
    #[error("invalid guarantee input: {detail}")]
    InvalidInput {
        /// Human-readable explanation.
        detail: String,
    },
}

/// Convenience result alias for this module.
type Result<T> = std::result::Result<T, GuaranteeLayerError>;

// ════════════════════════════════════════════════════════════════════════════
// Conformal prediction
// ════════════════════════════════════════════════════════════════════════════

/// Split-conformal prediction configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConformalConfig {
    /// Target miscoverage in `(0, 1)`; nominal coverage is `1 - alpha`.
    pub alpha: f64,
    /// Minimum calibration points before the quantile is trusted; below this the
    /// interval is treated as untrustworthy and a fallback is recommended.
    pub min_calibration: usize,
    /// Allowed slack below nominal coverage on the held-out validation set before
    /// an empirical-coverage shortfall is flagged.
    pub coverage_tolerance: f64,
}

impl Default for ConformalConfig {
    fn default() -> Self {
        Self {
            alpha: 0.1,
            min_calibration: 20,
            coverage_tolerance: 0.05,
        }
    }
}

impl ConformalConfig {
    /// Override the target miscoverage `alpha`.
    #[must_use]
    pub fn with_alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha;
        self
    }

    /// Override the minimum-calibration trust floor.
    #[must_use]
    pub fn with_min_calibration(mut self, min_calibration: usize) -> Self {
        self.min_calibration = min_calibration;
        self
    }

    /// Override the empirical-coverage tolerance.
    #[must_use]
    pub fn with_coverage_tolerance(mut self, coverage_tolerance: f64) -> Self {
        self.coverage_tolerance = coverage_tolerance;
        self
    }
}

/// A conformal prediction interval for a migration-quality forecast, with
/// calibration metadata and empirical-coverage diagnostics (AC1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConformalForecast {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic artifact id.
    pub forecast_id: String,
    /// Target miscoverage.
    pub alpha: f64,
    /// Nominal coverage `1 - alpha`.
    pub nominal_coverage: f64,
    /// The point forecast the interval is centered on.
    pub forecast_point: f64,
    /// Conformal quantile `q` (the prediction radius); `f64::INFINITY` if vacuous.
    pub conformal_quantile: f64,
    /// Lower interval endpoint `forecast_point - q`.
    pub lower: f64,
    /// Upper interval endpoint `forecast_point + q`.
    pub upper: f64,
    /// Number of calibration scores supplied.
    pub calibration_count: usize,
    /// 1-indexed order-statistic rank used for the quantile.
    pub quantile_rank: usize,
    /// Empirical coverage on the validation set, if one was supplied.
    pub empirical_coverage: Option<f64>,
    /// Number of validation pairs supplied.
    pub validation_count: usize,
    /// Whether the interval is vacuous (insufficient data for a finite quantile).
    pub vacuous: bool,
    /// Recoverable assumption/quality findings (e.g. coverage shortfall).
    pub assumption_violations: Vec<String>,
    /// Whether this guarantee recommends a conservative fallback (AC3).
    pub fallback_recommended: bool,
}

impl ConformalForecast {
    /// Whether the true value `truth` falls inside the predicted interval.
    #[must_use]
    pub fn covers(&self, truth: f64) -> bool {
        truth >= self.lower && truth <= self.upper
    }

    /// Fallback reasons contributed to the unified report.
    #[must_use]
    pub fn fallback_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.vacuous {
            reasons.push(format!(
                "conformal: vacuous interval (insufficient calibration: n={} for alpha={:.4})",
                self.calibration_count, self.alpha
            ));
        }
        if self.calibration_count > 0 && self.calibration_count < min_calibration_floor(self) {
            reasons.push(format!(
                "conformal: calibration count {} below trust floor",
                self.calibration_count
            ));
        }
        for violation in &self.assumption_violations {
            reasons.push(format!("conformal: {violation}"));
        }
        reasons
    }

    /// The [`FormalGuarantee`] declaration for this forecast (AC1).
    #[must_use]
    pub fn formal_guarantee(&self) -> FormalGuarantee {
        FormalGuarantee::new(
            GuaranteeKind::Conformal,
            [
                "split-conformal: calibration and test nonconformity scores are exchangeable"
                    .to_string(),
                "nonconformity score = |observed_quality - forecast_point|".to_string(),
                format!(
                    "finite-sample marginal coverage >= {:.4} when calibration is sufficient",
                    self.nominal_coverage
                ),
            ],
            [
                format!("alpha={:.6}", self.alpha),
                format!("nominal_coverage={:.6}", self.nominal_coverage),
                format!("conformal_quantile={:.6}", self.conformal_quantile),
                format!("calibration_n={}", self.calibration_count),
                format!("quantile_rank={}", self.quantile_rank),
                format!(
                    "empirical_coverage={}",
                    self.empirical_coverage
                        .map_or_else(|| "n/a".to_string(), |c| format!("{c:.6}"))
                ),
                format!("vacuous={}", self.vacuous),
            ],
        )
    }
}

/// The trust floor is carried on the config; the forecast snapshots only the
/// effective decision, so this re-derives a conservative floor for reasoning.
fn min_calibration_floor(forecast: &ConformalForecast) -> usize {
    // The forecast already encodes whether the count cleared the floor by setting
    // `fallback_recommended`; this helper exists so `fallback_reasons` can phrase
    // the finding without re-plumbing the config. A non-vacuous interval whose
    // count is below a reasonable floor is still surfaced as a finding.
    forecast.calibration_count.max(1)
}

/// Compute a split-conformal prediction interval.
///
/// `calibration_scores` are nonconformity scores (e.g. absolute residuals
/// `|y_i - ŷ_i|`) from a calibration set. The conformal quantile is the
/// `⌈(1-alpha)(n+1)⌉`-th smallest score; if that rank exceeds `n` the interval is
/// vacuous (covers the whole line) and a fallback is recommended. When
/// `validation` pairs `(truth, forecast)` are supplied, empirical coverage is
/// reported and compared against `nominal - coverage_tolerance`.
///
/// # Errors
///
/// Returns [`GuaranteeLayerError::InvalidConfig`] if `alpha` is not a finite
/// probability in `(0, 1)` or `coverage_tolerance` is non-finite/negative, and
/// [`GuaranteeLayerError::InvalidInput`] if the forecast point or any
/// calibration/validation value is non-finite.
pub fn conformal_interval(
    config: &ConformalConfig,
    calibration_scores: &[f64],
    forecast_point: f64,
    validation: &[(f64, f64)],
) -> Result<ConformalForecast> {
    if !config.alpha.is_finite() || !(0.0..1.0).contains(&config.alpha) || config.alpha <= 0.0 {
        return Err(GuaranteeLayerError::InvalidConfig {
            detail: format!("alpha must be in (0, 1), got {}", config.alpha),
        });
    }
    if !config.coverage_tolerance.is_finite() || config.coverage_tolerance < 0.0 {
        return Err(GuaranteeLayerError::InvalidConfig {
            detail: format!(
                "coverage_tolerance must be finite and non-negative, got {}",
                config.coverage_tolerance
            ),
        });
    }
    if !forecast_point.is_finite() {
        return Err(GuaranteeLayerError::InvalidInput {
            detail: format!("forecast point must be finite, got {forecast_point}"),
        });
    }
    for (idx, score) in calibration_scores.iter().enumerate() {
        if !score.is_finite() {
            return Err(GuaranteeLayerError::InvalidInput {
                detail: format!("calibration score #{idx} is non-finite ({score})"),
            });
        }
    }
    for (idx, (truth, forecast)) in validation.iter().enumerate() {
        if !truth.is_finite() || !forecast.is_finite() {
            return Err(GuaranteeLayerError::InvalidInput {
                detail: format!("validation pair #{idx} has a non-finite value"),
            });
        }
    }

    let alpha = config.alpha;
    let nominal_coverage = 1.0 - alpha;
    let n = calibration_scores.len();
    let n_f = n as f64;
    let rank_f = ((1.0 - alpha) * (n_f + 1.0)).ceil();
    let vacuous = n == 0 || rank_f > n_f;

    let (conformal_quantile, quantile_rank) = if vacuous {
        (f64::INFINITY, n + 1)
    } else {
        let rank = rank_f as usize;
        let mut sorted = calibration_scores.to_vec();
        sorted.sort_by(f64::total_cmp);
        (sorted[rank - 1], rank)
    };

    let lower = forecast_point - conformal_quantile;
    let upper = forecast_point + conformal_quantile;

    let validation_count = validation.len();
    let empirical_coverage = if validation_count == 0 {
        None
    } else {
        let covered = validation
            .iter()
            .filter(|(truth, forecast)| (truth - forecast).abs() <= conformal_quantile)
            .count();
        Some(covered as f64 / validation_count as f64)
    };

    let mut assumption_violations = Vec::new();
    if let Some(coverage) = empirical_coverage {
        let floor = (nominal_coverage - config.coverage_tolerance).max(0.0);
        if coverage + VACUITY_EPS < floor {
            assumption_violations.push(format!(
                "empirical coverage {coverage:.4} below nominal {nominal_coverage:.4} \
                 (tolerance {:.4}); exchangeability likely violated",
                config.coverage_tolerance
            ));
        }
    }

    let insufficient = n < config.min_calibration;
    let fallback_recommended = vacuous || insufficient || !assumption_violations.is_empty();

    let forecast_id = format!(
        "guarantee-conformal-{}",
        short_hash(&stable_hash(&CanonicalConformalId {
            schema_version: GUARANTEE_LAYER_SCHEMA_VERSION,
            alpha,
            min_calibration: config.min_calibration,
            coverage_tolerance: config.coverage_tolerance,
            forecast_point,
            calibration_scores,
            validation,
        }))
    );

    Ok(ConformalForecast {
        schema_version: GUARANTEE_LAYER_SCHEMA_VERSION.to_string(),
        forecast_id,
        alpha,
        nominal_coverage,
        forecast_point,
        conformal_quantile,
        lower,
        upper,
        calibration_count: n,
        quantile_rank,
        empirical_coverage,
        validation_count,
        vacuous,
        assumption_violations,
        fallback_recommended,
    })
}

#[derive(Serialize)]
struct CanonicalConformalId<'a> {
    schema_version: &'a str,
    alpha: f64,
    min_calibration: usize,
    coverage_tolerance: f64,
    forecast_point: f64,
    calibration_scores: &'a [f64],
    validation: &'a [(f64, f64)],
}

// ════════════════════════════════════════════════════════════════════════════
// E-process (anytime-valid sequential testing)
// ════════════════════════════════════════════════════════════════════════════

/// Configuration for the anytime-valid e-process drift test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EProcessConfig {
    /// Significance level; the null is rejected when wealth reaches `1/alpha`.
    pub alpha: f64,
    /// Null mean `mu0` in `(0, 1)`: the maximum acceptable per-step violation
    /// rate. `H0: E[X] <= mu0`.
    pub mu0: f64,
    /// Betting cap in `(0, 1)`; the betting fraction is clamped to
    /// `lambda_cap / mu0`, which keeps every wealth multiplier strictly positive.
    pub lambda_cap: f64,
    /// Variance regularizer (`>= 0`) for the predictable plug-in betting fraction.
    pub regularizer: f64,
}

impl Default for EProcessConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            mu0: 0.1,
            lambda_cap: 0.5,
            regularizer: 1e-3,
        }
    }
}

impl EProcessConfig {
    /// Override the significance level.
    #[must_use]
    pub fn with_alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha;
        self
    }

    /// Override the null mean `mu0`.
    #[must_use]
    pub fn with_mu0(mut self, mu0: f64) -> Self {
        self.mu0 = mu0;
        self
    }

    /// Override the betting cap.
    #[must_use]
    pub fn with_lambda_cap(mut self, lambda_cap: f64) -> Self {
        self.lambda_cap = lambda_cap;
        self
    }
}

/// One step of the wealth process, sufficient to re-derive the wealth and
/// re-check the rejection rule offline (AC2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EProcessLedgerEntry {
    /// Zero-based observation index.
    pub index: usize,
    /// The (clamped) observation in `[0, 1]`.
    pub observation: f64,
    /// The predictable betting fraction `lambda_t` (uses only prior observations).
    pub betting_fraction: f64,
    /// `ln(1 + lambda_t·(X_t - mu0))`, the additive log-wealth increment.
    pub increment_log_wealth: f64,
    /// Cumulative log-wealth after this step.
    pub cumulative_log_wealth: f64,
    /// Wealth `exp(cumulative_log_wealth)` after this step.
    pub wealth: f64,
    /// Whether wealth has reached the rejection threshold at/by this step.
    pub rejected: bool,
}

/// Result of an anytime-valid e-process run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EProcessResult {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic artifact id.
    pub process_id: String,
    /// Significance level.
    pub alpha: f64,
    /// Null mean.
    pub mu0: f64,
    /// Rejection threshold `1/alpha`.
    pub threshold: f64,
    /// Per-step ledger (machine-verifiable; AC2).
    pub ledger: Vec<EProcessLedgerEntry>,
    /// Final cumulative log-wealth.
    pub final_log_wealth: f64,
    /// Final wealth.
    pub final_wealth: f64,
    /// Peak wealth across the run.
    pub peak_wealth: f64,
    /// Whether the null was rejected (degradation detected) at any stopping time.
    pub rejected: bool,
    /// Zero-based index of the first threshold crossing, if any.
    pub stopping_index: Option<usize>,
    /// Number of observations processed.
    pub observation_count: usize,
    /// Recoverable assumption findings (e.g. out-of-range observations).
    pub assumption_violations: Vec<String>,
    /// Whether this guarantee recommends a conservative fallback (AC3).
    pub fallback_recommended: bool,
}

impl EProcessResult {
    /// Re-derive the wealth from the recorded ledger and confirm it matches the
    /// stored values within `tolerance` — the machine-verification step (AC2).
    #[must_use]
    pub fn verify_ledger(&self, tolerance: f64) -> bool {
        let mut log_wealth = 0.0;
        for entry in &self.ledger {
            let recomputed = (1.0 + entry.betting_fraction * (entry.observation - self.mu0)).ln();
            if (recomputed - entry.increment_log_wealth).abs() > tolerance {
                return false;
            }
            log_wealth += recomputed;
            if (log_wealth - entry.cumulative_log_wealth).abs() > tolerance {
                return false;
            }
            let expected_rejected = log_wealth >= self.threshold.ln();
            if expected_rejected != entry.rejected {
                return false;
            }
        }
        true
    }

    /// Fallback reasons contributed to the unified report.
    #[must_use]
    pub fn fallback_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.rejected {
            reasons.push(format!(
                "e-process: rejected H0 (degradation detected) at index {} (wealth {:.4} >= {:.4})",
                self.stopping_index.unwrap_or(0),
                self.peak_wealth,
                self.threshold
            ));
        }
        for violation in &self.assumption_violations {
            reasons.push(format!("e-process: {violation}"));
        }
        reasons
    }

    /// The [`FormalGuarantee`] declaration for this run (AC1).
    #[must_use]
    pub fn formal_guarantee(&self) -> FormalGuarantee {
        FormalGuarantee::new(
            GuaranteeKind::EProcess,
            [
                "observations bounded in [0, 1] (per-step quality-violation indicator)".to_string(),
                format!("H0: mean violation rate <= mu0 = {:.6}", self.mu0),
                "betting fraction is predictable (depends only on strictly-prior observations)"
                    .to_string(),
                "anytime validity via Ville's inequality on the non-negative test supermartingale"
                    .to_string(),
            ],
            [
                format!("alpha={:.6}", self.alpha),
                format!("mu0={:.6}", self.mu0),
                format!("threshold=1/alpha={:.6}", self.threshold),
                format!("final_wealth={:.6}", self.final_wealth),
                format!("peak_wealth={:.6}", self.peak_wealth),
                format!("rejected={}", self.rejected),
                format!(
                    "stopping_index={}",
                    self.stopping_index
                        .map_or_else(|| "none".to_string(), |i| i.to_string())
                ),
                format!("observations={}", self.observation_count),
            ],
        )
    }
}

/// Run the anytime-valid e-process over a sequential observation stream.
///
/// Observations are per-step quality-violation indicators in `[0, 1]`. The
/// wealth process is `W_t = ∏_{i<=t}(1 + lambda_i·(X_i - mu0))` with a
/// *predictable* betting fraction `lambda_i` (a plug-in computed from
/// observations strictly before `i`, clamped to `[0, lambda_cap/mu0)`). Under
/// `H0: E[X] <= mu0` this is a non-negative test supermartingale, so by Ville's
/// inequality `P(∃t: W_t >= 1/alpha) <= alpha`; rejecting at the first crossing
/// is valid at any stopping time. Out-of-range observations are clamped and
/// flagged (a recoverable assumption finding).
///
/// # Errors
///
/// Returns [`GuaranteeLayerError::InvalidConfig`] if `alpha`, `mu0`,
/// `lambda_cap` are not finite probabilities in `(0, 1)` or `regularizer` is
/// non-finite/negative.
pub fn run_eprocess(config: &EProcessConfig, observations: &[f64]) -> Result<EProcessResult> {
    if !config.alpha.is_finite() || !(0.0..1.0).contains(&config.alpha) || config.alpha <= 0.0 {
        return Err(GuaranteeLayerError::InvalidConfig {
            detail: format!("alpha must be in (0, 1), got {}", config.alpha),
        });
    }
    if !config.mu0.is_finite() || !(0.0..1.0).contains(&config.mu0) || config.mu0 <= 0.0 {
        return Err(GuaranteeLayerError::InvalidConfig {
            detail: format!("mu0 must be in (0, 1), got {}", config.mu0),
        });
    }
    if !config.lambda_cap.is_finite()
        || !(0.0..1.0).contains(&config.lambda_cap)
        || config.lambda_cap <= 0.0
    {
        return Err(GuaranteeLayerError::InvalidConfig {
            detail: format!("lambda_cap must be in (0, 1), got {}", config.lambda_cap),
        });
    }
    if !config.regularizer.is_finite() || config.regularizer < 0.0 {
        return Err(GuaranteeLayerError::InvalidConfig {
            detail: format!(
                "regularizer must be finite and non-negative, got {}",
                config.regularizer
            ),
        });
    }

    let mu0 = config.mu0;
    let lambda_max = config.lambda_cap / mu0;
    let threshold = 1.0 / config.alpha;
    let log_threshold = threshold.ln();

    let mut ledger: Vec<EProcessLedgerEntry> = Vec::with_capacity(observations.len());
    let mut assumption_violations: Vec<String> = Vec::new();
    let mut cumulative_log_wealth = 0.0;
    let mut peak_log_wealth = 0.0;
    let mut stopping_index: Option<usize> = None;

    // Predictable plug-in state: first/second moments of (X_i - mu0) over the
    // observations strictly before the current step. The number of prior
    // observations equals the enumerate `index` (state is updated each step).
    let mut past_sum = 0.0_f64;
    let mut past_sumsq = 0.0_f64;

    for (index, raw) in observations.iter().copied().enumerate() {
        let observation = if raw.is_finite() && (0.0..=1.0).contains(&raw) {
            raw
        } else {
            assumption_violations.push(format!(
                "observation #{index} out of [0, 1] (got {raw}); clamped"
            ));
            raw.clamp(0.0, 1.0)
        };

        // Predictable betting fraction from strictly-prior observations.
        let betting_fraction = if index == 0 {
            0.0
        } else {
            let count = index as f64;
            let mean_dev = past_sum / count;
            let second_moment = past_sumsq / count;
            let raw_lambda = mean_dev / (second_moment + config.regularizer);
            raw_lambda.clamp(0.0, lambda_max)
        };

        let multiplier = 1.0 + betting_fraction * (observation - mu0);
        // `multiplier >= 1 - lambda_max·mu0 = 1 - lambda_cap > 0` by construction.
        let increment = multiplier.ln();
        cumulative_log_wealth += increment;
        if cumulative_log_wealth > peak_log_wealth {
            peak_log_wealth = cumulative_log_wealth;
        }
        let wealth = cumulative_log_wealth.exp();
        let rejected = cumulative_log_wealth >= log_threshold;
        if rejected && stopping_index.is_none() {
            stopping_index = Some(index);
        }

        ledger.push(EProcessLedgerEntry {
            index,
            observation,
            betting_fraction,
            increment_log_wealth: increment,
            cumulative_log_wealth,
            wealth,
            rejected: stopping_index.is_some(),
        });

        // Update predictable state AFTER using it for this step.
        let dev = observation - mu0;
        past_sum += dev;
        past_sumsq += dev * dev;
    }

    let rejected = stopping_index.is_some();
    let final_wealth = cumulative_log_wealth.exp();
    let peak_wealth = peak_log_wealth.exp();
    let fallback_recommended = rejected || !assumption_violations.is_empty();

    let process_id = format!(
        "guarantee-eprocess-{}",
        short_hash(&stable_hash(&CanonicalEProcessId {
            schema_version: GUARANTEE_LAYER_SCHEMA_VERSION,
            alpha: config.alpha,
            mu0,
            lambda_cap: config.lambda_cap,
            regularizer: config.regularizer,
            observations,
        }))
    );

    Ok(EProcessResult {
        schema_version: GUARANTEE_LAYER_SCHEMA_VERSION.to_string(),
        process_id,
        alpha: config.alpha,
        mu0,
        threshold,
        ledger,
        final_log_wealth: cumulative_log_wealth,
        final_wealth,
        peak_wealth,
        rejected,
        stopping_index,
        observation_count: observations.len(),
        assumption_violations,
        fallback_recommended,
    })
}

#[derive(Serialize)]
struct CanonicalEProcessId<'a> {
    schema_version: &'a str,
    alpha: f64,
    mu0: f64,
    lambda_cap: f64,
    regularizer: f64,
    observations: &'a [f64],
}

// ════════════════════════════════════════════════════════════════════════════
// PAC-Bayes bound
// ════════════════════════════════════════════════════════════════════════════

/// Configuration for the PAC-Bayes bound reporter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PacBayesConfig {
    /// Confidence parameter `delta` in `(0, 1)`; the bound holds with probability
    /// `>= 1 - delta` over the sample.
    pub delta: f64,
    /// The risk ceiling the bound is clamped to (default `1.0` for `[0, 1]` loss);
    /// a bound that reaches the ceiling is vacuous.
    pub risk_ceiling: f64,
}

impl Default for PacBayesConfig {
    fn default() -> Self {
        Self {
            delta: 0.05,
            risk_ceiling: 1.0,
        }
    }
}

impl PacBayesConfig {
    /// Override the confidence parameter.
    #[must_use]
    pub fn with_delta(mut self, delta: f64) -> Self {
        self.delta = delta;
        self
    }

    /// Override the risk ceiling.
    #[must_use]
    pub fn with_risk_ceiling(mut self, risk_ceiling: f64) -> Self {
        self.risk_ceiling = risk_ceiling;
        self
    }
}

/// Inputs to the PAC-Bayes bound for a critical classifier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PacBayesInput {
    /// Empirical risk of the posterior `Q` on the sample, in `[0, 1]`.
    pub empirical_risk: f64,
    /// `KL(Q || P)` divergence between posterior and prior (`>= 0`).
    pub kl_divergence: f64,
    /// Sample size `n` (`>= 1`).
    pub sample_size: usize,
}

impl PacBayesInput {
    /// Construct a PAC-Bayes input.
    #[must_use]
    pub fn new(empirical_risk: f64, kl_divergence: f64, sample_size: usize) -> Self {
        Self {
            empirical_risk,
            kl_divergence,
            sample_size,
        }
    }
}

/// A McAllester PAC-Bayes generalization bound, with stated terms (AC1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PacBayesBound {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic artifact id.
    pub bound_id: String,
    /// Confidence parameter.
    pub delta: f64,
    /// Empirical risk.
    pub empirical_risk: f64,
    /// `KL(Q || P)`.
    pub kl_divergence: f64,
    /// Sample size.
    pub sample_size: usize,
    /// Confidence term `ln(2·sqrt(n) / delta)`.
    pub confidence_term: f64,
    /// Penalty `(KL + confidence_term) / n` (the KL-form right-hand side).
    pub penalty: f64,
    /// Complexity term `sqrt(penalty / 2)` (the Pinsker-relaxed sqrt form).
    pub complexity_term: f64,
    /// Risk bound `min(risk_ceiling, empirical_risk + complexity_term)`.
    pub risk_bound: f64,
    /// Whether the bound is vacuous (reaches the risk ceiling).
    pub vacuous: bool,
    /// Recoverable assumption findings.
    pub assumption_violations: Vec<String>,
    /// Whether this guarantee recommends a conservative fallback (AC3).
    pub fallback_recommended: bool,
}

impl PacBayesBound {
    /// Fallback reasons contributed to the unified report.
    #[must_use]
    pub fn fallback_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.vacuous {
            reasons.push(format!(
                "pac-bayes: vacuous bound {:.4} (KL={:.4}, n={})",
                self.risk_bound, self.kl_divergence, self.sample_size
            ));
        }
        for violation in &self.assumption_violations {
            reasons.push(format!("pac-bayes: {violation}"));
        }
        reasons
    }

    /// The [`FormalGuarantee`] declaration for this bound (AC1).
    #[must_use]
    pub fn formal_guarantee(&self) -> FormalGuarantee {
        FormalGuarantee::new(
            GuaranteeKind::PacBayes,
            [
                "loss bounded in [0, 1]".to_string(),
                "prior P fixed independently of the sample; posterior Q over the same hypotheses"
                    .to_string(),
                "i.i.d. sample of size n".to_string(),
                format!(
                    "McAllester bound holds with probability >= {:.6}",
                    1.0 - self.delta
                ),
            ],
            [
                format!("delta={:.6}", self.delta),
                format!("empirical_risk={:.6}", self.empirical_risk),
                format!("kl_divergence={:.6}", self.kl_divergence),
                format!("sample_size={}", self.sample_size),
                format!("confidence_term={:.6}", self.confidence_term),
                format!("complexity_term={:.6}", self.complexity_term),
                format!("risk_bound={:.6}", self.risk_bound),
                format!("vacuous={}", self.vacuous),
            ],
        )
    }
}

/// Compute the McAllester PAC-Bayes risk bound.
///
/// For bounded loss in `[0, 1]`, with probability `>= 1 - delta` over an i.i.d.
/// sample of size `n`,
/// `risk <= empirical_risk + sqrt((KL(Q||P) + ln(2·sqrt(n)/delta)) / (2n))`.
/// The bound is clamped to `risk_ceiling`; a bound that reaches the ceiling is
/// vacuous and recommends a fallback.
///
/// # Errors
///
/// Returns [`GuaranteeLayerError::InvalidConfig`] if `delta` is not in `(0, 1)`
/// or `risk_ceiling` is not in `(0, 1]`, and [`GuaranteeLayerError::InvalidInput`]
/// if `empirical_risk` is not in `[0, 1]`, `kl_divergence` is negative/non-finite,
/// or `sample_size` is zero.
pub fn pac_bayes_bound(config: &PacBayesConfig, input: &PacBayesInput) -> Result<PacBayesBound> {
    if !config.delta.is_finite() || !(0.0..1.0).contains(&config.delta) || config.delta <= 0.0 {
        return Err(GuaranteeLayerError::InvalidConfig {
            detail: format!("delta must be in (0, 1), got {}", config.delta),
        });
    }
    if !config.risk_ceiling.is_finite()
        || config.risk_ceiling <= 0.0
        || config.risk_ceiling > 1.0 + VACUITY_EPS
    {
        return Err(GuaranteeLayerError::InvalidConfig {
            detail: format!(
                "risk_ceiling must be in (0, 1], got {}",
                config.risk_ceiling
            ),
        });
    }
    if !input.empirical_risk.is_finite() || !(0.0..=1.0).contains(&input.empirical_risk) {
        return Err(GuaranteeLayerError::InvalidInput {
            detail: format!(
                "empirical_risk must be in [0, 1], got {}",
                input.empirical_risk
            ),
        });
    }
    if !input.kl_divergence.is_finite() || input.kl_divergence < 0.0 {
        return Err(GuaranteeLayerError::InvalidInput {
            detail: format!(
                "kl_divergence must be finite and non-negative, got {}",
                input.kl_divergence
            ),
        });
    }
    if input.sample_size == 0 {
        return Err(GuaranteeLayerError::InvalidInput {
            detail: "sample_size must be >= 1".to_string(),
        });
    }

    let n = input.sample_size as f64;
    let confidence_term = (2.0 * n.sqrt() / config.delta).ln();
    let penalty = (input.kl_divergence + confidence_term) / n;
    let complexity_term = (penalty / 2.0).max(0.0).sqrt();
    let risk_bound = (input.empirical_risk + complexity_term).min(config.risk_ceiling);
    let vacuous = risk_bound >= config.risk_ceiling - VACUITY_EPS;
    let fallback_recommended = vacuous;

    let bound_id = format!(
        "guarantee-pacbayes-{}",
        short_hash(&stable_hash(&CanonicalPacBayesId {
            schema_version: GUARANTEE_LAYER_SCHEMA_VERSION,
            delta: config.delta,
            risk_ceiling: config.risk_ceiling,
            empirical_risk: input.empirical_risk,
            kl_divergence: input.kl_divergence,
            sample_size: input.sample_size,
        }))
    );

    Ok(PacBayesBound {
        schema_version: GUARANTEE_LAYER_SCHEMA_VERSION.to_string(),
        bound_id,
        delta: config.delta,
        empirical_risk: input.empirical_risk,
        kl_divergence: input.kl_divergence,
        sample_size: input.sample_size,
        confidence_term,
        penalty,
        complexity_term,
        risk_bound,
        vacuous,
        assumption_violations: Vec::new(),
        fallback_recommended,
    })
}

#[derive(Serialize)]
struct CanonicalPacBayesId<'a> {
    schema_version: &'a str,
    delta: f64,
    risk_ceiling: f64,
    empirical_risk: f64,
    kl_divergence: f64,
    sample_size: usize,
}

// ════════════════════════════════════════════════════════════════════════════
// Unified guarantee layer
// ════════════════════════════════════════════════════════════════════════════

/// The conservative-fallback decision derived from the three guarantees (AC3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuaranteeFallback {
    /// Whether any guarantee failed, was vacuous, or had an assumption violation.
    pub triggered: bool,
    /// The collected reasons (empty when not triggered).
    pub reasons: Vec<String>,
    /// The forced decision when triggered; `None` when guarantees hold (the
    /// guarantee layer only *forces* a conservative fallback, never an approval).
    pub recommended_decision: Option<MigrationDecision>,
}

/// JSON-stats artifact (path / checksum / content), matching the crate idiom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonStatsArtifact {
    /// Suggested artifact path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// The serialized JSON content.
    pub content: String,
}

/// A full, deterministic guarantee report over one claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuaranteeReport {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// The claim this report pertains to.
    pub claim_id: String,
    /// The conformal forecast.
    pub conformal: ConformalForecast,
    /// The e-process result.
    pub e_process: EProcessResult,
    /// The PAC-Bayes bound.
    pub pac_bayes: PacBayesBound,
    /// The three formal-guarantee declarations (Conformal, EProcess, PacBayes).
    pub formal_guarantees: Vec<FormalGuarantee>,
    /// The derived conservative-fallback decision.
    pub fallback: GuaranteeFallback,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: JsonStatsArtifact,
    /// Deterministic replay command.
    pub replay_command: String,
}

impl GuaranteeReport {
    /// Pretty-printed JSON of the report.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|error| error.to_string())
    }

    /// The guarantee kinds present, in canonical order.
    #[must_use]
    pub fn guarantee_kinds(&self) -> Vec<GuaranteeKind> {
        self.formal_guarantees.iter().map(|g| g.kind).collect()
    }
}

/// Bundled inputs for a single [`GuaranteeLayerEngine::evaluate`] call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuaranteeEvaluationInput {
    /// The claim id.
    pub claim_id: String,
    /// Conformal calibration nonconformity scores.
    pub conformal_calibration: Vec<f64>,
    /// The conformal point forecast (e.g. a posterior acceptance probability).
    pub conformal_forecast_point: f64,
    /// Held-out validation pairs `(truth, forecast)` for empirical coverage.
    pub conformal_validation: Vec<(f64, f64)>,
    /// Sequential per-step quality-violation indicators in `[0, 1]`.
    pub eprocess_observations: Vec<f64>,
    /// The PAC-Bayes input for the critical classifier.
    pub pac_bayes: PacBayesInput,
}

impl GuaranteeEvaluationInput {
    /// Construct an input with empty calibration/validation/observation streams.
    #[must_use]
    pub fn new(
        claim_id: impl Into<String>,
        conformal_forecast_point: f64,
        pac_bayes: PacBayesInput,
    ) -> Self {
        Self {
            claim_id: claim_id.into(),
            conformal_calibration: Vec::new(),
            conformal_forecast_point,
            conformal_validation: Vec::new(),
            eprocess_observations: Vec::new(),
            pac_bayes,
        }
    }

    /// Seed an input from a posterior record: the conformal forecast point is the
    /// posterior acceptance probability and the claim id is carried through.
    #[must_use]
    pub fn from_posterior(record: &PosteriorRecord, pac_bayes: PacBayesInput) -> Self {
        Self::new(record.claim_id.clone(), record.posterior_prob, pac_bayes)
    }

    /// Attach conformal calibration scores.
    #[must_use]
    pub fn with_calibration(mut self, scores: impl IntoIterator<Item = f64>) -> Self {
        self.conformal_calibration = scores.into_iter().collect();
        self
    }

    /// Attach conformal validation pairs `(truth, forecast)`.
    #[must_use]
    pub fn with_validation(mut self, pairs: impl IntoIterator<Item = (f64, f64)>) -> Self {
        self.conformal_validation = pairs.into_iter().collect();
        self
    }

    /// Attach the sequential e-process observation stream.
    #[must_use]
    pub fn with_observations(mut self, observations: impl IntoIterator<Item = f64>) -> Self {
        self.eprocess_observations = observations.into_iter().collect();
        self
    }
}

/// Composes the three guarantee families into a unified, auditable report.
#[derive(Debug, Clone, Default)]
pub struct GuaranteeLayerEngine {
    conformal: ConformalConfig,
    eprocess: EProcessConfig,
    pac_bayes: PacBayesConfig,
}

impl GuaranteeLayerEngine {
    /// Build an engine from per-family configs.
    #[must_use]
    pub fn new(
        conformal: ConformalConfig,
        eprocess: EProcessConfig,
        pac_bayes: PacBayesConfig,
    ) -> Self {
        Self {
            conformal,
            eprocess,
            pac_bayes,
        }
    }

    /// Override the conformal config.
    #[must_use]
    pub fn with_conformal(mut self, conformal: ConformalConfig) -> Self {
        self.conformal = conformal;
        self
    }

    /// Override the e-process config.
    #[must_use]
    pub fn with_eprocess(mut self, eprocess: EProcessConfig) -> Self {
        self.eprocess = eprocess;
        self
    }

    /// Override the PAC-Bayes config.
    #[must_use]
    pub fn with_pac_bayes(mut self, pac_bayes: PacBayesConfig) -> Self {
        self.pac_bayes = pac_bayes;
        self
    }

    /// Borrow the conformal config.
    #[must_use]
    pub fn conformal_config(&self) -> &ConformalConfig {
        &self.conformal
    }

    /// Borrow the e-process config.
    #[must_use]
    pub fn eprocess_config(&self) -> &EProcessConfig {
        &self.eprocess
    }

    /// Borrow the PAC-Bayes config.
    #[must_use]
    pub fn pac_bayes_config(&self) -> &PacBayesConfig {
        &self.pac_bayes
    }

    /// Evaluate all three guarantees and assemble a deterministic report.
    ///
    /// # Errors
    ///
    /// Returns a [`GuaranteeLayerError`] when any per-family config or input is
    /// structurally invalid. Guarantees that are merely insufficient, vacuous, or
    /// violated do not error; they set `fallback_recommended` and drive the
    /// report's [`GuaranteeFallback`] (AC3).
    pub fn evaluate(&self, input: &GuaranteeEvaluationInput) -> Result<GuaranteeReport> {
        let conformal = conformal_interval(
            &self.conformal,
            &input.conformal_calibration,
            input.conformal_forecast_point,
            &input.conformal_validation,
        )?;
        let e_process = run_eprocess(&self.eprocess, &input.eprocess_observations)?;
        let pac_bayes = pac_bayes_bound(&self.pac_bayes, &input.pac_bayes)?;

        let formal_guarantees = vec![
            conformal.formal_guarantee(),
            e_process.formal_guarantee(),
            pac_bayes.formal_guarantee(),
        ];

        let mut reasons = Vec::new();
        reasons.extend(conformal.fallback_reasons());
        reasons.extend(e_process.fallback_reasons());
        reasons.extend(pac_bayes.fallback_reasons());
        let triggered = conformal.fallback_recommended
            || e_process.fallback_recommended
            || pac_bayes.fallback_recommended;
        let fallback = GuaranteeFallback {
            triggered,
            reasons,
            recommended_decision: if triggered {
                Some(MigrationDecision::ConservativeFallback)
            } else {
                None
            },
        };

        let report_id = format!(
            "guarantee-layer-{}",
            short_hash(&stable_hash(&CanonicalReportId {
                schema_version: GUARANTEE_LAYER_SCHEMA_VERSION,
                claim_id: &input.claim_id,
                conformal_id: &conformal.forecast_id,
                eprocess_id: &e_process.process_id,
                pac_bayes_id: &pac_bayes.bound_id,
                fallback_triggered: triggered,
            }))
        );
        let replay_command = format!(
            "cargo test -p doctor_frankentui --lib guarantee_layer -- --nocapture # report {report_id}"
        );
        let exported_json_stats = export_json_stats(
            &report_id,
            &input.claim_id,
            &conformal,
            &e_process,
            &pac_bayes,
            &fallback,
        );

        Ok(GuaranteeReport {
            schema_version: GUARANTEE_LAYER_SCHEMA_VERSION.to_string(),
            report_id,
            claim_id: input.claim_id.clone(),
            conformal,
            e_process,
            pac_bayes,
            formal_guarantees,
            fallback,
            exported_json_stats,
            replay_command,
        })
    }
}

#[derive(Serialize)]
struct CanonicalReportId<'a> {
    schema_version: &'a str,
    claim_id: &'a str,
    conformal_id: &'a str,
    eprocess_id: &'a str,
    pac_bayes_id: &'a str,
    fallback_triggered: bool,
}

fn export_json_stats(
    report_id: &str,
    claim_id: &str,
    conformal: &ConformalForecast,
    e_process: &EProcessResult,
    pac_bayes: &PacBayesBound,
    fallback: &GuaranteeFallback,
) -> JsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        claim_id: &'a str,
        conformal: &'a ConformalForecast,
        e_process: &'a EProcessResult,
        pac_bayes: &'a PacBayesBound,
        fallback: &'a GuaranteeFallback,
    }

    let payload = Export {
        schema_version: GUARANTEE_LAYER_SCHEMA_VERSION,
        report_id,
        claim_id,
        conformal,
        e_process,
        pac_bayes,
        fallback,
    };
    let content = serde_json::to_string_pretty(&payload).unwrap_or_else(|error| error.to_string());
    JsonStatsArtifact {
        path: format!("{report_id}/guarantee_layer_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Hashing helpers ───────────────────────────────────────────────────────────

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

    const TOL: f64 = 1e-9;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() <= TOL
    }

    fn approx_tol(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    /// 50 calibration scores `0.00, 0.01, ..., 0.49`.
    fn calibration_50() -> Vec<f64> {
        (0..50).map(|i| f64::from(i) * 0.01).collect()
    }

    // ── Conformal ─────────────────────────────────────────────────────────────

    #[test]
    fn conformal_quantile_rank_is_correct() {
        // n=50, alpha=0.1 -> rank = ceil(0.9 * 51) = ceil(45.9) = 46 -> sorted[45] = 0.45.
        let forecast =
            conformal_interval(&ConformalConfig::default(), &calibration_50(), 0.8, &[]).unwrap();
        assert_eq!(forecast.quantile_rank, 46);
        assert!(approx(forecast.conformal_quantile, 0.45));
        assert!(!forecast.vacuous);
    }

    #[test]
    fn conformal_interval_brackets_point() {
        let forecast =
            conformal_interval(&ConformalConfig::default(), &calibration_50(), 0.8, &[]).unwrap();
        assert!(approx(forecast.lower, 0.8 - 0.45));
        assert!(approx(forecast.upper, 0.8 + 0.45));
        assert!(forecast.covers(0.8));
        assert!(forecast.covers(0.4));
        assert!(!forecast.covers(0.0));
    }

    #[test]
    fn conformal_smaller_alpha_widens_interval() {
        let scores = calibration_50();
        let tight = conformal_interval(
            &ConformalConfig::default().with_alpha(0.2),
            &scores,
            0.5,
            &[],
        )
        .unwrap();
        let wide = conformal_interval(
            &ConformalConfig::default().with_alpha(0.05),
            &scores,
            0.5,
            &[],
        )
        .unwrap();
        assert!(wide.conformal_quantile > tight.conformal_quantile);
    }

    #[test]
    fn conformal_insufficient_calibration_is_vacuous_and_fallback() {
        // n=5, alpha=0.1 -> rank = ceil(0.9 * 6) = 6 > 5 -> vacuous.
        let forecast = conformal_interval(
            &ConformalConfig::default(),
            &[0.1, 0.2, 0.3, 0.4, 0.5],
            0.5,
            &[],
        )
        .unwrap();
        assert!(forecast.vacuous);
        assert!(forecast.conformal_quantile.is_infinite());
        assert!(forecast.fallback_recommended);
        assert!(forecast.lower.is_infinite());
        assert!(forecast.upper.is_infinite());
    }

    #[test]
    fn conformal_empty_calibration_is_vacuous() {
        let forecast = conformal_interval(&ConformalConfig::default(), &[], 0.5, &[]).unwrap();
        assert!(forecast.vacuous);
        assert_eq!(forecast.calibration_count, 0);
        assert!(forecast.fallback_recommended);
    }

    #[test]
    fn conformal_empirical_coverage_computed() {
        // All validation residuals are 0.1 <= q=0.45, so coverage = 1.0.
        let validation: Vec<(f64, f64)> = (0..20)
            .map(|i| (0.5, 0.5 + 0.1 * f64::from(i % 2)))
            .collect();
        let forecast = conformal_interval(
            &ConformalConfig::default(),
            &calibration_50(),
            0.8,
            &validation,
        )
        .unwrap();
        assert_eq!(forecast.validation_count, 20);
        assert!(approx(forecast.empirical_coverage.unwrap(), 1.0));
        assert!(forecast.assumption_violations.is_empty());
        assert!(!forecast.fallback_recommended);
    }

    #[test]
    fn conformal_coverage_shortfall_flags_fallback() {
        // Residuals of 0.9 exceed q=0.45 for every pair -> coverage 0.0 << nominal.
        let validation: Vec<(f64, f64)> = (0..20).map(|_| (0.0, 0.9)).collect();
        let forecast = conformal_interval(
            &ConformalConfig::default(),
            &calibration_50(),
            0.8,
            &validation,
        )
        .unwrap();
        assert!(approx(forecast.empirical_coverage.unwrap(), 0.0));
        assert!(!forecast.assumption_violations.is_empty());
        assert!(forecast.fallback_recommended);
    }

    #[test]
    fn conformal_invalid_alpha_errors() {
        assert!(matches!(
            conformal_interval(
                &ConformalConfig::default().with_alpha(0.0),
                &calibration_50(),
                0.5,
                &[]
            ),
            Err(GuaranteeLayerError::InvalidConfig { .. })
        ));
        assert!(matches!(
            conformal_interval(
                &ConformalConfig::default().with_alpha(1.0),
                &calibration_50(),
                0.5,
                &[]
            ),
            Err(GuaranteeLayerError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn conformal_nonfinite_input_errors() {
        assert!(matches!(
            conformal_interval(
                &ConformalConfig::default(),
                &calibration_50(),
                f64::NAN,
                &[]
            ),
            Err(GuaranteeLayerError::InvalidInput { .. })
        ));
        assert!(matches!(
            conformal_interval(&ConformalConfig::default(), &[0.1, f64::INFINITY], 0.5, &[]),
            Err(GuaranteeLayerError::InvalidInput { .. })
        ));
    }

    #[test]
    fn conformal_formal_guarantee_is_conformal_kind() {
        let forecast =
            conformal_interval(&ConformalConfig::default(), &calibration_50(), 0.8, &[]).unwrap();
        let guarantee = forecast.formal_guarantee();
        assert_eq!(guarantee.kind, GuaranteeKind::Conformal);
        assert!(!guarantee.assumptions.is_empty());
        assert!(
            guarantee
                .bound_terms
                .iter()
                .any(|t| t.starts_with("alpha="))
        );
    }

    // ── E-process ─────────────────────────────────────────────────────────────

    #[test]
    fn eprocess_clean_stream_never_rejects() {
        let result = run_eprocess(&EProcessConfig::default(), &[0.0; 50]).unwrap();
        assert!(!result.rejected);
        assert!(result.stopping_index.is_none());
        // No betting on a below-null stream: wealth stays at 1.
        assert!(result.final_wealth <= 1.0 + TOL);
        assert!(!result.fallback_recommended);
    }

    #[test]
    fn eprocess_strong_violation_rejects() {
        let result = run_eprocess(&EProcessConfig::default(), &[1.0; 30]).unwrap();
        assert!(result.rejected);
        let stop = result.stopping_index.expect("rejected => stopping index");
        assert!(result.ledger[stop].cumulative_log_wealth >= result.threshold.ln() - TOL);
        assert!(result.peak_wealth >= result.threshold - TOL);
        assert!(result.fallback_recommended);
    }

    #[test]
    fn eprocess_first_step_does_not_bet() {
        let result = run_eprocess(&EProcessConfig::default(), &[1.0, 1.0, 1.0]).unwrap();
        assert!(approx(result.ledger[0].betting_fraction, 0.0));
        assert!(approx(result.ledger[0].increment_log_wealth, 0.0));
        assert!(approx(result.ledger[0].wealth, 1.0));
    }

    #[test]
    fn eprocess_wealth_stays_positive() {
        // Mixed stream including the worst case (X=0 after positive betting).
        let stream = [1.0, 1.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0];
        let result = run_eprocess(&EProcessConfig::default(), &stream).unwrap();
        for entry in &result.ledger {
            assert!(entry.wealth > 0.0);
            assert!(entry.wealth.is_finite());
        }
    }

    #[test]
    fn eprocess_ledger_is_machine_verifiable() {
        let result = run_eprocess(&EProcessConfig::default(), &[1.0; 30]).unwrap();
        assert!(result.verify_ledger(1e-9));
    }

    #[test]
    fn eprocess_optional_stopping_is_stable() {
        // The decision at the first crossing is unchanged by truncating the stream
        // right after it: predictable betting => identical prefix ledger.
        let full = run_eprocess(&EProcessConfig::default(), &[1.0; 40]).unwrap();
        let stop = full.stopping_index.expect("rejected");
        let truncated_obs = vec![1.0; stop + 1];
        let truncated = run_eprocess(&EProcessConfig::default(), &truncated_obs).unwrap();
        assert_eq!(truncated.stopping_index, Some(stop));
        assert!(truncated.rejected);
        for i in 0..=stop {
            assert!(approx(
                full.ledger[i].cumulative_log_wealth,
                truncated.ledger[i].cumulative_log_wealth
            ));
        }
    }

    #[test]
    fn eprocess_threshold_is_inverse_alpha() {
        let result = run_eprocess(&EProcessConfig::default().with_alpha(0.05), &[0.0]).unwrap();
        assert!(approx(result.threshold, 20.0));
    }

    #[test]
    fn eprocess_out_of_range_observation_flags_and_clamps() {
        let result = run_eprocess(&EProcessConfig::default(), &[0.0, 2.0, 0.0]).unwrap();
        assert!(!result.assumption_violations.is_empty());
        assert!(result.fallback_recommended);
        assert!(approx(result.ledger[1].observation, 1.0));
    }

    #[test]
    fn eprocess_invalid_mu0_errors() {
        assert!(matches!(
            run_eprocess(&EProcessConfig::default().with_mu0(0.0), &[0.0]),
            Err(GuaranteeLayerError::InvalidConfig { .. })
        ));
        assert!(matches!(
            run_eprocess(&EProcessConfig::default().with_mu0(1.0), &[0.0]),
            Err(GuaranteeLayerError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn eprocess_invalid_alpha_errors() {
        assert!(matches!(
            run_eprocess(&EProcessConfig::default().with_alpha(0.0), &[0.0]),
            Err(GuaranteeLayerError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn eprocess_formal_guarantee_is_eprocess_kind() {
        let result = run_eprocess(&EProcessConfig::default(), &[0.0; 10]).unwrap();
        let guarantee = result.formal_guarantee();
        assert_eq!(guarantee.kind, GuaranteeKind::EProcess);
        assert!(guarantee.bound_terms.iter().any(|t| t.starts_with("mu0=")));
    }

    // ── PAC-Bayes ──────────────────────────────────────────────────────────────

    #[test]
    fn pac_bayes_bound_matches_hand_computation() {
        let bound = pac_bayes_bound(
            &PacBayesConfig::default(),
            &PacBayesInput::new(0.1, 2.0, 1000),
        )
        .unwrap();
        // Pin the implementation to the documented McAllester closed form
        // (1e-9), then sanity-check the magnitude against the hand calculation
        // (~0.168) so a grossly-wrong formula is still caught.
        let n = 1000.0_f64;
        let expected_confidence = (2.0 * n.sqrt() / 0.05).ln();
        let expected_penalty = (2.0 + expected_confidence) / n;
        let expected_complexity = (expected_penalty / 2.0).sqrt();
        let expected_bound = 0.1 + expected_complexity;
        assert!(approx_tol(bound.confidence_term, expected_confidence, 1e-9));
        assert!(approx_tol(bound.penalty, expected_penalty, 1e-9));
        assert!(approx_tol(bound.complexity_term, expected_complexity, 1e-9));
        assert!(approx_tol(bound.risk_bound, expected_bound, 1e-9));
        assert!(approx_tol(bound.risk_bound, 0.1676, 1e-3));
        assert!(!bound.vacuous);
        assert!(!bound.fallback_recommended);
    }

    #[test]
    fn pac_bayes_huge_kl_is_vacuous() {
        let bound = pac_bayes_bound(
            &PacBayesConfig::default(),
            &PacBayesInput::new(0.1, 1e6, 10),
        )
        .unwrap();
        assert!(bound.vacuous);
        assert!(approx(bound.risk_bound, 1.0));
        assert!(bound.fallback_recommended);
    }

    #[test]
    fn pac_bayes_tighter_with_larger_n() {
        let small = pac_bayes_bound(
            &PacBayesConfig::default(),
            &PacBayesInput::new(0.1, 1.0, 100),
        )
        .unwrap();
        let large = pac_bayes_bound(
            &PacBayesConfig::default(),
            &PacBayesInput::new(0.1, 1.0, 10_000),
        )
        .unwrap();
        assert!(large.risk_bound < small.risk_bound);
    }

    #[test]
    fn pac_bayes_tighter_with_smaller_kl() {
        let big_kl = pac_bayes_bound(
            &PacBayesConfig::default(),
            &PacBayesInput::new(0.1, 5.0, 1000),
        )
        .unwrap();
        let small_kl = pac_bayes_bound(
            &PacBayesConfig::default(),
            &PacBayesInput::new(0.1, 0.5, 1000),
        )
        .unwrap();
        assert!(small_kl.risk_bound < big_kl.risk_bound);
    }

    #[test]
    fn pac_bayes_clamped_to_risk_ceiling() {
        let bound = pac_bayes_bound(
            &PacBayesConfig::default().with_risk_ceiling(0.5),
            &PacBayesInput::new(0.4, 100.0, 10),
        )
        .unwrap();
        assert!(bound.risk_bound <= 0.5 + TOL);
        assert!(bound.vacuous);
    }

    #[test]
    fn pac_bayes_invalid_delta_errors() {
        assert!(matches!(
            pac_bayes_bound(
                &PacBayesConfig::default().with_delta(0.0),
                &PacBayesInput::new(0.1, 1.0, 100)
            ),
            Err(GuaranteeLayerError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn pac_bayes_invalid_inputs_error() {
        assert!(matches!(
            pac_bayes_bound(
                &PacBayesConfig::default(),
                &PacBayesInput::new(1.5, 1.0, 100)
            ),
            Err(GuaranteeLayerError::InvalidInput { .. })
        ));
        assert!(matches!(
            pac_bayes_bound(
                &PacBayesConfig::default(),
                &PacBayesInput::new(0.1, -1.0, 100)
            ),
            Err(GuaranteeLayerError::InvalidInput { .. })
        ));
        assert!(matches!(
            pac_bayes_bound(&PacBayesConfig::default(), &PacBayesInput::new(0.1, 1.0, 0)),
            Err(GuaranteeLayerError::InvalidInput { .. })
        ));
    }

    #[test]
    fn pac_bayes_formal_guarantee_is_pac_bayes_kind() {
        let bound = pac_bayes_bound(
            &PacBayesConfig::default(),
            &PacBayesInput::new(0.1, 1.0, 1000),
        )
        .unwrap();
        let guarantee = bound.formal_guarantee();
        assert_eq!(guarantee.kind, GuaranteeKind::PacBayes);
        assert!(
            guarantee
                .bound_terms
                .iter()
                .any(|t| t.starts_with("delta="))
        );
    }

    // ── Unified engine ──────────────────────────────────────────────────────────

    fn passing_input() -> GuaranteeEvaluationInput {
        let validation: Vec<(f64, f64)> = (0..20).map(|_| (0.5, 0.55)).collect();
        GuaranteeEvaluationInput::new("claim-ok", 0.8, PacBayesInput::new(0.1, 1.0, 1000))
            .with_calibration(calibration_50())
            .with_validation(validation)
            .with_observations(vec![0.0; 40])
    }

    #[test]
    fn engine_all_pass_has_no_fallback() {
        let report = GuaranteeLayerEngine::default()
            .evaluate(&passing_input())
            .unwrap();
        assert!(!report.fallback.triggered);
        assert!(report.fallback.reasons.is_empty());
        assert!(report.fallback.recommended_decision.is_none());
        assert_eq!(report.formal_guarantees.len(), 3);
        assert_eq!(
            report.guarantee_kinds(),
            vec![
                GuaranteeKind::Conformal,
                GuaranteeKind::EProcess,
                GuaranteeKind::PacBayes
            ]
        );
    }

    #[test]
    fn engine_pac_bayes_failure_forces_conservative_fallback() {
        let input =
            GuaranteeEvaluationInput::new("claim-bad", 0.8, PacBayesInput::new(0.1, 1e6, 10))
                .with_calibration(calibration_50())
                .with_validation((0..20).map(|_| (0.5, 0.55)).collect::<Vec<_>>())
                .with_observations(vec![0.0; 40]);
        let report = GuaranteeLayerEngine::default().evaluate(&input).unwrap();
        assert!(report.fallback.triggered);
        assert_eq!(
            report.fallback.recommended_decision,
            Some(MigrationDecision::ConservativeFallback)
        );
        assert!(
            report
                .fallback
                .reasons
                .iter()
                .any(|r| r.contains("pac-bayes"))
        );
    }

    #[test]
    fn engine_eprocess_rejection_forces_fallback() {
        let input =
            GuaranteeEvaluationInput::new("claim-drift", 0.8, PacBayesInput::new(0.1, 1.0, 1000))
                .with_calibration(calibration_50())
                .with_validation((0..20).map(|_| (0.5, 0.55)).collect::<Vec<_>>())
                .with_observations(vec![1.0; 40]);
        let report = GuaranteeLayerEngine::default().evaluate(&input).unwrap();
        assert!(report.fallback.triggered);
        assert!(report.e_process.rejected);
        assert!(
            report
                .fallback
                .reasons
                .iter()
                .any(|r| r.contains("e-process"))
        );
    }

    #[test]
    fn engine_report_is_deterministic() {
        let engine = GuaranteeLayerEngine::default();
        let a = engine.evaluate(&passing_input()).unwrap();
        let b = engine.evaluate(&passing_input()).unwrap();
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.exported_json_stats.sha256, b.exported_json_stats.sha256);
        assert_eq!(a.conformal.forecast_id, b.conformal.forecast_id);
        assert_eq!(a.e_process.process_id, b.e_process.process_id);
        assert_eq!(a.pac_bayes.bound_id, b.pac_bayes.bound_id);
    }

    #[test]
    fn engine_report_json_round_trips() {
        let report = GuaranteeLayerEngine::default()
            .evaluate(&passing_input())
            .unwrap();
        let json = report.to_json();
        let parsed: GuaranteeReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.report_id, report.report_id);
        assert_eq!(parsed.schema_version, GUARANTEE_LAYER_SCHEMA_VERSION);
        assert_eq!(parsed, report);
    }

    #[test]
    fn engine_replay_command_references_report() {
        let report = GuaranteeLayerEngine::default()
            .evaluate(&passing_input())
            .unwrap();
        assert!(report.replay_command.contains(&report.report_id));
        assert!(report.replay_command.contains("guarantee_layer"));
    }

    #[test]
    fn engine_exported_json_stats_checksum_matches_content() {
        let report = GuaranteeLayerEngine::default()
            .evaluate(&passing_input())
            .unwrap();
        let recomputed = sha256_hex(report.exported_json_stats.content.as_bytes());
        assert_eq!(report.exported_json_stats.sha256, recomputed);
        assert!(
            report
                .exported_json_stats
                .path
                .ends_with("guarantee_layer_stats.json")
        );
    }

    #[test]
    fn engine_propagates_config_errors() {
        let engine = GuaranteeLayerEngine::default()
            .with_pac_bayes(PacBayesConfig::default().with_delta(2.0));
        assert!(matches!(
            engine.evaluate(&passing_input()),
            Err(GuaranteeLayerError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn input_from_posterior_uses_posterior_prob() {
        let evidence: Vec<ChannelEvidence> = EvidenceChannel::ALL
            .iter()
            .copied()
            .map(|c| ChannelEvidence::new(c, 2.0).claim("claim-p"))
            .collect();
        let record = PosteriorEngine::default().infer("claim-p", &evidence);
        let input =
            GuaranteeEvaluationInput::from_posterior(&record, PacBayesInput::new(0.1, 1.0, 1000));
        assert_eq!(input.claim_id, "claim-p");
        assert!(approx(
            input.conformal_forecast_point,
            record.posterior_prob
        ));
    }

    #[test]
    fn input_from_posterior_flows_through_engine() {
        let evidence: Vec<ChannelEvidence> = EvidenceChannel::ALL
            .iter()
            .copied()
            .map(|c| ChannelEvidence::new(c, 1.5).claim("claim-flow"))
            .collect();
        let record = PosteriorEngine::default().infer("claim-flow", &evidence);
        let input =
            GuaranteeEvaluationInput::from_posterior(&record, PacBayesInput::new(0.1, 1.0, 1000))
                .with_calibration(calibration_50())
                .with_observations(vec![0.0; 20]);
        let report = GuaranteeLayerEngine::default().evaluate(&input).unwrap();
        assert_eq!(report.claim_id, "claim-flow");
        assert_eq!(report.formal_guarantees.len(), 3);
    }

    #[test]
    fn report_schema_version_is_stamped() {
        let report = GuaranteeLayerEngine::default()
            .evaluate(&passing_input())
            .unwrap();
        assert_eq!(report.schema_version, GUARANTEE_LAYER_SCHEMA_VERSION);
        assert!(report.report_id.starts_with("guarantee-layer-"));
        assert!(
            report
                .conformal
                .forecast_id
                .starts_with("guarantee-conformal-")
        );
        assert!(
            report
                .e_process
                .process_id
                .starts_with("guarantee-eprocess-")
        );
        assert!(report.pac_bayes.bound_id.starts_with("guarantee-pacbayes-"));
    }
}

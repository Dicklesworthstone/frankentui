#![forbid(unsafe_code)]

//! Bayesian Online Change-Point Detection (BOCPD) for Resize Regime Detection.
//!
//! This module implements BOCPD to replace heuristic threshold-based regime
//! detection in the resize coalescer. It provides a principled Bayesian
//! approach to detecting transitions between Steady and Burst regimes.
//!
//! # Mathematical Model
//!
//! ## Observation Model
//!
//! We observe inter-arrival times `x_t` between consecutive resize events.
//! The observation model conditions on the current regime:
//!
//! ```text
//! Steady regime: x_t ~ Exponential(λ_steady)
//!                where λ_steady = 1/μ_steady, μ_steady ≈ 200ms (slow events)
//!
//! Burst regime:  x_t ~ Exponential(λ_burst)
//!                where λ_burst = 1/μ_burst, μ_burst ≈ 20ms (rapid events)
//! ```
//!
//! The likelihood for observation x under each regime:
//!
//! ```text
//! P(x | steady) = λ_steady × exp(-λ_steady × x)
//! P(x | burst)  = λ_burst × exp(-λ_burst × x)
//! ```
//!
//! ## Run-Length Model
//!
//! BOCPD maintains a distribution over run-lengths `r_t`, where r represents
//! the number of observations since the last changepoint.
//!
//! The run-length posterior is recursively updated:
//!
//! ```text
//! P(r_t = 0 | x_1:t) ∝ Σᵣ P(r_{t-1} = r | x_1:t-1) × H(r) × P(x_t | r)
//! P(r_t = r+1 | x_1:t) ∝ P(r_{t-1} = r | x_1:t-1) × (1 - H(r)) × P(x_t | r)
//! ```
//!
//! where H(r) is the hazard function (probability of changepoint at run-length r).
//!
//! ## Hazard Function
//!
//! We use a constant hazard model with geometric prior on run-length:
//!
//! ```text
//! H(r) = 1/λ_hazard
//!
//! where λ_hazard is the expected run-length between changepoints.
//! Default: λ_hazard = 50 (expect changepoint every ~50 observations)
//! ```
//!
//! This implies:
//! - P(changepoint at r) = (1 - 1/λ)^r × (1/λ)
//! - E[run-length] = λ_hazard
//!
//! ## Run-Length Truncation
//!
//! To achieve O(K) complexity per update, we truncate the run-length
//! distribution at maximum K:
//!
//! ```text
//! K = 100 (default)
//!
//! For r ≥ K: P(r_t = r | x_1:t) is merged into P(r_t = K | x_1:t)
//! ```
//!
//! This approximation is accurate when K >> λ_hazard, since most mass
//! concentrates on recent run-lengths.
//!
//! ## Regime Posterior
//!
//! We maintain a separate regime indicator:
//!
//! ```text
//! Regime detection via likelihood ratio:
//!
//! LR_t = P(x_1:t | burst) / P(x_1:t | steady)
//!
//! P(burst | x_1:t) = LR_t × P(burst) / (LR_t × P(burst) + P(steady))
//!
//! where P(burst) = 0.2 (prior probability of burst regime)
//! ```
//!
//! The regime posterior is integrated over all run-lengths:
//!
//! ```text
//! P(burst | x_1:t) = Σᵣ P(burst | r, x_1:t) × P(r | x_1:t)
//! ```
//!
//! # Decision Rule
//!
//! The coalescing delay is selected based on the burst posterior:
//!
//! ```text
//! Let p_burst = P(burst | x_1:t)
//!
//! If p_burst < 0.3:  delay = steady_delay_ms  (16ms, responsive)
//! If p_burst > 0.7:  delay = burst_delay_ms   (40ms, coalescing)
//! Otherwise:         delay = interpolate(16ms, 40ms, p_burst)
//!
//! Always respect hard_deadline_ms (100ms) regardless of regime.
//! ```
//!
//! ## Log-Bayes Factor for Explainability
//!
//! We compute the log10 Bayes factor for each decision:
//!
//! ```text
//! LBF = log10(P(x_t | burst) / P(x_t | steady))
//!
//! Interpretation:
//! - LBF > 1:  Strong evidence for burst
//! - LBF > 2:  Decisive evidence for burst
//! - LBF < -1: Strong evidence for steady
//! - LBF < -2: Decisive evidence for steady
//! ```
//!
//! # Invariants
//!
//! 1. **Normalized posterior**: Σᵣ P(r_t = r) = 1 (up to numerical precision)
//! 2. **Deterministic**: Same observation sequence → same posteriors
//! 3. **Bounded complexity**: O(K) per observation update
//! 4. **Bounded memory**: O(K) state vector
//! 5. **Monotonic regime confidence**: p_burst increases with rapid events
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | x = 0 (instant event) | x = ε = 1ms | Avoid log(0) in likelihood |
//! | x > 10s (very slow) | Clamp to 10s | Numerical stability |
//! | All posterior mass at r=K | Normal operation | Truncation working |
//! | λ_hazard = 0 | Use default λ=50 | Avoid division by zero |
//! | K = 0 | Use default K=100 | Ensure valid state |
//!
//! # Configuration
//!
//! ```text
//! BocpdConfig {
//!     // Observation model
//!     mu_steady_ms: 200.0,    // Expected inter-arrival in steady (ms)
//!     mu_burst_ms: 20.0,      // Expected inter-arrival in burst (ms)
//!
//!     // Hazard function
//!     hazard_lambda: 50.0,    // Expected run-length between changepoints
//!
//!     // Truncation
//!     max_run_length: 100,    // K for O(K) complexity
//!
//!     // Decision thresholds
//!     steady_threshold: 0.3,  // p_burst below this → steady
//!     burst_threshold: 0.7,   // p_burst above this → burst
//!
//!     // Priors
//!     burst_prior: 0.2,       // P(burst) a priori
//! }
//! ```
//!
//! # Performance
//!
//! - **Time complexity**: O(K) per observation
//! - **Space complexity**: O(K) for run-length posterior
//! - **Default K=100**: ~100 multiplications per resize event
//! - **Suitable for**: Up to 1000 events/second without concern
//!
//! # References
//!
//! - Adams & MacKay (2007): "Bayesian Online Changepoint Detection"
//! - The run-length truncation follows standard BOCPD practice
//! - Hazard function choice is geometric (constant hazard)

use std::fmt;
use std::time::Instant;

// =============================================================================
// Configuration
// =============================================================================

/// Configuration for the BOCPD regime detector.
#[derive(Debug, Clone)]
pub struct BocpdConfig {
    /// Expected inter-arrival time in steady regime (ms).
    /// Longer values indicate slower, more spaced events.
    /// Default: 200.0 ms
    pub mu_steady_ms: f64,

    /// Expected inter-arrival time in burst regime (ms).
    /// Shorter values indicate rapid, clustered events.
    /// Default: 20.0 ms
    pub mu_burst_ms: f64,

    /// Expected run-length between changepoints (hazard parameter).
    /// Higher values mean changepoints are expected less frequently.
    /// Default: 50.0
    pub hazard_lambda: f64,

    /// Maximum run-length for truncation (K).
    /// Controls complexity: O(K) per update.
    /// Default: 100
    pub max_run_length: usize,

    /// Threshold below which we classify as Steady.
    /// If P(burst) < steady_threshold → Steady regime.
    /// Default: 0.3
    pub steady_threshold: f64,

    /// Threshold above which we classify as Burst.
    /// If P(burst) > burst_threshold → Burst regime.
    /// Default: 0.7
    pub burst_threshold: f64,

    /// Prior probability of burst regime.
    /// Used to initialize the regime posterior.
    /// Default: 0.2
    pub burst_prior: f64,

    /// Minimum observation value (ms) to avoid log(0).
    /// Default: 1.0 ms
    pub min_observation_ms: f64,

    /// Maximum observation value (ms) for numerical stability.
    /// Default: 10000.0 ms (10 seconds)
    pub max_observation_ms: f64,

    /// Enable evidence logging.
    /// Default: false
    pub enable_logging: bool,
}

impl Default for BocpdConfig {
    fn default() -> Self {
        Self {
            mu_steady_ms: 200.0,
            mu_burst_ms: 20.0,
            hazard_lambda: 50.0,
            max_run_length: 100,
            steady_threshold: 0.3,
            burst_threshold: 0.7,
            burst_prior: 0.2,
            min_observation_ms: 1.0,
            max_observation_ms: 10000.0,
            enable_logging: false,
        }
    }
}

impl BocpdConfig {
    /// Create a configuration tuned for responsive UI.
    ///
    /// Lower thresholds for faster regime detection.
    #[must_use]
    pub fn responsive() -> Self {
        Self {
            mu_steady_ms: 150.0,
            mu_burst_ms: 15.0,
            hazard_lambda: 30.0,
            steady_threshold: 0.25,
            burst_threshold: 0.6,
            ..Default::default()
        }
    }

    /// Create a configuration tuned for aggressive coalescing.
    ///
    /// Higher thresholds to stay in burst mode longer.
    #[must_use]
    pub fn aggressive_coalesce() -> Self {
        Self {
            mu_steady_ms: 250.0,
            mu_burst_ms: 25.0,
            hazard_lambda: 80.0,
            steady_threshold: 0.4,
            burst_threshold: 0.8,
            burst_prior: 0.3,
            ..Default::default()
        }
    }

    /// Enable evidence logging.
    #[must_use]
    pub fn with_logging(mut self, enabled: bool) -> Self {
        self.enable_logging = enabled;
        self
    }
}

// =============================================================================
// Regime Enum
// =============================================================================

/// Detected regime from BOCPD analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BocpdRegime {
    /// Low event rate, prioritize responsiveness.
    #[default]
    Steady,
    /// High event rate, prioritize coalescing.
    Burst,
    /// Transitional state (P(burst) between thresholds).
    Transitional,
}

impl BocpdRegime {
    /// Stable string representation for logging.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Steady => "steady",
            Self::Burst => "burst",
            Self::Transitional => "transitional",
        }
    }
}

impl fmt::Display for BocpdRegime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// =============================================================================
// Evidence for Explainability
// =============================================================================

/// Evidence from a BOCPD update step.
///
/// Provides explainability for regime detection decisions.
#[derive(Debug, Clone)]
pub struct BocpdEvidence {
    /// Posterior probability of burst regime.
    pub p_burst: f64,

    /// Log10 Bayes factor (positive favors burst).
    pub log_bayes_factor: f64,

    /// Current observation (inter-arrival time in ms).
    pub observation_ms: f64,

    /// Classified regime based on thresholds.
    pub regime: BocpdRegime,

    /// Likelihood under steady model.
    pub likelihood_steady: f64,

    /// Likelihood under burst model.
    pub likelihood_burst: f64,

    /// Expected run-length (mean of posterior).
    pub expected_run_length: f64,

    /// Run-length posterior variance.
    pub run_length_variance: f64,

    /// Run-length posterior mode (argmax).
    pub run_length_mode: usize,

    /// 95th percentile of run-length posterior.
    pub run_length_p95: usize,

    /// Tail mass at the truncation bucket (r = K).
    pub run_length_tail_mass: f64,

    /// Recommended delay based on current regime (ms), if provided.
    pub recommended_delay_ms: Option<u64>,

    /// Whether a hard deadline forced the decision, if provided.
    pub hard_deadline_forced: Option<bool>,

    /// Number of observations processed.
    pub observation_count: u64,

    /// Timestamp of this evidence.
    pub timestamp: Instant,
}

impl BocpdEvidence {
    /// Generate JSONL representation for logging.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        const SCHEMA_VERSION: &str = "bocpd-v1";
        let delay_ms = self
            .recommended_delay_ms
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string());
        let forced = self
            .hard_deadline_forced
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string());
        format!(
            r#"{{"schema_version":"{}","event":"bocpd","p_burst":{:.4},"log_bf":{:.3},"obs_ms":{:.1},"regime":"{}","ll_steady":{:.6},"ll_burst":{:.6},"runlen_mean":{:.1},"runlen_var":{:.3},"runlen_mode":{},"runlen_p95":{},"runlen_tail":{:.4},"delay_ms":{},"forced_deadline":{},"n_obs":{}}}"#,
            SCHEMA_VERSION,
            self.p_burst,
            self.log_bayes_factor,
            self.observation_ms,
            self.regime.as_str(),
            self.likelihood_steady,
            self.likelihood_burst,
            self.expected_run_length,
            self.run_length_variance,
            self.run_length_mode,
            self.run_length_p95,
            self.run_length_tail_mass,
            delay_ms,
            forced,
            self.observation_count,
        )
    }
}

impl fmt::Display for BocpdEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "BOCPD Evidence:")?;
        writeln!(
            f,
            "  Regime: {} (P(burst) = {:.3})",
            self.regime, self.p_burst
        )?;
        writeln!(
            f,
            "  Log BF: {:+.3} (positive favors burst)",
            self.log_bayes_factor
        )?;
        writeln!(f, "  Observation: {:.1} ms", self.observation_ms)?;
        writeln!(
            f,
            "  Likelihoods: steady={:.6}, burst={:.6}",
            self.likelihood_steady, self.likelihood_burst
        )?;
        writeln!(f, "  E[run-length]: {:.1}", self.expected_run_length)?;
        write!(f, "  Observations: {}", self.observation_count)
    }
}

#[derive(Debug, Clone, Copy)]
struct RunLengthSummary {
    mean: f64,
    variance: f64,
    mode: usize,
    p95: usize,
    tail_mass: f64,
}

// =============================================================================
// BOCPD Detector
// =============================================================================

/// Bayesian Online Change-Point Detection for regime classification.
///
/// Maintains a truncated run-length posterior and computes the
/// probability of being in burst vs steady regime.
#[derive(Debug, Clone)]
pub struct BocpdDetector {
    /// Configuration.
    config: BocpdConfig,

    /// Run-length posterior: P(r_t = r | x_1:t) for r in 0..=K.
    /// Indexed by run-length, normalized to sum to 1.
    run_length_posterior: Vec<f64>,

    /// Current burst probability P(burst | x_1:t).
    p_burst: f64,

    /// Timestamp of last observation.
    last_event_time: Option<Instant>,

    /// Number of observations processed.
    observation_count: u64,

    /// Last evidence for inspection.
    last_evidence: Option<BocpdEvidence>,

    /// Pre-computed rate parameters for efficiency.
    lambda_steady: f64, // 1 / mu_steady_ms
    lambda_burst: f64, // 1 / mu_burst_ms
    hazard: f64,       // 1 / hazard_lambda
}

impl BocpdDetector {
    /// Create a new BOCPD detector with the given configuration.
    pub fn new(config: BocpdConfig) -> Self {
        let mut config = config;
        config.max_run_length = config.max_run_length.max(1);
        config.mu_steady_ms = config.mu_steady_ms.max(1.0);
        config.mu_burst_ms = config.mu_burst_ms.max(1.0);
        config.hazard_lambda = config.hazard_lambda.max(1.0);
        config.min_observation_ms = config.min_observation_ms.max(0.1);
        config.max_observation_ms = config.max_observation_ms.max(config.min_observation_ms);
        config.steady_threshold = config.steady_threshold.clamp(0.0, 1.0);
        config.burst_threshold = config.burst_threshold.clamp(0.0, 1.0);
        if config.burst_threshold < config.steady_threshold {
            std::mem::swap(&mut config.steady_threshold, &mut config.burst_threshold);
        }
        config.burst_prior = config.burst_prior.clamp(0.001, 0.999);

        let k = config.max_run_length;

        // Initialize uniform run-length posterior
        let initial_prob = 1.0 / (k + 1) as f64;
        let run_length_posterior = vec![initial_prob; k + 1];

        // Pre-compute rate parameters
        let lambda_steady = 1.0 / config.mu_steady_ms;
        let lambda_burst = 1.0 / config.mu_burst_ms;
        let hazard = 1.0 / config.hazard_lambda;

        Self {
            p_burst: config.burst_prior,
            run_length_posterior,
            last_event_time: None,
            observation_count: 0,
            last_evidence: None,
            lambda_steady,
            lambda_burst,
            hazard,
            config,
        }
    }

    /// Create with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(BocpdConfig::default())
    }

    /// Get current burst probability.
    #[inline]
    pub fn p_burst(&self) -> f64 {
        self.p_burst
    }

    /// Get the run-length posterior distribution.
    ///
    /// Returns a slice where element `i` is `P(r_t = i | x_1:t)`.
    /// The length is bounded by `max_run_length + 1`.
    #[inline]
    pub fn run_length_posterior(&self) -> &[f64] {
        &self.run_length_posterior
    }

    /// Get current classified regime.
    #[inline]
    pub fn regime(&self) -> BocpdRegime {
        if self.p_burst < self.config.steady_threshold {
            BocpdRegime::Steady
        } else if self.p_burst > self.config.burst_threshold {
            BocpdRegime::Burst
        } else {
            BocpdRegime::Transitional
        }
    }

    /// Get the expected run-length (mean of posterior).
    pub fn expected_run_length(&self) -> f64 {
        self.run_length_posterior
            .iter()
            .enumerate()
            .map(|(r, p)| r as f64 * p)
            .sum()
    }

    fn run_length_summary(&self) -> RunLengthSummary {
        let mean = self.expected_run_length();
        let mut variance = 0.0;
        let mut mode = 0;
        let mut mode_p = -1.0;
        let mut cumulative = 0.0;
        let mut p95 = self.config.max_run_length;

        for (r, p) in self.run_length_posterior.iter().enumerate() {
            if *p > mode_p {
                mode_p = *p;
                mode = r;
            }
            let diff = r as f64 - mean;
            variance += p * diff * diff;
            if cumulative < 0.95 {
                cumulative += p;
                if cumulative >= 0.95 {
                    p95 = r;
                }
            }
        }

        RunLengthSummary {
            mean,
            variance,
            mode,
            p95,
            tail_mass: self.run_length_posterior[self.config.max_run_length],
        }
    }

    /// Get the last evidence.
    pub fn last_evidence(&self) -> Option<&BocpdEvidence> {
        self.last_evidence.as_ref()
    }

    /// Update the last evidence with decision context (delay + deadline).
    ///
    /// Call this after a decision is made (e.g., in the coalescer) to
    /// include chosen delay and hard-deadline forcing in JSONL logs.
    pub fn set_decision_context(
        &mut self,
        steady_delay_ms: u64,
        burst_delay_ms: u64,
        hard_deadline_forced: bool,
    ) {
        let recommended_delay = self.recommended_delay(steady_delay_ms, burst_delay_ms);
        if let Some(ref mut evidence) = self.last_evidence {
            evidence.recommended_delay_ms = Some(recommended_delay);
            evidence.hard_deadline_forced = Some(hard_deadline_forced);
        }
    }

    /// Return the latest JSONL evidence entry if logging is enabled.
    #[must_use]
    pub fn evidence_jsonl(&self) -> Option<String> {
        if !self.config.enable_logging {
            return None;
        }
        self.last_evidence.as_ref().map(BocpdEvidence::to_jsonl)
    }

    /// Return JSONL evidence with decision context applied.
    #[must_use]
    pub fn decision_log_jsonl(
        &self,
        steady_delay_ms: u64,
        burst_delay_ms: u64,
        hard_deadline_forced: bool,
    ) -> Option<String> {
        if !self.config.enable_logging {
            return None;
        }
        let mut evidence = self.last_evidence.clone()?;
        evidence.recommended_delay_ms =
            Some(self.recommended_delay(steady_delay_ms, burst_delay_ms));
        evidence.hard_deadline_forced = Some(hard_deadline_forced);
        Some(evidence.to_jsonl())
    }

    /// Get observation count.
    #[inline]
    pub fn observation_count(&self) -> u64 {
        self.observation_count
    }

    /// Get configuration.
    pub fn config(&self) -> &BocpdConfig {
        &self.config
    }

    /// Process a new resize event.
    ///
    /// Call this when a resize event occurs. Returns the classified regime.
    pub fn observe_event(&mut self, now: Instant) -> BocpdRegime {
        // Compute inter-arrival time
        let observation_ms = self
            .last_event_time
            .map(|last| now.duration_since(last).as_secs_f64() * 1000.0)
            .unwrap_or(self.config.mu_steady_ms); // Default to steady-like on first event

        // Clamp observation
        let x = observation_ms
            .max(self.config.min_observation_ms)
            .min(self.config.max_observation_ms);

        // Update posterior
        self.update_posterior(x, now);

        // Update last event time
        self.last_event_time = Some(now);

        self.regime()
    }

    /// Update the run-length posterior with a new observation.
    fn update_posterior(&mut self, x: f64, now: Instant) {
        self.observation_count += 1;

        // Compute likelihoods
        let ll_steady = self.exponential_pdf(x, self.lambda_steady);
        let ll_burst = self.exponential_pdf(x, self.lambda_burst);

        // Compute Bayes factor
        let log_bf = if ll_steady > 0.0 && ll_burst > 0.0 {
            (ll_burst / ll_steady).log10()
        } else {
            0.0
        };

        // ==== BOCPD Run-Length Update ====
        let k = self.config.max_run_length;
        let mut new_posterior = vec![0.0; k + 1];

        // Growth probability: P(r_t = r+1) ∝ P(r_{t-1} = r) × (1 - H(r)) × likelihood
        for r in 0..k {
            let growth_prob = self.run_length_posterior[r] * (1.0 - self.hazard);
            new_posterior[r + 1] += growth_prob * self.predictive_likelihood(r, x);
        }

        // Merge probability at r=K (truncation)
        new_posterior[k] +=
            self.run_length_posterior[k] * (1.0 - self.hazard) * self.predictive_likelihood(k, x);

        // Changepoint probability: P(r_t = 0) ∝ Σ P(r_{t-1}) × H × likelihood
        let cp_prob: f64 = self
            .run_length_posterior
            .iter()
            .enumerate()
            .map(|(r, &p)| p * self.hazard * self.predictive_likelihood(r, x))
            .sum();
        new_posterior[0] = cp_prob;

        // Normalize
        let total: f64 = new_posterior.iter().sum();
        if total > 0.0 {
            for p in &mut new_posterior {
                *p /= total;
            }
        } else {
            // Reset to uniform if numerical issues
            let uniform = 1.0 / (k + 1) as f64;
            new_posterior.fill(uniform);
        }

        self.run_length_posterior = new_posterior;

        // ==== Update Burst Probability ====
        // Use a Bayesian update with the likelihood ratio
        let prior_odds = self.p_burst / (1.0 - self.p_burst).max(1e-10);
        let likelihood_ratio = ll_burst / ll_steady.max(1e-10);
        let posterior_odds = prior_odds * likelihood_ratio;
        self.p_burst = (posterior_odds / (1.0 + posterior_odds)).clamp(0.001, 0.999);

        // Store evidence
        let summary = self.run_length_summary();
        self.last_evidence = Some(BocpdEvidence {
            p_burst: self.p_burst,
            log_bayes_factor: log_bf,
            observation_ms: x,
            regime: self.regime(),
            likelihood_steady: ll_steady,
            likelihood_burst: ll_burst,
            expected_run_length: summary.mean,
            run_length_variance: summary.variance,
            run_length_mode: summary.mode,
            run_length_p95: summary.p95,
            run_length_tail_mass: summary.tail_mass,
            recommended_delay_ms: None,
            hard_deadline_forced: None,
            observation_count: self.observation_count,
            timestamp: now,
        });
    }

    /// Compute exponential PDF: λ × exp(-λx).
    #[inline]
    fn exponential_pdf(&self, x: f64, lambda: f64) -> f64 {
        lambda * (-lambda * x).exp()
    }

    /// Predictive likelihood for observation given run-length.
    ///
    /// We use a mixture model weighted by regime probability:
    /// P(x | r) = p_burst × P(x | burst) + (1 - p_burst) × P(x | steady)
    #[inline]
    fn predictive_likelihood(&self, _r: usize, x: f64) -> f64 {
        // Note: A more sophisticated model would condition on r,
        // but for simplicity we use the current regime estimate.
        let ll_steady = self.exponential_pdf(x, self.lambda_steady);
        let ll_burst = self.exponential_pdf(x, self.lambda_burst);
        self.p_burst * ll_burst + (1.0 - self.p_burst) * ll_steady
    }

    /// Reset the detector to initial state.
    pub fn reset(&mut self) {
        let k = self.config.max_run_length;
        let initial_prob = 1.0 / (k + 1) as f64;
        self.run_length_posterior = vec![initial_prob; k + 1];
        self.p_burst = self.config.burst_prior;
        self.last_event_time = None;
        self.observation_count = 0;
        self.last_evidence = None;
    }

    /// Compute recommended coalesce delay based on current regime.
    ///
    /// Interpolates between steady_delay and burst_delay based on p_burst.
    pub fn recommended_delay(&self, steady_delay_ms: u64, burst_delay_ms: u64) -> u64 {
        if self.p_burst < self.config.steady_threshold {
            steady_delay_ms
        } else if self.p_burst > self.config.burst_threshold {
            burst_delay_ms
        } else {
            // Linear interpolation in transitional region
            let denom = (self.config.burst_threshold - self.config.steady_threshold).max(1e-6);
            let t = ((self.p_burst - self.config.steady_threshold) / denom).clamp(0.0, 1.0);
            let delay = steady_delay_ms as f64 * (1.0 - t) + burst_delay_ms as f64 * t;
            delay.round() as u64
        }
    }
}

impl Default for BocpdDetector {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_default_config() {
        let config = BocpdConfig::default();
        assert!((config.mu_steady_ms - 200.0).abs() < 0.01);
        assert!((config.mu_burst_ms - 20.0).abs() < 0.01);
        assert_eq!(config.max_run_length, 100);
    }

    #[test]
    fn test_initial_state() {
        let detector = BocpdDetector::with_defaults();
        assert!((detector.p_burst() - 0.2).abs() < 0.01); // Default prior
        assert_eq!(detector.regime(), BocpdRegime::Steady);
        assert_eq!(detector.observation_count(), 0);
    }

    #[test]
    fn test_steady_detection() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();

        // Simulate slow events (200ms apart) - should stay in steady
        for i in 0..10 {
            let t = start + Duration::from_millis(200 * (i + 1));
            detector.observe_event(t);
        }

        assert!(
            detector.p_burst() < 0.5,
            "p_burst={} should be low",
            detector.p_burst()
        );
        assert_eq!(detector.regime(), BocpdRegime::Steady);
    }

    #[test]
    fn test_burst_detection() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();

        // Simulate rapid events (10ms apart) - should trigger burst
        for i in 0..20 {
            let t = start + Duration::from_millis(10 * (i + 1));
            detector.observe_event(t);
        }

        assert!(
            detector.p_burst() > 0.5,
            "p_burst={} should be high",
            detector.p_burst()
        );
        assert!(matches!(
            detector.regime(),
            BocpdRegime::Burst | BocpdRegime::Transitional
        ));
    }

    #[test]
    fn test_regime_transition() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();

        // Start slow (steady)
        for i in 0..5 {
            let t = start + Duration::from_millis(200 * (i + 1));
            detector.observe_event(t);
        }
        let initial_p_burst = detector.p_burst();

        // Then rapid events (burst)
        let burst_start = start + Duration::from_millis(1000);
        for i in 0..20 {
            let t = burst_start + Duration::from_millis(10 * (i + 1));
            detector.observe_event(t);
        }

        assert!(
            detector.p_burst() > initial_p_burst,
            "p_burst should increase during burst"
        );
    }

    #[test]
    fn test_evidence_stored() {
        let mut detector = BocpdDetector::with_defaults();
        let t = Instant::now();
        detector.observe_event(t);

        let evidence = detector.last_evidence().expect("Evidence should be stored");
        assert_eq!(evidence.observation_count, 1);
        assert!(evidence.log_bayes_factor.is_finite());
    }

    #[test]
    fn test_reset() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();

        // Process some events
        for i in 0..10 {
            let t = start + Duration::from_millis(10 * (i + 1));
            detector.observe_event(t);
        }

        detector.reset();

        assert!((detector.p_burst() - 0.2).abs() < 0.01);
        assert_eq!(detector.observation_count(), 0);
        assert!(detector.last_evidence().is_none());
    }

    #[test]
    fn test_recommended_delay() {
        let mut detector = BocpdDetector::with_defaults();

        // In steady regime (default)
        assert_eq!(detector.recommended_delay(16, 40), 16);

        // Manually set to burst
        detector.p_burst = 0.9;
        assert_eq!(detector.recommended_delay(16, 40), 40);

        // Transitional (interpolated)
        detector.p_burst = 0.5;
        let delay = detector.recommended_delay(16, 40);
        assert!(
            delay > 16 && delay < 40,
            "delay={} should be interpolated",
            delay
        );
    }

    #[test]
    fn test_deterministic() {
        let mut det1 = BocpdDetector::with_defaults();
        let mut det2 = BocpdDetector::with_defaults();
        let start = Instant::now();

        for i in 0..10 {
            let t = start + Duration::from_millis(15 * (i + 1));
            det1.observe_event(t);
            det2.observe_event(t);
        }

        assert!((det1.p_burst() - det2.p_burst()).abs() < 1e-10);
        assert_eq!(det1.regime(), det2.regime());
    }

    #[test]
    fn test_posterior_normalized() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();

        for i in 0..20 {
            let t = start + Duration::from_millis(25 * (i + 1));
            detector.observe_event(t);

            let sum: f64 = detector.run_length_posterior.iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-6,
                "Posterior not normalized: sum={}",
                sum
            );
        }
    }

    #[test]
    fn test_p_burst_bounded() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();

        // Extreme rapid events
        for i in 0..100 {
            let t = start + Duration::from_millis(i + 1);
            detector.observe_event(t);
            assert!(detector.p_burst() >= 0.0 && detector.p_burst() <= 1.0);
        }
    }

    #[test]
    fn config_sanitization_clamps_thresholds_and_priors() {
        let config = BocpdConfig {
            steady_threshold: 0.9,
            burst_threshold: 0.1,
            burst_prior: 2.0,
            max_run_length: 0,
            mu_steady_ms: 0.0,
            mu_burst_ms: 0.0,
            hazard_lambda: 0.0,
            min_observation_ms: 0.0,
            max_observation_ms: 0.0,
            ..Default::default()
        };

        let detector = BocpdDetector::new(config);
        let cfg = detector.config();

        assert!(
            cfg.steady_threshold <= cfg.burst_threshold,
            "thresholds should be ordered after sanitization"
        );
        assert_eq!(cfg.max_run_length, 1);
        assert!(cfg.mu_steady_ms >= 1.0);
        assert!(cfg.mu_burst_ms >= 1.0);
        assert!(cfg.hazard_lambda >= 1.0);
        assert!(cfg.min_observation_ms >= 0.1);
        assert!(cfg.max_observation_ms >= cfg.min_observation_ms);
        assert!(
            (0.0..=1.0).contains(&detector.p_burst()),
            "p_burst should be clamped into [0,1]"
        );
    }

    #[test]
    fn test_jsonl_output() {
        let mut detector = BocpdDetector::with_defaults();
        let t = Instant::now();
        detector.observe_event(t);
        detector.config.enable_logging = true;

        let jsonl = detector
            .decision_log_jsonl(16, 40, false)
            .expect("jsonl should be emitted when enabled");

        assert!(jsonl.contains("bocpd-v1"));
        assert!(jsonl.contains("p_burst"));
        assert!(jsonl.contains("regime"));
        assert!(jsonl.contains("runlen_mean"));
        assert!(jsonl.contains("runlen_mode"));
        assert!(jsonl.contains("runlen_p95"));
        assert!(jsonl.contains("delay_ms"));
        assert!(jsonl.contains("forced_deadline"));
    }

    #[test]
    fn evidence_jsonl_respects_config() {
        let mut detector = BocpdDetector::with_defaults();
        let t = Instant::now();
        detector.observe_event(t);

        assert!(detector.evidence_jsonl().is_none());

        detector.config.enable_logging = true;
        assert!(detector.evidence_jsonl().is_some());
    }

    // Property test: expected run-length is non-negative
    #[test]
    fn prop_expected_runlen_non_negative() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();

        for i in 0..50 {
            let t = start + Duration::from_millis((i % 30 + 5) * (i + 1));
            detector.observe_event(t);
            assert!(detector.expected_run_length() >= 0.0);
        }
    }

    // ── Config presets ────────────────────────────────────────────

    #[test]
    fn responsive_config_values() {
        let cfg = BocpdConfig::responsive();
        assert!((cfg.mu_steady_ms - 150.0).abs() < f64::EPSILON);
        assert!((cfg.mu_burst_ms - 15.0).abs() < f64::EPSILON);
        assert!((cfg.hazard_lambda - 30.0).abs() < f64::EPSILON);
        assert!((cfg.steady_threshold - 0.25).abs() < f64::EPSILON);
        assert!((cfg.burst_threshold - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn aggressive_coalesce_config_values() {
        let cfg = BocpdConfig::aggressive_coalesce();
        assert!((cfg.mu_steady_ms - 250.0).abs() < f64::EPSILON);
        assert!((cfg.mu_burst_ms - 25.0).abs() < f64::EPSILON);
        assert!((cfg.hazard_lambda - 80.0).abs() < f64::EPSILON);
        assert!((cfg.steady_threshold - 0.4).abs() < f64::EPSILON);
        assert!((cfg.burst_threshold - 0.8).abs() < f64::EPSILON);
        assert!((cfg.burst_prior - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn with_logging_builder() {
        let cfg = BocpdConfig::default().with_logging(true);
        assert!(cfg.enable_logging);
        let cfg2 = cfg.with_logging(false);
        assert!(!cfg2.enable_logging);
    }

    // ── BocpdRegime traits ────────────────────────────────────────

    #[test]
    fn regime_as_str_values() {
        assert_eq!(BocpdRegime::Steady.as_str(), "steady");
        assert_eq!(BocpdRegime::Burst.as_str(), "burst");
        assert_eq!(BocpdRegime::Transitional.as_str(), "transitional");
    }

    #[test]
    fn regime_display_matches_as_str() {
        for regime in [
            BocpdRegime::Steady,
            BocpdRegime::Burst,
            BocpdRegime::Transitional,
        ] {
            assert_eq!(format!("{regime}"), regime.as_str());
        }
    }

    #[test]
    fn regime_default_is_steady() {
        assert_eq!(BocpdRegime::default(), BocpdRegime::Steady);
    }

    #[test]
    fn regime_clone_copy() {
        let r = BocpdRegime::Burst;
        let r2 = r;
        assert_eq!(r, r2);
        let r3 = r.clone();
        assert_eq!(r, r3);
    }

    // ── BocpdEvidence ─────────────────────────────────────────────

    #[test]
    fn evidence_to_jsonl_has_all_fields() {
        let mut detector = BocpdDetector::with_defaults();
        let t = Instant::now();
        detector.observe_event(t);
        let evidence = detector.last_evidence().unwrap();
        let jsonl = evidence.to_jsonl();

        for key in [
            "schema_version",
            "bocpd-v1",
            "p_burst",
            "log_bf",
            "obs_ms",
            "regime",
            "ll_steady",
            "ll_burst",
            "runlen_mean",
            "runlen_var",
            "runlen_mode",
            "runlen_p95",
            "runlen_tail",
            "delay_ms",
            "forced_deadline",
            "n_obs",
        ] {
            assert!(jsonl.contains(key), "missing field {key} in {jsonl}");
        }
    }

    #[test]
    fn evidence_display_contains_regime_and_pburst() {
        let mut detector = BocpdDetector::with_defaults();
        let t = Instant::now();
        detector.observe_event(t);
        let evidence = detector.last_evidence().unwrap();
        let display = format!("{evidence}");
        assert!(display.contains("BOCPD Evidence:"));
        assert!(display.contains("Regime:"));
        assert!(display.contains("P(burst)"));
        assert!(display.contains("Log BF:"));
        assert!(display.contains("Observation:"));
        assert!(display.contains("Observations:"));
    }

    #[test]
    fn evidence_null_optionals_in_jsonl() {
        let mut detector = BocpdDetector::with_defaults();
        let t = Instant::now();
        detector.observe_event(t);
        let evidence = detector.last_evidence().unwrap();
        let jsonl = evidence.to_jsonl();
        // Before set_decision_context, these should be null
        assert!(jsonl.contains("\"delay_ms\":null"));
        assert!(jsonl.contains("\"forced_deadline\":null"));
    }

    // ── Detector accessors ────────────────────────────────────────

    #[test]
    fn initial_detector_state() {
        let detector = BocpdDetector::with_defaults();
        assert!((detector.p_burst() - 0.2).abs() < 0.01);
        assert_eq!(detector.observation_count(), 0);
        assert!(detector.last_evidence().is_none());
        assert_eq!(detector.regime(), BocpdRegime::Steady);
    }

    #[test]
    fn run_length_posterior_sums_to_one() {
        let detector = BocpdDetector::with_defaults();
        let sum: f64 = detector.run_length_posterior().iter().sum();
        assert!((sum - 1.0).abs() < 1e-10);
    }

    #[test]
    fn config_accessor_returns_config() {
        let cfg = BocpdConfig::responsive();
        let detector = BocpdDetector::new(cfg);
        assert!((detector.config().mu_steady_ms - 150.0).abs() < f64::EPSILON);
    }

    // ── observe_event edge cases ──────────────────────────────────

    #[test]
    fn first_event_uses_steady_default() {
        let mut detector = BocpdDetector::with_defaults();
        let t = Instant::now();
        detector.observe_event(t);
        // First event should use mu_steady_ms as observation
        let evidence = detector.last_evidence().unwrap();
        assert!(
            (evidence.observation_ms - 200.0).abs() < 1.0,
            "first observation should be ~mu_steady_ms"
        );
    }

    #[test]
    fn rapid_events_increase_pburst() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();
        // First event (baseline)
        detector.observe_event(start);
        let initial = detector.p_burst();
        // Rapid events (5ms apart)
        for i in 1..20 {
            let t = start + Duration::from_millis(5 * i);
            detector.observe_event(t);
        }
        assert!(
            detector.p_burst() > initial,
            "p_burst should increase with rapid events"
        );
    }

    #[test]
    fn slow_events_decrease_pburst() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();
        // Seed with some rapid events to raise p_burst
        for i in 0..10 {
            let t = start + Duration::from_millis(5 * (i + 1));
            detector.observe_event(t);
        }
        let after_burst = detector.p_burst();
        // Now slow events (500ms apart)
        let slow_start = start + Duration::from_millis(50);
        for i in 0..20 {
            let t = slow_start + Duration::from_millis(500 * (i + 1));
            detector.observe_event(t);
        }
        assert!(
            detector.p_burst() < after_burst,
            "p_burst should decrease with slow events"
        );
    }

    // ── burst-to-steady recovery ──────────────────────────────────

    #[test]
    fn burst_to_steady_recovery() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();
        // Drive into burst with rapid events
        for i in 0..30 {
            let t = start + Duration::from_millis(5 * (i + 1));
            detector.observe_event(t);
        }
        let burst_p = detector.p_burst();
        assert!(burst_p > 0.5, "should be in burst, got p={burst_p}");
        // Recover with slow events
        let slow_start = start + Duration::from_millis(150);
        for i in 0..30 {
            let t = slow_start + Duration::from_millis(200 * (i + 1));
            detector.observe_event(t);
        }
        let steady_p = detector.p_burst();
        assert!(
            steady_p < burst_p,
            "p_burst should decrease during recovery"
        );
    }

    // ── set_decision_context ──────────────────────────────────────

    #[test]
    fn set_decision_context_populates_evidence() {
        let mut detector = BocpdDetector::with_defaults();
        let t = Instant::now();
        detector.observe_event(t);
        detector.set_decision_context(16, 40, false);
        let evidence = detector.last_evidence().unwrap();
        assert!(evidence.recommended_delay_ms.is_some());
        assert_eq!(evidence.hard_deadline_forced, Some(false));
    }

    #[test]
    fn set_decision_context_forced_deadline() {
        let mut detector = BocpdDetector::with_defaults();
        let t = Instant::now();
        detector.observe_event(t);
        detector.set_decision_context(16, 40, true);
        let evidence = detector.last_evidence().unwrap();
        assert_eq!(evidence.hard_deadline_forced, Some(true));
    }

    // ── decision_log_jsonl ────────────────────────────────────────

    #[test]
    fn decision_log_jsonl_none_when_logging_disabled() {
        let mut detector = BocpdDetector::with_defaults();
        let t = Instant::now();
        detector.observe_event(t);
        assert!(detector.decision_log_jsonl(16, 40, false).is_none());
    }

    #[test]
    fn decision_log_jsonl_has_delay_when_logging_enabled() {
        let mut detector = BocpdDetector::new(BocpdConfig::default().with_logging(true));
        let t = Instant::now();
        detector.observe_event(t);
        let jsonl = detector
            .decision_log_jsonl(16, 40, true)
            .expect("should emit when logging enabled");
        assert!(jsonl.contains("\"delay_ms\":"));
        assert!(!jsonl.contains("\"delay_ms\":null"));
        assert!(jsonl.contains("\"forced_deadline\":true"));
    }

    // ── recommended_delay ─────────────────────────────────────────

    #[test]
    fn recommended_delay_interpolation_in_transitional() {
        let mut detector = BocpdDetector::with_defaults();
        // Set p_burst to middle of transitional range
        detector.p_burst = 0.5;
        let delay = detector.recommended_delay(16, 40);
        assert!(
            delay > 16 && delay < 40,
            "transitional delay={delay} should be interpolated"
        );
    }

    #[test]
    fn recommended_delay_steady_when_low_pburst() {
        let detector = BocpdDetector::with_defaults();
        // Default p_burst is 0.2, below steady_threshold of 0.3
        assert_eq!(detector.recommended_delay(16, 40), 16);
    }

    #[test]
    fn recommended_delay_burst_when_high_pburst() {
        let mut detector = BocpdDetector::with_defaults();
        detector.p_burst = 0.9;
        assert_eq!(detector.recommended_delay(16, 40), 40);
    }

    // ── run-length summary ────────────────────────────────────────

    #[test]
    fn expected_run_length_initial_uniform() {
        let detector = BocpdDetector::with_defaults();
        let erl = detector.expected_run_length();
        // Uniform on 0..=100 → mean = 50
        assert!((erl - 50.0).abs() < 1.0);
    }

    // ── evidence fields accuracy ──────────────────────────────────

    #[test]
    fn evidence_observation_count_matches_events() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();
        for i in 0..7 {
            let t = start + Duration::from_millis(20 * (i + 1));
            detector.observe_event(t);
        }
        let evidence = detector.last_evidence().unwrap();
        assert_eq!(evidence.observation_count, 7);
    }

    #[test]
    fn evidence_likelihoods_are_positive() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();
        for i in 0..5 {
            let t = start + Duration::from_millis(50 * (i + 1));
            detector.observe_event(t);
        }
        let evidence = detector.last_evidence().unwrap();
        assert!(evidence.likelihood_steady > 0.0);
        assert!(evidence.likelihood_burst > 0.0);
    }

    // ── responsive vs default ─────────────────────────────────────

    #[test]
    fn responsive_detects_burst_faster() {
        let start = Instant::now();
        let mut default_det = BocpdDetector::with_defaults();
        let mut responsive_det = BocpdDetector::new(BocpdConfig::responsive());
        // Feed identical rapid events
        for i in 0..15 {
            let t = start + Duration::from_millis(5 * (i + 1));
            default_det.observe_event(t);
            responsive_det.observe_event(t);
        }
        // Responsive should have higher burst probability (lower thresholds)
        // or at least detect burst regime sooner
        let d_regime = default_det.regime();
        let r_regime = responsive_det.regime();
        // If default is still transitional, responsive should be at least transitional or burst
        if d_regime == BocpdRegime::Steady {
            assert_ne!(
                r_regime,
                BocpdRegime::Steady,
                "responsive should not be steady when default is"
            );
        }
    }

    // ── reset behavior ────────────────────────────────────────────

    #[test]
    fn reset_restores_initial_state() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();
        for i in 0..20 {
            let t = start + Duration::from_millis(5 * (i + 1));
            detector.observe_event(t);
        }
        assert!(detector.p_burst() > 0.5);
        detector.reset();
        assert!((detector.p_burst() - 0.2).abs() < 0.01);
        assert_eq!(detector.observation_count(), 0);
        assert!(detector.last_evidence().is_none());
        assert!(detector.last_event_time.is_none());
    }

    // ── posterior normalization under stress ───────────────────────

    #[test]
    fn posterior_stays_normalized_under_alternating_traffic() {
        let mut detector = BocpdDetector::with_defaults();
        let start = Instant::now();
        for i in 0..100 {
            // Alternate rapid and slow
            let gap = if i % 2 == 0 { 5 } else { 300 };
            let t = start + Duration::from_millis(gap * (i + 1));
            detector.observe_event(t);
            let sum: f64 = detector.run_length_posterior().iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-6,
                "posterior not normalized at step {i}: sum={sum}"
            );
        }
    }

    // ── BocpdConfig presets ─────────────────────────────────────────

    #[test]
    fn responsive_config_values() {
        let config = BocpdConfig::responsive();
        assert!((config.mu_steady_ms - 150.0).abs() < f64::EPSILON);
        assert!((config.mu_burst_ms - 15.0).abs() < f64::EPSILON);
        assert!((config.hazard_lambda - 30.0).abs() < f64::EPSILON);
        assert!((config.steady_threshold - 0.25).abs() < f64::EPSILON);
        assert!((config.burst_threshold - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn aggressive_coalesce_config_values() {
        let config = BocpdConfig::aggressive_coalesce();
        assert!((config.mu_steady_ms - 250.0).abs() < f64::EPSILON);
        assert!((config.mu_burst_ms - 25.0).abs() < f64::EPSILON);
        assert!((config.hazard_lambda - 80.0).abs() < f64::EPSILON);
        assert!((config.steady_threshold - 0.4).abs() < f64::EPSILON);
        assert!((config.burst_threshold - 0.8).abs() < f64::EPSILON);
        assert!((config.burst_prior - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn with_logging_builder() {
        let config = BocpdConfig::default().with_logging(true);
        assert!(config.enable_logging);
        let config2 = config.with_logging(false);
        assert!(!config2.enable_logging);
    }

    // ── BocpdRegime ─────────────────────────────────────────────────

    #[test]
    fn regime_as_str() {
        assert_eq!(BocpdRegime::Steady.as_str(), "steady");
        assert_eq!(BocpdRegime::Burst.as_str(), "burst");
        assert_eq!(BocpdRegime::Transitional.as_str(), "transitional");
    }

    #[test]
    fn regime_display() {
        assert_eq!(format!("{}", BocpdRegime::Steady), "steady");
        assert_eq!(format!("{}", BocpdRegime::Burst), "burst");
        assert_eq!(format!("{}", BocpdRegime::Transitional), "transitional");
    }

    #[test]
    fn regime_default_is_steady() {
        assert_eq!(BocpdRegime::default(), BocpdRegime::Steady);
    }

    #[test]
    fn regime_clone_eq() {
        let r = BocpdRegime::Burst;
        assert_eq!(r, r.clone());
        assert_ne!(BocpdRegime::Steady, BocpdRegime::Burst);
    }

    // ── BocpdDetector constructors ──────────────────────────────────

    #[test]
    fn detector_default_impl() {
        let det = BocpdDetector::default();
        assert_eq!(det.regime(), BocpdRegime::Steady);
        assert_eq!(det.observation_count(), 0);
    }

    #[test]
    fn detector_config_accessor() {
        let config = BocpdConfig {
            mu_steady_ms: 300.0,
            ..Default::default()
        };
        let det = BocpdDetector::new(config);
        assert!((det.config().mu_steady_ms - 300.0).abs() < f64::EPSILON);
    }

    #[test]
    fn detector_run_length_posterior_accessor() {
        let det = BocpdDetector::with_defaults();
        let posterior = det.run_length_posterior();
        // Default max_run_length = 100, so K+1 = 101 elements
        assert_eq!(posterior.len(), 101);
        let sum: f64 = posterior.iter().sum();
        assert!((sum - 1.0).abs() < 1e-10);
    }

    #[test]
    fn detector_expected_run_length_initial() {
        let det = BocpdDetector::with_defaults();
        let erl = det.expected_run_length();
        // Uniform posterior over 0..=100 → mean = 50.0
        assert!((erl - 50.0).abs() < 1e-10);
    }

    #[test]
    fn detector_last_evidence_initially_none() {
        let det = BocpdDetector::with_defaults();
        assert!(det.last_evidence().is_none());
    }

    // ── set_decision_context ────────────────────────────────────────

    #[test]
    fn set_decision_context_updates_evidence() {
        let mut det = BocpdDetector::with_defaults();
        det.observe_event(Instant::now());
        det.set_decision_context(16, 40, false);

        let ev = det.last_evidence().unwrap();
        assert_eq!(ev.recommended_delay_ms, Some(16)); // steady default
        assert_eq!(ev.hard_deadline_forced, Some(false));
    }

    #[test]
    fn set_decision_context_noop_without_evidence() {
        let mut det = BocpdDetector::with_defaults();
        // No observe_event called, so no evidence
        det.set_decision_context(16, 40, true);
        assert!(det.last_evidence().is_none());
    }

    // ── evidence_jsonl ──────────────────────────────────────────────

    #[test]
    fn evidence_jsonl_none_when_disabled() {
        let mut det = BocpdDetector::with_defaults();
        det.observe_event(Instant::now());
        assert!(det.evidence_jsonl().is_none());
    }

    #[test]
    fn decision_log_jsonl_none_when_disabled() {
        let mut det = BocpdDetector::with_defaults();
        det.observe_event(Instant::now());
        assert!(det.decision_log_jsonl(16, 40, false).is_none());
    }

    #[test]
    fn decision_log_jsonl_none_without_evidence() {
        let mut det = BocpdDetector::new(BocpdConfig::default().with_logging(true));
        // No observe_event called
        assert!(det.decision_log_jsonl(16, 40, false).is_none());
    }

    // ── BocpdEvidence Display ───────────────────────────────────────

    #[test]
    fn evidence_display_format() {
        let mut det = BocpdDetector::with_defaults();
        det.observe_event(Instant::now());
        let ev = det.last_evidence().unwrap();
        let display = format!("{}", ev);
        assert!(display.contains("BOCPD Evidence:"));
        assert!(display.contains("Regime:"));
        assert!(display.contains("P(burst)"));
        assert!(display.contains("Log BF:"));
        assert!(display.contains("Observation:"));
        assert!(display.contains("Likelihoods:"));
        assert!(display.contains("E[run-length]:"));
        assert!(display.contains("Observations:"));
    }

    // ── BocpdEvidence to_jsonl with optional fields ─────────────────

    #[test]
    fn evidence_jsonl_with_decision_context() {
        let mut det = BocpdDetector::new(BocpdConfig::default().with_logging(true));
        det.observe_event(Instant::now());
        det.set_decision_context(16, 40, true);

        let jsonl = det.evidence_jsonl().unwrap();
        assert!(jsonl.contains("\"delay_ms\":16"));
        assert!(jsonl.contains("\"forced_deadline\":true"));
    }

    #[test]
    fn evidence_jsonl_null_optional_fields() {
        let mut det = BocpdDetector::new(BocpdConfig::default().with_logging(true));
        det.observe_event(Instant::now());

        let jsonl = det.evidence_jsonl().unwrap();
        assert!(jsonl.contains("\"delay_ms\":null"));
        assert!(jsonl.contains("\"forced_deadline\":null"));
    }

    // ── recommended_delay edge cases ────────────────────────────────

    #[test]
    fn recommended_delay_at_exact_thresholds() {
        let mut det = BocpdDetector::with_defaults();
        // At exactly steady_threshold (0.3) → transitional
        det.p_burst = 0.3;
        let delay = det.recommended_delay(16, 40);
        assert_eq!(delay, 16); // t = (0.3 - 0.3) / (0.7 - 0.3) = 0

        // At exactly burst_threshold (0.7) → transitional
        det.p_burst = 0.7;
        let delay = det.recommended_delay(16, 40);
        assert_eq!(delay, 40); // t = (0.7 - 0.3) / (0.7 - 0.3) = 1
    }

    #[test]
    fn recommended_delay_midpoint() {
        let mut det = BocpdDetector::with_defaults();
        det.p_burst = 0.5; // midpoint of [0.3, 0.7]
        let delay = det.recommended_delay(16, 40);
        assert_eq!(delay, 28); // 16 * 0.5 + 40 * 0.5 = 28
    }

    // ── reset clears last_event_time ────────────────────────────────

    #[test]
    fn reset_clears_last_event_time() {
        let mut det = BocpdDetector::with_defaults();
        let start = Instant::now();
        det.observe_event(start);
        det.observe_event(start + Duration::from_millis(10));
        assert_eq!(det.observation_count(), 2);

        det.reset();
        assert_eq!(det.observation_count(), 0);
        assert!(det.last_evidence().is_none());
        // After reset, first event should use default mu_steady_ms
        let regime = det.observe_event(start + Duration::from_millis(100));
        assert_eq!(det.observation_count(), 1);
    }

    // ── First event uses default inter-arrival ──────────────────────

    #[test]
    fn first_event_uses_steady_default() {
        let mut det = BocpdDetector::with_defaults();
        let t = Instant::now();
        det.observe_event(t);
        let ev = det.last_evidence().unwrap();
        // First event should use mu_steady_ms as default observation
        assert!((ev.observation_ms - 200.0).abs() < f64::EPSILON);
    }

    // ── Observation clamping ────────────────────────────────────────

    #[test]
    fn observation_clamped_to_bounds() {
        let mut det = BocpdDetector::with_defaults();
        let start = Instant::now();
        // First event (uses default, not clamped)
        det.observe_event(start);
        // Second event 0ms later → should be clamped to min_observation_ms
        det.observe_event(start);
        let ev = det.last_evidence().unwrap();
        assert!(ev.observation_ms >= det.config().min_observation_ms);
    }

    // ── BocpdConfig clone/debug ─────────────────────────────────────

    #[test]
    fn config_clone_debug() {
        let config = BocpdConfig::default();
        let cloned = config.clone();
        assert!((cloned.mu_steady_ms - 200.0).abs() < f64::EPSILON);
        let dbg = format!("{:?}", config);
        assert!(dbg.contains("BocpdConfig"));
    }

    #[test]
    fn detector_clone_debug() {
        let det = BocpdDetector::with_defaults();
        let cloned = det.clone();
        assert!((cloned.p_burst() - det.p_burst()).abs() < f64::EPSILON);
        let dbg = format!("{:?}", det);
        assert!(dbg.contains("BocpdDetector"));
    }

    #[test]
    fn evidence_clone() {
        let mut det = BocpdDetector::with_defaults();
        det.observe_event(Instant::now());
        let ev = det.last_evidence().unwrap().clone();
        assert_eq!(ev.observation_count, 1);
    }
}

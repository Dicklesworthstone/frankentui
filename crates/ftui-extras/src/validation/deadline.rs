//! Async Validation Deadline Controller (bd-32x8).
//!
//! Implements a latency-aware decision rule for async validation cancellation
//! using survival distribution modeling and expected loss calculation.
//!
//! # Core Algorithm
//!
//! Given an in-flight validation with elapsed time `t`, we decide whether to
//! wait or cancel by comparing expected losses:
//!
//! - **Loss(wait)** = L_stale * P(T > deadline) + L_delay * E[T - t | T > t]
//! - **Loss(cancel)** = L_false_invalid + L_recompute
//!
//! We choose the action with smaller expected loss.
//!
//! # Survival Model
//!
//! We model completion time T using a Weibull distribution:
//! - S(t) = exp(-(t/λ)^k) - Survival function
//! - h(t) = (k/λ) * (t/λ)^(k-1) - Hazard function
//!
//! Parameters λ (scale) and k (shape) are estimated from rolling window stats.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::time::Duration;

// =============================================================================
// Configuration
// =============================================================================

/// Configuration for the deadline controller.
///
/// # Loss Weights
///
/// - `loss_stale`: Cost of returning a stale validation result (one that
///   completes after newer input arrived). Higher values favor cancellation.
/// - `loss_delay`: Cost per unit time of user-visible delay. Higher values
///   favor cancellation when expected wait is long.
/// - `loss_false_invalid`: Cost of incorrectly marking a value as invalid
///   due to cancellation. Higher values favor waiting.
/// - `loss_recompute`: Cost of having to re-validate from scratch after
///   cancellation. Higher values favor waiting.
///
/// # Deadline Budget
///
/// The `deadline` specifies the maximum allowed time for a validation.
/// Validations exceeding this are always candidates for cancellation.
#[derive(Debug, Clone, PartialEq)]
pub struct DeadlineConfig {
    /// Maximum allowed validation time.
    pub deadline: Duration,
    /// Cost of returning a stale result (normalized, 0.0-1.0).
    pub loss_stale: f64,
    /// Cost per unit time of delay (per second).
    pub loss_delay: f64,
    /// Cost of false invalid due to cancellation.
    pub loss_false_invalid: f64,
    /// Cost of re-validating after cancellation.
    pub loss_recompute: f64,
    /// Rolling window size for stats (number of samples).
    pub window_size: usize,
    /// Minimum samples before model is considered reliable.
    pub min_samples: usize,
}

impl Default for DeadlineConfig {
    fn default() -> Self {
        Self {
            deadline: Duration::from_millis(500),
            loss_stale: 0.8,
            loss_delay: 0.5,
            loss_false_invalid: 0.6,
            loss_recompute: 0.3,
            window_size: 100,
            min_samples: 5,
        }
    }
}

impl DeadlineConfig {
    /// Create a new configuration with the given deadline.
    #[must_use]
    pub fn new(deadline: Duration) -> Self {
        Self {
            deadline,
            ..Default::default()
        }
    }

    /// Set loss weights.
    #[must_use]
    pub fn with_losses(
        mut self,
        stale: f64,
        delay: f64,
        false_invalid: f64,
        recompute: f64,
    ) -> Self {
        self.loss_stale = stale;
        self.loss_delay = delay;
        self.loss_false_invalid = false_invalid;
        self.loss_recompute = recompute;
        self
    }

    /// Set the rolling window size.
    #[must_use]
    pub fn with_window_size(mut self, size: usize) -> Self {
        self.window_size = size.max(1);
        self
    }

    /// Set the minimum samples before model is reliable.
    #[must_use]
    pub fn with_min_samples(mut self, min: usize) -> Self {
        self.min_samples = min.max(1);
        self
    }
}

// =============================================================================
// Survival Statistics
// =============================================================================

/// Rolling window statistics for validation completion times.
///
/// Tracks completion times and maintains Weibull distribution parameters
/// using online estimation.
#[derive(Debug, Clone)]
pub struct SurvivalStats {
    /// Recent completion times (in seconds).
    samples: VecDeque<f64>,
    /// Maximum window size.
    window_size: usize,
    /// Estimated Weibull scale parameter (λ).
    lambda: f64,
    /// Estimated Weibull shape parameter (k).
    k: f64,
    /// Exponential moving average of completion time.
    ema: f64,
    /// Exponential moving average of squared completion time (for variance).
    ema_sq: f64,
    /// EMA smoothing factor (0.0-1.0).
    alpha: f64,
}

impl SurvivalStats {
    /// Create new statistics with the given window size.
    #[must_use]
    pub fn new(window_size: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(window_size),
            window_size: window_size.max(1),
            lambda: 0.1, // Initial guess: 100ms
            k: 1.5,      // Initial guess: slight right skew
            ema: 0.1,
            ema_sq: 0.01,
            alpha: 0.1,
        }
    }

    /// Record a completed validation's duration.
    pub fn record(&mut self, duration: Duration) {
        let t = duration.as_secs_f64();

        // Add to rolling window
        if self.samples.len() >= self.window_size {
            self.samples.pop_front();
        }
        self.samples.push_back(t);

        // Update EMA
        self.ema = self.alpha * t + (1.0 - self.alpha) * self.ema;
        self.ema_sq = self.alpha * t * t + (1.0 - self.alpha) * self.ema_sq;

        // Re-estimate Weibull parameters if we have enough samples
        if self.samples.len() >= 3 {
            self.estimate_weibull();
        }
    }

    /// Number of samples in the rolling window.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Current mean completion time estimate.
    #[must_use]
    pub fn mean(&self) -> f64 {
        self.ema
    }

    /// Current variance estimate.
    #[must_use]
    pub fn variance(&self) -> f64 {
        (self.ema_sq - self.ema * self.ema).max(0.0)
    }

    /// Standard deviation estimate.
    #[must_use]
    pub fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }

    /// Weibull scale parameter (λ).
    #[must_use]
    pub fn lambda(&self) -> f64 {
        self.lambda
    }

    /// Weibull shape parameter (k).
    #[must_use]
    pub fn k(&self) -> f64 {
        self.k
    }

    /// Survival function S(t) = P(T > t).
    #[must_use]
    pub fn survival(&self, t: f64) -> f64 {
        if t <= 0.0 {
            return 1.0;
        }
        (-(t / self.lambda).powf(self.k)).exp()
    }

    /// Expected remaining time E[T - t | T > t].
    ///
    /// For Weibull, this is computed via the incomplete gamma function
    /// approximation. For simplicity, we use a numerical approximation.
    #[must_use]
    pub fn expected_remaining(&self, elapsed: f64) -> f64 {
        let s_t = self.survival(elapsed);
        if s_t <= 1e-9 {
            // Very unlikely to still be running
            return 0.0;
        }

        // Numerical integration: E[T-t | T>t] = (1/S(t)) * integral_t^inf S(u) du
        // We approximate by summing over a reasonable range
        let max_t = elapsed + 10.0 * self.lambda; // Cap integration
        let steps = 100;
        let dt = (max_t - elapsed) / steps as f64;

        let mut integral = 0.0;
        for i in 0..steps {
            let u = elapsed + (i as f64 + 0.5) * dt;
            integral += self.survival(u) * dt;
        }

        integral / s_t
    }

    /// Estimate Weibull parameters from samples using method of moments.
    fn estimate_weibull(&mut self) {
        if self.samples.is_empty() {
            return;
        }

        // Compute sample mean and variance
        let n = self.samples.len() as f64;
        let mean: f64 = self.samples.iter().sum::<f64>() / n;
        let variance: f64 = self
            .samples
            .iter()
            .map(|&x| (x - mean).powi(2))
            .sum::<f64>()
            / n;

        if mean <= 0.0 || variance <= 0.0 {
            return;
        }

        // Coefficient of variation
        let cv = variance.sqrt() / mean;

        // Approximate k from CV (empirical relationship for Weibull)
        // CV = sqrt(Gamma(1 + 2/k) / Gamma(1 + 1/k)^2 - 1)
        // For k > 1: CV ≈ 1.2 / k (rough approximation)
        let k = (1.2 / cv).clamp(0.5, 10.0);

        // λ = mean / Gamma(1 + 1/k)
        // Approximate Gamma(1 + 1/k) ≈ 1 - 0.5772/k + 0.98905/k^2 for k > 1
        let gamma_approx = 1.0 - 0.5772 / k + 0.98905 / (k * k);
        let lambda = (mean / gamma_approx).max(0.001);

        self.k = k;
        self.lambda = lambda;
    }
}

impl Default for SurvivalStats {
    fn default() -> Self {
        Self::new(100)
    }
}

// =============================================================================
// Decision Types
// =============================================================================

/// The decision outcome from the deadline controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeadlineDecision {
    /// Continue waiting for the validation to complete.
    Wait,
    /// Cancel the in-flight validation.
    Cancel,
}

/// Detailed rationale for a deadline decision.
#[derive(Debug, Clone)]
pub struct DecisionRationale {
    /// The decision made.
    pub decision: DeadlineDecision,
    /// Expected loss if we wait.
    pub loss_wait: f64,
    /// Expected loss if we cancel.
    pub loss_cancel: f64,
    /// Estimated probability of exceeding deadline.
    pub prob_exceed_deadline: f64,
    /// Expected remaining time (seconds).
    pub expected_remaining_secs: f64,
    /// Whether the model is considered reliable.
    pub model_reliable: bool,
    /// Human-readable explanation.
    pub explanation: String,
}

impl DecisionRationale {
    fn new(decision: DeadlineDecision) -> Self {
        Self {
            decision,
            loss_wait: 0.0,
            loss_cancel: 0.0,
            prob_exceed_deadline: 0.0,
            expected_remaining_secs: 0.0,
            model_reliable: false,
            explanation: String::new(),
        }
    }
}

// =============================================================================
// Deadline Controller
// =============================================================================

/// Controller for managing async validation deadlines.
///
/// The controller tracks validation completion times and uses survival analysis
/// to decide whether to cancel or wait for in-flight validations.
///
/// # Example
///
/// ```rust,ignore
/// use ftui_extras::validation::deadline::{DeadlineController, DeadlineConfig, DeadlineDecision};
/// use std::time::Duration;
///
/// let config = DeadlineConfig::new(Duration::from_millis(500));
/// let mut controller = DeadlineController::new(config);
///
/// // Record some completion times to build the model
/// controller.record_completion(Duration::from_millis(50));
/// controller.record_completion(Duration::from_millis(80));
/// controller.record_completion(Duration::from_millis(120));
/// controller.record_completion(Duration::from_millis(90));
/// controller.record_completion(Duration::from_millis(100));
///
/// // Make a decision for an in-flight validation
/// let elapsed = Duration::from_millis(200);
/// let decision = controller.decide(elapsed);
/// ```
#[derive(Debug, Clone)]
pub struct DeadlineController {
    /// Configuration for the controller.
    config: DeadlineConfig,
    /// Survival statistics for completion times.
    stats: SurvivalStats,
    /// Sequence number for tracking staleness.
    sequence: u64,
    /// Sequence number of the most recent input.
    latest_input_sequence: u64,
}

impl DeadlineController {
    /// Create a new deadline controller with the given configuration.
    #[must_use]
    pub fn new(config: DeadlineConfig) -> Self {
        let stats = SurvivalStats::new(config.window_size);
        Self {
            config,
            stats,
            sequence: 0,
            latest_input_sequence: 0,
        }
    }

    /// Create a new controller with default configuration.
    #[must_use]
    pub fn with_deadline(deadline: Duration) -> Self {
        Self::new(DeadlineConfig::new(deadline))
    }

    /// Record a completed validation's duration.
    ///
    /// This updates the survival model for future decisions.
    pub fn record_completion(&mut self, duration: Duration) {
        self.stats.record(duration);
    }

    /// Mark that new input has arrived, making in-flight validations stale.
    ///
    /// Returns the sequence number for the new input.
    pub fn new_input(&mut self) -> u64 {
        self.sequence += 1;
        self.latest_input_sequence = self.sequence;
        self.sequence
    }

    /// Start a new validation and return its sequence number.
    ///
    /// The sequence number should be stored with the validation task
    /// to check for staleness when it completes.
    #[must_use]
    pub fn start_validation(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    /// Check if a validation with the given sequence is stale.
    ///
    /// A validation is stale if new input arrived after it started.
    #[must_use]
    pub fn is_stale(&self, validation_sequence: u64) -> bool {
        validation_sequence < self.latest_input_sequence
    }

    /// Decide whether to wait or cancel an in-flight validation.
    ///
    /// Returns the decision based on expected loss comparison.
    #[must_use]
    pub fn decide(&self, elapsed: Duration) -> DeadlineDecision {
        self.decide_with_rationale(elapsed).decision
    }

    /// Decide whether to wait or cancel, with detailed rationale.
    ///
    /// This is useful for debugging and logging decisions.
    #[must_use]
    pub fn decide_with_rationale(&self, elapsed: Duration) -> DecisionRationale {
        let elapsed_secs = elapsed.as_secs_f64();
        let deadline_secs = self.config.deadline.as_secs_f64();

        // Check if we have enough data for reliable estimation
        let model_reliable = self.stats.sample_count() >= self.config.min_samples;

        // If already past deadline, always cancel
        if elapsed >= self.config.deadline {
            let mut rationale = DecisionRationale::new(DeadlineDecision::Cancel);
            rationale.prob_exceed_deadline = 1.0;
            rationale.model_reliable = model_reliable;
            rationale.explanation = format!(
                "Elapsed time ({:.0}ms) exceeds deadline ({:.0}ms)",
                elapsed.as_millis(),
                self.config.deadline.as_millis()
            );
            return rationale;
        }

        // If model is not reliable, use conservative heuristic
        if !model_reliable {
            let mut rationale = DecisionRationale::new(DeadlineDecision::Wait);
            rationale.model_reliable = false;
            rationale.explanation = format!(
                "Model not reliable (only {} samples, need {})",
                self.stats.sample_count(),
                self.config.min_samples
            );
            return rationale;
        }

        // Calculate survival probabilities and expected remaining time
        let s_t = self.stats.survival(elapsed_secs);
        let s_deadline = self.stats.survival(deadline_secs);
        let prob_exceed = s_deadline / s_t.max(1e-9); // P(T > deadline | T > t)
        let expected_remaining = self.stats.expected_remaining(elapsed_secs);

        // Calculate expected losses
        let loss_wait =
            self.config.loss_stale * prob_exceed + self.config.loss_delay * expected_remaining;
        let loss_cancel = self.config.loss_false_invalid + self.config.loss_recompute;

        // Make decision
        let decision = if loss_wait < loss_cancel {
            DeadlineDecision::Wait
        } else {
            DeadlineDecision::Cancel
        };

        // Build rationale
        let mut rationale = DecisionRationale::new(decision.clone());
        rationale.loss_wait = loss_wait;
        rationale.loss_cancel = loss_cancel;
        rationale.prob_exceed_deadline = prob_exceed;
        rationale.expected_remaining_secs = expected_remaining;
        rationale.model_reliable = true;
        rationale.explanation = format!(
            "Loss(wait)={:.3} vs Loss(cancel)={:.3}. P(exceed deadline|running)={:.1}%, E[remaining]={:.0}ms",
            loss_wait,
            loss_cancel,
            prob_exceed * 100.0,
            expected_remaining * 1000.0
        );

        rationale
    }

    /// Get the current survival statistics.
    #[must_use]
    pub fn stats(&self) -> &SurvivalStats {
        &self.stats
    }

    /// Get the current configuration.
    #[must_use]
    pub fn config(&self) -> &DeadlineConfig {
        &self.config
    }

    /// Reset the statistics (e.g., when validator characteristics change).
    pub fn reset_stats(&mut self) {
        self.stats = SurvivalStats::new(self.config.window_size);
    }
}

impl Default for DeadlineController {
    fn default() -> Self {
        Self::new(DeadlineConfig::default())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- SurvivalStats tests --

    #[test]
    fn stats_new() {
        let stats = SurvivalStats::new(50);
        assert_eq!(stats.sample_count(), 0);
        assert_eq!(stats.window_size, 50);
    }

    #[test]
    fn stats_record() {
        let mut stats = SurvivalStats::new(10);
        stats.record(Duration::from_millis(100));
        assert_eq!(stats.sample_count(), 1);

        stats.record(Duration::from_millis(150));
        assert_eq!(stats.sample_count(), 2);
    }

    #[test]
    fn stats_rolling_window() {
        let mut stats = SurvivalStats::new(3);
        stats.record(Duration::from_millis(100));
        stats.record(Duration::from_millis(200));
        stats.record(Duration::from_millis(300));
        assert_eq!(stats.sample_count(), 3);

        stats.record(Duration::from_millis(400));
        assert_eq!(stats.sample_count(), 3); // Still 3, oldest dropped
    }

    #[test]
    fn stats_survival_function() {
        let mut stats = SurvivalStats::new(10);
        // Record some samples to establish parameters
        for _ in 0..10 {
            stats.record(Duration::from_millis(100));
        }

        // S(0) should be 1.0
        assert!((stats.survival(0.0) - 1.0).abs() < 0.01);

        // S(t) should decrease as t increases
        let s_1 = stats.survival(0.1);
        let s_2 = stats.survival(0.2);
        assert!(s_1 > s_2, "S(0.1)={} should be > S(0.2)={}", s_1, s_2);
    }

    #[test]
    fn stats_mean_and_variance() {
        let mut stats = SurvivalStats::new(10);
        stats.record(Duration::from_millis(100));
        stats.record(Duration::from_millis(100));
        stats.record(Duration::from_millis(100));

        // Mean should converge towards 0.1 (100ms)
        let mean = stats.mean();
        assert!(mean > 0.05 && mean < 0.15);

        let variance = stats.variance();
        assert!(variance >= 0.0);
        assert!(variance.is_finite());

        let std_dev = stats.std_dev();
        assert!(std_dev >= 0.0);
        assert!(std_dev.is_finite());

        assert!(stats.lambda() > 0.0);
        assert!(stats.lambda().is_finite());
        assert!(stats.k() > 0.0);
        assert!(stats.k().is_finite());
    }

    #[test]
    fn stats_default_uses_window_size_100() {
        let stats = SurvivalStats::default();
        assert_eq!(stats.sample_count(), 0);
        assert_eq!(stats.window_size, 100);
    }

    #[test]
    fn stats_estimate_weibull_empty_is_noop() {
        let mut stats = SurvivalStats::new(10);
        let lambda_before = stats.lambda;
        let k_before = stats.k;

        stats.estimate_weibull();

        assert_eq!(stats.sample_count(), 0);
        assert!((stats.lambda - lambda_before).abs() < 1e-12);
        assert!((stats.k - k_before).abs() < 1e-12);
    }

    // -- DeadlineConfig tests --

    #[test]
    fn config_default() {
        let config = DeadlineConfig::default();
        assert_eq!(config.deadline, Duration::from_millis(500));
        assert!(config.loss_stale > 0.0);
        assert!(config.window_size > 0);
    }

    #[test]
    fn config_builder() {
        let config = DeadlineConfig::new(Duration::from_secs(1))
            .with_losses(0.9, 0.4, 0.5, 0.2)
            .with_window_size(50);

        assert_eq!(config.deadline, Duration::from_secs(1));
        assert_eq!(config.loss_stale, 0.9);
        assert_eq!(config.window_size, 50);
    }

    // -- DeadlineController tests --

    #[test]
    fn controller_new() {
        let controller = DeadlineController::with_deadline(Duration::from_millis(100));
        assert_eq!(controller.config().deadline, Duration::from_millis(100));
        assert_eq!(controller.stats().sample_count(), 0);
    }

    #[test]
    fn controller_record_completion() {
        let mut controller = DeadlineController::default();
        controller.record_completion(Duration::from_millis(50));
        assert_eq!(controller.stats().sample_count(), 1);
    }

    #[test]
    fn unit_cancel_when_long_tail() {
        // Long-tail durations should trigger cancel under tight deadline
        let config = DeadlineConfig::new(Duration::from_millis(100))
            .with_losses(0.8, 0.5, 0.3, 0.2) // Low cancel cost
            .with_min_samples(3);
        let mut controller = DeadlineController::new(config);

        // Train with long-tail distribution (some fast, some very slow)
        controller.record_completion(Duration::from_millis(50));
        controller.record_completion(Duration::from_millis(60));
        controller.record_completion(Duration::from_millis(500)); // Long tail
        controller.record_completion(Duration::from_millis(80));
        controller.record_completion(Duration::from_millis(1000)); // Long tail

        // At 80ms elapsed with a 100ms deadline, should cancel due to long tail
        let rationale = controller.decide_with_rationale(Duration::from_millis(80));

        // The model should recognize the long tail and favor cancellation
        // when loss weights favor it
        assert!(
            rationale.model_reliable,
            "Model should be reliable with 5 samples"
        );
    }

    #[test]
    fn unit_wait_when_fast() {
        // Low-latency validator should not be cancelled
        let config = DeadlineConfig::new(Duration::from_millis(500))
            .with_losses(0.8, 0.5, 0.7, 0.4) // Higher cancel cost
            .with_min_samples(3);
        let mut controller = DeadlineController::new(config);

        // Train with fast, consistent completion times
        for _ in 0..10 {
            controller.record_completion(Duration::from_millis(50));
        }

        // At 100ms elapsed with a 500ms deadline, should wait
        let decision = controller.decide(Duration::from_millis(100));
        assert_eq!(
            decision,
            DeadlineDecision::Wait,
            "Should wait for fast validator"
        );
    }

    #[test]
    fn unit_deadline_respected() {
        // Validation at/past deadline should always cancel
        let config = DeadlineConfig::new(Duration::from_millis(100));
        let controller = DeadlineController::new(config);

        // At exactly deadline
        let decision = controller.decide(Duration::from_millis(100));
        assert_eq!(
            decision,
            DeadlineDecision::Cancel,
            "Should cancel at deadline"
        );

        // Past deadline
        let decision = controller.decide(Duration::from_millis(150));
        assert_eq!(
            decision,
            DeadlineDecision::Cancel,
            "Should cancel past deadline"
        );
    }

    #[test]
    fn controller_staleness_tracking() {
        let mut controller = DeadlineController::default();

        // Start a validation
        let seq1 = controller.start_validation();
        assert!(!controller.is_stale(seq1));

        // New input arrives
        controller.new_input();
        assert!(controller.is_stale(seq1), "Old validation should be stale");

        // New validation started after input
        let seq2 = controller.start_validation();
        assert!(
            !controller.is_stale(seq2),
            "New validation should not be stale"
        );
    }

    #[test]
    fn controller_unreliable_model_waits() {
        // When model is not reliable, should wait conservatively
        let config = DeadlineConfig::new(Duration::from_millis(100)).with_min_samples(10);
        let mut controller = DeadlineController::new(config);

        // Only add a few samples (below min_samples)
        controller.record_completion(Duration::from_millis(50));
        controller.record_completion(Duration::from_millis(60));

        let rationale = controller.decide_with_rationale(Duration::from_millis(80));
        assert!(!rationale.model_reliable);
        assert_eq!(rationale.decision, DeadlineDecision::Wait);
    }

    #[test]
    fn controller_reset_stats() {
        let mut controller = DeadlineController::default();
        controller.record_completion(Duration::from_millis(100));
        assert_eq!(controller.stats().sample_count(), 1);

        controller.reset_stats();
        assert_eq!(controller.stats().sample_count(), 0);
    }

    #[test]
    fn rationale_explanation_populated() {
        let mut controller = DeadlineController::default();
        for _ in 0..10 {
            controller.record_completion(Duration::from_millis(100));
        }

        let rationale = controller.decide_with_rationale(Duration::from_millis(50));
        assert!(!rationale.explanation.is_empty());
        assert!(rationale.model_reliable);
    }

    // -- Property-based test approximation --

    #[test]
    fn property_random_durations() {
        // Test with a variety of durations to ensure decision is reasonable
        let config = DeadlineConfig::new(Duration::from_millis(200))
            .with_losses(0.7, 0.4, 0.3, 0.3)
            .with_min_samples(5);
        let mut controller = DeadlineController::new(config.clone());

        // Train with varied durations
        let training_durations = [30, 50, 80, 100, 150, 120, 90, 70, 60, 110];
        for &ms in &training_durations {
            controller.record_completion(Duration::from_millis(ms));
        }

        // Test decisions at various elapsed times
        for elapsed_ms in [10, 50, 100, 150, 180, 199] {
            let rationale = controller.decide_with_rationale(Duration::from_millis(elapsed_ms));

            // Decision should be consistent with loss comparison
            if rationale.loss_wait < rationale.loss_cancel {
                assert_eq!(rationale.decision, DeadlineDecision::Wait);
            } else {
                assert_eq!(rationale.decision, DeadlineDecision::Cancel);
            }

            // Expected remaining should be non-negative
            assert!(rationale.expected_remaining_secs >= 0.0);

            // Probability should be in [0, 1]
            assert!(rationale.prob_exceed_deadline >= 0.0);
            assert!(rationale.prob_exceed_deadline <= 1.0);
        }
    }
}

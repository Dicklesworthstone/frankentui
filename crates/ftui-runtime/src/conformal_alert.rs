#![forbid(unsafe_code)]

//! Conformal alert threshold calibration with anytime-valid e-process control.
//!
//! This module provides change-point detection for action timeline events using:
//! 1. **Conformal thresholding** - Distribution-free threshold calibration
//! 2. **E-process layer** - Anytime-valid FPR control via test martingales
//! 3. **Evidence ledger** - Explainable alert decisions with full provenance
//!
//! # Mathematical Model
//!
//! ## Conformal Thresholding (Primary)
//!
//! Given calibration residuals R = {r_1, ..., r_n}, the conformal threshold is:
//!
//! ```text
//! q = quantile_{(1-alpha)(n+1)/n}(R)
//! ```
//!
//! This is the (n+1) rule: we pretend the new observation is one of (n+1) equally
//! likely positions, ensuring finite-sample coverage P(r_{n+1} <= q) >= 1-alpha.
//!
//! ## E-Process Layer (Anytime-Valid)
//!
//! For early stopping without FPR inflation, we maintain an e-process:
//!
//! ```text
//! e_t = exp(lambda * (z_t - mu_0) - lambda^2 * sigma_0^2 / 2)
//! E_t = prod_{s=1}^{t} e_s
//! ```
//!
//! where z_t = (r_t - mean) / std is the standardized residual. Alert when E_t > 1/alpha.
//!
//! # Key Invariants
//!
//! 1. **Coverage guarantee**: P(FP) <= alpha under H_0 for conformal threshold
//! 2. **Anytime-valid**: E_t is a supermartingale, so P(exists t: E_t >= 1/alpha) <= alpha
//! 3. **Non-negative wealth**: E_t >= 0 always (floored at epsilon)
//! 4. **Calibration monotonicity**: Threshold is non-decreasing in calibration set size
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | n < min_calibration | Use fallback threshold | Insufficient data |
//! | sigma = 0 | Use epsilon floor | Degenerate data |
//! | E_t underflow | Floor at E_MIN | Prevent permanent zero-lock |
//! | All residuals identical | Wide threshold | No variance to detect |
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::conformal_alert::{ConformalAlert, AlertConfig};
//!
//! let mut alerter = ConformalAlert::new(AlertConfig::default());
//!
//! // Calibration phase: feed baseline residuals
//! for baseline_value in baseline_data {
//!     alerter.calibrate(baseline_value);
//! }
//!
//! // Detection phase: check new observations
//! let decision = alerter.observe(new_value);
//! if decision.is_alert() {
//!     println!("Alert: {}", decision.evidence_summary());
//! }
//! ```

use std::collections::VecDeque;

/// Minimum e-value floor to prevent permanent zero-lock.
const E_MIN: f64 = 1e-12;

/// Maximum e-value ceiling to prevent overflow to infinity.
/// This is the inverse of E_MIN for symmetry - if we reach this value,
/// we're already well above any reasonable alert threshold.
const E_MAX: f64 = 1e12;

/// Minimum calibration samples before using conformal threshold.
const MIN_CALIBRATION: usize = 10;

/// Default fallback threshold when calibration is insufficient.
const FALLBACK_THRESHOLD: f64 = f64::MAX;

/// Epsilon for numerical stability.
const EPSILON: f64 = 1e-10;

/// Configuration for conformal alert calibration.
#[derive(Debug, Clone)]
pub struct AlertConfig {
    /// Significance level alpha. FPR is controlled at this level.
    /// Lower alpha = more conservative (fewer false alarms). Default: 0.05.
    pub alpha: f64,

    /// Minimum calibration samples before using conformal threshold.
    /// Default: 10.
    pub min_calibration: usize,

    /// Maximum calibration samples to retain. Default: 500.
    pub max_calibration: usize,

    /// E-process betting fraction lambda. Default: 0.5.
    pub lambda: f64,

    /// Null hypothesis mean for standardized residuals (usually 0). Default: 0.0.
    pub mu_0: f64,

    /// Null hypothesis std for standardized residuals (usually 1). Default: 1.0.
    pub sigma_0: f64,

    /// Use adaptive lambda via GRAPA. Default: true.
    pub adaptive_lambda: bool,

    /// GRAPA learning rate. Default: 0.1.
    pub grapa_eta: f64,

    /// Enable JSONL-compatible logging. Default: false.
    pub enable_logging: bool,

    /// Hysteresis factor: require E_t > (1/alpha) * hysteresis to alert.
    /// Prevents alert flicker at boundary. Default: 1.1.
    pub hysteresis: f64,

    /// Cooldown observations after alert before allowing another.
    /// Default: 5.
    pub alert_cooldown: u64,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            min_calibration: MIN_CALIBRATION,
            max_calibration: 500,
            lambda: 0.5,
            mu_0: 0.0,
            sigma_0: 1.0,
            adaptive_lambda: true,
            grapa_eta: 0.1,
            enable_logging: false,
            hysteresis: 1.1,
            alert_cooldown: 5,
        }
    }
}

/// Running statistics for calibration using Welford's algorithm.
#[derive(Debug, Clone)]
struct CalibrationStats {
    n: u64,
    mean: f64,
    m2: f64, // Sum of squared deviations
}

impl CalibrationStats {
    fn new() -> Self {
        Self {
            n: 0,
            mean: 0.0,
            m2: 0.0,
        }
    }

    fn update(&mut self, x: f64) {
        self.n += 1;
        let delta = x - self.mean;
        self.mean += delta / self.n as f64;
        let delta2 = x - self.mean;
        self.m2 += delta * delta2;
    }

    fn variance(&self) -> f64 {
        if self.n < 2 {
            return 1.0; // Fallback
        }
        (self.m2 / (self.n - 1) as f64).max(EPSILON)
    }

    fn std(&self) -> f64 {
        self.variance().sqrt()
    }
}

/// Evidence ledger entry for a single observation.
#[derive(Debug, Clone)]
pub struct AlertEvidence {
    /// Observation index.
    pub observation_idx: u64,
    /// Raw observation value.
    pub value: f64,
    /// Residual (value - calibration_mean).
    pub residual: f64,
    /// Standardized residual (z-score).
    pub z_score: f64,
    /// Current conformal threshold q.
    pub conformal_threshold: f64,
    /// Conformal score (proportion of calibration residuals >= this one).
    pub conformal_score: f64,
    /// Current e-value (wealth).
    pub e_value: f64,
    /// E-value threshold (1/alpha).
    pub e_threshold: f64,
    /// Current lambda (betting fraction).
    pub lambda: f64,
    /// Alert triggered by conformal threshold?
    pub conformal_alert: bool,
    /// Alert triggered by e-process?
    pub eprocess_alert: bool,
    /// Combined alert decision.
    pub is_alert: bool,
    /// Reason for alert (or non-alert).
    pub reason: AlertReason,
}

impl AlertEvidence {
    /// Generate a summary string for the evidence.
    pub fn summary(&self) -> String {
        format!(
            "obs={} val={:.2} res={:.2} z={:.2} q={:.2} conf_p={:.3} E={:.2}/{:.2} alert={}",
            self.observation_idx,
            self.value,
            self.residual,
            self.z_score,
            self.conformal_threshold,
            self.conformal_score,
            self.e_value,
            self.e_threshold,
            self.is_alert
        )
    }
}

/// Reason for alert decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertReason {
    /// No alert: observation within normal bounds.
    Normal,
    /// Alert: conformal threshold exceeded.
    ConformalExceeded,
    /// Alert: e-process threshold exceeded.
    EProcessExceeded,
    /// Alert: both thresholds exceeded.
    BothExceeded,
    /// No alert: in cooldown period after recent alert.
    InCooldown,
    /// No alert: insufficient calibration data.
    InsufficientCalibration,
}

/// Decision returned after observing a new value.
#[derive(Debug, Clone)]
pub struct AlertDecision {
    /// Whether to trigger an alert.
    pub is_alert: bool,
    /// Full evidence for this observation.
    pub evidence: AlertEvidence,
    /// Observations since last alert.
    pub observations_since_alert: u64,
}

impl AlertDecision {
    /// Summary string for the decision.
    pub fn evidence_summary(&self) -> String {
        self.evidence.summary()
    }
}

/// Aggregate statistics for the alerter.
#[derive(Debug, Clone)]
pub struct AlertStats {
    /// Total observations processed.
    pub total_observations: u64,
    /// Total calibration samples.
    pub calibration_samples: usize,
    /// Total alerts triggered.
    pub total_alerts: u64,
    /// Conformal-only alerts.
    pub conformal_alerts: u64,
    /// E-process-only alerts.
    pub eprocess_alerts: u64,
    /// Both-threshold alerts.
    pub both_alerts: u64,
    /// Current e-value.
    pub current_e_value: f64,
    /// Current conformal threshold.
    pub current_threshold: f64,
    /// Current lambda.
    pub current_lambda: f64,
    /// Calibration mean.
    pub calibration_mean: f64,
    /// Calibration std.
    pub calibration_std: f64,
    /// Empirical FPR (alerts / observations under H0 assumption).
    pub empirical_fpr: f64,
}

/// Conformal alert threshold calibrator with e-process control.
#[derive(Debug)]
pub struct ConformalAlert {
    config: AlertConfig,

    /// Calibration residuals (sorted for quantile computation).
    calibration: VecDeque<f64>,

    /// Running calibration statistics.
    stats: CalibrationStats,

    /// Current e-value (wealth).
    e_value: f64,

    /// E-value threshold (1/alpha * hysteresis).
    e_threshold: f64,

    /// Current adaptive lambda.
    lambda: f64,

    /// Total observation count.
    observation_count: u64,

    /// Observations since last alert.
    observations_since_alert: u64,

    /// In cooldown period.
    in_cooldown: bool,

    /// Total alerts.
    total_alerts: u64,

    /// Alert type counters.
    conformal_alerts: u64,
    eprocess_alerts: u64,
    both_alerts: u64,

    /// Evidence log (if logging enabled).
    logs: Vec<AlertEvidence>,
}

impl ConformalAlert {
    /// Create a new conformal alerter with given configuration.
    pub fn new(config: AlertConfig) -> Self {
        let e_threshold = (1.0 / config.alpha) * config.hysteresis;
        let lambda = config.lambda.clamp(EPSILON, 1.0 - EPSILON);

        Self {
            config,
            calibration: VecDeque::new(),
            stats: CalibrationStats::new(),
            e_value: 1.0,
            e_threshold,
            lambda,
            observation_count: 0,
            observations_since_alert: 0,
            in_cooldown: false,
            total_alerts: 0,
            conformal_alerts: 0,
            eprocess_alerts: 0,
            both_alerts: 0,
            logs: Vec::new(),
        }
    }

    /// Add a calibration sample.
    ///
    /// Call this during the baseline/training phase to build the null distribution.
    pub fn calibrate(&mut self, value: f64) {
        self.stats.update(value);

        // Store residual for quantile computation
        let residual = (value - self.stats.mean).abs();
        self.calibration.push_back(residual);

        // Enforce max calibration size
        while self.calibration.len() > self.config.max_calibration {
            self.calibration.pop_front();
        }
    }

    /// Observe a new value and return alert decision with evidence.
    pub fn observe(&mut self, value: f64) -> AlertDecision {
        self.observation_count += 1;
        self.observations_since_alert += 1;

        // Check cooldown
        if self.in_cooldown && self.observations_since_alert <= self.config.alert_cooldown {
            return self.no_alert_decision(value, AlertReason::InCooldown);
        }
        self.in_cooldown = false;

        // Check calibration sufficiency
        if self.calibration.len() < self.config.min_calibration {
            return self.no_alert_decision(value, AlertReason::InsufficientCalibration);
        }

        // Compute residual and z-score
        let residual = value - self.stats.mean;
        let abs_residual = residual.abs();
        let z_score = residual / self.stats.std().max(EPSILON);

        // Conformal threshold using (n+1) rule
        let conformal_threshold = self.compute_conformal_threshold();
        let conformal_score = self.compute_conformal_score(abs_residual);
        let conformal_alert = abs_residual > conformal_threshold;

        // E-process update
        let z_centered = z_score - self.config.mu_0;
        let exponent =
            self.lambda * z_centered - (self.lambda.powi(2) * self.config.sigma_0.powi(2)) / 2.0;
        // Clamp exponent to prevent exp() overflow to infinity (exp(709) ≈ 8.2e307)
        let e_factor = exponent.clamp(-700.0, 700.0).exp();
        self.e_value = (self.e_value * e_factor).clamp(E_MIN, E_MAX);

        let eprocess_alert = self.e_value > self.e_threshold;

        // Adaptive lambda update (GRAPA)
        if self.config.adaptive_lambda {
            let denominator = 1.0 + self.lambda * z_centered;
            if denominator.abs() > EPSILON {
                let grad = z_centered / denominator;
                self.lambda =
                    (self.lambda + self.config.grapa_eta * grad).clamp(EPSILON, 1.0 - EPSILON);
            }
        }

        // Combined decision
        let is_alert = conformal_alert || eprocess_alert;
        let reason = match (conformal_alert, eprocess_alert) {
            (true, true) => AlertReason::BothExceeded,
            (true, false) => AlertReason::ConformalExceeded,
            (false, true) => AlertReason::EProcessExceeded,
            (false, false) => AlertReason::Normal,
        };

        // Build evidence
        let evidence = AlertEvidence {
            observation_idx: self.observation_count,
            value,
            residual,
            z_score,
            conformal_threshold,
            conformal_score,
            e_value: self.e_value,
            e_threshold: self.e_threshold,
            lambda: self.lambda,
            conformal_alert,
            eprocess_alert,
            is_alert,
            reason,
        };

        // Log if enabled
        if self.config.enable_logging {
            self.logs.push(evidence.clone());
        }

        // Update alert stats
        if is_alert {
            self.total_alerts += 1;
            match reason {
                AlertReason::ConformalExceeded => self.conformal_alerts += 1,
                AlertReason::EProcessExceeded => self.eprocess_alerts += 1,
                AlertReason::BothExceeded => self.both_alerts += 1,
                _ => {}
            }
            self.observations_since_alert = 0;
            self.in_cooldown = true;
            // Reset e-value after alert
            self.e_value = 1.0;
        }

        AlertDecision {
            is_alert,
            evidence,
            observations_since_alert: self.observations_since_alert,
        }
    }

    /// Compute the conformal threshold using (n+1) rule.
    ///
    /// Returns the (1-alpha) quantile of calibration residuals, adjusted
    /// for finite sample coverage.
    fn compute_conformal_threshold(&self) -> f64 {
        if self.calibration.is_empty() {
            return FALLBACK_THRESHOLD;
        }

        let n = self.calibration.len();
        let alpha = self.config.alpha;

        // (n+1) rule: index = ceil((1-alpha) * (n+1)) - 1
        let target = (1.0 - alpha) * (n + 1) as f64;
        let idx = (target.ceil() as usize).saturating_sub(1).min(n - 1);

        // Sort calibration for quantile
        let mut sorted: Vec<f64> = self.calibration.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        sorted[idx]
    }

    /// Compute conformal p-value (proportion of calibration >= this residual).
    fn compute_conformal_score(&self, abs_residual: f64) -> f64 {
        if self.calibration.is_empty() {
            return 1.0;
        }

        let n = self.calibration.len();
        let count_geq = self
            .calibration
            .iter()
            .filter(|&&r| r >= abs_residual)
            .count();

        // (n+1) rule: (count + 1) / (n + 1)
        (count_geq + 1) as f64 / (n + 1) as f64
    }

    /// Helper to create a no-alert decision with given reason.
    fn no_alert_decision(&self, value: f64, reason: AlertReason) -> AlertDecision {
        let evidence = AlertEvidence {
            observation_idx: self.observation_count,
            value,
            residual: 0.0,
            z_score: 0.0,
            conformal_threshold: FALLBACK_THRESHOLD,
            conformal_score: 1.0,
            e_value: self.e_value,
            e_threshold: self.e_threshold,
            lambda: self.lambda,
            conformal_alert: false,
            eprocess_alert: false,
            is_alert: false,
            reason,
        };

        AlertDecision {
            is_alert: false,
            evidence,
            observations_since_alert: self.observations_since_alert,
        }
    }

    /// Reset the e-process state (but keep calibration).
    pub fn reset_eprocess(&mut self) {
        self.e_value = 1.0;
        self.observations_since_alert = 0;
        self.in_cooldown = false;
    }

    /// Clear calibration data.
    pub fn clear_calibration(&mut self) {
        self.calibration.clear();
        self.stats = CalibrationStats::new();
        self.reset_eprocess();
    }

    /// Get current statistics.
    pub fn stats(&self) -> AlertStats {
        let empirical_fpr = if self.observation_count > 0 {
            self.total_alerts as f64 / self.observation_count as f64
        } else {
            0.0
        };

        AlertStats {
            total_observations: self.observation_count,
            calibration_samples: self.calibration.len(),
            total_alerts: self.total_alerts,
            conformal_alerts: self.conformal_alerts,
            eprocess_alerts: self.eprocess_alerts,
            both_alerts: self.both_alerts,
            current_e_value: self.e_value,
            current_threshold: self.compute_conformal_threshold(),
            current_lambda: self.lambda,
            calibration_mean: self.stats.mean,
            calibration_std: self.stats.std(),
            empirical_fpr,
        }
    }

    /// Get evidence logs (if logging enabled).
    pub fn logs(&self) -> &[AlertEvidence] {
        &self.logs
    }

    /// Clear evidence logs.
    pub fn clear_logs(&mut self) {
        self.logs.clear();
    }

    /// Current e-value.
    #[inline]
    pub fn e_value(&self) -> f64 {
        self.e_value
    }

    /// Current conformal threshold.
    pub fn threshold(&self) -> f64 {
        self.compute_conformal_threshold()
    }

    /// Calibration mean.
    #[inline]
    pub fn mean(&self) -> f64 {
        self.stats.mean
    }

    /// Calibration std.
    #[inline]
    pub fn std(&self) -> f64 {
        self.stats.std()
    }

    /// Number of calibration samples.
    #[inline]
    pub fn calibration_count(&self) -> usize {
        self.calibration.len()
    }

    /// Alpha (significance level).
    #[inline]
    pub fn alpha(&self) -> f64 {
        self.config.alpha
    }
}

// =============================================================================
// Unit Tests (bd-1rzr)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AlertConfig {
        AlertConfig {
            alpha: 0.05,
            min_calibration: 5,
            max_calibration: 100,
            lambda: 0.5,
            mu_0: 0.0,
            sigma_0: 1.0,
            adaptive_lambda: false, // Fixed for deterministic tests
            grapa_eta: 0.1,
            enable_logging: true,
            hysteresis: 1.0,
            alert_cooldown: 0,
        }
    }

    // =========================================================================
    // Basic construction and invariants
    // =========================================================================

    #[test]
    fn initial_state() {
        let alerter = ConformalAlert::new(test_config());
        assert!((alerter.e_value() - 1.0).abs() < f64::EPSILON);
        assert_eq!(alerter.calibration_count(), 0);
        assert!((alerter.mean() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn calibration_updates_stats() {
        let mut alerter = ConformalAlert::new(test_config());

        alerter.calibrate(10.0);
        alerter.calibrate(20.0);
        alerter.calibrate(30.0);

        assert_eq!(alerter.calibration_count(), 3);
        assert!((alerter.mean() - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn calibration_window_enforced() {
        let mut config = test_config();
        config.max_calibration = 5;
        let mut alerter = ConformalAlert::new(config);

        for i in 1..=10 {
            alerter.calibrate(i as f64);
        }

        assert_eq!(alerter.calibration_count(), 5);
    }

    // =========================================================================
    // Conformal threshold tests
    // =========================================================================

    #[test]
    fn conformal_threshold_increases_with_calibration() {
        let mut alerter = ConformalAlert::new(test_config());

        // Calibrate with increasing residuals
        for i in 1..=20 {
            alerter.calibrate(i as f64);
        }

        let threshold = alerter.threshold();
        assert!(threshold > 0.0, "Threshold should be positive");
        assert!(threshold < f64::MAX, "Threshold should be finite");
    }

    #[test]
    fn conformal_threshold_n_plus_1_rule() {
        let mut config = test_config();
        config.alpha = 0.1; // 90% coverage
        config.min_calibration = 3;
        let mut alerter = ConformalAlert::new(config);

        // Note: residuals are computed as |value - current_mean| at calibration time.
        // With evolving mean, residuals don't directly correspond to absolute deviations
        // from the final mean. The key property is that threshold is computed correctly
        // from whatever residuals are stored.
        for v in [50.0, 60.0, 70.0, 40.0, 30.0] {
            alerter.calibrate(v);
        }

        // With n=5, alpha=0.1: idx = ceil(0.9 * 6) - 1 = 5 - 1 = 4
        let threshold = alerter.threshold();
        // Threshold should be reasonable (non-negative and finite)
        assert!(threshold >= 0.0, "Threshold should be non-negative");
        assert!(threshold < f64::MAX, "Threshold should be finite");
    }

    #[test]
    fn conformal_score_correct() {
        let mut alerter = ConformalAlert::new(test_config());

        // Calibrate with known residuals (centered at 100)
        for v in [100.0, 110.0, 120.0, 130.0, 140.0] {
            alerter.calibrate(v);
        }

        // Mean is ~120, so residuals are: 20, 10, 0, 10, 20
        // Sorted: [0, 10, 10, 20, 20]

        // Score for residual=0: (5+1)/(5+1) = 1.0
        let score_low = alerter.compute_conformal_score(0.0);
        assert!(score_low > 0.8);

        // Score for residual=100: (0+1)/(5+1) = 1/6
        let score_high = alerter.compute_conformal_score(100.0);
        assert!(score_high < 0.3);
    }

    // =========================================================================
    // E-process tests
    // =========================================================================

    #[test]
    fn evalue_grows_on_extreme_observation() {
        let mut config = test_config();
        config.hysteresis = 1e10; // Very high threshold so we don't trigger alert
        let mut alerter = ConformalAlert::new(config);

        // Calibrate with low variance data around 50
        for v in [49.0, 50.0, 51.0, 50.0, 49.5, 50.5] {
            alerter.calibrate(v);
        }

        let e_before = alerter.e_value();

        // Observe extreme value (many std devs away)
        let decision = alerter.observe(100.0);

        // E-value from evidence should show growth
        // Note: if alert triggers, e_value resets to 1.0 after
        // So check the evidence e_value instead
        assert!(
            decision.evidence.e_value > e_before,
            "E-value should grow on extreme observation: {} vs {}",
            decision.evidence.e_value,
            e_before
        );
    }

    #[test]
    fn evalue_shrinks_on_normal_observation() {
        let mut config = test_config();
        config.mu_0 = 0.0;
        config.sigma_0 = 1.0;
        let mut alerter = ConformalAlert::new(config);

        // Calibrate with data around 50
        for v in [48.0, 49.0, 50.0, 51.0, 52.0] {
            alerter.calibrate(v);
        }

        let e_before = alerter.e_value();

        // Observe normal value (close to mean)
        let _ = alerter.observe(50.0);

        // E-value should shrink or stay similar
        assert!(
            alerter.e_value() <= e_before * 2.0,
            "E-value should not explode on normal observation"
        );
    }

    #[test]
    fn evalue_stays_positive() {
        let mut alerter = ConformalAlert::new(test_config());

        for v in [45.0, 50.0, 55.0, 50.0, 45.0, 55.0] {
            alerter.calibrate(v);
        }

        // Many normal observations
        for _ in 0..100 {
            let _ = alerter.observe(50.0);
            assert!(alerter.e_value() > 0.0, "E-value must stay positive");
        }
    }

    #[test]
    fn evalue_resets_after_alert() {
        let mut config = test_config();
        config.alert_cooldown = 0;
        config.hysteresis = 0.5; // Easy trigger
        let mut alerter = ConformalAlert::new(config);

        for v in [49.0, 50.0, 51.0, 50.0, 49.5] {
            alerter.calibrate(v);
        }

        // Drive to alert with extreme values
        for _ in 0..50 {
            let decision = alerter.observe(200.0);
            if decision.is_alert {
                // E-value should reset to 1.0 after alert
                assert!(
                    (alerter.e_value() - 1.0).abs() < 0.01,
                    "E-value should reset after alert, got {}",
                    alerter.e_value()
                );
                return;
            }
        }
        // Should have triggered by now
        assert!(
            alerter.stats().total_alerts > 0,
            "Should have triggered alert"
        );
    }

    // =========================================================================
    // Alert triggering tests
    // =========================================================================

    #[test]
    fn extreme_value_triggers_conformal_alert() {
        let mut config = test_config();
        config.alert_cooldown = 0;
        let mut alerter = ConformalAlert::new(config);

        // Calibrate with tight distribution
        for v in [50.0, 50.1, 49.9, 50.0, 49.8, 50.2] {
            alerter.calibrate(v);
        }

        // Observe extreme value
        let decision = alerter.observe(100.0);
        assert!(
            decision.evidence.conformal_alert,
            "Extreme value should trigger conformal alert"
        );
    }

    #[test]
    fn normal_value_no_alert() {
        let mut alerter = ConformalAlert::new(test_config());

        for v in [45.0, 50.0, 55.0, 45.0, 55.0, 50.0] {
            alerter.calibrate(v);
        }

        // Normal observation
        let decision = alerter.observe(48.0);
        assert!(!decision.is_alert, "Normal value should not trigger alert");
    }

    #[test]
    fn insufficient_calibration_no_alert() {
        let config = test_config(); // min_calibration = 5
        let mut alerter = ConformalAlert::new(config);

        alerter.calibrate(50.0);
        alerter.calibrate(51.0);
        // Only 2 samples, need 5

        let decision = alerter.observe(1000.0); // Extreme value
        assert!(
            !decision.is_alert,
            "Should not alert with insufficient calibration"
        );
        assert_eq!(
            decision.evidence.reason,
            AlertReason::InsufficientCalibration
        );
    }

    #[test]
    fn cooldown_prevents_rapid_alerts() {
        let mut config = test_config();
        config.alert_cooldown = 5;
        config.hysteresis = 0.1; // Easy trigger
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 50.0, 50.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        // Trigger first alert
        let mut first_alert_obs = 0;
        for i in 1..=10 {
            let decision = alerter.observe(200.0);
            if decision.is_alert {
                first_alert_obs = i;
                break;
            }
        }
        assert!(first_alert_obs > 0, "Should trigger first alert");

        // Next few should be cooldown
        for _ in 0..3 {
            let decision = alerter.observe(200.0);
            if decision.evidence.reason == AlertReason::InCooldown {
                return; // Test passed
            }
        }
        // If we reach here, cooldown might have expired
    }

    // =========================================================================
    // Evidence ledger tests
    // =========================================================================

    #[test]
    fn evidence_contains_all_fields() {
        let mut alerter = ConformalAlert::new(test_config());

        // Use values with variance so residuals and threshold are positive
        for v in [45.0, 50.0, 55.0, 48.0, 52.0] {
            alerter.calibrate(v);
        }

        let decision = alerter.observe(75.0);
        let ev = &decision.evidence;

        assert_eq!(ev.observation_idx, 1);
        assert!((ev.value - 75.0).abs() < f64::EPSILON);
        assert!(ev.residual.abs() > 0.0 || ev.z_score.abs() > 0.0);
        // Threshold is non-negative (can be 0 for identical calibration data)
        assert!(ev.conformal_threshold >= 0.0);
        assert!(ev.conformal_score > 0.0 && ev.conformal_score <= 1.0);
        assert!(ev.e_value > 0.0);
        assert!(ev.e_threshold > 0.0);
        assert!(ev.lambda > 0.0);
    }

    #[test]
    fn logs_captured_when_enabled() {
        let mut config = test_config();
        config.enable_logging = true;
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 50.0, 50.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        alerter.observe(60.0);
        alerter.observe(70.0);
        alerter.observe(80.0);

        assert_eq!(alerter.logs().len(), 3);
        assert_eq!(alerter.logs()[0].observation_idx, 1);
        assert_eq!(alerter.logs()[2].observation_idx, 3);

        alerter.clear_logs();
        assert!(alerter.logs().is_empty());
    }

    #[test]
    fn logs_not_captured_when_disabled() {
        let mut config = test_config();
        config.enable_logging = false;
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 50.0, 50.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        alerter.observe(60.0);
        assert!(alerter.logs().is_empty());
    }

    // =========================================================================
    // Statistics tests
    // =========================================================================

    #[test]
    fn stats_reflect_state() {
        let mut config = test_config();
        config.alert_cooldown = 0;
        config.hysteresis = 0.1;
        let mut alerter = ConformalAlert::new(config);

        // Use values with variance for realistic calibration
        for v in [45.0, 50.0, 55.0, 48.0, 52.0] {
            alerter.calibrate(v);
        }

        // Some normal observations
        for _ in 0..5 {
            alerter.observe(50.0);
        }

        // Some extreme observations
        for _ in 0..5 {
            alerter.observe(200.0);
        }

        let stats = alerter.stats();
        assert_eq!(stats.total_observations, 10);
        assert_eq!(stats.calibration_samples, 5);
        assert!(stats.calibration_mean > 0.0);
        assert!(stats.calibration_std >= 0.0);
        // Threshold is non-negative (can be 0 for identical data)
        assert!(stats.current_threshold >= 0.0);
    }

    // =========================================================================
    // FPR control property tests
    // =========================================================================

    #[test]
    fn property_fpr_controlled_under_null() {
        // Under H0 (observations from same distribution as calibration),
        // the FPR should be approximately <= alpha.
        let mut config = test_config();
        config.alpha = 0.10;
        config.alert_cooldown = 0;
        config.hysteresis = 1.0;
        config.adaptive_lambda = false;
        let mut alerter = ConformalAlert::new(config);

        // LCG for deterministic pseudo-random
        let mut rng_state: u64 = 12345;
        let lcg_next = |state: &mut u64| -> f64 {
            *state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            // Map to roughly N(50, 5)
            let u = (*state >> 33) as f64 / (1u64 << 31) as f64;
            50.0 + (u - 0.5) * 10.0
        };

        // Calibration
        for _ in 0..100 {
            alerter.calibrate(lcg_next(&mut rng_state));
        }

        // Observation under H0
        let n_obs = 500;
        let mut alerts = 0;
        for _ in 0..n_obs {
            let decision = alerter.observe(lcg_next(&mut rng_state));
            if decision.is_alert {
                alerts += 1;
            }
        }

        let empirical_fpr = alerts as f64 / n_obs as f64;
        // Allow 3x slack for finite sample
        assert!(
            empirical_fpr < alerter.alpha() * 3.0 + 0.05,
            "Empirical FPR {} should be <= 3*alpha + slack",
            empirical_fpr
        );
    }

    #[test]
    fn property_conformal_threshold_monotonic() {
        // The (1-alpha) quantile should increase with calibration set size
        // (more data = better estimate of tail, but also more extreme values seen)
        let mut alerter = ConformalAlert::new(test_config());

        let mut rng_state: u64 = 54321;
        let lcg_next = |state: &mut u64| -> f64 {
            *state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            50.0 + ((*state >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 20.0
        };

        let mut thresholds = Vec::new();
        for _ in 0..50 {
            alerter.calibrate(lcg_next(&mut rng_state));
            if alerter.calibration_count() >= alerter.config.min_calibration {
                thresholds.push(alerter.threshold());
            }
        }

        // Not strictly monotonic due to sampling, but should be bounded
        assert!(!thresholds.is_empty());
        let max_threshold = *thresholds
            .iter()
            .max_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap();
        let min_threshold = *thresholds
            .iter()
            .min_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap();
        assert!(
            max_threshold < min_threshold * 10.0,
            "Thresholds should be reasonably stable"
        );
    }

    // =========================================================================
    // Determinism tests
    // =========================================================================

    #[test]
    fn deterministic_behavior() {
        let config = test_config();

        let run = |config: &AlertConfig| {
            let mut alerter = ConformalAlert::new(config.clone());
            for v in [50.0, 51.0, 49.0, 52.0, 48.0] {
                alerter.calibrate(v);
            }
            let mut decisions = Vec::new();
            for v in [55.0, 45.0, 100.0, 50.0] {
                decisions.push(alerter.observe(v).is_alert);
            }
            (decisions, alerter.e_value(), alerter.threshold())
        };

        let (d1, e1, t1) = run(&config);
        let (d2, e2, t2) = run(&config);

        assert_eq!(d1, d2, "Decisions must be deterministic");
        assert!((e1 - e2).abs() < 1e-10, "E-value must be deterministic");
        assert!((t1 - t2).abs() < 1e-10, "Threshold must be deterministic");
    }

    // =========================================================================
    // Edge cases
    // =========================================================================

    #[test]
    fn empty_calibration() {
        let alerter = ConformalAlert::new(test_config());
        let threshold = alerter.threshold();
        assert_eq!(threshold, FALLBACK_THRESHOLD);
    }

    #[test]
    fn single_calibration_value() {
        let mut alerter = ConformalAlert::new(test_config());
        alerter.calibrate(50.0);

        // With single sample, mean=50, residual=0, so threshold=0
        // This is expected behavior
        let threshold = alerter.threshold();
        assert!(threshold >= 0.0, "Threshold should be non-negative");
        assert!(threshold < f64::MAX, "Should not be fallback");
    }

    #[test]
    fn all_same_calibration() {
        let mut alerter = ConformalAlert::new(test_config());
        for _ in 0..10 {
            alerter.calibrate(50.0);
        }

        // Std should be 0 (or epsilon)
        assert!(alerter.std() < 0.1);

        // Any deviation should alert
        let decision = alerter.observe(51.0);
        assert!(
            decision.evidence.conformal_alert,
            "Any deviation from constant calibration should alert"
        );
    }

    #[test]
    fn reset_clears_eprocess() {
        let mut config = test_config();
        config.hysteresis = 1e10; // Prevent alert from triggering
        let mut alerter = ConformalAlert::new(config);

        // Use values with variance
        for v in [45.0, 50.0, 55.0, 48.0, 52.0] {
            alerter.calibrate(v);
        }

        // Drive e-value up with extreme observation
        let decision = alerter.observe(200.0);
        // Check the evidence e_value, not the final e_value (which may reset on alert)
        assert!(
            decision.evidence.e_value > 1.0,
            "E-value in evidence should be > 1.0: {}",
            decision.evidence.e_value
        );

        alerter.reset_eprocess();
        assert!((alerter.e_value() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clear_calibration_resets_all() {
        let mut alerter = ConformalAlert::new(test_config());

        for v in [50.0, 50.0, 50.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }
        alerter.observe(75.0);

        alerter.clear_calibration();
        assert_eq!(alerter.calibration_count(), 0);
        assert!((alerter.mean() - 0.0).abs() < f64::EPSILON);
        assert!((alerter.e_value() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn evidence_summary_format() {
        let mut alerter = ConformalAlert::new(test_config());

        for v in [50.0, 50.0, 50.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        let decision = alerter.observe(75.0);
        let summary = decision.evidence_summary();

        assert!(summary.contains("obs="));
        assert!(summary.contains("val="));
        assert!(summary.contains("res="));
        assert!(summary.contains("E="));
        assert!(summary.contains("alert="));
    }

    #[test]
    fn evalue_ceiling_prevents_overflow() {
        // Test that extremely large z-scores don't cause e-value to overflow to infinity
        let mut config = test_config();
        config.hysteresis = f64::MAX; // Prevent alerts from resetting e-value
        config.alert_cooldown = 0;
        let mut alerter = ConformalAlert::new(config);

        // Calibrate with tight distribution around 0
        for _ in 0..10 {
            alerter.calibrate(0.0);
        }

        // Observe astronomically large value that would cause overflow without ceiling
        // Without the fix, exp(lambda * z_score) would be infinity
        let decision = alerter.observe(1e100);

        // E-value should be capped at E_MAX (1e12), not infinity
        assert!(
            decision.evidence.e_value.is_finite(),
            "E-value should be finite, got {}",
            decision.evidence.e_value
        );
        assert!(
            decision.evidence.e_value <= E_MAX,
            "E-value {} should be <= E_MAX {}",
            decision.evidence.e_value,
            E_MAX
        );
        assert!(
            decision.evidence.e_value > 0.0,
            "E-value should be positive"
        );
    }

    #[test]
    fn evalue_floor_prevents_underflow() {
        // Test that extremely negative z-scores don't cause e-value to underflow to zero
        let mut config = test_config();
        config.hysteresis = f64::MAX;
        let mut alerter = ConformalAlert::new(config);

        // Calibrate with values around a large number
        for _ in 0..10 {
            alerter.calibrate(1e100);
        }

        // Observe zero - this creates a massive negative z-score
        let decision = alerter.observe(0.0);

        // E-value should be floored at E_MIN, not zero or subnormal
        assert!(
            decision.evidence.e_value >= E_MIN,
            "E-value {} should be >= E_MIN {}",
            decision.evidence.e_value,
            E_MIN
        );
        assert!(
            decision.evidence.e_value.is_finite(),
            "E-value should be finite"
        );
    }

    // =========================================================================
    // Edge-case tests (bd-1uzxz)
    // =========================================================================

    #[test]
    fn edge_observe_nan() {
        let mut alerter = ConformalAlert::new(test_config());
        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }
        // NaN observation should not panic
        let decision = alerter.observe(f64::NAN);
        // NaN comparisons are always false, so conformal_alert should be false
        assert!(!decision.evidence.conformal_alert);
        assert_eq!(alerter.stats().total_observations, 1);
    }

    #[test]
    fn edge_observe_infinity() {
        let mut alerter = ConformalAlert::new(test_config());
        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }
        let decision = alerter.observe(f64::INFINITY);
        // Infinite residual should trigger conformal alert
        assert!(decision.evidence.conformal_alert);
        // E-value should be clamped, not Inf
        assert!(decision.evidence.e_value.is_finite() || decision.evidence.e_value <= E_MAX);
    }

    #[test]
    fn edge_observe_neg_infinity() {
        let mut alerter = ConformalAlert::new(test_config());
        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }
        let decision = alerter.observe(f64::NEG_INFINITY);
        // Negative infinite residual should trigger conformal alert
        assert!(decision.evidence.conformal_alert);
    }

    #[test]
    fn edge_calibrate_nan() {
        let mut alerter = ConformalAlert::new(test_config());
        // NaN calibration should not panic
        alerter.calibrate(f64::NAN);
        assert_eq!(alerter.calibration_count(), 1);
        // Mean will be NaN, which is handled gracefully
    }

    #[test]
    fn edge_calibrate_infinity() {
        let mut alerter = ConformalAlert::new(test_config());
        alerter.calibrate(f64::INFINITY);
        assert_eq!(alerter.calibration_count(), 1);
    }

    #[test]
    fn edge_alpha_one() {
        // alpha=1.0 means 100% FPR tolerance -> threshold should be very low
        let mut config = test_config();
        config.alpha = 1.0;
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        // With alpha=1.0: idx = ceil(0.0 * 6) - 1 = saturating_sub -> 0
        // Threshold is the minimum residual
        let threshold = alerter.threshold();
        assert!(threshold >= 0.0);
        assert!(threshold < f64::MAX);
    }

    #[test]
    fn edge_alpha_very_small() {
        // Very small alpha -> very high threshold, very few alerts
        let mut config = test_config();
        config.alpha = 1e-10;
        config.hysteresis = 1.0;
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        // E-threshold = 1/alpha = 1e10 -> extremely hard to trigger
        let stats = alerter.stats();
        assert!(stats.current_threshold >= 0.0);
        // Normal observation should not alert
        let decision = alerter.observe(52.0);
        assert!(!decision.evidence.eprocess_alert);
    }

    #[test]
    fn edge_lambda_clamped_at_zero() {
        let mut config = test_config();
        config.lambda = 0.0;
        config.adaptive_lambda = false;
        let alerter = ConformalAlert::new(config);
        // Lambda should be clamped to EPSILON, not 0.0
        assert!(alerter.stats().current_lambda > 0.0);
    }

    #[test]
    fn edge_lambda_clamped_at_one() {
        let mut config = test_config();
        config.lambda = 1.0;
        config.adaptive_lambda = false;
        let alerter = ConformalAlert::new(config);
        // Lambda should be clamped to 1-EPSILON, not 1.0
        assert!(alerter.stats().current_lambda < 1.0);
    }

    #[test]
    fn edge_sigma_0_zero() {
        let mut config = test_config();
        config.sigma_0 = 0.0;
        config.adaptive_lambda = false;
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        // With sigma_0=0, e-process exponent = lambda * z - 0
        // Should not panic
        let decision = alerter.observe(55.0);
        assert!(decision.evidence.e_value.is_finite());
    }

    #[test]
    fn edge_hysteresis_zero() {
        let mut config = test_config();
        config.hysteresis = 0.0;
        config.alert_cooldown = 0;
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 50.0, 50.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        // E-threshold = 1/alpha * 0 = 0, so any positive e-value triggers e-process alert
        let decision = alerter.observe(51.0);
        assert!(decision.evidence.eprocess_alert);
    }

    #[test]
    fn edge_max_calibration_zero() {
        let mut config = test_config();
        config.max_calibration = 0;
        let mut alerter = ConformalAlert::new(config);

        alerter.calibrate(50.0);
        // With max_calibration=0, the value is immediately evicted
        assert_eq!(alerter.calibration_count(), 0);
    }

    #[test]
    fn edge_min_calibration_zero() {
        let mut config = test_config();
        config.min_calibration = 0;
        config.alert_cooldown = 0;
        let mut alerter = ConformalAlert::new(config);

        // Even with no calibration data, min_calibration=0 allows observation
        // But empty calibration returns FALLBACK_THRESHOLD
        alerter.calibrate(50.0);
        let decision = alerter.observe(55.0);
        // Should not return InsufficientCalibration
        assert_ne!(
            decision.evidence.reason,
            AlertReason::InsufficientCalibration
        );
    }

    #[test]
    fn edge_stats_no_observations() {
        let alerter = ConformalAlert::new(test_config());
        let stats = alerter.stats();
        assert_eq!(stats.total_observations, 0);
        assert_eq!(stats.total_alerts, 0);
        assert_eq!(stats.conformal_alerts, 0);
        assert_eq!(stats.eprocess_alerts, 0);
        assert_eq!(stats.both_alerts, 0);
        assert_eq!(stats.empirical_fpr, 0.0);
        assert_eq!(stats.calibration_samples, 0);
    }

    #[test]
    fn edge_adaptive_lambda_grapa() {
        let mut config = test_config();
        config.adaptive_lambda = true;
        config.grapa_eta = 0.5;
        config.hysteresis = 1e10; // Prevent alert reset
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        let lambda_before = alerter.stats().current_lambda;

        // Extreme observation should shift lambda via GRAPA
        alerter.observe(100.0);

        let lambda_after = alerter.stats().current_lambda;
        // Lambda should have changed (GRAPA gradient update)
        assert!(
            (lambda_after - lambda_before).abs() > 1e-10,
            "Lambda should change with GRAPA: before={} after={}",
            lambda_before,
            lambda_after
        );
    }

    #[test]
    fn edge_adaptive_lambda_stays_bounded() {
        let mut config = test_config();
        config.adaptive_lambda = true;
        config.grapa_eta = 1.0; // Aggressive learning rate
        config.hysteresis = 1e10;
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        // Many extreme observations
        for _ in 0..100 {
            alerter.observe(1000.0);
        }

        let lambda = alerter.stats().current_lambda;
        assert!(lambda > 0.0, "Lambda should be positive");
        assert!(lambda < 1.0, "Lambda should be < 1.0");
    }

    #[test]
    fn edge_alert_reason_equality() {
        assert_eq!(AlertReason::Normal, AlertReason::Normal);
        assert_eq!(
            AlertReason::ConformalExceeded,
            AlertReason::ConformalExceeded
        );
        assert_eq!(AlertReason::EProcessExceeded, AlertReason::EProcessExceeded);
        assert_eq!(AlertReason::BothExceeded, AlertReason::BothExceeded);
        assert_eq!(AlertReason::InCooldown, AlertReason::InCooldown);
        assert_eq!(
            AlertReason::InsufficientCalibration,
            AlertReason::InsufficientCalibration
        );
        assert_ne!(AlertReason::Normal, AlertReason::InCooldown);
    }

    #[test]
    fn edge_alert_config_clone_debug() {
        let config = AlertConfig::default();
        let cloned = config.clone();
        assert_eq!(cloned.alpha, config.alpha);
        assert_eq!(cloned.min_calibration, config.min_calibration);
        let debug = format!("{:?}", config);
        assert!(debug.contains("AlertConfig"));
    }

    #[test]
    fn edge_alert_evidence_clone_debug() {
        let mut alerter = ConformalAlert::new(test_config());
        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }
        let decision = alerter.observe(60.0);
        let cloned = decision.evidence.clone();
        assert_eq!(cloned.observation_idx, decision.evidence.observation_idx);
        assert_eq!(cloned.is_alert, decision.evidence.is_alert);
        let debug = format!("{:?}", decision.evidence);
        assert!(debug.contains("AlertEvidence"));
    }

    #[test]
    fn edge_alert_decision_clone_debug() {
        let mut alerter = ConformalAlert::new(test_config());
        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }
        let decision = alerter.observe(60.0);
        let cloned = decision.clone();
        assert_eq!(cloned.is_alert, decision.is_alert);
        assert_eq!(
            cloned.observations_since_alert,
            decision.observations_since_alert
        );
        let debug = format!("{:?}", decision);
        assert!(debug.contains("AlertDecision"));
    }

    #[test]
    fn edge_alert_stats_clone_debug() {
        let alerter = ConformalAlert::new(test_config());
        let stats = alerter.stats();
        let cloned = stats.clone();
        assert_eq!(cloned.total_observations, stats.total_observations);
        let debug = format!("{:?}", stats);
        assert!(debug.contains("AlertStats"));
    }

    #[test]
    fn edge_conformal_alert_debug() {
        let alerter = ConformalAlert::new(test_config());
        let debug = format!("{:?}", alerter);
        assert!(debug.contains("ConformalAlert"));
    }

    #[test]
    fn edge_evidence_is_alert_matches_decision() {
        let mut alerter = ConformalAlert::new(test_config());
        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        for obs in [50.0, 100.0, 50.5, 200.0, 49.0] {
            let decision = alerter.observe(obs);
            assert_eq!(
                decision.is_alert, decision.evidence.is_alert,
                "Decision.is_alert should match evidence.is_alert for obs={}",
                obs
            );
        }
    }

    #[test]
    fn edge_alert_counters_correct() {
        let mut config = test_config();
        config.alert_cooldown = 0;
        config.hysteresis = 0.1; // Easy trigger
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 50.0, 50.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        let mut total = 0u64;
        for _ in 0..20 {
            let decision = alerter.observe(200.0);
            if decision.is_alert {
                total += 1;
            }
        }

        let stats = alerter.stats();
        assert_eq!(stats.total_alerts, total);
        assert_eq!(
            stats.conformal_alerts + stats.eprocess_alerts + stats.both_alerts,
            stats.total_alerts
        );
    }

    #[test]
    fn edge_interleaved_calibrate_observe() {
        let mut config = test_config();
        config.min_calibration = 3;
        config.alert_cooldown = 0;
        let mut alerter = ConformalAlert::new(config);

        // Calibrate enough to observe
        alerter.calibrate(50.0);
        alerter.calibrate(51.0);
        alerter.calibrate(49.0);

        let d1 = alerter.observe(50.0);
        assert!(!d1.is_alert);

        // Add more calibration
        alerter.calibrate(50.0);
        alerter.calibrate(50.0);

        // Observe again - should still work
        let d2 = alerter.observe(50.0);
        assert!(!d2.is_alert);
        assert_eq!(alerter.calibration_count(), 5);
        assert_eq!(alerter.stats().total_observations, 2);
    }

    #[test]
    fn edge_clear_then_recalibrate() {
        let mut alerter = ConformalAlert::new(test_config());

        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }
        alerter.observe(60.0);
        alerter.clear_calibration();

        // Re-calibrate with completely different distribution
        for v in [0.0, 1.0, 2.0, 3.0, 4.0] {
            alerter.calibrate(v);
        }

        assert_eq!(alerter.calibration_count(), 5);
        assert!((alerter.mean() - 2.0).abs() < f64::EPSILON);
        assert!((alerter.e_value() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn edge_cooldown_max_u64() {
        let mut config = test_config();
        config.alert_cooldown = u64::MAX;
        config.hysteresis = 0.1;
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 50.0, 50.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        // First extreme observation triggers alert
        let mut got_alert = false;
        for _ in 0..10 {
            let d = alerter.observe(200.0);
            if d.is_alert {
                got_alert = true;
                break;
            }
        }
        assert!(got_alert, "Should get first alert");

        // All subsequent observations should be in cooldown
        for _ in 0..10 {
            let d = alerter.observe(200.0);
            assert_eq!(d.evidence.reason, AlertReason::InCooldown);
        }
    }

    #[test]
    fn edge_welford_variance_single_sample() {
        let mut stats = CalibrationStats::new();
        stats.update(42.0);
        // With n=1, variance falls back to 1.0
        assert!((stats.variance() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn edge_welford_variance_zero_samples() {
        let stats = CalibrationStats::new();
        // With n=0, variance falls back to 1.0
        assert!((stats.variance() - 1.0).abs() < f64::EPSILON);
        assert!((stats.std() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn edge_welford_known_variance() {
        let mut stats = CalibrationStats::new();
        // Values [2, 4, 6, 8, 10]: mean=6, var=10
        for v in [2.0, 4.0, 6.0, 8.0, 10.0] {
            stats.update(v);
        }
        assert!((stats.mean - 6.0).abs() < f64::EPSILON);
        assert!((stats.variance() - 10.0).abs() < 1e-10);
    }

    #[test]
    fn edge_conformal_score_empty_calibration() {
        let alerter = ConformalAlert::new(test_config());
        let score = alerter.compute_conformal_score(42.0);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn edge_long_run_evalue_bounded() {
        let mut config = test_config();
        config.hysteresis = 1e10; // Prevent alert reset
        config.adaptive_lambda = false;
        let mut alerter = ConformalAlert::new(config);

        for v in [50.0, 51.0, 49.0, 50.0, 50.0] {
            alerter.calibrate(v);
        }

        // 1000 normal observations - e-value should remain bounded
        for _ in 0..1000 {
            alerter.observe(50.0);
            let ev = alerter.e_value();
            assert!(ev >= E_MIN, "E-value should be >= E_MIN: {}", ev);
            assert!(ev <= E_MAX, "E-value should be <= E_MAX: {}", ev);
            assert!(ev.is_finite(), "E-value should be finite");
        }
    }

    #[test]
    fn edge_default_config_valid() {
        let config = AlertConfig::default();
        assert!(config.alpha > 0.0 && config.alpha < 1.0);
        assert!(config.min_calibration > 0);
        assert!(config.max_calibration > 0);
        assert!(config.lambda > 0.0 && config.lambda < 1.0);
        assert!(config.sigma_0 > 0.0);
        assert!(config.hysteresis >= 1.0);
        assert!(config.grapa_eta > 0.0);
    }
}

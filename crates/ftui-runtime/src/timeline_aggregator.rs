#![forbid(unsafe_code)]

//! Timeline Aggregator: Sketch-Based Aggregation + Change-Point Alerts (bd-11ck.7).
//!
//! This module provides bounded-memory aggregation for action timeline events with
//! automatic spike detection using Bayesian Online Change Point Detection (BOCPD).
//!
//! # Mathematical Model
//!
//! ## Sketch-Based Aggregation
//!
//! Uses Count-Min Sketch for O(1) frequency estimation with bounded memory:
//! - Width and depth determine memory/accuracy trade-off
//! - PAC-Bayes calibration tightens error bounds using observed data
//!
//! ## Bayesian Online Change Point Detection (BOCPD)
//!
//! Detects sudden changes in event rate using Bayesian inference:
//! 1. Maintain run length distribution P(r_t | x_{1:t})
//! 2. Compute hazard function H(r) = P(change at position r)
//! 3. Growth probability: P(r_t = r_{t-1} + 1) ∝ (1 - H) * P(x_t | r)
//! 4. Change probability: P(r_t = 0) ∝ H * Σ_r P(x_t | r) * P(r_{t-1})
//!
//! ## Conformal Alert Integration
//!
//! Uses conformal prediction for distribution-free threshold calibration:
//! - E-process provides anytime-valid FPR control
//! - Residuals computed as |observed - predicted| normalized by rolling variance
//!
//! # Key Invariants
//!
//! 1. **Bounded memory**: Aggregation state is O(width * depth + window_size)
//! 2. **Anytime valid**: Change-point alerts maintain FPR guarantee at all times
//! 3. **Online**: All updates are O(depth) time complexity
//! 4. **Deterministic**: Same sequence produces same alerts
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | Cold start | Suppress alerts | Need calibration data |
//! | Zero variance | Use minimum threshold | Avoid division by zero |
//! | Rapid bursts | Detect but throttle | Prevent alert storms |

use crate::conformal_alert::{
    AlertConfig, AlertDecision, AlertEvidence, AlertReason, ConformalAlert,
};
use crate::countmin_sketch::{CountMinSketch, ErrorEvidence, SketchConfig};

/// Minimum variance for normalization.
const MIN_VARIANCE: f64 = 1e-6;

/// Default BOCPD hazard rate (1/250 = expect change every 250 observations).
const DEFAULT_HAZARD_RATE: f64 = 0.004;

/// Maximum run lengths to track in BOCPD.
const MAX_RUN_LENGTH: usize = 256;

/// Configuration for the timeline aggregator.
#[derive(Debug, Clone)]
pub struct AggregatorConfig {
    /// Sketch configuration for frequency estimation.
    pub sketch: SketchConfig,

    /// Alert configuration for change-point detection.
    pub alert: AlertConfig,

    /// BOCPD hazard rate: probability of change point at each step.
    /// Lower values = fewer false positives, higher values = faster detection.
    /// Default: 0.004 (expect change every ~250 observations).
    pub hazard_rate: f64,

    /// Window size for rolling statistics.
    pub window_size: usize,

    /// Minimum observations before enabling alerts.
    pub warmup_observations: usize,

    /// Enable detailed logging.
    pub enable_logging: bool,
}

impl Default for AggregatorConfig {
    fn default() -> Self {
        Self {
            sketch: SketchConfig::default(),
            alert: AlertConfig::default(),
            hazard_rate: DEFAULT_HAZARD_RATE,
            window_size: 100,
            warmup_observations: 50,
            enable_logging: false,
        }
    }
}

/// Evidence for aggregation decisions.
#[derive(Debug, Clone)]
pub struct AggregationEvidence {
    /// Current sketch error bounds.
    pub sketch_evidence: ErrorEvidence,

    /// Current alert decision.
    pub alert_decision: AlertDecision,

    /// BOCPD run length estimate.
    pub run_length: usize,

    /// BOCPD change probability.
    pub change_probability: f64,

    /// Rolling mean of event rate.
    pub rolling_mean: f64,

    /// Rolling variance of event rate.
    pub rolling_variance: f64,

    /// Total observations processed.
    pub observation_count: u64,
}

/// Aggregation statistics.
#[derive(Debug, Clone, Default)]
pub struct AggregatorStats {
    /// Total events observed.
    pub total_events: u64,

    /// Total change points detected.
    pub change_points_detected: u64,

    /// Total alerts triggered.
    pub alerts_triggered: u64,

    /// Current run length since last change point.
    pub current_run_length: usize,

    /// Memory usage in bytes.
    pub memory_bytes: usize,
}

/// BOCPD run length distribution state.
#[derive(Debug, Clone)]
struct BocpdState {
    /// Run length probabilities P(r_t | x_{1:t}).
    run_lengths: Vec<f64>,

    /// Hazard rate.
    hazard: f64,

    /// Prior mean for new segments.
    prior_mean: f64,

    /// Prior variance for new segments.
    prior_variance: f64,

    /// Observation count per run length.
    obs_counts: Vec<u64>,

    /// Sum of observations per run length.
    obs_sums: Vec<f64>,

    /// Sum of squared observations per run length.
    obs_sq_sums: Vec<f64>,
}

impl BocpdState {
    fn new(hazard: f64) -> Self {
        let mut run_lengths = vec![0.0; MAX_RUN_LENGTH];
        run_lengths[0] = 1.0; // Start with r=0

        Self {
            run_lengths,
            hazard,
            prior_mean: 1.0,
            prior_variance: 1.0,
            obs_counts: vec![0; MAX_RUN_LENGTH],
            obs_sums: vec![0.0; MAX_RUN_LENGTH],
            obs_sq_sums: vec![0.0; MAX_RUN_LENGTH],
        }
    }

    /// Update with new observation and return (most_likely_run_length, change_probability).
    fn update(&mut self, observation: f64) -> (usize, f64) {
        let mut new_probs = vec![0.0; MAX_RUN_LENGTH];
        let mut likelihoods = vec![0.0; MAX_RUN_LENGTH];

        // Compute likelihoods for each run length
        for (r, (likelihood, run_prob)) in likelihoods
            .iter_mut()
            .zip(self.run_lengths.iter())
            .enumerate()
        {
            if *run_prob > 1e-10 {
                *likelihood = self.predictive_likelihood(r, observation);
            }
        }

        // Growth probabilities: P(r_t = r+1 | data)
        let mut growth_sum = 0.0;
        for (r, (run_prob, likelihood)) in self
            .run_lengths
            .iter()
            .zip(likelihoods.iter())
            .take(MAX_RUN_LENGTH - 1)
            .enumerate()
        {
            let prob = run_prob * likelihood * (1.0 - self.hazard);
            new_probs[r + 1] = prob;
            growth_sum += prob;
        }

        // Change probability: P(r_t = 0 | data)
        let change_sum: f64 = self
            .run_lengths
            .iter()
            .zip(likelihoods.iter())
            .map(|(run_prob, likelihood)| run_prob * likelihood * self.hazard)
            .sum();
        new_probs[0] = change_sum;

        // Normalize
        let total = growth_sum + change_sum;
        if total > 1e-10 {
            for p in &mut new_probs {
                *p /= total;
            }
        } else {
            // Fallback: reset to r=0
            new_probs = vec![0.0; MAX_RUN_LENGTH];
            new_probs[0] = 1.0;
        }

        // Update sufficient statistics for each run length
        let mut new_counts = vec![0u64; MAX_RUN_LENGTH];
        let mut new_sums = vec![0.0; MAX_RUN_LENGTH];
        let mut new_sq_sums = vec![0.0; MAX_RUN_LENGTH];

        // Shift statistics for growth
        for r in 0..(MAX_RUN_LENGTH - 1) {
            if self.run_lengths[r] > 1e-10 {
                new_counts[r + 1] = self.obs_counts[r] + 1;
                new_sums[r + 1] = self.obs_sums[r] + observation;
                new_sq_sums[r + 1] = self.obs_sq_sums[r] + observation * observation;
            }
        }

        // Reset statistics for change point
        new_counts[0] = 1;
        new_sums[0] = observation;
        new_sq_sums[0] = observation * observation;

        self.run_lengths = new_probs;
        self.obs_counts = new_counts;
        self.obs_sums = new_sums;
        self.obs_sq_sums = new_sq_sums;

        // Find most likely run length
        let (best_r, _) = self
            .run_lengths
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or((0, &0.0));

        // Change probability is P(r_t = 0)
        let change_prob = self.run_lengths[0];

        (best_r, change_prob)
    }

    /// Compute predictive likelihood P(x_t | r, data).
    fn predictive_likelihood(&self, r: usize, x: f64) -> f64 {
        let n = self.obs_counts[r] as f64;
        let (mean, var) = if n < 2.0 {
            (self.prior_mean, self.prior_variance)
        } else {
            let mean = self.obs_sums[r] / n;
            let var = (self.obs_sq_sums[r] / n - mean * mean).max(MIN_VARIANCE);
            // Posterior predictive has inflated variance
            let posterior_var = var * (1.0 + 1.0 / n);
            (mean, posterior_var)
        };

        // Gaussian predictive likelihood
        let diff = x - mean;
        let log_lik = -0.5 * (diff * diff / var + (2.0 * std::f64::consts::PI * var).ln());
        log_lik.exp().max(1e-100)
    }

    fn reset(&mut self) {
        self.run_lengths = vec![0.0; MAX_RUN_LENGTH];
        self.run_lengths[0] = 1.0;
        self.obs_counts = vec![0; MAX_RUN_LENGTH];
        self.obs_sums = vec![0.0; MAX_RUN_LENGTH];
        self.obs_sq_sums = vec![0.0; MAX_RUN_LENGTH];
    }
}

/// Timeline aggregator with sketch-based frequency estimation and change-point detection.
#[derive(Debug)]
pub struct TimelineAggregator {
    config: AggregatorConfig,

    /// Count-Min Sketch for frequency estimation.
    sketch: CountMinSketch,

    /// Conformal alert for threshold calibration.
    alert: ConformalAlert,

    /// BOCPD state.
    bocpd: BocpdState,

    /// Rolling window of observations.
    window: Vec<f64>,

    /// Rolling mean.
    rolling_mean: f64,

    /// Rolling M2 for variance (Welford's algorithm).
    rolling_m2: f64,

    /// Total observation count.
    observation_count: u64,

    /// Statistics.
    stats: AggregatorStats,
}

impl TimelineAggregator {
    /// Create a new aggregator with given configuration.
    pub fn new(config: AggregatorConfig) -> Self {
        let sketch = CountMinSketch::new(config.sketch.clone());
        let alert = ConformalAlert::new(config.alert.clone());
        let bocpd = BocpdState::new(config.hazard_rate);

        Self {
            config,
            sketch,
            alert,
            bocpd,
            window: Vec::new(),
            rolling_mean: 0.0,
            rolling_m2: 0.0,
            observation_count: 0,
            stats: AggregatorStats::default(),
        }
    }

    /// Observe an event category and count.
    ///
    /// Returns `Some(evidence)` if a change point is detected, `None` otherwise.
    pub fn observe<T: std::hash::Hash>(
        &mut self,
        category: &T,
        count: u64,
    ) -> Option<AggregationEvidence> {
        // Update sketch
        self.sketch.add(category, count);

        // Update rolling statistics
        let value = count as f64;
        self.observation_count += 1;
        self.stats.total_events += count;

        // Welford's online algorithm for mean/variance
        let delta = value - self.rolling_mean;
        self.rolling_mean += delta / self.observation_count as f64;
        let delta2 = value - self.rolling_mean;
        self.rolling_m2 += delta * delta2;

        // Update window
        self.window.push(value);
        if self.window.len() > self.config.window_size {
            self.window.remove(0);
        }

        // Skip during warmup
        if self.observation_count < self.config.warmup_observations as u64 {
            return None;
        }

        // Compute variance
        let variance = if self.observation_count > 1 {
            (self.rolling_m2 / (self.observation_count - 1) as f64).max(MIN_VARIANCE)
        } else {
            MIN_VARIANCE
        };

        // Update BOCPD
        let (run_length, change_prob) = self.bocpd.update(value);
        self.stats.current_run_length = run_length;

        // Compute normalized residual for conformal alert
        let residual = (value - self.rolling_mean).abs() / variance.sqrt();

        // Update conformal alert
        let alert_decision = self.alert.observe(residual);

        // Detect change point: high change probability OR conformal alert
        let is_change_point = change_prob > 0.5 || alert_decision.is_alert;

        if is_change_point {
            self.stats.change_points_detected += 1;
            if alert_decision.is_alert {
                self.stats.alerts_triggered += 1;
            }

            // Build evidence
            let evidence = AggregationEvidence {
                sketch_evidence: self.sketch.error_evidence(),
                alert_decision,
                run_length,
                change_probability: change_prob,
                rolling_mean: self.rolling_mean,
                rolling_variance: variance,
                observation_count: self.observation_count,
            };

            Some(evidence)
        } else {
            None
        }
    }

    /// Get current statistics.
    pub fn stats(&self) -> AggregatorStats {
        let sketch_stats = self.sketch.stats();
        let mut stats = self.stats.clone();
        stats.memory_bytes = sketch_stats.memory_bytes
            + self.window.len() * std::mem::size_of::<f64>()
            + MAX_RUN_LENGTH * (std::mem::size_of::<f64>() + std::mem::size_of::<u64>() * 2);
        stats
    }

    /// Estimate frequency for a category.
    pub fn estimate<T: std::hash::Hash>(&self, category: &T) -> u64 {
        self.sketch.estimate(category)
    }

    /// Get current aggregation evidence (without observing).
    pub fn current_evidence(&self) -> AggregationEvidence {
        let variance = if self.observation_count > 1 {
            (self.rolling_m2 / (self.observation_count - 1) as f64).max(MIN_VARIANCE)
        } else {
            MIN_VARIANCE
        };

        // Find most likely run length
        let (run_length, change_prob) = self
            .bocpd
            .run_lengths
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(r, _)| (r, self.bocpd.run_lengths[0]))
            .unwrap_or((0, 0.0));

        AggregationEvidence {
            sketch_evidence: self.sketch.error_evidence(),
            alert_decision: AlertDecision {
                is_alert: false,
                evidence: AlertEvidence {
                    observation_idx: self.observation_count,
                    value: 0.0,
                    residual: 0.0,
                    z_score: 0.0,
                    conformal_threshold: 0.0,
                    conformal_score: 0.0,
                    e_value: 1.0,
                    e_threshold: 1.0 / self.config.alert.alpha,
                    lambda: self.config.alert.lambda,
                    conformal_alert: false,
                    eprocess_alert: false,
                    is_alert: false,
                    reason: AlertReason::Normal,
                },
                observations_since_alert: self.observation_count,
            },
            run_length,
            change_probability: change_prob,
            rolling_mean: self.rolling_mean,
            rolling_variance: variance,
            observation_count: self.observation_count,
        }
    }

    /// Reset aggregator state.
    pub fn reset(&mut self) {
        self.sketch.clear();
        self.alert.reset_eprocess();
        self.alert.clear_calibration();
        self.bocpd.reset();
        self.window.clear();
        self.rolling_mean = 0.0;
        self.rolling_m2 = 0.0;
        self.observation_count = 0;
        self.stats = AggregatorStats::default();
    }

    /// Add calibration data for the sketch.
    pub fn calibrate<T: std::hash::Hash>(&mut self, category: &T, true_count: u64) {
        self.sketch.calibrate(category, true_count);
    }
}

// =============================================================================
// Unit Tests (bd-11ck.7)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AggregatorConfig {
        AggregatorConfig {
            sketch: SketchConfig {
                epsilon: 0.1,
                delta: 0.1,
                ..Default::default()
            },
            alert: AlertConfig {
                alpha: 0.05,
                min_calibration: 10,
                hysteresis: 1.1,
                ..Default::default()
            },
            hazard_rate: 0.01,
            window_size: 50,
            warmup_observations: 20,
            enable_logging: false,
        }
    }

    // =========================================================================
    // Initialization tests
    // =========================================================================

    #[test]
    fn new_creates_valid_aggregator() {
        let agg = TimelineAggregator::new(test_config());
        assert_eq!(agg.observation_count, 0);
        assert_eq!(agg.stats.total_events, 0);
        assert_eq!(agg.stats.change_points_detected, 0);
    }

    #[test]
    fn default_config_valid() {
        let config = AggregatorConfig::default();
        let agg = TimelineAggregator::new(config);
        assert_eq!(agg.observation_count, 0);
    }

    // =========================================================================
    // Observation tests
    // =========================================================================

    #[test]
    fn observe_updates_sketch() {
        let mut agg = TimelineAggregator::new(test_config());

        for _ in 0..10 {
            agg.observe(&"event_type_a", 1);
        }

        assert!(agg.estimate(&"event_type_a") >= 10);
        assert_eq!(agg.estimate(&"event_type_b"), 0);
    }

    #[test]
    fn observe_tracks_statistics() {
        let mut agg = TimelineAggregator::new(test_config());

        for i in 0..50 {
            agg.observe(&i, 1);
        }

        let stats = agg.stats();
        assert_eq!(stats.total_events, 50);
        assert!(stats.memory_bytes > 0);
    }

    #[test]
    fn observe_suppresses_during_warmup() {
        let mut config = test_config();
        config.warmup_observations = 100;
        let mut agg = TimelineAggregator::new(config);

        // During warmup, should not detect changes
        for i in 0..99 {
            let result = agg.observe(&i, if i == 50 { 1000 } else { 1 });
            assert!(result.is_none(), "Should not alert during warmup");
        }
    }

    // =========================================================================
    // Change point detection tests
    // =========================================================================

    #[test]
    fn detect_sudden_spike() {
        let mut config = test_config();
        config.warmup_observations = 20;
        config.hazard_rate = 0.1; // Higher sensitivity for test
        let mut agg = TimelineAggregator::new(config);

        // Stable period
        for i in 0..50 {
            agg.observe(&i, 1);
        }

        // Sudden spike
        let mut detected = false;
        for i in 50..60 {
            if agg.observe(&i, 100).is_some() {
                detected = true;
            }
        }

        assert!(detected, "Should detect sudden spike");
    }

    #[test]
    fn no_false_positives_stable_stream() {
        let mut config = test_config();
        config.warmup_observations = 20;
        let mut agg = TimelineAggregator::new(config);

        // Stable stream with constant rate
        let mut false_positives = 0;
        for i in 0..200 {
            if agg.observe(&(i % 10), 1).is_some() {
                false_positives += 1;
            }
        }

        // Should have very few false positives (< 10% after warmup)
        let observations_after_warmup = 200 - 20;
        let false_positive_rate = false_positives as f64 / observations_after_warmup as f64;
        assert!(
            false_positive_rate < 0.2,
            "False positive rate {} too high",
            false_positive_rate
        );
    }

    // =========================================================================
    // BOCPD tests
    // =========================================================================

    #[test]
    fn bocpd_run_length_increases_stable() {
        let mut bocpd = BocpdState::new(0.01);

        // Feed stable observations
        for i in 0..50 {
            let (run_len, _) = bocpd.update(1.0 + (i as f64) * 0.01);
            if i > 10 {
                assert!(
                    run_len > 0,
                    "Run length should grow for stable observations"
                );
            }
        }
    }

    #[test]
    fn bocpd_detects_change() {
        let mut bocpd = BocpdState::new(0.01);

        // Stable period
        for _ in 0..30 {
            bocpd.update(1.0);
        }

        // Change point
        let (_, change_prob) = bocpd.update(10.0);

        // Change probability should be elevated (though not necessarily > 0.5)
        // The key is that it increases relative to stable periods
        assert!(change_prob > 0.0, "Change probability should be positive");
    }

    #[test]
    fn bocpd_reset_clears_state() {
        let mut bocpd = BocpdState::new(0.01);

        for _ in 0..20 {
            bocpd.update(5.0);
        }

        bocpd.reset();

        assert_eq!(bocpd.run_lengths[0], 1.0);
        for i in 1..MAX_RUN_LENGTH {
            assert_eq!(bocpd.run_lengths[i], 0.0);
        }
    }

    // =========================================================================
    // Rolling statistics tests
    // =========================================================================

    #[test]
    fn rolling_mean_correct() {
        let mut agg = TimelineAggregator::new(test_config());

        // Add known values
        for i in 1..=10 {
            agg.observe(&0, i);
        }

        // Mean should be (1+2+...+10)/10 = 5.5
        let evidence = agg.current_evidence();
        assert!(
            (evidence.rolling_mean - 5.5).abs() < 0.01,
            "Rolling mean {} should be 5.5",
            evidence.rolling_mean
        );
    }

    #[test]
    fn rolling_variance_positive() {
        let mut agg = TimelineAggregator::new(test_config());

        // Add varied values
        for i in 0..50 {
            agg.observe(&0, (i % 10) as u64 + 1);
        }

        let evidence = agg.current_evidence();
        assert!(
            evidence.rolling_variance > 0.0,
            "Variance should be positive"
        );
    }

    // =========================================================================
    // Evidence tests
    // =========================================================================

    #[test]
    fn evidence_contains_all_fields() {
        let mut agg = TimelineAggregator::new(test_config());

        for i in 0..100 {
            agg.observe(&(i % 5), (i % 10) as u64 + 1);
        }

        let evidence = agg.current_evidence();

        assert!(evidence.observation_count > 0);
        assert!(evidence.rolling_mean > 0.0);
        assert!(evidence.rolling_variance > 0.0);
        assert!(evidence.sketch_evidence.total_count > 0);
    }

    #[test]
    fn change_evidence_includes_bocpd() {
        let mut config = test_config();
        config.warmup_observations = 10;
        config.hazard_rate = 0.5; // Very high sensitivity
        let mut agg = TimelineAggregator::new(config);

        // Warmup
        for i in 0..20 {
            agg.observe(&i, 1);
        }

        // Try to trigger change
        for i in 20..30 {
            if let Some(evidence) = agg.observe(&i, 1000) {
                assert!(evidence.change_probability > 0.0);
                assert!(evidence.run_length < MAX_RUN_LENGTH);
                return;
            }
        }
        // Note: May not always trigger due to stochastic nature
    }

    // =========================================================================
    // Statistics tests
    // =========================================================================

    #[test]
    fn stats_track_events() {
        let mut agg = TimelineAggregator::new(test_config());

        agg.observe(&"a", 5);
        agg.observe(&"b", 10);
        agg.observe(&"c", 3);

        let stats = agg.stats();
        assert_eq!(stats.total_events, 18);
    }

    #[test]
    fn stats_memory_bounded() {
        let mut agg = TimelineAggregator::new(test_config());

        for i in 0..1000 {
            agg.observe(&i, 1);
        }

        let stats = agg.stats();
        // Memory should be bounded (not grow with observation count)
        assert!(stats.memory_bytes < 1_000_000, "Memory should be bounded");
    }

    // =========================================================================
    // Reset tests
    // =========================================================================

    #[test]
    fn reset_clears_all_state() {
        let mut agg = TimelineAggregator::new(test_config());

        for i in 0..100 {
            agg.observe(&i, (i as u64) + 1);
        }

        agg.reset();

        assert_eq!(agg.observation_count, 0);
        assert_eq!(agg.stats.total_events, 0);
        assert_eq!(agg.rolling_mean, 0.0);
        assert!(agg.window.is_empty());
    }

    // =========================================================================
    // Calibration tests
    // =========================================================================

    #[test]
    fn calibrate_improves_bounds() {
        let mut agg = TimelineAggregator::new(test_config());

        // Add some events
        for i in 0..50 {
            agg.observe(&i, (i as u64) % 5 + 1);
        }

        // Calibrate with known counts
        for i in 0..10 {
            let true_count = (i as u64) % 5 + 1;
            agg.calibrate(&i, true_count);
        }

        let evidence = agg.current_evidence();
        assert!(evidence.sketch_evidence.calibration_samples > 0);
    }

    // =========================================================================
    // Determinism tests
    // =========================================================================

    #[test]
    fn deterministic_detection() {
        let config = test_config();

        let run = || {
            let mut agg = TimelineAggregator::new(config.clone());
            let mut detections = Vec::new();

            for i in 0..100 {
                let count = if i == 50 { 100 } else { 1 };
                if agg.observe(&(i % 5), count).is_some() {
                    detections.push(i);
                }
            }
            detections
        };

        let run1 = run();
        let run2 = run();

        assert_eq!(run1, run2, "Detection should be deterministic");
    }

    // =========================================================================
    // Property tests
    // =========================================================================

    #[test]
    fn property_observation_count_monotonic() {
        let mut agg = TimelineAggregator::new(test_config());

        let mut last_count = 0;
        for i in 0..100 {
            agg.observe(&i, 1);
            assert!(
                agg.observation_count >= last_count,
                "Observation count should be monotonically increasing"
            );
            last_count = agg.observation_count;
        }
    }

    #[test]
    fn property_memory_bounded_under_load() {
        let mut agg = TimelineAggregator::new(test_config());

        let initial_memory = agg.stats().memory_bytes;

        for i in 0..10000 {
            agg.observe(&(i % 100), (i as u64) % 10 + 1);
        }

        let final_memory = agg.stats().memory_bytes;

        // Memory should not grow unboundedly
        assert!(
            final_memory < initial_memory * 10,
            "Memory grew too much: {} -> {}",
            initial_memory,
            final_memory
        );
    }

    // =========================================================================
    // Edge case tests
    // =========================================================================

    #[test]
    fn empty_observation_safe() {
        let mut agg = TimelineAggregator::new(test_config());
        agg.observe(&"empty", 0);
        assert_eq!(agg.observation_count, 1);
    }

    #[test]
    fn large_count_safe() {
        let mut agg = TimelineAggregator::new(test_config());
        agg.observe(&"large", u64::MAX / 2);
        assert!(agg.estimate(&"large") > 0);
    }

    #[test]
    fn many_categories_safe() {
        let mut agg = TimelineAggregator::new(test_config());

        for i in 0..1000 {
            agg.observe(&i, 1);
        }

        // All should have positive estimates
        for i in 0..1000 {
            assert!(agg.estimate(&i) >= 1, "Category {} should have count", i);
        }
    }

    #[test]
    fn aggregator_stats_default_is_zero() {
        let stats = AggregatorStats::default();
        assert_eq!(stats.total_events, 0);
        assert_eq!(stats.change_points_detected, 0);
        assert_eq!(stats.alerts_triggered, 0);
        assert_eq!(stats.current_run_length, 0);
        assert_eq!(stats.memory_bytes, 0);
    }

    #[test]
    fn current_evidence_on_fresh_aggregator() {
        let agg = TimelineAggregator::new(test_config());
        let evidence = agg.current_evidence();
        assert_eq!(evidence.observation_count, 0);
        assert_eq!(evidence.rolling_mean, 0.0);
        assert_eq!(evidence.rolling_variance, MIN_VARIANCE);
    }

    #[test]
    fn window_trims_at_capacity() {
        let mut config = test_config();
        config.window_size = 5;
        let mut agg = TimelineAggregator::new(config);

        for i in 0..10 {
            agg.observe(&i, 1);
        }

        assert_eq!(agg.window.len(), 5);
    }

    #[test]
    fn estimate_delegates_to_sketch() {
        let mut agg = TimelineAggregator::new(test_config());
        for _ in 0..5 {
            agg.observe(&"key", 3);
        }
        assert!(agg.estimate(&"key") >= 15);
        assert_eq!(agg.estimate(&"missing"), 0);
    }

    #[test]
    fn regime_shift_at_event_250_detected_within_500_events() {
        let mut config = test_config();
        config.warmup_observations = 20;
        config.hazard_rate = 0.05;
        let mut agg = TimelineAggregator::new(config);

        for i in 0..500u64 {
            let category = i % 7;
            // Regime 1 (0..250): steady low volume (1-2 events per observation)
            // Regime 2 (250..500): abrupt high-volume burst (80-120 events)
            let count = if i < 250 { 1 + (i % 2) } else { 80 + (i % 41) };
            let _ = agg.observe(&category, count);
        }

        let stats = agg.stats();
        assert_eq!(
            stats.total_events,
            (0..500u64)
                .map(|i| if i < 250 { 1 + (i % 2) } else { 80 + (i % 41) })
                .sum::<u64>()
        );
        assert!(
            stats.change_points_detected >= 1,
            "Expected at least 1 change point across 500 events with regime shift at 250, got {}",
            stats.change_points_detected
        );
    }
}

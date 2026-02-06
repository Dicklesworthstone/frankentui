//! Bayesian height prediction with conformal bounds for virtualized lists.
//!
//! Predicts unseen row heights to pre-allocate scroll space and avoid
//! scroll jumps when actual heights are measured lazily.
//!
//! # Mathematical Model
//!
//! ## Bayesian Online Estimation
//!
//! Maintains a Normal-Normal conjugate model per item category:
//!
//! ```text
//! Prior:     μ ~ N(μ₀, σ₀²/κ₀)
//! Likelihood: h_i ~ N(μ, σ²)
//! Posterior:  μ | data ~ N(μ_n, σ²/κ_n)
//!
//! where:
//!   κ_n = κ₀ + n
//!   μ_n = (κ₀·μ₀ + n·x̄) / κ_n
//!   σ²  estimated via running variance (Welford's algorithm)
//! ```
//!
//! ## Conformal Prediction Bounds
//!
//! Given a calibration set of (predicted, actual) residuals, the conformal
//! interval is:
//!
//! ```text
//! [μ_n - q_{1-α/2}, μ_n + q_{1-α/2}]
//! ```
//!
//! where `q` is the empirical quantile of |residuals|. This provides
//! distribution-free coverage: P(h ∈ interval) ≥ 1 - α.
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | No measurements | Return default height | Cold start fallback |
//! | n = 1 | Wide interval (use prior σ) | Insufficient data |
//! | All same height | σ → 0, interval collapses | Homogeneous data |
//! | Actual > bound | Adjust + record violation | Expected at rate α |

use std::collections::VecDeque;

/// Configuration for the height predictor.
#[derive(Debug, Clone)]
pub struct PredictorConfig {
    /// Default height when no data is available.
    pub default_height: u16,
    /// Prior strength κ₀ (higher = more trust in default). Default: 2.0.
    pub prior_strength: f64,
    /// Prior mean μ₀ (usually same as default_height).
    pub prior_mean: f64,
    /// Prior variance estimate. Default: 4.0.
    pub prior_variance: f64,
    /// Conformal coverage level (1 - α). Default: 0.90.
    pub coverage: f64,
    /// Max calibration residuals to keep. Default: 200.
    pub calibration_window: usize,
}

impl Default for PredictorConfig {
    fn default() -> Self {
        Self {
            default_height: 1,
            prior_strength: 2.0,
            prior_mean: 1.0,
            prior_variance: 4.0,
            coverage: 0.90,
            calibration_window: 200,
        }
    }
}

/// Running statistics using Welford's online algorithm.
#[derive(Debug, Clone)]
struct WelfordStats {
    n: u64,
    mean: f64,
    m2: f64, // Sum of squared deviations
}

impl WelfordStats {
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
            return f64::MAX;
        }
        self.m2 / (self.n - 1) as f64
    }
}

/// Per-category prediction state.
#[derive(Debug, Clone)]
struct CategoryState {
    /// Welford running stats for observed heights.
    welford: WelfordStats,
    /// Posterior mean μ_n.
    posterior_mean: f64,
    /// Posterior κ_n.
    posterior_kappa: f64,
    /// Calibration residuals |predicted - actual|.
    residuals: VecDeque<f64>,
}

/// A prediction with conformal bounds.
#[derive(Debug, Clone, Copy)]
pub struct HeightPrediction {
    /// Point prediction (posterior mean, rounded).
    pub predicted: u16,
    /// Lower conformal bound.
    pub lower: u16,
    /// Upper conformal bound.
    pub upper: u16,
    /// Number of observations for this category.
    pub observations: u64,
}

/// Bayesian height predictor with conformal bounds.
#[derive(Debug, Clone)]
pub struct HeightPredictor {
    config: PredictorConfig,
    /// Per-category states. Key is category index (0 = default).
    categories: Vec<CategoryState>,
    /// Total measurements across all categories.
    total_measurements: u64,
    /// Total bound violations.
    total_violations: u64,
}

impl HeightPredictor {
    /// Create a new predictor with default config.
    pub fn new(config: PredictorConfig) -> Self {
        // Start with one default category.
        let default_cat = CategoryState {
            welford: WelfordStats::new(),
            posterior_mean: config.prior_mean,
            posterior_kappa: config.prior_strength,
            residuals: VecDeque::new(),
        };
        Self {
            config,
            categories: vec![default_cat],
            total_measurements: 0,
            total_violations: 0,
        }
    }

    /// Register a new category. Returns the category id.
    pub fn register_category(&mut self) -> usize {
        let id = self.categories.len();
        self.categories.push(CategoryState {
            welford: WelfordStats::new(),
            posterior_mean: self.config.prior_mean,
            posterior_kappa: self.config.prior_strength,
            residuals: VecDeque::new(),
        });
        id
    }

    /// Predict height for an item in the given category.
    pub fn predict(&self, category: usize) -> HeightPrediction {
        let cat = match self.categories.get(category) {
            Some(c) => c,
            None => return self.cold_prediction(),
        };

        if cat.welford.n == 0 {
            return self.cold_prediction();
        }

        let mu = cat.posterior_mean;
        let predicted = mu.round().max(1.0) as u16;

        // Conformal bounds from calibration residuals.
        let (lower, upper) = self.conformal_bounds(cat, mu);

        HeightPrediction {
            predicted,
            lower,
            upper,
            observations: cat.welford.n,
        }
    }

    /// Record an actual measured height, updating the model.
    /// Returns whether the measurement was within the predicted bounds.
    pub fn observe(&mut self, category: usize, actual_height: u16) -> bool {
        // Ensure category exists.
        while self.categories.len() <= category {
            self.register_category();
        }

        let prediction = self.predict(category);
        let within_bounds = actual_height >= prediction.lower && actual_height <= prediction.upper;

        self.total_measurements += 1;
        if !within_bounds && prediction.observations > 0 {
            self.total_violations += 1;
        }

        let cat = &mut self.categories[category];
        let h = actual_height as f64;

        // Record calibration residual.
        let residual = (cat.posterior_mean - h).abs();
        cat.residuals.push_back(residual);
        if cat.residuals.len() > self.config.calibration_window {
            cat.residuals.pop_front();
        }

        // Update Welford stats.
        cat.welford.update(h);

        // Update posterior: μ_n = (κ₀·μ₀ + n·x̄) / κ_n
        let n = cat.welford.n as f64;
        let kappa_0 = self.config.prior_strength;
        let mu_0 = self.config.prior_mean;
        cat.posterior_kappa = kappa_0 + n;
        cat.posterior_mean = (kappa_0 * mu_0 + n * cat.welford.mean) / cat.posterior_kappa;

        within_bounds
    }

    /// Cold-start prediction when no data is available.
    fn cold_prediction(&self) -> HeightPrediction {
        let d = self.config.default_height;
        let margin = (self.config.prior_variance.sqrt() * 2.0).ceil() as u16;
        HeightPrediction {
            predicted: d,
            lower: d.saturating_sub(margin),
            upper: d.saturating_add(margin),
            observations: 0,
        }
    }

    /// Compute conformal bounds from calibration residuals.
    fn conformal_bounds(&self, cat: &CategoryState, mu: f64) -> (u16, u16) {
        if cat.residuals.is_empty() {
            // Fallback: use prior variance.
            let margin = (self.config.prior_variance.sqrt() * 2.0).ceil() as u16;
            let predicted = mu.round().max(1.0) as u16;
            return (
                predicted.saturating_sub(margin),
                predicted.saturating_add(margin),
            );
        }

        // Sort residuals to find quantile.
        let mut sorted: Vec<f64> = cat.residuals.iter().copied().collect();
        sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let alpha = 1.0 - self.config.coverage;
        let quantile_idx = ((1.0 - alpha) * sorted.len() as f64).ceil() as usize;
        let quantile_idx = quantile_idx.min(sorted.len()).saturating_sub(1);
        let q = sorted[quantile_idx];

        let lower = (mu - q).max(1.0).floor() as u16;
        let upper = (mu + q).ceil().max(1.0) as u16;

        (lower, upper)
    }

    /// Get the posterior mean for a category.
    pub fn posterior_mean(&self, category: usize) -> f64 {
        self.categories
            .get(category)
            .map(|c| c.posterior_mean)
            .unwrap_or(self.config.prior_mean)
    }

    /// Get the posterior variance for a category.
    pub fn posterior_variance(&self, category: usize) -> f64 {
        self.categories
            .get(category)
            .map(|c| {
                let sigma_sq = if c.welford.n < 2 {
                    self.config.prior_variance
                } else {
                    c.welford.variance()
                };
                sigma_sq / c.posterior_kappa
            })
            .unwrap_or(self.config.prior_variance)
    }

    /// Total measurements observed.
    pub fn total_measurements(&self) -> u64 {
        self.total_measurements
    }

    /// Total bound violations.
    pub fn total_violations(&self) -> u64 {
        self.total_violations
    }

    /// Empirical violation rate.
    pub fn violation_rate(&self) -> f64 {
        if self.total_measurements == 0 {
            return 0.0;
        }
        self.total_violations as f64 / self.total_measurements as f64
    }

    /// Number of categories.
    pub fn category_count(&self) -> usize {
        self.categories.len()
    }

    /// Number of observations for a category.
    pub fn category_observations(&self, category: usize) -> u64 {
        self.categories
            .get(category)
            .map(|c| c.welford.n)
            .unwrap_or(0)
    }
}

impl Default for HeightPredictor {
    fn default() -> Self {
        Self::new(PredictorConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Posterior update tests ────────────────────────────────────

    #[test]
    fn unit_posterior_update() {
        let config = PredictorConfig {
            prior_mean: 2.0,
            prior_strength: 1.0,
            prior_variance: 4.0,
            ..Default::default()
        };
        let mut pred = HeightPredictor::new(config);

        // Prior: μ=2.0, κ=1.
        assert!((pred.posterior_mean(0) - 2.0).abs() < 1e-10);

        // Observe height 4.
        pred.observe(0, 4);
        // κ_1 = 1 + 1 = 2, μ_1 = (1*2 + 1*4) / 2 = 3.0
        assert!((pred.posterior_mean(0) - 3.0).abs() < 1e-10);

        // Observe another height 4.
        pred.observe(0, 4);
        // κ_2 = 1 + 2 = 3, x̄ = 4, μ_2 = (1*2 + 2*4) / 3 = 10/3 ≈ 3.333
        assert!((pred.posterior_mean(0) - 10.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn unit_posterior_variance_decreases() {
        let mut pred = HeightPredictor::new(PredictorConfig {
            prior_variance: 4.0,
            ..Default::default()
        });

        let var_0 = pred.posterior_variance(0);
        assert!(var_0 > 0.0, "prior variance should be positive");

        // Feed noisy data so Welford variance is non-zero.
        for i in 0..10 {
            pred.observe(0, if i % 2 == 0 { 2 } else { 4 });
        }
        let var_10 = pred.posterior_variance(0);

        for i in 0..90 {
            pred.observe(0, if i % 2 == 0 { 2 } else { 4 });
        }
        let var_100 = pred.posterior_variance(0);

        // With noisy data, posterior variance σ²/κ_n decreases as κ_n grows.
        assert!(
            var_10 < var_0,
            "variance should decrease: {var_10} >= {var_0}"
        );
        assert!(
            var_100 < var_10,
            "variance should decrease: {var_100} >= {var_10}"
        );
    }

    // ─── Conformal bounds tests ───────────────────────────────────

    #[test]
    fn unit_conformal_bounds() {
        let config = PredictorConfig {
            coverage: 0.90,
            prior_mean: 3.0,
            prior_strength: 1.0,
            ..Default::default()
        };
        let mut pred = HeightPredictor::new(config);

        // Feed consistent data.
        for _ in 0..50 {
            pred.observe(0, 3);
        }

        let p = pred.predict(0);
        // With all observations at 3, residuals should be near 0.
        // Bounds should be tight around 3.
        assert_eq!(p.predicted, 3);
        assert!(p.lower <= 3);
        assert!(p.upper >= 3);
    }

    #[test]
    fn conformal_bounds_widen_with_noise() {
        let config = PredictorConfig {
            coverage: 0.90,
            prior_mean: 5.0,
            prior_strength: 1.0,
            ..Default::default()
        };
        let mut pred = HeightPredictor::new(config);

        // Consistent data → tight bounds.
        for _ in 0..50 {
            pred.observe(0, 5);
        }
        let tight = pred.predict(0);

        // Reset with noisy data.
        let mut pred2 = HeightPredictor::new(PredictorConfig {
            coverage: 0.90,
            prior_mean: 5.0,
            prior_strength: 1.0,
            ..Default::default()
        });
        let mut seed: u64 = 0xABCD_1234_5678_9ABC;
        for _ in 0..50 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let h = 3 + (seed >> 62) as u16; // heights 3..6
            pred2.observe(0, h);
        }
        let wide = pred2.predict(0);

        assert!(
            (wide.upper - wide.lower) >= (tight.upper - tight.lower),
            "noisy data should produce wider bounds"
        );
    }

    // ─── Coverage property test ───────────────────────────────────

    #[test]
    fn property_coverage() {
        let alpha = 0.10;
        let config = PredictorConfig {
            coverage: 1.0 - alpha,
            prior_mean: 3.0,
            prior_strength: 2.0,
            prior_variance: 4.0,
            calibration_window: 100,
            ..Default::default()
        };
        let mut pred = HeightPredictor::new(config);

        // Warm up with calibration data.
        let mut seed: u64 = 0xDEAD_BEEF_CAFE_0001;
        for _ in 0..100 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let h = 2 + (seed >> 62) as u16; // heights 2..5
            pred.observe(0, h);
        }

        // Now check coverage on new data.
        let mut violations = 0u32;
        let test_n = 200;
        for _ in 0..test_n {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let h = 2 + (seed >> 62) as u16;
            let within = pred.observe(0, h);
            if !within {
                violations += 1;
            }
        }

        let viol_rate = violations as f64 / test_n as f64;
        // Empirical violation rate should be approximately ≤ α.
        // Allow generous tolerance for finite sample + discrete heights.
        assert!(
            viol_rate <= alpha + 0.15,
            "violation rate {viol_rate} exceeds α + tolerance ({alpha} + 0.15)"
        );
    }

    // ─── Scroll stability test ────────────────────────────────────

    #[test]
    fn e2e_scroll_stability() {
        let mut pred = HeightPredictor::new(PredictorConfig {
            prior_mean: 1.0,
            prior_strength: 2.0,
            default_height: 1,
            coverage: 0.90,
            ..Default::default()
        });

        // All items are height 1 (most common TUI case).
        let mut corrections = 0u32;
        for _ in 0..500 {
            let within = pred.observe(0, 1);
            if !within {
                corrections += 1;
            }
        }

        // With homogeneous heights, should converge quickly with zero corrections
        // after warmup.
        let p = pred.predict(0);
        assert_eq!(p.predicted, 1);
        assert!(corrections < 10, "too many corrections: {corrections}");
    }

    // ─── Multiple categories ──────────────────────────────────────

    #[test]
    fn categories_are_independent() {
        let mut pred = HeightPredictor::default();
        let cat_a = 0;
        let cat_b = pred.register_category();

        // Feed different data to each.
        for _ in 0..20 {
            pred.observe(cat_a, 1);
            pred.observe(cat_b, 5);
        }

        let pa = pred.predict(cat_a);
        let pb = pred.predict(cat_b);

        assert_eq!(pa.predicted, 1);
        assert!(pb.predicted >= 4 && pb.predicted <= 5);
    }

    // ─── Cold start ───────────────────────────────────────────────

    #[test]
    fn cold_prediction_uses_default() {
        let pred = HeightPredictor::new(PredictorConfig {
            default_height: 2,
            prior_variance: 1.0,
            ..Default::default()
        });
        let p = pred.predict(0);
        assert_eq!(p.predicted, 2);
        assert_eq!(p.observations, 0);
    }

    // ─── Determinism ──────────────────────────────────────────────

    #[test]
    fn deterministic_under_same_observations() {
        let run = || {
            let mut pred = HeightPredictor::default();
            let observations = [1, 2, 1, 3, 1, 2, 1, 1, 4, 1];
            for &h in &observations {
                pred.observe(0, h);
            }
            (pred.predict(0).predicted, pred.posterior_mean(0))
        };

        let (p1, m1) = run();
        let (p2, m2) = run();
        assert_eq!(p1, p2);
        assert!((m1 - m2).abs() < 1e-15);
    }

    // ─── Performance ──────────────────────────────────────────────

    #[test]
    fn perf_prediction_overhead() {
        let mut pred = HeightPredictor::default();

        // Warm up.
        for _ in 0..100 {
            pred.observe(0, 2);
        }

        let start = std::time::Instant::now();
        let mut _sink = 0u16;
        for _ in 0..100_000 {
            _sink = _sink.wrapping_add(pred.predict(0).predicted);
        }
        let elapsed = start.elapsed();
        let per_prediction = elapsed / 100_000;

        // Must be < 5μs per prediction (generous for debug builds).
        assert!(
            per_prediction < std::time::Duration::from_micros(5),
            "prediction too slow: {per_prediction:?}"
        );
    }

    // ─── Violation tracking ───────────────────────────────────────

    #[test]
    fn violation_tracking() {
        let mut pred = HeightPredictor::new(PredictorConfig {
            prior_mean: 5.0,
            prior_strength: 100.0, // strong prior
            default_height: 5,
            coverage: 0.95,
            ..Default::default()
        });

        // Warm up with height=5.
        for _ in 0..50 {
            pred.observe(0, 5);
        }

        // Sudden jump to height=20 should violate bounds.
        let within = pred.observe(0, 20);
        assert!(!within, "extreme outlier should violate bounds");
        assert!(pred.total_violations() > 0);
    }

    // ── PredictorConfig defaults ─────────────────────────────────

    #[test]
    fn config_default_values() {
        let config = PredictorConfig::default();
        assert_eq!(config.default_height, 1);
        assert!((config.prior_strength - 2.0).abs() < f64::EPSILON);
        assert!((config.prior_mean - 1.0).abs() < f64::EPSILON);
        assert!((config.prior_variance - 4.0).abs() < f64::EPSILON);
        assert!((config.coverage - 0.90).abs() < f64::EPSILON);
        assert_eq!(config.calibration_window, 200);
    }

    // ── HeightPredictor::default ─────────────────────────────────

    #[test]
    fn default_predictor_has_one_category() {
        let pred = HeightPredictor::default();
        assert_eq!(pred.category_count(), 1);
        assert_eq!(pred.total_measurements(), 0);
        assert_eq!(pred.total_violations(), 0);
        assert!((pred.violation_rate() - 0.0).abs() < f64::EPSILON);
    }

    // ── Predict unknown category ─────────────────────────────────

    #[test]
    fn predict_unknown_category_returns_cold() {
        let pred = HeightPredictor::default();
        let p = pred.predict(999);
        assert_eq!(p.predicted, pred.config.default_height);
        assert_eq!(p.observations, 0);
    }

    // ── Observe auto-creates categories ──────────────────────────

    #[test]
    fn observe_auto_creates_categories() {
        let mut pred = HeightPredictor::default();
        assert_eq!(pred.category_count(), 1);
        pred.observe(3, 5);
        // Should auto-create categories 1, 2, 3
        assert_eq!(pred.category_count(), 4);
        assert_eq!(pred.category_observations(3), 1);
    }

    // ── Violation rate ───────────────────────────────────────────

    #[test]
    fn violation_rate_empty() {
        let pred = HeightPredictor::default();
        assert!((pred.violation_rate() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn violation_rate_computation() {
        let mut pred = HeightPredictor::new(PredictorConfig {
            prior_mean: 5.0,
            prior_strength: 100.0,
            default_height: 5,
            coverage: 0.95,
            ..Default::default()
        });
        // Warm up so bounds are tight
        for _ in 0..50 {
            pred.observe(0, 5);
        }
        // 10 normal observations
        for _ in 0..10 {
            pred.observe(0, 5);
        }
        let before_violations = pred.total_violations();
        // 1 extreme outlier
        pred.observe(0, 100);
        let after_violations = pred.total_violations();
        assert!(after_violations > before_violations);
        assert!(pred.violation_rate() > 0.0);
    }

    // ── Category accessors ───────────────────────────────────────

    #[test]
    fn category_observations_returns_zero_for_unknown() {
        let pred = HeightPredictor::default();
        assert_eq!(pred.category_observations(999), 0);
    }

    #[test]
    fn category_observations_tracks_counts() {
        let mut pred = HeightPredictor::default();
        pred.observe(0, 3);
        pred.observe(0, 4);
        pred.observe(0, 5);
        assert_eq!(pred.category_observations(0), 3);
    }

    // ── Posterior accessors with unknown category ─────────────────

    #[test]
    fn posterior_mean_unknown_returns_prior() {
        let pred = HeightPredictor::default();
        assert!((pred.posterior_mean(999) - pred.config.prior_mean).abs() < f64::EPSILON);
    }

    #[test]
    fn posterior_variance_unknown_returns_prior() {
        let pred = HeightPredictor::default();
        assert!(
            (pred.posterior_variance(999) - pred.config.prior_variance).abs() < f64::EPSILON
        );
    }

    // ── Register category ────────────────────────────────────────

    #[test]
    fn register_category_returns_sequential_ids() {
        let mut pred = HeightPredictor::default();
        let id1 = pred.register_category();
        let id2 = pred.register_category();
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(pred.category_count(), 3);
    }

    // ── Observe returns within_bounds ─────────────────────────────

    #[test]
    fn observe_returns_true_for_consistent_data() {
        let mut pred = HeightPredictor::new(PredictorConfig {
            prior_mean: 3.0,
            prior_strength: 1.0,
            ..Default::default()
        });
        // Warm up
        for _ in 0..20 {
            pred.observe(0, 3);
        }
        // Same value should be within bounds
        assert!(pred.observe(0, 3));
    }

    // ── Total measurements ───────────────────────────────────────

    #[test]
    fn total_measurements_increments() {
        let mut pred = HeightPredictor::default();
        for i in 0..7 {
            pred.observe(0, (i + 1) as u16);
        }
        assert_eq!(pred.total_measurements(), 7);
    }

    // ── HeightPrediction bounds ordering ─────────────────────────

    #[test]
    fn prediction_lower_le_predicted_le_upper() {
        let mut pred = HeightPredictor::default();
        for _ in 0..30 {
            pred.observe(0, 3);
        }
        let p = pred.predict(0);
        assert!(p.lower <= p.predicted);
        assert!(p.predicted <= p.upper);
    }
}

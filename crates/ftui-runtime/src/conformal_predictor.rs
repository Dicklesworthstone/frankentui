#![forbid(unsafe_code)]

//! Conformal predictor for frame-time risk (bd-3e1t.3.2).
//!
//! This module provides a distribution-free upper bound on frame time using
//! Mondrian (bucketed) conformal prediction. It is intentionally lightweight
//! and explainable: each prediction returns the bucket key, quantile, and
//! fallback level used to produce the bound.
//!
//! See docs/spec/state-machines.md section 3.13 for the governing spec.

use std::collections::{HashMap, VecDeque};
use std::fmt;

use ftui_render::diff_strategy::DiffStrategy;

use crate::terminal_writer::ScreenMode;

/// Configuration for conformal frame-time prediction.
#[derive(Debug, Clone)]
pub struct ConformalConfig {
    /// Significance level alpha. Coverage is >= 1 - alpha.
    /// Default: 0.05.
    pub alpha: f64,

    /// Minimum samples required before a bucket is considered valid.
    /// Default: 20.
    pub min_samples: usize,

    /// Maximum samples retained per bucket (rolling window).
    /// Default: 256.
    pub window_size: usize,

    /// Conservative fallback residual (microseconds) when no calibration exists.
    /// Default: 10_000.0 (10ms).
    pub q_default: f64,
}

impl Default for ConformalConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            min_samples: 20,
            window_size: 256,
            q_default: 10_000.0,
        }
    }
}

/// Bucket identifier for conformal calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BucketKey {
    pub mode: ModeBucket,
    pub diff: DiffBucket,
    pub size_bucket: u8,
}

impl BucketKey {
    /// Create a bucket key from rendering context.
    pub fn from_context(
        screen_mode: ScreenMode,
        diff_strategy: DiffStrategy,
        cols: u16,
        rows: u16,
    ) -> Self {
        Self {
            mode: ModeBucket::from_screen_mode(screen_mode),
            diff: DiffBucket::from(diff_strategy),
            size_bucket: size_bucket(cols, rows),
        }
    }
}

/// Mode bucket for conformal calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModeBucket {
    Inline,
    InlineAuto,
    AltScreen,
}

impl ModeBucket {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::InlineAuto => "inline_auto",
            Self::AltScreen => "altscreen",
        }
    }

    pub fn from_screen_mode(mode: ScreenMode) -> Self {
        match mode {
            ScreenMode::Inline { .. } => Self::Inline,
            ScreenMode::InlineAuto { .. } => Self::InlineAuto,
            ScreenMode::AltScreen => Self::AltScreen,
        }
    }
}

/// Diff strategy bucket for conformal calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffBucket {
    Full,
    DirtyRows,
    FullRedraw,
}

impl DiffBucket {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::DirtyRows => "dirty",
            Self::FullRedraw => "redraw",
        }
    }
}

impl From<DiffStrategy> for DiffBucket {
    fn from(strategy: DiffStrategy) -> Self {
        match strategy {
            DiffStrategy::Full => Self::Full,
            DiffStrategy::DirtyRows => Self::DirtyRows,
            DiffStrategy::FullRedraw => Self::FullRedraw,
        }
    }
}

impl fmt::Display for BucketKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.mode.as_str(),
            self.diff.as_str(),
            self.size_bucket
        )
    }
}

/// Prediction output with full explainability.
#[derive(Debug, Clone)]
pub struct ConformalPrediction {
    /// Upper bound on frame time (microseconds).
    pub upper_us: f64,
    /// Whether the bound exceeds the current budget.
    pub risk: bool,
    /// Coverage confidence (1 - alpha).
    pub confidence: f64,
    /// Bucket key used for calibration (may be fallback aggregate).
    pub bucket: BucketKey,
    /// Calibration sample count used for the quantile.
    pub sample_count: usize,
    /// Conformal quantile q_b.
    pub quantile: f64,
    /// Fallback level (0 = exact, 1 = mode+diff, 2 = mode, 3 = global/default).
    pub fallback_level: u8,
    /// Rolling window size.
    pub window_size: usize,
    /// Total reset count for this predictor.
    pub reset_count: u64,
    /// Base prediction f(x_t).
    pub y_hat: f64,
    /// Frame budget in microseconds.
    pub budget_us: f64,
}

impl ConformalPrediction {
    /// Format this prediction as a JSONL line for structured logging.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        format!(
            r#"{{"schema":"conformal-v1","upper_us":{:.1},"risk":{},"confidence":{:.4},"bucket":"{}","samples":{},"quantile":{:.2},"fallback_level":{},"window":{},"resets":{},"y_hat":{:.1},"budget_us":{:.1}}}"#,
            self.upper_us,
            self.risk,
            self.confidence,
            self.bucket,
            self.sample_count,
            self.quantile,
            self.fallback_level,
            self.window_size,
            self.reset_count,
            self.y_hat,
            self.budget_us,
        )
    }
}

/// Update metadata after observing a frame.
#[derive(Debug, Clone)]
pub struct ConformalUpdate {
    /// Residual (y_t - f(x_t)).
    pub residual: f64,
    /// Bucket updated.
    pub bucket: BucketKey,
    /// New sample count in the bucket.
    pub sample_count: usize,
}

#[derive(Debug, Default)]
struct BucketState {
    residuals: VecDeque<f64>,
}

impl BucketState {
    fn push(&mut self, residual: f64, window_size: usize) {
        self.residuals.push_back(residual);
        while self.residuals.len() > window_size {
            self.residuals.pop_front();
        }
    }
}

/// Conformal predictor with bucketed calibration.
#[derive(Debug)]
pub struct ConformalPredictor {
    config: ConformalConfig,
    buckets: HashMap<BucketKey, BucketState>,
    reset_count: u64,
}

impl ConformalPredictor {
    /// Create a new predictor with the given config.
    pub fn new(config: ConformalConfig) -> Self {
        Self {
            config,
            buckets: HashMap::new(),
            reset_count: 0,
        }
    }

    /// Access the configuration.
    pub fn config(&self) -> &ConformalConfig {
        &self.config
    }

    /// Number of samples currently stored for a bucket.
    pub fn bucket_samples(&self, key: BucketKey) -> usize {
        self.buckets
            .get(&key)
            .map(|state| state.residuals.len())
            .unwrap_or(0)
    }

    /// Clear calibration for all buckets.
    pub fn reset_all(&mut self) {
        self.buckets.clear();
        self.reset_count += 1;
    }

    /// Clear calibration for a single bucket.
    pub fn reset_bucket(&mut self, key: BucketKey) {
        if let Some(state) = self.buckets.get_mut(&key) {
            state.residuals.clear();
            self.reset_count += 1;
        }
    }

    /// Observe a realized frame time and update calibration.
    pub fn observe(&mut self, key: BucketKey, y_hat_us: f64, observed_us: f64) -> ConformalUpdate {
        let residual = observed_us - y_hat_us;
        if !residual.is_finite() {
            return ConformalUpdate {
                residual,
                bucket: key,
                sample_count: self.bucket_samples(key),
            };
        }

        let window_size = self.config.window_size.max(1);
        let state = self.buckets.entry(key).or_default();
        state.push(residual, window_size);
        ConformalUpdate {
            residual,
            bucket: key,
            sample_count: state.residuals.len(),
        }
    }

    /// Predict a conservative upper bound for frame time.
    pub fn predict(&self, key: BucketKey, y_hat_us: f64, budget_us: f64) -> ConformalPrediction {
        let span = tracing::info_span!(
            "conformal.predict",
            calibration_set_size = tracing::field::Empty,
            predicted_upper_bound_us = tracing::field::Empty,
            frame_budget_us = budget_us,
            coverage_alpha = self.config.alpha,
            gate_triggered = tracing::field::Empty,
        );
        let _guard = span.enter();

        let QuantileDecision {
            quantile,
            sample_count,
            fallback_level,
        } = self.quantile_for(key);

        let upper_us = y_hat_us + quantile.max(0.0);
        let risk = upper_us > budget_us;

        span.record("calibration_set_size", sample_count);
        span.record("predicted_upper_bound_us", upper_us);
        span.record("gate_triggered", risk);

        tracing::debug!(
            bucket = %key,
            y_hat_us,
            quantile,
            interval_width_us = quantile.max(0.0),
            fallback_level,
            sample_count,
            "prediction interval"
        );

        ConformalPrediction {
            upper_us,
            risk,
            confidence: 1.0 - self.config.alpha,
            bucket: key,
            sample_count,
            quantile,
            fallback_level,
            window_size: self.config.window_size,
            reset_count: self.reset_count,
            y_hat: y_hat_us,
            budget_us,
        }
    }

    fn quantile_for(&self, key: BucketKey) -> QuantileDecision {
        let min_samples = self.config.min_samples.max(1);

        let exact = self.collect_exact(key);
        if exact.len() >= min_samples {
            return QuantileDecision::new(self.config.alpha, exact, 0);
        }

        let mode_diff = self.collect_mode_diff(key.mode, key.diff);
        if mode_diff.len() >= min_samples {
            return QuantileDecision::new(self.config.alpha, mode_diff, 1);
        }

        let mode_only = self.collect_mode(key.mode);
        if mode_only.len() >= min_samples {
            return QuantileDecision::new(self.config.alpha, mode_only, 2);
        }

        let global = self.collect_all();
        if !global.is_empty() {
            return QuantileDecision::new(self.config.alpha, global, 3);
        }

        QuantileDecision {
            quantile: self.config.q_default,
            sample_count: 0,
            fallback_level: 3,
        }
    }

    fn collect_exact(&self, key: BucketKey) -> Vec<f64> {
        self.buckets
            .get(&key)
            .map(|state| state.residuals.iter().copied().collect())
            .unwrap_or_default()
    }

    fn collect_mode_diff(&self, mode: ModeBucket, diff: DiffBucket) -> Vec<f64> {
        let mut values = Vec::new();
        for (key, state) in &self.buckets {
            if key.mode == mode && key.diff == diff {
                values.extend(state.residuals.iter().copied());
            }
        }
        values
    }

    fn collect_mode(&self, mode: ModeBucket) -> Vec<f64> {
        let mut values = Vec::new();
        for (key, state) in &self.buckets {
            if key.mode == mode {
                values.extend(state.residuals.iter().copied());
            }
        }
        values
    }

    fn collect_all(&self) -> Vec<f64> {
        let mut values = Vec::new();
        for state in self.buckets.values() {
            values.extend(state.residuals.iter().copied());
        }
        values
    }
}

#[derive(Debug)]
struct QuantileDecision {
    quantile: f64,
    sample_count: usize,
    fallback_level: u8,
}

impl QuantileDecision {
    fn new(alpha: f64, mut residuals: Vec<f64>, fallback_level: u8) -> Self {
        let quantile = conformal_quantile(alpha, &mut residuals);
        Self {
            quantile,
            sample_count: residuals.len(),
            fallback_level,
        }
    }
}

fn conformal_quantile(alpha: f64, residuals: &mut [f64]) -> f64 {
    if residuals.is_empty() {
        return 0.0;
    }
    residuals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = residuals.len();
    let rank = ((n as f64 + 1.0) * (1.0 - alpha)).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    residuals[idx]
}

fn size_bucket(cols: u16, rows: u16) -> u8 {
    let area = cols as u32 * rows as u32;
    if area == 0 {
        return 0;
    }
    (31 - area.leading_zeros()) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(cols: u16, rows: u16) -> BucketKey {
        BucketKey::from_context(
            ScreenMode::Inline { ui_height: 4 },
            DiffStrategy::Full,
            cols,
            rows,
        )
    }

    #[test]
    fn quantile_n_plus_1_rule() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.2,
            min_samples: 1,
            window_size: 10,
            q_default: 0.0,
        });

        let key = test_key(80, 24);
        predictor.observe(key, 0.0, 1.0);
        predictor.observe(key, 0.0, 2.0);
        predictor.observe(key, 0.0, 3.0);

        let decision = predictor.predict(key, 0.0, 1_000.0);
        assert_eq!(decision.quantile, 3.0);
    }

    #[test]
    fn fallback_hierarchy_mode_diff() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 4,
            window_size: 16,
            q_default: 0.0,
        });

        let key_a = test_key(80, 24);
        for value in [1.0, 2.0, 3.0, 4.0] {
            predictor.observe(key_a, 0.0, value);
        }

        let key_b = test_key(120, 40);
        let decision = predictor.predict(key_b, 0.0, 1_000.0);
        assert_eq!(decision.fallback_level, 1);
        assert_eq!(decision.sample_count, 4);
    }

    #[test]
    fn fallback_hierarchy_mode_only() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 3,
            window_size: 16,
            q_default: 0.0,
        });

        let key_dirty = BucketKey::from_context(
            ScreenMode::Inline { ui_height: 4 },
            DiffStrategy::DirtyRows,
            80,
            24,
        );
        for value in [10.0, 20.0, 30.0] {
            predictor.observe(key_dirty, 0.0, value);
        }

        let key_full = BucketKey::from_context(
            ScreenMode::Inline { ui_height: 4 },
            DiffStrategy::Full,
            120,
            40,
        );
        let decision = predictor.predict(key_full, 0.0, 1_000.0);
        assert_eq!(decision.fallback_level, 2);
        assert_eq!(decision.sample_count, 3);
    }

    #[test]
    fn window_enforced() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 3,
            q_default: 0.0,
        });
        let key = test_key(80, 24);
        for value in [1.0, 2.0, 3.0, 4.0, 5.0] {
            predictor.observe(key, 0.0, value);
        }
        assert_eq!(predictor.bucket_samples(key), 3);
    }

    #[test]
    fn predict_uses_default_when_empty() {
        let predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 2,
            window_size: 4,
            q_default: 42.0,
        });
        let key = test_key(120, 40);
        let prediction = predictor.predict(key, 5.0, 10_000.0);
        assert_eq!(prediction.quantile, 42.0);
        assert_eq!(prediction.sample_count, 0);
        assert_eq!(prediction.fallback_level, 3);
    }

    #[test]
    fn bucket_isolation_by_size() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.2,
            min_samples: 2,
            window_size: 10,
            q_default: 0.0,
        });

        let small = test_key(40, 10);
        predictor.observe(small, 0.0, 1.0);
        predictor.observe(small, 0.0, 2.0);

        let large = test_key(200, 60);
        predictor.observe(large, 0.0, 10.0);
        predictor.observe(large, 0.0, 12.0);

        let prediction = predictor.predict(large, 0.0, 1_000.0);
        assert_eq!(prediction.fallback_level, 0);
        assert_eq!(prediction.sample_count, 2);
        assert_eq!(prediction.quantile, 12.0);
    }

    #[test]
    fn reset_clears_bucket_and_raises_reset_count() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 7.0,
        });
        let key = test_key(80, 24);
        predictor.observe(key, 0.0, 3.0);
        assert_eq!(predictor.bucket_samples(key), 1);

        predictor.reset_bucket(key);
        assert_eq!(predictor.bucket_samples(key), 0);

        let prediction = predictor.predict(key, 0.0, 1_000.0);
        assert_eq!(prediction.quantile, 7.0);
        assert_eq!(prediction.reset_count, 1);
    }

    #[test]
    fn reset_all_forces_conservative_fallback() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 9.0,
        });
        let key = test_key(80, 24);
        predictor.observe(key, 0.0, 2.0);

        predictor.reset_all();
        let prediction = predictor.predict(key, 0.0, 1_000.0);
        assert_eq!(prediction.quantile, 9.0);
        assert_eq!(prediction.sample_count, 0);
        assert_eq!(prediction.fallback_level, 3);
        assert_eq!(prediction.reset_count, 1);
    }

    #[test]
    fn size_bucket_log2_area() {
        let a = size_bucket(8, 8); // area 64 -> log2 = 6
        let b = size_bucket(8, 16); // area 128 -> log2 = 7
        assert_eq!(a, 6);
        assert_eq!(b, 7);
    }

    // --- size_bucket edge cases ---

    #[test]
    fn size_bucket_zero_area() {
        assert_eq!(size_bucket(0, 0), 0);
        assert_eq!(size_bucket(0, 24), 0);
        assert_eq!(size_bucket(80, 0), 0);
    }

    #[test]
    fn size_bucket_one_by_one() {
        assert_eq!(size_bucket(1, 1), 0); // area 1, log2(1) = 0
    }

    #[test]
    fn size_bucket_typical_terminals() {
        let b80 = size_bucket(80, 24); // 1920 -> log2 ~ 10
        let b120 = size_bucket(120, 40); // 4800 -> log2 ~ 12
        assert_eq!(b80, 10);
        assert_eq!(b120, 12);
    }

    // --- conformal_quantile edge cases ---

    #[test]
    fn conformal_quantile_empty() {
        let mut data: Vec<f64> = vec![];
        assert_eq!(conformal_quantile(0.1, &mut data), 0.0);
    }

    #[test]
    fn conformal_quantile_single_element() {
        let mut data = vec![42.0];
        assert_eq!(conformal_quantile(0.1, &mut data), 42.0);
    }

    #[test]
    fn conformal_quantile_sorted_data() {
        let mut data = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let q = conformal_quantile(0.5, &mut data);
        // (5+1)*0.5 = 3.0 -> ceil = 3 -> idx = 2 -> data[2] = 3.0
        assert_eq!(q, 3.0);
    }

    #[test]
    fn conformal_quantile_alpha_half() {
        let mut data = vec![10.0, 20.0, 30.0, 40.0];
        let q = conformal_quantile(0.5, &mut data);
        // (4+1)*0.5 = 2.5 -> ceil = 3 -> idx = 2 -> data[2] = 30.0
        assert_eq!(q, 30.0);
    }

    // --- ModeBucket / DiffBucket ---

    #[test]
    fn mode_bucket_as_str_all_variants() {
        assert_eq!(ModeBucket::Inline.as_str(), "inline");
        assert_eq!(ModeBucket::InlineAuto.as_str(), "inline_auto");
        assert_eq!(ModeBucket::AltScreen.as_str(), "altscreen");
    }

    #[test]
    fn diff_bucket_as_str_all_variants() {
        assert_eq!(DiffBucket::Full.as_str(), "full");
        assert_eq!(DiffBucket::DirtyRows.as_str(), "dirty");
        assert_eq!(DiffBucket::FullRedraw.as_str(), "redraw");
    }

    #[test]
    fn diff_bucket_from_strategy() {
        assert_eq!(DiffBucket::from(DiffStrategy::Full), DiffBucket::Full);
        assert_eq!(
            DiffBucket::from(DiffStrategy::DirtyRows),
            DiffBucket::DirtyRows
        );
        assert_eq!(
            DiffBucket::from(DiffStrategy::FullRedraw),
            DiffBucket::FullRedraw
        );
    }

    // --- BucketKey Display ---

    #[test]
    fn bucket_key_display_format() {
        let key = BucketKey {
            mode: ModeBucket::AltScreen,
            diff: DiffBucket::DirtyRows,
            size_bucket: 12,
        };
        assert_eq!(format!("{key}"), "altscreen:dirty:12");
    }

    // --- observe edge cases ---

    #[test]
    fn observe_nan_residual_not_stored() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 5.0,
        });
        let key = test_key(80, 24);
        let update = predictor.observe(key, 0.0, f64::NAN);
        assert!(!update.residual.is_finite());
        assert_eq!(predictor.bucket_samples(key), 0);
    }

    #[test]
    fn observe_infinity_residual_not_stored() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 5.0,
        });
        let key = test_key(80, 24);
        predictor.observe(key, 0.0, f64::INFINITY);
        assert_eq!(predictor.bucket_samples(key), 0);
    }

    // --- prediction fields ---

    #[test]
    fn prediction_risk_flag() {
        let predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 50.0,
        });
        let key = test_key(80, 24);
        // No data -> q_default = 50.0, y_hat = 0 -> upper = 50
        let p = predictor.predict(key, 0.0, 100.0);
        assert!(!p.risk); // 50 <= 100
        let p2 = predictor.predict(key, 0.0, 30.0);
        assert!(p2.risk); // 50 > 30
    }

    #[test]
    fn prediction_confidence() {
        let predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.05,
            min_samples: 1,
            window_size: 8,
            q_default: 0.0,
        });
        let key = test_key(80, 24);
        let p = predictor.predict(key, 0.0, 100.0);
        assert!((p.confidence - 0.95).abs() < 1e-10);
    }

    // --- global fallback with data ---

    #[test]
    fn global_fallback_with_data() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 100, // impossibly high -> always fall through
            window_size: 256,
            q_default: 999.0,
        });
        // Use altscreen mode bucket, then query inline
        let alt_key = BucketKey::from_context(ScreenMode::AltScreen, DiffStrategy::Full, 80, 24);
        predictor.observe(alt_key, 0.0, 5.0);

        let inline_key = test_key(80, 24);
        let p = predictor.predict(inline_key, 0.0, 1000.0);
        // Falls all the way to global (level 3), has 1 sample
        assert_eq!(p.fallback_level, 3);
        assert_eq!(p.sample_count, 1);
        assert_eq!(p.quantile, 5.0);
    }

    // --- ModeBucket from_screen_mode ---

    #[test]
    fn mode_bucket_from_screen_modes() {
        assert_eq!(
            ModeBucket::from_screen_mode(ScreenMode::Inline { ui_height: 4 }),
            ModeBucket::Inline
        );
        assert_eq!(
            ModeBucket::from_screen_mode(ScreenMode::InlineAuto {
                min_height: 4,
                max_height: 24
            }),
            ModeBucket::InlineAuto
        );
        assert_eq!(
            ModeBucket::from_screen_mode(ScreenMode::AltScreen),
            ModeBucket::AltScreen
        );
    }

    // --- Config defaults ---

    #[test]
    fn config_defaults() {
        let config = ConformalConfig::default();
        assert!((config.alpha - 0.05).abs() < 1e-10);
        assert_eq!(config.min_samples, 20);
        assert_eq!(config.window_size, 256);
        assert!((config.q_default - 10_000.0).abs() < 1e-10);
    }

    #[test]
    fn predictor_config_accessor() {
        let config = ConformalConfig {
            alpha: 0.2,
            min_samples: 5,
            window_size: 32,
            q_default: 100.0,
        };
        let predictor = ConformalPredictor::new(config);
        assert!((predictor.config().alpha - 0.2).abs() < 1e-10);
        assert_eq!(predictor.config().min_samples, 5);
    }

    // --- negative residuals ---

    #[test]
    fn negative_residual_clamped_in_prediction() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 0.0,
        });
        let key = test_key(80, 24);
        // observed < y_hat -> negative residual
        predictor.observe(key, 10.0, 5.0);
        let p = predictor.predict(key, 10.0, 100.0);
        // quantile is -5.0, but clamped to 0.0 via .max(0.0)
        // so upper_us = 10.0 + 0.0 = 10.0
        assert_eq!(p.upper_us, 10.0);
    }

    // --- ConformalUpdate fields ---

    #[test]
    fn observe_returns_correct_update() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 0.0,
        });
        let key = test_key(80, 24);
        let update = predictor.observe(key, 3.0, 10.0);
        assert!((update.residual - 7.0).abs() < 1e-10);
        assert_eq!(update.bucket, key);
        assert_eq!(update.sample_count, 1);
    }

    // --- prediction y_hat and budget fields ---

    #[test]
    fn prediction_preserves_yhat_and_budget() {
        let predictor = ConformalPredictor::new(ConformalConfig::default());
        let key = test_key(80, 24);
        let p = predictor.predict(key, 42.5, 16666.0);
        assert!((p.y_hat - 42.5).abs() < 1e-10);
        assert!((p.budget_us - 16666.0).abs() < 1e-10);
    }

    // --- tracing span verification ---

    #[test]
    fn predict_emits_conformal_predict_span() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct SpanChecker {
            saw_conformal_predict: Arc<AtomicBool>,
        }

        impl tracing::Subscriber for SpanChecker {
            fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                if span.metadata().name() == "conformal.predict" {
                    self.saw_conformal_predict.store(true, Ordering::Relaxed);
                }
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {
            }
            fn event(&self, _event: &tracing::Event<'_>) {}
            fn enter(&self, _span: &tracing::span::Id) {}
            fn exit(&self, _span: &tracing::span::Id) {}
        }

        let saw_it = Arc::new(AtomicBool::new(false));
        let subscriber = SpanChecker {
            saw_conformal_predict: Arc::clone(&saw_it),
        };
        let _guard = tracing::subscriber::set_default(subscriber);

        let predictor = ConformalPredictor::new(ConformalConfig::default());
        let key = test_key(80, 24);
        let _ = predictor.predict(key, 100.0, 16666.0);

        assert!(
            saw_it.load(Ordering::Relaxed),
            "predict() must emit a 'conformal.predict' tracing span"
        );
    }

    #[test]
    fn predict_span_records_gate_triggered_true() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct GateChecker {
            saw_gate_true: Arc<AtomicBool>,
        }

        struct GateVisitor(Arc<AtomicBool>);

        impl tracing::field::Visit for GateVisitor {
            fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
                if field.name() == "gate_triggered" && value {
                    self.0.store(true, Ordering::Relaxed);
                }
            }
            fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn fmt::Debug) {}
        }

        impl tracing::Subscriber for GateChecker {
            fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
                let mut visitor = GateVisitor(Arc::clone(&self.saw_gate_true));
                values.record(&mut visitor);
            }
            fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {
            }
            fn event(&self, _event: &tracing::Event<'_>) {}
            fn enter(&self, _span: &tracing::span::Id) {}
            fn exit(&self, _span: &tracing::span::Id) {}
        }

        let saw_gate = Arc::new(AtomicBool::new(false));
        let subscriber = GateChecker {
            saw_gate_true: Arc::clone(&saw_gate),
        };
        let _guard = tracing::subscriber::set_default(subscriber);

        let predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 50_000.0, // large default to guarantee risk
        });
        let key = test_key(80, 24);
        // budget_us = 100 << q_default = 50_000 -> risk = true
        let p = predictor.predict(key, 0.0, 100.0);
        assert!(p.risk, "prediction should be risky");
        assert!(
            saw_gate.load(Ordering::Relaxed),
            "predict() must record gate_triggered=true when risk"
        );
    }
}

#![forbid(unsafe_code)]

//! Conformal predictor for frame-time risk (bd-3e1t.3.2).
//!
//! Finite-sample upper bounds require exchangeable calibration and future
//! residuals in the population being calibrated. Rolling adaptive timings and
//! pooled fallback buckets do not by themselves establish that assumption or
//! conditional coverage for the requested bucket. Missing ranks are explicit;
//! a warmup estimate is never reported as a calibrated bound.
//!
//! See docs/spec/state-machines.md section 3.13 for the governing spec.

use std::collections::{HashMap, VecDeque};
use std::fmt;

use ftui_render::diff_strategy::DiffStrategy;

use crate::terminal_writer::ScreenMode;

/// Configuration for conformal frame-time prediction.
#[derive(Debug, Clone)]
pub struct ConformalConfig {
    /// Significance level alpha. Marginal coverage is >= 1 - alpha only when
    /// calibration and future residuals are exchangeable.
    /// Default: 0.05.
    pub alpha: f64,

    /// Minimum samples before calibration, in addition to an attainable rank.
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

/// Invalid conformal calibration configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConformalConfigError(pub &'static str);

impl fmt::Display for ConformalConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ConformalConfigError {}

impl ConformalConfig {
    /// Validate without silently replacing an invalid policy with defaults.
    pub fn validate(&self) -> Result<(), ConformalConfigError> {
        if !self.alpha.is_finite() || self.alpha <= 0.0 || self.alpha >= 1.0 {
            return Err(ConformalConfigError(
                "alpha must be finite and strictly between zero and one",
            ));
        }
        if self.min_samples == 0 || self.window_size == 0 {
            return Err(ConformalConfigError(
                "calibration sample and window counts must be positive",
            ));
        }
        if self.min_samples > self.window_size {
            return Err(ConformalConfigError(
                "minimum samples must fit in the calibration window",
            ));
        }
        if !self.q_default.is_finite() || self.q_default < 0.0 {
            return Err(ConformalConfigError(
                "warmup residual must be finite and nonnegative",
            ));
        }
        Ok(())
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

/// Whether a finite bound can be used for the requested calibration population.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConformalStatus {
    /// The selected population has fewer than the configured minimum samples.
    Warmup,
    /// A finite order statistic exists; coverage still requires exchangeability.
    Calibrated,
    /// The requested order statistic lies beyond the retained residuals.
    UnattainableRank,
    /// The baseline or budget is nonfinite or negative.
    InvalidInput,
    /// Adding otherwise finite timings exceeded the finite numeric range.
    ArithmeticOverflow,
}

impl ConformalStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warmup => "warmup",
            Self::Calibrated => "calibrated",
            Self::UnattainableRank => "unattainable_rank",
            Self::InvalidInput => "invalid_input",
            Self::ArithmeticOverflow => "arithmetic_overflow",
        }
    }
}

/// Prediction output with explicit unavailable-bound evidence.
#[derive(Debug, Clone)]
pub struct ConformalPrediction {
    /// Finite upper bound, absent during warmup or when no bound is available.
    pub upper_us: Option<f64>,
    /// Budget risk. An unavailable post-warmup bound cannot certify low risk.
    pub risk: bool,
    /// Requested coverage, present only for a calibrated finite bound.
    /// This is a marginal guarantee only under exchangeability of the scores.
    pub confidence: Option<f64>,
    /// Requested error rate, independently of whether a bound is available.
    pub alpha: f64,
    pub status: ConformalStatus,
    /// One-based rank, including the unavailable n+1 rank.
    pub required_rank: usize,
    /// Heuristic used during warmup, without a coverage claim.
    pub warmup_upper_us: Option<f64>,
    /// Bucket key used for calibration (may be fallback aggregate).
    pub bucket: BucketKey,
    /// Calibration sample count used for the quantile.
    pub sample_count: usize,
    /// Available order statistic q_b; no observed maximum substitutes for n+1.
    pub quantile: Option<f64>,
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
        let number = finite_json_number;
        format!(
            r#"{{"schema":"conformal-v2","upper_us":{},"risk":{},"confidence":{},"alpha":{},"status":"{}","required_rank":{},"warmup_upper_us":{},"bucket":"{}","samples":{},"quantile":{},"fallback_level":{},"window":{},"resets":{},"y_hat":{},"budget_us":{}}}"#,
            number(self.upper_us, 1),
            self.risk,
            number(self.confidence, 17),
            number(Some(self.alpha), 17),
            self.status.as_str(),
            self.required_rank,
            number(self.warmup_upper_us, 1),
            self.bucket,
            self.sample_count,
            number(self.quantile, 2),
            self.fallback_level,
            self.window_size,
            self.reset_count,
            number(Some(self.y_hat), 1),
            number(Some(self.budget_us), 1),
        )
    }
}

pub(crate) fn finite_json_number(value: Option<f64>, precision: usize) -> String {
    value.filter(|v| v.is_finite()).map_or_else(
        || "null".to_string(),
        |value| {
            // Fixed formatting of MAX produces hundreds of digits that some
            // JSON parsers round out of range. Tiny policy values must not
            // round to zero either. Scientific notation preserves both ends.
            let magnitude = value.abs();
            if magnitude >= 1.0e16
                || (magnitude > 0.0 && magnitude < 10.0_f64.powi(-(precision as i32)))
            {
                format!("{value:e}")
            } else {
                let fixed = format!("{value:.precision$}");
                if fixed.parse::<f64>() == Ok(value) {
                    fixed
                } else {
                    // Evidence must preserve the bound, not round it to a
                    // display precision that could fall below an observation.
                    format!("{value:e}")
                }
            }
        },
    )
}

/// Update metadata after observing a frame.
#[derive(Debug, Clone)]
pub struct ConformalUpdate {
    /// Accepted residual (y_t - f(x_t)); absent for invalid observations.
    pub residual: Option<f64>,
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
    pub fn new(config: ConformalConfig) -> Result<Self, ConformalConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            buckets: HashMap::new(),
            reset_count: 0,
        })
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
        let residual = add_up(observed_us, -y_hat_us);
        if !y_hat_us.is_finite()
            || y_hat_us < 0.0
            || !observed_us.is_finite()
            || observed_us < 0.0
            || !residual.is_finite()
        {
            return ConformalUpdate {
                residual: None,
                bucket: key,
                sample_count: self.bucket_samples(key),
            };
        }

        let window_size = self.config.window_size;
        let state = self.buckets.entry(key).or_default();
        state.push(residual, window_size);
        ConformalUpdate {
            residual: Some(residual),
            bucket: key,
            sample_count: state.residuals.len(),
        }
    }

    /// Whether a finite bound with enough samples was actually obtained.
    #[must_use]
    pub fn is_calibrated(&self, prediction: &ConformalPrediction) -> bool {
        prediction.status == ConformalStatus::Calibrated
    }

    pub fn predict(&self, key: BucketKey, y_hat_us: f64, budget_us: f64) -> ConformalPrediction {
        let span = tracing::info_span!(
            "conformal.predict",
            calibration_set_size = tracing::field::Empty,
            predicted_upper_bound_us = tracing::field::Empty,
            frame_budget_us = budget_us,
            coverage_alpha = self.config.alpha,
            gate_triggered = tracing::field::Empty,
            bound_status = tracing::field::Empty,
            required_rank = tracing::field::Empty,
        );
        let _guard = span.enter();

        let QuantileDecision {
            quantile,
            sample_count,
            fallback_level,
            required_rank,
            heuristic_residual,
        } = self.quantile_for(key);

        let valid_input =
            y_hat_us.is_finite() && y_hat_us >= 0.0 && budget_us.is_finite() && budget_us >= 0.0;
        let mut status = if !valid_input {
            ConformalStatus::InvalidInput
        } else if sample_count < self.config.min_samples {
            ConformalStatus::Warmup
        } else if quantile.is_none() {
            ConformalStatus::UnattainableRank
        } else {
            ConformalStatus::Calibrated
        };
        let mut upper_us = None;
        let mut warmup_upper_us = None;
        if matches!(
            status,
            ConformalStatus::Calibrated | ConformalStatus::Warmup
        ) {
            let residual = quantile.unwrap_or(heuristic_residual).max(0.0);
            let upper = add_up(y_hat_us, residual);
            if !upper.is_finite() {
                status = ConformalStatus::ArithmeticOverflow;
            } else if status == ConformalStatus::Calibrated {
                upper_us = Some(upper);
            } else {
                warmup_upper_us = Some(upper);
            }
        }
        let risk = upper_us
            .or(warmup_upper_us)
            .is_none_or(|upper| upper > budget_us);

        span.record("calibration_set_size", sample_count);
        if let Some(upper) = upper_us {
            span.record("predicted_upper_bound_us", upper);
        }
        span.record("gate_triggered", risk);
        span.record("bound_status", status.as_str());
        span.record("required_rank", required_rank);

        tracing::debug!(
            bucket = %key,
            y_hat_us,
            quantile = ?quantile,
            status = status.as_str(),
            required_rank,
            fallback_level,
            sample_count,
            "prediction interval"
        );

        ConformalPrediction {
            upper_us,
            risk,
            confidence: upper_us.map(|_| 1.0 - self.config.alpha),
            alpha: self.config.alpha,
            status,
            required_rank,
            warmup_upper_us,
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
        let min_samples = self.config.min_samples;

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
            quantile: None,
            required_rank: 1,
            heuristic_residual: self.config.q_default,
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
    quantile: Option<f64>,
    required_rank: usize,
    heuristic_residual: f64,
    sample_count: usize,
    fallback_level: u8,
}

impl QuantileDecision {
    fn new(alpha: f64, mut residuals: Vec<f64>, fallback_level: u8) -> Self {
        let quantile = conformal_quantile(alpha, &mut residuals);
        Self {
            quantile,
            required_rank: conformal_rank(alpha, residuals.len()),
            heuristic_residual: residuals.last().copied().unwrap_or(0.0),
            sample_count: residuals.len(),
            fallback_level,
        }
    }
}

/// Least binary64 upper value for a sum of finite binary64 operands.
///
/// Magnitude-ordered FastTwoSum recovers the sign of the exact rounding error,
/// including gradual underflow. Only downward rounding needs the next value;
/// overflow remains nonfinite so callers report an unavailable bound.
/// See Rump, Ogita and Oishi, Fast high precision summation, Algorithm 2.5:
/// <https://www.tuhh.de/ti3/paper/rump/RuOgOi08.pdf>.
fn add_up(a: f64, b: f64) -> f64 {
    let sum = a + b;
    if !sum.is_finite() {
        return sum;
    }
    let (large, small) = if a.abs() >= b.abs() { (a, b) } else { (b, a) };
    let error = small - (sum - large);
    if error > 0.0 { sum.next_up() } else { sum }
}

fn conformal_quantile(alpha: f64, residuals: &mut [f64]) -> Option<f64> {
    residuals.sort_by(f64::total_cmp);
    let rank = conformal_rank(alpha, residuals.len());
    residuals.get(rank - 1).copied()
}

/// Exact ceil((n+1)*(1-alpha)) for the represented, validated binary64 alpha.
/// Subtracting alpha from one first can round a missing rank into existence.
pub(crate) fn conformal_rank(alpha: f64, n: usize) -> usize {
    let bits = alpha.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as u32;
    let fraction = bits & ((1_u64 << 52) - 1);
    let significand = if exponent == 0 {
        fraction
    } else {
        fraction | (1_u64 << 52)
    };
    let shift = if exponent == 0 { 1074 } else { 1075 - exponent };
    let count = n as u128 + 1;
    let product = count * u128::from(significand);
    let tail_count = if shift >= 128 { 0 } else { product >> shift };
    // A residual Vec cannot contain usize::MAX elements; rank <= n+1 fits.
    usize::try_from(count - tail_count).expect("rank fits addressable residual storage")
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
        })
        .expect("valid config");

        let key = test_key(80, 24);
        predictor.observe(key, 0.0, 1.0);
        predictor.observe(key, 0.0, 2.0);
        predictor.observe(key, 0.0, 3.0);

        let decision = predictor.predict(key, 0.0, 1_000.0);
        assert_eq!(decision.quantile, None);
        assert_eq!(decision.required_rank, 4);
        assert_eq!(decision.status, ConformalStatus::UnattainableRank);
        predictor.observe(key, 0.0, 4.0);
        assert_eq!(predictor.predict(key, 0.0, 1_000.0).quantile, Some(4.0));
    }

    #[test]
    fn unavailable_99_percent_rank_is_not_calibrated_at_twenty_samples() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.01,
            min_samples: 20,
            window_size: 256,
            q_default: 10_000.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        for observed_us in 1..=20 {
            predictor.observe(key, 0.0, f64::from(observed_us));
        }
        let prediction = predictor.predict(key, 0.0, 100.0);
        assert_eq!(prediction.sample_count, 20);
        assert_eq!(prediction.status, ConformalStatus::UnattainableRank);
        assert_eq!(prediction.required_rank, 21);
        assert_eq!(prediction.upper_us, None);
        assert_eq!(prediction.quantile, None);
        assert_eq!(prediction.confidence, None);
        assert!(prediction.risk);
        assert!(
            !predictor.is_calibrated(&prediction),
            "99% requires the unavailable 21st order statistic; the observed maximum only supports 20/21 marginal coverage"
        );
    }

    #[test]
    fn ranks_match_exhaustive_held_out_position_oracle() {
        // Independent finite-population oracle: hold out each of n+1 distinct
        // observations, count coverage for every candidate calibration order
        // statistic, and select the least rank attaining the requested count.
        // Dyadic alpha makes the oracle's integer inequality exact.
        for n in 0..=24 {
            for alpha_numerator in [1, 2, 31, 128, 256, 512, 768, 1023] {
                let alpha = f64::from(alpha_numerator) / 1024.0;
                let oracle = (1..=n).find(|&candidate| {
                    let covered = (0..=n)
                        .filter(|&held_out| {
                            let training: Vec<_> =
                                (0..=n).filter(|&point| point != held_out).collect();
                            held_out <= training[candidate - 1]
                        })
                        .count();
                    covered * 1024 >= (n + 1) * (1024 - alpha_numerator as usize)
                });
                let mut residuals: Vec<_> = (0..n).rev().map(|i| i as f64).collect();
                assert_eq!(
                    conformal_quantile(alpha, &mut residuals),
                    oracle.map(|rank| (rank - 1) as f64),
                    "n={n}, alpha={alpha}"
                );
                assert_eq!(conformal_rank(alpha, n), oracle.unwrap_or(n + 1));
            }
        }
    }

    #[test]
    fn attainable_boundaries_respect_adjacent_binary64_values() {
        for power in 1..=10 {
            let count = 1_usize << power;
            let alpha = 1.0 / count as f64;
            assert_eq!(conformal_rank(alpha.next_down(), count - 1), count);
            assert_eq!(conformal_rank(alpha, count - 1), count - 1);
            assert_eq!(conformal_rank(alpha.next_up(), count - 1), count - 1);
        }
        assert_eq!(conformal_rank(f64::from_bits(1), 20), 21);
        assert_eq!(conformal_rank(1.0_f64.next_down(), 20), 1);
        // Binary64's 1/3 is below the exact rational threshold for n=2.
        assert_eq!(conformal_rank(1.0 / 3.0, 2), 3);
        assert_eq!(conformal_rank((1.0_f64 / 3.0).next_up(), 2), 2);
    }

    #[test]
    fn invalid_configuration_is_rejected_without_clamping() {
        for alpha in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -1.0,
            0.0,
            1.0,
            2.0,
        ] {
            assert!(
                ConformalPredictor::new(ConformalConfig {
                    alpha,
                    ..Default::default()
                })
                .is_err()
            );
        }
        for (min_samples, window_size) in [(0, 20), (20, 0), (21, 20)] {
            assert!(
                ConformalPredictor::new(ConformalConfig {
                    min_samples,
                    window_size,
                    ..Default::default()
                })
                .is_err()
            );
        }
        for q_default in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            assert!(
                ConformalPredictor::new(ConformalConfig {
                    q_default,
                    ..Default::default()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn invalid_inputs_and_overflow_have_valid_nullable_evidence() {
        let key = test_key(80, 24);
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.5,
            min_samples: 1,
            window_size: 2,
            q_default: 0.0,
        })
        .expect("valid config");
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            for (baseline, observed) in [(invalid, 1.0), (1.0, invalid)] {
                assert_eq!(predictor.observe(key, baseline, observed).residual, None);
                assert_eq!(predictor.bucket_samples(key), 0);
            }
            for (baseline, budget) in [(invalid, 1.0), (1.0, invalid)] {
                let prediction = predictor.predict(key, baseline, budget);
                assert_eq!(prediction.status, ConformalStatus::InvalidInput);
                assert!(prediction.risk);
                let row: serde_json::Value = serde_json::from_str(&prediction.to_jsonl())
                    .expect("valid JSON even for invalid input");
                assert!(row["upper_us"].is_null());
                assert!(row["confidence"].is_null());
            }
        }
        predictor.observe(key, 0.0, f64::MAX);
        let overflow = predictor.predict(key, f64::MAX, f64::MAX);
        assert_eq!(overflow.status, ConformalStatus::ArithmeticOverflow);
        assert_eq!(overflow.upper_us, None);
        assert_eq!(overflow.confidence, None);
        assert!(overflow.risk);
        serde_json::from_str::<serde_json::Value>(&overflow.to_jsonl()).expect("finite JSON");
    }

    #[test]
    fn evidence_preserves_tiny_alpha_and_finite_extreme_timings() {
        let predictor = ConformalPredictor::new(ConformalConfig {
            alpha: f64::from_bits(1),
            ..Default::default()
        })
        .expect("valid subnormal alpha");
        let prediction = predictor.predict(test_key(80, 24), f64::MAX, f64::MAX);
        let row: serde_json::Value =
            serde_json::from_str(&prediction.to_jsonl()).expect("valid extreme JSON");
        assert_eq!(row["alpha"].as_f64(), Some(f64::from_bits(1)));
        assert_eq!(row["y_hat"].as_f64(), Some(f64::MAX));
    }

    #[test]
    fn tied_observations_remain_below_the_calibrated_bound_after_rounding() {
        for (baseline, observed) in [
            (1.005, 10.001),
            (1.0, 10_000_000_000_000_002.0),
            (0.0, f64::from_bits(1)),
            (10.0, 3.0),
            (f64::MAX, f64::MAX),
        ] {
            let mut predictor = ConformalPredictor::new(ConformalConfig {
                alpha: 0.5,
                min_samples: 1,
                window_size: 1,
                ..Default::default()
            })
            .expect("valid config");
            let key = test_key(80, 24);
            predictor.observe(key, baseline, observed);
            let prediction = predictor.predict(key, baseline, f64::MAX);
            assert_eq!(prediction.status, ConformalStatus::Calibrated);
            assert!(
                prediction.upper_us.unwrap() >= observed,
                "a tied future observation must remain covered: baseline={baseline:?}, observed={observed:?}, prediction={prediction:?}"
            );
            let json: serde_json::Value =
                serde_json::from_str(&prediction.to_jsonl()).expect("actual predictor JSON");
            assert!(
                json["upper_us"].as_f64().expect("finite upper") >= observed,
                "default JSON reader must retain coverage for this regression: {json}"
            );
        }
    }

    #[test]
    fn seeded_binary64_ties_remain_covered_or_explicitly_unbounded() {
        let mut seed = 0x5146_7557_554f_4159_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            f64::from_bits(seed & 0x7fff_ffff_ffff_ffff)
        };
        let mut checked = 0;
        for _ in 0..4_000 {
            let baseline = next();
            let observed = next();
            if !baseline.is_finite() || !observed.is_finite() {
                continue;
            }
            let mut predictor = ConformalPredictor::new(ConformalConfig {
                alpha: 0.5,
                min_samples: 1,
                window_size: 1,
                ..Default::default()
            })
            .expect("valid config");
            let key = test_key(80, 24);
            assert!(
                predictor
                    .observe(key, baseline, observed)
                    .residual
                    .is_some()
            );
            let prediction = predictor.predict(key, baseline, f64::MAX);
            if let Some(upper) = prediction.upper_us {
                assert!(
                    upper >= observed,
                    "{baseline:?}, {observed:?}: {prediction:?}"
                );
            } else {
                assert_eq!(prediction.status, ConformalStatus::ArithmeticOverflow);
                assert!(prediction.risk);
                assert!(prediction.confidence.is_none());
            }
            checked += 1;
        }
        assert!(checked > 3_900, "finite input denominator: {checked}");
    }

    #[test]
    fn directed_addition_preserves_exact_values_and_rounds_ties_outward() {
        let half_ulp = 2.0_f64.powi(-53);
        let tiny = f64::from_bits(1);
        for (a, b, expected) in [
            (1.0, half_ulp, 1.0_f64.next_up()),
            (1.0_f64.next_up(), half_ulp, 1.0 + 2.0_f64.powi(-51)),
            (-1.0, -half_ulp, -1.0),
            ((-1.0_f64).next_down(), -half_ulp, (-1.0_f64).next_down()),
            (f64::MAX, 0.0, f64::MAX),
            (f64::MAX, -1.0, f64::MAX),
            (f64::MAX, 1.0, f64::INFINITY),
            (f64::MAX, -f64::MAX, 0.0),
            (tiny, 2.0 * tiny, 3.0 * tiny),
            (f64::MIN_POSITIVE, -tiny, f64::MIN_POSITIVE.next_down()),
            (1.0, -1.0, 0.0),
        ] {
            assert_eq!(add_up(a, b), expected, "{a:?} + {b:?}");
            assert_eq!(add_up(b, a), expected, "{b:?} + {a:?}");
        }
    }

    #[test]
    fn finite_json_numbers_preserve_fractional_bounds() {
        for value in [10.001, 10.001_f64.next_up(), 0.995, 1.005] {
            let json = finite_json_number(Some(value), 1);
            // Standard binary64 parsing independently checks formatter fidelity;
            // decoder-specific rounding behavior is a separate concern.
            assert_eq!(json.parse::<f64>().expect("number"), value, "{json}");
        }
    }

    #[test]
    fn seeded_independent_trials_cover_exchangeable_data_but_not_drift() {
        // Fresh calibration data AND a fresh held-out point on every trial.
        // This is a reproducible synthetic experiment, not live timing proof.
        let mut seed = 0x6a09_e667_f3bc_c909_u64;
        let mut uniform = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1_u64 << 53) as f64
        };
        let key = test_key(80, 24);
        let trials = 4000;
        let mut covered = 0;
        let mut drift_covered = 0;
        for _ in 0..trials {
            let mut predictor = ConformalPredictor::new(ConformalConfig {
                alpha: 0.125,
                min_samples: 15,
                window_size: 31,
                q_default: 0.0,
            })
            .expect("valid config");
            for _ in 0..31 {
                predictor.observe(key, 0.0, uniform());
            }
            let prediction = predictor.predict(key, 0.0, 2.0);
            let upper = prediction.upper_us.expect("rank28 is available");
            let held_out = uniform();
            covered += usize::from(held_out <= upper);
            drift_covered += usize::from(held_out + 1.0 <= upper);
        }
        let target = 28.0 / 32.0;
        let mean = covered as f64 / trials as f64;
        let six_sigma = 6.0 * (target * (1.0 - target) / trials as f64).sqrt();
        eprintln!(
            "independent synthetic conformal trials: seed=0x6a09e667f3bcc909 n=31 alpha=0.125 rank=28 trials={trials} covered={covered} coverage={mean} six_sigma={six_sigma} drift_covered={drift_covered}"
        );
        assert!(
            (mean - target).abs() <= six_sigma,
            "coverage={mean}, expected={target}, six_sigma={six_sigma}, seed=0x6a09e667f3bcc909"
        );
        assert_eq!(
            drift_covered, 0,
            "a disjoint shifted population violates exchangeability"
        );
    }

    #[test]
    fn fallback_hierarchy_mode_diff() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 4,
            window_size: 16,
            q_default: 0.0,
        })
        .expect("valid config");

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
        })
        .expect("valid config");

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
        })
        .expect("valid config");
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
        })
        .expect("valid config");
        let key = test_key(120, 40);
        let prediction = predictor.predict(key, 5.0, 10_000.0);
        assert_eq!(prediction.quantile, None);
        assert_eq!(prediction.upper_us, None);
        assert_eq!(prediction.warmup_upper_us, Some(47.0));
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
        })
        .expect("valid config");

        let small = test_key(40, 10);
        predictor.observe(small, 0.0, 1.0);
        predictor.observe(small, 0.0, 2.0);

        let large = test_key(200, 60);
        predictor.observe(large, 0.0, 10.0);
        predictor.observe(large, 0.0, 12.0);

        let prediction = predictor.predict(large, 0.0, 1_000.0);
        assert_eq!(prediction.fallback_level, 0);
        assert_eq!(prediction.sample_count, 2);
        assert_eq!(prediction.quantile, None);
        assert_eq!(prediction.required_rank, 3);
        for value in [11.0, 9.0] {
            predictor.observe(large, 0.0, value);
        }
        assert_eq!(predictor.predict(large, 0.0, 1_000.0).quantile, Some(12.0));
    }

    #[test]
    fn reset_clears_bucket_and_raises_reset_count() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 7.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        predictor.observe(key, 0.0, 3.0);
        assert_eq!(predictor.bucket_samples(key), 1);

        predictor.reset_bucket(key);
        assert_eq!(predictor.bucket_samples(key), 0);

        let prediction = predictor.predict(key, 0.0, 1_000.0);
        assert_eq!(prediction.quantile, None);
        assert_eq!(prediction.warmup_upper_us, Some(7.0));
        assert_eq!(prediction.reset_count, 1);
    }

    #[test]
    fn reset_all_forces_conservative_fallback() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 9.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        predictor.observe(key, 0.0, 2.0);

        predictor.reset_all();
        let prediction = predictor.predict(key, 0.0, 1_000.0);
        assert_eq!(prediction.quantile, None);
        assert_eq!(prediction.warmup_upper_us, Some(9.0));
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
        assert_eq!(conformal_quantile(0.1, &mut data), None);
    }

    #[test]
    fn conformal_quantile_single_element() {
        let mut data = vec![42.0];
        assert_eq!(conformal_quantile(0.1, &mut data), None);
        assert_eq!(conformal_quantile(0.5, &mut data), Some(42.0));
    }

    #[test]
    fn conformal_quantile_sorted_data() {
        let mut data = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let q = conformal_quantile(0.5, &mut data);
        // (5+1)*0.5 = 3.0 -> ceil = 3 -> idx = 2 -> data[2] = 3.0
        assert_eq!(q, Some(3.0));
    }

    #[test]
    fn conformal_quantile_alpha_half() {
        let mut data = vec![10.0, 20.0, 30.0, 40.0];
        let q = conformal_quantile(0.5, &mut data);
        // (4+1)*0.5 = 2.5 -> ceil = 3 -> idx = 2 -> data[2] = 30.0
        assert_eq!(q, Some(30.0));
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
        })
        .expect("valid config");
        let key = test_key(80, 24);
        let update = predictor.observe(key, 0.0, f64::NAN);
        assert_eq!(update.residual, None);
        assert_eq!(predictor.bucket_samples(key), 0);
    }

    #[test]
    fn observe_infinity_residual_not_stored() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 5.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        predictor.observe(key, 0.0, f64::INFINITY);
        assert_eq!(predictor.bucket_samples(key), 0);
    }

    // --- prediction fields ---

    /// The runtime gates only on calibrated predictions: the `q_default`
    /// prior and pooled fallbacks below `min_samples` still report `risk`
    /// (for evidence) but must not count as a coverage guarantee.
    #[test]
    fn is_calibrated_requires_min_samples_on_the_level_used() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.25,
            min_samples: 3,
            window_size: 8,
            q_default: 50_000.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        let cold = predictor.predict(key, 16_000.0, 16_000.0);
        assert!(cold.risk, "q_default prior exceeds the budget");
        assert_eq!(cold.sample_count, 0);
        assert!(
            !predictor.is_calibrated(&cold),
            "no samples: not calibrated"
        );

        predictor.observe(key, 0.0, 10.0);
        predictor.observe(key, 0.0, 12.0);
        let two = predictor.predict(key, 0.0, 1.0);
        assert_eq!(two.sample_count, 2);
        assert!(
            !predictor.is_calibrated(&two),
            "below min_samples: not calibrated"
        );

        predictor.observe(key, 0.0, 11.0);
        let three = predictor.predict(key, 0.0, 1.0);
        assert_eq!(three.sample_count, 3);
        assert_eq!(three.fallback_level, 0);
        assert!(predictor.is_calibrated(&three));
        assert!(three.risk);

        // Pooled fallback counts too: a different size bucket in the same
        // mode/diff class borrows the three residuals above.
        let other = predictor.predict(test_key(200, 60), 0.0, 1.0);
        assert!(other.fallback_level >= 1);
        assert!(predictor.is_calibrated(&other));
    }

    #[test]
    fn prediction_risk_flag() {
        let predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 50.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        // No data -> q_default = 50.0, y_hat = 0 -> upper = 50
        let p = predictor.predict(key, 0.0, 100.0);
        assert!(!p.risk); // 50 <= 100
        let p2 = predictor.predict(key, 0.0, 30.0);
        assert!(p2.risk); // 50 > 30
    }

    #[test]
    fn prediction_confidence() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.05,
            min_samples: 1,
            window_size: 32,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        let p = predictor.predict(key, 0.0, 100.0);
        assert_eq!(p.confidence, None);
        for _ in 0..20 {
            predictor.observe(key, 0.0, 1.0);
        }
        assert_eq!(predictor.predict(key, 0.0, 100.0).confidence, Some(0.95));
    }

    // --- global fallback with data ---

    #[test]
    fn global_fallback_with_data() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 100, // impossibly high -> always fall through
            window_size: 256,
            q_default: 999.0,
        })
        .expect("valid config");
        // Use altscreen mode bucket, then query inline
        let alt_key = BucketKey::from_context(ScreenMode::AltScreen, DiffStrategy::Full, 80, 24);
        predictor.observe(alt_key, 0.0, 5.0);

        let inline_key = test_key(80, 24);
        let p = predictor.predict(inline_key, 0.0, 1000.0);
        // Falls all the way to global (level 3), has 1 sample
        assert_eq!(p.fallback_level, 3);
        assert_eq!(p.sample_count, 1);
        assert_eq!(p.quantile, None);
        assert_eq!(p.warmup_upper_us, Some(5.0));
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
        let predictor = ConformalPredictor::new(config).expect("valid config");
        assert!((predictor.config().alpha - 0.2).abs() < 1e-10);
        assert_eq!(predictor.config().min_samples, 5);
    }

    // --- negative residuals ---

    #[test]
    fn negative_residual_clamped_in_prediction() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.5,
            min_samples: 1,
            window_size: 8,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        // observed < y_hat -> negative residual
        predictor.observe(key, 10.0, 5.0);
        let p = predictor.predict(key, 10.0, 100.0);
        // quantile is -5.0, but clamped to 0.0 via .max(0.0)
        // so upper_us = 10.0 + 0.0 = 10.0
        assert_eq!(p.quantile, Some(-5.0));
        assert_eq!(p.upper_us, Some(10.0));
    }

    // --- ConformalUpdate fields ---

    #[test]
    fn observe_returns_correct_update() {
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        let update = predictor.observe(key, 3.0, 10.0);
        assert_eq!(update.residual, Some(7.0));
        assert_eq!(update.bucket, key);
        assert_eq!(update.sample_count, 1);
    }

    // --- prediction y_hat and budget fields ---

    #[test]
    fn prediction_preserves_yhat_and_budget() {
        let predictor = ConformalPredictor::new(ConformalConfig::default()).expect("valid config");
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

        let predictor = ConformalPredictor::new(ConformalConfig::default()).expect("valid config");
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
        })
        .expect("valid config");
        let key = test_key(80, 24);
        // budget_us = 100 << q_default = 50_000 -> risk = true
        let p = predictor.predict(key, 0.0, 100.0);
        assert!(p.risk, "prediction should be risky");
        assert!(
            saw_gate.load(Ordering::Relaxed),
            "predict() must record gate_triggered=true when risk"
        );
    }

    // ========================================================================
    // bd-1q5.12: Additional unit tests for conformal prediction frame-time gating
    // ========================================================================

    // --- Calibration with known distributions ---

    #[test]
    fn calibration_uniform_distribution_quantile() {
        // Uniform residuals from 0 to 99. The (1-alpha) quantile should be near
        // the top of the distribution.
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.05,
            min_samples: 1,
            window_size: 256,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        for i in 0..100 {
            predictor.observe(key, 0.0, i as f64);
        }
        let p = predictor.predict(key, 0.0, 1_000.0);
        // (100+1)*0.95 = 95.95, ceil = 96, idx = 95 -> sorted[95] = 95.0
        assert_eq!(p.fallback_level, 0);
        assert_eq!(p.sample_count, 100);
        assert_eq!(p.quantile, Some(95.0));
    }

    #[test]
    fn calibration_gaussian_like_distribution() {
        // Simulate a roughly gaussian-shaped distribution of residuals.
        // Use a simple deterministic approximation: residuals centered around 0
        // with known spread.
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 256,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(120, 40);

        // Generate 50 residuals that approximate a symmetric distribution
        // around 0: [-24.5, -23.5, ..., -0.5, 0.5, ..., 24.5]
        for i in 0..50 {
            let residual = (i as f64) - 24.5;
            predictor.observe(key, 100.0, 100.0 + residual);
        }

        let p = predictor.predict(key, 100.0, 1_000.0);
        // (50+1)*0.9 = 45.9, ceil = 46, idx = 45 -> sorted residual at index 45
        // sorted: [-24.5, -23.5, ..., 24.5], index 45 = 20.5
        assert_eq!(p.fallback_level, 0);
        assert_eq!(p.sample_count, 50);
        assert_eq!(p.quantile, Some(20.5));
        // upper_us = 100 + max(20.5, 0) = 120.5
        assert_eq!(p.upper_us, Some(120.5));
    }

    #[test]
    fn calibration_constant_residuals() {
        // All residuals identical -> quantile should be that constant.
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.05,
            min_samples: 1,
            window_size: 256,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        for _ in 0..30 {
            predictor.observe(key, 100.0, 105.0); // residual = 5.0
        }
        let p = predictor.predict(key, 100.0, 1_000.0);
        assert_eq!(p.quantile, Some(5.0));
        assert_eq!(p.upper_us, Some(105.0));
    }

    // --- Deterministic order-statistic checks (not statistical coverage evidence) ---

    #[test]
    fn quantile_covers_expected_fraction_of_uniform_grid() {
        // A fixed calibration grid and a finer query grid check indexing only.
        // These are not independent draws or a statistical coverage experiment.
        let alpha = 0.1;
        let n_calibrate = 100;
        let n_test = 200;

        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha,
            min_samples: 1,
            window_size: 256,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);

        // Calibrate: residuals are 0, 1, 2, ..., 99
        for i in 0..n_calibrate {
            predictor.observe(key, 0.0, i as f64);
        }

        // Test coverage: for each "new" sample, check if it falls within the
        // prediction interval [0, y_hat + q].
        let prediction = predictor.predict(key, 0.0, f64::MAX);
        let upper_bound = prediction.upper_us.expect("attainable rank");

        let mut covered = 0;
        // Test samples: same distribution range [0..200)
        for i in 0..n_test {
            let new_obs = (i as f64) * (n_calibrate as f64) / (n_test as f64);
            if new_obs <= upper_bound {
                covered += 1;
            }
        }

        let empirical_coverage = covered as f64 / n_test as f64;
        // Coverage should be >= 1 - alpha - epsilon (epsilon accounts for
        // finite-sample effects)
        let target_coverage = 1.0 - alpha - 0.05; // generous epsilon
        assert!(
            empirical_coverage >= target_coverage,
            "Empirical coverage {empirical_coverage:.3} should be >= {target_coverage:.3}"
        );
    }

    #[test]
    fn quantile_covers_expected_fraction_of_calibration_grid() {
        // Reusing calibration points checks the order statistic, not coverage
        // on future samples or under a distribution shift.
        let alpha = 0.05;
        let n = 200;

        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha,
            min_samples: 1,
            window_size: 512,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);

        // Calibrate with known residuals: 1.0, 2.0, ..., 200.0
        for i in 1..=n {
            predictor.observe(key, 0.0, i as f64);
        }

        let p = predictor.predict(key, 0.0, f64::MAX);
        // (200+1)*0.95 = 190.95, ceil = 191, idx = 190 -> sorted[190] = 191.0
        assert_eq!(p.quantile, Some(191.0));
        // At least 95% of calibration samples should be <= upper bound
        let upper = p.upper_us.expect("attainable rank");
        let covered = (1..=n).filter(|&i| (i as f64) <= upper).count();
        let coverage = covered as f64 / n as f64;
        assert!(
            coverage >= 1.0 - alpha,
            "Coverage {coverage:.3} should be >= {:.3}",
            1.0 - alpha
        );
    }

    // --- Gate trigger behavior at boundary conditions ---

    #[test]
    fn gate_trigger_exact_boundary() {
        // When upper_us == budget_us, risk should be false (not strictly greater)
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.5,
            min_samples: 1,
            window_size: 8,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        predictor.observe(key, 0.0, 50.0);
        // quantile = 50.0, y_hat = 0.0, upper_us = 50.0
        let p = predictor.predict(key, 0.0, 50.0);
        assert!(
            !p.risk,
            "upper_us ({:?}) == budget_us ({}) should NOT trigger risk",
            p.upper_us, p.budget_us
        );
    }

    #[test]
    fn gate_trigger_just_above_boundary() {
        // When upper_us is epsilon above budget, risk should be true
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.5,
            min_samples: 1,
            window_size: 8,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        predictor.observe(key, 0.0, 50.0);
        // upper_us = 50.0, budget = 49.999
        let p = predictor.predict(key, 0.0, 49.999);
        assert!(p.risk, "upper_us > budget should trigger risk");
    }

    #[test]
    fn gate_trigger_just_below_boundary() {
        // When upper_us is epsilon below budget, risk should be false
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.5,
            min_samples: 1,
            window_size: 8,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        predictor.observe(key, 0.0, 50.0);
        // upper_us = 50.0, budget = 50.001
        let p = predictor.predict(key, 0.0, 50.001);
        assert!(!p.risk, "upper_us < budget should NOT trigger risk");
    }

    #[test]
    fn gate_trigger_zero_budget() {
        // Zero budget: any positive prediction should trigger risk
        let predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 1.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        let p = predictor.predict(key, 0.0, 0.0);
        assert!(p.risk, "positive upper_us with zero budget should be risky");
    }

    #[test]
    fn gate_trigger_very_large_budget() {
        // Extremely large budget: should never trigger risk
        let predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 100_000.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        let p = predictor.predict(key, 1_000.0, f64::MAX);
        assert!(!p.risk, "huge budget should never trigger risk");
    }

    // --- Alpha parameter sensitivity ---

    #[test]
    fn alpha_sensitivity_wider_interval_with_lower_alpha() {
        let key = test_key(80, 24);

        // Calibrate two predictors with different alpha on same data
        let mut predictor_tight = ConformalPredictor::new(ConformalConfig {
            alpha: 0.5, // 50% coverage -> narrower interval
            min_samples: 1,
            window_size: 256,
            q_default: 0.0,
        })
        .expect("valid config");

        let mut predictor_wide = ConformalPredictor::new(ConformalConfig {
            alpha: 0.01, // 99% coverage -> wider interval
            min_samples: 1,
            window_size: 256,
            q_default: 0.0,
        })
        .expect("valid config");

        for i in 0..100 {
            let obs = i as f64;
            predictor_tight.observe(key, 0.0, obs);
            predictor_wide.observe(key, 0.0, obs);
        }

        let p_tight = predictor_tight.predict(key, 0.0, 10_000.0);
        let p_wide = predictor_wide.predict(key, 0.0, 10_000.0);

        assert!(
            p_wide.quantile.expect("attainable rank") > p_tight.quantile.expect("attainable rank"),
            "Lower alpha ({}) should produce wider interval: quantile {:?} vs {:?}",
            0.01,
            p_wide.quantile,
            p_tight.quantile
        );
        assert!(
            p_wide.upper_us.expect("attainable rank") > p_tight.upper_us.expect("attainable rank"),
            "Lower alpha should produce higher upper bound"
        );
    }

    #[test]
    fn alpha_sensitivity_confidence_reflects_alpha() {
        for &alpha in &[0.01, 0.05, 0.1, 0.2, 0.5] {
            let mut predictor = ConformalPredictor::new(ConformalConfig {
                alpha,
                min_samples: 1,
                window_size: 128,
                q_default: 0.0,
            })
            .expect("valid config");
            let key = test_key(80, 24);
            let p = predictor.predict(key, 0.0, 1_000.0);
            assert_eq!(p.alpha, alpha);
            assert_eq!(p.confidence, None);
            for _ in 0..100 {
                predictor.observe(key, 0.0, 1.0);
            }
            let p = predictor.predict(key, 0.0, 1_000.0);
            let expected_confidence = 1.0 - alpha;
            assert!(
                (p.confidence.expect("attainable rank") - expected_confidence).abs() < 1e-10,
                "confidence should be 1-alpha for alpha={alpha}"
            );
        }
    }

    #[test]
    fn alpha_sensitivity_extreme_alpha_zero() {
        // Very small alpha requires a rank beyond the available samples.
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.001,
            min_samples: 1,
            window_size: 256,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        for i in 0..100 {
            predictor.observe(key, 0.0, i as f64);
        }
        let p = predictor.predict(key, 0.0, 10_000.0);
        assert_eq!(p.required_rank, 101);
        assert_eq!(p.quantile, None);
        assert_eq!(p.upper_us, None);
        assert_eq!(p.status, ConformalStatus::UnattainableRank);
    }

    #[test]
    fn alpha_sensitivity_extreme_alpha_one() {
        // alpha near 1 -> coverage near 0% -> picks the smallest residual
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.99,
            min_samples: 1,
            window_size: 256,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        for i in 0..100 {
            predictor.observe(key, 0.0, i as f64);
        }
        let p = predictor.predict(key, 0.0, 10_000.0);
        // (100+1)*0.01 = 1.01, ceil=2, idx=1 -> sorted[1]=1
        assert_eq!(p.quantile, Some(1.0));
    }

    // --- Empty/small calibration set handling ---

    #[test]
    fn empty_calibration_uses_default() {
        let predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.05,
            min_samples: 20,
            window_size: 256,
            q_default: 10_000.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        let p = predictor.predict(key, 100.0, 16_666.0);
        assert_eq!(p.sample_count, 0);
        assert_eq!(p.fallback_level, 3);
        assert_eq!(p.quantile, None);
        assert_eq!(p.upper_us, None);
        assert_eq!(p.warmup_upper_us, Some(10_100.0));
    }

    #[test]
    fn one_sample_below_min_samples_uses_fallback() {
        // With min_samples=20, 1 sample should fall through to global (level 3)
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.05,
            min_samples: 20,
            window_size: 256,
            q_default: 999.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        predictor.observe(key, 0.0, 5.0);
        let p = predictor.predict(key, 0.0, 1_000.0);
        // Only 1 sample in exact bucket, 1 in mode+diff, 1 in mode, 1 in global
        // Global has data, so uses global fallback (level 3)
        assert_eq!(p.fallback_level, 3);
        assert_eq!(p.sample_count, 1);
    }

    #[test]
    fn exactly_min_samples_minus_one_uses_fallback() {
        let min_samples = 5;
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples,
            window_size: 256,
            q_default: 999.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        for i in 0..(min_samples - 1) {
            predictor.observe(key, 0.0, (i as f64) * 10.0);
        }
        let p = predictor.predict(key, 0.0, 1_000.0);
        // 4 samples < min_samples=5, so exact bucket not used
        // Falls through to mode+diff (4 samples < 5), mode (4 < 5), global (4 > 0)
        assert_eq!(p.fallback_level, 3);
        assert_eq!(p.sample_count, min_samples - 1);
    }

    #[test]
    fn exactly_min_samples_uses_exact_bucket() {
        let min_samples = 5;
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples,
            window_size: 256,
            q_default: 999.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        for i in 0..min_samples {
            predictor.observe(key, 0.0, (i as f64) * 10.0);
        }
        let p = predictor.predict(key, 0.0, 1_000.0);
        // 5 samples == min_samples=5, exact bucket should be used
        assert_eq!(p.fallback_level, 0);
        assert_eq!(p.sample_count, min_samples);
    }

    #[test]
    fn min_samples_plus_one_uses_exact_bucket() {
        let min_samples = 5;
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples,
            window_size: 256,
            q_default: 999.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        for i in 0..=min_samples {
            predictor.observe(key, 0.0, (i as f64) * 10.0);
        }
        let p = predictor.predict(key, 0.0, 1_000.0);
        assert_eq!(p.fallback_level, 0);
        assert_eq!(p.sample_count, min_samples + 1);
    }

    #[test]
    fn min_samples_one_allows_single_observation() {
        // Edge case: min_samples = 1
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 256,
            q_default: 999.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        predictor.observe(key, 0.0, 42.0);
        let p = predictor.predict(key, 0.0, 1_000.0);
        assert_eq!(p.fallback_level, 0);
        assert_eq!(p.sample_count, 1);
        assert_eq!(p.quantile, None);
        assert_eq!(p.status, ConformalStatus::UnattainableRank);
    }

    // --- Tracing span field assertions ---

    #[test]
    fn predict_span_records_calibration_set_size() {
        use std::sync::Arc;
        use std::sync::Mutex;

        struct FieldRecorder {
            calibration_size: Arc<Mutex<Option<u64>>>,
        }

        struct SizeVisitor(Arc<Mutex<Option<u64>>>);

        impl tracing::field::Visit for SizeVisitor {
            fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                if field.name() == "calibration_set_size" {
                    *self.0.lock().unwrap() = Some(value);
                }
            }
            fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn fmt::Debug) {}
        }

        impl tracing::Subscriber for FieldRecorder {
            fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
                let mut v = SizeVisitor(Arc::clone(&self.calibration_size));
                values.record(&mut v);
            }
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, _: &tracing::Event<'_>) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        let size = Arc::new(Mutex::new(None));
        let subscriber = FieldRecorder {
            calibration_size: Arc::clone(&size),
        };
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 0.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        for i in 0..5 {
            predictor.observe(key, 0.0, i as f64);
        }
        let _ = predictor.predict(key, 0.0, 1_000.0);

        let recorded = size.lock().unwrap();
        assert_eq!(*recorded, Some(5), "calibration_set_size should be 5");
    }

    #[test]
    fn predict_span_records_predicted_upper_bound() {
        use std::sync::Arc;
        use std::sync::Mutex;

        struct UpperBoundRecorder {
            upper_bound: Arc<Mutex<Option<f64>>>,
        }

        struct UpperVisitor(Arc<Mutex<Option<f64>>>);

        impl tracing::field::Visit for UpperVisitor {
            fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
                if field.name() == "predicted_upper_bound_us" {
                    *self.0.lock().unwrap() = Some(value);
                }
            }
            fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn fmt::Debug) {}
        }

        impl tracing::Subscriber for UpperBoundRecorder {
            fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
                let mut v = UpperVisitor(Arc::clone(&self.upper_bound));
                values.record(&mut v);
            }
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, _: &tracing::Event<'_>) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        let upper = Arc::new(Mutex::new(None));
        let subscriber = UpperBoundRecorder {
            upper_bound: Arc::clone(&upper),
        };
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.5,
            min_samples: 1,
            window_size: 8,
            q_default: 42.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        let p = predictor.predict(key, 10.0, 1_000.0);
        assert_eq!(p.upper_us, None);
        assert_eq!(
            *upper.lock().unwrap(),
            None,
            "warmup must not emit a calibrated bound"
        );
        predictor.observe(key, 0.0, 42.0);
        let p = predictor.predict(key, 10.0, 1_000.0);
        assert_eq!(p.upper_us, Some(52.0));
        assert_eq!(*upper.lock().unwrap(), p.upper_us);
    }

    #[test]
    fn predict_span_records_gate_triggered_false() {
        use std::sync::Arc;
        use std::sync::Mutex;

        struct GateFalseChecker {
            gate_value: Arc<Mutex<Option<bool>>>,
        }

        struct GateFalseVisitor(Arc<Mutex<Option<bool>>>);

        impl tracing::field::Visit for GateFalseVisitor {
            fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
                if field.name() == "gate_triggered" {
                    *self.0.lock().unwrap() = Some(value);
                }
            }
            fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn fmt::Debug) {}
        }

        impl tracing::Subscriber for GateFalseChecker {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
                let mut v = GateFalseVisitor(Arc::clone(&self.gate_value));
                values.record(&mut v);
            }
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, _: &tracing::Event<'_>) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        let gate = Arc::new(Mutex::new(None));
        let subscriber = GateFalseChecker {
            gate_value: Arc::clone(&gate),
        };
        let _guard = tracing::subscriber::set_default(subscriber);

        let predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 8,
            q_default: 1.0,
        })
        .expect("valid config");
        let key = test_key(80, 24);
        let p = predictor.predict(key, 0.0, 1_000_000.0);
        assert!(!p.risk);

        let recorded = gate.lock().unwrap();
        assert_eq!(
            *recorded,
            Some(false),
            "gate_triggered should be recorded as false"
        );
    }

    // --- JSONL output ---

    #[test]
    fn jsonl_output_contains_required_fields() {
        let prediction = ConformalPrediction {
            upper_us: Some(150.5),
            risk: true,
            confidence: Some(0.95),
            alpha: 0.05,
            status: ConformalStatus::Calibrated,
            required_rank: 41,
            warmup_upper_us: None,
            bucket: BucketKey {
                mode: ModeBucket::Inline,
                diff: DiffBucket::Full,
                size_bucket: 10,
            },
            sample_count: 42,
            quantile: Some(50.5),
            fallback_level: 0,
            window_size: 256,
            reset_count: 1,
            y_hat: 100.0,
            budget_us: 140.0,
        };
        let jsonl = prediction.to_jsonl();
        assert!(jsonl.contains("\"schema\":\"conformal-v2\""));
        assert!(jsonl.contains("\"upper_us\":150.5"));
        assert!(jsonl.contains("\"risk\":true"));
        let parsed: serde_json::Value = serde_json::from_str(&jsonl).expect("valid JSON");
        assert_eq!(parsed["confidence"].as_f64(), Some(0.95));
        assert!(jsonl.contains("\"bucket\":\"inline:full:10\""));
        assert!(jsonl.contains("\"samples\":42"));
        assert!(jsonl.contains("\"quantile\":50.50"));
        assert!(jsonl.contains("\"fallback_level\":0"));
        assert!(jsonl.contains("\"window\":256"));
        assert!(jsonl.contains("\"resets\":1"));
        assert!(jsonl.contains("\"y_hat\":100.0"));
        assert!(jsonl.contains("\"budget_us\":140.0"));
    }

    // --- Deterministic calibration-grid checks ---

    #[test]
    fn quantile_grid_fraction_for_multiple_alphas() {
        // These reused calibration points check indexing, not coverage of a
        // future random observation. Independent experiments are tested below.
        for alpha in [0.05, 0.1, 0.2] {
            let n_calibrate = 100;
            let n_test = 100;

            let mut predictor = ConformalPredictor::new(ConformalConfig {
                alpha,
                min_samples: 1,
                window_size: 256,
                q_default: 0.0,
            })
            .expect("valid config");
            let key = test_key(80, 24);

            // Calibrate with residuals 1.0, 2.0, ..., 100.0
            for i in 1..=n_calibrate {
                predictor.observe(key, 0.0, i as f64);
            }

            let p = predictor.predict(key, 0.0, f64::MAX);

            // Count how many calibration-like points are covered
            let upper = p.upper_us.expect("attainable rank");
            let covered = (1..=n_test).filter(|&i| (i as f64) <= upper).count();
            let coverage = covered as f64 / n_test as f64;

            assert!(
                coverage >= 1.0 - alpha - 0.02,
                "alpha={alpha}: coverage {coverage:.3} should be >= {:.3}",
                1.0 - alpha - 0.02
            );
        }
    }

    #[test]
    fn property_monotone_quantile_with_more_extreme_data() {
        // Adding more extreme residuals should increase the quantile
        let key = test_key(80, 24);

        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size: 256,
            q_default: 0.0,
        })
        .expect("valid config");

        // First: moderate residuals
        for i in 0..50 {
            predictor.observe(key, 0.0, i as f64);
        }
        let q_moderate = predictor
            .predict(key, 0.0, f64::MAX)
            .quantile
            .expect("attainable rank");

        // Add extreme residuals
        for _ in 0..50 {
            predictor.observe(key, 0.0, 1000.0);
        }
        let q_extreme = predictor
            .predict(key, 0.0, f64::MAX)
            .quantile
            .expect("attainable rank");

        assert!(
            q_extreme >= q_moderate,
            "Adding extreme data should not decrease quantile: {q_extreme} vs {q_moderate}"
        );
    }

    #[test]
    fn property_quantile_bounded_by_max_residual() {
        // Only an attainable finite quantile is bounded by the observed max.
        let key = test_key(80, 24);
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.001, // very high coverage
            min_samples: 1,
            window_size: 256,
            q_default: 0.0,
        })
        .expect("valid config");

        let max_residual = 98.0;
        for i in 0..50 {
            predictor.observe(key, 0.0, (i as f64) * 2.0); // 0, 2, 4, ..., 98
        }

        let p = predictor.predict(key, 0.0, f64::MAX);
        assert_eq!(p.quantile, None);
        assert_eq!(p.status, ConformalStatus::UnattainableRank);
        let mut finite_predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.05,
            ..predictor.config().clone()
        })
        .expect("valid config");
        for i in 0..50 {
            finite_predictor.observe(key, 0.0, f64::from(i) * 2.0);
        }
        let quantile = finite_predictor
            .predict(key, 0.0, f64::MAX)
            .quantile
            .expect("attainable rank");
        assert!(
            quantile <= max_residual,
            "quantile {quantile} should be <= max residual {max_residual}"
        );
    }

    #[test]
    fn property_window_eviction_changes_quantile() {
        // After filling and evicting the window, old extreme values should
        // no longer affect the quantile
        let key = test_key(80, 24);
        let window_size = 10;

        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 1,
            window_size,
            q_default: 0.0,
        })
        .expect("valid config");

        // Fill window with large residuals
        for _ in 0..window_size {
            predictor.observe(key, 0.0, 1000.0);
        }
        let q_large = predictor
            .predict(key, 0.0, f64::MAX)
            .quantile
            .expect("attainable rank");

        // Evict all large residuals with small ones
        for _ in 0..window_size {
            predictor.observe(key, 0.0, 1.0);
        }
        let q_small = predictor
            .predict(key, 0.0, f64::MAX)
            .quantile
            .expect("attainable rank");

        assert!(
            q_small < q_large,
            "After eviction, quantile should decrease: {q_small} vs {q_large}"
        );
    }

    // --- Multiple bucket interaction ---

    #[test]
    fn cross_mode_fallback_does_not_mix_modes() {
        // mode_diff fallback only aggregates same mode+diff, not across modes
        let mut predictor = ConformalPredictor::new(ConformalConfig {
            alpha: 0.1,
            min_samples: 5,
            window_size: 256,
            q_default: 999.0,
        })
        .expect("valid config");

        // Add data to AltScreen mode
        let alt_key = BucketKey::from_context(ScreenMode::AltScreen, DiffStrategy::Full, 80, 24);
        for i in 0..10 {
            predictor.observe(alt_key, 0.0, (i as f64) * 100.0);
        }

        // Query Inline mode with different size bucket (no exact match)
        let inline_key = BucketKey::from_context(
            ScreenMode::Inline { ui_height: 4 },
            DiffStrategy::Full,
            120,
            40,
        );
        let p = predictor.predict(inline_key, 0.0, 1_000_000.0);

        // Mode fallback should NOT find inline data, so falls to global
        assert_eq!(
            p.fallback_level, 3,
            "Cross-mode query should fall to global"
        );
    }

    #[test]
    fn reset_count_accumulates_across_resets() {
        let mut predictor =
            ConformalPredictor::new(ConformalConfig::default()).expect("valid config");
        let key = test_key(80, 24);

        predictor.observe(key, 0.0, 1.0);
        predictor.reset_bucket(key);
        predictor.observe(key, 0.0, 2.0);
        predictor.reset_all();
        predictor.observe(key, 0.0, 3.0);
        predictor.reset_bucket(key);

        let p = predictor.predict(key, 0.0, 1_000.0);
        assert_eq!(p.reset_count, 3, "reset_count should accumulate");
    }
}

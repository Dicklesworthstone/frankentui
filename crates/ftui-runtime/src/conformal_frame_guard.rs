#![forbid(unsafe_code)]

//! Conformal frame guard for frame timing, with explicit unavailable bounds.
//!
//! Wraps [`ConformalPredictor`] with a frame-time time series, nonconformity
//! score tracking and configured-coverage prediction intervals. The configured
//! alpha determines coverage; it is 99% only when alpha is 0.01. Coverage requires
//! exchangeable scores, which adaptive frame timings do not establish by default.
//!
//! **Fallback:** before calibration reaches `min_samples`, a fixed 16 ms
//! budget threshold is used (no conformal interval).
//!
//! # Integration
//!
//! The guard sits between frame measurement and `BudgetController`:
//!
//! ```text
//! frame_time ──► ConformalFrameGuard ──► P99Prediction
//!                        │                      │
//!                        ▼                      ▼
//!                   observe()              exceeds_budget?
//!                   (calibrate)           → trigger degrade
//! ```

use std::collections::VecDeque;

use ftui_render::budget::DegradationLevel;

use crate::conformal_predictor::{
    BucketKey, ConformalConfig, ConformalConfigError, ConformalPrediction, ConformalPredictor,
    ConformalStatus,
};

/// Default fallback budget threshold in microseconds (16 ms = 60 fps target).
const DEFAULT_FALLBACK_BUDGET_US: f64 = 16_000.0;

/// Configuration for the conformal frame guard.
#[derive(Debug, Clone)]
pub struct ConformalFrameGuardConfig {
    /// Underlying conformal predictor configuration.
    pub conformal: ConformalConfig,

    /// Fixed fallback budget threshold (µs) used before calibration.
    /// Default: 16 000.0 (16 ms).
    pub fallback_budget_us: f64,

    /// Maximum frame time samples retained for time-series tracking.
    /// Default: 512.
    pub time_series_window: usize,

    /// Maximum nonconformity scores retained.
    /// Default: 256 (matches conformal window).
    pub nonconformity_window: usize,
}

impl Default for ConformalFrameGuardConfig {
    fn default() -> Self {
        let conformal = ConformalConfig::default();
        let nonconformity_window = conformal.window_size;
        Self {
            conformal,
            fallback_budget_us: DEFAULT_FALLBACK_BUDGET_US,
            time_series_window: 512,
            nonconformity_window,
        }
    }
}

/// State of the conformal frame guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardState {
    /// Insufficient calibration data; using fixed fallback threshold.
    Warmup,
    /// Calibrated with enough samples; conformal intervals active.
    Calibrated,
    /// Last evaluation indicated a conformal or measured fallback overrun.
    AtRisk,
    /// Warmup ended, but the requested finite bound is still unavailable.
    Unbounded,
}

impl GuardState {
    /// Stable string for JSONL logging.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warmup => "warmup",
            Self::Calibrated => "calibrated",
            Self::AtRisk => "at_risk",
            Self::Unbounded => "unbounded",
        }
    }
}

/// Result of a p99 prediction from the guard.
#[derive(Debug, Clone)]
pub struct P99Prediction {
    /// Base prediction (most recent frame time or EMA estimate) in µs.
    pub y_hat_us: f64,
    /// Upper bound of the p99 prediction interval in µs.
    pub upper_us: Option<f64>,
    /// Frame budget in µs.
    pub budget_us: f64,
    /// Whether the p99 upper bound exceeds the budget.
    pub exceeds_budget: bool,
    /// Calibration sample count used.
    pub calibration_size: usize,
    /// Fallback level from the underlying conformal predictor (0..=4).
    /// Level 4 means frame-guard fixed fallback was used.
    pub fallback_level: u8,
    /// Current guard state.
    pub state: GuardState,
    /// Width of the prediction interval (upper - y_hat) in µs.
    pub interval_width_us: Option<f64>,
    /// Underlying prediction, including unavailable-bound diagnostics.
    pub conformal: Option<ConformalPrediction>,
}

impl P99Prediction {
    /// Format as a JSONL line for structured evidence logging.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let number = crate::conformal_predictor::finite_json_number;
        let conformal_fields = self
            .conformal
            .as_ref()
            .map(|c| {
                format!(
                    r#","conformal_quantile":{},"conformal_bucket":"{}","conformal_confidence":{},"conformal_alpha":{},"conformal_status":"{}","required_rank":{}"#,
                    number(c.quantile, 2), c.bucket, number(c.confidence, 17), number(Some(c.alpha), 17),
                    c.status.as_str(), c.required_rank,
                )
            })
            .unwrap_or_default();

        format!(
            r#"{{"schema":"conformal-frame-guard-v2","y_hat_us":{},"upper_us":{},"budget_us":{},"exceeds_budget":{},"calibration_size":{},"fallback_level":{},"state":"{}","interval_width_us":{}{}}}"#,
            number(Some(self.y_hat_us), 1),
            number(self.upper_us, 1),
            number(Some(self.budget_us), 1),
            self.exceeds_budget,
            self.calibration_size,
            self.fallback_level,
            self.state.as_str(),
            number(self.interval_width_us, 1),
            conformal_fields,
        )
    }
}

/// Conformal frame guard with intervals at the configured quantile.
///
/// Tracks frame render times as a time series, computes nonconformity scores,
/// and predicts an upper bound for the next frame when calibration permits.
/// The target is `1 - alpha` (p99 only for alpha 0.01); its marginal coverage
/// requires exchangeable scores, which adaptive timings need not satisfy.
/// A predicted or measured fallback overrun signals degradation.
#[derive(Debug)]
pub struct ConformalFrameGuard {
    config: ConformalFrameGuardConfig,
    predictor: ConformalPredictor,
    /// Rolling frame time measurements (µs).
    frame_times: VecDeque<f64>,
    /// Rolling nonconformity scores (residual = observed - predicted).
    nonconformity_scores: VecDeque<f64>,
    /// EMA of frame times (µs) for base prediction.
    ema_us: f64,
    /// EMA decay factor. Closer to 1.0 = slower adaptation.
    ema_decay: f64,
    /// Current guard state.
    state: GuardState,
    /// Total observations processed.
    observations: u64,
    /// Count of degradation triggers.
    degradation_triggers: u64,
    calibrated: bool,
}

impl ConformalFrameGuard {
    /// Create a new guard with the given configuration.
    pub fn new(config: ConformalFrameGuardConfig) -> Result<Self, ConformalConfigError> {
        if !config.fallback_budget_us.is_finite() || config.fallback_budget_us <= 0.0 {
            return Err(ConformalConfigError(
                "frame guard fallback budget must be finite and positive",
            ));
        }
        if config.time_series_window == 0 || config.nonconformity_window == 0 {
            return Err(ConformalConfigError("frame guard windows must be positive"));
        }
        let predictor = ConformalPredictor::new(config.conformal.clone())?;
        Ok(Self {
            config,
            predictor,
            frame_times: VecDeque::new(),
            nonconformity_scores: VecDeque::new(),
            ema_us: 0.0,
            ema_decay: 0.95,
            state: GuardState::Warmup,
            observations: 0,
            degradation_triggers: 0,
            calibrated: false,
        })
    }

    /// Create a guard with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(ConformalFrameGuardConfig::default()).expect("valid default frame guard config")
    }

    /// Observe a realized frame time and update calibration.
    ///
    /// `frame_time_us`: measured frame render time in microseconds.
    /// `key`: bucket key from the rendering context.
    pub fn observe(&mut self, frame_time_us: f64, key: BucketKey) {
        if !frame_time_us.is_finite() || frame_time_us < 0.0 {
            return;
        }

        // Score against the prediction available before this observation.
        // Updating the EMA first leaks the outcome into its own residual.
        let y_hat = if self.observations == 0 {
            self.config.fallback_budget_us
        } else {
            self.ema_us
        };
        let Some(residual) = self.predictor.observe(key, y_hat, frame_time_us).residual else {
            return;
        };
        self.observations += 1;

        // Update EMA
        if self.observations == 1 {
            self.ema_us = frame_time_us;
        } else {
            self.ema_us = self.ema_decay * self.ema_us + (1.0 - self.ema_decay) * frame_time_us;
        }

        // Track frame time in rolling window
        self.frame_times.push_back(frame_time_us);
        while self.frame_times.len() > self.config.time_series_window {
            self.frame_times.pop_front();
        }

        // Track the exact accepted score, including conservative rounding.
        self.nonconformity_scores.push_back(residual);
        while self.nonconformity_scores.len() > self.config.nonconformity_window {
            self.nonconformity_scores.pop_front();
        }

        // Refresh metadata using the same pooled population and arithmetic as
        // prediction. Observing is not itself a degradation decision.
        let prediction = self.evaluate(self.config.fallback_budget_us, key);
        self.calibrated = prediction.upper_us.is_some();
        self.state = prediction.state;
    }

    /// Predict the configured upper quantile for the next frame (p99 at alpha 0.01).
    ///
    /// `budget_us`: current frame budget in microseconds.
    /// `key`: bucket key for the upcoming rendering context.
    ///
    /// Returns a [`P99Prediction`] with the interval and risk assessment.
    pub fn predict_p99(&mut self, budget_us: f64, key: BucketKey) -> P99Prediction {
        let prediction = self.evaluate(budget_us, key);
        self.calibrated = prediction.upper_us.is_some();
        if prediction.exceeds_budget && (self.calibrated || self.state != GuardState::Warmup) {
            self.degradation_triggers += 1;
        }
        self.state = prediction.state;
        prediction
    }

    fn evaluate(&self, budget_us: f64, key: BucketKey) -> P99Prediction {
        let y_hat = if self.observations > 0 {
            self.ema_us
        } else {
            // The configured prior is available before the first outcome.
            // Zero would manufacture a large initial residual for normal work.
            self.config.fallback_budget_us
        };

        let prediction = self.predictor.predict(key, y_hat, budget_us);
        if self.predictor.is_calibrated(&prediction) {
            let exceeds = prediction.risk;

            let state = if exceeds {
                GuardState::AtRisk
            } else {
                GuardState::Calibrated
            };

            P99Prediction {
                y_hat_us: y_hat,
                upper_us: prediction.upper_us,
                budget_us,
                exceeds_budget: exceeds,
                calibration_size: prediction.sample_count,
                fallback_level: prediction.fallback_level,
                state,
                interval_width_us: prediction.upper_us.map(|upper| (upper - y_hat).max(0.0)),
                conformal: Some(prediction),
            }
        } else {
            // Fallback: fixed budget threshold (16ms default)
            // Independent measured fallback: respect a smaller caller budget.
            // It can trigger degradation, but cannot certify a recovery bound.
            let fallback = self.config.fallback_budget_us.min(budget_us);
            let exceeds = !budget_us.is_finite()
                || budget_us < 0.0
                || (self.observations > 0 && y_hat > fallback);

            // In warmup, signal risk only if EMA clearly exceeds fallback
            let state = if exceeds {
                GuardState::AtRisk
            } else if prediction.status == ConformalStatus::Warmup {
                GuardState::Warmup
            } else {
                GuardState::Unbounded
            };
            P99Prediction {
                y_hat_us: y_hat,
                upper_us: None,
                budget_us: fallback,
                exceeds_budget: exceeds,
                calibration_size: prediction.sample_count,
                fallback_level: 4, // Frame-guard fixed fallback
                state,
                interval_width_us: None,
                conformal: Some(prediction),
            }
        }
    }

    /// State of the last evaluation. After observation this uses the observed
    /// bucket and configured fallback budget; prediction uses its caller's budget.
    #[inline]
    pub fn state(&self) -> GuardState {
        self.state
    }

    /// Whether the last evaluation produced a finite conformal upper bound.
    #[inline]
    pub fn is_calibrated(&self) -> bool {
        self.calibrated
    }

    /// Total frame observations processed.
    #[inline]
    pub fn observations(&self) -> u64 {
        self.observations
    }

    /// Total degradation triggers.
    #[inline]
    pub fn degradation_triggers(&self) -> u64 {
        self.degradation_triggers
    }

    /// Access the rolling nonconformity scores.
    pub fn nonconformity_scores(&self) -> &VecDeque<f64> {
        &self.nonconformity_scores
    }

    /// Access the rolling frame time series.
    pub fn frame_times(&self) -> &VecDeque<f64> {
        &self.frame_times
    }

    /// Current EMA of frame times (µs).
    #[inline]
    pub fn ema_us(&self) -> f64 {
        self.ema_us
    }

    /// Access the underlying conformal predictor.
    pub fn predictor(&self) -> &ConformalPredictor {
        &self.predictor
    }

    /// Access the configuration.
    pub fn config(&self) -> &ConformalFrameGuardConfig {
        &self.config
    }

    /// Compute summary statistics for the nonconformity score distribution.
    ///
    /// Returns `(mean, p50, p90, p99, max)` or `None` if no scores exist.
    pub fn nonconformity_summary(&self) -> Option<NonconformitySummary> {
        if self.nonconformity_scores.is_empty() {
            return None;
        }

        let mut sorted: Vec<f64> = self.nonconformity_scores.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let n = sorted.len();
        // Divide before summing so a finite mean does not overflow through
        // an unnecessarily large intermediate total.
        let mean = sorted.iter().map(|value| value / n as f64).sum::<f64>();
        let p50 = sorted[n / 2];
        let p90 = sorted[(n as f64 * 0.90).ceil() as usize - 1];
        let p99 = sorted[(n as f64 * 0.99).ceil() as usize - 1];
        let max = sorted[n - 1];

        Some(NonconformitySummary {
            count: n,
            mean,
            p50,
            p90,
            p99,
            max,
        })
    }

    /// Reset all calibration state (e.g., after a mode change).
    pub fn reset(&mut self) {
        self.predictor.reset_all();
        self.frame_times.clear();
        self.nonconformity_scores.clear();
        self.ema_us = 0.0;
        self.state = GuardState::Warmup;
        self.calibrated = false;
        self.observations = 0;
        // Preserve degradation_triggers count across resets for audit trail
    }

    /// Suggest what degradation action to take based on the prediction.
    ///
    /// Returns `Some(DegradationLevel::next())` for a conformal or measured
    /// fallback overrun, unless already at maximum degradation.
    pub fn suggest_action(
        &self,
        prediction: &P99Prediction,
        current_level: DegradationLevel,
    ) -> Option<DegradationLevel> {
        if prediction.exceeds_budget && !current_level.is_max() {
            Some(current_level.next())
        } else {
            None
        }
    }

    /// Capture a telemetry snapshot for structured logging.
    pub fn telemetry(&self) -> ConformalFrameGuardTelemetry {
        ConformalFrameGuardTelemetry {
            state: self.state,
            observations: self.observations,
            degradation_triggers: self.degradation_triggers,
            ema_us: self.ema_us,
            frame_times_len: self.frame_times.len(),
            nonconformity_len: self.nonconformity_scores.len(),
            summary: self.nonconformity_summary(),
        }
    }
}

/// Summary statistics for nonconformity score distribution.
#[derive(Debug, Clone, Copy)]
pub struct NonconformitySummary {
    /// Number of scores in the window.
    pub count: usize,
    /// Mean nonconformity score.
    pub mean: f64,
    /// Median (p50).
    pub p50: f64,
    /// 90th percentile.
    pub p90: f64,
    /// 99th percentile.
    pub p99: f64,
    /// Maximum.
    pub max: f64,
}

impl NonconformitySummary {
    /// Format as a JSONL fragment (no outer braces).
    #[must_use]
    pub fn to_jsonl_fragment(&self) -> String {
        let number = crate::conformal_predictor::finite_json_number;
        format!(
            r#""nc_count":{},"nc_mean":{},"nc_p50":{},"nc_p90":{},"nc_p99":{},"nc_max":{}"#,
            self.count,
            number(Some(self.mean), 2),
            number(Some(self.p50), 2),
            number(Some(self.p90), 2),
            number(Some(self.p99), 2),
            number(Some(self.max), 2),
        )
    }
}

/// Telemetry snapshot of the conformal frame guard.
#[derive(Debug, Clone)]
pub struct ConformalFrameGuardTelemetry {
    /// Current guard state.
    pub state: GuardState,
    /// Total observations.
    pub observations: u64,
    /// Total degradation triggers.
    pub degradation_triggers: u64,
    /// Current EMA estimate (µs).
    pub ema_us: f64,
    /// Frame time window length.
    pub frame_times_len: usize,
    /// Nonconformity window length.
    pub nonconformity_len: usize,
    /// Nonconformity summary (if any scores exist).
    pub summary: Option<NonconformitySummary>,
}

impl ConformalFrameGuardTelemetry {
    /// Format as a JSONL line.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let summary_fields = self
            .summary
            .as_ref()
            .map(|s| format!(",{}", s.to_jsonl_fragment()))
            .unwrap_or_default();

        format!(
            r#"{{"schema":"conformal-frame-guard-telemetry-v1","state":"{}","observations":{},"degradation_triggers":{},"ema_us":{},"frame_times_len":{},"nonconformity_len":{}{}}}"#,
            self.state.as_str(),
            self.observations,
            self.degradation_triggers,
            crate::conformal_predictor::finite_json_number(Some(self.ema_us), 1),
            self.frame_times_len,
            self.nonconformity_len,
            summary_fields,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformal_predictor::{DiffBucket, ModeBucket};

    fn test_key() -> BucketKey {
        BucketKey {
            mode: ModeBucket::AltScreen,
            diff: DiffBucket::Full,
            size_bucket: 2,
        }
    }

    #[test]
    fn warmup_uses_fixed_fallback() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();

        // No observations yet
        let pred = guard.predict_p99(16_000.0, key);
        assert_eq!(pred.fallback_level, 4);
        assert_eq!(pred.state, GuardState::Warmup);
        assert!(!pred.exceeds_budget); // no measured overrun yet
        assert_eq!(
            pred.conformal.as_ref().unwrap().status,
            ConformalStatus::Warmup
        );
        assert!(pred.upper_us.is_none());
    }

    #[test]
    fn warmup_with_slow_frames_signals_risk() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();

        // Feed 5 slow frames (30ms each) — not enough for calibration
        for _ in 0..5 {
            guard.observe(30_000.0, key);
        }

        let pred = guard.predict_p99(16_000.0, key);
        assert_eq!(pred.fallback_level, 4);
        assert!(pred.exceeds_budget); // EMA ~30ms > 16ms fallback
        assert_eq!(pred.state, GuardState::AtRisk);
    }

    #[test]
    fn calibration_transitions_from_warmup() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();

        // Feed min_samples (20) fast frames
        for _ in 0..20 {
            guard.observe(8_000.0, key);
        }

        assert!(guard.is_calibrated());
        assert_eq!(guard.state(), GuardState::Calibrated);
    }

    #[test]
    fn calibrated_prediction_has_conformal_data() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();

        // Calibrate with 25 samples of ~10ms
        for _ in 0..25 {
            guard.observe(10_000.0, key);
        }

        let pred = guard.predict_p99(16_000.0, key);
        assert!(pred.conformal.is_some());
        assert!(pred.fallback_level < 4);
        // The first residual is scored against the configured cold prior;
        // subsequent stable observations have zero residual.
        assert_eq!(pred.upper_us, Some(10_000.0));
        assert!(!pred.exceeds_budget);
        assert_eq!(pred.state, GuardState::Calibrated);
        for _ in 25..40 {
            guard.observe(10_000.0, key);
        }
        let later = guard.predict_p99(16_000.0, key);
        assert_eq!(later.upper_us, Some(10_000.0));
        assert_eq!(later.state, GuardState::Calibrated);
    }

    #[test]
    fn scores_use_the_baseline_before_the_observation() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();
        assert_eq!(guard.predict_p99(16_000.0, key).y_hat_us, 16_000.0);
        guard.observe(10_000.0, key);
        assert_eq!(guard.nonconformity_scores().back(), Some(&-6_000.0));
        assert_eq!(guard.predict_p99(16_000.0, key).y_hat_us, 10_000.0);
        guard.observe(20_000.0, key);
        assert_eq!(guard.nonconformity_scores().back(), Some(&10_000.0));
        assert_eq!(guard.ema_us(), 10_500.0);
    }

    #[test]
    fn unavailable_rank_uses_measured_budget_without_claiming_a_bound() {
        let mut guard = ConformalFrameGuard::new(ConformalFrameGuardConfig {
            conformal: ConformalConfig {
                alpha: 0.01,
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("valid config");
        let key = test_key();
        for _ in 0..20 {
            guard.observe(10_000.0, key);
        }
        let safe_measurement = guard.predict_p99(16_000.0, key);
        assert_eq!(safe_measurement.state, GuardState::Unbounded);
        assert!(!guard.is_calibrated());
        assert_eq!(safe_measurement.upper_us, None);
        assert_eq!(safe_measurement.conformal.unwrap().required_rank, 21);
        let smaller_budget = guard.predict_p99(5_000.0, key);
        assert!(smaller_budget.exceeds_budget);
        assert!(!guard.is_calibrated(), "measured AtRisk is not calibrated");
        let row: serde_json::Value =
            serde_json::from_str(&smaller_budget.to_jsonl()).expect("valid JSON");
        assert!(row["upper_us"].is_null());
        assert!(row["conformal_confidence"].is_null());
        for _ in 20..99 {
            guard.observe(10_000.0, key);
        }
        assert!(guard.predict_p99(50_000.0, key).upper_us.is_some());
        assert!(guard.is_calibrated());
        guard.reset();
        assert!(!guard.is_calibrated());
        assert_eq!(guard.predict_p99(50_000.0, key).state, GuardState::Warmup);
    }

    #[test]
    fn guard_diagnostics_retain_the_predictors_conservative_score() {
        let mut guard = ConformalFrameGuard::new(ConformalFrameGuardConfig {
            conformal: ConformalConfig {
                alpha: 0.5,
                min_samples: 1,
                window_size: 1,
                ..Default::default()
            },
            fallback_budget_us: 1.005,
            ..Default::default()
        })
        .expect("valid config");
        guard.observe(10.001, test_key());
        let prediction = guard.predict_p99(100.0, test_key());
        let score = prediction.conformal.unwrap().quantile.unwrap();
        assert!(
            score > 10.001 - 1.005,
            "rounded-down score must be corrected"
        );
        assert_eq!(guard.nonconformity_scores().back(), Some(&score));
    }

    #[test]
    fn invalid_guard_configuration_is_rejected() {
        for fallback_budget_us in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(
                ConformalFrameGuard::new(ConformalFrameGuardConfig {
                    fallback_budget_us,
                    ..Default::default()
                })
                .is_err()
            );
        }
        for (time_series_window, nonconformity_window) in [(0, 20), (20, 0)] {
            assert!(
                ConformalFrameGuard::new(ConformalFrameGuardConfig {
                    time_series_window,
                    nonconformity_window,
                    ..Default::default()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn observation_metadata_uses_pooled_calibration_and_actual_upper_bound() {
        let config = ConformalFrameGuardConfig {
            conformal: ConformalConfig {
                alpha: 0.5,
                min_samples: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut guard = ConformalFrameGuard::new(config).expect("valid config");
        let first = test_key();
        let second = BucketKey {
            size_bucket: first.size_bucket + 1,
            ..first
        };
        guard.observe(10_000.0, first);
        assert!(!guard.is_calibrated());
        guard.observe(10_000.0, second);
        assert!(guard.is_calibrated(), "two pooled scores provide rank two");
        assert_eq!(guard.state(), GuardState::Calibrated);
        let prediction = guard.predict_p99(16_000.0, second);
        assert_eq!(prediction.calibration_size, 2);
        assert_eq!(prediction.fallback_level, 1);
        assert_eq!(prediction.upper_us, Some(10_000.0));

        let mut extreme = ConformalFrameGuard::new(ConformalFrameGuardConfig {
            conformal: ConformalConfig {
                alpha: 0.5,
                min_samples: 1,
                window_size: 1,
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("valid config");
        extreme.observe(f64::MAX, first);
        assert!(
            !extreme.is_calibrated(),
            "attainable rank cannot hide overflow"
        );
        assert_eq!(extreme.state(), GuardState::AtRisk);
        assert_eq!(
            extreme.degradation_triggers(),
            0,
            "observation is not a decision"
        );
        let prediction = extreme.predict_p99(f64::MAX, first);
        assert_eq!(
            prediction.conformal.unwrap().status,
            ConformalStatus::ArithmeticOverflow
        );
        assert!(!extreme.is_calibrated());
    }

    #[test]
    fn telemetry_remains_json_for_extreme_finite_observations() {
        let mut guard = ConformalFrameGuard::with_defaults();
        for value in [f64::MAX, 0.0, f64::MAX, 0.0] {
            guard.observe(value, test_key());
        }
        let row: serde_json::Value =
            serde_json::from_str(&guard.telemetry().to_jsonl()).expect("valid telemetry JSON");
        assert!(row["ema_us"].is_number());
        assert!(row["nc_mean"].is_number());
    }

    #[test]
    fn calibrated_slow_frames_trigger_at_risk() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();

        // Calibrate with slow frames (20ms)
        for _ in 0..25 {
            guard.observe(20_000.0, key);
        }

        let pred = guard.predict_p99(16_000.0, key);
        assert!(pred.exceeds_budget);
        assert_eq!(pred.state, GuardState::AtRisk);
        assert!(guard.degradation_triggers() > 0);
    }

    #[test]
    fn nonconformity_scores_tracked() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();

        for i in 0..10 {
            guard.observe(10_000.0 + (i as f64 * 100.0), key);
        }

        assert_eq!(guard.nonconformity_scores().len(), 10);
        assert_eq!(guard.frame_times().len(), 10);
    }

    #[test]
    fn nonconformity_summary_computes_percentiles() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();

        // Feed 100 samples with known distribution
        for i in 0..100 {
            guard.observe(10_000.0 + (i as f64 * 100.0), key);
        }

        let summary = guard.nonconformity_summary();
        assert!(summary.is_some());
        let s = summary.unwrap();
        assert_eq!(s.count, 100);
        assert!(s.p99 >= s.p90);
        assert!(s.p90 >= s.p50);
        assert!(s.max >= s.p99);
    }

    #[test]
    fn reset_clears_state_but_preserves_triggers() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();

        // Feed slow frames to trigger degradation
        for _ in 0..25 {
            guard.observe(20_000.0, key);
        }
        let _ = guard.predict_p99(16_000.0, key);
        let triggers_before = guard.degradation_triggers();
        assert!(triggers_before > 0);

        guard.reset();

        assert_eq!(guard.state(), GuardState::Warmup);
        assert_eq!(guard.observations(), 0);
        assert!(guard.frame_times().is_empty());
        assert!(guard.nonconformity_scores().is_empty());
        // Triggers preserved for audit trail
        assert_eq!(guard.degradation_triggers(), triggers_before);
    }

    #[test]
    fn suggest_action_degrades_when_at_risk() {
        let guard = ConformalFrameGuard::with_defaults();

        let pred = P99Prediction {
            y_hat_us: 18_000.0,
            upper_us: Some(20_000.0),
            budget_us: 16_000.0,
            exceeds_budget: true,
            calibration_size: 25,
            fallback_level: 0,
            state: GuardState::AtRisk,
            interval_width_us: Some(2_000.0),
            conformal: None,
        };

        let action = guard.suggest_action(&pred, DegradationLevel::Full);
        assert_eq!(action, Some(DegradationLevel::SimpleBorders));
    }

    #[test]
    fn suggest_action_holds_at_max_degradation() {
        let guard = ConformalFrameGuard::with_defaults();

        let pred = P99Prediction {
            y_hat_us: 30_000.0,
            upper_us: Some(35_000.0),
            budget_us: 16_000.0,
            exceeds_budget: true,
            calibration_size: 25,
            fallback_level: 0,
            state: GuardState::AtRisk,
            interval_width_us: Some(5_000.0),
            conformal: None,
        };

        let action = guard.suggest_action(&pred, DegradationLevel::SkipFrame);
        assert!(action.is_none());
    }

    #[test]
    fn suggest_action_holds_when_within_budget() {
        let guard = ConformalFrameGuard::with_defaults();

        let pred = P99Prediction {
            y_hat_us: 10_000.0,
            upper_us: Some(14_000.0),
            budget_us: 16_000.0,
            exceeds_budget: false,
            calibration_size: 25,
            fallback_level: 0,
            state: GuardState::Calibrated,
            interval_width_us: Some(4_000.0),
            conformal: None,
        };

        let action = guard.suggest_action(&pred, DegradationLevel::Full);
        assert!(action.is_none());
    }

    #[test]
    fn ema_tracks_frame_times() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();

        // All 10ms frames
        for _ in 0..50 {
            guard.observe(10_000.0, key);
        }

        // EMA should converge close to 10_000
        let ema = guard.ema_us();
        assert!(
            (ema - 10_000.0).abs() < 500.0,
            "EMA should be ~10000, got {ema}"
        );
    }

    #[test]
    fn invalid_frame_time_ignored() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();

        guard.observe(f64::NAN, key);
        guard.observe(f64::INFINITY, key);
        guard.observe(-1.0, key);

        assert_eq!(guard.observations(), 0);
        assert!(guard.frame_times().is_empty());
    }

    #[test]
    fn jsonl_output_is_valid_json() {
        let pred = P99Prediction {
            y_hat_us: 10_000.0,
            upper_us: Some(14_000.0),
            budget_us: 16_000.0,
            exceeds_budget: false,
            calibration_size: 25,
            fallback_level: 0,
            state: GuardState::Calibrated,
            interval_width_us: Some(4_000.0),
            conformal: None,
        };

        let json_str = pred.to_jsonl();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("valid JSON");
        assert_eq!(parsed["schema"], "conformal-frame-guard-v2");
        assert_eq!(parsed["upper_us"], 14_000.0);
    }

    #[test]
    fn telemetry_snapshot_captures_state() {
        let mut guard = ConformalFrameGuard::with_defaults();
        let key = test_key();

        for _ in 0..30 {
            guard.observe(12_000.0, key);
        }

        let telem = guard.telemetry();
        assert_eq!(telem.observations, 30);
        assert_eq!(telem.frame_times_len, 30);
        assert_eq!(telem.nonconformity_len, 30);
        assert!(telem.summary.is_some());

        let json_str = telem.to_jsonl();
        assert!(json_str.contains("conformal-frame-guard-telemetry-v1"));
    }

    #[test]
    fn window_limits_respected() {
        let config = ConformalFrameGuardConfig {
            time_series_window: 10,
            nonconformity_window: 5,
            ..Default::default()
        };
        let mut guard = ConformalFrameGuard::new(config).expect("valid config");
        let key = test_key();

        for i in 0..100 {
            guard.observe(10_000.0 + (i as f64), key);
        }

        assert_eq!(guard.frame_times().len(), 10);
        assert_eq!(guard.nonconformity_scores().len(), 5);
    }
}

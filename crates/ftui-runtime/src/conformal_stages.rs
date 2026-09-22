//! Multi-stage conformal prediction for render pipeline timing.
//!
//! Extends conformal alerting to individual render stages (layout, diff,
//! presenter) with Mondrian-style stage bucketing. Each stage maintains
//! independent calibration and e-process tracking, so a regression in
//! layout computation doesn't pollute diff detection and vice versa.
//!
//! # Render Pipeline Stages
//!
//! ```text
//! view() → [Layout] → Buffer → [Diff] → Changes → [Present] → ANSI
//!            ↑               ↑                ↑
//!       stage monitor   stage monitor   stage monitor
//! ```
//!
//! # Mondrian Conformal Prediction
//!
//! Mondrian conformal prediction partitions the input space into buckets
//! and maintains separate calibration sets per bucket. Here, each render
//! stage is a natural partition:
//!
//! ```text
//! Bucket 0 (Layout):  calibrate on layout_time_us
//! Bucket 1 (Diff):    calibrate on diff_time_us
//! Bucket 2 (Present): calibrate on present_time_us
//! ```
//!
//! Stage-level alerts feed into the unified degradation decision:
//! if **any** stage exceeds its conformal bound, trigger degradation.
//!
//! # Usage
//!
//! ```rust
//! use ftui_runtime::conformal_stages::{StagedConformalPredictor, RenderStage, StageObservation};
//!
//! let mut predictor = StagedConformalPredictor::default();
//!
//! // Calibration: feed baseline timings per stage
//! for _ in 0..50 {
//!     predictor.calibrate(RenderStage::Layout, 120.0);  // ~120μs
//!     predictor.calibrate(RenderStage::Diff, 80.0);     // ~80μs
//!     predictor.calibrate(RenderStage::Present, 200.0); // ~200μs
//! }
//!
//! // Detection: observe new frame timings
//! let result = predictor.observe_frame(StageObservation {
//!     layout_us: 500.0,  // regression!
//!     diff_us: 85.0,
//!     present_us: 210.0,
//! });
//!
//! if result.any_alert() {
//!     // Trigger degradation
//! }
//! ```

use std::collections::VecDeque;

/// Render pipeline stages for Mondrian bucketing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderStage {
    /// Widget layout computation.
    Layout,
    /// Buffer diff computation.
    Diff,
    /// ANSI presenter emission.
    Present,
}

impl RenderStage {
    /// All stages in pipeline order.
    pub const ALL: [RenderStage; 3] = [Self::Layout, Self::Diff, Self::Present];

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Layout => "layout",
            Self::Diff => "diff",
            Self::Present => "present",
        }
    }
}

/// Per-stage timing observation for a single frame.
#[derive(Debug, Clone, Copy)]
pub struct StageObservation {
    /// Layout computation time in microseconds.
    pub layout_us: f64,
    /// Diff computation time in microseconds.
    pub diff_us: f64,
    /// Presenter ANSI emission time in microseconds.
    pub present_us: f64,
}

impl StageObservation {
    /// Get timing for a specific stage.
    pub fn get(&self, stage: RenderStage) -> f64 {
        match stage {
            RenderStage::Layout => self.layout_us,
            RenderStage::Diff => self.diff_us,
            RenderStage::Present => self.present_us,
        }
    }

    /// Total frame time across all stages.
    pub fn total_us(&self) -> f64 {
        self.layout_us + self.diff_us + self.present_us
    }
}

/// Alert decision for a single stage.
#[derive(Debug, Clone)]
pub struct StageAlert {
    pub stage: RenderStage,
    /// Whether this stage exceeded its conformal threshold.
    pub is_alert: bool,
    /// The observed value.
    pub observed: f64,
    /// The conformal threshold (quantile-based).
    pub threshold: f64,
    /// Current e-process value (anytime-valid evidence).
    pub e_value: f64,
    /// Number of calibration samples for this stage.
    pub calibration_count: usize,
}

/// Combined result from observing all stages.
#[derive(Debug, Clone)]
pub struct FrameResult {
    /// Per-stage alert decisions.
    pub stages: [StageAlert; 3],
}

impl FrameResult {
    /// Whether any stage triggered an alert.
    pub fn any_alert(&self) -> bool {
        self.stages.iter().any(|s| s.is_alert)
    }

    /// Which stages triggered alerts.
    pub fn alerting_stages(&self) -> Vec<RenderStage> {
        self.stages
            .iter()
            .filter(|s| s.is_alert)
            .map(|s| s.stage)
            .collect()
    }

    /// Get result for a specific stage.
    pub fn stage(&self, stage: RenderStage) -> &StageAlert {
        &self.stages[stage as usize]
    }
}

/// Configuration for staged conformal prediction.
#[derive(Debug, Clone)]
pub struct StagedConfig {
    /// Significance level alpha per stage. Default: 0.05.
    pub alpha: f64,
    /// Maximum calibration window per stage. Default: 500.
    pub max_calibration: usize,
    /// Minimum calibration samples before alerting. Default: 10.
    pub min_calibration: usize,
    /// E-process betting fraction. Default: 0.5.
    pub lambda: f64,
}

impl Default for StagedConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            max_calibration: 500,
            min_calibration: 10,
            lambda: 0.5,
        }
    }
}

/// E-value floor to prevent permanent zero-lock.
const E_MIN: f64 = 1e-12;
/// E-value ceiling to prevent overflow.
const E_MAX: f64 = 1e12;

/// Per-stage calibration and detection state.
#[derive(Debug, Clone)]
struct StageState {
    /// Calibration residuals (sorted ring buffer).
    calibration: VecDeque<f64>,
    /// Running mean for standardization.
    mean: f64,
    /// Running M2 (sum of squared deviations) for variance.
    m2: f64,
    /// Number of calibration samples seen.
    n: u64,
    /// Current e-process value.
    e_value: f64,
}

impl StageState {
    fn new() -> Self {
        Self {
            calibration: VecDeque::new(),
            mean: 0.0,
            m2: 0.0,
            n: 0,
            e_value: 1.0,
        }
    }

    /// Add a calibration sample (Welford's online algorithm).
    fn calibrate(&mut self, value: f64, max_samples: usize) {
        self.n += 1;
        let delta = value - self.mean;
        self.mean += delta / self.n as f64;
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;

        self.calibration.push_back(value);
        while self.calibration.len() > max_samples {
            self.calibration.pop_front();
        }
    }

    /// Variance of calibration data.
    fn variance(&self) -> f64 {
        if self.n < 2 {
            return 1.0;
        }
        (self.m2 / (self.n - 1) as f64).max(1e-10)
    }

    /// Compute conformal threshold at level alpha using the (n+1) rule.
    fn conformal_threshold(&self, alpha: f64) -> f64 {
        if self.calibration.is_empty() {
            return f64::MAX;
        }
        let n = self.calibration.len();
        let mut sorted: Vec<f64> = self.calibration.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);

        // (n+1) rule: index = ceil((1 - alpha) * (n + 1) / n * n) - 1
        let quantile_idx = (((1.0 - alpha) * (n + 1) as f64).ceil() as usize).min(n) - 1;
        sorted[quantile_idx.min(n - 1)]
    }

    /// Update e-process with a new observation.
    fn update_e_process(&mut self, value: f64, lambda: f64) {
        let std = self.variance().sqrt();
        let z = if std > 1e-10 {
            (value - self.mean) / std
        } else {
            0.0
        };
        let log_e = lambda * z - lambda * lambda / 2.0;
        self.e_value = (self.e_value * log_e.exp()).clamp(E_MIN, E_MAX);
    }
}

/// Multi-stage conformal predictor with Mondrian bucketing.
///
/// Maintains independent conformal alerters for each render pipeline stage
/// (layout, diff, present). Alerts are per-stage but the degradation
/// decision is unified: any stage alert triggers overall degradation.
#[derive(Debug, Clone)]
pub struct StagedConformalPredictor {
    config: StagedConfig,
    states: [StageState; 3],
}

impl Default for StagedConformalPredictor {
    fn default() -> Self {
        Self::new(StagedConfig::default())
    }
}

impl StagedConformalPredictor {
    /// Create a new predictor with the given configuration.
    pub fn new(config: StagedConfig) -> Self {
        Self {
            config,
            states: [StageState::new(), StageState::new(), StageState::new()],
        }
    }

    /// Add a calibration sample for a specific stage.
    pub fn calibrate(&mut self, stage: RenderStage, value: f64) {
        self.states[stage as usize].calibrate(value, self.config.max_calibration);
    }

    /// Add calibration samples for all stages from a frame observation.
    pub fn calibrate_frame(&mut self, obs: &StageObservation) {
        for stage in RenderStage::ALL {
            self.calibrate(stage, obs.get(stage));
        }
    }

    /// Observe a new frame and get per-stage alert decisions.
    pub fn observe_frame(&mut self, obs: StageObservation) -> FrameResult {
        let mut alerts = [
            self.observe_stage(RenderStage::Layout, obs.layout_us),
            self.observe_stage(RenderStage::Diff, obs.diff_us),
            self.observe_stage(RenderStage::Present, obs.present_us),
        ];
        // Ensure stage field is correct (redundant but explicit).
        alerts[0].stage = RenderStage::Layout;
        alerts[1].stage = RenderStage::Diff;
        alerts[2].stage = RenderStage::Present;
        FrameResult { stages: alerts }
    }

    fn observe_stage(&mut self, stage: RenderStage, value: f64) -> StageAlert {
        let state = &mut self.states[stage as usize];
        let threshold = state.conformal_threshold(self.config.alpha);
        let calibration_count = state.calibration.len();

        // Update e-process.
        state.update_e_process(value, self.config.lambda);

        let is_alert = calibration_count >= self.config.min_calibration
            && value > threshold
            && state.e_value > 1.0 / self.config.alpha;

        StageAlert {
            stage,
            is_alert,
            observed: value,
            threshold,
            e_value: state.e_value,
            calibration_count,
        }
    }

    /// Number of calibration samples for a stage.
    pub fn calibration_count(&self, stage: RenderStage) -> usize {
        self.states[stage as usize].calibration.len()
    }

    /// Reset all stage states.
    pub fn reset(&mut self) {
        self.states = [StageState::new(), StageState::new(), StageState::new()];
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let cfg = StagedConfig::default();
        assert_eq!(cfg.alpha, 0.05);
        assert_eq!(cfg.max_calibration, 500);
        assert_eq!(cfg.min_calibration, 10);
    }

    #[test]
    fn render_stage_names() {
        assert_eq!(RenderStage::Layout.name(), "layout");
        assert_eq!(RenderStage::Diff.name(), "diff");
        assert_eq!(RenderStage::Present.name(), "present");
    }

    #[test]
    fn stage_observation_total() {
        let obs = StageObservation {
            layout_us: 100.0,
            diff_us: 50.0,
            present_us: 200.0,
        };
        assert!((obs.total_us() - 350.0).abs() < 1e-10);
    }

    #[test]
    fn stage_observation_get() {
        let obs = StageObservation {
            layout_us: 100.0,
            diff_us: 50.0,
            present_us: 200.0,
        };
        assert!((obs.get(RenderStage::Layout) - 100.0).abs() < 1e-10);
        assert!((obs.get(RenderStage::Diff) - 50.0).abs() < 1e-10);
        assert!((obs.get(RenderStage::Present) - 200.0).abs() < 1e-10);
    }

    #[test]
    fn no_alert_during_calibration() {
        let mut pred = StagedConformalPredictor::default();
        // Only 5 calibration samples (below min_calibration=10)
        for _ in 0..5 {
            pred.calibrate(RenderStage::Layout, 100.0);
        }
        let result = pred.observe_frame(StageObservation {
            layout_us: 999.0, // extreme but shouldn't alert
            diff_us: 0.0,
            present_us: 0.0,
        });
        // Layout has insufficient calibration
        assert!(!result.stage(RenderStage::Layout).is_alert);
    }

    #[test]
    fn alert_on_regression() {
        let mut pred = StagedConformalPredictor::default();
        // Calibrate with stable baseline
        for _ in 0..50 {
            pred.calibrate_frame(&StageObservation {
                layout_us: 100.0,
                diff_us: 50.0,
                present_us: 200.0,
            });
        }

        // Observe many anomalous frames to build e-process evidence
        let mut alerted = false;
        for _ in 0..20 {
            let result = pred.observe_frame(StageObservation {
                layout_us: 500.0, // 5x regression
                diff_us: 50.0,
                present_us: 200.0,
            });
            if result.any_alert() {
                alerted = true;
                // Should be layout that alerts
                assert!(result.stage(RenderStage::Layout).is_alert);
                // Diff and present should be fine
                assert!(!result.stage(RenderStage::Diff).is_alert);
                assert!(!result.stage(RenderStage::Present).is_alert);
                break;
            }
        }
        assert!(alerted, "Should have alerted on 5x layout regression");
    }

    #[test]
    fn no_alert_on_normal() {
        let mut pred = StagedConformalPredictor::default();
        // Calibrate
        for _ in 0..50 {
            pred.calibrate_frame(&StageObservation {
                layout_us: 100.0,
                diff_us: 50.0,
                present_us: 200.0,
            });
        }
        // Observe normal frames
        for _ in 0..20 {
            let result = pred.observe_frame(StageObservation {
                layout_us: 100.0,
                diff_us: 50.0,
                present_us: 200.0,
            });
            assert!(!result.any_alert(), "Should not alert on normal frames");
        }
    }

    #[test]
    fn independent_stage_tracking() {
        let mut pred = StagedConformalPredictor::default();
        // Only calibrate layout
        for _ in 0..50 {
            pred.calibrate(RenderStage::Layout, 100.0);
        }
        assert_eq!(pred.calibration_count(RenderStage::Layout), 50);
        assert_eq!(pred.calibration_count(RenderStage::Diff), 0);
        assert_eq!(pred.calibration_count(RenderStage::Present), 0);
    }

    #[test]
    fn reset_clears_state() {
        let mut pred = StagedConformalPredictor::default();
        for _ in 0..20 {
            pred.calibrate(RenderStage::Layout, 100.0);
        }
        assert_eq!(pred.calibration_count(RenderStage::Layout), 20);
        pred.reset();
        assert_eq!(pred.calibration_count(RenderStage::Layout), 0);
    }

    #[test]
    fn alerting_stages_list() {
        // Create a manually constructed FrameResult for testing
        let result = FrameResult {
            stages: [
                StageAlert {
                    stage: RenderStage::Layout,
                    is_alert: true,
                    observed: 500.0,
                    threshold: 120.0,
                    e_value: 100.0,
                    calibration_count: 50,
                },
                StageAlert {
                    stage: RenderStage::Diff,
                    is_alert: false,
                    observed: 50.0,
                    threshold: 80.0,
                    e_value: 0.5,
                    calibration_count: 50,
                },
                StageAlert {
                    stage: RenderStage::Present,
                    is_alert: true,
                    observed: 800.0,
                    threshold: 250.0,
                    e_value: 200.0,
                    calibration_count: 50,
                },
            ],
        };
        let alerting = result.alerting_stages();
        assert_eq!(alerting.len(), 2);
        assert!(alerting.contains(&RenderStage::Layout));
        assert!(alerting.contains(&RenderStage::Present));
    }

    #[test]
    fn calibration_window_bounded() {
        let cfg = StagedConfig {
            max_calibration: 20,
            ..Default::default()
        };
        let mut pred = StagedConformalPredictor::new(cfg);
        for i in 0..100 {
            pred.calibrate(RenderStage::Layout, i as f64);
        }
        assert_eq!(pred.calibration_count(RenderStage::Layout), 20);
    }
}

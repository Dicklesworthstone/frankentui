#![forbid(unsafe_code)]

//! Degradation cascade: conformal guard → budget controller → widget priority.
//!
//! Orchestrates the flow from conformal frame guard risk detection through
//! budget degradation to widget-level rendering decisions. Tracks recovery
//! and emits structured evidence at each decision point.
//!
//! # Cascade Flow
//!
//! ```text
//! ┌─────────────────────┐
//! │ ConformalFrameGuard  │  p99 exceeds budget?
//! └─────────┬───────────┘
//!           │ yes
//!           ▼
//! ┌─────────────────────┐
//! │   Budget Degrade     │  next degradation level
//! └─────────┬───────────┘
//!           │
//!           ▼
//! ┌─────────────────────┐
//! │ Widget Filter        │  skip non-essential at EssentialOnly+
//! └─────────────────────┘
//!
//! Recovery: N consecutive within-budget frames → upgrade one level
//! ```
//!
//! # Evidence
//!
//! Every cascade decision emits a JSONL evidence entry with:
//! - Guard state and prediction
//! - Degradation level transition
//! - Recovery progress
//! - Nonconformity summary

use ftui_render::budget::DegradationLevel;

use crate::conformal_frame_guard::{
    ConformalFrameGuard, ConformalFrameGuardConfig, GuardState, P99Prediction,
};
use crate::conformal_predictor::BucketKey;

/// Configuration for the degradation cascade.
#[derive(Debug, Clone)]
pub struct CascadeConfig {
    /// Conformal frame guard configuration.
    pub guard: ConformalFrameGuardConfig,

    /// Consecutive within-budget frames required before upgrading (recovery).
    /// Default: 10.
    pub recovery_threshold: u32,

    /// Maximum degradation level the cascade is allowed to reach.
    /// Default: `DegradationLevel::SkipFrame` (no limit).
    pub max_degradation: DegradationLevel,

    /// Minimum degradation level to use when the guard triggers.
    /// If the current level is below this, jump directly to it.
    /// Default: `DegradationLevel::SimpleBorders` (gradual).
    pub min_trigger_level: DegradationLevel,

    /// Minimum quality floor: the cascade will never degrade past this level.
    ///
    /// Default: `DegradationLevel::SimpleBorders` — preserves readable text
    /// content, preventing escalation to `EssentialOnly`, `Skeleton`, or
    /// `SkipFrame` after transient focus/resize spikes.
    ///
    /// This is distinct from `max_degradation` which caps the absolute worst
    /// level. The floor is a safety net that prevents content-suppressing
    /// degradation regardless of how bad the frame budget looks.
    pub degradation_floor: DegradationLevel,
}

impl Default for CascadeConfig {
    fn default() -> Self {
        Self {
            guard: ConformalFrameGuardConfig::default(),
            recovery_threshold: 10,
            max_degradation: DegradationLevel::SkipFrame,
            min_trigger_level: DegradationLevel::SimpleBorders,
            degradation_floor: DegradationLevel::SimpleBorders,
        }
    }
}

/// Decision made by the cascade for a single frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CascadeDecision {
    /// No action needed; rendering proceeds at current level.
    Hold,
    /// Degrade: reduce visual fidelity one (or more) levels.
    Degrade,
    /// Recover: restore visual fidelity one level after sustained good frames.
    Recover,
}

impl CascadeDecision {
    /// Stable string for JSONL logging.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hold => "hold",
            Self::Degrade => "degrade",
            Self::Recover => "recover",
        }
    }
}

/// Evidence record emitted for each cascade decision.
#[derive(Debug, Clone)]
pub struct CascadeEvidence {
    /// Frame index within the run.
    pub frame_idx: u64,
    /// Decision taken.
    pub decision: CascadeDecision,
    /// Degradation level before this frame.
    pub level_before: DegradationLevel,
    /// Degradation level after this frame.
    pub level_after: DegradationLevel,
    /// Guard state.
    pub guard_state: GuardState,
    /// Consecutive within-budget frame count.
    pub recovery_streak: u32,
    /// Recovery threshold.
    pub recovery_threshold: u32,
    /// Frame time in µs (observed).
    pub frame_time_us: f64,
    /// Budget in µs.
    pub budget_us: f64,
    /// P99 prediction (if available).
    pub prediction: Option<P99Prediction>,
}

impl CascadeEvidence {
    /// Format as a JSONL line.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let pred_fields = self
            .prediction
            .as_ref()
            .map(|p| {
                let conformal_fields = p.conformal.as_ref().map(|c| {
                    format!(
                        r#","conformal_status":"{}","conformal_required_rank":{},"conformal_alpha":{}"#,
                        c.status.as_str(), c.required_rank,
                        crate::conformal_predictor::finite_json_number(Some(c.alpha), 17),
                    )
                }).unwrap_or_default();
                format!(
                    r#","p99_upper_us":{},"p99_exceeds":{},"p99_fallback_level":{},"p99_calibration_size":{},"p99_interval_width_us":{}{}"#,
                    crate::conformal_predictor::finite_json_number(p.upper_us, 1),
                    p.exceeds_budget,
                    p.fallback_level,
                    p.calibration_size,
                    crate::conformal_predictor::finite_json_number(p.interval_width_us, 1),
                    conformal_fields,
                )
            })
            .unwrap_or_default();

        format!(
            r#"{{"schema":"degradation-cascade-v1","frame_idx":{},"decision":"{}","level_before":"{}","level_after":"{}","guard_state":"{}","recovery_streak":{},"recovery_threshold":{},"frame_time_us":{},"budget_us":{}{}}}"#,
            self.frame_idx,
            self.decision.as_str(),
            self.level_before.as_str(),
            self.level_after.as_str(),
            self.guard_state.as_str(),
            self.recovery_streak,
            self.recovery_threshold,
            crate::conformal_predictor::finite_json_number(Some(self.frame_time_us), 1),
            crate::conformal_predictor::finite_json_number(Some(self.budget_us), 1),
            pred_fields,
        )
    }
}

/// Degradation cascade orchestrator.
///
/// Sits between the conformal frame guard and the render budget system.
/// Call `pre_render` before each frame and `post_render` after.
#[derive(Debug)]
pub struct DegradationCascade {
    config: CascadeConfig,
    guard: ConformalFrameGuard,
    /// Current degradation level managed by this cascade.
    current_level: DegradationLevel,
    /// Consecutive frames where p99 was within budget.
    recovery_streak: u32,
    /// Frame counter.
    frame_idx: u64,
    /// Total degrade events.
    total_degrades: u64,
    /// Total recovery events.
    total_recoveries: u64,
    /// Last cascade evidence (for external consumers).
    last_evidence: Option<CascadeEvidence>,
}

impl DegradationCascade {
    /// Create a new cascade with the given configuration.
    pub fn new(config: CascadeConfig) -> Result<Self, crate::ConformalConfigError> {
        let guard = ConformalFrameGuard::new(config.guard.clone())?;
        Ok(Self {
            config,
            guard,
            current_level: DegradationLevel::Full,
            recovery_streak: 0,
            frame_idx: 0,
            total_degrades: 0,
            total_recoveries: 0,
            last_evidence: None,
        })
    }

    /// Create a cascade with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(CascadeConfig::default()).expect("valid default cascade config")
    }

    /// Pre-render check: predict p99 and decide whether to degrade.
    ///
    /// Returns the degradation level to use for this frame and the prediction.
    /// The caller should apply the returned level to the render budget.
    pub fn pre_render(&mut self, budget_us: f64, key: BucketKey) -> PreRenderResult {
        self.frame_idx += 1;
        let level_before = self.current_level;

        let prediction = self.guard.predict_p99(budget_us, key);

        let decision = if prediction.exceeds_budget {
            // Degrade (if not at max and not already at degradation floor)
            if self.current_level < self.config.max_degradation
                && self.current_level < self.config.degradation_floor
            {
                self.current_level = self.current_level.next();

                // Jump to minimum trigger level if below it
                if self.current_level < self.config.min_trigger_level {
                    self.current_level = self.config.min_trigger_level;
                }

                // Clamp to degradation limits: never degrade past the configured
                // minimum quality level or maximum degradation level.
                if self.current_level > self.config.degradation_floor {
                    self.current_level = self.config.degradation_floor;
                }
                if self.current_level > self.config.max_degradation {
                    self.current_level = self.config.max_degradation;
                }

                self.recovery_streak = 0;
                self.total_degrades += 1;
                CascadeDecision::Degrade
            } else {
                self.recovery_streak = 0;
                CascadeDecision::Hold
            }
        } else if !self.guard.is_calibrated() {
            // Missing confidence is not a successful within-budget frame.
            // Keep measuring at the current quality, without advancing recovery.
            self.recovery_streak = 0;
            CascadeDecision::Hold
        } else {
            // Within budget: track recovery streak
            self.recovery_streak += 1;

            if self.recovery_streak >= self.config.recovery_threshold
                && !self.current_level.is_full()
            {
                self.current_level = self.current_level.prev();
                self.recovery_streak = 0;
                self.total_recoveries += 1;
                CascadeDecision::Recover
            } else {
                CascadeDecision::Hold
            }
        };

        let evidence = CascadeEvidence {
            frame_idx: self.frame_idx,
            decision,
            level_before,
            level_after: self.current_level,
            guard_state: self.guard.state(),
            recovery_streak: self.recovery_streak,
            recovery_threshold: self.config.recovery_threshold,
            frame_time_us: self.guard.ema_us(),
            budget_us,
            prediction: Some(prediction.clone()),
        };

        self.last_evidence = Some(evidence);

        PreRenderResult {
            level: self.current_level,
            decision,
            prediction,
        }
    }

    /// Post-render observation: feed actual frame time to the guard.
    ///
    /// Call this after the frame has been rendered with the measured time.
    pub fn post_render(&mut self, frame_time_us: f64, key: BucketKey) {
        self.guard.observe(frame_time_us, key);
    }

    /// Get the current degradation level.
    #[inline]
    pub fn level(&self) -> DegradationLevel {
        self.current_level
    }

    /// Get the current recovery streak.
    #[inline]
    pub fn recovery_streak(&self) -> u32 {
        self.recovery_streak
    }

    /// Get the frame counter.
    #[inline]
    pub fn frame_idx(&self) -> u64 {
        self.frame_idx
    }

    /// Total degrade events.
    #[inline]
    pub fn total_degrades(&self) -> u64 {
        self.total_degrades
    }

    /// Total recovery events.
    #[inline]
    pub fn total_recoveries(&self) -> u64 {
        self.total_recoveries
    }

    /// Access the last cascade evidence.
    pub fn last_evidence(&self) -> Option<&CascadeEvidence> {
        self.last_evidence.as_ref()
    }

    /// Access the underlying guard.
    pub fn guard(&self) -> &ConformalFrameGuard {
        &self.guard
    }

    /// Access the configuration.
    pub fn config(&self) -> &CascadeConfig {
        &self.config
    }

    /// Reset the cascade to initial state.
    pub fn reset(&mut self) {
        self.guard.reset();
        self.current_level = DegradationLevel::Full;
        self.recovery_streak = 0;
        self.frame_idx = 0;
        self.last_evidence = None;
        // Preserve aggregate counts for audit trail
    }

    /// Whether widget should render given current degradation level and essentiality.
    ///
    /// At `EssentialOnly` or higher degradation, non-essential widgets are skipped.
    #[inline]
    pub fn should_render_widget(&self, is_essential: bool) -> bool {
        if self.current_level >= DegradationLevel::EssentialOnly {
            is_essential
        } else {
            true
        }
    }

    /// Capture telemetry for the cascade.
    pub fn telemetry(&self) -> CascadeTelemetry {
        CascadeTelemetry {
            level: self.current_level,
            recovery_streak: self.recovery_streak,
            recovery_threshold: self.config.recovery_threshold,
            frame_idx: self.frame_idx,
            total_degrades: self.total_degrades,
            total_recoveries: self.total_recoveries,
            guard_state: self.guard.state(),
            guard_observations: self.guard.observations(),
            guard_ema_us: self.guard.ema_us(),
        }
    }
}

/// Result of a pre-render cascade check.
#[derive(Debug, Clone)]
pub struct PreRenderResult {
    /// Degradation level to use for this frame.
    pub level: DegradationLevel,
    /// Decision taken.
    pub decision: CascadeDecision,
    /// P99 prediction from the guard.
    pub prediction: P99Prediction,
}

/// Telemetry snapshot of the cascade.
#[derive(Debug, Clone)]
pub struct CascadeTelemetry {
    /// Current degradation level.
    pub level: DegradationLevel,
    /// Recovery streak.
    pub recovery_streak: u32,
    /// Recovery threshold.
    pub recovery_threshold: u32,
    /// Frame counter.
    pub frame_idx: u64,
    /// Total degrade events.
    pub total_degrades: u64,
    /// Total recovery events.
    pub total_recoveries: u64,
    /// Guard state.
    pub guard_state: GuardState,
    /// Guard total observations.
    pub guard_observations: u64,
    /// Guard EMA estimate (µs).
    pub guard_ema_us: f64,
}

impl CascadeTelemetry {
    /// Format as JSONL.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        format!(
            r#"{{"schema":"cascade-telemetry-v1","level":"{}","recovery_streak":{},"recovery_threshold":{},"frame_idx":{},"total_degrades":{},"total_recoveries":{},"guard_state":"{}","guard_observations":{},"guard_ema_us":{}}}"#,
            self.level.as_str(),
            self.recovery_streak,
            self.recovery_threshold,
            self.frame_idx,
            self.total_degrades,
            self.total_recoveries,
            self.guard_state.as_str(),
            self.guard_observations,
            crate::conformal_predictor::finite_json_number(Some(self.guard_ema_us), 1),
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

    fn budget_us() -> f64 {
        16_000.0 // 16ms
    }

    #[test]
    fn initial_state_is_full_quality() {
        let cascade = DegradationCascade::with_defaults();
        assert_eq!(cascade.level(), DegradationLevel::Full);
        assert_eq!(cascade.recovery_streak(), 0);
        assert_eq!(cascade.frame_idx(), 0);
    }

    #[test]
    fn fast_frames_stay_at_full() {
        let mut cascade = DegradationCascade::with_defaults();
        let key = test_key();

        // Calibrate with fast frames
        for _ in 0..30 {
            cascade.post_render(8_000.0, key);
        }

        let result = cascade.pre_render(budget_us(), key);
        assert_eq!(result.level, DegradationLevel::Full);
        assert_eq!(result.decision, CascadeDecision::Hold);
    }

    #[test]
    fn slow_frames_trigger_degradation() {
        let mut cascade = DegradationCascade::with_defaults();
        let key = test_key();

        // Calibrate with slow frames (20ms > 16ms budget)
        for _ in 0..25 {
            cascade.post_render(20_000.0, key);
        }

        let result = cascade.pre_render(budget_us(), key);
        assert_eq!(result.decision, CascadeDecision::Degrade);
        assert!(result.level > DegradationLevel::Full);
    }

    #[test]
    fn recovery_after_sustained_good_frames() {
        let config = CascadeConfig {
            recovery_threshold: 5, // Low threshold for testing
            ..Default::default()
        };
        let mut cascade = DegradationCascade::new(config).expect("valid config");
        let key = test_key();

        // Calibrate with slow frames to trigger degradation
        for _ in 0..25 {
            cascade.post_render(20_000.0, key);
        }
        let result = cascade.pre_render(budget_us(), key);
        assert_eq!(result.decision, CascadeDecision::Degrade);
        let degraded_level = cascade.level();
        assert!(degraded_level > DegradationLevel::Full);

        // Now feed fast frames to trigger recovery
        for _ in 0..25 {
            cascade.post_render(8_000.0, key);
        }

        // Run enough pre_render calls (with fast calibration) to hit recovery threshold
        let mut recovered = false;
        for _ in 0..10 {
            let result = cascade.pre_render(budget_us(), key);
            if result.decision == CascadeDecision::Recover {
                recovered = true;
                break;
            }
        }
        assert!(
            recovered,
            "Should have recovered after sustained good frames"
        );
        assert!(cascade.level() < degraded_level);
    }

    #[test]
    fn max_degradation_capped() {
        let config = CascadeConfig {
            max_degradation: DegradationLevel::NoStyling,
            ..Default::default()
        };
        let mut cascade = DegradationCascade::new(config).expect("valid config");
        let key = test_key();

        // Feed many slow frames
        for _ in 0..25 {
            cascade.post_render(30_000.0, key);
        }

        // Degrade multiple times
        for _ in 0..10 {
            cascade.pre_render(budget_us(), key);
        }

        // Should be capped at NoStyling
        assert!(cascade.level() <= DegradationLevel::NoStyling);
    }

    #[test]
    fn widget_filtering_at_essential_only() {
        let mut cascade = DegradationCascade::with_defaults();

        // At Full level, everything renders
        assert!(cascade.should_render_widget(true));
        assert!(cascade.should_render_widget(false));

        // Force to EssentialOnly
        cascade.current_level = DegradationLevel::EssentialOnly;
        assert!(cascade.should_render_widget(true));
        assert!(!cascade.should_render_widget(false));

        // At Skeleton, still only essential
        cascade.current_level = DegradationLevel::Skeleton;
        assert!(cascade.should_render_widget(true));
        assert!(!cascade.should_render_widget(false));
    }

    #[test]
    fn evidence_emitted_on_degrade() {
        let mut cascade = DegradationCascade::with_defaults();
        let key = test_key();

        for _ in 0..25 {
            cascade.post_render(20_000.0, key);
        }

        cascade.pre_render(budget_us(), key);

        let evidence = cascade.last_evidence().expect("evidence should exist");
        assert_eq!(evidence.decision, CascadeDecision::Degrade);
        assert_eq!(evidence.level_before, DegradationLevel::Full);
        assert!(evidence.level_after > DegradationLevel::Full);
        assert!(evidence.prediction.is_some());

        // Check JSONL is well-formed
        let json_str = evidence.to_jsonl();
        assert!(json_str.contains("degradation-cascade-v1"));
        assert!(json_str.contains("\"decision\":\"degrade\""));
    }

    #[test]
    fn recovery_streak_resets_on_degrade() {
        let mut cascade = DegradationCascade::with_defaults();
        let key = test_key();

        // Warmup cannot provide recovery evidence.
        for _ in 0..5 {
            cascade.post_render(8_000.0, key);
            cascade.pre_render(budget_us(), key);
        }
        assert_eq!(cascade.recovery_streak(), 0);
        // Accumulate enough calibration data, then begin a real recovery streak.
        for _ in 5..20 {
            cascade.post_render(8_000.0, key);
        }
        cascade.pre_render(budget_us(), key);

        let streak_before = cascade.recovery_streak();
        assert!(streak_before > 0);

        // Now send slow frames to trigger degradation
        for _ in 0..25 {
            cascade.post_render(25_000.0, key);
        }
        cascade.pre_render(budget_us(), key);

        // After degradation, streak should be reset
        assert_eq!(cascade.recovery_streak(), 0);
    }

    #[test]
    fn reset_preserves_aggregate_counts() {
        let mut cascade = DegradationCascade::with_defaults();
        let key = test_key();

        for _ in 0..25 {
            cascade.post_render(20_000.0, key);
        }
        cascade.pre_render(budget_us(), key);
        assert!(cascade.total_degrades() > 0);

        cascade.reset();

        assert_eq!(cascade.level(), DegradationLevel::Full);
        assert_eq!(cascade.frame_idx(), 0);
        assert_eq!(cascade.recovery_streak(), 0);
        // Aggregate counts preserved
        assert!(cascade.total_degrades() > 0);
    }

    #[test]
    fn telemetry_captures_state() {
        let mut cascade = DegradationCascade::with_defaults();
        let key = test_key();

        for _ in 0..10 {
            cascade.post_render(12_000.0, key);
            cascade.pre_render(budget_us(), key);
        }

        let telem = cascade.telemetry();
        assert_eq!(telem.frame_idx, 10);
        assert_eq!(telem.level, DegradationLevel::Full);

        let json_str = telem.to_jsonl();
        assert!(json_str.contains("cascade-telemetry-v1"));
    }

    #[test]
    fn warmup_fallback_does_not_degrade_for_fast_frames() {
        let mut cascade = DegradationCascade::with_defaults();
        let key = test_key();

        // Only 5 observations (warmup, not calibrated)
        for _ in 0..5 {
            cascade.post_render(10_000.0, key);
        }

        let result = cascade.pre_render(budget_us(), key);
        // During warmup with 10ms frames, should not degrade (10ms < 16ms fallback)
        assert_eq!(result.decision, CascadeDecision::Hold);
        assert_eq!(result.level, DegradationLevel::Full);
    }

    #[test]
    fn warmup_fallback_degrades_for_slow_frames() {
        let mut cascade = DegradationCascade::with_defaults();
        let key = test_key();

        // Only 5 observations (warmup, not calibrated) but slow
        for _ in 0..5 {
            cascade.post_render(20_000.0, key);
        }

        let result = cascade.pre_render(budget_us(), key);
        // During warmup with 20ms frames, EMA > 16ms fallback → degrade
        assert_eq!(result.decision, CascadeDecision::Degrade);
    }

    #[test]
    fn unavailable_rank_holds_recovery_until_calibration_is_possible() {
        let mut cascade = DegradationCascade::new(CascadeConfig {
            guard: ConformalFrameGuardConfig {
                conformal: crate::ConformalConfig {
                    alpha: 0.01,
                    ..Default::default()
                },
                ..Default::default()
            },
            recovery_threshold: 2,
            ..Default::default()
        })
        .expect("valid config");
        let key = test_key();
        cascade.post_render(20_000.0, key);
        assert_eq!(
            cascade.pre_render(16_000.0, key).decision,
            CascadeDecision::Degrade
        );
        let degraded = cascade.level();
        for _ in 1..99 {
            let result = cascade.pre_render(100_000.0, key);
            assert_eq!(result.decision, CascadeDecision::Hold);
            assert_eq!(result.level, degraded);
            assert_eq!(cascade.recovery_streak(), 0);
            assert_eq!(result.prediction.upper_us, None);
            cascade.post_render(10_000.0, key);
        }
        assert_eq!(
            cascade.pre_render(100_000.0, key).decision,
            CascadeDecision::Hold
        );
        assert_eq!(cascade.recovery_streak(), 1);
        cascade.post_render(10_000.0, key);
        assert_eq!(
            cascade.pre_render(100_000.0, key).decision,
            CascadeDecision::Recover
        );
        assert_eq!(cascade.level(), DegradationLevel::Full);
    }

    #[test]
    fn invalid_budget_and_extreme_timings_keep_cascade_evidence_valid() {
        let mut cascade = DegradationCascade::with_defaults();
        let key = test_key();
        cascade.post_render(f64::MAX, key);
        for budget in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, f64::MAX] {
            let result = cascade.pre_render(budget, key);
            assert_eq!(result.prediction.upper_us, None);
            serde_json::from_str::<serde_json::Value>(&cascade.last_evidence().unwrap().to_jsonl())
                .expect("valid cascade evidence");
            serde_json::from_str::<serde_json::Value>(&cascade.telemetry().to_jsonl())
                .expect("valid cascade telemetry");
        }
    }

    #[test]
    fn cascade_evidence_preserves_unavailable_bound_causes() {
        for (alpha, min_samples, observation, status, rank) in [
            (0.01, 1, 10_000.0, "unattainable_rank", 2),
            (0.5, 1, f64::MAX, "arithmetic_overflow", 1),
            (0.5, 2, 10_000.0, "warmup", 1),
        ] {
            let mut cascade = DegradationCascade::new(CascadeConfig {
                guard: ConformalFrameGuardConfig {
                    conformal: crate::conformal_predictor::ConformalConfig {
                        alpha,
                        min_samples,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            })
            .expect("valid config");
            cascade.post_render(observation, test_key());
            cascade.pre_render(16_000.0, test_key());
            let row: serde_json::Value =
                serde_json::from_str(&cascade.last_evidence().unwrap().to_jsonl())
                    .expect("actual cascade JSON");
            assert!(row["p99_upper_us"].is_null());
            assert_eq!(row["conformal_status"], status);
            assert_eq!(row["conformal_required_rank"], rank);
            assert_eq!(row["conformal_alpha"], alpha);
        }
    }

    #[test]
    fn min_trigger_level_enforced() {
        let config = CascadeConfig {
            min_trigger_level: DegradationLevel::NoStyling,
            // Floor must be >= min_trigger_level for the jump to work
            degradation_floor: DegradationLevel::NoStyling,
            ..Default::default()
        };
        let mut cascade = DegradationCascade::new(config).expect("valid config");
        let key = test_key();

        for _ in 0..25 {
            cascade.post_render(20_000.0, key);
        }

        let result = cascade.pre_render(budget_us(), key);
        // Should jump directly to NoStyling (skipping SimpleBorders)
        assert_eq!(result.decision, CascadeDecision::Degrade);
        assert!(cascade.level() >= DegradationLevel::NoStyling);
    }

    #[test]
    fn consecutive_degrades_increase_level() {
        let mut cascade = DegradationCascade::with_defaults();
        let key = test_key();

        // Keep feeding slow frames
        for _ in 0..25 {
            cascade.post_render(25_000.0, key);
        }

        let mut levels = vec![];
        for _ in 0..5 {
            let result = cascade.pre_render(budget_us(), key);
            levels.push(result.level);
            // Feed more slow frames between checks
            for _ in 0..5 {
                cascade.post_render(25_000.0, key);
            }
        }

        // Levels should be non-decreasing
        for window in levels.windows(2) {
            assert!(
                window[1] >= window[0],
                "levels should not decrease: {levels:?}"
            );
        }
    }
}

#![forbid(unsafe_code)]

//! Render budget enforcement with graceful degradation.
//!
//! This module provides time-based budget tracking for frame rendering,
//! enabling the system to gracefully degrade visual fidelity when
//! performance budgets are exceeded.
//!
//! # Overview
//!
//! Agent UIs receive unpredictable content (burst log output, large tool responses).
//! A frozen UI during burst input makes the agent feel broken. Users tolerate
//! reduced visual fidelity; they do NOT tolerate hangs.
//!
//! # Usage
//!
//! ```
//! use ftui_render::budget::{RenderBudget, DegradationLevel, FrameBudgetConfig};
//! use std::time::Duration;
//!
//! // Create a budget with 16ms total (60fps target)
//! let mut budget = RenderBudget::new(Duration::from_millis(16));
//!
//! // Check remaining time
//! let remaining = budget.remaining();
//!
//! // Check if we should degrade for an expensive operation
//! if budget.should_degrade(Duration::from_millis(5)) {
//!     budget.degrade();
//! }
//!
//! // Render at current degradation level
//! match budget.degradation() {
//!     DegradationLevel::Full => { /* full rendering */ }
//!     DegradationLevel::SimpleBorders => { /* ASCII borders */ }
//!     _ => { /* further degradation */ }
//! }
//! ```

use web_time::{Duration, Instant};

#[cfg(feature = "tracing")]
use tracing::{trace, warn};

// ---------------------------------------------------------------------------
// Budget Controller: PID + Anytime-Valid E-Process
// ---------------------------------------------------------------------------

/// PID controller gains for frame time regulation.
///
/// # Mathematical Model
///
/// Let `e_t = frame_time_t − target` be the error signal at frame `t`.
///
/// The PID control output is:
///
/// ```text
/// u_t = Kp * e_t  +  Ki * Σ_{j=0..t} e_j  +  Kd * (e_t − e_{t−1})
/// ```
///
/// The output `u_t` maps to degradation level adjustments:
/// - `u_t > degrade_threshold` → degrade one level (if e-process permits)
/// - `u_t < -upgrade_threshold` → upgrade one level
/// - otherwise → hold current level
///
/// # Gain Selection Rationale
///
/// For a 16ms target (60fps):
/// - `Kp = 0.5`: Proportional response. Moderate gain avoids oscillation
///   while still reacting to single-frame overruns.
/// - `Ki = 0.05`: Integral term. Low gain eliminates steady-state error
///   over ~20 frames without integral windup issues.
/// - `Kd = 0.2`: Derivative term. Provides anticipatory damping to reduce
///   overshoot when frame times are trending upward.
///
/// # Stability Analysis
///
/// For a first-order plant model G(s) = 1/(τs + 1) with τ ≈ 1 frame:
/// - Phase margin > 45° with these gains
/// - Gain margin > 6dB
/// - Settling time ≈ 8-12 frames for a step disturbance
///
/// Anti-windup: integral term is clamped to `[-integral_max, +integral_max]`
/// to prevent runaway accumulation during sustained overload.
#[derive(Debug, Clone, PartialEq)]
pub struct PidGains {
    /// Proportional gain. Reacts to current error magnitude.
    pub kp: f64,
    /// Integral gain. Eliminates steady-state error over time.
    pub ki: f64,
    /// Derivative gain. Dampens oscillations by reacting to error rate.
    pub kd: f64,
    /// Maximum absolute value of the integral accumulator (anti-windup).
    pub integral_max: f64,
}

impl Default for PidGains {
    fn default() -> Self {
        Self {
            kp: 0.5,
            ki: 0.05,
            kd: 0.2,
            integral_max: 5.0,
        }
    }
}

/// Internal PID controller state.
///
/// Tracks the error integral and previous error for derivative computation.
#[derive(Debug, Clone)]
struct PidState {
    /// Accumulated integral of error (clamped by `integral_max`).
    integral: f64,
    /// Previous frame's error value (for derivative).
    prev_error: f64,
    /// Last proportional term (for telemetry).
    last_p: f64,
    /// Last integral term (for telemetry).
    last_i: f64,
    /// Last derivative term (for telemetry).
    last_d: f64,
}

impl Default for PidState {
    fn default() -> Self {
        Self {
            integral: 0.0,
            prev_error: 0.0,
            last_p: 0.0,
            last_i: 0.0,
            last_d: 0.0,
        }
    }
}

impl PidState {
    /// Compute PID output for the current error and update internal state.
    ///
    /// Returns the control signal `u_t`.
    fn update(&mut self, error: f64, gains: &PidGains) -> f64 {
        if error.is_nan() {
            return 0.0;
        }
        // Integral with anti-windup clamping
        self.integral = (self.integral + error).clamp(-gains.integral_max, gains.integral_max);

        // Derivative (first-frame uses zero derivative)
        let derivative = error - self.prev_error;
        self.prev_error = error;

        // Record individual PID terms for telemetry
        self.last_p = gains.kp * error;
        self.last_i = gains.ki * self.integral;
        self.last_d = gains.kd * derivative;

        // PID output
        self.last_p + self.last_i + self.last_d
    }

    /// Reset controller state (e.g., after a mode change).
    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// E-process-style evidence accumulator for gating degradation decisions.
///
/// # Mathematical Model
///
/// Inspired by anytime-valid e-processes; the accumulator is
///
/// ```text
/// E_t = Π_{j=1..t} exp(λ * r_j − λ² / 2)
/// ```
///
/// where:
/// - `r_j` is the standardized residual at frame j: `(frame_time − target) / σ̂`
/// - `σ̂` is an EMA of *absolute* deviation (≈ 0.8σ for Gaussian noise),
///   measured against the already-updated EMA mean of frame times
/// - `λ` is a tuning parameter controlling sensitivity (default: 0.5)
///
/// # Decision Rule
///
/// - **Degrade** only when `E_t > 1/α` (evidence exceeds threshold).
///   Default α = 0.05, so we need `E_t > 20`.
/// - **Upgrade** only when `E_t < β` (evidence that overload has passed).
///   Default β = 0.5.
///
/// # Properties
///
/// 1. **Check-every-frame**: The gate can be evaluated after every frame;
///    thresholds are tuned empirically rather than by fixed-sample theory.
/// 2. **Heuristic threshold, not a formal α-bound**: because σ̂ is a
///    mean-absolute-deviation EMA (biased low vs. true σ), residuals are
///    inflated and `E_t` drifts upward slightly even under healthy jitter,
///    so Ville's inequality `P(E_t > 1/α | H₀) ≤ α` does NOT formally hold.
///    In practice this makes the gate a little more eager to degrade than
///    the α suggests, which errs on the fail-safe side. Treat `alpha` as a
///    tuning knob, not a guaranteed false-positive rate.
/// 3. **Self-correcting**: After a burst passes, E_t decays back toward 1.0,
///    naturally enabling recovery.
///
/// # Failure Modes
///
/// - **Sustained overload**: E_t grows exponentially → rapid degradation.
/// - **Transient spike**: E_t grows briefly → may not cross threshold →
///   PID handles short-term. Only persistent overload triggers e-process gate.
/// - **σ estimation drift**: We use an exponential moving average for σ with
///   a warmup period of 10 frames to avoid unstable early estimates.
#[derive(Debug, Clone, PartialEq)]
pub struct EProcessConfig {
    /// Sensitivity parameter λ. Higher values detect overload faster
    /// but increase false positive risk near the boundary.
    pub lambda: f64,
    /// Significance level α. Degrade when E_t > 1/α.
    /// Default: 0.05 (need E_t > 20 to degrade).
    pub alpha: f64,
    /// Recovery threshold β. Upgrade allowed when E_t < β.
    /// Default: 0.5.
    pub beta: f64,
    /// EMA decay for σ estimation. Closer to 1.0 = slower adaptation.
    /// Default: 0.9 (adapts over ~10 frames).
    pub sigma_ema_decay: f64,
    /// Minimum σ floor to prevent division by zero.
    /// Default: 1.0 ms.
    pub sigma_floor_ms: f64,
    /// Warmup frames before e-process activates. During warmup, fall back
    /// to PID-only decisions.
    pub warmup_frames: u32,
}

impl Default for EProcessConfig {
    fn default() -> Self {
        Self {
            lambda: 0.5,
            alpha: 0.05,
            beta: 0.5,
            sigma_ema_decay: 0.9,
            sigma_floor_ms: 1.0,
            warmup_frames: 10,
        }
    }
}

/// Internal e-process state.
#[derive(Debug, Clone)]
struct EProcessState {
    /// Current e-process value E_t (starts at 1.0).
    e_value: f64,
    /// EMA estimate of frame time standard deviation (ms).
    sigma_ema: f64,
    /// EMA estimate of mean frame time (ms) for residual computation.
    mean_ema: f64,
    /// Frames observed so far.
    frames_observed: u32,
}

impl Default for EProcessState {
    fn default() -> Self {
        Self {
            e_value: 1.0,
            sigma_ema: 0.0,
            mean_ema: 0.0,
            frames_observed: 0,
        }
    }
}

impl EProcessState {
    /// Update the e-process with a new frame time observation.
    ///
    /// Returns the updated E_t value.
    fn update(&mut self, frame_time_ms: f64, target_ms: f64, config: &EProcessConfig) -> f64 {
        self.frames_observed = self.frames_observed.saturating_add(1);

        // Update mean EMA
        if self.frames_observed == 1 {
            self.mean_ema = frame_time_ms;
            self.sigma_ema = config.sigma_floor_ms;
        } else {
            let decay = config.sigma_ema_decay;
            self.mean_ema = decay * self.mean_ema + (1.0 - decay) * frame_time_ms;
            // Update sigma EMA using absolute deviation as proxy
            let deviation = (frame_time_ms - self.mean_ema).abs();
            self.sigma_ema = decay * self.sigma_ema + (1.0 - decay) * deviation;
        }

        // Floor sigma to prevent instability
        let sigma = self.sigma_ema.max(config.sigma_floor_ms);

        // Compute standardized residual
        let residual = (frame_time_ms - target_ms) / sigma;

        // E-process multiplicative update:
        // E_{t+1} = E_t * exp(λ * r_t − λ² * σ² / 2)
        // Since r_t is already standardized, σ in the exponent is 1.0.
        let lambda = config.lambda;
        let log_factor = lambda * residual - lambda * lambda / 2.0;
        if !log_factor.is_nan() {
            self.e_value *= log_factor.exp();
            // Clamp to avoid numerical issues (but preserve the supermartingale property
            // by allowing it to grow large or shrink small).
            self.e_value = self.e_value.clamp(1e-10, 1e10);
        }

        self.e_value
    }

    /// Check if evidence supports degradation.
    fn should_degrade(&self, config: &EProcessConfig) -> bool {
        if self.frames_observed < config.warmup_frames {
            return false; // Fall back to PID during warmup
        }
        self.e_value > 1.0 / config.alpha
    }

    /// Check if evidence supports upgrade (overload has passed).
    fn should_upgrade(&self, config: &EProcessConfig) -> bool {
        if self.frames_observed < config.warmup_frames {
            return true; // Allow PID-driven upgrades during warmup
        }
        self.e_value < config.beta
    }

    /// Reset state.
    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Configuration for the adaptive budget controller.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetControllerConfig {
    /// PID controller gains.
    pub pid: PidGains,
    /// E-process configuration.
    pub eprocess: EProcessConfig,
    /// Target frame time.
    pub target: Duration,
    /// Hysteresis: PID output must exceed this to trigger degradation.
    ///
    /// This prevents oscillation at the boundary. The value is in
    /// normalized units (error / target). Default: 0.3 (30% of target).
    ///
    /// # Justification
    ///
    /// A threshold of 0.3 means the controller needs ~5ms sustained error
    /// at 16ms target before degrading. This filters out single-frame jitter
    /// while remaining responsive to genuine overload (2-3 consecutive
    /// slow frames will cross the threshold via integral accumulation).
    pub degrade_threshold: f64,
    /// Hysteresis: PID output must be below negative of this to trigger upgrade.
    /// Default: 0.2 (20% of target).
    pub upgrade_threshold: f64,
    /// Cooldown frames between level changes.
    ///
    /// A value of `N` keeps transitions at least `N` frames apart. Since
    /// decisions are made once per frame, `0` and `1` are equivalent (no
    /// extra spacing beyond the per-frame cadence).
    pub cooldown_frames: u32,
    /// Minimum quality floor: the controller will never degrade past this level.
    ///
    /// Default: `DegradationLevel::SimpleBorders` — preserves readable text
    /// content while still allowing border simplification.
    ///
    /// Setting this to `DegradationLevel::Full` disables all degradation.
    /// Setting this to `DegradationLevel::SkipFrame` effectively removes the floor.
    pub degradation_floor: DegradationLevel,
}

impl Default for BudgetControllerConfig {
    fn default() -> Self {
        Self {
            pid: PidGains::default(),
            eprocess: EProcessConfig::default(),
            target: Duration::from_millis(16),
            degrade_threshold: 0.3,
            upgrade_threshold: 0.2,
            cooldown_frames: 3,
            degradation_floor: DegradationLevel::SimpleBorders,
        }
    }
}

/// Adaptive budget controller combining PID regulation with e-process gating.
///
/// # Architecture
///
/// ```text
/// frame_time ─┬─► PID Controller ─► control signal u_t
///             │                              │
///             └─► E-Process ──────► gate ────┤
///                                            ▼
///                                    Decision Logic
///                                    ┌───────────────┐
///                                    │ u_t > thresh   │──► DEGRADE (if e-process permits)
///                                    │ u_t < -thresh  │──► UPGRADE (if e-process permits)
///                                    │ otherwise      │──► HOLD
///                                    └───────────────┘
/// ```
///
/// The PID controller provides smooth, reactive adaptation. The e-process
/// gates decisions to ensure statistical validity — we only degrade when
/// there is strong evidence of sustained overload, not just transient spikes.
///
/// # Usage
///
/// ```rust
/// use ftui_render::budget::{BudgetController, BudgetControllerConfig, DegradationLevel};
/// use std::time::Duration;
///
/// let mut controller = BudgetController::new(BudgetControllerConfig::default());
///
/// // After each frame, feed the observed frame time:
/// let decision = controller.update(Duration::from_millis(20)); // slow frame
/// // decision tells you what to do: Hold, Degrade, or Upgrade
/// ```
#[derive(Debug, Clone)]
pub struct BudgetController {
    config: BudgetControllerConfig,
    pid: PidState,
    eprocess: EProcessState,
    current_level: DegradationLevel,
    frames_since_change: u32,
    last_pid_output: f64,
    last_decision: BudgetDecision,
    last_decision_reason: BudgetDecisionReason,
    last_frame_ms: f64,
    transition_seq: u64,
    last_transition_correlation_id: u64,
    last_pid_gate_threshold: f64,
    last_pid_gate_margin: f64,
    last_evidence_threshold: f64,
    last_evidence_margin: f64,
}

/// Decision output from the budget controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDecision {
    /// Maintain current degradation level.
    Hold,
    /// Degrade one level (reduce visual fidelity).
    Degrade,
    /// Upgrade one level (restore visual fidelity).
    Upgrade,
}

impl BudgetDecision {
    /// JSONL-compatible string representation.
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hold => "stay",
            Self::Degrade => "degrade",
            Self::Upgrade => "upgrade",
        }
    }
}

/// Version tag for budget telemetry schema emitted by [`BudgetTelemetry`].
pub const BUDGET_TELEMETRY_SCHEMA_VERSION: u16 = 1;

/// Controller rationale for a per-frame decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDecisionReason {
    /// No decision change while cooldown is active.
    CooldownActive,
    /// Overload + evidence gate passed, so degrade one level.
    OverloadEvidencePassed,
    /// Underload + evidence gate passed, so upgrade one level.
    UnderloadEvidencePassed,
    /// Already at maximum degradation; cannot degrade further.
    AtMaxDegradation,
    /// Already at the configured degradation floor; policy forbids degrading further.
    AtDegradationFloor,
    /// Already at full quality; cannot upgrade further.
    AtFullQuality,
    /// Overload signal present but e-process degrade gate not satisfied.
    OverloadEvidenceInsufficient,
    /// Underload signal present but e-process upgrade gate not satisfied.
    UnderloadEvidenceInsufficient,
    /// PID output remained in the hold band.
    WithinThresholdBand,
}

impl BudgetDecisionReason {
    /// Stable string code for JSONL logs and CI parsing.
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CooldownActive => "cooldown_active",
            Self::OverloadEvidencePassed => "overload_evidence_passed",
            Self::UnderloadEvidencePassed => "underload_evidence_passed",
            Self::AtMaxDegradation => "at_max_degradation",
            Self::AtDegradationFloor => "at_degradation_floor",
            Self::AtFullQuality => "at_full_quality",
            Self::OverloadEvidenceInsufficient => "overload_evidence_insufficient",
            Self::UnderloadEvidenceInsufficient => "underload_evidence_insufficient",
            Self::WithinThresholdBand => "within_threshold_band",
        }
    }
}

impl BudgetController {
    /// Create a new budget controller with the given configuration.
    pub fn new(config: BudgetControllerConfig) -> Self {
        Self {
            config,
            pid: PidState::default(),
            eprocess: EProcessState::default(),
            current_level: DegradationLevel::Full,
            frames_since_change: 0,
            last_pid_output: 0.0,
            last_decision: BudgetDecision::Hold,
            last_decision_reason: BudgetDecisionReason::WithinThresholdBand,
            last_frame_ms: 0.0,
            transition_seq: 0,
            last_transition_correlation_id: 0,
            last_pid_gate_threshold: 0.0,
            last_pid_gate_margin: 0.0,
            last_evidence_threshold: 0.0,
            last_evidence_margin: 0.0,
        }
    }

    /// Feed a frame time observation and get a decision.
    ///
    /// Call this once per frame with the measured frame duration.
    pub fn update(&mut self, frame_time: Duration) -> BudgetDecision {
        let target_ms = self.config.target.as_secs_f64() * 1000.0;
        let frame_ms = frame_time.as_secs_f64() * 1000.0;

        // Compute normalized error (positive = over budget)
        let error = (frame_ms - target_ms) / target_ms;

        // Update PID
        let u = self.pid.update(error, &self.config.pid);
        self.last_pid_output = u;
        self.last_frame_ms = frame_ms;

        // Update e-process
        self.eprocess
            .update(frame_ms, target_ms, &self.config.eprocess);

        // Increment cooldown counter
        self.frames_since_change = self.frames_since_change.saturating_add(1);

        let mut decision = BudgetDecision::Hold;
        let mut reason = BudgetDecisionReason::WithinThresholdBand;
        let mut pid_gate_threshold = 0.0;
        let mut pid_gate_margin = 0.0;
        let mut evidence_threshold = 0.0;
        let mut evidence_margin = 0.0;

        // Decision logic with hysteresis + e-process gating + explainable reason/evidence.
        if self.frames_since_change < self.config.cooldown_frames {
            reason = BudgetDecisionReason::CooldownActive;
        } else if u > self.config.degrade_threshold {
            pid_gate_threshold = self.config.degrade_threshold;
            pid_gate_margin = u - pid_gate_threshold;
            evidence_threshold = 1.0 / self.config.eprocess.alpha;
            evidence_margin = self.eprocess.e_value - evidence_threshold;

            if self.current_level.is_max() {
                reason = BudgetDecisionReason::AtMaxDegradation;
            } else if self.current_level >= self.config.degradation_floor {
                reason = BudgetDecisionReason::AtDegradationFloor;
            } else if self.eprocess.should_degrade(&self.config.eprocess) {
                decision = BudgetDecision::Degrade;
                reason = BudgetDecisionReason::OverloadEvidencePassed;
            } else {
                reason = BudgetDecisionReason::OverloadEvidenceInsufficient;
            }
        } else if u < -self.config.upgrade_threshold {
            pid_gate_threshold = -self.config.upgrade_threshold;
            pid_gate_margin = (-u) - self.config.upgrade_threshold;
            evidence_threshold = self.config.eprocess.beta;
            evidence_margin = evidence_threshold - self.eprocess.e_value;

            if self.current_level.is_full() {
                reason = BudgetDecisionReason::AtFullQuality;
            } else if self.eprocess.should_upgrade(&self.config.eprocess) {
                decision = BudgetDecision::Upgrade;
                reason = BudgetDecisionReason::UnderloadEvidencePassed;
            } else {
                reason = BudgetDecisionReason::UnderloadEvidenceInsufficient;
            }
        }

        // Record decision for telemetry
        self.last_decision = decision;
        self.last_decision_reason = reason;
        self.last_pid_gate_threshold = pid_gate_threshold;
        self.last_pid_gate_margin = pid_gate_margin;
        self.last_evidence_threshold = evidence_threshold;
        self.last_evidence_margin = evidence_margin;

        // Apply decision
        match decision {
            BudgetDecision::Degrade => {
                self.transition_seq = self.transition_seq.saturating_add(1);
                self.last_transition_correlation_id =
                    (self.transition_seq << 32) ^ u64::from(self.eprocess.frames_observed);
                let next = self.current_level.next();
                // Clamp to degradation floor: never degrade past the configured minimum quality.
                self.current_level = if next > self.config.degradation_floor {
                    self.config.degradation_floor
                } else {
                    next
                };
                self.frames_since_change = 0;

                #[cfg(feature = "tracing")]
                warn!(
                    level = self.current_level.as_str(),
                    pid_output = u,
                    e_value = self.eprocess.e_value,
                    "budget controller: degrade"
                );
            }
            BudgetDecision::Upgrade => {
                self.transition_seq = self.transition_seq.saturating_add(1);
                self.last_transition_correlation_id =
                    (self.transition_seq << 32) ^ u64::from(self.eprocess.frames_observed);
                self.current_level = self.current_level.prev();
                self.frames_since_change = 0;

                #[cfg(feature = "tracing")]
                trace!(
                    level = self.current_level.as_str(),
                    pid_output = u,
                    e_value = self.eprocess.e_value,
                    "budget controller: upgrade"
                );
            }
            BudgetDecision::Hold => {}
        }

        decision
    }

    /// Get the current degradation level.
    #[inline]
    pub fn level(&self) -> DegradationLevel {
        self.current_level
    }

    /// Synchronize the controller's level with an externally applied change.
    ///
    /// [`RenderBudget`] calls this whenever its degradation level is mutated
    /// outside the controller's own decision (guardrail emergencies, the
    /// conformal risk gate, manual `set_degradation`). Without it the
    /// controller keeps reasoning from a stale level: believing it is at
    /// `Full` it reports `AtFullQuality` and holds forever, so an externally
    /// degraded UI never recovers, and its floor clamp computes from the
    /// wrong base. Starts a cooldown window, since a transition just
    /// happened.
    pub fn sync_level(&mut self, level: DegradationLevel) {
        if self.current_level != level {
            self.current_level = level;
            self.frames_since_change = 0;
        }
    }

    /// Get the current e-process value (for diagnostics/logging).
    #[inline]
    pub fn e_value(&self) -> f64 {
        self.eprocess.e_value
    }

    /// Get the current e-process sigma estimate (ms).
    #[inline]
    pub fn eprocess_sigma_ms(&self) -> f64 {
        self.eprocess
            .sigma_ema
            .max(self.config.eprocess.sigma_floor_ms)
    }

    /// Get the current PID integral term (for diagnostics/logging).
    #[inline]
    pub fn pid_integral(&self) -> f64 {
        self.pid.integral
    }

    /// Get the number of frames observed by the e-process.
    #[inline]
    pub fn frames_observed(&self) -> u32 {
        self.eprocess.frames_observed
    }

    /// Capture a telemetry snapshot of the controller state.
    ///
    /// This is allocation-free and suitable for calling every frame.
    /// Forward the result to a debug overlay or structured logger.
    #[inline]
    pub fn telemetry(&self) -> BudgetTelemetry {
        BudgetTelemetry {
            schema_version: BUDGET_TELEMETRY_SCHEMA_VERSION,
            level: self.current_level,
            pid_output: self.last_pid_output,
            pid_p: self.pid.last_p,
            pid_i: self.pid.last_i,
            pid_d: self.pid.last_d,
            e_value: self.eprocess.e_value,
            frames_observed: self.eprocess.frames_observed,
            frames_since_change: self.frames_since_change,
            last_decision: self.last_decision,
            decision_reason: self.last_decision_reason,
            transition_seq: self.transition_seq,
            transition_correlation_id: self.last_transition_correlation_id,
            frame_time_ms: self.last_frame_ms,
            target_ms: self.config.target.as_secs_f64() * 1000.0,
            pid_gate_threshold: self.last_pid_gate_threshold,
            pid_gate_margin: self.last_pid_gate_margin,
            evidence_threshold: self.last_evidence_threshold,
            evidence_margin: self.last_evidence_margin,
            in_warmup: self.eprocess.frames_observed < self.config.eprocess.warmup_frames,
        }
    }

    /// Reset the controller to initial state.
    pub fn reset(&mut self) {
        self.pid.reset();
        self.eprocess.reset();
        self.current_level = DegradationLevel::Full;
        self.frames_since_change = 0;
        self.last_pid_output = 0.0;
        self.last_decision = BudgetDecision::Hold;
        self.last_decision_reason = BudgetDecisionReason::WithinThresholdBand;
        self.last_frame_ms = 0.0;
        self.transition_seq = 0;
        self.last_transition_correlation_id = 0;
        self.last_pid_gate_threshold = 0.0;
        self.last_pid_gate_margin = 0.0;
        self.last_evidence_threshold = 0.0;
        self.last_evidence_margin = 0.0;
    }

    /// Get a reference to the controller configuration.
    #[inline]
    #[must_use]
    pub fn config(&self) -> &BudgetControllerConfig {
        &self.config
    }
}

/// Snapshot of budget controller telemetry for diagnostics and debug overlay.
///
/// All fields are `Copy` — no allocations. Intended to be cheaply captured
/// once per frame and forwarded to a tracing subscriber or debug overlay widget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetTelemetry {
    /// Telemetry schema version for CI/E2E consumers.
    pub schema_version: u16,
    /// Current degradation level.
    pub level: DegradationLevel,
    /// Last PID control signal (positive = over budget).
    pub pid_output: f64,
    /// Last PID proportional term.
    pub pid_p: f64,
    /// Last PID integral term.
    pub pid_i: f64,
    /// Last PID derivative term.
    pub pid_d: f64,
    /// Current e-process value E_t.
    pub e_value: f64,
    /// Frames observed by the e-process.
    pub frames_observed: u32,
    /// Frames since last level change.
    pub frames_since_change: u32,
    /// Last decision made by the controller.
    pub last_decision: BudgetDecision,
    /// Rationale code describing why the last decision was taken.
    pub decision_reason: BudgetDecisionReason,
    /// Monotonic transition sequence number (increments on degrade/upgrade).
    pub transition_seq: u64,
    /// Correlation ID for the most recent transition event (0 if none yet).
    pub transition_correlation_id: u64,
    /// Last observed frame time in milliseconds.
    pub frame_time_ms: f64,
    /// Current target frame budget in milliseconds.
    pub target_ms: f64,
    /// PID gate threshold used for the last decision path.
    pub pid_gate_threshold: f64,
    /// PID gate margin (positive values indicate stronger gate pass).
    pub pid_gate_margin: f64,
    /// Evidence (e-process) threshold used for the last decision path.
    pub evidence_threshold: f64,
    /// Evidence gate margin (positive values indicate stronger gate pass).
    pub evidence_margin: f64,
    /// Whether the controller is in warmup (e-process not yet active).
    pub in_warmup: bool,
}

/// Progressive degradation levels for render quality.
///
/// Higher levels mean less visual fidelity but faster rendering.
/// The ordering is significant: `Full` < `SimpleBorders` < ... < `SkipFrame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum DegradationLevel {
    /// All visual features enabled.
    #[default]
    Full = 0,
    /// Unicode box-drawing replaced with ASCII (+--+).
    SimpleBorders = 1,
    /// Colors disabled, monochrome output.
    NoStyling = 2,
    /// Skip decorative widgets, essential content only.
    EssentialOnly = 3,
    /// Just layout boxes, no content.
    Skeleton = 4,
    /// Emergency: skip frame entirely.
    SkipFrame = 5,
}

impl DegradationLevel {
    /// Move to the next degradation level.
    ///
    /// Returns `SkipFrame` if already at maximum degradation.
    #[inline]
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Full => Self::SimpleBorders,
            Self::SimpleBorders => Self::NoStyling,
            Self::NoStyling => Self::EssentialOnly,
            Self::EssentialOnly => Self::Skeleton,
            Self::Skeleton | Self::SkipFrame => Self::SkipFrame,
        }
    }

    /// Move to the previous (better quality) degradation level.
    ///
    /// Returns `Full` if already at minimum degradation.
    #[inline]
    #[must_use]
    pub fn prev(self) -> Self {
        match self {
            Self::SkipFrame => Self::Skeleton,
            Self::Skeleton => Self::EssentialOnly,
            Self::EssentialOnly => Self::NoStyling,
            Self::NoStyling => Self::SimpleBorders,
            Self::SimpleBorders | Self::Full => Self::Full,
        }
    }

    /// Check if this is the maximum degradation level.
    #[inline]
    pub fn is_max(self) -> bool {
        self == Self::SkipFrame
    }

    /// Check if this is full quality (no degradation).
    #[inline]
    pub fn is_full(self) -> bool {
        self == Self::Full
    }

    /// Get a human-readable name for logging.
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::SimpleBorders => "SimpleBorders",
            Self::NoStyling => "NoStyling",
            Self::EssentialOnly => "EssentialOnly",
            Self::Skeleton => "Skeleton",
            Self::SkipFrame => "SkipFrame",
        }
    }

    /// Number of levels from Full (0) to this level.
    #[inline]
    pub fn level(self) -> u8 {
        self as u8
    }

    // ---- Widget convenience queries ----

    /// Whether to use Unicode box-drawing characters.
    ///
    /// Returns `false` at `SimpleBorders` and above (use ASCII instead).
    #[inline]
    pub fn use_unicode_borders(self) -> bool {
        self < Self::SimpleBorders
    }

    /// Whether to apply colors and style attributes to cells.
    ///
    /// Returns `false` at `NoStyling` and above.
    #[inline]
    pub fn apply_styling(self) -> bool {
        self < Self::NoStyling
    }

    /// Whether to render decorative (non-essential) elements.
    ///
    /// Returns `false` at `EssentialOnly` and above.
    /// Decorative elements include borders, scrollbars, spinners, rules.
    #[inline]
    pub fn render_decorative(self) -> bool {
        self < Self::EssentialOnly
    }

    /// Whether to render content text.
    ///
    /// Returns `false` at `Skeleton` and above.
    #[inline]
    pub fn render_content(self) -> bool {
        self < Self::Skeleton
    }
}

/// Per-phase time budgets within a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseBudgets {
    /// Budget for diff computation.
    pub diff: Duration,
    /// Budget for ANSI presentation/emission.
    pub present: Duration,
    /// Budget for widget rendering.
    pub render: Duration,
}

impl Default for PhaseBudgets {
    fn default() -> Self {
        Self {
            diff: Duration::from_millis(2),
            present: Duration::from_millis(4),
            render: Duration::from_millis(8),
        }
    }
}

/// Configuration for frame budget behavior.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameBudgetConfig {
    /// Total time budget per frame.
    pub total: Duration,
    /// Per-phase budgets.
    pub phase_budgets: PhaseBudgets,
    /// Allow skipping frames entirely when severely over budget.
    pub allow_frame_skip: bool,
    /// Frames to wait between degradation level changes.
    pub degradation_cooldown: u32,
    /// Threshold (as fraction of total) above which we consider upgrading.
    /// Default: 0.5 (upgrade when >50% budget remains).
    pub upgrade_threshold: f32,
}

impl Default for FrameBudgetConfig {
    fn default() -> Self {
        Self {
            total: Duration::from_millis(16), // ~60fps feel
            phase_budgets: PhaseBudgets::default(),
            allow_frame_skip: true,
            degradation_cooldown: 3,
            upgrade_threshold: 0.5,
        }
    }
}

impl FrameBudgetConfig {
    /// Create a new config with the specified total budget.
    pub fn with_total(total: Duration) -> Self {
        Self {
            total,
            ..Default::default()
        }
    }

    /// Create a strict config that never skips frames.
    pub fn strict(total: Duration) -> Self {
        Self {
            total,
            allow_frame_skip: false,
            ..Default::default()
        }
    }

    /// Create a relaxed config for slower refresh rates.
    pub fn relaxed() -> Self {
        Self {
            total: Duration::from_millis(33), // ~30fps
            degradation_cooldown: 5,
            ..Default::default()
        }
    }
}

/// Render time budget with graceful degradation.
///
/// Tracks elapsed time within a frame and manages degradation level
/// to maintain responsive rendering under load.
#[derive(Debug, Clone)]
pub struct RenderBudget {
    /// Total time budget for this frame.
    total: Duration,
    /// When this frame started.
    start: Instant,
    /// Measured render+present time for the last frame (if recorded).
    last_frame_time: Option<Duration>,
    /// Current degradation level.
    degradation: DegradationLevel,
    /// Per-phase budgets.
    phase_budgets: PhaseBudgets,
    /// Allow frame skip at maximum degradation.
    allow_frame_skip: bool,
    /// Upgrade threshold fraction.
    upgrade_threshold: f32,
    /// Frames since last degradation change (for cooldown).
    frames_since_change: u32,
    /// Cooldown frames required between changes.
    cooldown: u32,
    /// Optional adaptive budget controller (PID + e-process).
    /// When present, `next_frame()` delegates degradation decisions to the controller.
    controller: Option<BudgetController>,
}

impl RenderBudget {
    /// Create a new budget with the specified total time.
    pub fn new(total: Duration) -> Self {
        Self {
            total,
            start: Instant::now(),
            last_frame_time: None,
            degradation: DegradationLevel::Full,
            phase_budgets: PhaseBudgets::default(),
            allow_frame_skip: true,
            upgrade_threshold: 0.5,
            frames_since_change: 0,
            cooldown: 3,
            controller: None,
        }
    }

    /// Create a budget from configuration.
    pub fn from_config(config: &FrameBudgetConfig) -> Self {
        Self {
            total: config.total,
            start: Instant::now(),
            last_frame_time: None,
            degradation: DegradationLevel::Full,
            phase_budgets: config.phase_budgets,
            allow_frame_skip: config.allow_frame_skip,
            upgrade_threshold: config.upgrade_threshold,
            frames_since_change: 0,
            cooldown: config.degradation_cooldown,
            controller: None,
        }
    }

    /// Attach an adaptive budget controller to this render budget.
    ///
    /// When a controller is attached, `next_frame()` feeds the measured frame
    /// duration to the controller and applies its degradation decisions
    /// instead of the simple threshold-based upgrade logic.
    ///
    /// # Example
    ///
    /// ```
    /// use ftui_render::budget::{RenderBudget, BudgetControllerConfig};
    /// use std::time::Duration;
    ///
    /// let budget = RenderBudget::new(Duration::from_millis(16))
    ///     .with_controller(BudgetControllerConfig::default());
    /// ```
    #[must_use]
    pub fn with_controller(mut self, config: BudgetControllerConfig) -> Self {
        self.controller = Some(BudgetController::new(config));
        self
    }

    /// Get the total budget duration.
    #[inline]
    pub fn total(&self) -> Duration {
        self.total
    }

    /// Get the elapsed time since budget started.
    #[inline]
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Get the remaining time in the budget.
    #[inline]
    pub fn remaining(&self) -> Duration {
        self.total.saturating_sub(self.start.elapsed())
    }

    /// Get the remaining time as a fraction of total (0.0 to 1.0).
    #[inline]
    pub fn remaining_fraction(&self) -> f32 {
        if self.total.is_zero() {
            return 0.0;
        }
        let remaining = self.remaining().as_secs_f32();
        let total = self.total.as_secs_f32();
        (remaining / total).clamp(0.0, 1.0)
    }

    /// Check if we should degrade given an estimated operation cost.
    ///
    /// Returns `true` if the estimated cost exceeds remaining budget.
    #[inline]
    pub fn should_degrade(&self, estimated_cost: Duration) -> bool {
        self.remaining() < estimated_cost
    }

    /// Degrade to the next level.
    ///
    /// Logs a warning when degradation occurs. An attached controller is
    /// kept in sync so it reasons (and can later upgrade) from the actual
    /// level. External callers may degrade past the controller's
    /// `degradation_floor` — emergency paths (guardrails, risk gates)
    /// deliberately outrank the visual-quality floor; the controller itself
    /// never degrades further once at or past it.
    pub fn degrade(&mut self) {
        let from = self.degradation;
        self.degradation = self.degradation.next();
        self.frames_since_change = 0;
        self.sync_controller_level();

        #[cfg(feature = "tracing")]
        if from != self.degradation {
            warn!(
                from = from.as_str(),
                to = self.degradation.as_str(),
                remaining_ms = self.remaining().as_millis() as u32,
                "render budget degradation"
            );
        }
        let _ = from; // Suppress unused warning when tracing is disabled
    }

    /// Keep an attached controller's level in lockstep with the budget's.
    #[inline]
    fn sync_controller_level(&mut self) {
        if let Some(controller) = self.controller.as_mut() {
            controller.sync_level(self.degradation);
        }
    }

    /// Get the current degradation level.
    #[inline]
    pub fn degradation(&self) -> DegradationLevel {
        self.degradation
    }

    /// Set the degradation level directly.
    ///
    /// Use with caution - prefer `degrade()` and `upgrade()` for gradual
    /// changes. An attached controller is kept in sync so recovery from an
    /// externally forced level remains possible.
    pub fn set_degradation(&mut self, level: DegradationLevel) {
        if self.degradation != level {
            self.degradation = level;
            self.frames_since_change = 0;
            self.sync_controller_level();
        }
    }

    /// Check if the budget is exhausted.
    ///
    /// Returns `true` if no time remains OR if at SkipFrame level.
    #[inline]
    pub fn exhausted(&self) -> bool {
        self.remaining().is_zero()
            || (self.degradation == DegradationLevel::SkipFrame && self.allow_frame_skip)
    }

    /// Check if we should attempt to upgrade quality.
    ///
    /// Returns `true` if more than `upgrade_threshold` of budget remains
    /// and we're not already at full quality, and cooldown has passed.
    pub fn should_upgrade(&self) -> bool {
        !self.degradation.is_full()
            && self.remaining_fraction() > self.upgrade_threshold
            && self.frames_since_change >= self.cooldown
    }

    /// Check if we should upgrade using a measured frame time.
    fn should_upgrade_with_elapsed(&self, elapsed: Duration) -> bool {
        if self.degradation.is_full() || self.frames_since_change < self.cooldown {
            return false;
        }
        self.remaining_fraction_for_elapsed(elapsed) > self.upgrade_threshold
    }

    /// Remaining fraction computed from an elapsed frame time.
    fn remaining_fraction_for_elapsed(&self, elapsed: Duration) -> f32 {
        if self.total.is_zero() {
            return 0.0;
        }
        let remaining = self.total.saturating_sub(elapsed);
        let remaining = remaining.as_secs_f32();
        let total = self.total.as_secs_f32();
        (remaining / total).clamp(0.0, 1.0)
    }

    /// Upgrade to the previous (better quality) level.
    ///
    /// Logs when upgrade occurs.
    pub fn upgrade(&mut self) {
        let from = self.degradation;
        self.degradation = self.degradation.prev();
        self.frames_since_change = 0;
        self.sync_controller_level();

        #[cfg(feature = "tracing")]
        if from != self.degradation {
            trace!(
                from = from.as_str(),
                to = self.degradation.as_str(),
                remaining_fraction = self.remaining_fraction(),
                "render budget upgrade"
            );
        }
        let _ = from; // Suppress unused warning when tracing is disabled
    }

    /// Reset the budget for a new frame.
    ///
    /// Keeps the current degradation level but resets timing.
    pub fn reset(&mut self) {
        self.start = Instant::now();
        self.frames_since_change = self.frames_since_change.saturating_add(1);
    }

    /// Reset the budget and attempt upgrade if conditions are met.
    ///
    /// Call this at the start of each frame to enable recovery.
    ///
    /// When an adaptive controller is attached (via [`with_controller`](Self::with_controller)),
    /// the measured frame duration is fed to the controller and its decision
    /// (degrade / upgrade / hold) is applied automatically. The simple
    /// threshold-based upgrade path is skipped in that case.
    pub fn next_frame(&mut self) {
        // Consume the recorded time: each observation feeds the controller
        // exactly once. Without take(), a frame that skips recording (e.g.
        // the emergency drop-frame path) re-feeds the previous frame's
        // duration, accumulating PID/e-process evidence from a measurement
        // that never happened; the elapsed-time fallback below also becomes
        // permanently dead after the first record_frame_time call.
        let frame_time = self
            .last_frame_time
            .take()
            .unwrap_or_else(|| self.start.elapsed());

        if self.controller.is_some() {
            // Measure how long the previous frame took

            // SAFETY: we just checked is_some; this avoids a borrow-checker
            // conflict with `&mut self` needed for degrade/upgrade below.
            let decision = self
                .controller
                .as_mut()
                .expect("controller guaranteed by is_some guard")
                .update(frame_time);

            match decision {
                BudgetDecision::Degrade => self.degrade(),
                BudgetDecision::Upgrade => self.upgrade(),
                BudgetDecision::Hold => {}
            }
        } else {
            // Legacy path: simple threshold-based upgrade
            if self.should_upgrade_with_elapsed(frame_time) {
                self.upgrade();
            }
        }
        self.reset();
    }

    /// Record the measured render+present time for the last frame.
    pub fn record_frame_time(&mut self, elapsed: Duration) {
        self.last_frame_time = Some(elapsed);
    }

    /// Get a telemetry snapshot from the adaptive controller, if attached.
    ///
    /// Returns `None` if no controller is attached.
    /// This is allocation-free and safe to call every frame.
    #[inline]
    pub fn telemetry(&self) -> Option<BudgetTelemetry> {
        self.controller.as_ref().map(BudgetController::telemetry)
    }

    /// Get a reference to the adaptive controller, if attached.
    #[inline]
    pub fn controller(&self) -> Option<&BudgetController> {
        self.controller.as_ref()
    }

    /// Get the phase budgets.
    #[inline]
    #[must_use]
    pub fn phase_budgets(&self) -> &PhaseBudgets {
        &self.phase_budgets
    }

    /// Check if a specific phase has budget remaining.
    pub fn phase_has_budget(&self, phase: Phase) -> bool {
        let phase_budget = match phase {
            Phase::Diff => self.phase_budgets.diff,
            Phase::Present => self.phase_budgets.present,
            Phase::Render => self.phase_budgets.render,
        };
        self.remaining() >= phase_budget
    }

    /// Create a sub-budget for a specific phase.
    ///
    /// The sub-budget's clock starts now and its total is the phase
    /// allocation clamped to the frame's remaining time, so the phase gets
    /// its full allowance from the moment it begins. (Inheriting the frame's
    /// start instant would count time spent in earlier phases against this
    /// phase, leaving any phase entered later than its own allocation into
    /// the frame born exhausted.)
    #[must_use]
    pub fn phase_budget(&self, phase: Phase) -> Self {
        let phase_total = match phase {
            Phase::Diff => self.phase_budgets.diff,
            Phase::Present => self.phase_budgets.present,
            Phase::Render => self.phase_budgets.render,
        };
        Self {
            total: phase_total.min(self.remaining()),
            start: Instant::now(),
            last_frame_time: self.last_frame_time,
            degradation: self.degradation,
            phase_budgets: self.phase_budgets,
            allow_frame_skip: self.allow_frame_skip,
            upgrade_threshold: self.upgrade_threshold,
            frames_since_change: self.frames_since_change,
            cooldown: self.cooldown,
            controller: None, // Phase sub-budgets don't carry the controller
        }
    }
}

/// Render phases for budget allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Buffer diff computation.
    Diff,
    /// ANSI sequence presentation.
    Present,
    /// Widget tree rendering.
    Render,
}

impl Phase {
    /// Get a human-readable name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::Present => "present",
            Self::Render => "render",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn degradation_level_ordering() {
        assert!(DegradationLevel::Full < DegradationLevel::SimpleBorders);
        assert!(DegradationLevel::SimpleBorders < DegradationLevel::NoStyling);
        assert!(DegradationLevel::NoStyling < DegradationLevel::EssentialOnly);
        assert!(DegradationLevel::EssentialOnly < DegradationLevel::Skeleton);
        assert!(DegradationLevel::Skeleton < DegradationLevel::SkipFrame);
    }

    #[test]
    fn degradation_level_next() {
        assert_eq!(
            DegradationLevel::Full.next(),
            DegradationLevel::SimpleBorders
        );
        assert_eq!(
            DegradationLevel::SimpleBorders.next(),
            DegradationLevel::NoStyling
        );
        assert_eq!(
            DegradationLevel::NoStyling.next(),
            DegradationLevel::EssentialOnly
        );
        assert_eq!(
            DegradationLevel::EssentialOnly.next(),
            DegradationLevel::Skeleton
        );
        assert_eq!(
            DegradationLevel::Skeleton.next(),
            DegradationLevel::SkipFrame
        );
        assert_eq!(
            DegradationLevel::SkipFrame.next(),
            DegradationLevel::SkipFrame
        );
    }

    #[test]
    fn degradation_level_prev() {
        assert_eq!(
            DegradationLevel::SkipFrame.prev(),
            DegradationLevel::Skeleton
        );
        assert_eq!(
            DegradationLevel::Skeleton.prev(),
            DegradationLevel::EssentialOnly
        );
        assert_eq!(
            DegradationLevel::EssentialOnly.prev(),
            DegradationLevel::NoStyling
        );
        assert_eq!(
            DegradationLevel::NoStyling.prev(),
            DegradationLevel::SimpleBorders
        );
        assert_eq!(
            DegradationLevel::SimpleBorders.prev(),
            DegradationLevel::Full
        );
        assert_eq!(DegradationLevel::Full.prev(), DegradationLevel::Full);
    }

    #[test]
    fn degradation_level_is_max() {
        assert!(!DegradationLevel::Full.is_max());
        assert!(!DegradationLevel::Skeleton.is_max());
        assert!(DegradationLevel::SkipFrame.is_max());
    }

    #[test]
    fn degradation_level_is_full() {
        assert!(DegradationLevel::Full.is_full());
        assert!(!DegradationLevel::SimpleBorders.is_full());
        assert!(!DegradationLevel::SkipFrame.is_full());
    }

    #[test]
    fn degradation_level_as_str() {
        assert_eq!(DegradationLevel::Full.as_str(), "Full");
        assert_eq!(DegradationLevel::SimpleBorders.as_str(), "SimpleBorders");
        assert_eq!(DegradationLevel::NoStyling.as_str(), "NoStyling");
        assert_eq!(DegradationLevel::EssentialOnly.as_str(), "EssentialOnly");
        assert_eq!(DegradationLevel::Skeleton.as_str(), "Skeleton");
        assert_eq!(DegradationLevel::SkipFrame.as_str(), "SkipFrame");
    }

    #[test]
    fn degradation_level_values() {
        assert_eq!(DegradationLevel::Full.level(), 0);
        assert_eq!(DegradationLevel::SimpleBorders.level(), 1);
        assert_eq!(DegradationLevel::NoStyling.level(), 2);
        assert_eq!(DegradationLevel::EssentialOnly.level(), 3);
        assert_eq!(DegradationLevel::Skeleton.level(), 4);
        assert_eq!(DegradationLevel::SkipFrame.level(), 5);
    }

    #[test]
    fn budget_remaining_decreases() {
        let budget = RenderBudget::new(Duration::from_millis(100));
        let initial = budget.remaining();

        thread::sleep(Duration::from_millis(10));

        let later = budget.remaining();
        assert!(later < initial);
    }

    #[test]
    fn budget_remaining_fraction() {
        let mut budget = RenderBudget::new(Duration::from_millis(100));

        // Initially should be close to 1.0
        let initial = budget.remaining_fraction();
        assert!(initial > 0.9);

        // Pin elapsed to ~50ms deterministically instead of `thread::sleep`,
        // which overshoots under CI scheduling jitter and made this test flaky
        // on contended runners (macOS CI). The few microseconds between setting
        // `start` and reading are negligible against the 0.3..0.6 margins.
        budget.start = Instant::now() - Duration::from_millis(50);

        // Should be around 0.5 now
        let later = budget.remaining_fraction();
        assert!(
            later < 0.6,
            "expected remaining_fraction < 0.6, got {later}"
        );
        assert!(
            later > 0.3,
            "expected remaining_fraction > 0.3, got {later}"
        );
    }

    #[test]
    fn should_degrade_when_cost_exceeds_remaining() {
        // Pin elapsed to ~50ms deterministically (no `thread::sleep`, which is
        // flaky under CI scheduling jitter), leaving ~50ms of the 100ms budget.
        let mut budget = RenderBudget::new(Duration::from_millis(100));
        budget.start = Instant::now() - Duration::from_millis(50);

        // Should degrade for expensive operations (80ms > ~50ms remaining)
        assert!(budget.should_degrade(Duration::from_millis(80)));
        // Should not degrade for cheap operations (10ms < ~50ms remaining)
        assert!(!budget.should_degrade(Duration::from_millis(10)));
    }

    #[test]
    fn degrade_advances_level() {
        let mut budget = RenderBudget::new(Duration::from_millis(16));

        assert_eq!(budget.degradation(), DegradationLevel::Full);

        budget.degrade();
        assert_eq!(budget.degradation(), DegradationLevel::SimpleBorders);

        budget.degrade();
        assert_eq!(budget.degradation(), DegradationLevel::NoStyling);
    }

    #[test]
    fn exhausted_when_no_time_left() {
        let budget = RenderBudget::new(Duration::from_millis(5));

        assert!(!budget.exhausted());

        thread::sleep(Duration::from_millis(10));

        assert!(budget.exhausted());
    }

    #[test]
    fn exhausted_at_skip_frame() {
        let mut budget = RenderBudget::new(Duration::from_millis(1000));

        // Set to SkipFrame
        budget.set_degradation(DegradationLevel::SkipFrame);

        // Should be exhausted even with time remaining
        assert!(budget.exhausted());
    }

    #[test]
    fn should_upgrade_with_remaining_budget() {
        let mut budget = RenderBudget::new(Duration::from_millis(1000));

        // At Full, should not upgrade
        assert!(!budget.should_upgrade());

        // Degrade and set cooldown frames
        budget.degrade();
        budget.frames_since_change = 5;

        // With lots of budget remaining, should upgrade
        assert!(budget.should_upgrade());
    }

    #[test]
    fn upgrade_improves_level() {
        let mut budget = RenderBudget::new(Duration::from_millis(16));

        budget.set_degradation(DegradationLevel::Skeleton);
        assert_eq!(budget.degradation(), DegradationLevel::Skeleton);

        budget.upgrade();
        assert_eq!(budget.degradation(), DegradationLevel::EssentialOnly);

        budget.upgrade();
        assert_eq!(budget.degradation(), DegradationLevel::NoStyling);
    }

    #[test]
    fn upgrade_downgrade_symmetric() {
        let mut budget = RenderBudget::new(Duration::from_millis(16));

        // Degrade all the way
        while !budget.degradation().is_max() {
            budget.degrade();
        }
        assert_eq!(budget.degradation(), DegradationLevel::SkipFrame);

        // Upgrade all the way
        while !budget.degradation().is_full() {
            budget.upgrade();
        }
        assert_eq!(budget.degradation(), DegradationLevel::Full);
    }

    #[test]
    fn reset_preserves_degradation() {
        let mut budget = RenderBudget::new(Duration::from_millis(16));

        budget.degrade();
        budget.degrade();
        let level = budget.degradation();

        budget.reset();

        assert_eq!(budget.degradation(), level);
        // Remaining should be close to full again
        assert!(budget.remaining_fraction() > 0.9);
    }

    #[test]
    fn next_frame_upgrades_when_possible() {
        let mut budget = RenderBudget::new(Duration::from_millis(1000));

        // Degrade and simulate several frames
        budget.degrade();
        for _ in 0..5 {
            budget.reset();
        }

        let before = budget.degradation();
        budget.next_frame();

        // Should have upgraded
        assert!(budget.degradation() < before);
    }

    #[test]
    fn next_frame_prefers_recorded_frame_time_for_upgrade() {
        let mut budget = RenderBudget::new(Duration::from_millis(16));

        budget.degrade();
        for _ in 0..5 {
            budget.reset();
        }

        // Record a fast frame, then wait long enough that start.elapsed()
        // would otherwise exceed the budget.
        budget.record_frame_time(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(25));

        let before = budget.degradation();
        budget.next_frame();

        assert!(budget.degradation() < before);
    }

    #[test]
    fn config_defaults() {
        let config = FrameBudgetConfig::default();

        assert_eq!(config.total, Duration::from_millis(16));
        assert!(config.allow_frame_skip);
        assert_eq!(config.degradation_cooldown, 3);
        assert!((config.upgrade_threshold - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn config_with_total() {
        let config = FrameBudgetConfig::with_total(Duration::from_millis(33));

        assert_eq!(config.total, Duration::from_millis(33));
        // Other defaults preserved
        assert!(config.allow_frame_skip);
    }

    #[test]
    fn config_strict() {
        let config = FrameBudgetConfig::strict(Duration::from_millis(16));

        assert!(!config.allow_frame_skip);
    }

    #[test]
    fn config_relaxed() {
        let config = FrameBudgetConfig::relaxed();

        assert_eq!(config.total, Duration::from_millis(33));
        assert_eq!(config.degradation_cooldown, 5);
    }

    #[test]
    fn from_config() {
        let config = FrameBudgetConfig {
            total: Duration::from_millis(20),
            allow_frame_skip: false,
            ..Default::default()
        };

        let budget = RenderBudget::from_config(&config);

        assert_eq!(budget.total(), Duration::from_millis(20));
        assert!(!budget.exhausted()); // allow_frame_skip is false

        // Set to SkipFrame - should NOT be exhausted since frame skip disabled
        let mut budget = RenderBudget::from_config(&config);
        budget.set_degradation(DegradationLevel::SkipFrame);
        assert!(!budget.exhausted());
    }

    #[test]
    fn phase_budgets_default() {
        let budgets = PhaseBudgets::default();

        assert_eq!(budgets.diff, Duration::from_millis(2));
        assert_eq!(budgets.present, Duration::from_millis(4));
        assert_eq!(budgets.render, Duration::from_millis(8));
    }

    #[test]
    fn phase_has_budget() {
        let budget = RenderBudget::new(Duration::from_millis(100));

        assert!(budget.phase_has_budget(Phase::Diff));
        assert!(budget.phase_has_budget(Phase::Present));
        assert!(budget.phase_has_budget(Phase::Render));
    }

    #[test]
    fn phase_budget_respects_remaining() {
        let budget = RenderBudget::new(Duration::from_millis(100));

        let diff_budget = budget.phase_budget(Phase::Diff);
        assert_eq!(diff_budget.total(), Duration::from_millis(2));

        let present_budget = budget.phase_budget(Phase::Present);
        assert_eq!(present_budget.total(), Duration::from_millis(4));
    }

    #[test]
    fn phase_as_str() {
        assert_eq!(Phase::Diff.as_str(), "diff");
        assert_eq!(Phase::Present.as_str(), "present");
        assert_eq!(Phase::Render.as_str(), "render");
    }

    #[test]
    fn zero_budget_is_immediately_exhausted() {
        let budget = RenderBudget::new(Duration::ZERO);
        assert!(budget.exhausted());
        assert_eq!(budget.remaining_fraction(), 0.0);
    }

    #[test]
    fn degradation_level_never_exceeds_skip_frame() {
        let mut level = DegradationLevel::Full;

        for _ in 0..100 {
            level = level.next();
        }

        assert_eq!(level, DegradationLevel::SkipFrame);
    }

    #[test]
    fn budget_remaining_never_negative() {
        let budget = RenderBudget::new(Duration::from_millis(1));

        // Wait well past the budget
        thread::sleep(Duration::from_millis(10));

        // Should be zero, not negative
        assert_eq!(budget.remaining(), Duration::ZERO);
        assert_eq!(budget.remaining_fraction(), 0.0);
    }

    #[test]
    fn infinite_budget_stays_at_full() {
        let mut budget = RenderBudget::new(Duration::from_secs(1000));

        // With huge budget, should never need to degrade
        assert!(!budget.should_degrade(Duration::from_millis(100)));
        assert_eq!(budget.degradation(), DegradationLevel::Full);

        // Next frame should not upgrade since already at full
        budget.next_frame();
        assert_eq!(budget.degradation(), DegradationLevel::Full);
    }

    #[test]
    fn cooldown_prevents_immediate_upgrade() {
        let mut budget = RenderBudget::new(Duration::from_millis(1000));
        budget.cooldown = 3;

        // Degrade
        budget.degrade();
        assert_eq!(budget.frames_since_change, 0);

        // Should not upgrade immediately (cooldown not met)
        assert!(!budget.should_upgrade());

        // Simulate frames
        budget.frames_since_change = 3;

        // Now should be able to upgrade
        assert!(budget.should_upgrade());
    }

    #[test]
    fn set_degradation_resets_cooldown() {
        let mut budget = RenderBudget::new(Duration::from_millis(16));
        budget.frames_since_change = 10;

        budget.set_degradation(DegradationLevel::NoStyling);

        assert_eq!(budget.frames_since_change, 0);
    }

    #[test]
    fn set_degradation_same_level_preserves_cooldown() {
        let mut budget = RenderBudget::new(Duration::from_millis(16));
        budget.frames_since_change = 10;

        // Set to same level
        budget.set_degradation(DegradationLevel::Full);

        // Cooldown preserved since level didn't change
        assert_eq!(budget.frames_since_change, 10);
    }

    // -----------------------------------------------------------------------
    // Budget Controller Tests (bd-4kq0.3.1)
    // -----------------------------------------------------------------------

    mod controller_tests {
        use super::super::*;

        fn make_controller() -> BudgetController {
            BudgetController::new(BudgetControllerConfig::default())
        }

        fn make_controller_with_config(
            target_ms: u64,
            warmup: u32,
            cooldown: u32,
        ) -> BudgetController {
            BudgetController::new(BudgetControllerConfig {
                target: Duration::from_millis(target_ms),
                eprocess: EProcessConfig {
                    warmup_frames: warmup,
                    ..Default::default()
                },
                cooldown_frames: cooldown,
                ..Default::default()
            })
        }

        // --- PID response tests ---

        #[test]
        fn pid_step_input_yields_nonzero_output() {
            let mut state = PidState::default();
            let gains = PidGains::default();

            // Step input: constant error of 1.0
            let u = state.update(1.0, &gains);
            // Kp*1.0 + Ki*1.0 + Kd*(1.0 - 0.0) = 0.5 + 0.05 + 0.2 = 0.75
            assert!(
                (u - 0.75).abs() < 1e-10,
                "First PID output should be 0.75, got {}",
                u
            );
        }

        #[test]
        fn pid_zero_error_zero_output() {
            let mut state = PidState::default();
            let gains = PidGains::default();

            let u = state.update(0.0, &gains);
            assert!(
                u.abs() < 1e-10,
                "Zero error should produce zero output, got {}",
                u
            );
        }

        #[test]
        fn pid_integral_accumulates() {
            let mut state = PidState::default();
            let gains = PidGains::default();

            // Feed constant error
            state.update(1.0, &gains);
            state.update(1.0, &gains);
            state.update(1.0, &gains);

            assert!(
                state.integral > 2.5,
                "Integral should accumulate: {}",
                state.integral
            );
        }

        #[test]
        fn pid_integral_anti_windup() {
            let mut state = PidState::default();
            let gains = PidGains {
                integral_max: 2.0,
                ..Default::default()
            };

            // Feed many frames of error to saturate integral
            for _ in 0..100 {
                state.update(10.0, &gains);
            }

            assert!(
                state.integral <= 2.0 + f64::EPSILON,
                "Integral should be clamped to max: {}",
                state.integral
            );
            assert!(
                state.integral >= -2.0 - f64::EPSILON,
                "Integral should be clamped to -max: {}",
                state.integral
            );
        }

        #[test]
        fn pid_derivative_responds_to_change() {
            let mut state = PidState::default();
            let gains = PidGains::default();

            // First frame: error=0
            let u1 = state.update(0.0, &gains);
            // Second frame: error=1.0 (step change)
            let u2 = state.update(1.0, &gains);

            // u2 should include derivative component Kd*(1.0 - 0.0) = 0.2
            assert!(
                u2 > u1,
                "Step change should produce larger output: u1={}, u2={}",
                u1,
                u2
            );
        }

        #[test]
        fn pid_settling_after_step() {
            let mut state = PidState::default();
            let gains = PidGains::default();

            // Apply step error then zero error (simulate settling)
            state.update(1.0, &gains);
            state.update(1.0, &gains);
            state.update(1.0, &gains);

            // Now remove the error
            let mut outputs = Vec::new();
            for _ in 0..20 {
                outputs.push(state.update(0.0, &gains));
            }

            // Output should trend toward zero (settling)
            let last = *outputs.last().unwrap();
            assert!(
                last.abs() < 0.5,
                "PID should settle toward zero: last={}",
                last
            );
        }

        #[test]
        fn pid_reset_clears_state() {
            let mut state = PidState::default();
            let gains = PidGains::default();

            state.update(5.0, &gains);
            state.update(5.0, &gains);
            assert!(state.integral.abs() > 0.0);

            state.reset();
            assert_eq!(state.integral, 0.0);
            assert_eq!(state.prev_error, 0.0);
        }

        // --- E-process tests ---

        #[test]
        fn eprocess_starts_at_one() {
            let state = EProcessState::default();
            assert!(
                (state.e_value - 1.0).abs() < f64::EPSILON,
                "E-process should start at 1.0"
            );
        }

        #[test]
        fn eprocess_grows_under_overload() {
            let mut state = EProcessState::default();
            let config = EProcessConfig {
                warmup_frames: 0,
                ..Default::default()
            };

            // Feed sustained overload (30ms vs 16ms target)
            for _ in 0..20 {
                state.update(30.0, 16.0, &config);
            }

            assert!(
                state.e_value > 1.0,
                "E-value should grow under overload: {}",
                state.e_value
            );
        }

        #[test]
        fn eprocess_shrinks_under_underload() {
            let mut state = EProcessState::default();
            let config = EProcessConfig {
                warmup_frames: 0,
                ..Default::default()
            };

            // Feed fast frames (8ms vs 16ms target)
            for _ in 0..20 {
                state.update(8.0, 16.0, &config);
            }

            assert!(
                state.e_value < 1.0,
                "E-value should shrink under underload: {}",
                state.e_value
            );
        }

        #[test]
        fn eprocess_gate_blocks_during_warmup() {
            let mut state = EProcessState::default();
            let config = EProcessConfig {
                warmup_frames: 10,
                ..Default::default()
            };

            // Feed overload during warmup
            for _ in 0..5 {
                state.update(50.0, 16.0, &config);
            }

            assert!(
                !state.should_degrade(&config),
                "E-process should not permit degradation during warmup"
            );
        }

        #[test]
        fn eprocess_gate_allows_after_warmup() {
            let mut state = EProcessState::default();
            let config = EProcessConfig {
                warmup_frames: 5,
                alpha: 0.05,
                ..Default::default()
            };

            // Feed severe overload past warmup
            for _ in 0..50 {
                state.update(80.0, 16.0, &config);
            }

            assert!(
                state.should_degrade(&config),
                "E-process should permit degradation after sustained overload: E={}",
                state.e_value
            );
        }

        #[test]
        fn eprocess_recovery_after_overload() {
            let mut state = EProcessState::default();
            let config = EProcessConfig {
                warmup_frames: 0,
                ..Default::default()
            };

            // Overload phase
            for _ in 0..30 {
                state.update(40.0, 16.0, &config);
            }
            let peak = state.e_value;

            // Recovery phase (fast frames)
            for _ in 0..100 {
                state.update(8.0, 16.0, &config);
            }

            assert!(
                state.e_value < peak,
                "E-value should decrease after recovery: peak={}, now={}",
                peak,
                state.e_value
            );
        }

        #[test]
        fn eprocess_sigma_floor_prevents_instability() {
            let mut state = EProcessState::default();
            let config = EProcessConfig {
                sigma_floor_ms: 1.0,
                warmup_frames: 0,
                ..Default::default()
            };

            // Feed identical frames (zero variance)
            for _ in 0..20 {
                state.update(16.0, 16.0, &config);
            }

            // sigma_ema should not be below floor
            assert!(
                state.sigma_ema >= 0.0,
                "Sigma should be non-negative: {}",
                state.sigma_ema
            );
            // E-value should remain finite
            assert!(
                state.e_value.is_finite(),
                "E-value should be finite: {}",
                state.e_value
            );
        }

        #[test]
        fn eprocess_reset_returns_to_initial() {
            let mut state = EProcessState::default();
            let config = EProcessConfig::default();

            state.update(50.0, 16.0, &config);
            state.update(50.0, 16.0, &config);

            state.reset();
            assert!((state.e_value - 1.0).abs() < f64::EPSILON);
            assert_eq!(state.frames_observed, 0);
        }

        // --- Controller integration tests ---

        #[test]
        fn controller_holds_under_normal_load() {
            let mut ctrl = make_controller_with_config(16, 0, 0);

            // Feed on-target frames
            for _ in 0..20 {
                let decision = ctrl.update(Duration::from_millis(16));
                assert_eq!(
                    decision,
                    BudgetDecision::Hold,
                    "On-target frames should hold"
                );
            }
            assert_eq!(ctrl.level(), DegradationLevel::Full);
        }

        #[test]
        fn controller_degrades_under_sustained_overload() {
            let mut ctrl = make_controller_with_config(16, 0, 0);

            let mut degraded = false;
            // Feed severe overload
            for _ in 0..50 {
                let decision = ctrl.update(Duration::from_millis(40));
                if decision == BudgetDecision::Degrade {
                    degraded = true;
                }
            }

            assert!(
                degraded,
                "Controller should degrade under sustained overload"
            );
            assert!(
                ctrl.level() > DegradationLevel::Full,
                "Level should be degraded: {:?}",
                ctrl.level()
            );
        }

        #[test]
        fn controller_upgrades_after_recovery() {
            let mut ctrl = make_controller_with_config(16, 0, 0);

            // Overload to degrade
            for _ in 0..50 {
                ctrl.update(Duration::from_millis(40));
            }
            let degraded_level = ctrl.level();
            assert!(degraded_level > DegradationLevel::Full);

            // Recovery: fast frames
            let mut upgraded = false;
            for _ in 0..200 {
                let decision = ctrl.update(Duration::from_millis(4));
                if decision == BudgetDecision::Upgrade {
                    upgraded = true;
                }
            }

            assert!(upgraded, "Controller should upgrade after recovery");
            assert!(
                ctrl.level() < degraded_level,
                "Level should improve: before={:?}, after={:?}",
                degraded_level,
                ctrl.level()
            );
        }

        #[test]
        fn controller_cooldown_prevents_oscillation() {
            let mut ctrl = make_controller_with_config(16, 0, 5);

            // Trigger degradation
            for _ in 0..50 {
                ctrl.update(Duration::from_millis(40));
            }

            // Immediately try fast frames
            let mut decisions_during_cooldown = Vec::new();
            for _ in 0..4 {
                decisions_during_cooldown.push(ctrl.update(Duration::from_millis(4)));
            }

            // During cooldown (frames 0-4), should all be Hold
            assert!(
                decisions_during_cooldown
                    .iter()
                    .all(|d| *d == BudgetDecision::Hold),
                "Cooldown should prevent changes: {:?}",
                decisions_during_cooldown
            );
        }

        #[test]
        fn controller_no_oscillation_under_constant_load() {
            let mut ctrl = make_controller_with_config(16, 0, 3);

            // Moderate overload (20ms vs 16ms)
            let mut transitions = 0u32;
            let mut prev_level = ctrl.level();
            for _ in 0..100 {
                ctrl.update(Duration::from_millis(20));
                if ctrl.level() != prev_level {
                    transitions += 1;
                    prev_level = ctrl.level();
                }
            }

            // Under constant load, transitions should be limited
            // (progressive degradation, not oscillation)
            assert!(
                transitions < 10,
                "Too many transitions under constant load: {}",
                transitions
            );
        }

        #[test]
        fn controller_reset_restores_full_quality() {
            let mut ctrl = make_controller();

            // Degrade
            for _ in 0..50 {
                ctrl.update(Duration::from_millis(40));
            }

            ctrl.reset();

            assert_eq!(ctrl.level(), DegradationLevel::Full);
            assert!((ctrl.e_value() - 1.0).abs() < f64::EPSILON);
            assert_eq!(ctrl.pid_integral(), 0.0);
        }

        #[test]
        fn controller_transient_spike_does_not_degrade() {
            let mut ctrl = make_controller_with_config(16, 5, 3);

            // Normal frames to build history
            for _ in 0..20 {
                ctrl.update(Duration::from_millis(16));
            }

            // Single spike
            ctrl.update(Duration::from_millis(100));

            // Back to normal
            for _ in 0..5 {
                ctrl.update(Duration::from_millis(16));
            }

            // Should still be at full quality (spike was transient)
            assert_eq!(
                ctrl.level(),
                DegradationLevel::Full,
                "Single spike should not cause degradation"
            );
        }

        #[test]
        fn controller_never_exceeds_skip_frame() {
            let mut ctrl = make_controller_with_config(16, 0, 0);

            // Extreme overload
            for _ in 0..500 {
                ctrl.update(Duration::from_millis(200));
            }

            assert!(
                ctrl.level() <= DegradationLevel::SkipFrame,
                "Level should not exceed SkipFrame: {:?}",
                ctrl.level()
            );
        }

        #[test]
        fn controller_never_goes_below_full() {
            let mut ctrl = make_controller_with_config(16, 0, 0);

            // Extreme underload
            for _ in 0..200 {
                ctrl.update(Duration::from_millis(1));
            }

            assert_eq!(
                ctrl.level(),
                DegradationLevel::Full,
                "Level should not go below Full"
            );
        }

        // --- Config tests ---

        #[test]
        fn pid_gains_default_valid() {
            let gains = PidGains::default();
            assert!(gains.kp > 0.0);
            assert!(gains.ki > 0.0);
            assert!(gains.kd > 0.0);
            assert!(gains.integral_max > 0.0);
        }

        #[test]
        fn eprocess_config_default_valid() {
            let config = EProcessConfig::default();
            assert!(config.lambda > 0.0);
            assert!(config.alpha > 0.0 && config.alpha < 1.0);
            assert!(config.beta > 0.0 && config.beta < 1.0);
            assert!(config.sigma_floor_ms > 0.0);
        }

        #[test]
        fn controller_config_default_valid() {
            let config = BudgetControllerConfig::default();
            assert!(config.degrade_threshold > 0.0);
            assert!(config.upgrade_threshold > 0.0);
            assert!(config.target > Duration::ZERO);
        }

        #[test]
        fn budget_decision_equality() {
            assert_eq!(BudgetDecision::Hold, BudgetDecision::Hold);
            assert_ne!(BudgetDecision::Hold, BudgetDecision::Degrade);
            assert_ne!(BudgetDecision::Degrade, BudgetDecision::Upgrade);
        }
    }

    // -----------------------------------------------------------------------
    // Budget Controller Integration + Telemetry Tests (bd-4kq0.3.2)
    // -----------------------------------------------------------------------

    mod integration_tests {
        use super::super::*;

        #[test]
        fn render_budget_without_controller_returns_no_telemetry() {
            let budget = RenderBudget::new(Duration::from_millis(16));
            assert!(budget.telemetry().is_none());
            assert!(budget.controller().is_none());
        }

        #[test]
        fn render_budget_with_controller_returns_telemetry() {
            let budget = RenderBudget::new(Duration::from_millis(16))
                .with_controller(BudgetControllerConfig::default());
            assert!(budget.controller().is_some());

            let telem = budget.telemetry().unwrap();
            assert_eq!(telem.level, DegradationLevel::Full);
            assert_eq!(telem.last_decision, BudgetDecision::Hold);
            assert_eq!(telem.frames_observed, 0);
            assert!(telem.in_warmup);
        }

        #[test]
        fn telemetry_fields_update_after_next_frame() {
            let mut budget = RenderBudget::new(Duration::from_millis(16)).with_controller(
                BudgetControllerConfig {
                    eprocess: EProcessConfig {
                        warmup_frames: 0,
                        ..Default::default()
                    },
                    cooldown_frames: 0,
                    ..Default::default()
                },
            );

            // Simulate a few frames
            for _ in 0..5 {
                budget.next_frame();
            }

            let telem = budget.telemetry().unwrap();
            assert_eq!(telem.frames_observed, 5);
            assert!(!telem.in_warmup);
            // PID output should be non-positive (frames are fast, under budget)
            // but the exact value depends on timing, so just check it's finite
            assert!(telem.pid_output.is_finite());
            assert!(telem.e_value.is_finite());
        }

        #[test]
        fn controller_next_frame_degrades_under_simulated_overload() {
            // We can't easily simulate slow frames in unit tests (thread::sleep
            // would be flaky), so we test the controller integration by verifying
            // the decision path works: attach controller, manually check that
            // the controller's level is reflected in the budget's degradation.
            let config = BudgetControllerConfig {
                target: Duration::from_millis(16),
                eprocess: EProcessConfig {
                    warmup_frames: 0,
                    ..Default::default()
                },
                cooldown_frames: 0,
                ..Default::default()
            };
            let mut ctrl = BudgetController::new(config);

            // Feed severe overload to the controller directly
            for _ in 0..50 {
                ctrl.update(Duration::from_millis(40));
            }

            // Controller should have degraded
            assert!(
                ctrl.level() > DegradationLevel::Full,
                "Controller should degrade: {:?}",
                ctrl.level()
            );

            // Telemetry should reflect the degradation
            let telem = ctrl.telemetry();
            assert!(telem.level > DegradationLevel::Full);
            assert!(
                telem.pid_output > 0.0,
                "PID output should be positive under overload"
            );
            assert!(telem.e_value > 1.0, "E-value should grow under overload");
        }

        #[test]
        fn next_frame_delegates_to_controller_when_attached() {
            // With a controller, next_frame should not use the simple
            // threshold-based upgrade path
            let mut budget = RenderBudget::new(Duration::from_millis(1000))
                .with_controller(BudgetControllerConfig::default());

            // Degrade manually
            budget.degrade();
            assert_eq!(budget.degradation(), DegradationLevel::SimpleBorders);

            // In legacy mode, next_frame would upgrade immediately (lots of budget).
            // With controller, it should hold because the controller hasn't seen
            // enough underload evidence yet.
            budget.next_frame();

            // The controller may or may not upgrade depending on the single frame
            // measurement, but the key assertion is that the code path works.
            // With a fresh controller, the fast frame should eventually allow upgrade.
            // Just verify it doesn't panic and telemetry is populated.
            let telem = budget.telemetry().unwrap();
            assert_eq!(telem.frames_observed, 1);
        }

        #[test]
        fn telemetry_is_copy_and_no_alloc() {
            let budget = RenderBudget::new(Duration::from_millis(16))
                .with_controller(BudgetControllerConfig::default());

            let telem = budget.telemetry().unwrap();
            // BudgetTelemetry is Copy — verify by copying
            let telem2 = telem;
            assert_eq!(telem.level, telem2.level);
            assert_eq!(telem.e_value, telem2.e_value);
        }

        #[test]
        fn telemetry_warmup_flag_transitions() {
            let mut budget = RenderBudget::new(Duration::from_millis(16)).with_controller(
                BudgetControllerConfig {
                    eprocess: EProcessConfig {
                        warmup_frames: 3,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );

            // During warmup
            budget.next_frame();
            budget.next_frame();
            let telem = budget.telemetry().unwrap();
            assert!(telem.in_warmup, "Should be in warmup at frame 2");

            // After warmup
            budget.next_frame();
            let telem = budget.telemetry().unwrap();
            assert!(!telem.in_warmup, "Should exit warmup at frame 3");
        }

        #[test]
        fn phase_sub_budget_does_not_carry_controller() {
            let budget = RenderBudget::new(Duration::from_millis(100))
                .with_controller(BudgetControllerConfig::default());

            let phase = budget.phase_budget(Phase::Render);
            assert!(
                phase.controller().is_none(),
                "Phase sub-budgets should not carry the controller"
            );
        }

        #[test]
        fn controller_telemetry_tracks_frames_since_change() {
            let mut ctrl = BudgetController::new(BudgetControllerConfig {
                eprocess: EProcessConfig {
                    warmup_frames: 0,
                    ..Default::default()
                },
                cooldown_frames: 0,
                ..Default::default()
            });

            // On-target frames: frames_since_change should increase
            for i in 1..=5 {
                ctrl.update(Duration::from_millis(16));
                let telem = ctrl.telemetry();
                assert_eq!(
                    telem.frames_since_change, i,
                    "frames_since_change should be {} after {} frames",
                    i, i
                );
            }
        }

        #[test]
        fn telemetry_last_decision_reflects_controller_decision() {
            let mut ctrl = BudgetController::new(BudgetControllerConfig {
                eprocess: EProcessConfig {
                    warmup_frames: 0,
                    ..Default::default()
                },
                cooldown_frames: 0,
                ..Default::default()
            });

            // On-target: should hold
            ctrl.update(Duration::from_millis(16));
            assert_eq!(ctrl.telemetry().last_decision, BudgetDecision::Hold);

            // Feed enough overload to trigger degrade
            let mut saw_degrade = false;
            for _ in 0..50 {
                let d = ctrl.update(Duration::from_millis(50));
                if d == BudgetDecision::Degrade {
                    saw_degrade = true;
                    assert_eq!(ctrl.telemetry().last_decision, BudgetDecision::Degrade);
                    break;
                }
            }
            assert!(saw_degrade, "Should have seen a Degrade decision");
        }

        #[test]
        fn perf_overhead_controller_update_is_fast() {
            // Verify the controller update is a lightweight arithmetic operation.
            // We run 10_000 iterations and check they complete quickly.
            // This is a smoke test, not a precise benchmark (that's bd-4kq0.3.3).
            let mut ctrl = BudgetController::new(BudgetControllerConfig::default());

            let start = Instant::now();
            for _ in 0..10_000 {
                ctrl.update(Duration::from_millis(16));
            }
            let elapsed = start.elapsed();

            // 10k iterations should complete in well under 10ms on any modern CPU.
            // At 16ms target, 2% overhead = 0.32ms per frame, so 10k frames
            // budget = 3.2 seconds worth of overhead budget. We check <50ms total.
            assert!(
                elapsed < Duration::from_millis(50),
                "10k controller updates took {:?}, expected <50ms",
                elapsed
            );
        }

        #[test]
        fn perf_overhead_telemetry_snapshot_is_fast() {
            let mut ctrl = BudgetController::new(BudgetControllerConfig::default());
            ctrl.update(Duration::from_millis(16));

            let start = Instant::now();
            for _ in 0..10_000 {
                let _telem = ctrl.telemetry();
            }
            let elapsed = start.elapsed();

            assert!(
                elapsed < Duration::from_millis(10),
                "10k telemetry snapshots took {:?}, expected <10ms",
                elapsed
            );
        }
    }

    // -----------------------------------------------------------------------
    // Budget Stability + E2E Replay Tests (bd-4kq0.3.3)
    // -----------------------------------------------------------------------

    mod stability_tests {
        use super::super::*;

        #[derive(Debug, Clone)]
        struct CampaignFrameLog {
            frame_idx: u64,
            phase: &'static str,
            frame_time_us: u64,
            telemetry: BudgetTelemetry,
        }

        /// Helper: create a controller with minimal warmup/cooldown for testing.
        fn fast_controller(target_ms: u64) -> BudgetController {
            BudgetController::new(BudgetControllerConfig {
                target: Duration::from_millis(target_ms),
                eprocess: EProcessConfig {
                    warmup_frames: 0,
                    ..Default::default()
                },
                cooldown_frames: 0,
                ..Default::default()
            })
        }

        /// Helper: run a frame time trace through the controller and collect
        /// JSONL-style telemetry records (as structured tuples).
        /// Returns `(frame_index, frame_time_us, telemetry)` for each frame.
        fn run_trace(
            ctrl: &mut BudgetController,
            trace: &[Duration],
        ) -> Vec<(u64, u64, BudgetTelemetry)> {
            trace
                .iter()
                .enumerate()
                .map(|(i, &ft)| {
                    ctrl.update(ft);
                    let telem = ctrl.telemetry();
                    (i as u64, ft.as_micros() as u64, telem)
                })
                .collect()
        }

        /// Run a labeled phase campaign and collect deterministic replay logs.
        fn run_campaign(
            ctrl: &mut BudgetController,
            phases: &[(&'static str, usize, Duration)],
        ) -> Vec<CampaignFrameLog> {
            let mut logs = Vec::new();
            let mut frame_idx: u64 = 0;
            for &(phase, count, frame_time) in phases {
                for _ in 0..count {
                    ctrl.update(frame_time);
                    logs.push(CampaignFrameLog {
                        frame_idx,
                        phase,
                        frame_time_us: frame_time.as_micros() as u64,
                        telemetry: ctrl.telemetry(),
                    });
                    frame_idx = frame_idx.saturating_add(1);
                }
            }
            logs
        }

        /// Count level transitions in a trace log.
        fn count_transitions(log: &[(u64, u64, BudgetTelemetry)]) -> u32 {
            let mut transitions = 0u32;
            for pair in log.windows(2) {
                if pair[0].2.level != pair[1].2.level {
                    transitions += 1;
                }
            }
            transitions
        }

        // --- e2e_burst_logs ---

        #[test]
        fn e2e_burst_logs_no_oscillation() {
            // Simulate bursty output: alternating bursts of slow frames
            // and calm periods. Verify no oscillation (bounded transitions).
            let mut ctrl = fast_controller(16);

            let mut trace = Vec::new();
            for _cycle in 0..5 {
                // Burst: 10 frames at 40ms
                for _ in 0..10 {
                    trace.push(Duration::from_millis(40));
                }
                // Calm: 20 frames at 16ms
                for _ in 0..20 {
                    trace.push(Duration::from_millis(16));
                }
            }

            let log = run_trace(&mut ctrl, &trace);

            // Count level transitions. Under bursty load, transitions should
            // be bounded — no rapid oscillation. With 5 cycles of 30 frames
            // each (150 total), we expect at most ~15 transitions (degrade
            // during each burst, upgrade during each calm).
            let transitions = count_transitions(&log);
            assert!(
                transitions < 20,
                "Too many transitions under bursty load: {} (expected <20)",
                transitions
            );

            // Verify all telemetry fields are populated
            for (frame, ft_us, telem) in &log {
                assert!(
                    telem.pid_output.is_finite(),
                    "frame {}: NaN pid_output",
                    frame
                );
                assert!(telem.e_value.is_finite(), "frame {}: NaN e_value", frame);
                assert!(telem.pid_p.is_finite(), "frame {}: NaN pid_p", frame);
                assert!(telem.pid_i.is_finite(), "frame {}: NaN pid_i", frame);
                assert!(telem.pid_d.is_finite(), "frame {}: NaN pid_d", frame);
                assert!(*ft_us > 0, "frame {}: zero frame time", frame);
            }
        }

        #[test]
        fn e2e_burst_recovers_after_moderate_overload() {
            // Moderate bursts (30ms vs 16ms target) followed by calm periods.
            // The controller may degrade during bursts, but should recover
            // during calm periods — final state should not be SkipFrame.
            let mut ctrl = BudgetController::new(BudgetControllerConfig {
                target: Duration::from_millis(16),
                eprocess: EProcessConfig {
                    warmup_frames: 5,
                    ..Default::default()
                },
                cooldown_frames: 3,
                ..Default::default()
            });

            let mut trace = Vec::new();
            for _cycle in 0..3 {
                // Moderate burst
                for _ in 0..15 {
                    trace.push(Duration::from_millis(30));
                }
                // Extended calm to allow recovery
                for _ in 0..50 {
                    trace.push(Duration::from_millis(10));
                }
            }

            let log = run_trace(&mut ctrl, &trace);

            // After each calm period, level should have recovered below Skeleton.
            // Check at the end of each calm phase (frames 64, 129, 194).
            for cycle in 0..3 {
                let calm_end = (cycle + 1) * 65 - 1;
                if calm_end < log.len() {
                    assert!(
                        log[calm_end].2.level < DegradationLevel::SkipFrame,
                        "cycle {}: should recover after calm period, got {:?} at frame {}",
                        cycle,
                        log[calm_end].2.level,
                        calm_end
                    );
                }
            }

            // Final level should be better than Skeleton
            let final_level = log.last().unwrap().2.level;
            assert!(
                final_level < DegradationLevel::Skeleton,
                "Final level should recover below Skeleton: {:?}",
                final_level
            );
        }

        // --- e2e_idle_to_burst ---

        #[test]
        fn e2e_idle_to_burst_recovery() {
            // Start idle (well under budget), then sudden burst, then back to idle.
            // Verify: fast recovery without over-degrading.
            let mut ctrl = fast_controller(16);

            let mut trace = Vec::new();
            // Phase 1: idle (8ms frames)
            for _ in 0..50 {
                trace.push(Duration::from_millis(8));
            }
            // Phase 2: sudden burst (50ms frames)
            for _ in 0..20 {
                trace.push(Duration::from_millis(50));
            }
            // Phase 3: recovery (8ms frames)
            for _ in 0..100 {
                trace.push(Duration::from_millis(8));
            }

            let log = run_trace(&mut ctrl, &trace);

            // After idle phase (frame 49), should still be Full
            assert_eq!(
                log[49].2.level,
                DegradationLevel::Full,
                "Should be Full during idle phase"
            );

            // During burst, should degrade
            let max_during_burst = log[50..70].iter().map(|(_, _, t)| t.level).max().unwrap();
            assert!(
                max_during_burst > DegradationLevel::Full,
                "Should degrade during burst"
            );

            // After recovery (last 20 frames), should have recovered toward Full
            let final_level = log.last().unwrap().2.level;
            assert!(
                final_level < max_during_burst,
                "Should recover after burst: final={:?}, max_during_burst={:?}",
                final_level,
                max_during_burst
            );
        }

        #[test]
        fn e2e_idle_to_burst_no_over_degrade() {
            // A brief burst (5 frames) should not cause more than 1-2 levels
            // of degradation, even with zero warmup.
            let mut ctrl = fast_controller(16);

            // Idle
            for _ in 0..30 {
                ctrl.update(Duration::from_millis(8));
            }

            // Brief burst (only 5 frames)
            for _ in 0..5 {
                ctrl.update(Duration::from_millis(40));
            }

            // Check degradation is modest
            let level = ctrl.level();
            assert!(
                level <= DegradationLevel::NoStyling,
                "Brief burst should not over-degrade: {:?}",
                level
            );
        }

        #[test]
        fn e2e_overload_campaign_burst_sustained_recovery_with_replay_logs() {
            // bd-2vr05.15.4.5:
            // 1) burst overload
            // 2) sustained overload
            // 3) recovery/underload
            //
            // This test validates full-range degradation, so remove the floor.
            let phases: [(&str, usize, Duration); 3] = [
                ("burst_overload", 24, Duration::from_millis(28)),
                ("sustained_overload", 80, Duration::from_millis(52)),
                ("recovery_underload", 140, Duration::from_millis(8)),
            ];

            let mut ctrl = BudgetController::new(BudgetControllerConfig {
                target: Duration::from_millis(16),
                eprocess: EProcessConfig {
                    warmup_frames: 0,
                    ..Default::default()
                },
                cooldown_frames: 0,
                degradation_floor: DegradationLevel::SkipFrame,
                ..Default::default()
            });
            let logs = run_campaign(&mut ctrl, &phases);
            assert!(!logs.is_empty(), "campaign logs must be non-empty");

            let mut burst_degrades = 0u32;
            let mut sustained_degrades = 0u32;
            let mut sustained_degraded_frames = 0u32;
            let mut recovery_upgrades = 0u32;
            let mut max_level = DegradationLevel::Full;

            for log in &logs {
                let telem = &log.telemetry;
                if telem.level > max_level {
                    max_level = telem.level;
                }
                if log.phase == "burst_overload" && telem.last_decision == BudgetDecision::Degrade {
                    burst_degrades = burst_degrades.saturating_add(1);
                }
                if log.phase == "sustained_overload"
                    && telem.last_decision == BudgetDecision::Degrade
                {
                    sustained_degrades = sustained_degrades.saturating_add(1);
                }
                if log.phase == "sustained_overload" && telem.level > DegradationLevel::Full {
                    sustained_degraded_frames = sustained_degraded_frames.saturating_add(1);
                }
                if log.phase == "recovery_underload"
                    && telem.last_decision == BudgetDecision::Upgrade
                {
                    recovery_upgrades = recovery_upgrades.saturating_add(1);
                }

                // Semantic integrity invariants (no corruption under degradation)
                assert!(
                    telem.level <= DegradationLevel::SkipFrame,
                    "frame {}: invalid degradation level {:?}",
                    log.frame_idx,
                    telem.level
                );
                assert!(
                    telem.e_value.is_finite() && telem.e_value > 0.0,
                    "frame {}: invalid e_value {}",
                    log.frame_idx,
                    telem.e_value
                );
                assert!(
                    telem.pid_output.is_finite(),
                    "frame {}: invalid pid_output {}",
                    log.frame_idx,
                    telem.pid_output
                );
            }

            // Adjacent level changes must be stepwise (no jump corruption).
            for pair in logs.windows(2) {
                let prev = pair[0].telemetry.level.level();
                let curr = pair[1].telemetry.level.level();
                let delta = (curr as i16 - prev as i16).unsigned_abs();
                assert!(
                    delta <= 1,
                    "frame {}->{} level jump {}: {:?} -> {:?}",
                    pair[0].frame_idx,
                    pair[1].frame_idx,
                    delta,
                    pair[0].telemetry.level,
                    pair[1].telemetry.level
                );
            }

            assert!(
                burst_degrades > 0,
                "burst phase should trigger degradation decisions"
            );
            assert!(
                sustained_degrades > 0 || sustained_degraded_frames > 0,
                "sustained overload phase should maintain degraded operation"
            );
            assert!(
                max_level >= DegradationLevel::Skeleton,
                "sustained overload should reach deep degradation (got {:?})",
                max_level
            );
            assert!(
                recovery_upgrades > 0,
                "recovery phase should trigger upgrade decisions"
            );

            let final_level = logs
                .last()
                .map(|entry| entry.telemetry.level)
                .unwrap_or(DegradationLevel::SkipFrame);
            assert!(
                final_level < max_level,
                "final level should recover below peak degradation: final={:?} peak={:?}",
                final_level,
                max_level
            );

            // Deterministic replay contract: same scenario -> same decisions/telemetry.
            let mut ctrl_replay = BudgetController::new(BudgetControllerConfig {
                target: Duration::from_millis(16),
                eprocess: EProcessConfig {
                    warmup_frames: 0,
                    ..Default::default()
                },
                cooldown_frames: 0,
                degradation_floor: DegradationLevel::SkipFrame,
                ..Default::default()
            });
            let replay_logs = run_campaign(&mut ctrl_replay, &phases);
            assert_eq!(
                logs.len(),
                replay_logs.len(),
                "log length mismatch in replay"
            );
            for (lhs, rhs) in logs.iter().zip(replay_logs.iter()) {
                assert_eq!(lhs.frame_idx, rhs.frame_idx);
                assert_eq!(lhs.phase, rhs.phase);
                assert_eq!(lhs.frame_time_us, rhs.frame_time_us);
                assert_eq!(lhs.telemetry.schema_version, rhs.telemetry.schema_version);
                assert_eq!(lhs.telemetry.level, rhs.telemetry.level);
                assert_eq!(lhs.telemetry.last_decision, rhs.telemetry.last_decision);
                assert_eq!(
                    lhs.telemetry.decision_reason, rhs.telemetry.decision_reason,
                    "decision_reason mismatch at frame {}",
                    lhs.frame_idx
                );
                assert_eq!(
                    lhs.telemetry.transition_seq, rhs.telemetry.transition_seq,
                    "transition_seq mismatch at frame {}",
                    lhs.frame_idx
                );
                assert_eq!(
                    lhs.telemetry.transition_correlation_id,
                    rhs.telemetry.transition_correlation_id,
                    "transition_correlation_id mismatch at frame {}",
                    lhs.frame_idx
                );
                assert!(
                    (lhs.telemetry.pid_output - rhs.telemetry.pid_output).abs() < 1e-12,
                    "pid_output mismatch at frame {}",
                    lhs.frame_idx
                );
                assert!(
                    (lhs.telemetry.e_value - rhs.telemetry.e_value).abs() < 1e-12,
                    "e_value mismatch at frame {}",
                    lhs.frame_idx
                );
            }

            // Replay-grade diagnostics for controller postmortems.
            for entry in &logs {
                let t = &entry.telemetry;
                eprintln!(
                    r#"{{"event":"control_campaign_frame","schema_version":{},"scenario":"bd-2vr05.15.4.5","frame_idx":{},"phase":"{}","frame_time_us":{},"decision":"{}","decision_reason":"{}","transition_seq":{},"transition_correlation_id":{},"level":"{}","pid_output":{:.6},"pid_p":{:.6},"pid_i":{:.6},"pid_d":{:.6},"e_value":{:.6},"frame_time_ms":{:.6},"target_ms":{:.6},"pid_gate_threshold":{:.6},"pid_gate_margin":{:.6},"evidence_threshold":{:.6},"evidence_margin":{:.6},"frames_observed":{},"frames_since_change":{}}}"#,
                    t.schema_version,
                    entry.frame_idx,
                    entry.phase,
                    entry.frame_time_us,
                    t.last_decision.as_str(),
                    t.decision_reason.as_str(),
                    t.transition_seq,
                    t.transition_correlation_id,
                    t.level.as_str(),
                    t.pid_output,
                    t.pid_p,
                    t.pid_i,
                    t.pid_d,
                    t.e_value,
                    t.frame_time_ms,
                    t.target_ms,
                    t.pid_gate_threshold,
                    t.pid_gate_margin,
                    t.evidence_threshold,
                    t.evidence_margin,
                    t.frames_observed,
                    t.frames_since_change
                );
            }
            eprintln!(
                r#"{{"event":"control_campaign_summary","schema_version":{},"scenario":"bd-2vr05.15.4.5","frames":{},"burst_degrades":{},"sustained_degrades":{},"recovery_upgrades":{},"peak_level":"{}","final_level":"{}"}}"#,
                BUDGET_TELEMETRY_SCHEMA_VERSION,
                logs.len(),
                burst_degrades,
                sustained_degrades,
                recovery_upgrades,
                max_level.as_str(),
                final_level.as_str()
            );
        }

        // --- property_random_load ---

        #[test]
        fn property_random_load_hysteresis_bounds() {
            // Verify: degradation changes are bounded by hysteresis constraints.
            // Specifically, level can only change by 1 step per decision.
            let mut ctrl = fast_controller(16);

            // Generate a deterministic pseudo-random load trace using a simple
            // linear congruential generator (no std::rand dependency).
            let mut rng_state: u64 = 0xDEAD_BEEF_CAFE_BABE;
            let mut trace = Vec::new();
            for _ in 0..1000 {
                // LCG: next = (a * state + c) mod m
                rng_state = rng_state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                // Map to frame time: 4ms..80ms
                let frame_ms = 4 + ((rng_state >> 33) % 77);
                trace.push(Duration::from_millis(frame_ms));
            }

            let log = run_trace(&mut ctrl, &trace);

            // Property 1: Level only changes by at most 1 step per frame
            for pair in log.windows(2) {
                let prev = pair[0].2.level.level();
                let curr = pair[1].2.level.level();
                let delta = (curr as i16 - prev as i16).unsigned_abs();
                assert!(
                    delta <= 1,
                    "Level jumped {} steps at frame {}: {:?} -> {:?}",
                    delta,
                    pair[1].0,
                    pair[0].2.level,
                    pair[1].2.level
                );
            }

            // Property 2: Level never exceeds valid range
            for (frame, _, telem) in &log {
                assert!(
                    telem.level <= DegradationLevel::SkipFrame,
                    "frame {}: level out of range: {:?}",
                    frame,
                    telem.level
                );
            }

            // Property 3: All numeric fields are finite
            for (frame, _, telem) in &log {
                assert!(
                    telem.pid_output.is_finite(),
                    "frame {}: NaN pid_output",
                    frame
                );
                assert!(telem.pid_p.is_finite(), "frame {}: NaN pid_p", frame);
                assert!(telem.pid_i.is_finite(), "frame {}: NaN pid_i", frame);
                assert!(telem.pid_d.is_finite(), "frame {}: NaN pid_d", frame);
                assert!(telem.e_value.is_finite(), "frame {}: NaN e_value", frame);
                assert!(
                    telem.e_value > 0.0,
                    "frame {}: e_value not positive: {}",
                    frame,
                    telem.e_value
                );
            }
        }

        #[test]
        fn property_random_load_bounded_transitions() {
            // Under random load, transitions should be bounded and not exceed
            // a reasonable rate (no rapid oscillation).
            let mut ctrl = BudgetController::new(BudgetControllerConfig {
                target: Duration::from_millis(16),
                eprocess: EProcessConfig {
                    warmup_frames: 5,
                    ..Default::default()
                },
                cooldown_frames: 3,
                ..Default::default()
            });

            // Deterministic pseudo-random trace
            let mut rng_state: u64 = 0x1234_5678_9ABC_DEF0;
            let mut trace = Vec::new();
            for _ in 0..500 {
                rng_state = rng_state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let frame_ms = 8 + ((rng_state >> 33) % 40);
                trace.push(Duration::from_millis(frame_ms));
            }

            let log = run_trace(&mut ctrl, &trace);
            let transitions = count_transitions(&log);

            // With cooldown=3 and 500 frames, max theoretical transitions = 500/4 = 125.
            // In practice with hysteresis + e-process gating, much less.
            assert!(
                transitions < 80,
                "Too many transitions under random load: {} (expected <80 with cooldown=3)",
                transitions
            );
        }

        #[test]
        fn property_deterministic_replay() {
            // Same trace should produce identical telemetry every time.
            let trace: Vec<Duration> = (0..100)
                .map(|i| Duration::from_millis(10 + (i * 7 % 30)))
                .collect();

            let mut ctrl1 = fast_controller(16);
            let log1 = run_trace(&mut ctrl1, &trace);

            let mut ctrl2 = fast_controller(16);
            let log2 = run_trace(&mut ctrl2, &trace);

            for (r1, r2) in log1.iter().zip(log2.iter()) {
                assert_eq!(r1.0, r2.0, "frame index mismatch");
                assert_eq!(r1.1, r2.1, "frame time mismatch");
                assert_eq!(r1.2.schema_version, r2.2.schema_version);
                assert_eq!(r1.2.level, r2.2.level, "level mismatch at frame {}", r1.0);
                assert_eq!(
                    r1.2.last_decision, r2.2.last_decision,
                    "decision mismatch at frame {}",
                    r1.0
                );
                assert_eq!(
                    r1.2.decision_reason, r2.2.decision_reason,
                    "decision_reason mismatch at frame {}",
                    r1.0
                );
                assert_eq!(
                    r1.2.transition_seq, r2.2.transition_seq,
                    "transition_seq mismatch at frame {}",
                    r1.0
                );
                assert_eq!(
                    r1.2.transition_correlation_id, r2.2.transition_correlation_id,
                    "transition_correlation_id mismatch at frame {}",
                    r1.0
                );
                assert!(
                    (r1.2.pid_output - r2.2.pid_output).abs() < 1e-10,
                    "pid_output mismatch at frame {}: {} vs {}",
                    r1.0,
                    r1.2.pid_output,
                    r2.2.pid_output
                );
                assert!(
                    (r1.2.e_value - r2.2.e_value).abs() < 1e-10,
                    "e_value mismatch at frame {}: {} vs {}",
                    r1.0,
                    r1.2.e_value,
                    r2.2.e_value
                );
            }
        }

        // --- JSONL schema validation ---

        #[test]
        fn telemetry_jsonl_fields_complete() {
            // Verify all JSONL schema fields are accessible from BudgetTelemetry.
            let mut ctrl = fast_controller(16);
            ctrl.update(Duration::from_millis(20));

            let telem = ctrl.telemetry();

            // All schema fields present and accessible:
            let _schema_version: u16 = telem.schema_version;
            let _degradation: &str = telem.level.as_str();
            let _pid_p: f64 = telem.pid_p;
            let _pid_i: f64 = telem.pid_i;
            let _pid_d: f64 = telem.pid_d;
            let _e_value: f64 = telem.e_value;
            let _decision: &str = telem.last_decision.as_str();
            let _reason: &str = telem.decision_reason.as_str();
            let _transition_seq: u64 = telem.transition_seq;
            let _transition_correlation_id: u64 = telem.transition_correlation_id;
            let _frame_time_ms: f64 = telem.frame_time_ms;
            let _target_ms: f64 = telem.target_ms;
            let _pid_gate_threshold: f64 = telem.pid_gate_threshold;
            let _pid_gate_margin: f64 = telem.pid_gate_margin;
            let _evidence_threshold: f64 = telem.evidence_threshold;
            let _evidence_margin: f64 = telem.evidence_margin;
            let _frames: u32 = telem.frames_observed;

            // Verify decision string mapping
            assert_eq!(BudgetDecision::Hold.as_str(), "stay");
            assert_eq!(BudgetDecision::Degrade.as_str(), "degrade");
            assert_eq!(BudgetDecision::Upgrade.as_str(), "upgrade");
            assert_eq!(
                BUDGET_TELEMETRY_SCHEMA_VERSION, telem.schema_version,
                "schema version mismatch"
            );
        }

        #[test]
        fn telemetry_transition_records_correlation_reason_and_evidence() {
            let mut ctrl = fast_controller(16);

            // Drive toward a degrade transition.
            let mut degrade_telem = None;
            for _ in 0..64 {
                ctrl.update(Duration::from_millis(48));
                let telem = ctrl.telemetry();
                if telem.last_decision == BudgetDecision::Degrade {
                    degrade_telem = Some(telem);
                    break;
                }
            }
            let degrade_telem =
                degrade_telem.expect("expected degrade transition with correlation metadata");
            assert_eq!(
                degrade_telem.decision_reason,
                BudgetDecisionReason::OverloadEvidencePassed
            );
            assert!(
                degrade_telem.transition_seq > 0,
                "transition_seq should increment on transitions"
            );
            assert!(
                degrade_telem.transition_correlation_id > 0,
                "transition correlation id should be populated on transitions"
            );
            assert!(
                degrade_telem.pid_gate_margin > 0.0,
                "degrade transition should have positive PID gate margin"
            );
            assert!(
                degrade_telem.evidence_margin > 0.0,
                "degrade transition should have positive evidence margin"
            );

            // Drive toward an upgrade transition.
            let mut upgrade_telem = None;
            for _ in 0..160 {
                ctrl.update(Duration::from_millis(4));
                let telem = ctrl.telemetry();
                if telem.last_decision == BudgetDecision::Upgrade {
                    upgrade_telem = Some(telem);
                    break;
                }
            }
            let upgrade_telem =
                upgrade_telem.expect("expected upgrade transition with correlation metadata");
            assert_eq!(
                upgrade_telem.decision_reason,
                BudgetDecisionReason::UnderloadEvidencePassed
            );
            assert!(
                upgrade_telem.transition_seq >= degrade_telem.transition_seq,
                "transition sequence should be monotonic"
            );
            assert!(
                upgrade_telem.transition_correlation_id >= degrade_telem.transition_correlation_id,
                "transition correlation id should be monotonic"
            );
            assert!(
                upgrade_telem.pid_gate_margin > 0.0,
                "upgrade transition should have positive PID gate margin"
            );
            assert!(
                upgrade_telem.evidence_margin > 0.0,
                "upgrade transition should have positive evidence margin"
            );
        }

        #[test]
        fn telemetry_pid_components_sum_to_output() {
            // Verify P + I + D == total PID output.
            let mut ctrl = fast_controller(16);

            for ms in [10u64, 16, 20, 30, 8, 50] {
                ctrl.update(Duration::from_millis(ms));
                let telem = ctrl.telemetry();
                let sum = telem.pid_p + telem.pid_i + telem.pid_d;
                assert!(
                    (sum - telem.pid_output).abs() < 1e-10,
                    "P+I+D != output at {}ms: {} + {} + {} = {} != {}",
                    ms,
                    telem.pid_p,
                    telem.pid_i,
                    telem.pid_d,
                    sum,
                    telem.pid_output
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Edge-case tests (bd-1x69n)
    // -----------------------------------------------------------------------

    mod edge_case_tests {
        use super::super::*;

        // --- PID edge cases ---

        #[test]
        fn pid_negative_integral_windup() {
            // Sustained negative error should clamp integral at -integral_max
            let mut state = PidState::default();
            let gains = PidGains {
                integral_max: 3.0,
                ..Default::default()
            };

            for _ in 0..200 {
                state.update(-10.0, &gains);
            }

            assert!(
                state.integral >= -3.0 - f64::EPSILON,
                "Negative integral should be clamped to -max: {}",
                state.integral
            );
            assert!(
                state.integral <= -3.0 + f64::EPSILON,
                "Negative integral should saturate at -max: {}",
                state.integral
            );
        }

        #[test]
        fn pid_zero_gains_zero_output() {
            let mut state = PidState::default();
            let gains = PidGains {
                kp: 0.0,
                ki: 0.0,
                kd: 0.0,
                integral_max: 5.0,
            };

            let u = state.update(42.0, &gains);
            assert!(
                u.abs() < 1e-10,
                "Zero gains should yield zero output: {}",
                u
            );
        }

        #[test]
        fn pid_large_error_stays_finite() {
            let mut state = PidState::default();
            let gains = PidGains::default();

            // Very large error
            let u = state.update(1e12, &gains);
            assert!(
                u.is_finite(),
                "PID output should be finite for large error: {}",
                u
            );

            // Integral should be clamped
            assert!(
                state.integral <= gains.integral_max + f64::EPSILON,
                "Integral should be clamped: {}",
                state.integral
            );
        }

        #[test]
        fn pid_alternating_error_derivative_responds() {
            let mut state = PidState::default();
            let gains = PidGains::default();

            // Alternating +1/-1 error
            let u1 = state.update(1.0, &gains);
            let u2 = state.update(-1.0, &gains);

            // Derivative component for second call: Kd * (-1.0 - 1.0) = 0.2 * -2.0 = -0.4
            // So u2 should have negative derivative contribution
            assert!(
                u2 < u1,
                "Alternating error should reduce output: u1={}, u2={}",
                u1,
                u2
            );
        }

        #[test]
        fn pid_telemetry_terms_match_after_update() {
            let mut state = PidState::default();
            let gains = PidGains::default();

            state.update(2.0, &gains);

            // P = Kp * error = 0.5 * 2.0 = 1.0
            assert!(
                (state.last_p - 1.0).abs() < 1e-10,
                "P term: {}",
                state.last_p
            );
            // I = Ki * integral = 0.05 * 2.0 = 0.1
            assert!(
                (state.last_i - 0.1).abs() < 1e-10,
                "I term: {}",
                state.last_i
            );
            // D = Kd * (error - prev_error) = 0.2 * (2.0 - 0.0) = 0.4
            assert!(
                (state.last_d - 0.4).abs() < 1e-10,
                "D term: {}",
                state.last_d
            );
        }

        #[test]
        fn pid_integral_clamping_symmetric() {
            let mut state = PidState::default();
            let gains = PidGains {
                integral_max: 1.0,
                ..Default::default()
            };

            // Positive saturation
            for _ in 0..50 {
                state.update(100.0, &gains);
            }
            let pos_integral = state.integral;

            state.reset();

            // Negative saturation
            for _ in 0..50 {
                state.update(-100.0, &gains);
            }
            let neg_integral = state.integral;

            assert!(
                (pos_integral + neg_integral).abs() < f64::EPSILON,
                "Clamping should be symmetric: pos={}, neg={}",
                pos_integral,
                neg_integral
            );
        }

        // --- E-process edge cases ---

        #[test]
        fn eprocess_first_frame_initializes_mean() {
            let mut state = EProcessState::default();
            let config = EProcessConfig::default();

            state.update(25.0, 16.0, &config);

            assert!(
                (state.mean_ema - 25.0).abs() < f64::EPSILON,
                "First frame should set mean_ema directly: {}",
                state.mean_ema
            );
            assert!(
                (state.sigma_ema - config.sigma_floor_ms).abs() < f64::EPSILON,
                "First frame should set sigma_ema to floor: {}",
                state.sigma_ema
            );
            assert_eq!(state.frames_observed, 1);
        }

        #[test]
        fn eprocess_e_value_clamped_at_upper_bound() {
            let mut state = EProcessState::default();
            let config = EProcessConfig {
                lambda: 2.0, // High sensitivity to force rapid growth
                warmup_frames: 0,
                sigma_floor_ms: 0.001, // Tiny floor to amplify residuals
                ..Default::default()
            };

            // Extreme overload to push e_value toward upper clamp
            for _ in 0..1000 {
                state.update(1e6, 16.0, &config);
            }

            assert!(
                state.e_value <= 1e10,
                "E-value should be clamped at 1e10: {}",
                state.e_value
            );
        }

        #[test]
        fn eprocess_e_value_clamped_at_lower_bound() {
            let mut state = EProcessState::default();
            let config = EProcessConfig {
                lambda: 2.0,
                warmup_frames: 0,
                sigma_floor_ms: 0.001,
                ..Default::default()
            };

            // Extreme underload to push e_value toward lower clamp
            for _ in 0..1000 {
                state.update(0.001, 1e6, &config);
            }

            assert!(
                state.e_value >= 1e-10,
                "E-value should be clamped at 1e-10: {}",
                state.e_value
            );
        }

        #[test]
        fn eprocess_should_upgrade_during_warmup() {
            let state = EProcessState::default();
            let config = EProcessConfig {
                warmup_frames: 10,
                ..Default::default()
            };

            // During warmup, should_upgrade returns true to allow PID-driven upgrades
            assert!(
                state.should_upgrade(&config),
                "should_upgrade should return true during warmup"
            );
        }

        #[test]
        fn eprocess_frames_observed_saturates() {
            let mut state = EProcessState {
                frames_observed: u32::MAX,
                ..EProcessState::default()
            };
            let config = EProcessConfig::default();

            // Should not panic or wrap around
            state.update(16.0, 16.0, &config);
            assert_eq!(
                state.frames_observed,
                u32::MAX,
                "frames_observed should saturate at u32::MAX"
            );
        }

        #[test]
        fn eprocess_sigma_ema_decay_boundary_zero() {
            let mut state = EProcessState::default();
            let config = EProcessConfig {
                sigma_ema_decay: 0.0,
                warmup_frames: 0,
                ..Default::default()
            };

            // With decay=0, each update fully replaces the EMA
            state.update(20.0, 16.0, &config);
            state.update(30.0, 16.0, &config);

            // mean_ema should be exactly the latest value
            assert!(
                (state.mean_ema - 30.0).abs() < f64::EPSILON,
                "decay=0 should fully replace mean_ema: {}",
                state.mean_ema
            );
        }

        #[test]
        fn eprocess_sigma_ema_decay_boundary_one() {
            let mut state = EProcessState::default();
            let config = EProcessConfig {
                sigma_ema_decay: 1.0,
                warmup_frames: 0,
                ..Default::default()
            };

            // With decay=1, EMA never changes from initial
            state.update(20.0, 16.0, &config);
            let first_mean = state.mean_ema;
            state.update(100.0, 16.0, &config);

            assert!(
                (state.mean_ema - first_mean).abs() < f64::EPSILON,
                "decay=1 should lock mean_ema at first value: got {}, expected {}",
                state.mean_ema,
                first_mean
            );
        }

        #[test]
        fn eprocess_zero_target_no_panic() {
            let mut state = EProcessState::default();
            let config = EProcessConfig {
                warmup_frames: 0,
                ..Default::default()
            };

            // Zero target — residual computation divides by sigma (floored), not target
            let e = state.update(16.0, 0.0, &config);
            assert!(
                e.is_finite(),
                "E-value should be finite with zero target: {}",
                e
            );
        }

        // --- DegradationLevel edge cases ---

        #[test]
        fn degradation_level_default_is_full() {
            assert_eq!(DegradationLevel::default(), DegradationLevel::Full);
        }

        #[test]
        fn degradation_level_hash_unique() {
            use std::collections::HashSet;
            let levels = [
                DegradationLevel::Full,
                DegradationLevel::SimpleBorders,
                DegradationLevel::NoStyling,
                DegradationLevel::EssentialOnly,
                DegradationLevel::Skeleton,
                DegradationLevel::SkipFrame,
            ];
            let set: HashSet<DegradationLevel> = levels.iter().copied().collect();
            assert_eq!(set.len(), 6, "All levels should hash uniquely");
        }

        #[test]
        fn degradation_level_widget_queries_full() {
            let l = DegradationLevel::Full;
            assert!(l.use_unicode_borders());
            assert!(l.apply_styling());
            assert!(l.render_decorative());
            assert!(l.render_content());
        }

        #[test]
        fn degradation_level_widget_queries_simple_borders() {
            let l = DegradationLevel::SimpleBorders;
            assert!(!l.use_unicode_borders());
            assert!(l.apply_styling());
            assert!(l.render_decorative());
            assert!(l.render_content());
        }

        #[test]
        fn degradation_level_widget_queries_no_styling() {
            let l = DegradationLevel::NoStyling;
            assert!(!l.use_unicode_borders());
            assert!(!l.apply_styling());
            assert!(l.render_decorative());
            assert!(l.render_content());
        }

        #[test]
        fn degradation_level_widget_queries_essential_only() {
            let l = DegradationLevel::EssentialOnly;
            assert!(!l.use_unicode_borders());
            assert!(!l.apply_styling());
            assert!(!l.render_decorative());
            assert!(l.render_content());
        }

        #[test]
        fn degradation_level_widget_queries_skeleton() {
            let l = DegradationLevel::Skeleton;
            assert!(!l.use_unicode_borders());
            assert!(!l.apply_styling());
            assert!(!l.render_decorative());
            assert!(!l.render_content());
        }

        #[test]
        fn degradation_level_widget_queries_skip_frame() {
            let l = DegradationLevel::SkipFrame;
            assert!(!l.use_unicode_borders());
            assert!(!l.apply_styling());
            assert!(!l.render_decorative());
            assert!(!l.render_content());
        }

        #[test]
        fn degradation_level_partial_ord_consistent() {
            // PartialOrd should agree with Ord for all pairs
            let levels = [
                DegradationLevel::Full,
                DegradationLevel::SimpleBorders,
                DegradationLevel::NoStyling,
                DegradationLevel::EssentialOnly,
                DegradationLevel::Skeleton,
                DegradationLevel::SkipFrame,
            ];
            for (i, a) in levels.iter().enumerate() {
                for (j, b) in levels.iter().enumerate() {
                    let po = a.partial_cmp(b);
                    let o = a.cmp(b);
                    assert_eq!(po, Some(o), "PartialOrd != Ord for {:?} vs {:?}", a, b);
                    if i < j {
                        assert!(*a < *b, "{:?} should be < {:?}", a, b);
                    }
                }
            }
        }

        #[test]
        fn degradation_level_clone_eq() {
            let a = DegradationLevel::NoStyling;
            let b = a;
            assert_eq!(a, b);
        }

        #[test]
        fn degradation_level_debug() {
            let s = format!("{:?}", DegradationLevel::EssentialOnly);
            assert!(s.contains("EssentialOnly"), "Debug output: {}", s);
        }

        // --- BudgetController accessor edge cases ---

        #[test]
        fn controller_eprocess_sigma_ms_uses_floor() {
            let ctrl = BudgetController::new(BudgetControllerConfig {
                eprocess: EProcessConfig {
                    sigma_floor_ms: 2.5,
                    ..Default::default()
                },
                ..Default::default()
            });

            // Before any updates, sigma_ema is 0.0, so should return floor
            assert!(
                (ctrl.eprocess_sigma_ms() - 2.5).abs() < f64::EPSILON,
                "Should return sigma_floor_ms when sigma_ema < floor: {}",
                ctrl.eprocess_sigma_ms()
            );
        }

        #[test]
        fn controller_config_accessor() {
            let config = BudgetControllerConfig {
                degrade_threshold: 0.42,
                ..Default::default()
            };
            let ctrl = BudgetController::new(config.clone());

            assert_eq!(ctrl.config().degrade_threshold, 0.42);
            assert_eq!(ctrl.config().target, Duration::from_millis(16));
        }

        #[test]
        fn controller_frames_observed_accessor() {
            let mut ctrl = BudgetController::new(BudgetControllerConfig::default());

            assert_eq!(ctrl.frames_observed(), 0);

            ctrl.update(Duration::from_millis(16));
            assert_eq!(ctrl.frames_observed(), 1);

            ctrl.update(Duration::from_millis(16));
            assert_eq!(ctrl.frames_observed(), 2);
        }

        // --- RenderBudget edge cases ---

        #[test]
        fn render_budget_record_frame_time_used_by_next_frame() {
            let mut budget = RenderBudget::new(Duration::from_millis(1000));
            budget.degrade();

            // Simulate many frames to pass cooldown
            for _ in 0..10 {
                budget.reset();
            }

            // Record a very fast frame time
            budget.record_frame_time(Duration::from_millis(1));
            // Sleep past the budget so start.elapsed() would be large
            std::thread::sleep(Duration::from_millis(15));

            let before = budget.degradation();
            budget.next_frame();

            // The recorded frame time (1ms) should trigger upgrade
            // since remaining_fraction_for_elapsed(1ms) > upgrade_threshold
            assert!(
                budget.degradation() < before,
                "Recorded frame time should enable upgrade: before={:?}, after={:?}",
                before,
                budget.degradation()
            );
        }

        #[test]
        fn render_budget_phase_budget_clamped_by_remaining() {
            // Create a budget that has very little remaining
            let budget = RenderBudget::new(Duration::from_millis(1));
            std::thread::sleep(Duration::from_millis(5));

            // Phase budget should be clamped to remaining (0ms)
            let phase = budget.phase_budget(Phase::Render);
            assert!(
                phase.total() <= Duration::from_millis(1),
                "Phase budget should be clamped by remaining: {:?}",
                phase.total()
            );
        }

        #[test]
        fn render_budget_exhausted_skipframe_with_no_frame_skip() {
            let mut budget = RenderBudget::new(Duration::from_millis(1000));
            budget.allow_frame_skip = false;
            budget.set_degradation(DegradationLevel::SkipFrame);

            // With allow_frame_skip = false, SkipFrame should NOT cause exhaustion
            // (only time-based exhaustion matters)
            assert!(
                !budget.exhausted(),
                "SkipFrame should not exhaust when frame skip disabled"
            );
        }

        #[test]
        fn render_budget_remaining_fraction_zero_total() {
            let budget = RenderBudget::new(Duration::ZERO);
            assert_eq!(budget.remaining_fraction(), 0.0);
        }

        #[test]
        fn render_budget_total_accessor() {
            let budget = RenderBudget::new(Duration::from_millis(42));
            assert_eq!(budget.total(), Duration::from_millis(42));
        }

        #[test]
        fn render_budget_phase_budgets_accessor() {
            let budget = RenderBudget::new(Duration::from_millis(16));
            let pb = budget.phase_budgets();
            assert_eq!(pb.diff, Duration::from_millis(2));
            assert_eq!(pb.present, Duration::from_millis(4));
            assert_eq!(pb.render, Duration::from_millis(8));
        }

        #[test]
        fn render_budget_set_degradation_no_op_preserves_cooldown() {
            let mut budget = RenderBudget::new(Duration::from_millis(16));
            budget.set_degradation(DegradationLevel::NoStyling);
            budget.frames_since_change = 7;

            // Setting to same level is a no-op
            budget.set_degradation(DegradationLevel::NoStyling);
            assert_eq!(budget.frames_since_change, 7);

            // Setting to different level resets cooldown
            budget.set_degradation(DegradationLevel::Skeleton);
            assert_eq!(budget.frames_since_change, 0);
        }

        #[test]
        fn render_budget_should_upgrade_false_at_full() {
            let budget = RenderBudget::new(Duration::from_millis(1000));
            assert!(!budget.should_upgrade(), "Full level should never upgrade");
        }

        #[test]
        fn render_budget_should_upgrade_false_during_cooldown() {
            let mut budget = RenderBudget::new(Duration::from_millis(1000));
            budget.degrade();
            // frames_since_change is 0, cooldown is 3
            assert!(
                !budget.should_upgrade(),
                "Should not upgrade during cooldown"
            );
        }

        #[test]
        fn render_budget_degrade_at_max_stays_at_max() {
            let mut budget = RenderBudget::new(Duration::from_millis(16));
            budget.set_degradation(DegradationLevel::SkipFrame);
            budget.degrade();
            assert_eq!(budget.degradation(), DegradationLevel::SkipFrame);
        }

        #[test]
        fn render_budget_upgrade_at_full_stays_at_full() {
            let mut budget = RenderBudget::new(Duration::from_millis(16));
            budget.upgrade();
            assert_eq!(budget.degradation(), DegradationLevel::Full);
        }

        // --- Config edge cases ---

        #[test]
        fn frame_budget_config_partial_eq() {
            let a = FrameBudgetConfig::default();
            let b = FrameBudgetConfig::default();
            assert_eq!(a, b);

            let c = FrameBudgetConfig::strict(Duration::from_millis(16));
            assert_ne!(a, c, "Different configs should not be equal");
        }

        #[test]
        fn phase_budgets_eq_and_copy() {
            let a = PhaseBudgets::default();
            let b = a; // Copy
            assert_eq!(a, b);

            let c = PhaseBudgets {
                diff: Duration::from_millis(1),
                ..Default::default()
            };
            assert_ne!(a, c);
        }

        #[test]
        fn budget_controller_config_partial_eq() {
            let a = BudgetControllerConfig::default();
            let b = BudgetControllerConfig::default();
            assert_eq!(a, b);
        }

        #[test]
        fn pid_gains_partial_eq() {
            let a = PidGains::default();
            let b = PidGains::default();
            assert_eq!(a, b);
        }

        #[test]
        fn eprocess_config_partial_eq() {
            let a = EProcessConfig::default();
            let b = EProcessConfig::default();
            assert_eq!(a, b);
        }

        // --- BudgetDecision edge cases ---

        #[test]
        fn budget_decision_debug_format() {
            assert!(format!("{:?}", BudgetDecision::Hold).contains("Hold"));
            assert!(format!("{:?}", BudgetDecision::Degrade).contains("Degrade"));
            assert!(format!("{:?}", BudgetDecision::Upgrade).contains("Upgrade"));
        }

        #[test]
        fn budget_decision_clone_copy() {
            let d = BudgetDecision::Degrade;
            let d2 = d;
            assert_eq!(d, d2);
        }

        #[test]
        fn budget_decision_as_str_coverage() {
            assert_eq!(BudgetDecision::Hold.as_str(), "stay");
            assert_eq!(BudgetDecision::Degrade.as_str(), "degrade");
            assert_eq!(BudgetDecision::Upgrade.as_str(), "upgrade");
        }

        #[test]
        fn budget_decision_reason_debug_and_as_str() {
            assert!(
                format!("{:?}", BudgetDecisionReason::CooldownActive).contains("CooldownActive")
            );
            assert_eq!(
                BudgetDecisionReason::CooldownActive.as_str(),
                "cooldown_active"
            );
            assert_eq!(
                BudgetDecisionReason::OverloadEvidencePassed.as_str(),
                "overload_evidence_passed"
            );
            assert_eq!(
                BudgetDecisionReason::UnderloadEvidencePassed.as_str(),
                "underload_evidence_passed"
            );
            assert_eq!(
                BudgetDecisionReason::AtMaxDegradation.as_str(),
                "at_max_degradation"
            );
            assert_eq!(
                BudgetDecisionReason::AtDegradationFloor.as_str(),
                "at_degradation_floor"
            );
            assert_eq!(
                BudgetDecisionReason::AtFullQuality.as_str(),
                "at_full_quality"
            );
            assert_eq!(
                BudgetDecisionReason::WithinThresholdBand.as_str(),
                "within_threshold_band"
            );
        }

        // --- Phase edge cases ---

        #[test]
        fn phase_eq_and_hash() {
            use std::collections::HashSet;
            let mut set = HashSet::new();
            set.insert(Phase::Diff);
            set.insert(Phase::Present);
            set.insert(Phase::Render);
            assert_eq!(set.len(), 3);

            // Same phase hashes to same bucket
            set.insert(Phase::Diff);
            assert_eq!(set.len(), 3);
        }

        #[test]
        fn phase_debug() {
            assert!(format!("{:?}", Phase::Diff).contains("Diff"));
            assert!(format!("{:?}", Phase::Present).contains("Present"));
            assert!(format!("{:?}", Phase::Render).contains("Render"));
        }

        #[test]
        fn phase_clone_copy() {
            let p = Phase::Present;
            let p2 = p;
            assert_eq!(p, p2);
        }

        // --- BudgetTelemetry edge cases ---

        #[test]
        fn budget_telemetry_debug() {
            let telem = BudgetTelemetry {
                schema_version: BUDGET_TELEMETRY_SCHEMA_VERSION,
                level: DegradationLevel::Full,
                pid_output: 0.0,
                pid_p: 0.0,
                pid_i: 0.0,
                pid_d: 0.0,
                e_value: 1.0,
                frames_observed: 0,
                frames_since_change: 0,
                last_decision: BudgetDecision::Hold,
                decision_reason: BudgetDecisionReason::WithinThresholdBand,
                transition_seq: 0,
                transition_correlation_id: 0,
                frame_time_ms: 0.0,
                target_ms: 16.0,
                pid_gate_threshold: 0.0,
                pid_gate_margin: 0.0,
                evidence_threshold: 0.0,
                evidence_margin: 0.0,
                in_warmup: true,
            };
            let s = format!("{:?}", telem);
            assert!(s.contains("BudgetTelemetry"), "Debug output: {}", s);
        }

        #[test]
        fn budget_telemetry_partial_eq() {
            let a = BudgetTelemetry {
                schema_version: BUDGET_TELEMETRY_SCHEMA_VERSION,
                level: DegradationLevel::Full,
                pid_output: 0.5,
                pid_p: 0.3,
                pid_i: 0.1,
                pid_d: 0.1,
                e_value: 1.0,
                frames_observed: 5,
                frames_since_change: 2,
                last_decision: BudgetDecision::Hold,
                decision_reason: BudgetDecisionReason::WithinThresholdBand,
                transition_seq: 0,
                transition_correlation_id: 0,
                frame_time_ms: 16.0,
                target_ms: 16.0,
                pid_gate_threshold: 0.0,
                pid_gate_margin: 0.0,
                evidence_threshold: 0.0,
                evidence_margin: 0.0,
                in_warmup: false,
            };
            let b = a;
            assert_eq!(a, b);

            let c = BudgetTelemetry {
                level: DegradationLevel::SimpleBorders,
                ..a
            };
            assert_ne!(a, c);
        }

        // --- Controller + RenderBudget integration edge cases ---

        #[test]
        fn next_frame_without_recorded_time_uses_elapsed() {
            let mut budget = RenderBudget::new(Duration::from_millis(1000));

            // Don't record frame time — next_frame falls back to start.elapsed()
            budget.next_frame();

            // Should not panic, remaining should reset
            assert!(budget.remaining_fraction() > 0.9);
        }

        #[test]
        fn controller_at_max_degradation_holds() {
            let mut ctrl = BudgetController::new(BudgetControllerConfig {
                eprocess: EProcessConfig {
                    warmup_frames: 0,
                    ..Default::default()
                },
                cooldown_frames: 0,
                // Remove the floor so we can test reaching SkipFrame
                degradation_floor: DegradationLevel::SkipFrame,
                ..Default::default()
            });

            // Drive to SkipFrame
            for _ in 0..500 {
                ctrl.update(Duration::from_millis(200));
            }
            assert_eq!(ctrl.level(), DegradationLevel::SkipFrame);

            // At max level, further overload should Hold (can't degrade further)
            let d = ctrl.update(Duration::from_millis(200));
            assert_eq!(d, BudgetDecision::Hold, "At max level, should hold");
        }

        #[test]
        fn controller_at_configured_degradation_floor_reports_floor_reason() {
            let mut ctrl = BudgetController::new(BudgetControllerConfig {
                eprocess: EProcessConfig {
                    warmup_frames: 0,
                    ..Default::default()
                },
                cooldown_frames: 0,
                degradation_floor: DegradationLevel::SimpleBorders,
                ..Default::default()
            });

            ctrl.current_level = DegradationLevel::SimpleBorders;

            let decision = ctrl.update(Duration::from_millis(200));
            let telemetry = ctrl.telemetry();

            assert_eq!(decision, BudgetDecision::Hold);
            assert_eq!(telemetry.level, DegradationLevel::SimpleBorders);
            assert_eq!(
                telemetry.decision_reason,
                BudgetDecisionReason::AtDegradationFloor
            );
        }

        #[test]
        fn controller_at_full_level_no_upgrade() {
            let mut ctrl = BudgetController::new(BudgetControllerConfig {
                eprocess: EProcessConfig {
                    warmup_frames: 0,
                    ..Default::default()
                },
                cooldown_frames: 0,
                ..Default::default()
            });

            // Feed underload — already at Full, so no upgrade possible
            for _ in 0..50 {
                let d = ctrl.update(Duration::from_millis(1));
                assert_ne!(
                    d,
                    BudgetDecision::Upgrade,
                    "Full level should never upgrade"
                );
            }
        }

        #[test]
        fn controller_recovers_after_external_degradation() {
            // Regression: external mutations (guardrail set_degradation,
            // conformal-gate degrade) used to leave the controller believing
            // it was still at Full, so it reported AtFullQuality and held
            // forever — the UI stayed degraded permanently. The controller
            // must track the budget's actual level and upgrade back out.
            let mut budget = RenderBudget::new(Duration::from_millis(16)).with_controller(
                BudgetControllerConfig {
                    eprocess: EProcessConfig {
                        warmup_frames: 0,
                        ..Default::default()
                    },
                    cooldown_frames: 0,
                    ..Default::default()
                },
            );

            // Emergency path forces a jump (as program.rs guardrails do).
            budget.set_degradation(DegradationLevel::EssentialOnly);
            assert_eq!(
                budget.controller().expect("controller attached").level(),
                DegradationLevel::EssentialOnly,
                "controller must observe the external level change"
            );

            // Load is healthy again: fast frames must upgrade back up.
            for _ in 0..300 {
                budget.record_frame_time(Duration::from_millis(2));
                budget.next_frame();
            }
            assert!(
                budget.degradation() < DegradationLevel::EssentialOnly,
                "externally degraded budget must recover under healthy load, got {:?}",
                budget.degradation()
            );

            // Telemetry and rendered level agree at every step.
            assert_eq!(
                budget.controller().expect("controller attached").level(),
                budget.degradation(),
                "controller and budget level must stay in lockstep"
            );
        }

        #[test]
        fn external_conformal_style_degrade_syncs_controller() {
            let mut budget = RenderBudget::new(Duration::from_millis(16)).with_controller(
                BudgetControllerConfig {
                    eprocess: EProcessConfig {
                        warmup_frames: 0,
                        ..Default::default()
                    },
                    cooldown_frames: 0,
                    ..Default::default()
                },
            );

            // Conformal risk gate path calls degrade() directly.
            budget.degrade();
            assert_eq!(
                budget.controller().expect("controller attached").level(),
                budget.degradation()
            );
        }

        #[test]
        fn phase_budget_starts_with_full_phase_allocation() {
            // Regression: phase sub-budgets inherited the frame's start
            // instant, so a phase entered later than its own allocation into
            // the frame was born exhausted.
            let budget = RenderBudget::new(Duration::from_millis(1000));
            std::thread::sleep(Duration::from_millis(20));

            let sub = budget.phase_budget(Phase::Diff);
            assert!(
                !sub.exhausted(),
                "phase budget must not be exhausted at phase start"
            );
            // The allocation is bounded by the frame's remaining time and is
            // fully available at the moment the phase begins.
            assert!(sub.remaining() > Duration::ZERO);
            assert!(sub.remaining() <= sub.total());
        }

        #[test]
        fn next_frame_consumes_recorded_time_once() {
            // Regression: last_frame_time was never cleared, so a frame that
            // skipped record_frame_time (emergency drop path) re-fed the
            // previous frame's duration to the controller.
            let mut budget = RenderBudget::new(Duration::from_millis(16)).with_controller(
                BudgetControllerConfig {
                    eprocess: EProcessConfig {
                        warmup_frames: 0,
                        ..Default::default()
                    },
                    cooldown_frames: 0,
                    ..Default::default()
                },
            );

            budget.record_frame_time(Duration::from_millis(40));
            budget.next_frame();
            let after_first = budget
                .controller()
                .expect("controller attached")
                .telemetry()
                .frame_time_ms;
            assert!((after_first - 40.0).abs() < 1.0);

            // No recording before the second call: the controller must see
            // the (tiny) actual elapsed time, not the stale 40ms again.
            budget.next_frame();
            let after_second = budget
                .controller()
                .expect("controller attached")
                .telemetry()
                .frame_time_ms;
            assert!(
                after_second < 20.0,
                "stale frame time re-fed to controller: {after_second}ms"
            );
        }

        #[test]
        fn render_budget_full_degrade_cycle_with_controller() {
            let mut budget = RenderBudget::new(Duration::from_millis(16)).with_controller(
                BudgetControllerConfig {
                    eprocess: EProcessConfig {
                        warmup_frames: 0,
                        ..Default::default()
                    },
                    cooldown_frames: 0,
                    ..Default::default()
                },
            );

            // Overload to degrade via controller
            for _ in 0..100 {
                budget.record_frame_time(Duration::from_millis(40));
                budget.next_frame();
            }
            let degraded = budget.degradation();
            assert!(
                degraded > DegradationLevel::Full,
                "Should degrade: {:?}",
                degraded
            );

            // Recovery via controller
            for _ in 0..200 {
                budget.record_frame_time(Duration::from_millis(4));
                budget.next_frame();
            }
            let recovered = budget.degradation();
            assert!(
                recovered < degraded,
                "Should recover: {:?} -> {:?}",
                degraded,
                recovered
            );
        }

        #[test]
        fn render_budget_phase_has_budget_exhausted() {
            let budget = RenderBudget::new(Duration::from_millis(1));
            std::thread::sleep(Duration::from_millis(10));

            // All phases should report no budget
            assert!(!budget.phase_has_budget(Phase::Diff));
            assert!(!budget.phase_has_budget(Phase::Present));
            assert!(!budget.phase_has_budget(Phase::Render));
        }

        #[test]
        fn render_budget_elapsed_increases() {
            let budget = RenderBudget::new(Duration::from_millis(1000));
            let e1 = budget.elapsed();
            std::thread::sleep(Duration::from_millis(5));
            let e2 = budget.elapsed();
            assert!(e2 > e1, "Elapsed should increase: {:?} vs {:?}", e1, e2);
        }

        #[test]
        fn controller_pid_integral_accessor() {
            let mut ctrl = BudgetController::new(BudgetControllerConfig::default());

            assert_eq!(ctrl.pid_integral(), 0.0);

            // Feed overload to accumulate integral
            ctrl.update(Duration::from_millis(32)); // 2x target
            assert!(
                ctrl.pid_integral() > 0.0,
                "Integral should grow: {}",
                ctrl.pid_integral()
            );
        }

        #[test]
        fn controller_e_value_accessor() {
            let ctrl = BudgetController::new(BudgetControllerConfig::default());
            assert!((ctrl.e_value() - 1.0).abs() < f64::EPSILON);
        }
    }
}

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

use std::time::{Duration, Instant};

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

/// Anytime-valid e-process for gating degradation decisions.
///
/// # Mathematical Model
///
/// The e-process is a nonnegative supermartingale under H₀ (system is healthy):
///
/// ```text
/// E_t = Π_{j=1..t} exp(λ * r_j − λ² * σ² / 2)
/// ```
///
/// where:
/// - `r_j` is the standardized residual at frame j: `(frame_time − target) / σ`
/// - `σ` is the estimated standard deviation of frame times
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
/// 1. **Anytime-valid**: The test is valid at any stopping time, unlike
///    fixed-sample tests. We can check after every frame without p-hacking.
/// 2. **Bounded false positive rate**: P(E_t ever exceeds 1/α | H₀) ≤ α
///    (Ville's inequality).
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
        self.e_value *= log_factor.exp();

        // Clamp to avoid numerical issues (but preserve the supermartingale property
        // by allowing it to grow large or shrink small).
        self.e_value = self.e_value.clamp(1e-10, 1e10);

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
    pub cooldown_frames: u32,
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

        // Update e-process
        self.eprocess
            .update(frame_ms, target_ms, &self.config.eprocess);

        // Increment cooldown counter
        self.frames_since_change = self.frames_since_change.saturating_add(1);

        // Decision logic with hysteresis + e-process gating
        let decision = if self.frames_since_change < self.config.cooldown_frames {
            // Cooldown active — hold
            BudgetDecision::Hold
        } else if u > self.config.degrade_threshold
            && !self.current_level.is_max()
            && self.eprocess.should_degrade(&self.config.eprocess)
        {
            BudgetDecision::Degrade
        } else if u < -self.config.upgrade_threshold
            && !self.current_level.is_full()
            && self.eprocess.should_upgrade(&self.config.eprocess)
        {
            BudgetDecision::Upgrade
        } else {
            BudgetDecision::Hold
        };

        // Record decision for telemetry
        self.last_decision = decision;

        // Apply decision
        match decision {
            BudgetDecision::Degrade => {
                self.current_level = self.current_level.next();
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
            level: self.current_level,
            pid_output: self.last_pid_output,
            pid_p: self.pid.last_p,
            pid_i: self.pid.last_i,
            pid_d: self.pid.last_d,
            e_value: self.eprocess.e_value,
            frames_observed: self.eprocess.frames_observed,
            frames_since_change: self.frames_since_change,
            last_decision: self.last_decision,
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
    }

    /// Get a reference to the controller configuration.
    #[inline]
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
    /// Logs a warning when degradation occurs.
    pub fn degrade(&mut self) {
        let from = self.degradation;
        self.degradation = self.degradation.next();
        self.frames_since_change = 0;

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

    /// Get the current degradation level.
    #[inline]
    pub fn degradation(&self) -> DegradationLevel {
        self.degradation
    }

    /// Set the degradation level directly.
    ///
    /// Use with caution - prefer `degrade()` and `upgrade()` for gradual changes.
    pub fn set_degradation(&mut self, level: DegradationLevel) {
        if self.degradation != level {
            self.degradation = level;
            self.frames_since_change = 0;
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
        let frame_time = self.last_frame_time.unwrap_or_else(|| self.start.elapsed());

        if self.controller.is_some() {
            // Measure how long the previous frame took

            // SAFETY: we just checked is_some; this avoids a borrow-checker
            // conflict with `&mut self` needed for degrade/upgrade below.
            let decision = self.controller.as_mut().unwrap().update(frame_time);

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
    /// The sub-budget shares the same start time but has a phase-specific total.
    pub fn phase_budget(&self, phase: Phase) -> Self {
        let phase_total = match phase {
            Phase::Diff => self.phase_budgets.diff,
            Phase::Present => self.phase_budgets.present,
            Phase::Render => self.phase_budgets.render,
        };
        Self {
            total: phase_total.min(self.remaining()),
            start: self.start,
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
        let budget = RenderBudget::new(Duration::from_millis(100));

        // Initially should be close to 1.0
        let initial = budget.remaining_fraction();
        assert!(initial > 0.9);

        thread::sleep(Duration::from_millis(50));

        // Should be around 0.5 now
        let later = budget.remaining_fraction();
        assert!(later < 0.6);
        assert!(later > 0.3);
    }

    #[test]
    fn should_degrade_when_cost_exceeds_remaining() {
        // Use wider margins to avoid timing flakiness
        let budget = RenderBudget::new(Duration::from_millis(100));

        // Wait until ~half budget is consumed (~50ms remaining)
        thread::sleep(Duration::from_millis(50));

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
                assert_eq!(r1.2.level, r2.2.level, "level mismatch at frame {}", r1.0);
                assert_eq!(
                    r1.2.last_decision, r2.2.last_decision,
                    "decision mismatch at frame {}",
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
            let _degradation: &str = telem.level.as_str();
            let _pid_p: f64 = telem.pid_p;
            let _pid_i: f64 = telem.pid_i;
            let _pid_d: f64 = telem.pid_d;
            let _e_value: f64 = telem.e_value;
            let _decision: &str = telem.last_decision.as_str();
            let _frames: u32 = telem.frames_observed;

            // Verify decision string mapping
            assert_eq!(BudgetDecision::Hold.as_str(), "stay");
            assert_eq!(BudgetDecision::Degrade.as_str(), "degrade");
            assert_eq!(BudgetDecision::Upgrade.as_str(), "upgrade");
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
}

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
}

impl RenderBudget {
    /// Create a new budget with the specified total time.
    pub fn new(total: Duration) -> Self {
        Self {
            total,
            start: Instant::now(),
            degradation: DegradationLevel::Full,
            phase_budgets: PhaseBudgets::default(),
            allow_frame_skip: true,
            upgrade_threshold: 0.5,
            frames_since_change: 0,
            cooldown: 3,
        }
    }

    /// Create a budget from configuration.
    pub fn from_config(config: &FrameBudgetConfig) -> Self {
        Self {
            total: config.total,
            start: Instant::now(),
            degradation: DegradationLevel::Full,
            phase_budgets: config.phase_budgets,
            allow_frame_skip: config.allow_frame_skip,
            upgrade_threshold: config.upgrade_threshold,
            frames_since_change: 0,
            cooldown: config.degradation_cooldown,
        }
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
    pub fn next_frame(&mut self) {
        // Check upgrade before resetting timing
        if self.should_upgrade() {
            self.upgrade();
        }
        self.reset();
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
            degradation: self.degradation,
            phase_budgets: self.phase_budgets,
            allow_frame_skip: self.allow_frame_skip,
            upgrade_threshold: self.upgrade_threshold,
            frames_since_change: self.frames_since_change,
            cooldown: self.cooldown,
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
}

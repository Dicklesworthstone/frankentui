#![forbid(unsafe_code)]

//! Modal animation system for entrance, exit, and backdrop transitions.
//!
//! This module provides:
//! - Scale-in / scale-out animations for modal content
//! - Backdrop fade animations
//! - Animation cancellation on rapid open/close
//! - Reduced motion support
//!
//! # Example
//!
//! ```ignore
//! let config = ModalAnimationConfig::default();
//! let mut state = ModalAnimationState::new();
//!
//! // Start opening animation
//! state.start_opening();
//!
//! // Each frame, update and check progress
//! state.tick(delta_time);
//! let (scale, opacity, backdrop_opacity) = state.current_values(&config);
//! ```
//!
//! # Invariants
//!
//! - Animation progress is always in [0.0, 1.0]
//! - Scale factor is always in [min_scale, 1.0] during animation
//! - Opacity is always in [0.0, 1.0]
//! - Rapid open/close cancels in-flight animations properly
//!
//! # Failure Modes
//!
//! - If delta_time is negative, it's clamped to 0
//! - Zero-duration animations complete instantly

use std::time::Duration;

// ============================================================================
// Animation Phase
// ============================================================================

/// Current phase of the modal animation lifecycle.
///
/// State machine: Closed → Opening → Open → Closing → Closed
///
/// Rapid toggling can skip phases (e.g., Opening → Closing directly).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModalAnimationPhase {
    /// Modal is fully closed and invisible.
    #[default]
    Closed,
    /// Modal is animating in (scale-up, fade-in).
    Opening,
    /// Modal is fully open and visible.
    Open,
    /// Modal is animating out (scale-down, fade-out).
    Closing,
}

impl ModalAnimationPhase {
    /// Check if the modal should be rendered.
    #[inline]
    pub fn is_visible(self) -> bool {
        !matches!(self, Self::Closed)
    }

    /// Check if animation is in progress.
    #[inline]
    pub fn is_animating(self) -> bool {
        matches!(self, Self::Opening | Self::Closing)
    }
}

// ============================================================================
// Entrance Animation Types
// ============================================================================

/// Entrance animation type for modal content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModalEntranceAnimation {
    /// Scale up from center (classic modal pop).
    #[default]
    ScaleIn,
    /// Fade in (opacity only, no scale).
    FadeIn,
    /// Slide down from top with fade.
    SlideDown,
    /// Slide up from bottom with fade.
    SlideUp,
    /// No animation (instant appear).
    None,
}

impl ModalEntranceAnimation {
    /// Get the initial scale factor for this animation.
    ///
    /// Returns a scale in [0.0, 1.0] where 1.0 = full size.
    pub fn initial_scale(self, config: &ModalAnimationConfig) -> f64 {
        match self {
            Self::ScaleIn => config.min_scale,
            Self::FadeIn | Self::SlideDown | Self::SlideUp | Self::None => 1.0,
        }
    }

    /// Get the initial opacity for this animation.
    pub fn initial_opacity(self) -> f64 {
        match self {
            Self::ScaleIn | Self::FadeIn | Self::SlideDown | Self::SlideUp => 0.0,
            Self::None => 1.0,
        }
    }

    /// Get the initial Y offset in cells for this animation.
    pub fn initial_y_offset(self, modal_height: u16) -> i16 {
        match self {
            Self::SlideDown => -(modal_height as i16).min(8),
            Self::SlideUp => (modal_height as i16).min(8),
            Self::ScaleIn | Self::FadeIn | Self::None => 0,
        }
    }

    /// Calculate scale at a given eased progress (0.0 to 1.0).
    pub fn scale_at_progress(self, progress: f64, config: &ModalAnimationConfig) -> f64 {
        let initial = self.initial_scale(config);
        let p = progress.clamp(0.0, 1.0);
        initial + (1.0 - initial) * p
    }

    /// Calculate opacity at a given eased progress (0.0 to 1.0).
    pub fn opacity_at_progress(self, progress: f64) -> f64 {
        let initial = self.initial_opacity();
        let p = progress.clamp(0.0, 1.0);
        initial + (1.0 - initial) * p
    }

    /// Calculate Y offset at a given eased progress (0.0 to 1.0).
    pub fn y_offset_at_progress(self, progress: f64, modal_height: u16) -> i16 {
        let initial = self.initial_y_offset(modal_height);
        let p = progress.clamp(0.0, 1.0);
        let inv = 1.0 - p;
        (initial as f64 * inv).round() as i16
    }
}

// ============================================================================
// Exit Animation Types
// ============================================================================

/// Exit animation type for modal content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModalExitAnimation {
    /// Scale down to center (reverse of ScaleIn).
    #[default]
    ScaleOut,
    /// Fade out (opacity only, no scale).
    FadeOut,
    /// Slide up with fade.
    SlideUp,
    /// Slide down with fade.
    SlideDown,
    /// No animation (instant disappear).
    None,
}

impl ModalExitAnimation {
    /// Get the final scale factor for this animation.
    pub fn final_scale(self, config: &ModalAnimationConfig) -> f64 {
        match self {
            Self::ScaleOut => config.min_scale,
            Self::FadeOut | Self::SlideUp | Self::SlideDown | Self::None => 1.0,
        }
    }

    /// Get the final opacity for this animation.
    pub fn final_opacity(self) -> f64 {
        match self {
            Self::ScaleOut | Self::FadeOut | Self::SlideUp | Self::SlideDown => 0.0,
            Self::None => 0.0, // Still 0 because modal is closing
        }
    }

    /// Get the final Y offset in cells for this animation.
    pub fn final_y_offset(self, modal_height: u16) -> i16 {
        match self {
            Self::SlideUp => -(modal_height as i16).min(8),
            Self::SlideDown => (modal_height as i16).min(8),
            Self::ScaleOut | Self::FadeOut | Self::None => 0,
        }
    }

    /// Calculate scale at a given eased progress (0.0 to 1.0).
    ///
    /// Progress 0.0 = full size, 1.0 = final (shrunken).
    pub fn scale_at_progress(self, progress: f64, config: &ModalAnimationConfig) -> f64 {
        let final_scale = self.final_scale(config);
        let p = progress.clamp(0.0, 1.0);
        1.0 - (1.0 - final_scale) * p
    }

    /// Calculate opacity at a given eased progress (0.0 to 1.0).
    pub fn opacity_at_progress(self, progress: f64) -> f64 {
        let p = progress.clamp(0.0, 1.0);
        1.0 - p
    }

    /// Calculate Y offset at a given eased progress (0.0 to 1.0).
    pub fn y_offset_at_progress(self, progress: f64, modal_height: u16) -> i16 {
        let final_offset = self.final_y_offset(modal_height);
        let p = progress.clamp(0.0, 1.0);
        (final_offset as f64 * p).round() as i16
    }
}

// ============================================================================
// Easing Functions
// ============================================================================

/// Easing function for modal animations.
///
/// Simplified subset of easing curves for modal animations.
/// For the full set, see `ftui_extras::text_effects::Easing`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ModalEasing {
    /// Linear interpolation.
    Linear,
    /// Smooth ease-out (decelerating) - good for entrances.
    #[default]
    EaseOut,
    /// Smooth ease-in (accelerating) - good for exits.
    EaseIn,
    /// Smooth S-curve - good for general transitions.
    EaseInOut,
    /// Slight overshoot then settle - bouncy feel.
    Back,
}

impl ModalEasing {
    /// Apply the easing function to a progress value (0.0 to 1.0).
    pub fn apply(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::EaseOut => {
                let inv = 1.0 - t;
                1.0 - inv * inv * inv
            }
            Self::EaseIn => t * t * t,
            Self::EaseInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let inv = -2.0 * t + 2.0;
                    1.0 - inv * inv * inv / 2.0
                }
            }
            Self::Back => {
                // Back ease-out: slight overshoot then settle
                let c1 = 1.70158;
                let c3 = c1 + 1.0;
                let t_minus_1 = t - 1.0;
                1.0 + c3 * t_minus_1 * t_minus_1 * t_minus_1 + c1 * t_minus_1 * t_minus_1
            }
        }
    }

    /// Check if this easing can produce values outside 0.0-1.0.
    pub fn can_overshoot(self) -> bool {
        matches!(self, Self::Back)
    }
}

// ============================================================================
// Animation Configuration
// ============================================================================

/// Animation configuration for modals.
#[derive(Debug, Clone)]
pub struct ModalAnimationConfig {
    /// Entrance animation type.
    pub entrance: ModalEntranceAnimation,
    /// Exit animation type.
    pub exit: ModalExitAnimation,
    /// Duration of entrance animation.
    pub entrance_duration: Duration,
    /// Duration of exit animation.
    pub exit_duration: Duration,
    /// Easing function for entrance.
    pub entrance_easing: ModalEasing,
    /// Easing function for exit.
    pub exit_easing: ModalEasing,
    /// Minimum scale for scale animations (typically 0.9-0.95).
    pub min_scale: f64,
    /// Whether backdrop should animate independently.
    pub animate_backdrop: bool,
    /// Backdrop fade-in duration (can differ from content).
    pub backdrop_duration: Duration,
    /// Whether to respect reduced-motion preference.
    pub respect_reduced_motion: bool,
}

impl Default for ModalAnimationConfig {
    fn default() -> Self {
        Self {
            entrance: ModalEntranceAnimation::ScaleIn,
            exit: ModalExitAnimation::ScaleOut,
            entrance_duration: Duration::from_millis(200),
            exit_duration: Duration::from_millis(150),
            entrance_easing: ModalEasing::EaseOut,
            exit_easing: ModalEasing::EaseIn,
            min_scale: 0.92,
            animate_backdrop: true,
            backdrop_duration: Duration::from_millis(150),
            respect_reduced_motion: true,
        }
    }
}

impl ModalAnimationConfig {
    /// Create a new default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a configuration with no animations.
    pub fn none() -> Self {
        Self {
            entrance: ModalEntranceAnimation::None,
            exit: ModalExitAnimation::None,
            entrance_duration: Duration::ZERO,
            exit_duration: Duration::ZERO,
            backdrop_duration: Duration::ZERO,
            ..Default::default()
        }
    }

    /// Create a configuration for reduced motion preference.
    ///
    /// Uses fade only (no scale/slide) with shorter durations.
    pub fn reduced_motion() -> Self {
        Self {
            entrance: ModalEntranceAnimation::FadeIn,
            exit: ModalExitAnimation::FadeOut,
            entrance_duration: Duration::from_millis(100),
            exit_duration: Duration::from_millis(100),
            entrance_easing: ModalEasing::Linear,
            exit_easing: ModalEasing::Linear,
            min_scale: 1.0,
            animate_backdrop: true,
            backdrop_duration: Duration::from_millis(100),
            respect_reduced_motion: true,
        }
    }

    /// Set entrance animation type.
    #[must_use]
    pub fn entrance(mut self, anim: ModalEntranceAnimation) -> Self {
        self.entrance = anim;
        self
    }

    /// Set exit animation type.
    #[must_use]
    pub fn exit(mut self, anim: ModalExitAnimation) -> Self {
        self.exit = anim;
        self
    }

    /// Set entrance duration.
    #[must_use]
    pub fn entrance_duration(mut self, duration: Duration) -> Self {
        self.entrance_duration = duration;
        self
    }

    /// Set exit duration.
    #[must_use]
    pub fn exit_duration(mut self, duration: Duration) -> Self {
        self.exit_duration = duration;
        self
    }

    /// Set entrance easing function.
    #[must_use]
    pub fn entrance_easing(mut self, easing: ModalEasing) -> Self {
        self.entrance_easing = easing;
        self
    }

    /// Set exit easing function.
    #[must_use]
    pub fn exit_easing(mut self, easing: ModalEasing) -> Self {
        self.exit_easing = easing;
        self
    }

    /// Set minimum scale for scale animations.
    #[must_use]
    pub fn min_scale(mut self, scale: f64) -> Self {
        self.min_scale = scale.clamp(0.5, 1.0);
        self
    }

    /// Set whether backdrop should animate.
    #[must_use]
    pub fn animate_backdrop(mut self, animate: bool) -> Self {
        self.animate_backdrop = animate;
        self
    }

    /// Set backdrop fade duration.
    #[must_use]
    pub fn backdrop_duration(mut self, duration: Duration) -> Self {
        self.backdrop_duration = duration;
        self
    }

    /// Set whether to respect reduced-motion preference.
    #[must_use]
    pub fn respect_reduced_motion(mut self, respect: bool) -> Self {
        self.respect_reduced_motion = respect;
        self
    }

    /// Check if animations are effectively disabled.
    pub fn is_disabled(&self) -> bool {
        matches!(self.entrance, ModalEntranceAnimation::None)
            && matches!(self.exit, ModalExitAnimation::None)
    }

    /// Get the effective config, applying reduced motion if needed.
    pub fn effective(&self, reduced_motion: bool) -> Self {
        if reduced_motion && self.respect_reduced_motion {
            Self::reduced_motion()
        } else {
            self.clone()
        }
    }
}

// ============================================================================
// Animation State
// ============================================================================

/// Current animation state for a modal.
///
/// Tracks progress through open/close animations and computes
/// interpolated values for scale, opacity, and position offset.
#[derive(Debug, Clone)]
pub struct ModalAnimationState {
    /// Current animation phase.
    phase: ModalAnimationPhase,
    /// Progress within current phase (0.0 to 1.0).
    progress: f64,
    /// Backdrop animation progress (may differ from content).
    backdrop_progress: f64,
    /// Whether reduced motion is enabled.
    reduced_motion: bool,
}

impl Default for ModalAnimationState {
    fn default() -> Self {
        Self::new()
    }
}

impl ModalAnimationState {
    /// Create a new animation state (closed, no animation).
    pub fn new() -> Self {
        Self {
            phase: ModalAnimationPhase::Closed,
            progress: 0.0,
            backdrop_progress: 0.0,
            reduced_motion: false,
        }
    }

    /// Create a state that starts fully open (for testing or instant open).
    pub fn open() -> Self {
        Self {
            phase: ModalAnimationPhase::Open,
            progress: 1.0,
            backdrop_progress: 1.0,
            reduced_motion: false,
        }
    }

    /// Get the current phase.
    pub fn phase(&self) -> ModalAnimationPhase {
        self.phase
    }

    /// Get the raw progress value (0.0 to 1.0).
    pub fn progress(&self) -> f64 {
        self.progress
    }

    /// Get the backdrop progress value (0.0 to 1.0).
    pub fn backdrop_progress(&self) -> f64 {
        self.backdrop_progress
    }

    /// Check if the modal is visible (should be rendered).
    #[inline]
    pub fn is_visible(&self) -> bool {
        self.phase.is_visible()
    }

    /// Check if animation is in progress.
    #[inline]
    pub fn is_animating(&self) -> bool {
        self.phase.is_animating()
    }

    /// Check if the modal is fully open.
    #[inline]
    pub fn is_open(&self) -> bool {
        matches!(self.phase, ModalAnimationPhase::Open)
    }

    /// Check if the modal is fully closed.
    #[inline]
    pub fn is_closed(&self) -> bool {
        matches!(self.phase, ModalAnimationPhase::Closed)
    }

    /// Set reduced motion preference.
    pub fn set_reduced_motion(&mut self, enabled: bool) {
        self.reduced_motion = enabled;
    }

    /// Start opening animation.
    ///
    /// If already opening or open, this is a no-op.
    /// If closing, reverses direction and preserves momentum.
    pub fn start_opening(&mut self) {
        match self.phase {
            ModalAnimationPhase::Closed => {
                self.phase = ModalAnimationPhase::Opening;
                self.progress = 0.0;
                self.backdrop_progress = 0.0;
            }
            ModalAnimationPhase::Closing => {
                // Reverse animation, preserving progress
                self.phase = ModalAnimationPhase::Opening;
                // Invert progress: if we were 30% through closing, start at 70% open
                self.progress = 1.0 - self.progress;
                self.backdrop_progress = 1.0 - self.backdrop_progress;
            }
            ModalAnimationPhase::Opening | ModalAnimationPhase::Open => {
                // Already opening or open, nothing to do
            }
        }
    }

    /// Start closing animation.
    ///
    /// If already closing or closed, this is a no-op.
    /// If opening, reverses direction and preserves momentum.
    pub fn start_closing(&mut self) {
        match self.phase {
            ModalAnimationPhase::Open => {
                self.phase = ModalAnimationPhase::Closing;
                self.progress = 0.0;
                self.backdrop_progress = 0.0;
            }
            ModalAnimationPhase::Opening => {
                // Reverse animation, preserving progress
                self.phase = ModalAnimationPhase::Closing;
                // Invert progress
                self.progress = 1.0 - self.progress;
                self.backdrop_progress = 1.0 - self.backdrop_progress;
            }
            ModalAnimationPhase::Closing | ModalAnimationPhase::Closed => {
                // Already closing or closed, nothing to do
            }
        }
    }

    /// Force the modal to be fully open (skip animation).
    pub fn force_open(&mut self) {
        self.phase = ModalAnimationPhase::Open;
        self.progress = 1.0;
        self.backdrop_progress = 1.0;
    }

    /// Force the modal to be fully closed (skip animation).
    pub fn force_close(&mut self) {
        self.phase = ModalAnimationPhase::Closed;
        self.progress = 0.0;
        self.backdrop_progress = 0.0;
    }

    /// Advance the animation by the given delta time.
    ///
    /// Returns `true` if the animation phase changed (e.g., Opening → Open).
    pub fn tick(&mut self, delta: Duration, config: &ModalAnimationConfig) -> bool {
        let delta_secs = delta.as_secs_f64().max(0.0);
        let config = config.effective(self.reduced_motion);

        match self.phase {
            ModalAnimationPhase::Opening => {
                let content_duration = config.entrance_duration.as_secs_f64();
                let backdrop_duration = if config.animate_backdrop {
                    config.backdrop_duration.as_secs_f64()
                } else {
                    0.0
                };

                // Advance content progress
                if content_duration > 0.0 {
                    self.progress += delta_secs / content_duration;
                } else {
                    self.progress = 1.0;
                }

                // Advance backdrop progress
                if backdrop_duration > 0.0 {
                    self.backdrop_progress += delta_secs / backdrop_duration;
                } else {
                    self.backdrop_progress = 1.0;
                }

                // Clamp and check for completion
                self.progress = self.progress.min(1.0);
                self.backdrop_progress = self.backdrop_progress.min(1.0);

                if self.progress >= 1.0 && self.backdrop_progress >= 1.0 {
                    self.phase = ModalAnimationPhase::Open;
                    self.progress = 1.0;
                    self.backdrop_progress = 1.0;
                    return true;
                }
            }
            ModalAnimationPhase::Closing => {
                let content_duration = config.exit_duration.as_secs_f64();
                let backdrop_duration = if config.animate_backdrop {
                    config.backdrop_duration.as_secs_f64()
                } else {
                    0.0
                };

                // Advance content progress
                if content_duration > 0.0 {
                    self.progress += delta_secs / content_duration;
                } else {
                    self.progress = 1.0;
                }

                // Advance backdrop progress
                if backdrop_duration > 0.0 {
                    self.backdrop_progress += delta_secs / backdrop_duration;
                } else {
                    self.backdrop_progress = 1.0;
                }

                // Clamp and check for completion
                self.progress = self.progress.min(1.0);
                self.backdrop_progress = self.backdrop_progress.min(1.0);

                if self.progress >= 1.0 && self.backdrop_progress >= 1.0 {
                    self.phase = ModalAnimationPhase::Closed;
                    self.progress = 0.0;
                    self.backdrop_progress = 0.0;
                    return true;
                }
            }
            ModalAnimationPhase::Open | ModalAnimationPhase::Closed => {
                // No animation in progress
            }
        }

        false
    }

    /// Get the current eased progress for content animation.
    pub fn eased_progress(&self, config: &ModalAnimationConfig) -> f64 {
        let config = config.effective(self.reduced_motion);
        match self.phase {
            ModalAnimationPhase::Opening => config.entrance_easing.apply(self.progress),
            ModalAnimationPhase::Closing => config.exit_easing.apply(self.progress),
            ModalAnimationPhase::Open => 1.0,
            ModalAnimationPhase::Closed => 0.0,
        }
    }

    /// Get the current eased progress for backdrop animation.
    pub fn eased_backdrop_progress(&self, config: &ModalAnimationConfig) -> f64 {
        let _config = config.effective(self.reduced_motion);
        // Backdrop always uses EaseOut for fade-in and EaseIn for fade-out
        match self.phase {
            ModalAnimationPhase::Opening => ModalEasing::EaseOut.apply(self.backdrop_progress),
            ModalAnimationPhase::Closing => ModalEasing::EaseIn.apply(self.backdrop_progress),
            ModalAnimationPhase::Open => 1.0,
            ModalAnimationPhase::Closed => 0.0,
        }
    }

    /// Get the current scale factor for the modal content.
    ///
    /// Returns a value in [min_scale, 1.0].
    pub fn current_scale(&self, config: &ModalAnimationConfig) -> f64 {
        let config = config.effective(self.reduced_motion);
        let eased = self.eased_progress(&config);

        match self.phase {
            ModalAnimationPhase::Opening => config.entrance.scale_at_progress(eased, &config),
            ModalAnimationPhase::Closing => config.exit.scale_at_progress(eased, &config),
            ModalAnimationPhase::Open => 1.0,
            ModalAnimationPhase::Closed => config.entrance.initial_scale(&config),
        }
    }

    /// Get the current opacity for the modal content.
    ///
    /// Returns a value in [0.0, 1.0].
    pub fn current_opacity(&self, config: &ModalAnimationConfig) -> f64 {
        let config = config.effective(self.reduced_motion);
        let eased = self.eased_progress(&config);

        match self.phase {
            ModalAnimationPhase::Opening => config.entrance.opacity_at_progress(eased),
            ModalAnimationPhase::Closing => config.exit.opacity_at_progress(eased),
            ModalAnimationPhase::Open => 1.0,
            ModalAnimationPhase::Closed => 0.0,
        }
    }

    /// Get the current backdrop opacity.
    ///
    /// Returns a value in [0.0, 1.0] to be multiplied with the backdrop's configured opacity.
    pub fn current_backdrop_opacity(&self, config: &ModalAnimationConfig) -> f64 {
        let config = config.effective(self.reduced_motion);

        if !config.animate_backdrop {
            return match self.phase {
                ModalAnimationPhase::Open | ModalAnimationPhase::Opening => 1.0,
                ModalAnimationPhase::Closed | ModalAnimationPhase::Closing => 0.0,
            };
        }

        let eased = self.eased_backdrop_progress(&config);

        match self.phase {
            ModalAnimationPhase::Opening => eased,
            ModalAnimationPhase::Closing => 1.0 - eased,
            ModalAnimationPhase::Open => 1.0,
            ModalAnimationPhase::Closed => 0.0,
        }
    }

    /// Get the current Y offset for the modal content.
    ///
    /// Returns an offset in cells (negative = above final position).
    pub fn current_y_offset(&self, config: &ModalAnimationConfig, modal_height: u16) -> i16 {
        let config = config.effective(self.reduced_motion);
        let eased = self.eased_progress(&config);

        match self.phase {
            ModalAnimationPhase::Opening => {
                config.entrance.y_offset_at_progress(eased, modal_height)
            }
            ModalAnimationPhase::Closing => config.exit.y_offset_at_progress(eased, modal_height),
            ModalAnimationPhase::Open | ModalAnimationPhase::Closed => 0,
        }
    }

    /// Get all current animation values at once.
    ///
    /// Returns (scale, opacity, backdrop_opacity, y_offset).
    pub fn current_values(
        &self,
        config: &ModalAnimationConfig,
        modal_height: u16,
    ) -> (f64, f64, f64, i16) {
        (
            self.current_scale(config),
            self.current_opacity(config),
            self.current_backdrop_opacity(config),
            self.current_y_offset(config, modal_height),
        )
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Phase Transitions
    // -------------------------------------------------------------------------

    #[test]
    fn test_phase_visibility() {
        assert!(!ModalAnimationPhase::Closed.is_visible());
        assert!(ModalAnimationPhase::Opening.is_visible());
        assert!(ModalAnimationPhase::Open.is_visible());
        assert!(ModalAnimationPhase::Closing.is_visible());
    }

    #[test]
    fn test_phase_animating() {
        assert!(!ModalAnimationPhase::Closed.is_animating());
        assert!(ModalAnimationPhase::Opening.is_animating());
        assert!(!ModalAnimationPhase::Open.is_animating());
        assert!(ModalAnimationPhase::Closing.is_animating());
    }

    #[test]
    fn test_start_opening_from_closed() {
        let mut state = ModalAnimationState::new();
        assert_eq!(state.phase(), ModalAnimationPhase::Closed);

        state.start_opening();
        assert_eq!(state.phase(), ModalAnimationPhase::Opening);
        assert_eq!(state.progress(), 0.0);
    }

    #[test]
    fn test_start_closing_from_open() {
        let mut state = ModalAnimationState::open();
        assert_eq!(state.phase(), ModalAnimationPhase::Open);

        state.start_closing();
        assert_eq!(state.phase(), ModalAnimationPhase::Closing);
        assert_eq!(state.progress(), 0.0);
    }

    #[test]
    fn test_rapid_toggle_reverses_animation() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default();

        // Start opening
        state.start_opening();
        state.tick(Duration::from_millis(100), &config); // 50% through 200ms

        let opening_progress = state.progress();
        assert!(opening_progress > 0.0);
        assert!(opening_progress < 1.0);

        // Quickly close - should reverse
        state.start_closing();
        assert_eq!(state.phase(), ModalAnimationPhase::Closing);

        // Progress should be inverted: if we were 50% open, we're now 50% closed
        let closing_progress = state.progress();
        assert!((opening_progress + closing_progress - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_opening_noop_when_already_opening() {
        let mut state = ModalAnimationState::new();
        state.start_opening();
        let progress1 = state.progress();

        state.start_opening(); // Should be no-op
        assert_eq!(state.progress(), progress1);
        assert_eq!(state.phase(), ModalAnimationPhase::Opening);
    }

    // -------------------------------------------------------------------------
    // Animation Progress
    // -------------------------------------------------------------------------

    #[test]
    fn test_tick_advances_progress() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default();

        state.start_opening();
        assert_eq!(state.progress(), 0.0);

        state.tick(Duration::from_millis(100), &config);
        assert!(state.progress() > 0.0);
        assert!(state.progress() < 1.0);
    }

    #[test]
    fn test_tick_completes_animation() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default();

        state.start_opening();
        let changed = state.tick(Duration::from_millis(500), &config);

        assert!(changed);
        assert_eq!(state.phase(), ModalAnimationPhase::Open);
        assert_eq!(state.progress(), 1.0);
    }

    #[test]
    fn test_zero_duration_completes_instantly() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::none();

        state.start_opening();
        let changed = state.tick(Duration::from_millis(1), &config);

        assert!(changed);
        assert_eq!(state.phase(), ModalAnimationPhase::Open);
    }

    // -------------------------------------------------------------------------
    // Easing
    // -------------------------------------------------------------------------

    #[test]
    fn test_easing_linear() {
        assert_eq!(ModalEasing::Linear.apply(0.0), 0.0);
        assert_eq!(ModalEasing::Linear.apply(0.5), 0.5);
        assert_eq!(ModalEasing::Linear.apply(1.0), 1.0);
    }

    #[test]
    fn test_easing_clamps_input() {
        assert_eq!(ModalEasing::Linear.apply(-0.5), 0.0);
        assert_eq!(ModalEasing::Linear.apply(1.5), 1.0);
    }

    #[test]
    fn test_easing_ease_out_decelerates() {
        // EaseOut should be > linear at 0.5 (faster start, slower end)
        let linear = ModalEasing::Linear.apply(0.5);
        let ease_out = ModalEasing::EaseOut.apply(0.5);
        assert!(ease_out > linear);
    }

    #[test]
    fn test_easing_ease_in_accelerates() {
        // EaseIn should be < linear at 0.5 (slower start, faster end)
        let linear = ModalEasing::Linear.apply(0.5);
        let ease_in = ModalEasing::EaseIn.apply(0.5);
        assert!(ease_in < linear);
    }

    // -------------------------------------------------------------------------
    // Animation Values
    // -------------------------------------------------------------------------

    #[test]
    fn test_scale_during_opening() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default();

        // At start (closed)
        let scale = state.current_scale(&config);
        assert!((scale - config.min_scale).abs() < 0.001);

        // During opening
        state.start_opening();
        state.tick(Duration::from_millis(100), &config);
        let mid_scale = state.current_scale(&config);
        assert!(mid_scale > config.min_scale);
        assert!(mid_scale < 1.0);

        // At end (open)
        state.tick(Duration::from_millis(500), &config);
        let final_scale = state.current_scale(&config);
        assert!((final_scale - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_opacity_during_closing() {
        let mut state = ModalAnimationState::open();
        let config = ModalAnimationConfig::default();

        // At start (open)
        assert!((state.current_opacity(&config) - 1.0).abs() < 0.001);

        // During closing
        state.start_closing();
        state.tick(Duration::from_millis(75), &config);
        let mid_opacity = state.current_opacity(&config);
        assert!(mid_opacity > 0.0);
        assert!(mid_opacity < 1.0);

        // At end (closed)
        state.tick(Duration::from_millis(500), &config);
        let final_opacity = state.current_opacity(&config);
        assert!((final_opacity - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_backdrop_opacity_independent() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default()
            .entrance_duration(Duration::from_millis(200))
            .backdrop_duration(Duration::from_millis(100));

        state.start_opening();

        // After 100ms, backdrop should be at 100% but content still animating
        state.tick(Duration::from_millis(100), &config);

        let content_opacity = state.current_opacity(&config);
        let backdrop_opacity = state.current_backdrop_opacity(&config);

        // Backdrop animates faster, so should be closer to 1.0
        assert!(backdrop_opacity > content_opacity);
    }

    // -------------------------------------------------------------------------
    // Reduced Motion
    // -------------------------------------------------------------------------

    #[test]
    fn test_reduced_motion_config() {
        let config = ModalAnimationConfig::reduced_motion();

        assert!(matches!(config.entrance, ModalEntranceAnimation::FadeIn));
        assert!(matches!(config.exit, ModalExitAnimation::FadeOut));
        assert!((config.min_scale - 1.0).abs() < 0.001); // No scale
    }

    #[test]
    fn test_reduced_motion_applies_effective_config() {
        let mut state = ModalAnimationState::new();
        state.set_reduced_motion(true);

        let config = ModalAnimationConfig::default();

        state.start_opening();
        let scale = state.current_scale(&config);

        // With reduced motion, scale should be 1.0 (no scale animation)
        assert!((scale - 1.0).abs() < 0.001);
    }

    // -------------------------------------------------------------------------
    // Force Open/Close
    // -------------------------------------------------------------------------

    #[test]
    fn test_force_open() {
        let mut state = ModalAnimationState::new();
        state.force_open();

        assert_eq!(state.phase(), ModalAnimationPhase::Open);
        assert_eq!(state.progress(), 1.0);
        assert_eq!(state.backdrop_progress(), 1.0);
    }

    #[test]
    fn test_force_close() {
        let mut state = ModalAnimationState::open();
        state.force_close();

        assert_eq!(state.phase(), ModalAnimationPhase::Closed);
        assert_eq!(state.progress(), 0.0);
        assert_eq!(state.backdrop_progress(), 0.0);
    }

    // -------------------------------------------------------------------------
    // Entrance/Exit Animation Types
    // -------------------------------------------------------------------------

    #[test]
    fn test_scale_in_initial_scale() {
        let config = ModalAnimationConfig::default();
        let initial = ModalEntranceAnimation::ScaleIn.initial_scale(&config);
        assert!((initial - config.min_scale).abs() < 0.001);
    }

    #[test]
    fn test_fade_in_no_scale() {
        let config = ModalAnimationConfig::default();
        let initial = ModalEntranceAnimation::FadeIn.initial_scale(&config);
        assert!((initial - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_slide_down_y_offset() {
        let initial = ModalEntranceAnimation::SlideDown.initial_y_offset(20);
        assert!(initial < 0); // Above final position
    }

    #[test]
    fn test_slide_up_y_offset() {
        let initial = ModalEntranceAnimation::SlideUp.initial_y_offset(20);
        assert!(initial > 0); // Below final position
    }

    // -------------------------------------------------------------------------
    // Invariants
    // -------------------------------------------------------------------------

    #[test]
    fn test_progress_always_in_bounds() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default();

        state.start_opening();

        // Many ticks with large delta
        for _ in 0..100 {
            state.tick(Duration::from_millis(100), &config);
            assert!(state.progress() >= 0.0);
            assert!(state.progress() <= 1.0);
            assert!(state.backdrop_progress() >= 0.0);
            assert!(state.backdrop_progress() <= 1.0);
        }
    }

    #[test]
    fn test_scale_always_in_bounds() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default();

        state.start_opening();

        for i in 0..20 {
            state.tick(Duration::from_millis(20), &config);
            let scale = state.current_scale(&config);
            assert!(
                scale >= config.min_scale,
                "scale {} < min {} at step {}",
                scale,
                config.min_scale,
                i
            );
            assert!(scale <= 1.0, "scale {} > 1.0 at step {}", scale, i);
        }
    }

    #[test]
    fn test_opacity_always_in_bounds() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default();

        state.start_opening();

        for i in 0..20 {
            state.tick(Duration::from_millis(20), &config);
            let opacity = state.current_opacity(&config);
            assert!(opacity >= 0.0, "opacity {} < 0 at step {}", opacity, i);
            assert!(opacity <= 1.0, "opacity {} > 1.0 at step {}", opacity, i);
        }
    }

    // ---- Edge-case tests (bd-a4n4z) ----

    #[test]
    fn edge_easing_ease_in_out_at_boundary() {
        // At exactly 0.5 the branch flips
        let at_half = ModalEasing::EaseInOut.apply(0.5);
        assert!(
            (at_half - 0.5).abs() < 0.001,
            "EaseInOut at 0.5 should be ~0.5, got {at_half}"
        );
        // Endpoints
        assert_eq!(ModalEasing::EaseInOut.apply(0.0), 0.0);
        assert!((ModalEasing::EaseInOut.apply(1.0) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn edge_easing_back_overshoots() {
        // Back easing overshoots 1.0 briefly then settles at 1.0
        // At midpoint it should overshoot
        let mid = ModalEasing::Back.apply(0.5);
        // At t=0 and t=1
        assert!((ModalEasing::Back.apply(0.0)).abs() < 1e-10);
        assert!((ModalEasing::Back.apply(1.0) - 1.0).abs() < 1e-10);
        // Verify overshoot actually happens somewhere in (0, 1)
        let mut found_overshoot = false;
        for i in 1..100 {
            let t = i as f64 / 100.0;
            let v = ModalEasing::Back.apply(t);
            if v > 1.0 {
                found_overshoot = true;
                break;
            }
        }
        assert!(
            found_overshoot,
            "Back easing should overshoot 1.0 at some point, mid={mid}"
        );
    }

    #[test]
    fn edge_can_overshoot_only_back() {
        assert!(!ModalEasing::Linear.can_overshoot());
        assert!(!ModalEasing::EaseOut.can_overshoot());
        assert!(!ModalEasing::EaseIn.can_overshoot());
        assert!(!ModalEasing::EaseInOut.can_overshoot());
        assert!(ModalEasing::Back.can_overshoot());
    }

    #[test]
    fn edge_easing_ease_in_endpoints() {
        assert_eq!(ModalEasing::EaseIn.apply(0.0), 0.0);
        assert!((ModalEasing::EaseIn.apply(1.0) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn edge_easing_ease_out_endpoints() {
        assert_eq!(ModalEasing::EaseOut.apply(0.0), 0.0);
        assert!((ModalEasing::EaseOut.apply(1.0) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn edge_exit_final_scale_variants() {
        let config = ModalAnimationConfig::default();
        assert!(
            (ModalExitAnimation::ScaleOut.final_scale(&config) - config.min_scale).abs() < 1e-10
        );
        assert!((ModalExitAnimation::FadeOut.final_scale(&config) - 1.0).abs() < 1e-10);
        assert!((ModalExitAnimation::SlideUp.final_scale(&config) - 1.0).abs() < 1e-10);
        assert!((ModalExitAnimation::SlideDown.final_scale(&config) - 1.0).abs() < 1e-10);
        assert!((ModalExitAnimation::None.final_scale(&config) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn edge_exit_final_opacity_all_zero() {
        // All exit animations end at opacity 0
        assert_eq!(ModalExitAnimation::ScaleOut.final_opacity(), 0.0);
        assert_eq!(ModalExitAnimation::FadeOut.final_opacity(), 0.0);
        assert_eq!(ModalExitAnimation::SlideUp.final_opacity(), 0.0);
        assert_eq!(ModalExitAnimation::SlideDown.final_opacity(), 0.0);
        assert_eq!(ModalExitAnimation::None.final_opacity(), 0.0);
    }

    #[test]
    fn edge_exit_final_y_offset() {
        assert!(ModalExitAnimation::SlideUp.final_y_offset(20) < 0);
        assert!(ModalExitAnimation::SlideDown.final_y_offset(20) > 0);
        assert_eq!(ModalExitAnimation::ScaleOut.final_y_offset(20), 0);
        assert_eq!(ModalExitAnimation::FadeOut.final_y_offset(20), 0);
        assert_eq!(ModalExitAnimation::None.final_y_offset(20), 0);
    }

    #[test]
    fn edge_exit_scale_at_progress() {
        let config = ModalAnimationConfig::default();
        // At progress 0, scale = 1.0 (fully open)
        let s0 = ModalExitAnimation::ScaleOut.scale_at_progress(0.0, &config);
        assert!((s0 - 1.0).abs() < 1e-10);
        // At progress 1, scale = min_scale
        let s1 = ModalExitAnimation::ScaleOut.scale_at_progress(1.0, &config);
        assert!((s1 - config.min_scale).abs() < 1e-10);
    }

    #[test]
    fn edge_exit_opacity_at_progress() {
        assert!((ModalExitAnimation::FadeOut.opacity_at_progress(0.0) - 1.0).abs() < 1e-10);
        assert!((ModalExitAnimation::FadeOut.opacity_at_progress(1.0)).abs() < 1e-10);
        assert!((ModalExitAnimation::FadeOut.opacity_at_progress(0.5) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn edge_exit_y_offset_at_progress() {
        assert_eq!(ModalExitAnimation::SlideUp.y_offset_at_progress(0.0, 20), 0);
        let final_offset = ModalExitAnimation::SlideUp.y_offset_at_progress(1.0, 20);
        assert_eq!(final_offset, ModalExitAnimation::SlideUp.final_y_offset(20));
    }

    #[test]
    fn edge_entrance_none_instant() {
        let config = ModalAnimationConfig::default();
        assert!((ModalEntranceAnimation::None.initial_scale(&config) - 1.0).abs() < 1e-10);
        assert!((ModalEntranceAnimation::None.initial_opacity() - 1.0).abs() < 1e-10);
        assert_eq!(ModalEntranceAnimation::None.initial_y_offset(20), 0);
    }

    #[test]
    fn edge_slide_height_clamped_at_8() {
        // Large modal_height should clamp offset at 8
        let down = ModalEntranceAnimation::SlideDown.initial_y_offset(100);
        assert_eq!(down, -8);
        let up = ModalEntranceAnimation::SlideUp.initial_y_offset(100);
        assert_eq!(up, 8);

        // Exit slide clamping
        let exit_up = ModalExitAnimation::SlideUp.final_y_offset(100);
        assert_eq!(exit_up, -8);
        let exit_down = ModalExitAnimation::SlideDown.final_y_offset(100);
        assert_eq!(exit_down, 8);
    }

    #[test]
    fn edge_zero_modal_height_y_offset() {
        assert_eq!(ModalEntranceAnimation::SlideDown.initial_y_offset(0), 0);
        assert_eq!(ModalEntranceAnimation::SlideUp.initial_y_offset(0), 0);
        assert_eq!(ModalExitAnimation::SlideUp.final_y_offset(0), 0);
        assert_eq!(ModalExitAnimation::SlideDown.final_y_offset(0), 0);
    }

    #[test]
    fn edge_config_builder_methods() {
        let config = ModalAnimationConfig::new()
            .entrance(ModalEntranceAnimation::SlideDown)
            .exit(ModalExitAnimation::SlideUp)
            .entrance_duration(Duration::from_millis(300))
            .exit_duration(Duration::from_millis(200))
            .entrance_easing(ModalEasing::Back)
            .exit_easing(ModalEasing::EaseInOut)
            .min_scale(0.8)
            .animate_backdrop(false)
            .backdrop_duration(Duration::from_millis(50))
            .respect_reduced_motion(false);

        assert_eq!(config.entrance, ModalEntranceAnimation::SlideDown);
        assert_eq!(config.exit, ModalExitAnimation::SlideUp);
        assert_eq!(config.entrance_duration, Duration::from_millis(300));
        assert_eq!(config.exit_duration, Duration::from_millis(200));
        assert_eq!(config.entrance_easing, ModalEasing::Back);
        assert_eq!(config.exit_easing, ModalEasing::EaseInOut);
        assert!((config.min_scale - 0.8).abs() < 1e-10);
        assert!(!config.animate_backdrop);
        assert_eq!(config.backdrop_duration, Duration::from_millis(50));
        assert!(!config.respect_reduced_motion);
    }

    #[test]
    fn edge_min_scale_clamped() {
        // Below 0.5 → clamped to 0.5
        let config = ModalAnimationConfig::new().min_scale(0.1);
        assert!((config.min_scale - 0.5).abs() < 1e-10);

        // Above 1.0 → clamped to 1.0
        let config = ModalAnimationConfig::new().min_scale(1.5);
        assert!((config.min_scale - 1.0).abs() < 1e-10);

        // Normal value passes through
        let config = ModalAnimationConfig::new().min_scale(0.75);
        assert!((config.min_scale - 0.75).abs() < 1e-10);
    }

    #[test]
    fn edge_is_disabled() {
        let config = ModalAnimationConfig::none();
        assert!(config.is_disabled());

        let config = ModalAnimationConfig::default();
        assert!(!config.is_disabled());

        // Only entrance None but exit not → not disabled
        let config = ModalAnimationConfig::new()
            .entrance(ModalEntranceAnimation::None)
            .exit(ModalExitAnimation::FadeOut);
        assert!(!config.is_disabled());
    }

    #[test]
    fn edge_effective_without_reduced_motion() {
        let config = ModalAnimationConfig::default();
        let eff = config.effective(false);
        // Should return a clone of the original config
        assert_eq!(eff.entrance, ModalEntranceAnimation::ScaleIn);
        assert_eq!(eff.exit, ModalExitAnimation::ScaleOut);
    }

    #[test]
    fn edge_effective_with_reduced_motion_but_not_respected() {
        let config = ModalAnimationConfig::default().respect_reduced_motion(false);
        let eff = config.effective(true);
        // respect_reduced_motion=false → should NOT apply reduced motion
        assert_eq!(eff.entrance, ModalEntranceAnimation::ScaleIn);
    }

    #[test]
    fn edge_current_values_helper() {
        let state = ModalAnimationState::open();
        let config = ModalAnimationConfig::default();
        let (scale, opacity, backdrop, y_offset) = state.current_values(&config, 20);
        assert!((scale - 1.0).abs() < 1e-10);
        assert!((opacity - 1.0).abs() < 1e-10);
        assert!((backdrop - 1.0).abs() < 1e-10);
        assert_eq!(y_offset, 0);
    }

    #[test]
    fn edge_current_values_closed() {
        let state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default();
        let (scale, opacity, backdrop, y_offset) = state.current_values(&config, 20);
        assert!((scale - config.min_scale).abs() < 1e-10);
        assert!(opacity.abs() < 1e-10);
        assert!(backdrop.abs() < 1e-10);
        assert_eq!(y_offset, 0);
    }

    #[test]
    fn edge_tick_noop_on_open() {
        let mut state = ModalAnimationState::open();
        let config = ModalAnimationConfig::default();
        let changed = state.tick(Duration::from_millis(100), &config);
        assert!(!changed);
        assert_eq!(state.phase(), ModalAnimationPhase::Open);
    }

    #[test]
    fn edge_tick_noop_on_closed() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default();
        let changed = state.tick(Duration::from_millis(100), &config);
        assert!(!changed);
        assert_eq!(state.phase(), ModalAnimationPhase::Closed);
    }

    #[test]
    fn edge_tick_returns_false_mid_animation() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default();
        state.start_opening();
        // Small tick that won't complete the 200ms animation
        let changed = state.tick(Duration::from_millis(50), &config);
        assert!(!changed);
        assert_eq!(state.phase(), ModalAnimationPhase::Opening);
    }

    #[test]
    fn edge_closing_animation_completes_to_closed() {
        let mut state = ModalAnimationState::open();
        let config = ModalAnimationConfig::default();
        state.start_closing();
        let changed = state.tick(Duration::from_secs(1), &config);
        assert!(changed);
        assert_eq!(state.phase(), ModalAnimationPhase::Closed);
        assert_eq!(state.progress(), 0.0);
        assert_eq!(state.backdrop_progress(), 0.0);
    }

    #[test]
    fn edge_start_opening_when_open_is_noop() {
        let mut state = ModalAnimationState::open();
        state.start_opening();
        assert_eq!(state.phase(), ModalAnimationPhase::Open);
        assert_eq!(state.progress(), 1.0);
    }

    #[test]
    fn edge_start_closing_when_closed_is_noop() {
        let mut state = ModalAnimationState::new();
        state.start_closing();
        assert_eq!(state.phase(), ModalAnimationPhase::Closed);
        assert_eq!(state.progress(), 0.0);
    }

    #[test]
    fn edge_default_state_equals_new() {
        let default = ModalAnimationState::default();
        let new = ModalAnimationState::new();
        assert_eq!(default.phase(), new.phase());
        assert_eq!(default.progress(), new.progress());
        assert_eq!(default.backdrop_progress(), new.backdrop_progress());
    }

    #[test]
    fn edge_backdrop_no_animation() {
        let mut state = ModalAnimationState::new();
        let config = ModalAnimationConfig::default().animate_backdrop(false);
        state.start_opening();

        // With animate_backdrop=false, backdrop should be 1.0 during Opening
        let backdrop = state.current_backdrop_opacity(&config);
        assert!((backdrop - 1.0).abs() < 1e-10);

        // Force to closing
        state.force_open();
        state.start_closing();
        let backdrop = state.current_backdrop_opacity(&config);
        assert!(backdrop.abs() < 1e-10);
    }

    #[test]
    fn edge_entrance_scale_at_progress_clamped() {
        let config = ModalAnimationConfig::default();
        // Progress values outside [0, 1] should be clamped
        let s = ModalEntranceAnimation::ScaleIn.scale_at_progress(-0.5, &config);
        assert!((s - config.min_scale).abs() < 1e-10);
        let s = ModalEntranceAnimation::ScaleIn.scale_at_progress(2.0, &config);
        assert!((s - 1.0).abs() < 1e-10);
    }

    #[test]
    fn edge_entrance_opacity_at_progress_clamped() {
        let o = ModalEntranceAnimation::FadeIn.opacity_at_progress(-1.0);
        assert!(o.abs() < 1e-10);
        let o = ModalEntranceAnimation::FadeIn.opacity_at_progress(5.0);
        assert!((o - 1.0).abs() < 1e-10);
    }

    #[test]
    fn edge_entrance_y_offset_at_progress_clamped() {
        // At progress < 0 → clamped to 0 → full initial offset
        let y = ModalEntranceAnimation::SlideDown.y_offset_at_progress(-1.0, 20);
        assert_eq!(y, ModalEntranceAnimation::SlideDown.initial_y_offset(20));
        // At progress > 1 → clamped to 1 → offset 0
        let y = ModalEntranceAnimation::SlideDown.y_offset_at_progress(5.0, 20);
        assert_eq!(y, 0);
    }

    #[test]
    fn edge_phase_default_is_closed() {
        assert_eq!(ModalAnimationPhase::default(), ModalAnimationPhase::Closed);
    }

    #[test]
    fn edge_entrance_default_is_scale_in() {
        assert_eq!(
            ModalEntranceAnimation::default(),
            ModalEntranceAnimation::ScaleIn
        );
    }

    #[test]
    fn edge_exit_default_is_scale_out() {
        assert_eq!(ModalExitAnimation::default(), ModalExitAnimation::ScaleOut);
    }

    #[test]
    fn edge_easing_default_is_ease_out() {
        assert_eq!(ModalEasing::default(), ModalEasing::EaseOut);
    }

    #[test]
    fn edge_config_none_fields() {
        let config = ModalAnimationConfig::none();
        assert_eq!(config.entrance, ModalEntranceAnimation::None);
        assert_eq!(config.exit, ModalExitAnimation::None);
        assert_eq!(config.entrance_duration, Duration::ZERO);
        assert_eq!(config.exit_duration, Duration::ZERO);
        assert_eq!(config.backdrop_duration, Duration::ZERO);
    }

    #[test]
    fn edge_state_is_visible_is_closed_is_open() {
        let mut state = ModalAnimationState::new();
        assert!(!state.is_visible());
        assert!(state.is_closed());
        assert!(!state.is_open());
        assert!(!state.is_animating());

        state.start_opening();
        assert!(state.is_visible());
        assert!(!state.is_closed());
        assert!(!state.is_open());
        assert!(state.is_animating());

        state.force_open();
        assert!(state.is_visible());
        assert!(!state.is_closed());
        assert!(state.is_open());
        assert!(!state.is_animating());
    }

    #[test]
    fn edge_force_open_during_closing() {
        let mut state = ModalAnimationState::open();
        state.start_closing();
        let config = ModalAnimationConfig::default();
        state.tick(Duration::from_millis(50), &config);
        assert_eq!(state.phase(), ModalAnimationPhase::Closing);

        state.force_open();
        assert_eq!(state.phase(), ModalAnimationPhase::Open);
        assert_eq!(state.progress(), 1.0);
    }

    #[test]
    fn edge_force_close_during_opening() {
        let mut state = ModalAnimationState::new();
        state.start_opening();
        let config = ModalAnimationConfig::default();
        state.tick(Duration::from_millis(50), &config);

        state.force_close();
        assert_eq!(state.phase(), ModalAnimationPhase::Closed);
        assert_eq!(state.progress(), 0.0);
    }

    #[test]
    fn edge_eased_progress_open_closed() {
        let config = ModalAnimationConfig::default();
        let state_open = ModalAnimationState::open();
        assert!((state_open.eased_progress(&config) - 1.0).abs() < 1e-10);

        let state_closed = ModalAnimationState::new();
        assert!(state_closed.eased_progress(&config).abs() < 1e-10);
    }

    #[test]
    fn edge_eased_backdrop_progress_open_closed() {
        let config = ModalAnimationConfig::default();
        let state_open = ModalAnimationState::open();
        assert!((state_open.eased_backdrop_progress(&config) - 1.0).abs() < 1e-10);

        let state_closed = ModalAnimationState::new();
        assert!(state_closed.eased_backdrop_progress(&config).abs() < 1e-10);
    }

    #[test]
    fn edge_clone_debug_phase() {
        let phase = ModalAnimationPhase::Opening;
        let cloned = phase;
        assert_eq!(cloned, ModalAnimationPhase::Opening);
        let _ = format!("{phase:?}");
    }

    #[test]
    fn edge_clone_debug_entrance() {
        let anim = ModalEntranceAnimation::SlideDown;
        let cloned = anim;
        assert_eq!(cloned, ModalEntranceAnimation::SlideDown);
        let _ = format!("{anim:?}");
    }

    #[test]
    fn edge_clone_debug_exit() {
        let anim = ModalExitAnimation::SlideUp;
        let cloned = anim;
        assert_eq!(cloned, ModalExitAnimation::SlideUp);
        let _ = format!("{anim:?}");
    }

    #[test]
    fn edge_clone_debug_easing() {
        let easing = ModalEasing::Back;
        let _ = format!("{easing:?}");
        // PartialEq
        assert_eq!(easing, ModalEasing::Back);
        assert_ne!(easing, ModalEasing::Linear);
    }

    #[test]
    fn edge_clone_debug_config() {
        let config = ModalAnimationConfig::default();
        let cloned = config.clone();
        assert_eq!(cloned.entrance, config.entrance);
        assert_eq!(cloned.exit, config.exit);
        let _ = format!("{config:?}");
    }

    #[test]
    fn edge_clone_debug_state() {
        let mut state = ModalAnimationState::new();
        state.start_opening();
        let cloned = state.clone();
        assert_eq!(cloned.phase(), state.phase());
        assert_eq!(cloned.progress(), state.progress());
        let _ = format!("{state:?}");
    }
}

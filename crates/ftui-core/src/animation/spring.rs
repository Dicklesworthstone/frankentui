#![forbid(unsafe_code)]

//! Damped harmonic oscillator (spring) animation.
//!
//! Provides physically-based motion for smooth, natural UI transitions.
//! Based on the classical damped spring equation:
//!
//!   F = -stiffness × (position - target) - damping × velocity
//!
//! # Parameters
//!
//! - **stiffness** (k): Restoring force strength. Higher = faster response.
//!   Typical range: 100–400 for UI motion.
//! - **damping** (c): Velocity drag. Higher = less oscillation.
//!   - Underdamped (c < 2√k): oscillates past target before settling
//!   - Critically damped (c ≈ 2√k): fastest convergence without overshoot
//!   - Overdamped (c > 2√k): slow convergence, no overshoot
//! - **rest_threshold**: Position delta below which the spring is considered
//!   at rest. Default: 0.001.
//!
//! # Integration
//!
//! Uses semi-implicit Euler integration for stability. The `tick()` method
//! accepts a `Duration` and internally converts to seconds for the physics
//! step.
//!
//! # Invariants
//!
//! 1. `value()` returns a normalized position in [0.0, 1.0] (clamped).
//! 2. `position()` returns the raw (unclamped) position for full-range use.
//! 3. A spring at rest (`is_complete() == true`) will not resume unless
//!    `set_target()` or `reset()` is called.
//! 4. `reset()` returns position to the initial value and zeroes velocity.
//! 5. Stiffness and damping are always positive (clamped on construction).
//!
//! # Failure Modes
//!
//! - Very large dt: Integration may overshoot badly. Callers should cap dt
//!   to a reasonable frame budget (e.g., 33ms). For dt > 100ms, the spring
//!   subdivides into smaller steps for stability.
//! - Zero stiffness: Spring will never converge; clamped to minimum 0.1.
//! - Zero damping: Spring oscillates forever; not a failure mode, but
//!   `is_complete()` may never return true.

use std::time::Duration;

use super::Animation;

/// Maximum dt per integration step (4ms). Larger deltas are subdivided
/// for numerical stability with high stiffness values.
const MAX_STEP_SECS: f64 = 0.004;

/// Default rest threshold: position delta below which the spring is "at rest".
const DEFAULT_REST_THRESHOLD: f64 = 0.001;

/// Default velocity threshold: velocity below which (combined with position
/// threshold) the spring is considered at rest.
const DEFAULT_VELOCITY_THRESHOLD: f64 = 0.01;

/// Minimum stiffness to prevent degenerate springs.
const MIN_STIFFNESS: f64 = 0.1;

/// A damped harmonic oscillator producing physically-based motion.
///
/// The spring interpolates from an initial position toward a target,
/// with configurable stiffness and damping.
///
/// # Example
///
/// ```ignore
/// use std::time::Duration;
/// use ftui_core::animation::spring::Spring;
///
/// let mut spring = Spring::new(0.0, 1.0)
///     .with_stiffness(170.0)
///     .with_damping(26.0);
///
/// // Simulate at 60fps
/// for _ in 0..120 {
///     spring.tick(Duration::from_millis(16));
/// }
///
/// assert!((spring.position() - 1.0).abs() < 0.01);
/// ```
#[derive(Debug, Clone)]
pub struct Spring {
    position: f64,
    velocity: f64,
    target: f64,
    initial: f64,
    stiffness: f64,
    damping: f64,
    rest_threshold: f64,
    velocity_threshold: f64,
    at_rest: bool,
}

impl Spring {
    /// Create a spring starting at `initial` and targeting `target`.
    ///
    /// Default parameters: stiffness = 170.0, damping = 26.0 (slightly
    /// underdamped, producing a subtle bounce).
    #[must_use]
    pub fn new(initial: f64, target: f64) -> Self {
        Self {
            position: initial,
            velocity: 0.0,
            target,
            initial,
            stiffness: 170.0,
            damping: 26.0,
            rest_threshold: DEFAULT_REST_THRESHOLD,
            velocity_threshold: DEFAULT_VELOCITY_THRESHOLD,
            at_rest: false,
        }
    }

    /// Create a spring animating from 0.0 to 1.0 (normalized).
    #[must_use]
    pub fn normalized() -> Self {
        Self::new(0.0, 1.0)
    }

    /// Set stiffness (builder pattern). Clamped to minimum 0.1.
    #[must_use]
    pub fn with_stiffness(mut self, k: f64) -> Self {
        self.stiffness = k.max(MIN_STIFFNESS);
        self
    }

    /// Set damping (builder pattern). Clamped to minimum 0.0.
    #[must_use]
    pub fn with_damping(mut self, c: f64) -> Self {
        self.damping = c.max(0.0);
        self
    }

    /// Set rest threshold (builder pattern).
    #[must_use]
    pub fn with_rest_threshold(mut self, threshold: f64) -> Self {
        self.rest_threshold = threshold.abs();
        self
    }

    /// Set velocity threshold (builder pattern).
    #[must_use]
    pub fn with_velocity_threshold(mut self, threshold: f64) -> Self {
        self.velocity_threshold = threshold.abs();
        self
    }

    /// Current position (unclamped).
    #[inline]
    #[must_use]
    pub fn position(&self) -> f64 {
        self.position
    }

    /// Current velocity.
    #[inline]
    #[must_use]
    pub fn velocity(&self) -> f64 {
        self.velocity
    }

    /// Current target.
    #[inline]
    #[must_use]
    pub fn target(&self) -> f64 {
        self.target
    }

    /// Stiffness parameter.
    #[inline]
    #[must_use]
    pub fn stiffness(&self) -> f64 {
        self.stiffness
    }

    /// Damping parameter.
    #[inline]
    #[must_use]
    pub fn damping(&self) -> f64 {
        self.damping
    }

    /// Change the target. Wakes the spring if it was at rest.
    pub fn set_target(&mut self, target: f64) {
        if (self.target - target).abs() > self.rest_threshold {
            self.target = target;
            self.at_rest = false;
        }
    }

    /// Apply an impulse (add to velocity). Wakes the spring.
    pub fn impulse(&mut self, velocity_delta: f64) {
        self.velocity += velocity_delta;
        self.at_rest = false;
    }

    /// Whether the spring has settled at the target.
    #[inline]
    #[must_use]
    pub fn is_at_rest(&self) -> bool {
        self.at_rest
    }

    /// Compute the critical damping coefficient for the current stiffness.
    ///
    /// At critical damping, the spring converges as fast as possible without
    /// oscillating.
    #[must_use]
    pub fn critical_damping(&self) -> f64 {
        2.0 * self.stiffness.sqrt()
    }

    /// Perform a single integration step of `dt` seconds.
    fn step(&mut self, dt: f64) {
        // Semi-implicit Euler:
        // 1. Compute acceleration from current position.
        // 2. Update velocity.
        // 3. Update position from new velocity.
        let displacement = self.position - self.target;
        let spring_force = -self.stiffness * displacement;
        let damping_force = -self.damping * self.velocity;
        let acceleration = spring_force + damping_force;

        self.velocity += acceleration * dt;
        self.position += self.velocity * dt;
    }

    /// Advance the spring by `dt`, subdividing if necessary for stability.
    pub fn advance(&mut self, dt: Duration) {
        if self.at_rest {
            return;
        }

        let total_secs = dt.as_secs_f64();
        if total_secs <= 0.0 {
            return;
        }

        // Subdivide large dt for numerical stability.
        let mut remaining = total_secs;
        while remaining > 0.0 {
            let step_dt = remaining.min(MAX_STEP_SECS);
            self.step(step_dt);
            remaining -= step_dt;
        }

        // Check if at rest.
        let pos_delta = (self.position - self.target).abs();
        let vel_abs = self.velocity.abs();
        if pos_delta < self.rest_threshold && vel_abs < self.velocity_threshold {
            self.position = self.target;
            self.velocity = 0.0;
            self.at_rest = true;
        }
    }
}

impl Animation for Spring {
    fn tick(&mut self, dt: Duration) {
        self.advance(dt);
    }

    fn is_complete(&self) -> bool {
        self.at_rest
    }

    /// Returns the spring position clamped to [0.0, 1.0].
    ///
    /// For springs with targets outside [0, 1], use [`position()`](Spring::position)
    /// directly.
    fn value(&self) -> f32 {
        (self.position as f32).clamp(0.0, 1.0)
    }

    fn reset(&mut self) {
        self.position = self.initial;
        self.velocity = 0.0;
        self.at_rest = false;
    }
}

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

/// Common spring configurations for UI motion.
pub mod presets {
    use super::Spring;

    /// Gentle spring: low stiffness, high damping. Smooth and slow.
    #[must_use]
    pub fn gentle() -> Spring {
        Spring::normalized()
            .with_stiffness(120.0)
            .with_damping(20.0)
    }

    /// Bouncy spring: high stiffness, low damping. Visible oscillation.
    #[must_use]
    pub fn bouncy() -> Spring {
        Spring::normalized()
            .with_stiffness(300.0)
            .with_damping(10.0)
    }

    /// Stiff spring: high stiffness, near-critical damping. Snappy response.
    #[must_use]
    pub fn stiff() -> Spring {
        Spring::normalized()
            .with_stiffness(400.0)
            .with_damping(38.0)
    }

    /// Critically damped spring: fastest convergence without overshoot.
    #[must_use]
    pub fn critical() -> Spring {
        let k: f64 = 170.0;
        let c = 2.0 * k.sqrt(); // critical damping
        Spring::normalized().with_stiffness(k).with_damping(c)
    }

    /// Slow spring: very low stiffness. Good for background transitions.
    #[must_use]
    pub fn slow() -> Spring {
        Spring::normalized().with_stiffness(50.0).with_damping(14.0)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const MS_16: Duration = Duration::from_millis(16);

    fn simulate(spring: &mut Spring, frames: usize) {
        for _ in 0..frames {
            spring.tick(MS_16);
        }
    }

    #[test]
    fn spring_reaches_target() {
        let mut spring = Spring::new(0.0, 100.0)
            .with_stiffness(170.0)
            .with_damping(26.0);

        simulate(&mut spring, 200);

        assert!(
            (spring.position() - 100.0).abs() < 0.1,
            "position: {}",
            spring.position()
        );
        assert!(spring.is_complete());
    }

    #[test]
    fn spring_starts_at_initial() {
        let spring = Spring::new(50.0, 100.0);
        assert!((spring.position() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn spring_target_change() {
        let mut spring = Spring::new(0.0, 100.0);
        spring.set_target(200.0);
        assert!((spring.target() - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn spring_with_high_damping_minimal_overshoot() {
        let mut spring = Spring::new(0.0, 100.0)
            .with_stiffness(170.0)
            .with_damping(100.0); // Heavily overdamped

        let mut max_overshoot = 0.0_f64;
        for _ in 0..300 {
            spring.tick(MS_16);
            let overshoot = spring.position() - 100.0;
            if overshoot > max_overshoot {
                max_overshoot = overshoot;
            }
        }

        assert!(
            max_overshoot < 1.0,
            "High damping should minimize overshoot, got {max_overshoot}"
        );
    }

    #[test]
    fn critical_damping_no_overshoot() {
        let mut spring = presets::critical();
        // Scale target for easier measurement
        spring.set_target(1.0);

        let mut max_pos = 0.0_f64;
        for _ in 0..300 {
            spring.tick(MS_16);
            if spring.position() > max_pos {
                max_pos = spring.position();
            }
        }

        assert!(
            max_pos < 1.05,
            "Critical damping should have negligible overshoot, got {max_pos}"
        );
    }

    #[test]
    fn bouncy_spring_overshoots() {
        let mut spring = presets::bouncy();

        let mut max_pos = 0.0_f64;
        for _ in 0..200 {
            spring.tick(MS_16);
            if spring.position() > max_pos {
                max_pos = spring.position();
            }
        }

        assert!(
            max_pos > 1.0,
            "Bouncy spring should overshoot target, max was {max_pos}"
        );
    }

    #[test]
    fn normalized_spring_value_clamped() {
        let mut spring = presets::bouncy();
        for _ in 0..200 {
            spring.tick(MS_16);
            let v = spring.value();
            assert!(
                (0.0..=1.0).contains(&v),
                "Animation::value() must be in [0,1], got {v}"
            );
        }
    }

    #[test]
    fn spring_reset() {
        let mut spring = Spring::new(0.0, 1.0);
        simulate(&mut spring, 100);
        assert!(spring.is_complete());

        spring.reset();
        assert!(!spring.is_complete());
        assert!((spring.position() - 0.0).abs() < f64::EPSILON);
        assert!((spring.velocity() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn spring_impulse_wakes() {
        let mut spring = Spring::new(0.0, 0.0);
        simulate(&mut spring, 100);
        assert!(spring.is_complete());

        spring.impulse(50.0);
        assert!(!spring.is_complete());
        spring.tick(MS_16);
        assert!(spring.position().abs() > 0.0);
    }

    #[test]
    fn set_target_wakes_spring() {
        let mut spring = Spring::new(0.0, 1.0);
        simulate(&mut spring, 200);
        assert!(spring.is_complete());

        spring.set_target(2.0);
        assert!(!spring.is_complete());
    }

    #[test]
    fn set_target_same_value_stays_at_rest() {
        let mut spring = Spring::new(0.0, 1.0);
        simulate(&mut spring, 200);
        assert!(spring.is_complete());

        spring.set_target(1.0);
        assert!(spring.is_complete());
    }

    #[test]
    fn zero_dt_noop() {
        let mut spring = Spring::new(0.0, 1.0);
        let pos_before = spring.position();
        spring.tick(Duration::ZERO);
        assert!((spring.position() - pos_before).abs() < f64::EPSILON);
    }

    #[test]
    fn large_dt_subdivided() {
        let mut spring = Spring::new(0.0, 1.0)
            .with_stiffness(170.0)
            .with_damping(26.0);

        // Large dt should still converge (subdivided internally).
        spring.tick(Duration::from_secs(5));
        assert!(
            (spring.position() - 1.0).abs() < 0.01,
            "position: {}",
            spring.position()
        );
    }

    #[test]
    fn zero_stiffness_clamped() {
        let spring = Spring::new(0.0, 1.0).with_stiffness(0.0);
        assert!(spring.stiffness() >= MIN_STIFFNESS);
    }

    #[test]
    fn negative_damping_clamped() {
        let spring = Spring::new(0.0, 1.0).with_damping(-5.0);
        assert!(spring.damping() >= 0.0);
    }

    #[test]
    fn critical_damping_coefficient() {
        let spring = Spring::new(0.0, 1.0).with_stiffness(100.0);
        assert!((spring.critical_damping() - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn spring_negative_target() {
        let mut spring = Spring::new(0.0, -1.0)
            .with_stiffness(170.0)
            .with_damping(26.0);

        simulate(&mut spring, 200);
        assert!(
            (spring.position() - -1.0).abs() < 0.01,
            "position: {}",
            spring.position()
        );
    }

    #[test]
    fn spring_reverse_direction() {
        let mut spring = Spring::new(1.0, 0.0)
            .with_stiffness(170.0)
            .with_damping(26.0);

        simulate(&mut spring, 200);
        assert!(
            spring.position().abs() < 0.01,
            "position: {}",
            spring.position()
        );
    }

    #[test]
    fn presets_all_converge() {
        let presets: Vec<(&str, Spring)> = vec![
            ("gentle", presets::gentle()),
            ("bouncy", presets::bouncy()),
            ("stiff", presets::stiff()),
            ("critical", presets::critical()),
            ("slow", presets::slow()),
        ];

        for (name, mut spring) in presets {
            simulate(&mut spring, 500);
            assert!(
                spring.is_complete(),
                "preset '{name}' did not converge after 500 frames (pos: {}, vel: {})",
                spring.position(),
                spring.velocity()
            );
        }
    }

    #[test]
    fn deterministic_across_runs() {
        let run = || {
            let mut spring = Spring::new(0.0, 1.0)
                .with_stiffness(170.0)
                .with_damping(26.0);
            let mut positions = Vec::new();
            for _ in 0..50 {
                spring.tick(MS_16);
                positions.push(spring.position());
            }
            positions
        };

        let run1 = run();
        let run2 = run();
        assert_eq!(run1, run2, "Spring should be deterministic");
    }

    #[test]
    fn at_rest_spring_skips_computation() {
        let mut spring = Spring::new(0.0, 1.0);
        simulate(&mut spring, 200);
        assert!(spring.is_complete());

        let pos = spring.position();
        spring.tick(MS_16);
        assert!(
            (spring.position() - pos).abs() < f64::EPSILON,
            "At-rest spring should not change position on tick"
        );
    }

    #[test]
    fn animation_trait_value_for_normalized() {
        let mut spring = Spring::normalized();
        assert!((spring.value() - 0.0).abs() < f32::EPSILON);

        simulate(&mut spring, 200);
        assert!((spring.value() - 1.0).abs() < 0.01);
    }

    #[test]
    fn stiff_preset_faster_than_slow() {
        let mut stiff = presets::stiff();
        let mut slow = presets::slow();

        // After 30 frames, stiff should be closer to target
        for _ in 0..30 {
            stiff.tick(MS_16);
            slow.tick(MS_16);
        }

        let stiff_delta = (stiff.position() - 1.0).abs();
        let slow_delta = (slow.position() - 1.0).abs();
        assert!(
            stiff_delta < slow_delta,
            "Stiff ({stiff_delta}) should be closer to target than slow ({slow_delta})"
        );
    }

    // ── Edge-case tests (bd-3r5rp) ──────────────────────────────────

    #[test]
    fn clone_independence() {
        let mut spring = Spring::new(0.0, 1.0);
        simulate(&mut spring, 5); // Only 5 frames — still in motion.
        let pos_after_5 = spring.position();
        let mut clone = spring.clone();
        // Original doesn't advance further.
        // Clone advances 5 more frames.
        simulate(&mut clone, 5);
        // Clone should have moved beyond the original's position.
        assert!(
            (clone.position() - pos_after_5).abs() > 0.01,
            "clone should advance independently (clone: {}, original: {})",
            clone.position(),
            pos_after_5
        );
        // Original should not have moved.
        assert!(
            (spring.position() - pos_after_5).abs() < f64::EPSILON,
            "original should not have changed"
        );
    }

    #[test]
    fn debug_format() {
        let spring = Spring::new(0.0, 1.0);
        let dbg = format!("{spring:?}");
        assert!(dbg.contains("Spring"));
        assert!(dbg.contains("position"));
        assert!(dbg.contains("velocity"));
        assert!(dbg.contains("target"));
    }

    #[test]
    fn negative_stiffness_clamped() {
        let spring = Spring::new(0.0, 1.0).with_stiffness(-100.0);
        assert!(spring.stiffness() >= MIN_STIFFNESS);
    }

    #[test]
    fn with_rest_threshold_builder() {
        let spring = Spring::new(0.0, 1.0).with_rest_threshold(0.1);
        assert!((spring.rest_threshold - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn with_rest_threshold_negative_takes_abs() {
        let spring = Spring::new(0.0, 1.0).with_rest_threshold(-0.05);
        assert!((spring.rest_threshold - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn with_velocity_threshold_builder() {
        let spring = Spring::new(0.0, 1.0).with_velocity_threshold(0.5);
        assert!((spring.velocity_threshold - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn with_velocity_threshold_negative_takes_abs() {
        let spring = Spring::new(0.0, 1.0).with_velocity_threshold(-0.3);
        assert!((spring.velocity_threshold - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn initial_equals_target_settles_immediately() {
        let mut spring = Spring::new(5.0, 5.0);
        // After one tick, should settle since position == target and velocity == 0.
        spring.tick(MS_16);
        assert!(spring.is_complete());
        assert!((spring.position() - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn normalized_constructor() {
        let spring = Spring::normalized();
        assert!((spring.position() - 0.0).abs() < f64::EPSILON);
        assert!((spring.target() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn impulse_negative_velocity() {
        let mut spring = Spring::new(0.5, 0.5);
        spring.tick(MS_16); // Settle at 0.5.
        // Apply strong negative impulse.
        spring.impulse(-100.0);
        assert!(!spring.is_complete());
        spring.tick(MS_16);
        assert!(
            spring.position() < 0.5,
            "Negative impulse should move position below target, got {}",
            spring.position()
        );
    }

    #[test]
    fn impulse_on_moving_spring() {
        let mut spring = Spring::new(0.0, 1.0);
        spring.tick(MS_16);
        let vel_before = spring.velocity();
        spring.impulse(10.0);
        // Velocity should be additive.
        assert!(
            (spring.velocity() - (vel_before + 10.0)).abs() < f64::EPSILON,
            "impulse should add to velocity"
        );
    }

    #[test]
    fn set_target_within_rest_threshold_stays_at_rest() {
        let mut spring = Spring::new(0.0, 1.0).with_rest_threshold(0.01);
        simulate(&mut spring, 300);
        assert!(spring.is_complete());

        // Set target very close (within rest_threshold).
        spring.set_target(1.0 + 0.005);
        assert!(
            spring.is_complete(),
            "set_target within rest_threshold should not wake spring"
        );
    }

    #[test]
    fn set_target_just_beyond_rest_threshold_wakes() {
        let mut spring = Spring::new(0.0, 1.0).with_rest_threshold(0.01);
        simulate(&mut spring, 300);
        assert!(spring.is_complete());

        // Set target just beyond rest_threshold.
        spring.set_target(1.0 + 0.02);
        assert!(
            !spring.is_complete(),
            "set_target beyond rest_threshold should wake spring"
        );
    }

    #[test]
    fn large_rest_threshold_settles_quickly() {
        let mut spring = Spring::new(0.0, 1.0)
            .with_stiffness(170.0)
            .with_damping(26.0)
            .with_rest_threshold(0.5)
            .with_velocity_threshold(10.0);

        // With huge thresholds, spring should settle very quickly.
        simulate(&mut spring, 10);
        assert!(
            spring.is_complete(),
            "Large thresholds should cause early settling (pos: {}, vel: {})",
            spring.position(),
            spring.velocity()
        );
    }

    #[test]
    fn value_clamps_negative_position() {
        // Spring going negative due to overshoot/impulse.
        let mut spring = Spring::new(0.0, 0.0);
        spring.impulse(-100.0);
        spring.tick(MS_16);
        // Position should be negative, but value() clamped to 0.
        assert!(spring.position() < 0.0);
        assert!((spring.value() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn value_clamps_above_one() {
        // Spring targeting beyond 1.0.
        let mut spring = Spring::new(0.0, 5.0);
        simulate(&mut spring, 200);
        assert!(spring.position() > 1.0);
        assert!((spring.value() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn zero_damping_oscillates() {
        let mut spring = Spring::new(0.0, 1.0)
            .with_stiffness(170.0)
            .with_damping(0.0);

        // With zero damping, spring should oscillate.
        let mut crossed_target = false;
        let mut crossed_back = false;
        let mut above = false;
        for _ in 0..200 {
            spring.tick(MS_16);
            if spring.position() > 1.0 {
                above = true;
            }
            if above && spring.position() < 1.0 {
                crossed_target = true;
            }
            if crossed_target && spring.position() > 1.0 {
                crossed_back = true;
                break;
            }
        }
        assert!(crossed_back, "Zero-damping spring should oscillate");
    }

    #[test]
    fn advance_at_rest_is_noop() {
        let mut spring = Spring::new(0.0, 1.0);
        simulate(&mut spring, 300);
        assert!(spring.is_complete());

        let pos = spring.position();
        let vel = spring.velocity();
        spring.advance(Duration::from_secs(10));
        assert!((spring.position() - pos).abs() < f64::EPSILON);
        assert!((spring.velocity() - vel).abs() < f64::EPSILON);
    }

    #[test]
    fn reset_restores_initial() {
        let mut spring = Spring::new(42.0, 100.0);
        simulate(&mut spring, 200);
        spring.reset();
        assert!((spring.position() - 42.0).abs() < f64::EPSILON);
        assert!((spring.velocity() - 0.0).abs() < f64::EPSILON);
        assert!(!spring.is_complete());
    }

    #[test]
    fn reset_after_impulse() {
        let mut spring = Spring::new(0.0, 0.0);
        spring.impulse(50.0);
        spring.tick(MS_16);
        spring.reset();
        assert!((spring.position() - 0.0).abs() < f64::EPSILON);
        assert!((spring.velocity() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn multiple_set_target_chained() {
        let mut spring = Spring::new(0.0, 1.0);
        simulate(&mut spring, 50);
        spring.set_target(2.0);
        simulate(&mut spring, 50);
        spring.set_target(0.0);
        simulate(&mut spring, 300);
        assert!(
            spring.position().abs() < 0.01,
            "Should converge to final target 0.0, got {}",
            spring.position()
        );
    }

    #[test]
    fn animation_trait_overshoot_is_zero_for_spring() {
        let mut spring = Spring::new(0.0, 1.0);
        assert_eq!(spring.overshoot(), Duration::ZERO);

        simulate(&mut spring, 300);
        assert_eq!(spring.overshoot(), Duration::ZERO);
    }

    #[test]
    fn preset_gentle_parameters() {
        let s = presets::gentle();
        assert!((s.stiffness() - 120.0).abs() < f64::EPSILON);
        assert!((s.damping() - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn preset_bouncy_parameters() {
        let s = presets::bouncy();
        assert!((s.stiffness() - 300.0).abs() < f64::EPSILON);
        assert!((s.damping() - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn preset_stiff_parameters() {
        let s = presets::stiff();
        assert!((s.stiffness() - 400.0).abs() < f64::EPSILON);
        assert!((s.damping() - 38.0).abs() < f64::EPSILON);
    }

    #[test]
    fn preset_slow_parameters() {
        let s = presets::slow();
        assert!((s.stiffness() - 50.0).abs() < f64::EPSILON);
        assert!((s.damping() - 14.0).abs() < f64::EPSILON);
    }

    #[test]
    fn preset_critical_is_critically_damped() {
        let s = presets::critical();
        let expected_damping = 2.0 * s.stiffness().sqrt();
        assert!(
            (s.damping() - expected_damping).abs() < f64::EPSILON,
            "critical preset should have c = 2*sqrt(k)"
        );
    }
}

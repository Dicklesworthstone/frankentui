#![forbid(unsafe_code)]

//! Composable animation primitives.
//!
//! Time-based animations that produce normalized `f32` values (0.0–1.0).
//! Designed for zero allocation during tick, composable via generics.
//!
//! # Budget Integration
//!
//! Animations themselves are budget-unaware. The caller decides whether to
//! call [`Animation::tick`] based on the current [`DegradationLevel`]:
//!
//! ```ignore
//! if budget.degradation().render_decorative() {
//!     my_animation.tick(dt);
//! }
//! ```
//!
//! [`DegradationLevel`]: ftui_render::budget::DegradationLevel

use std::time::Duration;

// ---------------------------------------------------------------------------
// Easing functions
// ---------------------------------------------------------------------------

/// Easing function signature: maps `t` in [0, 1] to output in [0, 1].
pub type EasingFn = fn(f32) -> f32;

/// Identity easing (constant velocity).
#[inline]
pub fn linear(t: f32) -> f32 {
    t.clamp(0.0, 1.0)
}

/// Quadratic ease-in (slow start).
#[inline]
pub fn ease_in(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t
}

/// Quadratic ease-out (slow end).
#[inline]
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Quadratic ease-in-out (slow start and end).
#[inline]
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}

/// Cubic ease-in (slower start than quadratic).
#[inline]
pub fn ease_in_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t
}

/// Cubic ease-out (slower end than quadratic).
#[inline]
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

// ---------------------------------------------------------------------------
// Animation trait
// ---------------------------------------------------------------------------

/// A time-based animation producing values in [0.0, 1.0].
pub trait Animation {
    /// Advance the animation by `dt`.
    fn tick(&mut self, dt: Duration);

    /// Whether the animation has reached its end.
    fn is_complete(&self) -> bool;

    /// Current output value, clamped to [0.0, 1.0].
    fn value(&self) -> f32;

    /// Reset the animation to its initial state.
    fn reset(&mut self);

    /// Time elapsed past completion. Used by composition types to forward
    /// remaining time (e.g., [`Sequence`] forwards overshoot from first to second).
    /// Returns [`Duration::ZERO`] for animations that never complete.
    fn overshoot(&self) -> Duration {
        Duration::ZERO
    }
}

// ---------------------------------------------------------------------------
// Fade
// ---------------------------------------------------------------------------

/// Linear progression from 0.0 to 1.0 over a duration, with configurable easing.
///
/// Tracks elapsed time as [`Duration`] internally for precise accumulation
/// (no floating-point drift) and accurate overshoot calculation.
#[derive(Debug, Clone, Copy)]
pub struct Fade {
    elapsed: Duration,
    duration: Duration,
    easing: EasingFn,
}

impl Fade {
    /// Create a fade with the given duration and default linear easing.
    pub fn new(duration: Duration) -> Self {
        Self {
            elapsed: Duration::ZERO,
            duration: if duration.is_zero() {
                Duration::from_nanos(1)
            } else {
                duration
            },
            easing: linear,
        }
    }

    /// Set the easing function.
    pub fn easing(mut self, easing: EasingFn) -> Self {
        self.easing = easing;
        self
    }

    /// Raw linear progress (before easing), in [0.0, 1.0].
    pub fn raw_progress(&self) -> f32 {
        let t = self.elapsed.as_secs_f64() / self.duration.as_secs_f64();
        (t as f32).clamp(0.0, 1.0)
    }
}

impl Animation for Fade {
    fn tick(&mut self, dt: Duration) {
        self.elapsed = self.elapsed.saturating_add(dt);
    }

    fn is_complete(&self) -> bool {
        self.elapsed >= self.duration
    }

    fn value(&self) -> f32 {
        (self.easing)(self.raw_progress())
    }

    fn reset(&mut self) {
        self.elapsed = Duration::ZERO;
    }

    fn overshoot(&self) -> Duration {
        self.elapsed.saturating_sub(self.duration)
    }
}

// ---------------------------------------------------------------------------
// Slide
// ---------------------------------------------------------------------------

/// Interpolates an `i16` value between `from` and `to` over a duration.
///
/// [`Animation::value`] returns the normalized progress; use [`Slide::position`]
/// for the interpolated integer position.
#[derive(Debug, Clone, Copy)]
pub struct Slide {
    from: i16,
    to: i16,
    elapsed: Duration,
    duration: Duration,
    easing: EasingFn,
}

impl Slide {
    /// Create a new slide animation from `from` to `to` over `duration`.
    pub fn new(from: i16, to: i16, duration: Duration) -> Self {
        Self {
            from,
            to,
            elapsed: Duration::ZERO,
            duration: if duration.is_zero() {
                Duration::from_nanos(1)
            } else {
                duration
            },
            easing: ease_out,
        }
    }

    /// Set the easing function (builder).
    pub fn easing(mut self, easing: EasingFn) -> Self {
        self.easing = easing;
        self
    }

    fn progress(&self) -> f32 {
        let t = self.elapsed.as_secs_f64() / self.duration.as_secs_f64();
        (t as f32).clamp(0.0, 1.0)
    }

    /// Current interpolated position as an integer.
    pub fn position(&self) -> i16 {
        let t = (self.easing)(self.progress());
        let range = f32::from(self.to) - f32::from(self.from);
        let pos = f32::from(self.from) + range * t;
        pos.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
    }
}

impl Animation for Slide {
    fn tick(&mut self, dt: Duration) {
        self.elapsed = self.elapsed.saturating_add(dt);
    }

    fn is_complete(&self) -> bool {
        self.elapsed >= self.duration
    }

    fn value(&self) -> f32 {
        (self.easing)(self.progress())
    }

    fn reset(&mut self) {
        self.elapsed = Duration::ZERO;
    }

    fn overshoot(&self) -> Duration {
        self.elapsed.saturating_sub(self.duration)
    }
}

// ---------------------------------------------------------------------------
// Pulse
// ---------------------------------------------------------------------------

/// Continuous sine-wave oscillation. Never completes.
///
/// `value()` oscillates between 0.0 and 1.0 at the given frequency (Hz).
#[derive(Debug, Clone, Copy)]
pub struct Pulse {
    frequency: f32,
    phase: f32,
}

impl Pulse {
    /// Create a pulse at the given frequency in Hz.
    ///
    /// A frequency of 1.0 means one full cycle per second.
    pub fn new(frequency: f32) -> Self {
        Self {
            frequency: frequency.abs().max(f32::MIN_POSITIVE),
            phase: 0.0,
        }
    }

    /// Current phase in radians.
    pub fn phase(&self) -> f32 {
        self.phase
    }
}

impl Animation for Pulse {
    fn tick(&mut self, dt: Duration) {
        self.phase += std::f32::consts::TAU * self.frequency * dt.as_secs_f32();
        // Keep phase bounded to avoid precision loss over long runs.
        self.phase %= std::f32::consts::TAU;
    }

    fn is_complete(&self) -> bool {
        false // Pulses never complete.
    }

    fn value(&self) -> f32 {
        // Map sin output from [-1, 1] to [0, 1].
        (self.phase.sin() + 1.0) / 2.0
    }

    fn reset(&mut self) {
        self.phase = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Sequence
// ---------------------------------------------------------------------------

/// Play animation `A`, then animation `B`.
///
/// `value()` returns A's value while A is running, then B's value.
#[derive(Debug, Clone, Copy)]
pub struct Sequence<A, B> {
    first: A,
    second: B,
    first_done: bool,
}

impl<A: Animation, B: Animation> Sequence<A, B> {
    /// Create a new sequence that plays `first` then `second`.
    pub fn new(first: A, second: B) -> Self {
        Self {
            first,
            second,
            first_done: false,
        }
    }
}

impl<A: Animation, B: Animation> Animation for Sequence<A, B> {
    fn tick(&mut self, dt: Duration) {
        if !self.first_done {
            self.first.tick(dt);
            if self.first.is_complete() {
                self.first_done = true;
                // Forward any overshoot into the second animation.
                let os = self.first.overshoot();
                if !os.is_zero() {
                    self.second.tick(os);
                }
            }
        } else {
            self.second.tick(dt);
        }
    }

    fn is_complete(&self) -> bool {
        self.first_done && self.second.is_complete()
    }

    fn value(&self) -> f32 {
        if self.first_done {
            self.second.value()
        } else {
            self.first.value()
        }
    }

    fn reset(&mut self) {
        self.first.reset();
        self.second.reset();
        self.first_done = false;
    }

    fn overshoot(&self) -> Duration {
        if self.first_done {
            self.second.overshoot()
        } else {
            Duration::ZERO
        }
    }
}

// ---------------------------------------------------------------------------
// Parallel
// ---------------------------------------------------------------------------

/// Play animations `A` and `B` simultaneously.
///
/// `value()` returns the average of both values. Completes when both complete.
#[derive(Debug, Clone, Copy)]
pub struct Parallel<A, B> {
    a: A,
    b: B,
}

impl<A: Animation, B: Animation> Parallel<A, B> {
    /// Create a new parallel animation that plays `a` and `b` simultaneously.
    pub fn new(a: A, b: B) -> Self {
        Self { a, b }
    }

    /// Access the first animation.
    pub fn first(&self) -> &A {
        &self.a
    }

    /// Access the second animation.
    pub fn second(&self) -> &B {
        &self.b
    }
}

impl<A: Animation, B: Animation> Animation for Parallel<A, B> {
    fn tick(&mut self, dt: Duration) {
        if !self.a.is_complete() {
            self.a.tick(dt);
        }
        if !self.b.is_complete() {
            self.b.tick(dt);
        }
    }

    fn is_complete(&self) -> bool {
        self.a.is_complete() && self.b.is_complete()
    }

    fn value(&self) -> f32 {
        (self.a.value() + self.b.value()) / 2.0
    }

    fn reset(&mut self) {
        self.a.reset();
        self.b.reset();
    }
}

// ---------------------------------------------------------------------------
// Delayed
// ---------------------------------------------------------------------------

/// Wait for a delay, then play the inner animation.
#[derive(Debug, Clone, Copy)]
pub struct Delayed<A> {
    delay: Duration,
    elapsed: Duration,
    inner: A,
    started: bool,
}

impl<A: Animation> Delayed<A> {
    /// Create a delayed animation that waits `delay` before starting `inner`.
    pub fn new(delay: Duration, inner: A) -> Self {
        Self {
            delay,
            elapsed: Duration::ZERO,
            inner,
            started: false,
        }
    }

    /// Whether the delay period has elapsed and the inner animation has started.
    pub fn has_started(&self) -> bool {
        self.started
    }

    /// Access the inner animation.
    pub fn inner(&self) -> &A {
        &self.inner
    }
}

impl<A: Animation> Animation for Delayed<A> {
    fn tick(&mut self, dt: Duration) {
        if !self.started {
            self.elapsed = self.elapsed.saturating_add(dt);
            if self.elapsed >= self.delay {
                self.started = true;
                // Forward overshoot into the inner animation.
                let os = self.elapsed.saturating_sub(self.delay);
                if !os.is_zero() {
                    self.inner.tick(os);
                }
            }
        } else {
            self.inner.tick(dt);
        }
    }

    fn is_complete(&self) -> bool {
        self.started && self.inner.is_complete()
    }

    fn value(&self) -> f32 {
        if self.started {
            self.inner.value()
        } else {
            0.0
        }
    }

    fn reset(&mut self) {
        self.elapsed = Duration::ZERO;
        self.started = false;
        self.inner.reset();
    }

    fn overshoot(&self) -> Duration {
        if self.started {
            self.inner.overshoot()
        } else {
            Duration::ZERO
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

/// Create a [`Sequence`] from two animations.
pub fn sequence<A: Animation, B: Animation>(a: A, b: B) -> Sequence<A, B> {
    Sequence::new(a, b)
}

/// Create a [`Parallel`] pair from two animations.
pub fn parallel<A: Animation, B: Animation>(a: A, b: B) -> Parallel<A, B> {
    Parallel::new(a, b)
}

/// Create a [`Delayed`] animation.
pub fn delay<A: Animation>(d: Duration, a: A) -> Delayed<A> {
    Delayed::new(d, a)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const MS_16: Duration = Duration::from_millis(16);
    const MS_100: Duration = Duration::from_millis(100);
    const MS_500: Duration = Duration::from_millis(500);
    const SEC_1: Duration = Duration::from_secs(1);

    // ---- Easing tests ----

    #[test]
    fn easing_linear_endpoints() {
        assert!((linear(0.0) - 0.0).abs() < f32::EPSILON);
        assert!((linear(1.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn easing_linear_midpoint() {
        assert!((linear(0.5) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn easing_clamps_input() {
        assert!((linear(-1.0) - 0.0).abs() < f32::EPSILON);
        assert!((linear(2.0) - 1.0).abs() < f32::EPSILON);
        assert!((ease_in(-0.5) - 0.0).abs() < f32::EPSILON);
        assert!((ease_out(1.5) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn ease_in_slower_start() {
        // At t=0.5, ease_in should be less than linear
        assert!(ease_in(0.5) < linear(0.5));
    }

    #[test]
    fn ease_out_faster_start() {
        // At t=0.5, ease_out should be more than linear
        assert!(ease_out(0.5) > linear(0.5));
    }

    #[test]
    fn ease_in_out_endpoints() {
        assert!((ease_in_out(0.0) - 0.0).abs() < f32::EPSILON);
        assert!((ease_in_out(1.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn ease_in_out_midpoint() {
        assert!((ease_in_out(0.5) - 0.5).abs() < 0.01);
    }

    #[test]
    fn ease_in_cubic_endpoints() {
        assert!((ease_in_cubic(0.0) - 0.0).abs() < f32::EPSILON);
        assert!((ease_in_cubic(1.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn ease_out_cubic_endpoints() {
        assert!((ease_out_cubic(0.0) - 0.0).abs() < f32::EPSILON);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn ease_in_cubic_slower_than_quadratic() {
        assert!(ease_in_cubic(0.5) < ease_in(0.5));
    }

    // ---- Fade tests ----

    #[test]
    fn fade_starts_at_zero() {
        let fade = Fade::new(SEC_1);
        assert!((fade.value() - 0.0).abs() < f32::EPSILON);
        assert!(!fade.is_complete());
    }

    #[test]
    fn fade_completes_after_duration() {
        let mut fade = Fade::new(SEC_1);
        fade.tick(SEC_1);
        assert!(fade.is_complete());
        assert!((fade.value() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn fade_midpoint() {
        let mut fade = Fade::new(SEC_1);
        fade.tick(MS_500);
        assert!((fade.value() - 0.5).abs() < 0.01);
    }

    #[test]
    fn fade_incremental_ticks() {
        let mut fade = Fade::new(Duration::from_millis(160));
        for _ in 0..10 {
            fade.tick(MS_16);
        }
        assert!(fade.is_complete());
        assert!((fade.value() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn fade_with_ease_in() {
        let mut fade = Fade::new(SEC_1).easing(ease_in);
        fade.tick(MS_500);
        // ease_in at 0.5 = 0.25
        assert!((fade.value() - 0.25).abs() < 0.01);
    }

    #[test]
    fn fade_clamps_overshoot() {
        let mut fade = Fade::new(MS_100);
        fade.tick(SEC_1); // 10x the duration
        assert!(fade.is_complete());
        assert!((fade.value() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn fade_reset() {
        let mut fade = Fade::new(SEC_1);
        fade.tick(SEC_1);
        assert!(fade.is_complete());
        fade.reset();
        assert!(!fade.is_complete());
        assert!((fade.value() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn fade_zero_duration() {
        let mut fade = Fade::new(Duration::ZERO);
        // Should not panic; duration is clamped to MIN_POSITIVE
        fade.tick(MS_16);
        assert!(fade.is_complete());
    }

    #[test]
    fn fade_raw_progress() {
        let mut fade = Fade::new(SEC_1).easing(ease_in);
        fade.tick(MS_500);
        // Raw progress is 0.5, but value() is ease_in(0.5) = 0.25
        assert!((fade.raw_progress() - 0.5).abs() < 0.01);
        assert!((fade.value() - 0.25).abs() < 0.01);
    }

    // ---- Slide tests ----

    #[test]
    fn slide_starts_at_from() {
        let slide = Slide::new(0, 100, SEC_1);
        assert_eq!(slide.position(), 0);
    }

    #[test]
    fn slide_ends_at_to() {
        let mut slide = Slide::new(0, 100, SEC_1);
        slide.tick(SEC_1);
        assert_eq!(slide.position(), 100);
    }

    #[test]
    fn slide_negative_range() {
        let mut slide = Slide::new(100, -50, SEC_1).easing(linear);
        slide.tick(SEC_1);
        assert_eq!(slide.position(), -50);
    }

    #[test]
    fn slide_midpoint_with_linear() {
        let mut slide = Slide::new(0, 100, SEC_1).easing(linear);
        slide.tick(MS_500);
        assert_eq!(slide.position(), 50);
    }

    #[test]
    fn slide_reset() {
        let mut slide = Slide::new(10, 90, SEC_1);
        slide.tick(SEC_1);
        assert_eq!(slide.position(), 90);
        slide.reset();
        assert_eq!(slide.position(), 10);
    }

    // ---- Pulse tests ----

    #[test]
    fn pulse_starts_at_midpoint() {
        let pulse = Pulse::new(1.0);
        // sin(0) = 0, mapped to 0.5
        assert!((pulse.value() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn pulse_never_completes() {
        let mut pulse = Pulse::new(1.0);
        for _ in 0..100 {
            pulse.tick(MS_100);
        }
        assert!(!pulse.is_complete());
    }

    #[test]
    fn pulse_value_bounded() {
        let mut pulse = Pulse::new(2.0);
        for _ in 0..200 {
            pulse.tick(MS_16);
            let v = pulse.value();
            assert!((0.0..=1.0).contains(&v), "pulse value out of range: {v}");
        }
    }

    #[test]
    fn pulse_quarter_cycle_reaches_peak() {
        let mut pulse = Pulse::new(1.0);
        // Quarter cycle at 1Hz = 0.25s → sin(π/2) = 1 → value = 1.0
        pulse.tick(Duration::from_millis(250));
        assert!((pulse.value() - 1.0).abs() < 0.02);
    }

    #[test]
    fn pulse_phase_wraps() {
        let mut pulse = Pulse::new(1.0);
        pulse.tick(Duration::from_secs(10)); // Many full cycles
        // Phase should be bounded
        assert!(pulse.phase() < std::f32::consts::TAU);
    }

    #[test]
    fn pulse_reset() {
        let mut pulse = Pulse::new(1.0);
        pulse.tick(SEC_1);
        pulse.reset();
        assert!((pulse.phase() - 0.0).abs() < f32::EPSILON);
        assert!((pulse.value() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn pulse_zero_frequency_clamped() {
        let mut pulse = Pulse::new(0.0);
        // Should not panic; frequency clamped to MIN_POSITIVE
        pulse.tick(SEC_1);
        // Value should barely change
    }

    // ---- Sequence tests ----

    #[test]
    fn sequence_plays_first_then_second() {
        let a = Fade::new(SEC_1);
        let b = Fade::new(SEC_1);
        let mut seq = sequence(a, b);

        // First animation runs
        seq.tick(MS_500);
        assert!(!seq.is_complete());
        assert!((seq.value() - 0.5).abs() < 0.01);

        // First completes
        seq.tick(MS_500);
        // Now second should start
        assert!(!seq.is_complete());

        // Second runs
        seq.tick(MS_500);
        assert!((seq.value() - 0.5).abs() < 0.01);

        // Both complete
        seq.tick(MS_500);
        assert!(seq.is_complete());
        assert!((seq.value() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn sequence_reset() {
        let mut seq = sequence(Fade::new(MS_100), Fade::new(MS_100));
        seq.tick(Duration::from_millis(200));
        assert!(seq.is_complete());

        seq.reset();
        assert!(!seq.is_complete());
        assert!((seq.value() - 0.0).abs() < f32::EPSILON);
    }

    // ---- Parallel tests ----

    #[test]
    fn parallel_ticks_both() {
        let a = Fade::new(SEC_1);
        let b = Fade::new(Duration::from_millis(500));
        let mut par = parallel(a, b);

        par.tick(MS_500);
        // a at 0.5, b at 1.0 → average = 0.75
        assert!((par.value() - 0.75).abs() < 0.01);
        assert!(!par.is_complete()); // a not done yet

        par.tick(MS_500);
        assert!(par.is_complete());
    }

    #[test]
    fn parallel_access_components() {
        let par = parallel(Fade::new(SEC_1), Fade::new(SEC_1));
        assert!((par.first().value() - 0.0).abs() < f32::EPSILON);
        assert!((par.second().value() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn parallel_reset() {
        let mut par = parallel(Fade::new(MS_100), Fade::new(MS_100));
        par.tick(MS_100);
        assert!(par.is_complete());

        par.reset();
        assert!(!par.is_complete());
    }

    // ---- Delayed tests ----

    #[test]
    fn delayed_waits_then_plays() {
        let mut d = delay(MS_500, Fade::new(MS_500));

        // During delay: value is 0
        d.tick(Duration::from_millis(250));
        assert!(!d.has_started());
        assert!((d.value() - 0.0).abs() < f32::EPSILON);

        // Delay expires
        d.tick(Duration::from_millis(250));
        assert!(d.has_started());

        // Inner animation runs
        d.tick(MS_500);
        assert!(d.is_complete());
        assert!((d.value() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn delayed_forwards_overshoot() {
        let mut d = delay(MS_100, Fade::new(SEC_1));

        // Tick 200ms past a 100ms delay → inner should get ~100ms
        d.tick(Duration::from_millis(200));
        assert!(d.has_started());
        // Inner should be at ~0.1 (100ms of 1s)
        assert!((d.value() - 0.1).abs() < 0.02);
    }

    #[test]
    fn delayed_reset() {
        let mut d = delay(MS_100, Fade::new(MS_100));
        d.tick(Duration::from_millis(200));
        assert!(d.is_complete());

        d.reset();
        assert!(!d.has_started());
        assert!(!d.is_complete());
    }

    // ---- Composition tests ----

    #[test]
    fn nested_sequence() {
        let inner = sequence(Fade::new(MS_100), Fade::new(MS_100));
        let mut outer = sequence(inner, Fade::new(MS_100));

        outer.tick(Duration::from_millis(300));
        assert!(outer.is_complete());
    }

    #[test]
    fn delayed_parallel() {
        let a = delay(MS_100, Fade::new(MS_100));
        let b = Fade::new(Duration::from_millis(200));
        let mut par = parallel(a, b);

        par.tick(Duration::from_millis(200));
        assert!(par.is_complete());
    }

    #[test]
    fn parallel_of_sequences() {
        let s1 = sequence(Fade::new(MS_100), Fade::new(MS_100));
        let s2 = sequence(Fade::new(MS_100), Fade::new(MS_100));
        let mut par = parallel(s1, s2);

        par.tick(Duration::from_millis(200));
        assert!(par.is_complete());
    }

    // ---- Edge case tests ----

    #[test]
    fn zero_dt_is_noop() {
        let mut fade = Fade::new(SEC_1);
        fade.tick(Duration::ZERO);
        assert!((fade.value() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn very_small_dt() {
        let mut fade = Fade::new(SEC_1);
        fade.tick(Duration::from_nanos(1));
        // Should barely move, not panic
        assert!(fade.value() < 0.001);
    }

    #[test]
    fn very_large_dt() {
        let mut fade = Fade::new(MS_100);
        fade.tick(Duration::from_secs(3600));
        assert!(fade.is_complete());
        assert!((fade.value() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn rapid_small_ticks() {
        let mut fade = Fade::new(SEC_1);
        for _ in 0..1000 {
            fade.tick(Duration::from_millis(1));
        }
        assert!(fade.is_complete());
    }

    #[test]
    fn tick_after_complete_is_safe() {
        let mut fade = Fade::new(MS_100);
        fade.tick(SEC_1);
        assert!(fade.is_complete());
        // Extra ticks should not panic or produce values > 1.0
        fade.tick(SEC_1);
        assert!((fade.value() - 1.0).abs() < f32::EPSILON);
    }
}

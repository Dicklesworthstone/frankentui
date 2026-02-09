#![forbid(unsafe_code)]

//! Animation callbacks: event hooks at animation milestones.
//!
//! [`Callbacks`] wraps any [`Animation`] and tracks milestone events
//! (start, completion, progress thresholds) that can be polled via
//! [`drain_events`](Callbacks::drain_events).
//!
//! # Usage
//!
//! ```ignore
//! use std::time::Duration;
//! use ftui_core::animation::{Fade, callbacks::{Callbacks, AnimationEvent}};
//!
//! let mut anim = Callbacks::new(Fade::new(Duration::from_millis(500)))
//!     .on_start()
//!     .on_complete()
//!     .at_progress(0.5);
//!
//! anim.tick(Duration::from_millis(300));
//! for event in anim.drain_events() {
//!     match event {
//!         AnimationEvent::Started => { /* ... */ }
//!         AnimationEvent::Progress(pct) => { /* crossed 50% */ }
//!         AnimationEvent::Completed => { /* ... */ }
//!         _ => {}
//!     }
//! }
//! ```
//!
//! # Design
//!
//! Events are collected into an internal queue during `tick()` and drained
//! by the caller. This avoids closures/callbacks (which don't compose well
//! in Elm architectures) and keeps the API pure.
//!
//! # Invariants
//!
//! 1. `Started` fires at most once per play-through (after first `tick()`).
//! 2. `Completed` fires at most once (when `is_complete()` transitions to true).
//! 3. Progress thresholds fire at most once each, in ascending order.
//! 4. `drain_events()` clears the queue — events are not replayed.
//! 5. `reset()` resets all tracking state so events can fire again.
//!
//! # Failure Modes
//!
//! - Threshold out of range (< 0 or > 1): clamped to [0.0, 1.0].
//! - Duplicate thresholds: each fires independently.

use std::time::Duration;

use super::Animation;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// An event emitted by a [`Callbacks`]-wrapped animation.
#[derive(Debug, Clone, PartialEq)]
pub enum AnimationEvent {
    /// The animation received its first tick.
    Started,
    /// The animation crossed a progress threshold (value in [0.0, 1.0]).
    Progress(f32),
    /// The animation completed.
    Completed,
}

/// Configuration for which events to track.
#[derive(Debug, Clone, Default)]
struct EventConfig {
    on_start: bool,
    on_complete: bool,
    /// Sorted thresholds in [0.0, 1.0].
    thresholds: Vec<f32>,
}

/// Tracking state for fired events.
#[derive(Debug, Clone, Default)]
struct EventState {
    started_fired: bool,
    completed_fired: bool,
    /// Which thresholds have been crossed (parallel to config.thresholds).
    thresholds_fired: Vec<bool>,
}

/// An animation wrapper that emits events at milestones.
///
/// Wraps any `Animation` and queues [`AnimationEvent`]s during `tick()`.
/// Call [`drain_events`](Self::drain_events) to retrieve and clear them.
pub struct Callbacks<A> {
    inner: A,
    config: EventConfig,
    state: EventState,
    events: Vec<AnimationEvent>,
}

impl<A: std::fmt::Debug> std::fmt::Debug for Callbacks<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Callbacks")
            .field("inner", &self.inner)
            .field("pending_events", &self.events.len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

impl<A: Animation> Callbacks<A> {
    /// Wrap an animation with callback tracking.
    #[must_use]
    pub fn new(inner: A) -> Self {
        Self {
            inner,
            config: EventConfig::default(),
            state: EventState::default(),
            events: Vec::new(),
        }
    }

    /// Enable the `Started` event (builder pattern).
    #[must_use]
    pub fn on_start(mut self) -> Self {
        self.config.on_start = true;
        self
    }

    /// Enable the `Completed` event (builder pattern).
    #[must_use]
    pub fn on_complete(mut self) -> Self {
        self.config.on_complete = true;
        self
    }

    /// Add a progress threshold event (builder pattern).
    ///
    /// Fires when the animation's value crosses `threshold` (clamped to [0.0, 1.0]).
    #[must_use]
    pub fn at_progress(mut self, threshold: f32) -> Self {
        if !threshold.is_finite() {
            return self;
        }
        let clamped = threshold.clamp(0.0, 1.0);
        let idx = self
            .config
            .thresholds
            .partition_point(|&value| value <= clamped);
        self.config.thresholds.insert(idx, clamped);
        self.state.thresholds_fired.insert(idx, false);
        self
    }

    /// Access the inner animation.
    #[inline]
    #[must_use]
    pub fn inner(&self) -> &A {
        &self.inner
    }

    /// Mutable access to the inner animation.
    #[inline]
    pub fn inner_mut(&mut self) -> &mut A {
        &mut self.inner
    }

    /// Drain all pending events. Clears the event queue.
    pub fn drain_events(&mut self) -> Vec<AnimationEvent> {
        std::mem::take(&mut self.events)
    }

    /// Number of pending events.
    #[inline]
    #[must_use]
    pub fn pending_event_count(&self) -> usize {
        self.events.len()
    }

    /// Check events after a tick.
    fn check_events(&mut self) {
        let value = self.inner.value();

        // Started: fires on first tick.
        if self.config.on_start && !self.state.started_fired {
            self.state.started_fired = true;
            self.events.push(AnimationEvent::Started);
        }

        // Progress thresholds.
        for (i, &threshold) in self.config.thresholds.iter().enumerate() {
            if !self.state.thresholds_fired[i] && value >= threshold {
                self.state.thresholds_fired[i] = true;
                self.events.push(AnimationEvent::Progress(threshold));
            }
        }

        // Completed: fires when animation transitions to complete.
        if self.config.on_complete && !self.state.completed_fired && self.inner.is_complete() {
            self.state.completed_fired = true;
            self.events.push(AnimationEvent::Completed);
        }
    }
}

// ---------------------------------------------------------------------------
// Animation trait implementation
// ---------------------------------------------------------------------------

impl<A: Animation> Animation for Callbacks<A> {
    fn tick(&mut self, dt: Duration) {
        self.inner.tick(dt);
        self.check_events();
    }

    fn is_complete(&self) -> bool {
        self.inner.is_complete()
    }

    fn value(&self) -> f32 {
        self.inner.value()
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.state.started_fired = false;
        self.state.completed_fired = false;
        for fired in &mut self.state.thresholds_fired {
            *fired = false;
        }
        self.events.clear();
    }

    fn overshoot(&self) -> Duration {
        self.inner.overshoot()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::Fade;

    const MS_100: Duration = Duration::from_millis(100);
    const MS_250: Duration = Duration::from_millis(250);
    const MS_500: Duration = Duration::from_millis(500);
    const SEC_1: Duration = Duration::from_secs(1);

    #[test]
    fn no_events_configured() {
        let mut anim = Callbacks::new(Fade::new(SEC_1));
        anim.tick(MS_500);
        assert!(anim.drain_events().is_empty());
    }

    #[test]
    fn started_fires_on_first_tick() {
        let mut anim = Callbacks::new(Fade::new(SEC_1)).on_start();
        anim.tick(MS_100);
        let events = anim.drain_events();
        assert_eq!(events, vec![AnimationEvent::Started]);

        // Does not fire again.
        anim.tick(MS_100);
        assert!(anim.drain_events().is_empty());
    }

    #[test]
    fn completed_fires_when_done() {
        let mut anim = Callbacks::new(Fade::new(MS_500)).on_complete();
        anim.tick(MS_250);
        assert!(anim.drain_events().is_empty()); // Not complete yet.

        anim.tick(MS_500); // Past completion.
        let events = anim.drain_events();
        assert_eq!(events, vec![AnimationEvent::Completed]);

        // Does not fire again.
        anim.tick(MS_100);
        assert!(anim.drain_events().is_empty());
    }

    #[test]
    fn progress_threshold_fires_once() {
        let mut anim = Callbacks::new(Fade::new(SEC_1)).at_progress(0.5);
        anim.tick(MS_250);
        assert!(anim.drain_events().is_empty()); // At 25%.

        anim.tick(MS_500); // At 75%.
        let events = anim.drain_events();
        assert_eq!(events, vec![AnimationEvent::Progress(0.5)]);

        // Does not fire again.
        anim.tick(MS_250);
        assert!(anim.drain_events().is_empty());
    }

    #[test]
    fn multiple_thresholds() {
        let mut anim = Callbacks::new(Fade::new(SEC_1))
            .at_progress(0.25)
            .at_progress(0.75);

        anim.tick(MS_500); // At 50% — should cross 0.25.
        let events = anim.drain_events();
        assert_eq!(events, vec![AnimationEvent::Progress(0.25)]);

        anim.tick(MS_500); // At 100% — should cross 0.75.
        let events = anim.drain_events();
        assert_eq!(events, vec![AnimationEvent::Progress(0.75)]);
    }

    #[test]
    fn all_events_in_order() {
        let mut anim = Callbacks::new(Fade::new(MS_500))
            .on_start()
            .at_progress(0.5)
            .on_complete();

        anim.tick(MS_500); // Completes in one tick.
        let events = anim.drain_events();
        assert_eq!(
            events,
            vec![
                AnimationEvent::Started,
                AnimationEvent::Progress(0.5),
                AnimationEvent::Completed,
            ]
        );
    }

    #[test]
    fn reset_allows_events_to_fire_again() {
        let mut anim = Callbacks::new(Fade::new(MS_500)).on_start().on_complete();
        anim.tick(SEC_1);
        let _ = anim.drain_events();

        anim.reset();
        anim.tick(SEC_1);
        let events = anim.drain_events();
        assert_eq!(
            events,
            vec![AnimationEvent::Started, AnimationEvent::Completed]
        );
    }

    #[test]
    fn drain_clears_queue() {
        let mut anim = Callbacks::new(Fade::new(SEC_1)).on_start();
        anim.tick(MS_100);
        assert_eq!(anim.pending_event_count(), 1);

        let _ = anim.drain_events();
        assert_eq!(anim.pending_event_count(), 0);
    }

    #[test]
    fn inner_access() {
        let anim = Callbacks::new(Fade::new(SEC_1));
        assert!(!anim.inner().is_complete());
    }

    #[test]
    fn inner_mut_access() {
        let mut anim = Callbacks::new(Fade::new(SEC_1));
        anim.inner_mut().tick(SEC_1);
        assert!(anim.inner().is_complete());
    }

    #[test]
    fn animation_trait_value_delegates() {
        let mut anim = Callbacks::new(Fade::new(SEC_1));
        anim.tick(MS_500);
        assert!((anim.value() - 0.5).abs() < 0.02);
    }

    #[test]
    fn animation_trait_is_complete_delegates() {
        let mut anim = Callbacks::new(Fade::new(MS_100));
        assert!(!anim.is_complete());
        anim.tick(MS_100);
        assert!(anim.is_complete());
    }

    #[test]
    fn threshold_clamped() {
        let mut anim = Callbacks::new(Fade::new(SEC_1))
            .at_progress(-0.5) // Clamped to 0.0
            .at_progress(1.5); // Clamped to 1.0

        anim.tick(Duration::from_nanos(1)); // Barely started.
        let events = anim.drain_events();
        // 0.0 threshold should fire immediately.
        assert!(events.contains(&AnimationEvent::Progress(0.0)));
    }

    #[test]
    fn debug_format() {
        let anim = Callbacks::new(Fade::new(MS_100)).on_start();
        let dbg = format!("{:?}", anim);
        assert!(dbg.contains("Callbacks"));
        assert!(dbg.contains("pending_events"));
    }

    #[test]
    fn overshoot_delegates() {
        let mut anim = Callbacks::new(Fade::new(MS_100));
        anim.tick(MS_500);
        assert!(anim.overshoot() > Duration::ZERO);
    }
}

#![forbid(unsafe_code)]

//! Input macro recording and playback.
//!
//! Record terminal input events with timing information for deterministic
//! replay through the [`ProgramSimulator`](crate::simulator::ProgramSimulator).
//!
//! # Example
//!
//! ```ignore
//! use ftui_runtime::input_macro::{InputMacro, MacroRecorder, MacroPlayer};
//! use ftui_runtime::simulator::ProgramSimulator;
//! use ftui_core::event::Event;
//! use std::time::Duration;
//!
//! // Record events
//! let mut recorder = MacroRecorder::new("test_flow");
//! recorder.record_event(some_event.clone());
//! // ... time passes ...
//! recorder.record_event(another_event.clone());
//! let macro_recording = recorder.finish();
//!
//! // Replay through simulator
//! let mut sim = ProgramSimulator::new(my_model);
//! sim.init();
//! let mut player = MacroPlayer::new(&macro_recording);
//! player.replay_all(&mut sim);
//! ```

use ftui_core::event::Event;
use std::time::{Duration, Instant};

/// A recorded input event with timing relative to recording start.
#[derive(Debug, Clone)]
pub struct TimedEvent {
    /// The recorded event.
    pub event: Event,
    /// Delay from the previous event (or from recording start for the first event).
    pub delay: Duration,
}

impl TimedEvent {
    /// Create a new timed event with the given delay.
    pub fn new(event: Event, delay: Duration) -> Self {
        Self { event, delay }
    }

    /// Create a timed event with zero delay.
    pub fn immediate(event: Event) -> Self {
        Self {
            event,
            delay: Duration::ZERO,
        }
    }
}

/// Metadata about a recorded macro.
#[derive(Debug, Clone)]
pub struct MacroMetadata {
    /// Human-readable name for this macro.
    pub name: String,
    /// Terminal size at recording time.
    pub terminal_size: (u16, u16),
    /// Total duration of the recording.
    pub total_duration: Duration,
}

/// A recorded sequence of input events with timing.
///
/// An `InputMacro` captures events and their relative timing so they can
/// be replayed deterministically through a [`ProgramSimulator`](crate::simulator::ProgramSimulator).
#[derive(Debug, Clone)]
pub struct InputMacro {
    /// The recorded events with timing.
    events: Vec<TimedEvent>,
    /// Recording metadata.
    metadata: MacroMetadata,
}

impl InputMacro {
    /// Create a new macro from events and metadata.
    pub fn new(events: Vec<TimedEvent>, metadata: MacroMetadata) -> Self {
        Self { events, metadata }
    }

    /// Create a macro from events with no timing (all zero delay).
    ///
    /// Useful for building test macros programmatically.
    pub fn from_events(name: impl Into<String>, events: Vec<Event>) -> Self {
        let timed: Vec<TimedEvent> = events.into_iter().map(TimedEvent::immediate).collect();
        Self {
            metadata: MacroMetadata {
                name: name.into(),
                terminal_size: (80, 24),
                total_duration: Duration::ZERO,
            },
            events: timed,
        }
    }

    /// Get the recorded events.
    pub fn events(&self) -> &[TimedEvent] {
        &self.events
    }

    /// Get the metadata.
    #[inline]
    pub fn metadata(&self) -> &MacroMetadata {
        &self.metadata
    }

    /// Get the number of recorded events.
    #[inline]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Check if the macro has no events.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Get the total duration of the recording.
    #[inline]
    pub fn total_duration(&self) -> Duration {
        self.metadata.total_duration
    }

    /// Extract just the events (without timing) in order.
    pub fn bare_events(&self) -> Vec<Event> {
        self.events.iter().map(|te| te.event.clone()).collect()
    }

    /// Replay this macro through a simulator, honoring recorded delays.
    pub fn replay_with_timing<M: crate::program::Model>(
        &self,
        sim: &mut crate::simulator::ProgramSimulator<M>,
    ) {
        let mut player = MacroPlayer::new(self);
        player.replay_with_timing(sim);
    }

    /// Replay this macro through a simulator with a custom sleep function.
    ///
    /// Useful for tests that want deterministic timing without wall-clock sleep.
    pub fn replay_with_sleeper<M, F>(
        &self,
        sim: &mut crate::simulator::ProgramSimulator<M>,
        sleep: F,
    ) where
        M: crate::program::Model,
        F: FnMut(Duration),
    {
        let mut player = MacroPlayer::new(self);
        player.replay_with_sleeper(sim, sleep);
    }
}

/// Records input events with timing into an [`InputMacro`].
///
/// Call [`record_event`](Self::record_event) for each event, then
/// [`finish`](Self::finish) to produce the final macro.
pub struct MacroRecorder {
    name: String,
    terminal_size: (u16, u16),
    events: Vec<TimedEvent>,
    start_time: Instant,
    last_event_time: Instant,
}

impl MacroRecorder {
    /// Start a new recording session.
    pub fn new(name: impl Into<String>) -> Self {
        let now = Instant::now();
        Self {
            name: name.into(),
            terminal_size: (80, 24),
            events: Vec::new(),
            start_time: now,
            last_event_time: now,
        }
    }

    /// Set the terminal size metadata.
    #[must_use]
    pub fn with_terminal_size(mut self, width: u16, height: u16) -> Self {
        self.terminal_size = (width, height);
        self
    }

    /// Record an event at the current time.
    ///
    /// The delay is measured from the previous event (or recording start).
    pub fn record_event(&mut self, event: Event) {
        let now = Instant::now();
        let delay = now.duration_since(self.last_event_time);
        #[cfg(feature = "tracing")]
        tracing::debug!(event = ?event, delay = ?delay, "macro record event");
        self.events.push(TimedEvent::new(event, delay));
        self.last_event_time = now;
    }

    /// Record an event with an explicit delay from the previous event.
    pub fn record_event_with_delay(&mut self, event: Event, delay: Duration) {
        #[cfg(feature = "tracing")]
        tracing::debug!(event = ?event, delay = ?delay, "macro record event");
        self.events.push(TimedEvent::new(event, delay));
        // Advance the synthetic clock
        self.last_event_time += delay;
    }

    /// Get the number of events recorded so far.
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Finish recording and produce the macro.
    pub fn finish(self) -> InputMacro {
        let total_duration = self.last_event_time.duration_since(self.start_time);
        InputMacro {
            events: self.events,
            metadata: MacroMetadata {
                name: self.name,
                terminal_size: self.terminal_size,
                total_duration,
            },
        }
    }
}

/// Replays an [`InputMacro`] through a [`ProgramSimulator`].
///
/// Events are injected in order. Timing information is available
/// for inspection but does not cause real delays (the simulator
/// is deterministic and instant).
pub struct MacroPlayer<'a> {
    input_macro: &'a InputMacro,
    position: usize,
    elapsed: Duration,
}

impl<'a> MacroPlayer<'a> {
    /// Create a player for the given macro.
    pub fn new(input_macro: &'a InputMacro) -> Self {
        Self {
            input_macro,
            position: 0,
            elapsed: Duration::ZERO,
        }
    }

    /// Get current playback position (event index).
    pub fn position(&self) -> usize {
        self.position
    }

    /// Get elapsed virtual time.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Check if playback is complete.
    pub fn is_done(&self) -> bool {
        self.position >= self.input_macro.len()
    }

    /// Get the number of remaining events.
    pub fn remaining(&self) -> usize {
        self.input_macro.len().saturating_sub(self.position)
    }

    /// Step one event, injecting it into the simulator.
    ///
    /// Returns `true` if an event was played, `false` if playback is complete.
    pub fn step<M: crate::program::Model>(
        &mut self,
        sim: &mut crate::simulator::ProgramSimulator<M>,
    ) -> bool {
        if self.is_done() {
            return false;
        }

        let timed = &self.input_macro.events[self.position];
        #[cfg(feature = "tracing")]
        tracing::debug!(event = ?timed.event, delay = ?timed.delay, "macro playback event");
        self.elapsed += timed.delay;
        sim.inject_events(std::slice::from_ref(&timed.event));
        self.position += 1;
        true
    }

    /// Replay all remaining events into the simulator.
    ///
    /// Stops early if the simulator quits.
    pub fn replay_all<M: crate::program::Model>(
        &mut self,
        sim: &mut crate::simulator::ProgramSimulator<M>,
    ) {
        while !self.is_done() && sim.is_running() {
            self.step(sim);
        }
    }

    /// Replay all remaining events, honoring recorded delays.
    ///
    /// This uses real wall-clock sleeping for each recorded delay before
    /// injecting the event. Stops early if the simulator quits.
    pub fn replay_with_timing<M: crate::program::Model>(
        &mut self,
        sim: &mut crate::simulator::ProgramSimulator<M>,
    ) {
        self.replay_with_sleeper(sim, std::thread::sleep);
    }

    /// Replay all remaining events with a custom sleep function.
    ///
    /// Useful for tests that want to avoid real sleeping while still verifying
    /// the delay schedule.
    pub fn replay_with_sleeper<M, F>(
        &mut self,
        sim: &mut crate::simulator::ProgramSimulator<M>,
        mut sleep: F,
    ) where
        M: crate::program::Model,
        F: FnMut(Duration),
    {
        while !self.is_done() && sim.is_running() {
            let timed = &self.input_macro.events[self.position];
            if timed.delay > Duration::ZERO {
                sleep(timed.delay);
            }
            self.step(sim);
        }
    }

    /// Replay events up to the given virtual time.
    ///
    /// Only events whose cumulative delay is within `until` are played.
    pub fn replay_until<M: crate::program::Model>(
        &mut self,
        sim: &mut crate::simulator::ProgramSimulator<M>,
        until: Duration,
    ) {
        while !self.is_done() && sim.is_running() {
            let timed = &self.input_macro.events[self.position];
            let next_elapsed = self.elapsed + timed.delay;
            if next_elapsed > until {
                break;
            }
            self.step(sim);
        }
    }

    /// Reset playback to the beginning.
    pub fn reset(&mut self) {
        self.position = 0;
        self.elapsed = Duration::ZERO;
    }
}

// ---------------------------------------------------------------------------
// MacroPlayback – deterministic scheduler for live playback
// ---------------------------------------------------------------------------

/// Deterministic playback scheduler with speed and looping controls.
///
/// Invariants:
/// - Event order is preserved.
/// - `elapsed` is monotonic for a given `advance` sequence.
/// - No events are emitted without their cumulative delay being satisfied.
///
/// Failure modes:
/// - If total duration is zero and looping is enabled, looping is ignored to
///   avoid infinite emission within a single `advance` call.
#[derive(Debug, Clone)]
pub struct MacroPlayback {
    input_macro: InputMacro,
    position: usize,
    elapsed: Duration,
    next_due: Duration,
    speed: f64,
    looping: bool,
    start_logged: bool,
    stop_logged: bool,
    error_logged: bool,
}

impl MacroPlayback {
    /// Create a new playback scheduler for the given macro.
    pub fn new(input_macro: InputMacro) -> Self {
        let next_due = input_macro
            .events()
            .first()
            .map(|e| e.delay)
            .unwrap_or(Duration::ZERO);
        Self {
            input_macro,
            position: 0,
            elapsed: Duration::ZERO,
            next_due,
            speed: 1.0,
            looping: false,
            start_logged: false,
            stop_logged: false,
            error_logged: false,
        }
    }

    /// Set playback speed (must be finite and positive).
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = normalize_speed(speed);
    }

    /// Fluent speed setter.
    #[must_use]
    pub fn with_speed(mut self, speed: f64) -> Self {
        self.set_speed(speed);
        self
    }

    /// Enable or disable looping.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// Fluent looping setter.
    #[must_use]
    pub fn with_looping(mut self, looping: bool) -> Self {
        self.set_looping(looping);
        self
    }

    /// Get the current playback speed.
    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Get current playback position (event index).
    pub fn position(&self) -> usize {
        self.position
    }

    /// Get elapsed virtual time.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Check if playback is complete (non-looping).
    pub fn is_done(&self) -> bool {
        if self.input_macro.is_empty() {
            return true;
        }
        if self.looping && self.input_macro.total_duration() > Duration::ZERO {
            return false;
        }
        self.position >= self.input_macro.len()
    }

    /// Reset playback to the beginning.
    pub fn reset(&mut self) {
        self.position = 0;
        self.elapsed = Duration::ZERO;
        self.next_due = self
            .input_macro
            .events()
            .first()
            .map(|e| e.delay)
            .unwrap_or(Duration::ZERO);
        self.start_logged = false;
        self.stop_logged = false;
        self.error_logged = false;
    }

    /// Advance playback time and return any events now due.
    pub fn advance(&mut self, delta: Duration) -> Vec<Event> {
        if self.input_macro.is_empty() {
            #[cfg(feature = "tracing")]
            if !self.error_logged {
                let meta = self.input_macro.metadata();
                tracing::warn!(
                    macro_event = "playback_error",
                    reason = "macro_empty",
                    name = %meta.name,
                    events = 0usize,
                    duration_ms = self.input_macro.total_duration().as_millis() as u64,
                );
                self.error_logged = true;
            }
            return Vec::new();
        }
        if self.is_done() {
            return Vec::new();
        }

        #[cfg(feature = "tracing")]
        if !self.start_logged {
            let meta = self.input_macro.metadata();
            tracing::info!(
                macro_event = "playback_start",
                name = %meta.name,
                events = self.input_macro.len(),
                duration_ms = self.input_macro.total_duration().as_millis() as u64,
                speed = self.speed,
                looping = self.looping,
            );
            self.start_logged = true;
        }

        self.elapsed += scale_duration(delta, self.speed);
        let events = self.drain_due_events();

        #[cfg(feature = "tracing")]
        if self.is_done() && !self.stop_logged {
            let meta = self.input_macro.metadata();
            tracing::info!(
                macro_event = "playback_stop",
                reason = "completed",
                name = %meta.name,
                events = self.input_macro.len(),
                elapsed_ms = self.elapsed.as_millis() as u64,
                looping = self.looping,
            );
            self.stop_logged = true;
        }

        events
    }

    fn drain_due_events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        let total_duration = self.input_macro.total_duration();
        let can_loop = self.looping && total_duration > Duration::ZERO;
        if can_loop && self.position >= self.input_macro.len() {
            let total_secs = total_duration.as_secs_f64();
            if total_secs > 0.0 {
                let elapsed_secs = self.elapsed.as_secs_f64() % total_secs;
                self.elapsed = Duration::from_secs_f64(elapsed_secs);
            } else {
                self.elapsed = Duration::ZERO;
            }
            self.position = 0;
            self.next_due = self
                .input_macro
                .events()
                .first()
                .map(|e| e.delay)
                .unwrap_or(Duration::ZERO);
        }

        while self.position < self.input_macro.len() && self.elapsed >= self.next_due {
            let timed = &self.input_macro.events[self.position];
            #[cfg(feature = "tracing")]
            tracing::debug!(event = ?timed.event, delay = ?timed.delay, "macro playback event");
            out.push(timed.event.clone());
            self.position += 1;
            if self.position < self.input_macro.len() {
                self.next_due += self.input_macro.events[self.position].delay;
            } else if can_loop {
                // Carry any overflow elapsed time into the next loop.
                self.elapsed = self.elapsed.saturating_sub(total_duration);
                self.position = 0;
                self.next_due = self
                    .input_macro
                    .events()
                    .first()
                    .map(|e| e.delay)
                    .unwrap_or(Duration::ZERO);
            }
        }

        out
    }
}

fn normalize_speed(speed: f64) -> f64 {
    if !speed.is_finite() {
        return 1.0;
    }
    if speed <= 0.0 {
        return 0.0;
    }
    speed
}

fn scale_duration(delta: Duration, speed: f64) -> Duration {
    if delta == Duration::ZERO {
        return Duration::ZERO;
    }
    let speed = normalize_speed(speed);
    if speed == 1.0 {
        return delta;
    }
    Duration::from_secs_f64(delta.as_secs_f64() * speed)
}

// ---------------------------------------------------------------------------
// EventRecorder – live event stream recording with start/stop/pause
// ---------------------------------------------------------------------------

/// State of an [`EventRecorder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    /// Not yet started or has been stopped.
    Idle,
    /// Actively recording events.
    Recording,
    /// Temporarily paused (events are ignored).
    Paused,
}

/// Records events from a live event stream with start/stop/pause control.
///
/// This is a higher-level wrapper around [`MacroRecorder`] designed for
/// integration with the [`Program`](crate::program::Program) event loop.
///
/// # Usage
///
/// ```ignore
/// let mut recorder = EventRecorder::new("my_session");
/// recorder.start();
///
/// // In event loop:
/// for event in events {
///     recorder.record(&event);  // No-op if not recording
///     // ... process event normally ...
/// }
///
/// recorder.pause();
/// // ... events here are not recorded ...
/// recorder.resume();
///
/// let macro_recording = recorder.finish();
/// ```
pub struct EventRecorder {
    inner: MacroRecorder,
    state: RecordingState,
    pause_start: Option<Instant>,
    total_paused: Duration,
    event_count: usize,
}

impl EventRecorder {
    /// Create a new recorder with the given name.
    ///
    /// Starts in [`RecordingState::Idle`]. Call [`start`](Self::start)
    /// to begin recording.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            inner: MacroRecorder::new(name),
            state: RecordingState::Idle,
            pause_start: None,
            total_paused: Duration::ZERO,
            event_count: 0,
        }
    }

    /// Set the terminal size metadata.
    #[must_use]
    pub fn with_terminal_size(mut self, width: u16, height: u16) -> Self {
        self.inner = self.inner.with_terminal_size(width, height);
        self
    }

    /// Get the current recording state.
    pub fn state(&self) -> RecordingState {
        self.state
    }

    /// Check if actively recording (not idle or paused).
    pub fn is_recording(&self) -> bool {
        self.state == RecordingState::Recording
    }

    /// Start recording. No-op if already recording.
    pub fn start(&mut self) {
        match self.state {
            RecordingState::Idle => {
                self.state = RecordingState::Recording;
                #[cfg(feature = "tracing")]
                tracing::info!(
                    macro_event = "recorder_start",
                    name = %self.inner.name,
                    term_cols = self.inner.terminal_size.0,
                    term_rows = self.inner.terminal_size.1,
                );
            }
            RecordingState::Paused => {
                self.resume();
            }
            RecordingState::Recording => {} // Already recording
        }
    }

    /// Pause recording. Events received while paused are ignored.
    ///
    /// No-op if not recording.
    pub fn pause(&mut self) {
        if self.state == RecordingState::Recording {
            self.state = RecordingState::Paused;
            self.pause_start = Some(Instant::now());
        }
    }

    /// Resume recording after a pause.
    ///
    /// No-op if not paused.
    pub fn resume(&mut self) {
        if self.state == RecordingState::Paused {
            if let Some(pause_start) = self.pause_start.take() {
                self.total_paused += pause_start.elapsed();
            }
            // Reset the inner recorder's timestamp so the next event's
            // delay is measured from the resume instant, not from the
            // last event before the pause.
            self.inner.last_event_time = Instant::now();
            self.state = RecordingState::Recording;
        }
    }

    /// Record an event. Only records if state is [`RecordingState::Recording`].
    ///
    /// Returns `true` if the event was recorded.
    pub fn record(&mut self, event: &Event) -> bool {
        if self.state != RecordingState::Recording {
            return false;
        }
        self.inner.record_event(event.clone());
        self.event_count += 1;
        true
    }

    /// Record an event with an explicit delay override.
    ///
    /// Returns `true` if the event was recorded.
    pub fn record_with_delay(&mut self, event: &Event, delay: Duration) -> bool {
        if self.state != RecordingState::Recording {
            return false;
        }
        self.inner.record_event_with_delay(event.clone(), delay);
        self.event_count += 1;
        true
    }

    /// Get the number of events recorded so far.
    pub fn event_count(&self) -> usize {
        self.event_count
    }

    /// Get the total time spent paused.
    pub fn total_paused(&self) -> Duration {
        let mut total = self.total_paused;
        if let Some(pause_start) = self.pause_start {
            total += pause_start.elapsed();
        }
        total
    }

    /// Stop recording and produce the final [`InputMacro`].
    ///
    /// Consumes the recorder.
    pub fn finish(self) -> InputMacro {
        self.finish_internal(true)
    }

    #[allow(unused_variables)]
    fn finish_internal(self, log: bool) -> InputMacro {
        let paused = self.total_paused();
        let macro_data = self.inner.finish();
        #[cfg(feature = "tracing")]
        if log {
            let meta = macro_data.metadata();
            tracing::info!(
                macro_event = "recorder_stop",
                name = %meta.name,
                events = macro_data.len(),
                duration_ms = macro_data.total_duration().as_millis() as u64,
                paused_ms = paused.as_millis() as u64,
                term_cols = meta.terminal_size.0,
                term_rows = meta.terminal_size.1,
            );
        }
        macro_data
    }

    /// Stop recording and discard all events.
    ///
    /// Returns the number of events that were discarded.
    pub fn discard(self) -> usize {
        self.event_count
    }
}

/// Filter specification for recording.
///
/// Controls which events are recorded. Useful for excluding noise
/// events (like resize storms or mouse moves) from recordings.
#[derive(Debug, Clone)]
pub struct RecordingFilter {
    /// Record keyboard events.
    pub keys: bool,
    /// Record mouse events.
    pub mouse: bool,
    /// Record resize events.
    pub resize: bool,
    /// Record paste events.
    pub paste: bool,
    /// Record focus events.
    pub focus: bool,
}

impl Default for RecordingFilter {
    fn default() -> Self {
        Self {
            keys: true,
            mouse: true,
            resize: true,
            paste: true,
            focus: true,
        }
    }
}

impl RecordingFilter {
    /// Record only keyboard events.
    pub fn keys_only() -> Self {
        Self {
            keys: true,
            mouse: false,
            resize: false,
            paste: false,
            focus: false,
        }
    }

    /// Check if an event should be recorded.
    pub fn accepts(&self, event: &Event) -> bool {
        match event {
            Event::Key(_) => self.keys,
            Event::Mouse(_) => self.mouse,
            Event::Resize { .. } => self.resize,
            Event::Paste(_) => self.paste,
            Event::Focus(_) => self.focus,
            Event::Clipboard(_) => true, // Always record clipboard responses
            Event::Tick => false,        // Internal timing, not recorded
        }
    }
}

/// A filtered event recorder that only records events matching a filter.
pub struct FilteredEventRecorder {
    recorder: EventRecorder,
    filter: RecordingFilter,
    filtered_count: usize,
}

impl FilteredEventRecorder {
    /// Create a filtered recorder.
    pub fn new(name: impl Into<String>, filter: RecordingFilter) -> Self {
        Self {
            recorder: EventRecorder::new(name),
            filter,
            filtered_count: 0,
        }
    }

    /// Set terminal size metadata.
    #[must_use]
    pub fn with_terminal_size(mut self, width: u16, height: u16) -> Self {
        self.recorder = self.recorder.with_terminal_size(width, height);
        self
    }

    /// Start recording.
    pub fn start(&mut self) {
        self.recorder.start();
    }

    /// Pause recording.
    pub fn pause(&mut self) {
        self.recorder.pause();
    }

    /// Resume recording.
    pub fn resume(&mut self) {
        self.recorder.resume();
    }

    /// Get current state.
    pub fn state(&self) -> RecordingState {
        self.recorder.state()
    }

    /// Check if actively recording.
    pub fn is_recording(&self) -> bool {
        self.recorder.is_recording()
    }

    /// Record an event if it passes the filter.
    ///
    /// Returns `true` if the event was recorded (passed filter and recorder is active).
    pub fn record(&mut self, event: &Event) -> bool {
        if !self.filter.accepts(event) {
            self.filtered_count += 1;
            return false;
        }
        self.recorder.record(event)
    }

    /// Get the number of events that were filtered out.
    pub fn filtered_count(&self) -> usize {
        self.filtered_count
    }

    /// Get the number of events actually recorded.
    pub fn event_count(&self) -> usize {
        self.recorder.event_count()
    }

    /// Stop recording and produce the final macro.
    #[allow(unused_variables)]
    pub fn finish(self) -> InputMacro {
        let filtered = self.filtered_count;
        let paused = self.recorder.total_paused();
        let macro_data = self.recorder.finish_internal(false);
        #[cfg(feature = "tracing")]
        {
            let meta = macro_data.metadata();
            tracing::info!(
                macro_event = "recorder_stop",
                name = %meta.name,
                events = macro_data.len(),
                filtered,
                duration_ms = macro_data.total_duration().as_millis() as u64,
                paused_ms = paused.as_millis() as u64,
                term_cols = meta.terminal_size.0,
                term_rows = meta.terminal_size.1,
            );
        }
        macro_data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{Cmd, Model};
    use crate::simulator::ProgramSimulator;
    use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
    use ftui_render::frame::Frame;
    use proptest::prelude::*;

    // ---------- Test model ----------

    struct Counter {
        value: i32,
    }

    #[derive(Debug)]
    enum CounterMsg {
        Increment,
        Decrement,
        Quit,
    }

    impl From<Event> for CounterMsg {
        fn from(event: Event) -> Self {
            match event {
                Event::Key(k) if k.code == KeyCode::Char('+') => CounterMsg::Increment,
                Event::Key(k) if k.code == KeyCode::Char('-') => CounterMsg::Decrement,
                Event::Key(k) if k.code == KeyCode::Char('q') => CounterMsg::Quit,
                _ => CounterMsg::Increment,
            }
        }
    }

    impl Model for Counter {
        type Message = CounterMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                CounterMsg::Increment => {
                    self.value += 1;
                    Cmd::none()
                }
                CounterMsg::Decrement => {
                    self.value -= 1;
                    Cmd::none()
                }
                CounterMsg::Quit => Cmd::quit(),
            }
        }

        fn view(&self, _frame: &mut Frame) {}
    }

    fn key_event(c: char) -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    // ---------- TimedEvent tests ----------

    #[test]
    fn timed_event_immediate_has_zero_delay() {
        let te = TimedEvent::immediate(key_event('a'));
        assert_eq!(te.delay, Duration::ZERO);
    }

    #[test]
    fn timed_event_new_preserves_delay() {
        let delay = Duration::from_millis(100);
        let te = TimedEvent::new(key_event('x'), delay);
        assert_eq!(te.delay, delay);
    }

    // ---------- InputMacro tests ----------

    #[test]
    fn macro_from_events_has_zero_delays() {
        let m = InputMacro::from_events("test", vec![key_event('+'), key_event('-')]);
        assert_eq!(m.len(), 2);
        assert!(!m.is_empty());
        assert_eq!(m.total_duration(), Duration::ZERO);
        for te in m.events() {
            assert_eq!(te.delay, Duration::ZERO);
        }
    }

    #[test]
    fn macro_metadata() {
        let m = InputMacro::from_events("my_macro", vec![key_event('a')]);
        assert_eq!(m.metadata().name, "my_macro");
        assert_eq!(m.metadata().terminal_size, (80, 24));
    }

    #[test]
    fn empty_macro() {
        let m = InputMacro::from_events("empty", vec![]);
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn bare_events_extracts_events() {
        let events = vec![key_event('+'), key_event('-'), key_event('q')];
        let m = InputMacro::from_events("test", events.clone());
        let bare = m.bare_events();
        assert_eq!(bare.len(), 3);
        assert_eq!(bare, events);
    }

    // ---------- MacroRecorder tests ----------

    #[test]
    fn recorder_captures_events() {
        let mut rec = MacroRecorder::new("rec_test");
        rec.record_event(key_event('+'));
        rec.record_event(key_event('+'));
        rec.record_event(key_event('-'));
        assert_eq!(rec.event_count(), 3);

        let m = rec.finish();
        assert_eq!(m.len(), 3);
        assert_eq!(m.metadata().name, "rec_test");
    }

    #[test]
    fn recorder_with_terminal_size() {
        let rec = MacroRecorder::new("sized").with_terminal_size(120, 40);
        let m = rec.finish();
        assert_eq!(m.metadata().terminal_size, (120, 40));
    }

    #[test]
    fn recorder_explicit_delays() {
        let mut rec = MacroRecorder::new("delayed");
        rec.record_event_with_delay(key_event('+'), Duration::from_millis(0));
        rec.record_event_with_delay(key_event('-'), Duration::from_millis(50));
        rec.record_event_with_delay(key_event('q'), Duration::from_millis(100));

        let m = rec.finish();
        assert_eq!(m.events()[0].delay, Duration::from_millis(0));
        assert_eq!(m.events()[1].delay, Duration::from_millis(50));
        assert_eq!(m.events()[2].delay, Duration::from_millis(100));
    }

    // ---------- MacroPlayer tests ----------

    #[test]
    fn player_replays_all_events() {
        let m = InputMacro::from_events(
            "replay",
            vec![key_event('+'), key_event('+'), key_event('+')],
        );

        let mut sim = ProgramSimulator::new(Counter { value: 0 });
        sim.init();

        let mut player = MacroPlayer::new(&m);
        assert_eq!(player.remaining(), 3);
        assert!(!player.is_done());

        player.replay_all(&mut sim);

        assert!(player.is_done());
        assert_eq!(player.remaining(), 0);
        assert_eq!(sim.model().value, 3);
    }

    #[test]
    fn player_step_advances_position() {
        let m = InputMacro::from_events("step", vec![key_event('+'), key_event('+')]);

        let mut sim = ProgramSimulator::new(Counter { value: 0 });
        sim.init();

        let mut player = MacroPlayer::new(&m);
        assert_eq!(player.position(), 0);

        assert!(player.step(&mut sim));
        assert_eq!(player.position(), 1);
        assert_eq!(sim.model().value, 1);

        assert!(player.step(&mut sim));
        assert_eq!(player.position(), 2);
        assert_eq!(sim.model().value, 2);

        assert!(!player.step(&mut sim));
    }

    #[test]
    fn player_stops_on_quit() {
        let m = InputMacro::from_events(
            "quit_test",
            vec![key_event('+'), key_event('q'), key_event('+')],
        );

        let mut sim = ProgramSimulator::new(Counter { value: 0 });
        sim.init();

        let mut player = MacroPlayer::new(&m);
        player.replay_all(&mut sim);

        // Only increment and quit processed; third event skipped
        assert_eq!(sim.model().value, 1);
        assert!(!sim.is_running());
    }

    #[test]
    fn player_replay_until_respects_time() {
        let events = vec![
            TimedEvent::new(key_event('+'), Duration::from_millis(10)),
            TimedEvent::new(key_event('+'), Duration::from_millis(20)),
            TimedEvent::new(key_event('+'), Duration::from_millis(100)),
        ];
        let m = InputMacro::new(
            events,
            MacroMetadata {
                name: "timed".to_string(),
                terminal_size: (80, 24),
                total_duration: Duration::from_millis(130),
            },
        );

        let mut sim = ProgramSimulator::new(Counter { value: 0 });
        sim.init();

        let mut player = MacroPlayer::new(&m);

        // Play events up to 50ms: first two events (10ms + 20ms = 30ms)
        player.replay_until(&mut sim, Duration::from_millis(50));
        assert_eq!(sim.model().value, 2);
        assert_eq!(player.position(), 2);

        // Third event at 130ms, play until 200ms
        player.replay_until(&mut sim, Duration::from_millis(200));
        assert_eq!(sim.model().value, 3);
        assert!(player.is_done());
    }

    #[test]
    fn player_elapsed_tracks_virtual_time() {
        let events = vec![
            TimedEvent::new(key_event('+'), Duration::from_millis(10)),
            TimedEvent::new(key_event('+'), Duration::from_millis(20)),
        ];
        let m = InputMacro::new(
            events,
            MacroMetadata {
                name: "elapsed".to_string(),
                terminal_size: (80, 24),
                total_duration: Duration::from_millis(30),
            },
        );

        let mut sim = ProgramSimulator::new(Counter { value: 0 });
        sim.init();

        let mut player = MacroPlayer::new(&m);
        assert_eq!(player.elapsed(), Duration::ZERO);

        player.step(&mut sim);
        assert_eq!(player.elapsed(), Duration::from_millis(10));

        player.step(&mut sim);
        assert_eq!(player.elapsed(), Duration::from_millis(30));
    }

    #[test]
    fn player_reset_restarts_playback() {
        let m = InputMacro::from_events("reset", vec![key_event('+'), key_event('+')]);

        let mut sim = ProgramSimulator::new(Counter { value: 0 });
        sim.init();

        let mut player = MacroPlayer::new(&m);
        player.replay_all(&mut sim);
        assert_eq!(sim.model().value, 2);
        assert!(player.is_done());

        // Reset player and replay into fresh simulator
        player.reset();
        assert_eq!(player.position(), 0);
        assert!(!player.is_done());

        let mut sim2 = ProgramSimulator::new(Counter { value: 10 });
        sim2.init();
        player.replay_all(&mut sim2);
        assert_eq!(sim2.model().value, 12);
    }

    #[test]
    fn player_replay_with_sleeper_respects_delays() {
        let events = vec![
            TimedEvent::new(key_event('+'), Duration::from_millis(10)),
            TimedEvent::new(key_event('+'), Duration::from_millis(0)),
            TimedEvent::new(key_event('+'), Duration::from_millis(25)),
        ];
        let m = InputMacro::new(
            events,
            MacroMetadata {
                name: "timed_sleep".to_string(),
                terminal_size: (80, 24),
                total_duration: Duration::from_millis(35),
            },
        );

        let mut sim = ProgramSimulator::new(Counter { value: 0 });
        sim.init();

        let mut player = MacroPlayer::new(&m);
        let mut sleeps = Vec::new();
        player.replay_with_sleeper(&mut sim, |d| sleeps.push(d));

        assert_eq!(
            sleeps,
            vec![Duration::from_millis(10), Duration::from_millis(25)]
        );
        assert_eq!(sim.model().value, 3);
    }

    // ---------- MacroPlayback tests ----------

    #[test]
    fn playback_emits_due_events_in_order() {
        let events = vec![
            TimedEvent::new(key_event('+'), Duration::from_millis(10)),
            TimedEvent::new(key_event('+'), Duration::from_millis(10)),
        ];
        let m = InputMacro::new(
            events,
            MacroMetadata {
                name: "playback".to_string(),
                terminal_size: (80, 24),
                total_duration: Duration::from_millis(20),
            },
        );

        let mut playback = MacroPlayback::new(m.clone());
        assert!(playback.advance(Duration::from_millis(5)).is_empty());
        let first = playback.advance(Duration::from_millis(5));
        assert_eq!(first.len(), 1);
        let second = playback.advance(Duration::from_millis(10));
        assert_eq!(second.len(), 1);
        assert!(playback.advance(Duration::from_millis(10)).is_empty());
    }

    #[test]
    fn playback_speed_scales_time() {
        let events = vec![TimedEvent::new(key_event('+'), Duration::from_millis(10))];
        let m = InputMacro::new(
            events,
            MacroMetadata {
                name: "speed".to_string(),
                terminal_size: (80, 24),
                total_duration: Duration::from_millis(10),
            },
        );

        let mut playback = MacroPlayback::new(m.clone()).with_speed(2.0);
        let events = playback.advance(Duration::from_millis(5));
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn playback_looping_handles_large_delta() {
        let events = vec![
            TimedEvent::new(key_event('+'), Duration::from_millis(10)),
            TimedEvent::new(key_event('+'), Duration::from_millis(10)),
        ];
        let m = InputMacro::new(
            events,
            MacroMetadata {
                name: "loop".to_string(),
                terminal_size: (80, 24),
                total_duration: Duration::from_millis(20),
            },
        );

        let mut playback = MacroPlayback::new(m.clone()).with_looping(true);
        let events = playback.advance(Duration::from_millis(50));
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn playback_zero_duration_does_not_loop_forever() {
        let m = InputMacro::from_events("zero", vec![key_event('+'), key_event('+')]);
        let mut playback = MacroPlayback::new(m.clone()).with_looping(true);

        let events = playback.advance(Duration::ZERO);
        assert_eq!(events.len(), 2);
        assert!(playback.advance(Duration::from_millis(10)).is_empty());
    }

    #[test]
    fn macro_replay_with_sleeper_wrapper() {
        let events = vec![
            TimedEvent::new(key_event('+'), Duration::from_millis(5)),
            TimedEvent::new(key_event('+'), Duration::from_millis(10)),
        ];
        let m = InputMacro::new(
            events,
            MacroMetadata {
                name: "wrapper".to_string(),
                terminal_size: (80, 24),
                total_duration: Duration::from_millis(15),
            },
        );

        let mut sim = ProgramSimulator::new(Counter { value: 0 });
        sim.init();

        let mut slept = Vec::new();
        m.replay_with_sleeper(&mut sim, |d| slept.push(d));

        assert_eq!(
            slept,
            vec![Duration::from_millis(5), Duration::from_millis(10)]
        );
        assert_eq!(sim.model().value, 2);
    }

    #[test]
    fn empty_macro_replay() {
        let m = InputMacro::from_events("empty", vec![]);

        let mut sim = ProgramSimulator::new(Counter { value: 5 });
        sim.init();

        let mut player = MacroPlayer::new(&m);
        assert!(player.is_done());
        player.replay_all(&mut sim);
        assert_eq!(sim.model().value, 5);
    }

    #[test]
    fn macro_with_mixed_events() {
        let events = vec![
            key_event('+'),
            Event::Resize {
                width: 100,
                height: 50,
            },
            key_event('-'),
            Event::Focus(true),
            key_event('+'),
        ];
        let m = InputMacro::from_events("mixed", events);

        let mut sim = ProgramSimulator::new(Counter { value: 0 });
        sim.init();

        let mut player = MacroPlayer::new(&m);
        player.replay_all(&mut sim);

        // +1, resize->increment, -1, focus->increment, +1 = 3
        // (Counter converts all non-matching events to Increment)
        assert_eq!(sim.model().value, 3);
    }

    #[test]
    fn deterministic_replay() {
        let m = InputMacro::from_events(
            "determinism",
            vec![
                key_event('+'),
                key_event('+'),
                key_event('-'),
                key_event('+'),
                key_event('+'),
            ],
        );

        // Replay twice and verify identical results
        let result1 = {
            let mut sim = ProgramSimulator::new(Counter { value: 0 });
            sim.init();
            MacroPlayer::new(&m).replay_all(&mut sim);
            sim.model().value
        };

        let result2 = {
            let mut sim = ProgramSimulator::new(Counter { value: 0 });
            sim.init();
            MacroPlayer::new(&m).replay_all(&mut sim);
            sim.model().value
        };

        assert_eq!(result1, result2);
        assert_eq!(result1, 3);
    }

    // ---------- EventRecorder tests ----------

    #[test]
    fn event_recorder_starts_idle() {
        let rec = EventRecorder::new("test");
        assert_eq!(rec.state(), RecordingState::Idle);
        assert!(!rec.is_recording());
        assert_eq!(rec.event_count(), 0);
    }

    #[test]
    fn event_recorder_start_activates() {
        let mut rec = EventRecorder::new("test");
        rec.start();
        assert_eq!(rec.state(), RecordingState::Recording);
        assert!(rec.is_recording());
    }

    #[test]
    fn event_recorder_ignores_events_when_idle() {
        let mut rec = EventRecorder::new("test");
        assert!(!rec.record(&key_event('a')));
        assert_eq!(rec.event_count(), 0);
    }

    #[test]
    fn event_recorder_records_when_active() {
        let mut rec = EventRecorder::new("test");
        rec.start();
        assert!(rec.record(&key_event('a')));
        assert!(rec.record(&key_event('b')));
        assert_eq!(rec.event_count(), 2);

        let m = rec.finish();
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn event_recorder_pause_ignores_events() {
        let mut rec = EventRecorder::new("test");
        rec.start();
        rec.record(&key_event('a'));
        rec.pause();
        assert_eq!(rec.state(), RecordingState::Paused);
        assert!(!rec.is_recording());

        // Events during pause are ignored
        assert!(!rec.record(&key_event('b')));
        assert_eq!(rec.event_count(), 1);
    }

    #[test]
    fn event_recorder_resume_after_pause() {
        let mut rec = EventRecorder::new("test");
        rec.start();
        rec.record(&key_event('a'));
        rec.pause();
        rec.record(&key_event('b')); // ignored
        rec.resume();
        assert!(rec.is_recording());
        rec.record(&key_event('c'));
        assert_eq!(rec.event_count(), 2);

        let m = rec.finish();
        assert_eq!(m.len(), 2);
        assert_eq!(m.bare_events()[0], key_event('a'));
        assert_eq!(m.bare_events()[1], key_event('c'));
    }

    #[test]
    fn event_recorder_start_resumes_when_paused() {
        let mut rec = EventRecorder::new("test");
        rec.start();
        rec.pause();
        assert_eq!(rec.state(), RecordingState::Paused);

        rec.start(); // Should resume
        assert_eq!(rec.state(), RecordingState::Recording);
    }

    #[test]
    fn event_recorder_pause_noop_when_idle() {
        let mut rec = EventRecorder::new("test");
        rec.pause();
        assert_eq!(rec.state(), RecordingState::Idle);
    }

    #[test]
    fn event_recorder_resume_noop_when_idle() {
        let mut rec = EventRecorder::new("test");
        rec.resume();
        assert_eq!(rec.state(), RecordingState::Idle);
    }

    #[test]
    fn event_recorder_discard() {
        let mut rec = EventRecorder::new("test");
        rec.start();
        rec.record(&key_event('a'));
        rec.record(&key_event('b'));
        let count = rec.discard();
        assert_eq!(count, 2);
    }

    #[test]
    fn event_recorder_with_terminal_size() {
        let mut rec = EventRecorder::new("sized").with_terminal_size(120, 40);
        rec.start();
        rec.record(&key_event('x'));
        let m = rec.finish();
        assert_eq!(m.metadata().terminal_size, (120, 40));
    }

    #[test]
    fn event_recorder_finish_produces_valid_macro() {
        let mut rec = EventRecorder::new("full_test");
        rec.start();
        rec.record(&key_event('+'));
        rec.record(&key_event('+'));
        rec.record(&key_event('-'));

        let m = rec.finish();
        assert_eq!(m.len(), 3);
        assert_eq!(m.metadata().name, "full_test");

        // Replay and verify
        let mut sim = ProgramSimulator::new(Counter { value: 0 });
        sim.init();
        MacroPlayer::new(&m).replay_all(&mut sim);
        assert_eq!(sim.model().value, 1); // +1 +1 -1 = 1
    }

    #[test]
    fn event_recorder_record_with_delay() {
        let mut rec = EventRecorder::new("delayed");
        rec.start();
        assert!(rec.record_with_delay(&key_event('a'), Duration::from_millis(50)));
        assert!(rec.record_with_delay(&key_event('b'), Duration::from_millis(100)));
        assert_eq!(rec.event_count(), 2);

        let m = rec.finish();
        assert_eq!(m.events()[0].delay, Duration::from_millis(50));
        assert_eq!(m.events()[1].delay, Duration::from_millis(100));
    }

    #[test]
    fn event_recorder_record_with_delay_ignores_when_idle() {
        let mut rec = EventRecorder::new("test");
        assert!(!rec.record_with_delay(&key_event('a'), Duration::from_millis(50)));
        assert_eq!(rec.event_count(), 0);
    }

    // ---------- RecordingFilter tests ----------

    #[test]
    fn filter_default_accepts_all() {
        let filter = RecordingFilter::default();
        assert!(filter.accepts(&key_event('a')));
        assert!(filter.accepts(&Event::Resize {
            width: 80,
            height: 24
        }));
        assert!(filter.accepts(&Event::Focus(true)));
    }

    #[test]
    fn filter_keys_only() {
        let filter = RecordingFilter::keys_only();
        assert!(filter.accepts(&key_event('a')));
        assert!(!filter.accepts(&Event::Resize {
            width: 80,
            height: 24
        }));
        assert!(!filter.accepts(&Event::Focus(true)));
    }

    #[test]
    fn filter_custom() {
        let filter = RecordingFilter {
            keys: true,
            mouse: false,
            resize: false,
            paste: true,
            focus: false,
        };
        assert!(filter.accepts(&key_event('a')));
        assert!(!filter.accepts(&Event::Resize {
            width: 80,
            height: 24
        }));
        assert!(!filter.accepts(&Event::Focus(false)));
    }

    // ---------- FilteredEventRecorder tests ----------

    #[test]
    fn filtered_recorder_records_matching_events() {
        let mut rec = FilteredEventRecorder::new("filtered", RecordingFilter::default());
        rec.start();
        assert!(rec.record(&key_event('a')));
        assert_eq!(rec.event_count(), 1);
        assert_eq!(rec.filtered_count(), 0);
    }

    #[test]
    fn filtered_recorder_skips_filtered_events() {
        let mut rec = FilteredEventRecorder::new("keys_only", RecordingFilter::keys_only());
        rec.start();
        assert!(rec.record(&key_event('a')));
        assert!(!rec.record(&Event::Focus(true)));
        assert!(!rec.record(&Event::Resize {
            width: 100,
            height: 50
        }));
        assert!(rec.record(&key_event('b')));

        assert_eq!(rec.event_count(), 2);
        assert_eq!(rec.filtered_count(), 2);
    }

    #[test]
    fn filtered_recorder_finish_produces_macro() {
        let mut rec = FilteredEventRecorder::new("test", RecordingFilter::keys_only());
        rec.start();
        rec.record(&key_event('+'));
        rec.record(&Event::Focus(true)); // filtered
        rec.record(&key_event('+'));

        let m = rec.finish();
        assert_eq!(m.len(), 2);

        let mut sim = ProgramSimulator::new(Counter { value: 0 });
        sim.init();
        MacroPlayer::new(&m).replay_all(&mut sim);
        assert_eq!(sim.model().value, 2);
    }

    #[test]
    fn filtered_recorder_pause_resume() {
        let mut rec = FilteredEventRecorder::new("test", RecordingFilter::default());
        rec.start();
        rec.record(&key_event('a'));
        rec.pause();
        assert!(!rec.record(&key_event('b'))); // paused
        rec.resume();
        rec.record(&key_event('c'));
        assert_eq!(rec.event_count(), 2);
    }

    #[test]
    fn filtered_recorder_with_terminal_size() {
        let mut rec = FilteredEventRecorder::new("sized", RecordingFilter::default())
            .with_terminal_size(200, 60);
        rec.start();
        rec.record(&key_event('x'));
        let m = rec.finish();
        assert_eq!(m.metadata().terminal_size, (200, 60));
    }

    // ---------- Property tests ----------

    #[derive(Default)]
    struct EventSink {
        events: Vec<Event>,
    }

    #[derive(Debug, Clone)]
    struct EventMsg(Event);

    impl From<Event> for EventMsg {
        fn from(event: Event) -> Self {
            Self(event)
        }
    }

    impl Model for EventSink {
        type Message = EventMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            self.events.push(msg.0);
            Cmd::none()
        }

        fn view(&self, _frame: &mut Frame) {}
    }

    proptest! {
        #[test]
        fn recorder_with_explicit_delays_roundtrips(pairs in proptest::collection::vec((0u8..=25, 0u16..=2000), 0..32)) {
            let mut recorder = MacroRecorder::new("prop").with_terminal_size(80, 24);
            let mut expected_total = Duration::ZERO;
            let mut expected_events = Vec::with_capacity(pairs.len());

            for (ch_idx, delay_ms) in &pairs {
                let ch = char::from(b'a' + *ch_idx);
                let delay = Duration::from_millis(*delay_ms as u64);
                expected_total += delay;
                let ev = key_event(ch);
                expected_events.push(ev.clone());
                recorder.record_event_with_delay(ev, delay);
            }

            let m = recorder.finish();
            prop_assert_eq!(m.len(), pairs.len());
            prop_assert_eq!(m.metadata().terminal_size, (80, 24));
            prop_assert_eq!(m.total_duration(), expected_total);
            prop_assert_eq!(m.bare_events(), expected_events);
        }

        #[test]
        fn player_replays_events_in_order(pairs in proptest::collection::vec((0u8..=25, 0u16..=2000), 0..32)) {
            let mut timed = Vec::with_capacity(pairs.len());
            let mut total = Duration::ZERO;
            let mut expected_events = Vec::with_capacity(pairs.len());

            for (ch_idx, delay_ms) in &pairs {
                let ch = char::from(b'a' + *ch_idx);
                let delay = Duration::from_millis(*delay_ms as u64);
                total += delay;
                let ev = key_event(ch);
                expected_events.push(ev.clone());
                timed.push(TimedEvent::new(ev, delay));
            }

            let m = InputMacro::new(timed, MacroMetadata {
                name: "prop".to_string(),
                terminal_size: (80, 24),
                total_duration: total,
            });

            let mut sim = ProgramSimulator::new(EventSink::default());
            sim.init();
            let mut player = MacroPlayer::new(&m);
            player.replay_all(&mut sim);

            prop_assert_eq!(sim.model().events.clone(), expected_events);
            prop_assert_eq!(player.elapsed(), total);
        }
    }
}

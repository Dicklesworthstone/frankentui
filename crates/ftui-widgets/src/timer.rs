#![forbid(unsafe_code)]

//! Countdown timer widget.
//!
//! Provides a [`Timer`] widget that renders remaining time, and a
//! [`TimerState`] that counts down from a set duration with start/stop/reset
//! semantics.
//!
//! # Example
//!
//! ```rust
//! use ftui_widgets::timer::{Timer, TimerState};
//! use std::time::Duration;
//!
//! let mut state = TimerState::new(Duration::from_secs(60));
//! assert_eq!(state.remaining(), Duration::from_secs(60));
//! assert!(!state.finished());
//!
//! state.start();
//! state.tick(Duration::from_secs(1));
//! assert_eq!(state.remaining(), Duration::from_secs(59));
//! ```

use crate::{StatefulWidget, Widget, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;

// Re-use format types from stopwatch.
pub use crate::stopwatch::StopwatchFormat as TimerFormat;

/// State for the countdown timer.
#[derive(Debug, Clone)]
pub struct TimerState {
    duration: std::time::Duration,
    remaining: std::time::Duration,
    running: bool,
}

impl TimerState {
    /// Creates a new timer with the given countdown duration, initially stopped.
    pub fn new(duration: std::time::Duration) -> Self {
        Self {
            duration,
            remaining: duration,
            running: false,
        }
    }

    /// Returns the original countdown duration.
    pub fn duration(&self) -> std::time::Duration {
        self.duration
    }

    /// Returns the remaining time.
    pub fn remaining(&self) -> std::time::Duration {
        self.remaining
    }

    /// Returns whether the timer is currently running.
    pub fn running(&self) -> bool {
        self.running && !self.finished()
    }

    /// Returns whether the timer has reached zero.
    pub fn finished(&self) -> bool {
        self.remaining.is_zero()
    }

    /// Starts the timer.
    pub fn start(&mut self) {
        self.running = true;
    }

    /// Stops (pauses) the timer.
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Toggles between running and stopped.
    pub fn toggle(&mut self) {
        self.running = !self.running;
    }

    /// Resets the timer to its original duration. Does not change running state.
    pub fn reset(&mut self) {
        self.remaining = self.duration;
    }

    /// Sets a new countdown duration and resets remaining time.
    pub fn set_duration(&mut self, duration: std::time::Duration) {
        self.duration = duration;
        self.remaining = duration;
    }

    /// Subtracts delta from remaining time if running.
    /// Returns `true` if the tick was applied.
    pub fn tick(&mut self, delta: std::time::Duration) -> bool {
        if self.running && !self.finished() {
            self.remaining = self.remaining.saturating_sub(delta);
            true
        } else {
            false
        }
    }
}

/// A widget that displays remaining time from a [`TimerState`].
#[derive(Debug, Clone, Default)]
pub struct Timer<'a> {
    format: TimerFormat,
    style: Style,
    running_style: Option<Style>,
    finished_style: Option<Style>,
    label: Option<&'a str>,
}

impl<'a> Timer<'a> {
    /// Creates a new timer widget with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the display format.
    pub fn format(mut self, format: TimerFormat) -> Self {
        self.format = format;
        self
    }

    /// Sets the base style.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Sets a style override used while the timer is running.
    pub fn running_style(mut self, style: Style) -> Self {
        self.running_style = Some(style);
        self
    }

    /// Sets a style override used when the timer has finished.
    pub fn finished_style(mut self, style: Style) -> Self {
        self.finished_style = Some(style);
        self
    }

    /// Sets an optional label rendered before the time.
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }
}

impl StatefulWidget for Timer<'_> {
    type State = TimerState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        if area.is_empty() || area.height == 0 {
            return;
        }

        let deg = frame.buffer.degradation;
        if !deg.render_content() {
            return;
        }

        let style = if deg.apply_styling() {
            if state.finished() {
                self.finished_style.unwrap_or(self.style)
            } else if state.running() {
                self.running_style.unwrap_or(self.style)
            } else {
                self.style
            }
        } else {
            Style::default()
        };

        let formatted = crate::stopwatch::format_duration(state.remaining, self.format);
        let mut x = area.x;

        if let Some(label) = self.label {
            x = draw_text_span(frame, x, area.y, label, style, area.right());
            if x < area.right() {
                x = draw_text_span(frame, x, area.y, " ", style, area.right());
            }
        }

        draw_text_span(frame, x, area.y, &formatted, style, area.right());
    }
}

impl Widget for Timer<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let mut state = TimerState::new(std::time::Duration::ZERO);
        StatefulWidget::render(self, area, frame, &mut state);
    }

    fn is_essential(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::buffer::Buffer;
    use ftui_render::grapheme_pool::GraphemePool;
    use std::time::Duration;

    fn cell_char(buf: &Buffer, x: u16, y: u16) -> Option<char> {
        buf.get(x, y).and_then(|c| c.content.as_char())
    }

    fn render_to_string(widget: &Timer, state: &mut TimerState, width: u16) -> String {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(width, 1, &mut pool);
        let area = Rect::new(0, 0, width, 1);
        StatefulWidget::render(widget, area, &mut frame, state);
        (0..width)
            .filter_map(|x| cell_char(&frame.buffer, x, 0))
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    // --- TimerState tests ---

    #[test]
    fn state_new() {
        let state = TimerState::new(Duration::from_secs(60));
        assert_eq!(state.duration(), Duration::from_secs(60));
        assert_eq!(state.remaining(), Duration::from_secs(60));
        assert!(!state.running());
        assert!(!state.finished());
    }

    #[test]
    fn state_start_stop() {
        let mut state = TimerState::new(Duration::from_secs(10));
        state.start();
        assert!(state.running());
        state.stop();
        assert!(!state.running());
    }

    #[test]
    fn state_toggle() {
        let mut state = TimerState::new(Duration::from_secs(10));
        state.toggle();
        assert!(state.running());
        state.toggle();
        assert!(!state.running());
    }

    #[test]
    fn state_tick_counts_down() {
        let mut state = TimerState::new(Duration::from_secs(10));
        state.start();
        assert!(state.tick(Duration::from_secs(3)));
        assert_eq!(state.remaining(), Duration::from_secs(7));
    }

    #[test]
    fn state_tick_when_stopped_is_noop() {
        let mut state = TimerState::new(Duration::from_secs(10));
        assert!(!state.tick(Duration::from_secs(1)));
        assert_eq!(state.remaining(), Duration::from_secs(10));
    }

    #[test]
    fn state_tick_saturates_at_zero() {
        let mut state = TimerState::new(Duration::from_secs(2));
        state.start();
        state.tick(Duration::from_secs(5));
        assert_eq!(state.remaining(), Duration::ZERO);
        assert!(state.finished());
    }

    #[test]
    fn state_finished_stops_running() {
        let mut state = TimerState::new(Duration::from_secs(1));
        state.start();
        state.tick(Duration::from_secs(1));
        assert!(state.finished());
        assert!(!state.running()); // running() returns false when finished
    }

    #[test]
    fn state_tick_after_finished_is_noop() {
        let mut state = TimerState::new(Duration::from_secs(1));
        state.start();
        state.tick(Duration::from_secs(1));
        assert!(!state.tick(Duration::from_secs(1)));
        assert_eq!(state.remaining(), Duration::ZERO);
    }

    #[test]
    fn state_reset() {
        let mut state = TimerState::new(Duration::from_secs(60));
        state.start();
        state.tick(Duration::from_secs(30));
        state.reset();
        assert_eq!(state.remaining(), Duration::from_secs(60));
    }

    #[test]
    fn state_set_duration() {
        let mut state = TimerState::new(Duration::from_secs(60));
        state.start();
        state.tick(Duration::from_secs(10));
        state.set_duration(Duration::from_secs(120));
        assert_eq!(state.duration(), Duration::from_secs(120));
        assert_eq!(state.remaining(), Duration::from_secs(120));
    }

    #[test]
    fn state_zero_duration_is_finished() {
        let state = TimerState::new(Duration::ZERO);
        assert!(state.finished());
        assert!(!state.running());
    }

    // --- Widget rendering tests ---

    #[test]
    fn render_zero_area() {
        let widget = Timer::new();
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let mut state = TimerState::new(Duration::from_secs(60));
        StatefulWidget::render(&widget, area, &mut frame, &mut state);
    }

    #[test]
    fn render_remaining_human() {
        let widget = Timer::new();
        let mut state = TimerState::new(Duration::from_secs(125));
        let text = render_to_string(&widget, &mut state, 20);
        assert_eq!(text, "2m5s");
    }

    #[test]
    fn render_digital_format() {
        let widget = Timer::new().format(TimerFormat::Digital);
        let mut state = TimerState::new(Duration::from_secs(3665));
        let text = render_to_string(&widget, &mut state, 20);
        assert_eq!(text, "01:01:05");
    }

    #[test]
    fn render_seconds_format() {
        let widget = Timer::new().format(TimerFormat::Seconds);
        let mut state = TimerState::new(Duration::from_secs(90));
        let text = render_to_string(&widget, &mut state, 20);
        assert_eq!(text, "90s");
    }

    #[test]
    fn render_with_label() {
        let widget = Timer::new().label("Remaining:");
        let mut state = TimerState::new(Duration::from_secs(45));
        let text = render_to_string(&widget, &mut state, 30);
        assert_eq!(text, "Remaining: 45s");
    }

    #[test]
    fn render_finished_shows_zero() {
        let widget = Timer::new();
        let mut state = TimerState::new(Duration::from_secs(1));
        state.start();
        state.tick(Duration::from_secs(1));
        let text = render_to_string(&widget, &mut state, 20);
        assert_eq!(text, "0s");
    }

    #[test]
    fn stateless_render_shows_zero() {
        let widget = Timer::new();
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        Widget::render(&widget, area, &mut frame);
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('0'));
        assert_eq!(cell_char(&frame.buffer, 1, 0), Some('s'));
    }

    #[test]
    fn is_essential() {
        let widget = Timer::new();
        assert!(widget.is_essential());
    }

    // --- Degradation tests ---

    #[test]
    fn degradation_skeleton_skips() {
        use ftui_render::budget::DegradationLevel;

        let widget = Timer::new();
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::Skeleton;
        let mut state = TimerState::new(Duration::from_secs(60));
        StatefulWidget::render(&widget, area, &mut frame, &mut state);
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    // --- Countdown progression test ---

    #[test]
    fn countdown_progression() {
        let mut state = TimerState::new(Duration::from_secs(5));
        state.start();

        for expected in (0..=4).rev() {
            state.tick(Duration::from_secs(1));
            assert_eq!(state.remaining(), Duration::from_secs(expected));
        }

        assert!(state.finished());
    }
}

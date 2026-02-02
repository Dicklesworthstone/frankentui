#![forbid(unsafe_code)]

//! Stopwatch widget for displaying elapsed time.
//!
//! Provides a [`Stopwatch`] widget that renders a formatted duration, and a
//! [`StopwatchState`] that tracks elapsed time with start/stop/reset semantics.
//!
//! # Example
//!
//! ```rust
//! use ftui_widgets::stopwatch::{Stopwatch, StopwatchState};
//! use std::time::Duration;
//!
//! let mut state = StopwatchState::new();
//! assert_eq!(state.elapsed(), Duration::ZERO);
//! assert!(!state.running());
//!
//! state.start();
//! state.tick(Duration::from_secs(1));
//! assert_eq!(state.elapsed(), Duration::from_secs(1));
//! ```

use crate::{StatefulWidget, Widget, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;

/// Display format for the stopwatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopwatchFormat {
    /// Human-readable with units: "1h30m15s", "45s", "100ms".
    #[default]
    Human,
    /// Fixed-width digital clock: "01:30:15", "00:00:45".
    Digital,
    /// Compact seconds only: "5415s", "45s".
    Seconds,
}

/// State for the stopwatch, tracking elapsed time and running status.
#[derive(Debug, Clone)]
pub struct StopwatchState {
    elapsed: std::time::Duration,
    running: bool,
}

impl Default for StopwatchState {
    fn default() -> Self {
        Self::new()
    }
}

impl StopwatchState {
    /// Creates a new stopped stopwatch at zero elapsed time.
    pub fn new() -> Self {
        Self {
            elapsed: std::time::Duration::ZERO,
            running: false,
        }
    }

    /// Returns the elapsed time.
    pub fn elapsed(&self) -> std::time::Duration {
        self.elapsed
    }

    /// Returns whether the stopwatch is currently running.
    pub fn running(&self) -> bool {
        self.running
    }

    /// Starts the stopwatch.
    pub fn start(&mut self) {
        self.running = true;
    }

    /// Stops (pauses) the stopwatch.
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Toggles between running and stopped.
    pub fn toggle(&mut self) {
        self.running = !self.running;
    }

    /// Resets elapsed time to zero. Does not change running state.
    pub fn reset(&mut self) {
        self.elapsed = std::time::Duration::ZERO;
    }

    /// Advances the stopwatch by the given delta if running.
    /// Returns `true` if the tick was applied.
    pub fn tick(&mut self, delta: std::time::Duration) -> bool {
        if self.running {
            self.elapsed += delta;
            true
        } else {
            false
        }
    }
}

/// A widget that displays elapsed time from a [`StopwatchState`].
#[derive(Debug, Clone, Default)]
pub struct Stopwatch<'a> {
    format: StopwatchFormat,
    style: Style,
    running_style: Option<Style>,
    stopped_style: Option<Style>,
    label: Option<&'a str>,
}

impl<'a> Stopwatch<'a> {
    /// Creates a new stopwatch widget with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the display format.
    pub fn format(mut self, format: StopwatchFormat) -> Self {
        self.format = format;
        self
    }

    /// Sets the base style.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Sets a style override used when the stopwatch is running.
    pub fn running_style(mut self, style: Style) -> Self {
        self.running_style = Some(style);
        self
    }

    /// Sets a style override used when the stopwatch is stopped.
    pub fn stopped_style(mut self, style: Style) -> Self {
        self.stopped_style = Some(style);
        self
    }

    /// Sets an optional label rendered before the time.
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }
}

impl StatefulWidget for Stopwatch<'_> {
    type State = StopwatchState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        if area.is_empty() || area.height == 0 {
            return;
        }

        let deg = frame.buffer.degradation;
        if !deg.render_content() {
            return;
        }

        let style = if deg.apply_styling() {
            if state.running {
                self.running_style.unwrap_or(self.style)
            } else {
                self.stopped_style.unwrap_or(self.style)
            }
        } else {
            Style::default()
        };

        let formatted = format_duration(state.elapsed, self.format);
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

impl Widget for Stopwatch<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let mut state = StopwatchState::new();
        StatefulWidget::render(self, area, frame, &mut state);
    }

    fn is_essential(&self) -> bool {
        true
    }
}

/// Formats a duration according to the given format.
pub(crate) fn format_duration(d: std::time::Duration, fmt: StopwatchFormat) -> String {
    match fmt {
        StopwatchFormat::Human => format_human(d),
        StopwatchFormat::Digital => format_digital(d),
        StopwatchFormat::Seconds => format_seconds(d),
    }
}

/// Human-readable format: "1h30m15s", "45s", "100ms", "0s".
fn format_human(d: std::time::Duration) -> String {
    let total_nanos = d.as_nanos();
    if total_nanos == 0 {
        return "0s".to_string();
    }

    let total_secs = d.as_secs();
    let subsec_nanos = d.subsec_nanos();

    // Sub-second: show ms, µs, or ns
    if total_secs == 0 {
        let micros = d.as_micros();
        if micros >= 1000 {
            let millis = d.as_millis();
            let remainder_micros = micros % 1000;
            if remainder_micros == 0 {
                return format!("{millis}ms");
            }
            let decimal = format!("{:06}", d.as_nanos() % 1_000_000);
            let trimmed = decimal.trim_end_matches('0');
            if trimmed.is_empty() {
                return format!("{millis}ms");
            }
            return format!("{millis}.{trimmed}ms");
        } else if micros >= 1 {
            let nanos = d.as_nanos() % 1000;
            if nanos == 0 {
                return format!("{micros}µs");
            }
            let decimal = format!("{:03}", nanos);
            let trimmed = decimal.trim_end_matches('0');
            return format!("{micros}.{trimmed}µs");
        } else {
            return format!("{}ns", d.as_nanos());
        }
    }

    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    let subsec_str = if subsec_nanos > 0 {
        let decimal = format!("{subsec_nanos:09}");
        let trimmed = decimal.trim_end_matches('0');
        if trimmed.is_empty() {
            String::new()
        } else {
            format!(".{trimmed}")
        }
    } else {
        String::new()
    };

    if hours > 0 {
        format!("{hours}h{minutes}m{seconds}{subsec_str}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds}{subsec_str}s")
    } else {
        format!("{seconds}{subsec_str}s")
    }
}

/// Fixed-width digital format: "01:30:15", "00:00:45".
fn format_digital(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Compact seconds format: "5415s", "0s".
fn format_seconds(d: std::time::Duration) -> String {
    format!("{}s", d.as_secs())
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

    fn render_to_string(widget: &Stopwatch, state: &mut StopwatchState, width: u16) -> String {
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

    // --- StopwatchState tests ---

    #[test]
    fn state_default_is_zero_and_stopped() {
        let state = StopwatchState::new();
        assert_eq!(state.elapsed(), Duration::ZERO);
        assert!(!state.running());
    }

    #[test]
    fn state_start_stop() {
        let mut state = StopwatchState::new();
        state.start();
        assert!(state.running());
        state.stop();
        assert!(!state.running());
    }

    #[test]
    fn state_toggle() {
        let mut state = StopwatchState::new();
        state.toggle();
        assert!(state.running());
        state.toggle();
        assert!(!state.running());
    }

    #[test]
    fn state_tick_when_running() {
        let mut state = StopwatchState::new();
        state.start();
        assert!(state.tick(Duration::from_secs(1)));
        assert_eq!(state.elapsed(), Duration::from_secs(1));
        assert!(state.tick(Duration::from_secs(2)));
        assert_eq!(state.elapsed(), Duration::from_secs(3));
    }

    #[test]
    fn state_tick_when_stopped_is_noop() {
        let mut state = StopwatchState::new();
        assert!(!state.tick(Duration::from_secs(1)));
        assert_eq!(state.elapsed(), Duration::ZERO);
    }

    #[test]
    fn state_reset() {
        let mut state = StopwatchState::new();
        state.start();
        state.tick(Duration::from_secs(100));
        state.reset();
        assert_eq!(state.elapsed(), Duration::ZERO);
        assert!(state.running()); // reset doesn't change running state
    }

    // --- format_human tests ---

    #[test]
    fn human_zero() {
        assert_eq!(format_human(Duration::ZERO), "0s");
    }

    #[test]
    fn human_seconds() {
        assert_eq!(format_human(Duration::from_secs(45)), "45s");
    }

    #[test]
    fn human_minutes_seconds() {
        assert_eq!(format_human(Duration::from_secs(125)), "2m5s");
    }

    #[test]
    fn human_hours_minutes_seconds() {
        assert_eq!(format_human(Duration::from_secs(3665)), "1h1m5s");
    }

    #[test]
    fn human_with_subseconds() {
        assert_eq!(format_human(Duration::from_millis(5500)), "5.5s");
        assert_eq!(format_human(Duration::from_millis(5001)), "5.001s");
    }

    #[test]
    fn human_sub_second_ms() {
        assert_eq!(format_human(Duration::from_millis(100)), "100ms");
        assert_eq!(format_human(Duration::from_millis(1)), "1ms");
    }

    #[test]
    fn human_sub_second_us() {
        assert_eq!(format_human(Duration::from_micros(500)), "500µs");
    }

    #[test]
    fn human_sub_second_ns() {
        assert_eq!(format_human(Duration::from_nanos(123)), "123ns");
    }

    #[test]
    fn human_large_hours() {
        assert_eq!(
            format_human(Duration::from_secs(100 * 3600 + 30 * 60 + 15)),
            "100h30m15s"
        );
    }

    // --- format_digital tests ---

    #[test]
    fn digital_zero() {
        assert_eq!(format_digital(Duration::ZERO), "00:00");
    }

    #[test]
    fn digital_seconds() {
        assert_eq!(format_digital(Duration::from_secs(45)), "00:45");
    }

    #[test]
    fn digital_minutes_seconds() {
        assert_eq!(format_digital(Duration::from_secs(125)), "02:05");
    }

    #[test]
    fn digital_hours() {
        assert_eq!(format_digital(Duration::from_secs(3665)), "01:01:05");
    }

    // --- format_seconds tests ---

    #[test]
    fn seconds_format() {
        assert_eq!(format_seconds(Duration::ZERO), "0s");
        assert_eq!(format_seconds(Duration::from_secs(5415)), "5415s");
    }

    // --- Widget rendering tests ---

    #[test]
    fn render_zero_area() {
        let widget = Stopwatch::new();
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let mut state = StopwatchState::new();
        StatefulWidget::render(&widget, area, &mut frame, &mut state);
        // Should not panic
    }

    #[test]
    fn render_default_zero() {
        let widget = Stopwatch::new();
        let mut state = StopwatchState::new();
        let text = render_to_string(&widget, &mut state, 20);
        assert_eq!(text, "0s");
    }

    #[test]
    fn render_elapsed_human() {
        let widget = Stopwatch::new();
        let mut state = StopwatchState {
            elapsed: Duration::from_secs(125),
            running: false,
        };
        let text = render_to_string(&widget, &mut state, 20);
        assert_eq!(text, "2m5s");
    }

    #[test]
    fn render_digital_format() {
        let widget = Stopwatch::new().format(StopwatchFormat::Digital);
        let mut state = StopwatchState {
            elapsed: Duration::from_secs(3665),
            running: false,
        };
        let text = render_to_string(&widget, &mut state, 20);
        assert_eq!(text, "01:01:05");
    }

    #[test]
    fn render_seconds_format() {
        let widget = Stopwatch::new().format(StopwatchFormat::Seconds);
        let mut state = StopwatchState {
            elapsed: Duration::from_secs(90),
            running: false,
        };
        let text = render_to_string(&widget, &mut state, 20);
        assert_eq!(text, "90s");
    }

    #[test]
    fn render_with_label() {
        let widget = Stopwatch::new().label("Elapsed:");
        let mut state = StopwatchState {
            elapsed: Duration::from_secs(45),
            running: false,
        };
        let text = render_to_string(&widget, &mut state, 30);
        assert_eq!(text, "Elapsed: 45s");
    }

    #[test]
    fn render_clips_to_area() {
        let widget = Stopwatch::new().format(StopwatchFormat::Digital);
        let mut state = StopwatchState {
            elapsed: Duration::from_secs(3665),
            running: false,
        };
        // Area of width 5 should clip "01:01:05"
        let text = render_to_string(&widget, &mut state, 5);
        assert_eq!(text, "01:01");
    }

    #[test]
    fn stateless_render_shows_zero() {
        let widget = Stopwatch::new();
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        Widget::render(&widget, area, &mut frame);
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('0'));
        assert_eq!(cell_char(&frame.buffer, 1, 0), Some('s'));
    }

    #[test]
    fn is_essential() {
        let widget = Stopwatch::new();
        assert!(widget.is_essential());
    }

    // --- Degradation tests ---

    #[test]
    fn degradation_skeleton_skips() {
        use ftui_render::budget::DegradationLevel;

        let widget = Stopwatch::new();
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::Skeleton;
        let mut state = StopwatchState {
            elapsed: Duration::from_secs(45),
            running: false,
        };
        StatefulWidget::render(&widget, area, &mut frame, &mut state);
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn degradation_no_styling_uses_default_style() {
        use ftui_render::budget::DegradationLevel;

        let widget = Stopwatch::new().style(Style::default().bold());
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::NoStyling;
        let mut state = StopwatchState {
            elapsed: Duration::from_secs(5),
            running: false,
        };
        StatefulWidget::render(&widget, area, &mut frame, &mut state);
        // Content should still render, just without custom style
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('5'));
    }
}

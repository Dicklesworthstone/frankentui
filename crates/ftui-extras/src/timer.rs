#![forbid(unsafe_code)]

//! Countdown timer widget.
//!
//! Counts down from a specified duration with start/pause/reset.
//! Uses the same formatting as the stopwatch module.
//!
//! # Example
//! ```
//! use ftui_extras::timer::Timer;
//! use std::time::Duration;
//!
//! let mut timer = Timer::new(Duration::from_secs(60));
//! assert_eq!(timer.remaining(), Duration::from_secs(60));
//! assert!(!timer.finished());
//!
//! timer.start();
//! timer.tick(Duration::from_secs(5));
//! assert_eq!(timer.remaining(), Duration::from_secs(55));
//! ```

use std::time::Duration;

/// Display format for the timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayFormat {
    /// Compact: "1h2m3s", "45s", "100ms"
    Compact,
    /// Clock-style: "01:02:03", "00:45"
    Clock,
}

/// A countdown timer.
#[derive(Debug, Clone)]
pub struct Timer {
    /// The initial duration (for reset).
    initial: Duration,
    /// Remaining time.
    remaining: Duration,
    /// Tick interval for `tick_once`.
    interval: Duration,
    /// Whether the timer is running.
    running: bool,
    /// Display format.
    format: DisplayFormat,
}

impl Timer {
    /// Create a new timer with the given duration and default 1-second tick interval.
    #[must_use]
    pub fn new(duration: Duration) -> Self {
        Self::with_interval(duration, Duration::from_secs(1))
    }

    /// Create a new timer with the given duration and tick interval.
    #[must_use]
    pub fn with_interval(duration: Duration, interval: Duration) -> Self {
        Self {
            initial: duration,
            remaining: duration,
            interval,
            running: false,
            format: DisplayFormat::Compact,
        }
    }

    /// Set the display format.
    #[must_use]
    pub fn format(mut self, format: DisplayFormat) -> Self {
        self.format = format;
        self
    }

    /// Whether the timer is currently running.
    ///
    /// Returns `false` if finished, even if not explicitly stopped.
    #[must_use]
    pub fn running(&self) -> bool {
        self.running && !self.finished()
    }

    /// Whether the countdown has reached zero.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.remaining.is_zero()
    }

    /// The remaining time.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.remaining
    }

    /// The initial duration (before any ticks).
    #[must_use]
    pub fn initial(&self) -> Duration {
        self.initial
    }

    /// The tick interval.
    #[must_use]
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Progress as a fraction from 0.0 (just started) to 1.0 (finished).
    #[must_use]
    pub fn progress(&self) -> f64 {
        if self.initial.is_zero() {
            return 1.0;
        }
        let elapsed = self.initial.saturating_sub(self.remaining);
        elapsed.as_secs_f64() / self.initial.as_secs_f64()
    }

    /// Start the timer.
    pub fn start(&mut self) {
        self.running = true;
    }

    /// Stop (pause) the timer.
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Toggle between running and stopped.
    pub fn toggle(&mut self) {
        self.running = !self.running;
    }

    /// Reset the timer to its initial duration. Does not change running state.
    pub fn reset(&mut self) {
        self.remaining = self.initial;
    }

    /// Advance the timer by one tick interval.
    ///
    /// Only advances if running and not finished.
    /// Returns `true` if the timer just finished on this tick.
    pub fn tick_once(&mut self) -> bool {
        if !self.running || self.finished() {
            return false;
        }
        let was_nonzero = !self.remaining.is_zero();
        self.remaining = self.remaining.saturating_sub(self.interval);
        was_nonzero && self.remaining.is_zero()
    }

    /// Advance the timer by an arbitrary duration.
    ///
    /// Only advances if running and not finished.
    /// Returns `true` if the timer just finished on this tick.
    pub fn tick(&mut self, delta: Duration) -> bool {
        if !self.running || self.finished() {
            return false;
        }
        let was_nonzero = !self.remaining.is_zero();
        self.remaining = self.remaining.saturating_sub(delta);
        was_nonzero && self.remaining.is_zero()
    }

    /// Render the remaining time as a string.
    #[must_use]
    pub fn view(&self) -> String {
        match self.format {
            DisplayFormat::Compact => format_compact(self.remaining),
            DisplayFormat::Clock => format_clock(self.remaining),
        }
    }
}

/// Format duration in compact style.
fn format_compact(d: Duration) -> String {
    let total_nanos = d.as_nanos();

    if total_nanos == 0 {
        return "0s".to_string();
    }

    let total_secs = d.as_secs();
    let subsec_nanos = d.subsec_nanos();

    if total_secs == 0 {
        let micros = d.as_micros();
        if micros >= 1000 {
            let millis = d.as_millis();
            let remainder_micros = micros % 1000;
            if remainder_micros == 0 {
                return format!("{millis}ms");
            }
            let decimal = format!("{:06}", total_nanos % 1_000_000);
            let trimmed = decimal.trim_end_matches('0');
            if trimmed.is_empty() {
                return format!("{millis}ms");
            }
            return format!("{millis}.{trimmed}ms");
        } else if micros >= 1 {
            let nanos = total_nanos % 1000;
            if nanos == 0 {
                return format!("{micros}\u{00B5}s");
            }
            let decimal = format!("{nanos:03}");
            let trimmed = decimal.trim_end_matches('0');
            return format!("{micros}.{trimmed}\u{00B5}s");
        } else {
            return format!("{total_nanos}ns");
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

/// Format duration in clock style.
fn format_clock(d: Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_timer() {
        let t = Timer::new(Duration::from_secs(60));
        assert_eq!(t.remaining(), Duration::from_secs(60));
        assert!(!t.running());
        assert!(!t.finished());
    }

    #[test]
    fn with_interval() {
        let t = Timer::with_interval(Duration::from_secs(30), Duration::from_millis(100));
        assert_eq!(t.interval(), Duration::from_millis(100));
        assert_eq!(t.initial(), Duration::from_secs(30));
    }

    #[test]
    fn start_stop_toggle() {
        let mut t = Timer::new(Duration::from_secs(10));
        assert!(!t.running());
        t.start();
        assert!(t.running());
        t.stop();
        assert!(!t.running());
        t.toggle();
        assert!(t.running());
        t.toggle();
        assert!(!t.running());
    }

    #[test]
    fn tick_once_counts_down() {
        let mut t = Timer::new(Duration::from_secs(10));
        t.start();
        let finished = t.tick_once();
        assert!(!finished);
        assert_eq!(t.remaining(), Duration::from_secs(9));
    }

    #[test]
    fn tick_once_when_stopped() {
        let mut t = Timer::new(Duration::from_secs(10));
        let finished = t.tick_once();
        assert!(!finished);
        assert_eq!(t.remaining(), Duration::from_secs(10));
    }

    #[test]
    fn tick_arbitrary_duration() {
        let mut t = Timer::new(Duration::from_secs(10));
        t.start();
        let finished = t.tick(Duration::from_secs(3));
        assert!(!finished);
        assert_eq!(t.remaining(), Duration::from_secs(7));
    }

    #[test]
    fn tick_to_zero() {
        let mut t = Timer::new(Duration::from_secs(5));
        t.start();
        for i in 0..4 {
            let finished = t.tick_once();
            assert!(!finished, "Should not be finished at tick {i}");
        }
        let finished = t.tick_once();
        assert!(finished, "Should finish on last tick");
        assert!(t.finished());
        assert!(!t.running());
    }

    #[test]
    fn tick_past_zero_saturates() {
        let mut t = Timer::new(Duration::from_secs(2));
        t.start();
        let finished = t.tick(Duration::from_secs(10));
        assert!(finished);
        assert_eq!(t.remaining(), Duration::ZERO);
        assert!(t.finished());
    }

    #[test]
    fn tick_when_finished_is_noop() {
        let mut t = Timer::new(Duration::from_secs(1));
        t.start();
        t.tick_once();
        assert!(t.finished());

        let finished = t.tick_once();
        assert!(!finished); // already finished, shouldn't trigger again
        assert_eq!(t.remaining(), Duration::ZERO);
    }

    #[test]
    fn reset() {
        let mut t = Timer::new(Duration::from_secs(60));
        t.start();
        t.tick(Duration::from_secs(30));
        assert_eq!(t.remaining(), Duration::from_secs(30));

        t.reset();
        assert_eq!(t.remaining(), Duration::from_secs(60));
        assert!(t.running()); // reset doesn't change running state
    }

    #[test]
    fn progress_calculation() {
        let mut t = Timer::new(Duration::from_secs(100));
        assert!((t.progress() - 0.0).abs() < f64::EPSILON);

        t.start();
        t.tick(Duration::from_secs(50));
        assert!((t.progress() - 0.5).abs() < f64::EPSILON);

        t.tick(Duration::from_secs(50));
        assert!((t.progress() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn progress_zero_initial() {
        let t = Timer::new(Duration::ZERO);
        assert!((t.progress() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn zero_duration_timer() {
        let t = Timer::new(Duration::ZERO);
        assert!(t.finished());
        assert!(!t.running());
        assert_eq!(t.view(), "0s");
    }

    // View tests

    #[test]
    fn view_compact() {
        let t = Timer::new(Duration::from_secs(125));
        assert_eq!(t.view(), "2m5s");
    }

    #[test]
    fn view_clock() {
        let t = Timer::new(Duration::from_secs(125)).format(DisplayFormat::Clock);
        assert_eq!(t.view(), "02:05");
    }

    #[test]
    fn view_updates_after_tick() {
        let mut t = Timer::new(Duration::from_secs(10));
        t.start();
        assert_eq!(t.view(), "10s");
        t.tick(Duration::from_secs(3));
        assert_eq!(t.view(), "7s");
    }

    // Format tests

    #[test]
    fn compact_zero() {
        assert_eq!(format_compact(Duration::ZERO), "0s");
    }

    #[test]
    fn compact_seconds() {
        assert_eq!(format_compact(Duration::from_secs(45)), "45s");
    }

    #[test]
    fn compact_minutes() {
        assert_eq!(format_compact(Duration::from_secs(90)), "1m30s");
    }

    #[test]
    fn compact_hours() {
        assert_eq!(format_compact(Duration::from_secs(3665)), "1h1m5s");
    }

    #[test]
    fn compact_millis() {
        assert_eq!(format_compact(Duration::from_millis(500)), "500ms");
    }

    #[test]
    fn clock_zero() {
        assert_eq!(format_clock(Duration::ZERO), "00:00");
    }

    #[test]
    fn clock_minutes() {
        assert_eq!(format_clock(Duration::from_secs(125)), "02:05");
    }

    #[test]
    fn clock_hours() {
        assert_eq!(format_clock(Duration::from_secs(3665)), "1:01:05");
    }
}

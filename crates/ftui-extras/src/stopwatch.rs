#![forbid(unsafe_code)]

//! Stopwatch widget for tracking elapsed time.
//!
//! Provides a stopwatch that counts up from zero with start/stop/reset
//! and configurable display formatting.
//!
//! # Example
//! ```
//! use ftui_extras::stopwatch::Stopwatch;
//! use std::time::Duration;
//!
//! let mut sw = Stopwatch::new();
//! assert_eq!(sw.elapsed(), Duration::ZERO);
//! assert!(!sw.running());
//!
//! sw.start();
//! sw.tick(Duration::from_secs(5));
//! assert_eq!(sw.view(), "5s");
//! ```

use std::time::Duration;

/// Display format for the stopwatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayFormat {
    /// Compact: "1h2m3s", "45s", "100ms"
    Compact,
    /// Clock-style: "01:02:03", "00:45"
    Clock,
}

/// A stopwatch that tracks elapsed time.
#[derive(Debug, Clone)]
pub struct Stopwatch {
    elapsed: Duration,
    interval: Duration,
    running: bool,
    format: DisplayFormat,
}

impl Default for Stopwatch {
    fn default() -> Self {
        Self::new()
    }
}

impl Stopwatch {
    /// Create a new stopwatch with the default 1-second tick interval.
    #[must_use]
    pub fn new() -> Self {
        Self::with_interval(Duration::from_secs(1))
    }

    /// Create a new stopwatch with the given tick interval.
    #[must_use]
    pub fn with_interval(interval: Duration) -> Self {
        Self {
            elapsed: Duration::ZERO,
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

    /// Whether the stopwatch is currently running.
    #[must_use]
    pub fn running(&self) -> bool {
        self.running
    }

    /// The elapsed time.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// The tick interval.
    #[must_use]
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Start the stopwatch.
    pub fn start(&mut self) {
        self.running = true;
    }

    /// Stop (pause) the stopwatch.
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Toggle between running and stopped.
    pub fn toggle(&mut self) {
        self.running = !self.running;
    }

    /// Reset elapsed time to zero. Does not change running state.
    pub fn reset(&mut self) {
        self.elapsed = Duration::ZERO;
    }

    /// Advance the stopwatch by one tick interval.
    ///
    /// Only advances if the stopwatch is running.
    /// Returns `true` if the elapsed time changed.
    pub fn tick_once(&mut self) -> bool {
        if self.running {
            self.elapsed += self.interval;
            true
        } else {
            false
        }
    }

    /// Advance the stopwatch by an arbitrary duration.
    ///
    /// Only advances if the stopwatch is running.
    /// Returns `true` if the elapsed time changed.
    pub fn tick(&mut self, delta: Duration) -> bool {
        if self.running {
            self.elapsed += delta;
            true
        } else {
            false
        }
    }

    /// Render the current elapsed time as a string.
    #[must_use]
    pub fn view(&self) -> String {
        match self.format {
            DisplayFormat::Compact => format_compact(self.elapsed),
            DisplayFormat::Clock => format_clock(self.elapsed),
        }
    }
}

/// Format duration in compact style: "1h2m3s", "45s", "100ms".
fn format_compact(d: Duration) -> String {
    let total_nanos = d.as_nanos();

    if total_nanos == 0 {
        return "0s".to_string();
    }

    let total_secs = d.as_secs();
    let subsec_nanos = d.subsec_nanos();

    // Sub-second durations
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

/// Format duration in clock style: "01:02:03", "00:45", "1:30:15".
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
    fn new_stopwatch() {
        let sw = Stopwatch::new();
        assert_eq!(sw.elapsed(), Duration::ZERO);
        assert!(!sw.running());
        assert_eq!(sw.interval(), Duration::from_secs(1));
    }

    #[test]
    fn with_interval() {
        let sw = Stopwatch::with_interval(Duration::from_millis(100));
        assert_eq!(sw.interval(), Duration::from_millis(100));
    }

    #[test]
    fn start_stop() {
        let mut sw = Stopwatch::new();
        assert!(!sw.running());
        sw.start();
        assert!(sw.running());
        sw.stop();
        assert!(!sw.running());
    }

    #[test]
    fn toggle() {
        let mut sw = Stopwatch::new();
        sw.toggle();
        assert!(sw.running());
        sw.toggle();
        assert!(!sw.running());
    }

    #[test]
    fn tick_once_when_running() {
        let mut sw = Stopwatch::new();
        sw.start();
        assert!(sw.tick_once());
        assert_eq!(sw.elapsed(), Duration::from_secs(1));
    }

    #[test]
    fn tick_once_when_stopped() {
        let mut sw = Stopwatch::new();
        assert!(!sw.tick_once());
        assert_eq!(sw.elapsed(), Duration::ZERO);
    }

    #[test]
    fn tick_arbitrary_duration() {
        let mut sw = Stopwatch::new();
        sw.start();
        sw.tick(Duration::from_millis(500));
        assert_eq!(sw.elapsed(), Duration::from_millis(500));
        sw.tick(Duration::from_millis(500));
        assert_eq!(sw.elapsed(), Duration::from_secs(1));
    }

    #[test]
    fn tick_when_stopped_is_noop() {
        let mut sw = Stopwatch::new();
        assert!(!sw.tick(Duration::from_secs(5)));
        assert_eq!(sw.elapsed(), Duration::ZERO);
    }

    #[test]
    fn reset() {
        let mut sw = Stopwatch::new();
        sw.start();
        sw.tick(Duration::from_secs(100));
        sw.reset();
        assert_eq!(sw.elapsed(), Duration::ZERO);
        assert!(sw.running()); // reset doesn't change running state
    }

    #[test]
    fn multiple_ticks() {
        let mut sw = Stopwatch::with_interval(Duration::from_secs(1));
        sw.start();
        for _ in 0..10 {
            sw.tick_once();
        }
        assert_eq!(sw.elapsed(), Duration::from_secs(10));
    }

    // Compact format tests

    #[test]
    fn compact_zero() {
        assert_eq!(format_compact(Duration::ZERO), "0s");
    }

    #[test]
    fn compact_seconds() {
        assert_eq!(format_compact(Duration::from_secs(45)), "45s");
    }

    #[test]
    fn compact_seconds_with_millis() {
        assert_eq!(format_compact(Duration::from_millis(5500)), "5.5s");
        assert_eq!(format_compact(Duration::from_millis(5050)), "5.05s");
        assert_eq!(format_compact(Duration::from_millis(5001)), "5.001s");
    }

    #[test]
    fn compact_minutes() {
        assert_eq!(format_compact(Duration::from_secs(60)), "1m0s");
        assert_eq!(format_compact(Duration::from_secs(90)), "1m30s");
        assert_eq!(format_compact(Duration::from_secs(125)), "2m5s");
    }

    #[test]
    fn compact_minutes_with_millis() {
        assert_eq!(format_compact(Duration::from_millis(90500)), "1m30.5s");
    }

    #[test]
    fn compact_hours() {
        assert_eq!(format_compact(Duration::from_secs(3600)), "1h0m0s");
        assert_eq!(format_compact(Duration::from_secs(3665)), "1h1m5s");
        assert_eq!(
            format_compact(Duration::from_secs(100 * 3600 + 30 * 60 + 15)),
            "100h30m15s"
        );
    }

    #[test]
    fn compact_hours_with_millis() {
        assert_eq!(format_compact(Duration::from_millis(3_600_500)), "1h0m0.5s");
    }

    #[test]
    fn compact_milliseconds() {
        assert_eq!(format_compact(Duration::from_millis(100)), "100ms");
        assert_eq!(format_compact(Duration::from_millis(1)), "1ms");
        assert_eq!(format_compact(Duration::from_millis(999)), "999ms");
    }

    #[test]
    fn compact_microseconds() {
        assert_eq!(format_compact(Duration::from_micros(500)), "500\u{00B5}s");
    }

    #[test]
    fn compact_nanoseconds() {
        assert_eq!(format_compact(Duration::from_nanos(123)), "123ns");
    }

    // Clock format tests

    #[test]
    fn clock_zero() {
        assert_eq!(format_clock(Duration::ZERO), "00:00");
    }

    #[test]
    fn clock_seconds() {
        assert_eq!(format_clock(Duration::from_secs(45)), "00:45");
    }

    #[test]
    fn clock_minutes() {
        assert_eq!(format_clock(Duration::from_secs(90)), "01:30");
        assert_eq!(format_clock(Duration::from_secs(125)), "02:05");
    }

    #[test]
    fn clock_hours() {
        assert_eq!(format_clock(Duration::from_secs(3600)), "1:00:00");
        assert_eq!(format_clock(Duration::from_secs(3665)), "1:01:05");
    }

    #[test]
    fn clock_ignores_subseconds() {
        assert_eq!(format_clock(Duration::from_millis(5500)), "00:05");
    }

    // View tests

    #[test]
    fn view_default_format() {
        let mut sw = Stopwatch::new();
        sw.start();
        sw.tick(Duration::from_secs(125));
        assert_eq!(sw.view(), "2m5s");
    }

    #[test]
    fn view_clock_format() {
        let mut sw = Stopwatch::new().format(DisplayFormat::Clock);
        sw.start();
        sw.tick(Duration::from_secs(125));
        assert_eq!(sw.view(), "02:05");
    }

    #[test]
    fn default_impl() {
        let sw = Stopwatch::default();
        assert_eq!(sw.elapsed(), Duration::ZERO);
        assert!(!sw.running());
    }
}

#![forbid(unsafe_code)]

//! Live display system for transient, self-updating terminal output.
//!
//! Provides an optional "live updating" display helper that renders content
//! repeatedly and cleans up after itself. This is **not** the core ftui
//! rendering model (full-buffer diff + presenter). It's an optional utility
//! for cases where you want transient output in a normal terminal stream
//! (progress bars, status lines, spinners outside the TUI).
//!
//! # One-Writer Rule
//!
//! All output goes through an explicit writer (`Box<dyn Write + Send>`).
//! No hidden writes to stdout/stderr.
//!
//! # Quick Start
//!
//! ```no_run
//! use ftui_extras::live::{Live, LiveConfig, VerticalOverflow};
//! use ftui_extras::console::{Console, ConsoleSink};
//! use ftui_text::Segment;
//!
//! let mut live = Live::new(Box::new(std::io::stdout()), 80);
//! live.start().unwrap();
//! live.update(|console| {
//!     console.print(Segment::text("Loading..."));
//!     console.newline();
//! });
//! live.stop().unwrap();
//! ```

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::console::{Console, ConsoleSink};
use ftui_render::sanitize::sanitize;

// ============================================================================
// Configuration
// ============================================================================

/// Strategy for handling content that exceeds the available height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerticalOverflow {
    /// Truncate silently at the bottom.
    Crop,
    /// Show "..." on the last visible line.
    #[default]
    Ellipsis,
    /// Show all content (may cause scrolling).
    Visible,
}

/// Configuration for the Live display.
#[derive(Debug, Clone)]
pub struct LiveConfig {
    /// Maximum number of lines to display. 0 = unlimited.
    pub max_height: usize,
    /// Vertical overflow strategy.
    pub overflow: VerticalOverflow,
    /// Whether to clean up (erase) the live region when stopped.
    pub transient: bool,
    /// Refresh rate for auto-refresh (if using `start_auto_refresh`).
    pub refresh_per_second: f64,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            max_height: 0,
            overflow: VerticalOverflow::Ellipsis,
            transient: true,
            refresh_per_second: 4.0,
        }
    }
}

const AUTO_REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(1);
const AUTO_REFRESH_SLEEP_SLICE: Duration = Duration::from_millis(50);

#[inline]
fn auto_refresh_interval(rate: f64) -> Option<Duration> {
    if !rate.is_finite() || rate <= 0.0 {
        return None;
    }

    // Guard both extremes:
    // - Very high rates can round to zero and spin.
    // - Very low positive rates can produce multi-hour sleeps that make
    //   stop/join appear hung.
    let secs = (1.0 / rate)
        .max(AUTO_REFRESH_MIN_INTERVAL.as_secs_f64())
        .min(Duration::MAX.as_secs_f64());
    Some(Duration::try_from_secs_f64(secs).unwrap_or(Duration::MAX))
}

// ============================================================================
// ANSI escape helpers
// ============================================================================

/// Write ANSI escape to move cursor up N lines.
fn cursor_up(writer: &mut dyn Write, n: usize) -> io::Result<()> {
    if n > 0 {
        write!(writer, "\x1b[{n}A")
    } else {
        Ok(())
    }
}

/// Write ANSI escape to move cursor to start of line.
fn carriage_return(writer: &mut dyn Write) -> io::Result<()> {
    write!(writer, "\r")
}

/// Write ANSI escape to erase from cursor to end of line.
fn erase_line(writer: &mut dyn Write) -> io::Result<()> {
    write!(writer, "\x1b[2K")
}

/// Write ANSI escape to hide cursor.
fn hide_cursor(writer: &mut dyn Write) -> io::Result<()> {
    write!(writer, "\x1b[?25l")
}

/// Write ANSI escape to show cursor.
fn show_cursor(writer: &mut dyn Write) -> io::Result<()> {
    write!(writer, "\x1b[?25h")
}

// ============================================================================
// Live Display
// ============================================================================

/// A live-updating display region in the terminal.
///
/// Uses cursor movement and line erasure to update content in place.
/// Thread-safe: the inner writer is protected by a Mutex.
pub struct Live {
    writer: Mutex<Box<dyn Write + Send>>,
    width: usize,
    config: LiveConfig,
    /// Number of lines written in the last render (for cursor repositioning).
    last_height: Mutex<usize>,
    /// Whether the live display is currently active.
    started: AtomicBool,
    /// Auto-refresh thread state (stop token + join handle).
    refresh_thread: Mutex<Option<RefreshThread>>,
}

struct RefreshThread {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl Live {
    /// Create a new Live display.
    pub fn new(writer: Box<dyn Write + Send>, width: usize) -> Self {
        Self::with_config(writer, width, LiveConfig::default())
    }

    /// Create a new Live display with custom configuration.
    pub fn with_config(writer: Box<dyn Write + Send>, width: usize, config: LiveConfig) -> Self {
        Self {
            writer: Mutex::new(writer),
            width,
            config,
            last_height: Mutex::new(0),
            started: AtomicBool::new(false),
            refresh_thread: Mutex::new(None),
        }
    }

    /// Start the live display (hide cursor, mark as active).
    ///
    /// Idempotent: calling start when already started is a no-op.
    pub fn start(&self) -> io::Result<()> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(()); // Already started
        }

        let mut writer = self.lock_writer();
        hide_cursor(&mut *writer)?;
        writer.flush()
    }

    /// Stop the live display (show cursor, optionally clean up).
    ///
    /// Idempotent: calling stop when already stopped is a no-op.
    pub fn stop(&self) -> io::Result<()> {
        // Auto-refresh may be running even if `start()` was never called.
        self.stop_refresh_thread();

        if !self.started.swap(false, Ordering::SeqCst) {
            return Ok(()); // Already stopped
        }

        let mut writer = self.lock_writer();

        if self.config.transient {
            // Erase the live region
            let height = *self.lock_height();
            self.erase_region(&mut *writer, height)?;
        }

        show_cursor(&mut *writer)?;
        writer.flush()
    }

    /// Update the live display with new content.
    ///
    /// The callback receives a `Console` for building styled output.
    /// The console output replaces the previous live region.
    pub fn update<F>(&self, f: F)
    where
        F: FnOnce(&mut Console),
    {
        if !self.started.load(Ordering::Relaxed) {
            return;
        }

        // Build content via Console with capture sink
        let sink = ConsoleSink::capture();
        let mut console = Console::new(self.width, sink);
        f(&mut console);
        let lines = console.into_captured_lines();

        // Apply overflow strategy
        let lines = self.apply_overflow(lines);
        let new_height = lines.len();

        let mut writer = self.lock_writer();
        let last_height = {
            let mut h = self.lock_height();
            let old = *h;
            *h = new_height;
            old
        };

        // Move cursor back to start of live region
        if last_height > 0 {
            let _ = self.reposition_cursor(&mut *writer, last_height);
        }

        // Write new content
        for (i, line) in lines.iter().enumerate() {
            let _ = erase_line(&mut *writer);
            let plain_text = line.plain_text();
            let safe_text = sanitize(&plain_text);
            let _ = write!(writer, "{safe_text}");
            if i < lines.len() - 1 {
                let _ = writeln!(writer);
            }
        }

        // Erase any extra lines from previous render
        if last_height > new_height {
            for _ in 0..(last_height - new_height) {
                let _ = writeln!(writer);
                let _ = erase_line(&mut *writer);
            }
            // Move cursor back up to end of new content
            let extra = last_height - new_height;
            let _ = cursor_up(&mut *writer, extra);
        }

        let _ = writer.flush();
    }

    /// Refresh the display by re-rendering the last content.
    ///
    /// Note: This is a no-op since we don't store the renderable callback.
    /// For auto-refresh, use `start_auto_refresh` with a callback.
    pub fn clear(&self) -> io::Result<()> {
        if !self.started.load(Ordering::Relaxed) {
            return Ok(());
        }

        let mut writer = self.lock_writer();
        let height = *self.lock_height();
        self.erase_region(&mut *writer, height)?;
        *self.lock_height() = 0;
        writer.flush()
    }

    /// Whether the live display is currently active.
    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::Relaxed)
    }

    /// Start an auto-refresh thread that calls a callback periodically.
    ///
    /// The callback should call `update()` on the Live instance.
    /// Stop with `stop()` or `stop_refresh_thread()`.
    pub fn start_auto_refresh<F>(&self, callback: F)
    where
        F: Fn() + Send + 'static,
    {
        // Stop any existing refresh thread before starting a new one.
        self.stop_refresh_thread();

        let Some(interval) = auto_refresh_interval(self.config.refresh_per_second) else {
            return;
        };

        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);

        let handle = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                let mut slept = Duration::ZERO;
                while slept < interval && !stop_thread.load(Ordering::Relaxed) {
                    let remaining = interval.saturating_sub(slept);
                    let step = remaining.min(AUTO_REFRESH_SLEEP_SLICE);
                    std::thread::sleep(step);
                    slept = slept.saturating_add(step);
                }
                if !stop_thread.load(Ordering::Relaxed) {
                    callback();
                }
            }
        });

        let mut guard = self
            .refresh_thread
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = Some(RefreshThread { stop, handle });
    }

    /// Stop the auto-refresh thread.
    pub fn stop_refresh_thread(&self) {
        let current = std::thread::current().id();
        let to_join = {
            let mut guard = self
                .refresh_thread
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Some(rt) = guard.as_ref() else {
                return;
            };

            rt.stop.store(true, Ordering::SeqCst);
            if rt.handle.thread().id() == current {
                // Don't self-join. Leave the handle so another thread can join later.
                None
            } else {
                guard.take()
            }
        };

        if let Some(rt) = to_join {
            let _ = rt.handle.join();
        }
    }

    // -- Internal helpers --

    fn lock_writer(&self) -> std::sync::MutexGuard<'_, Box<dyn Write + Send>> {
        self.writer.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_height(&self) -> std::sync::MutexGuard<'_, usize> {
        self.last_height.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn reposition_cursor(&self, writer: &mut dyn Write, height: usize) -> io::Result<()> {
        if height > 0 {
            carriage_return(writer)?;
            cursor_up(writer, height.saturating_sub(1))?;
        }
        Ok(())
    }

    fn erase_region(&self, writer: &mut dyn Write, height: usize) -> io::Result<()> {
        if height == 0 {
            return Ok(());
        }
        self.reposition_cursor(writer, height)?;
        for i in 0..height {
            erase_line(writer)?;
            if i < height - 1 {
                writeln!(writer)?;
            }
        }
        carriage_return(writer)
    }

    fn apply_overflow(
        &self,
        lines: Vec<crate::console::CapturedLine>,
    ) -> Vec<crate::console::CapturedLine> {
        let max = self.config.max_height;
        if max == 0 || lines.len() <= max {
            return lines;
        }

        match self.config.overflow {
            VerticalOverflow::Visible => lines,
            VerticalOverflow::Crop => lines.into_iter().take(max).collect(),
            VerticalOverflow::Ellipsis => {
                if max == 0 {
                    return Vec::new();
                }
                let mut truncated: Vec<_> = lines.into_iter().take(max.saturating_sub(1)).collect();
                truncated.push(crate::console::CapturedLine::from_plain("..."));
                truncated
            }
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_text::Segment;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A test writer that captures all bytes written.
    #[derive(Clone, Default)]
    struct TestWriter {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl TestWriter {
        fn new() -> Self {
            Self::default()
        }

        fn output(&self) -> String {
            let buf = self.buf.lock().unwrap();
            String::from_utf8_lossy(&buf).to_string()
        }

        fn clear(&self) {
            self.buf.lock().unwrap().clear();
        }
    }

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buf.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // -- Config tests --

    #[test]
    fn default_config() {
        let cfg = LiveConfig::default();
        assert_eq!(cfg.max_height, 0);
        assert_eq!(cfg.overflow, VerticalOverflow::Ellipsis);
        assert!(cfg.transient);
        assert!((cfg.refresh_per_second - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn overflow_default_is_ellipsis() {
        assert_eq!(VerticalOverflow::default(), VerticalOverflow::Ellipsis);
    }

    // -- Construction tests --

    #[test]
    fn new_creates_inactive() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w), 80);
        assert!(!live.is_started());
    }

    // -- Start/stop lifecycle --

    #[test]
    fn start_hides_cursor() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        live.start().unwrap();
        assert!(live.is_started());
        assert!(
            w.output().contains("\x1b[?25l"),
            "Should contain hide cursor"
        );
        live.stop().unwrap();
    }

    #[test]
    fn stop_shows_cursor() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        live.start().unwrap();
        w.clear();
        live.stop().unwrap();
        assert!(!live.is_started());
        assert!(
            w.output().contains("\x1b[?25h"),
            "Should contain show cursor"
        );
    }

    #[test]
    fn start_is_idempotent() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        live.start().unwrap();
        let first_output = w.output();
        live.start().unwrap(); // Should be no-op
        assert_eq!(
            w.output(),
            first_output,
            "Second start should not write anything"
        );
        live.stop().unwrap();
    }

    #[test]
    fn stop_is_idempotent() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        live.start().unwrap();
        live.stop().unwrap();
        w.clear();
        live.stop().unwrap(); // Should be no-op
        assert!(
            w.output().is_empty(),
            "Second stop should not write anything"
        );
    }

    #[test]
    fn stop_refresh_thread_without_start_is_safe() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w), 80);
        live.stop_refresh_thread();
    }

    #[test]
    fn start_auto_refresh_ignores_non_positive_rate() {
        let w = TestWriter::new();
        let cfg = LiveConfig {
            refresh_per_second: 0.0,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w), 80, cfg);

        live.start_auto_refresh(|| {});

        let refresh = live.refresh_thread.lock().unwrap();
        assert!(refresh.is_none(), "no refresh thread should be spawned");
    }

    #[test]
    fn stop_stops_refresh_thread_even_when_not_started() {
        let w = TestWriter::new();
        let cfg = LiveConfig {
            refresh_per_second: 200.0,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w), 80, cfg);

        let ticks = Arc::new(AtomicUsize::new(0));
        let ticks_clone = Arc::clone(&ticks);
        live.start_auto_refresh(move || {
            ticks_clone.fetch_add(1, Ordering::Relaxed);
        });

        {
            let refresh = live.refresh_thread.lock().unwrap();
            assert!(refresh.is_some(), "refresh thread should be active");
        }

        std::thread::sleep(Duration::from_millis(30));
        live.stop().unwrap();

        let after_stop = ticks.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(
            ticks.load(Ordering::Relaxed),
            after_stop,
            "refresh callback should not run after stop()"
        );

        let refresh = live.refresh_thread.lock().unwrap();
        assert!(
            refresh.is_none(),
            "refresh thread should be joined and cleared"
        );
    }

    #[test]
    fn drop_stops_live() {
        let w = TestWriter::new();
        {
            let live = Live::new(Box::new(w.clone()), 80);
            live.start().unwrap();
            // Drop here
        }
        assert!(w.output().contains("\x1b[?25h"), "Drop should show cursor");
    }

    // -- Update tests --

    #[test]
    fn update_writes_content() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        live.start().unwrap();
        w.clear();

        live.update(|console| {
            console.print(Segment::text("Hello"));
            console.newline();
        });

        assert!(w.output().contains("Hello"), "Should contain rendered text");
        live.stop().unwrap();
    }

    #[test]
    fn update_sanitizes_escape_injection_payloads() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        live.start().unwrap();
        w.clear();

        live.update(|console| {
            console.print(Segment::text("safe\x1b]52;c;SGVsbG8=\x1b\\tail"));
            console.newline();
        });

        let output = w.output();
        assert!(
            output.contains("safetail"),
            "sanitized output should preserve visible payload"
        );
        assert!(
            !output.contains("\x1b]52"),
            "OSC 52 payload must not be emitted from live output"
        );
        assert!(
            !output.contains("SGVsbG8="),
            "clipboard base64 payload must be stripped"
        );
        live.stop().unwrap();
    }

    #[test]
    fn update_when_stopped_is_noop() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        // Don't start

        live.update(|console| {
            console.print(Segment::text("Should not appear"));
            console.newline();
        });

        assert!(!w.output().contains("Should not appear"));
    }

    #[test]
    fn clear_when_stopped_is_noop() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        live.clear().unwrap();
        assert!(w.output().is_empty());
    }

    #[test]
    fn multiple_updates_reposition_cursor() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        live.start().unwrap();
        w.clear();

        live.update(|console| {
            console.print(Segment::text("Line 1"));
            console.newline();
            console.print(Segment::text("Line 2"));
            console.newline();
        });

        w.clear();

        live.update(|console| {
            console.print(Segment::text("Updated 1"));
            console.newline();
            console.print(Segment::text("Updated 2"));
            console.newline();
        });

        let output = w.output();
        assert!(output.contains("Updated 1"));
        assert!(output.contains("Updated 2"));
        // Should contain cursor up escape (repositioning)
        assert!(output.contains("\x1b["), "Should contain ANSI escapes");
        live.stop().unwrap();
    }

    // -- Overflow tests --

    #[test]
    fn overflow_crop_truncates() {
        let w = TestWriter::new();
        let config = LiveConfig {
            max_height: 2,
            overflow: VerticalOverflow::Crop,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w.clone()), 80, config);
        live.start().unwrap();
        w.clear();

        live.update(|console| {
            console.print(Segment::text("Line 1"));
            console.newline();
            console.print(Segment::text("Line 2"));
            console.newline();
            console.print(Segment::text("Line 3"));
            console.newline();
        });

        let output = w.output();
        assert!(output.contains("Line 1"));
        assert!(output.contains("Line 2"));
        assert!(!output.contains("Line 3"), "Line 3 should be cropped");
        live.stop().unwrap();
    }

    #[test]
    fn overflow_ellipsis_adds_dots() {
        let w = TestWriter::new();
        let config = LiveConfig {
            max_height: 2,
            overflow: VerticalOverflow::Ellipsis,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w.clone()), 80, config);
        live.start().unwrap();
        w.clear();

        live.update(|console| {
            console.print(Segment::text("Line 1"));
            console.newline();
            console.print(Segment::text("Line 2"));
            console.newline();
            console.print(Segment::text("Line 3"));
            console.newline();
        });

        let output = w.output();
        assert!(output.contains("Line 1"));
        assert!(output.contains("..."), "Should show ellipsis");
        assert!(!output.contains("Line 3"), "Line 3 should be hidden");
        live.stop().unwrap();
    }

    #[test]
    fn overflow_visible_shows_all() {
        let w = TestWriter::new();
        let config = LiveConfig {
            max_height: 2,
            overflow: VerticalOverflow::Visible,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w.clone()), 80, config);
        live.start().unwrap();
        w.clear();

        live.update(|console| {
            console.print(Segment::text("Line 1"));
            console.newline();
            console.print(Segment::text("Line 2"));
            console.newline();
            console.print(Segment::text("Line 3"));
            console.newline();
        });

        let output = w.output();
        assert!(output.contains("Line 1"));
        assert!(output.contains("Line 2"));
        assert!(output.contains("Line 3"), "All lines should be visible");
        live.stop().unwrap();
    }

    #[test]
    fn no_overflow_when_within_limit() {
        let w = TestWriter::new();
        let config = LiveConfig {
            max_height: 5,
            overflow: VerticalOverflow::Ellipsis,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w.clone()), 80, config);
        live.start().unwrap();
        w.clear();

        live.update(|console| {
            console.print(Segment::text("Short"));
            console.newline();
        });

        let output = w.output();
        assert!(output.contains("Short"));
        assert!(!output.contains("..."), "Should not show ellipsis");
        live.stop().unwrap();
    }

    // -- Clear test --

    #[test]
    fn clear_erases_region() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        live.start().unwrap();

        live.update(|console| {
            console.print(Segment::text("To be cleared"));
            console.newline();
        });

        w.clear();
        live.clear().unwrap();

        let output = w.output();
        // Should contain erase line escapes
        assert!(output.contains("\x1b[2K"), "Should contain erase line");
        live.stop().unwrap();
    }

    // -- Thread safety --

    #[test]
    fn live_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<Live>();
        assert_sync::<Live>();
    }

    // -- Transient vs non-transient --

    #[test]
    fn non_transient_stop_preserves_output() {
        let w = TestWriter::new();
        let config = LiveConfig {
            transient: false,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w.clone()), 80, config);
        live.start().unwrap();

        live.update(|console| {
            console.print(Segment::text("Persistent"));
            console.newline();
        });

        w.clear();
        live.stop().unwrap();

        let output = w.output();
        // Non-transient should NOT erase the region
        assert!(!output.contains("\x1b[2K"), "Should not erase lines");
        assert!(output.contains("\x1b[?25h"), "Should show cursor");
    }

    #[test]
    fn transient_stop_erases_output() {
        let w = TestWriter::new();
        let config = LiveConfig {
            transient: true,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w.clone()), 80, config);
        live.start().unwrap();

        live.update(|console| {
            console.print(Segment::text("Temporary"));
            console.newline();
        });

        w.clear();
        live.stop().unwrap();

        let output = w.output();
        // Transient should erase the region
        assert!(output.contains("\x1b[2K"), "Should erase lines");
    }

    // -- ANSI escape helper tests --

    #[test]
    fn cursor_up_writes_escape() {
        let mut buf = Vec::new();
        cursor_up(&mut buf, 3).unwrap();
        assert_eq!(String::from_utf8_lossy(&buf), "\x1b[3A");
    }

    #[test]
    fn cursor_up_zero_is_noop() {
        let mut buf = Vec::new();
        cursor_up(&mut buf, 0).unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn erase_line_writes_escape() {
        let mut buf = Vec::new();
        erase_line(&mut buf).unwrap();
        assert_eq!(String::from_utf8_lossy(&buf), "\x1b[2K");
    }

    #[test]
    fn hide_show_cursor_escapes() {
        let mut buf = Vec::new();
        hide_cursor(&mut buf).unwrap();
        assert_eq!(String::from_utf8_lossy(&buf), "\x1b[?25l");

        buf.clear();
        show_cursor(&mut buf).unwrap();
        assert_eq!(String::from_utf8_lossy(&buf), "\x1b[?25h");
    }

    // --- Additional edge case tests (bd-1kziq) ---

    #[test]
    fn carriage_return_writes_escape() {
        let mut buf = Vec::new();
        carriage_return(&mut buf).unwrap();
        assert_eq!(String::from_utf8_lossy(&buf), "\r");
    }

    #[test]
    fn with_config_custom_values() {
        let w = TestWriter::new();
        let config = LiveConfig {
            max_height: 10,
            overflow: VerticalOverflow::Crop,
            transient: false,
            refresh_per_second: 30.0,
        };
        let live = Live::with_config(Box::new(w), 120, config);
        assert!(!live.is_started());
        assert_eq!(live.width, 120);
        assert_eq!(live.config.max_height, 10);
        assert_eq!(live.config.overflow, VerticalOverflow::Crop);
        assert!(!live.config.transient);
    }

    #[test]
    fn auto_refresh_ignores_nan_rate() {
        let w = TestWriter::new();
        let cfg = LiveConfig {
            refresh_per_second: f64::NAN,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w), 80, cfg);
        live.start_auto_refresh(|| {});
        let refresh = live.refresh_thread.lock().unwrap();
        assert!(refresh.is_none(), "NaN rate should not spawn thread");
    }

    #[test]
    fn auto_refresh_ignores_negative_rate() {
        let w = TestWriter::new();
        let cfg = LiveConfig {
            refresh_per_second: -5.0,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w), 80, cfg);
        live.start_auto_refresh(|| {});
        let refresh = live.refresh_thread.lock().unwrap();
        assert!(refresh.is_none(), "negative rate should not spawn thread");
    }

    #[test]
    fn auto_refresh_ignores_infinity_rate() {
        let w = TestWriter::new();
        let cfg = LiveConfig {
            refresh_per_second: f64::INFINITY,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w), 80, cfg);
        live.start_auto_refresh(|| {});
        let refresh = live.refresh_thread.lock().unwrap();
        assert!(refresh.is_none(), "infinite rate should not spawn thread");
    }

    #[test]
    fn auto_refresh_tiny_positive_rate_stops_promptly() {
        let w = TestWriter::new();
        let cfg = LiveConfig {
            refresh_per_second: f64::MIN_POSITIVE,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w), 80, cfg);
        live.start_auto_refresh(|| {});

        {
            let refresh = live.refresh_thread.lock().unwrap();
            assert!(
                refresh.is_some(),
                "tiny positive rate should still spawn thread"
            );
        }

        let started = std::time::Instant::now();
        live.stop_refresh_thread();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "stop_refresh_thread should not block on huge sleep intervals: {elapsed:?}"
        );

        let refresh = live.refresh_thread.lock().unwrap();
        assert!(refresh.is_none(), "refresh thread should be stopped");
    }

    #[test]
    fn update_shrinks_height_erases_extra() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        live.start().unwrap();

        // First: 3 lines
        live.update(|console| {
            console.print(Segment::text("A"));
            console.newline();
            console.print(Segment::text("B"));
            console.newline();
            console.print(Segment::text("C"));
            console.newline();
        });

        w.clear();

        // Second: 1 line (shrink)
        live.update(|console| {
            console.print(Segment::text("Only"));
            console.newline();
        });

        let output = w.output();
        assert!(output.contains("Only"));
        // Should contain erase escapes for the extra lines
        assert!(output.contains("\x1b[2K"), "Should erase extra lines");
        live.stop().unwrap();
    }

    #[test]
    fn empty_update_writes_nothing() {
        let w = TestWriter::new();
        let live = Live::new(Box::new(w.clone()), 80);
        live.start().unwrap();
        w.clear();

        live.update(|_console| {
            // Write nothing
        });

        // Even with empty content, the function may write erase sequences,
        // but should not panic or produce text content
        live.stop().unwrap();
    }

    #[test]
    fn max_height_zero_means_unlimited() {
        let w = TestWriter::new();
        let config = LiveConfig {
            max_height: 0,
            overflow: VerticalOverflow::Crop,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w.clone()), 80, config);
        live.start().unwrap();
        w.clear();

        live.update(|console| {
            for i in 0..10 {
                console.print(Segment::text(format!("Line {i}")));
                console.newline();
            }
        });

        let output = w.output();
        // All 10 lines should appear (max_height=0 means no limit)
        assert!(output.contains("Line 0"));
        assert!(output.contains("Line 9"));
        live.stop().unwrap();
    }

    #[test]
    fn overflow_ellipsis_max_height_1() {
        let w = TestWriter::new();
        let config = LiveConfig {
            max_height: 1,
            overflow: VerticalOverflow::Ellipsis,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w.clone()), 80, config);
        live.start().unwrap();
        w.clear();

        live.update(|console| {
            console.print(Segment::text("Line 1"));
            console.newline();
            console.print(Segment::text("Line 2"));
            console.newline();
        });

        let output = w.output();
        // With max_height=1 and ellipsis, should show "..."
        assert!(output.contains("..."));
        assert!(!output.contains("Line 1"), "Should be truncated");
        live.stop().unwrap();
    }

    #[test]
    fn overflow_crop_max_height_1() {
        let w = TestWriter::new();
        let config = LiveConfig {
            max_height: 1,
            overflow: VerticalOverflow::Crop,
            ..Default::default()
        };
        let live = Live::with_config(Box::new(w.clone()), 80, config);
        live.start().unwrap();
        w.clear();

        live.update(|console| {
            console.print(Segment::text("First"));
            console.newline();
            console.print(Segment::text("Second"));
            console.newline();
        });

        let output = w.output();
        assert!(output.contains("First"));
        assert!(!output.contains("Second"), "Should be cropped at 1");
        live.stop().unwrap();
    }

    #[test]
    fn live_config_clone() {
        let cfg = LiveConfig {
            max_height: 5,
            overflow: VerticalOverflow::Crop,
            transient: false,
            refresh_per_second: 10.0,
        };
        let cloned = cfg.clone();
        assert_eq!(cloned.max_height, 5);
        assert_eq!(cloned.overflow, VerticalOverflow::Crop);
        assert!(!cloned.transient);
    }

    #[test]
    fn live_config_debug() {
        let cfg = LiveConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("LiveConfig"));
    }

    #[test]
    fn vertical_overflow_debug() {
        let dbg = format!("{:?}", VerticalOverflow::Ellipsis);
        assert!(dbg.contains("Ellipsis"));
    }

    #[test]
    fn vertical_overflow_eq() {
        assert_eq!(VerticalOverflow::Crop, VerticalOverflow::Crop);
        assert_ne!(VerticalOverflow::Crop, VerticalOverflow::Ellipsis);
        assert_ne!(VerticalOverflow::Visible, VerticalOverflow::Crop);
    }
}

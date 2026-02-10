#![forbid(unsafe_code)]

//! Counting writer for tracking bytes emitted.
//!
//! This module provides a wrapper around any `Write` implementation that
//! counts the number of bytes written. This is used to verify O(changes)
//! output size for diff-based rendering.
//!
//! # Usage
//!
//! ```
//! use ftui_render::counting_writer::CountingWriter;
//! use std::io::Write;
//!
//! let mut buffer = Vec::new();
//! let mut writer = CountingWriter::new(&mut buffer);
//!
//! writer.write_all(b"Hello, world!").unwrap();
//! assert_eq!(writer.bytes_written(), 13);
//!
//! writer.reset_counter();
//! writer.write_all(b"Hi").unwrap();
//! assert_eq!(writer.bytes_written(), 2);
//! ```

use std::io::{self, Write};
use std::time::{Duration, Instant};

/// A write wrapper that counts bytes written.
///
/// Wraps any `Write` implementation and tracks the total number of bytes
/// written through it. The counter can be reset between operations.
#[derive(Debug)]
pub struct CountingWriter<W> {
    /// The underlying writer.
    inner: W,
    /// Total bytes written since last reset.
    bytes_written: u64,
}

impl<W> CountingWriter<W> {
    /// Create a new counting writer wrapping the given writer.
    #[inline]
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            bytes_written: 0,
        }
    }

    /// Get the number of bytes written since the last reset.
    #[inline]
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Reset the byte counter to zero.
    #[inline]
    pub fn reset_counter(&mut self) {
        self.bytes_written = 0;
    }

    /// Get a reference to the underlying writer.
    #[inline]
    pub fn inner(&self) -> &W {
        &self.inner
    }

    /// Get a mutable reference to the underlying writer.
    #[inline]
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Consume the counting writer and return the inner writer.
    #[inline]
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes_written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.inner.write_all(buf)?;
        self.bytes_written += buf.len() as u64;
        Ok(())
    }
}

/// Statistics from a present() operation.
///
/// Captures metrics for verifying O(changes) output size and detecting
/// performance regressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentStats {
    /// Bytes emitted for this frame.
    pub bytes_emitted: u64,
    /// Number of cells changed.
    pub cells_changed: usize,
    /// Number of runs (groups of consecutive changes).
    pub run_count: usize,
    /// Time spent in present().
    pub duration: Duration,
}

impl PresentStats {
    /// Create new stats with the given values.
    #[inline]
    pub fn new(
        bytes_emitted: u64,
        cells_changed: usize,
        run_count: usize,
        duration: Duration,
    ) -> Self {
        Self {
            bytes_emitted,
            cells_changed,
            run_count,
            duration,
        }
    }

    /// Calculate bytes per cell changed.
    ///
    /// Returns 0.0 if no cells were changed.
    #[inline]
    pub fn bytes_per_cell(&self) -> f64 {
        if self.cells_changed == 0 {
            0.0
        } else {
            self.bytes_emitted as f64 / self.cells_changed as f64
        }
    }

    /// Calculate bytes per run.
    ///
    /// Returns 0.0 if no runs.
    #[inline]
    pub fn bytes_per_run(&self) -> f64 {
        if self.run_count == 0 {
            0.0
        } else {
            self.bytes_emitted as f64 / self.run_count as f64
        }
    }

    /// Check if output is within the expected budget.
    ///
    /// Uses conservative estimates for worst-case bytes per cell.
    #[inline]
    pub fn within_budget(&self) -> bool {
        let budget = expected_max_bytes(self.cells_changed, self.run_count);
        self.bytes_emitted <= budget
    }

    /// Log stats at debug level (requires tracing feature).
    #[cfg(feature = "tracing")]
    pub fn log(&self) {
        tracing::debug!(
            bytes = self.bytes_emitted,
            cells_changed = self.cells_changed,
            runs = self.run_count,
            duration_us = self.duration.as_micros() as u64,
            bytes_per_cell = format!("{:.1}", self.bytes_per_cell()),
            "Present stats"
        );
    }

    /// Log stats at debug level (no-op without tracing feature).
    #[cfg(not(feature = "tracing"))]
    pub fn log(&self) {
        // No-op without tracing
    }
}

impl Default for PresentStats {
    fn default() -> Self {
        Self {
            bytes_emitted: 0,
            cells_changed: 0,
            run_count: 0,
            duration: Duration::ZERO,
        }
    }
}

/// Expected bytes per cell change (approximate worst case).
///
/// Worst case: cursor move (10) + full SGR reset+apply (25) + 4-byte UTF-8 char
pub const BYTES_PER_CELL_MAX: u64 = 40;

/// Bytes for sync output wrapper.
pub const SYNC_OVERHEAD: u64 = 20;

/// Bytes for cursor move sequence (CUP).
pub const BYTES_PER_CURSOR_MOVE: u64 = 10;

/// Calculate expected maximum bytes for a frame with given changes.
///
/// This is a conservative budget for regression testing.
#[inline]
pub fn expected_max_bytes(cells_changed: usize, runs: usize) -> u64 {
    // cursor move per run + cells * max_per_cell + sync overhead
    (runs as u64 * BYTES_PER_CURSOR_MOVE)
        + (cells_changed as u64 * BYTES_PER_CELL_MAX)
        + SYNC_OVERHEAD
}

/// A stats collector for measuring present operations.
///
/// Use this to wrap present() calls and collect statistics.
#[derive(Debug)]
pub struct StatsCollector {
    start: Instant,
    cells_changed: usize,
    run_count: usize,
}

impl StatsCollector {
    /// Start collecting stats for a present operation.
    #[inline]
    pub fn start(cells_changed: usize, run_count: usize) -> Self {
        Self {
            start: Instant::now(),
            cells_changed,
            run_count,
        }
    }

    /// Finish collecting and return stats.
    #[inline]
    pub fn finish(self, bytes_emitted: u64) -> PresentStats {
        PresentStats {
            bytes_emitted,
            cells_changed: self.cells_changed,
            run_count: self.run_count,
            duration: self.start.elapsed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============== CountingWriter Tests ==============

    #[test]
    fn counting_writer_basic() {
        let mut buffer = Vec::new();
        let mut writer = CountingWriter::new(&mut buffer);

        writer.write_all(b"Hello").unwrap();
        assert_eq!(writer.bytes_written(), 5);

        writer.write_all(b", world!").unwrap();
        assert_eq!(writer.bytes_written(), 13);
    }

    #[test]
    fn counting_writer_reset() {
        let mut buffer = Vec::new();
        let mut writer = CountingWriter::new(&mut buffer);

        writer.write_all(b"Hello").unwrap();
        assert_eq!(writer.bytes_written(), 5);

        writer.reset_counter();
        assert_eq!(writer.bytes_written(), 0);

        writer.write_all(b"Hi").unwrap();
        assert_eq!(writer.bytes_written(), 2);
    }

    #[test]
    fn counting_writer_write() {
        let mut buffer = Vec::new();
        let mut writer = CountingWriter::new(&mut buffer);

        // write() may write partial buffer
        let n = writer.write(b"Hello").unwrap();
        assert_eq!(n, 5);
        assert_eq!(writer.bytes_written(), 5);
    }

    #[test]
    fn counting_writer_flush() {
        let mut buffer = Vec::new();
        let mut writer = CountingWriter::new(&mut buffer);

        writer.write_all(b"test").unwrap();
        writer.flush().unwrap();

        // flush doesn't change byte count
        assert_eq!(writer.bytes_written(), 4);
    }

    #[test]
    fn counting_writer_into_inner() {
        let buffer: Vec<u8> = Vec::new();
        let writer = CountingWriter::new(buffer);
        let inner = writer.into_inner();
        assert!(inner.is_empty());
    }

    #[test]
    fn counting_writer_inner_ref() {
        let mut buffer = Vec::new();
        let mut writer = CountingWriter::new(&mut buffer);
        writer.write_all(b"test").unwrap();

        assert_eq!(writer.inner().len(), 4);
    }

    // ============== PresentStats Tests ==============

    #[test]
    fn stats_bytes_per_cell() {
        let stats = PresentStats::new(100, 10, 2, Duration::from_micros(50));
        assert!((stats.bytes_per_cell() - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_bytes_per_cell_zero() {
        let stats = PresentStats::new(0, 0, 0, Duration::ZERO);
        assert!((stats.bytes_per_cell() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_bytes_per_run() {
        let stats = PresentStats::new(100, 10, 5, Duration::from_micros(50));
        assert!((stats.bytes_per_run() - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_bytes_per_run_zero() {
        let stats = PresentStats::new(0, 0, 0, Duration::ZERO);
        assert!((stats.bytes_per_run() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_within_budget_pass() {
        // 10 cells, 2 runs
        // Budget = 2*10 + 10*40 + 20 = 440
        let stats = PresentStats::new(200, 10, 2, Duration::from_micros(50));
        assert!(stats.within_budget());
    }

    #[test]
    fn stats_within_budget_fail() {
        // 10 cells, 2 runs
        // Budget = 2*10 + 10*40 + 20 = 440
        let stats = PresentStats::new(500, 10, 2, Duration::from_micros(50));
        assert!(!stats.within_budget());
    }

    #[test]
    fn stats_default() {
        let stats = PresentStats::default();
        assert_eq!(stats.bytes_emitted, 0);
        assert_eq!(stats.cells_changed, 0);
        assert_eq!(stats.run_count, 0);
        assert_eq!(stats.duration, Duration::ZERO);
    }

    // ============== Budget Calculation Tests ==============

    #[test]
    fn expected_max_bytes_calculation() {
        // 10 cells, 2 runs
        let budget = expected_max_bytes(10, 2);
        // 2*10 + 10*40 + 20 = 440
        assert_eq!(budget, 440);
    }

    #[test]
    fn expected_max_bytes_empty() {
        let budget = expected_max_bytes(0, 0);
        // Just sync overhead
        assert_eq!(budget, SYNC_OVERHEAD);
    }

    #[test]
    fn expected_max_bytes_single_cell() {
        let budget = expected_max_bytes(1, 1);
        // 1*10 + 1*40 + 20 = 70
        assert_eq!(budget, 70);
    }

    // ============== StatsCollector Tests ==============

    #[test]
    fn stats_collector_basic() {
        let collector = StatsCollector::start(10, 2);
        std::thread::sleep(Duration::from_micros(100));
        let stats = collector.finish(150);

        assert_eq!(stats.cells_changed, 10);
        assert_eq!(stats.run_count, 2);
        assert_eq!(stats.bytes_emitted, 150);
        assert!(stats.duration >= Duration::from_micros(100));
    }

    // ============== Integration Tests ==============

    #[test]
    fn full_stats_workflow() {
        let mut buffer = Vec::new();
        let mut writer = CountingWriter::new(&mut buffer);

        // Simulate present operation
        let collector = StatsCollector::start(5, 1);

        writer.write_all(b"\x1b[1;1H").unwrap(); // CUP
        writer.write_all(b"\x1b[0m").unwrap(); // SGR reset
        writer.write_all(b"Hello").unwrap(); // Content
        writer.flush().unwrap();

        let stats = collector.finish(writer.bytes_written());

        assert_eq!(stats.cells_changed, 5);
        assert_eq!(stats.run_count, 1);
        assert_eq!(stats.bytes_emitted, 6 + 4 + 5); // 15 bytes
        assert!(stats.within_budget());
    }

    #[test]
    fn spinner_update_budget() {
        // Single cell update should be well under budget
        let stats = PresentStats::new(35, 1, 1, Duration::from_micros(10));
        assert!(
            stats.within_budget(),
            "Single cell update should be within budget"
        );
        assert!(
            stats.bytes_emitted < 50,
            "Spinner tick should be < 50 bytes"
        );
    }

    #[test]
    fn status_bar_budget() {
        // 80-column status bar
        let stats = PresentStats::new(2500, 80, 1, Duration::from_micros(100));
        assert!(
            stats.within_budget(),
            "Status bar update should be within budget"
        );
        assert!(
            stats.bytes_emitted < 3500,
            "Status bar should be < 3500 bytes"
        );
    }

    #[test]
    fn full_redraw_budget() {
        // Full 80x24 screen
        let stats = PresentStats::new(50000, 1920, 24, Duration::from_micros(1000));
        assert!(stats.within_budget(), "Full redraw should be within budget");
        assert!(stats.bytes_emitted < 80000, "Full redraw should be < 80KB");
    }

    // --- CountingWriter edge cases ---

    #[test]
    fn counting_writer_debug() {
        let buffer: Vec<u8> = Vec::new();
        let writer = CountingWriter::new(buffer);
        let dbg = format!("{:?}", writer);
        assert!(dbg.contains("CountingWriter"), "Debug: {dbg}");
    }

    #[test]
    fn counting_writer_inner_mut() {
        let mut writer = CountingWriter::new(Vec::<u8>::new());
        writer.write_all(b"hello").unwrap();
        // Modify inner via inner_mut
        writer.inner_mut().push(b'!');
        assert_eq!(writer.inner(), &b"hello!"[..]);
        // Byte counter unchanged by direct inner manipulation
        assert_eq!(writer.bytes_written(), 5);
    }

    #[test]
    fn counting_writer_empty_write() {
        let mut buffer = Vec::new();
        let mut writer = CountingWriter::new(&mut buffer);
        writer.write_all(b"").unwrap();
        assert_eq!(writer.bytes_written(), 0);
        let n = writer.write(b"").unwrap();
        assert_eq!(n, 0);
        assert_eq!(writer.bytes_written(), 0);
    }

    #[test]
    fn counting_writer_multiple_resets() {
        let mut buffer = Vec::new();
        let mut writer = CountingWriter::new(&mut buffer);
        writer.write_all(b"abc").unwrap();
        writer.reset_counter();
        writer.reset_counter();
        assert_eq!(writer.bytes_written(), 0);
        writer.write_all(b"de").unwrap();
        assert_eq!(writer.bytes_written(), 2);
    }

    #[test]
    fn counting_writer_accumulates_u64() {
        let mut buffer = Vec::new();
        let mut writer = CountingWriter::new(&mut buffer);
        // Write enough to test u64 accumulation (though not near overflow)
        for _ in 0..1000 {
            writer.write_all(b"x").unwrap();
        }
        assert_eq!(writer.bytes_written(), 1000);
    }

    #[test]
    fn counting_writer_multiple_flushes() {
        let mut buffer = Vec::new();
        let mut writer = CountingWriter::new(&mut buffer);
        writer.write_all(b"test").unwrap();
        writer.flush().unwrap();
        writer.flush().unwrap();
        writer.flush().unwrap();
        assert_eq!(writer.bytes_written(), 4);
    }

    #[test]
    fn counting_writer_into_inner_preserves_data() {
        let mut writer = CountingWriter::new(Vec::<u8>::new());
        writer.write_all(b"hello world").unwrap();
        let inner = writer.into_inner();
        assert_eq!(&inner, b"hello world");
    }

    #[test]
    fn counting_writer_initial_state() {
        let buffer: Vec<u8> = Vec::new();
        let writer = CountingWriter::new(buffer);
        assert_eq!(writer.bytes_written(), 0);
        assert!(writer.inner().is_empty());
    }

    // --- PresentStats edge cases ---

    #[test]
    fn present_stats_debug_clone_eq() {
        let a = PresentStats::new(100, 10, 2, Duration::from_micros(50));
        let dbg = format!("{:?}", a);
        assert!(dbg.contains("PresentStats"), "Debug: {dbg}");
        let cloned = a.clone();
        assert_eq!(a, cloned);
        let b = PresentStats::new(200, 10, 2, Duration::from_micros(50));
        assert_ne!(a, b);
    }

    #[test]
    fn present_stats_log_noop() {
        let stats = PresentStats::default();
        stats.log(); // Should not panic (noop without tracing)
    }

    #[test]
    fn present_stats_large_values() {
        let stats = PresentStats::new(u64::MAX, usize::MAX, usize::MAX, Duration::MAX);
        assert_eq!(stats.bytes_emitted, u64::MAX);
        assert_eq!(stats.cells_changed, usize::MAX);
    }

    #[test]
    fn present_stats_bytes_per_cell_fractional() {
        let stats = PresentStats::new(10, 3, 1, Duration::ZERO);
        let bpc = stats.bytes_per_cell();
        assert!((bpc - 3.333333333).abs() < 0.001);
    }

    #[test]
    fn present_stats_bytes_per_run_fractional() {
        let stats = PresentStats::new(10, 5, 3, Duration::ZERO);
        let bpr = stats.bytes_per_run();
        assert!((bpr - 3.333333333).abs() < 0.001);
    }

    #[test]
    fn present_stats_within_budget_at_exact_boundary() {
        // Budget for 10 cells, 2 runs: 2*10 + 10*40 + 20 = 440
        let budget = expected_max_bytes(10, 2);
        assert_eq!(budget, 440);

        let at_boundary = PresentStats::new(440, 10, 2, Duration::ZERO);
        assert!(at_boundary.within_budget());

        let over_boundary = PresentStats::new(441, 10, 2, Duration::ZERO);
        assert!(!over_boundary.within_budget());
    }

    // --- Constants ---

    #[test]
    fn constants_values() {
        assert_eq!(BYTES_PER_CELL_MAX, 40);
        assert_eq!(SYNC_OVERHEAD, 20);
        assert_eq!(BYTES_PER_CURSOR_MOVE, 10);
    }

    // --- expected_max_bytes edge cases ---

    #[test]
    fn expected_max_bytes_many_runs_few_cells() {
        // 1 cell, 100 runs (pathological case)
        let budget = expected_max_bytes(1, 100);
        // 100*10 + 1*40 + 20 = 1060
        assert_eq!(budget, 1060);
    }

    #[test]
    fn expected_max_bytes_many_cells_one_run() {
        let budget = expected_max_bytes(1000, 1);
        // 1*10 + 1000*40 + 20 = 40030
        assert_eq!(budget, 40030);
    }

    // --- StatsCollector edge cases ---

    #[test]
    fn stats_collector_debug() {
        let collector = StatsCollector::start(5, 2);
        let dbg = format!("{:?}", collector);
        assert!(dbg.contains("StatsCollector"), "Debug: {dbg}");
    }

    #[test]
    fn stats_collector_zero_cells_runs() {
        let collector = StatsCollector::start(0, 0);
        let stats = collector.finish(0);
        assert_eq!(stats.cells_changed, 0);
        assert_eq!(stats.run_count, 0);
        assert_eq!(stats.bytes_emitted, 0);
        assert!(stats.within_budget()); // 0 <= SYNC_OVERHEAD
    }

    #[test]
    fn stats_collector_immediate_finish() {
        let collector = StatsCollector::start(1, 1);
        let stats = collector.finish(50);
        assert_eq!(stats.bytes_emitted, 50);
        // Duration should be very small (near zero)
        assert!(stats.duration < Duration::from_millis(100));
    }
}

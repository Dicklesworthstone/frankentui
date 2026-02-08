//! Frame-time measurement harness for FrankenTerm web renderer.
//!
//! Provides reusable types for collecting, summarising, and exporting per-frame
//! performance metrics.  The harness is platform-agnostic: it records raw
//! `Duration` samples and computes histograms / JSONL output without depending
//! on any GPU API.
//!
//! # Usage
//!
//! ```ignore
//! let mut collector = FrameTimeCollector::new("renderer_bench", 80, 24);
//!
//! for _ in 0..100 {
//!     let start = Instant::now();
//!     // ... render frame ...
//!     collector.record_frame(FrameRecord {
//!         elapsed: start.elapsed(),
//!         dirty_cells: 42,
//!         patch_count: 3,
//!         bytes_uploaded: 42 * 16,
//!     });
//! }
//!
//! let report = collector.report();
//! println!("{}", report.to_json());
//! ```

use serde::Serialize;
use std::time::Duration;

/// A single frame's measurements.
#[derive(Debug, Clone, Copy)]
pub struct FrameRecord {
    /// Wall-clock time for the frame (CPU side).
    pub elapsed: Duration,
    /// Number of dirty cells updated this frame.
    pub dirty_cells: u32,
    /// Number of contiguous patches uploaded.
    pub patch_count: u32,
    /// Total bytes uploaded to the GPU this frame.
    pub bytes_uploaded: u64,
}

/// Collects per-frame records and produces summary statistics.
pub struct FrameTimeCollector {
    run_id: String,
    cols: u16,
    rows: u16,
    records: Vec<FrameRecord>,
}

impl FrameTimeCollector {
    /// Create a new collector for a benchmark run.
    #[must_use]
    pub fn new(run_id: &str, cols: u16, rows: u16) -> Self {
        Self {
            run_id: run_id.to_string(),
            cols,
            rows,
            records: Vec::with_capacity(1024),
        }
    }

    /// Record one frame's measurements.
    pub fn record_frame(&mut self, record: FrameRecord) {
        self.records.push(record);
    }

    /// Number of frames recorded so far.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.records.len()
    }

    /// Produce a summary report from all recorded frames.
    #[must_use]
    pub fn report(&self) -> SessionReport {
        let mut times_us: Vec<u64> = self
            .records
            .iter()
            .map(|r| r.elapsed.as_micros() as u64)
            .collect();
        times_us.sort_unstable();

        let total_dirty: u64 = self.records.iter().map(|r| r.dirty_cells as u64).sum();
        let total_patches: u64 = self.records.iter().map(|r| r.patch_count as u64).sum();
        let total_bytes: u64 = self.records.iter().map(|r| r.bytes_uploaded).sum();

        let n = times_us.len();
        let histogram = if n == 0 {
            FrameTimeHistogram::default()
        } else {
            FrameTimeHistogram {
                count: n as u64,
                min_us: times_us[0],
                max_us: times_us[n - 1],
                p50_us: percentile(&times_us, 0.50),
                p95_us: percentile(&times_us, 0.95),
                p99_us: percentile(&times_us, 0.99),
                mean_us: if n > 0 {
                    times_us.iter().sum::<u64>() / n as u64
                } else {
                    0
                },
            }
        };

        SessionReport {
            run_id: self.run_id.clone(),
            cols: self.cols,
            rows: self.rows,
            frame_time: histogram,
            patch_stats: PatchStats {
                total_dirty_cells: total_dirty,
                total_patches,
                total_bytes_uploaded: total_bytes,
                avg_dirty_per_frame: if n > 0 {
                    total_dirty as f64 / n as f64
                } else {
                    0.0
                },
                avg_patches_per_frame: if n > 0 {
                    total_patches as f64 / n as f64
                } else {
                    0.0
                },
                avg_bytes_per_frame: if n > 0 {
                    total_bytes as f64 / n as f64
                } else {
                    0.0
                },
            },
        }
    }

    /// Emit per-frame JSONL records to a string.
    ///
    /// Each line is a JSON object with `run_id`, `frame_idx`, `elapsed_us`,
    /// `dirty_cells`, `patch_count`, and `bytes_uploaded`.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        for (i, r) in self.records.iter().enumerate() {
            let line = format!(
                "{{\"run_id\":\"{}\",\"cols\":{},\"rows\":{},\"frame_idx\":{},\"elapsed_us\":{},\"dirty_cells\":{},\"patch_count\":{},\"bytes_uploaded\":{}}}\n",
                self.run_id,
                self.cols,
                self.rows,
                i,
                r.elapsed.as_micros(),
                r.dirty_cells,
                r.patch_count,
                r.bytes_uploaded,
            );
            out.push_str(&line);
        }
        out
    }
}

/// Percentile histogram of frame times.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct FrameTimeHistogram {
    pub count: u64,
    pub min_us: u64,
    pub max_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub mean_us: u64,
}

/// Aggregate patch upload statistics.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct PatchStats {
    pub total_dirty_cells: u64,
    pub total_patches: u64,
    pub total_bytes_uploaded: u64,
    pub avg_dirty_per_frame: f64,
    pub avg_patches_per_frame: f64,
    pub avg_bytes_per_frame: f64,
}

/// Complete session report with histogram and patch stats.
#[derive(Debug, Clone, Serialize)]
pub struct SessionReport {
    pub run_id: String,
    pub cols: u16,
    pub rows: u16,
    pub frame_time: FrameTimeHistogram,
    pub patch_stats: PatchStats,
}

impl SessionReport {
    /// Serialize to a JSON string (machine-readable for CI gating).
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 * p) as usize).min(sorted.len() - 1);
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_collector_produces_zero_report() {
        let c = FrameTimeCollector::new("test", 80, 24);
        let r = c.report();
        assert_eq!(r.frame_time.count, 0);
        assert_eq!(r.patch_stats.total_dirty_cells, 0);
    }

    #[test]
    fn single_frame_report() {
        let mut c = FrameTimeCollector::new("test", 80, 24);
        c.record_frame(FrameRecord {
            elapsed: Duration::from_micros(500),
            dirty_cells: 10,
            patch_count: 2,
            bytes_uploaded: 160,
        });

        let r = c.report();
        assert_eq!(r.frame_time.count, 1);
        assert_eq!(r.frame_time.p50_us, 500);
        assert_eq!(r.patch_stats.total_dirty_cells, 10);
        assert_eq!(r.patch_stats.total_patches, 2);
    }

    #[test]
    fn histogram_percentiles() {
        let mut c = FrameTimeCollector::new("test", 120, 40);
        // Record 100 frames with increasing latencies (1..=100 us).
        for i in 1..=100u64 {
            c.record_frame(FrameRecord {
                elapsed: Duration::from_micros(i),
                dirty_cells: 1,
                patch_count: 1,
                bytes_uploaded: 16,
            });
        }

        let r = c.report();
        assert_eq!(r.frame_time.count, 100);
        assert_eq!(r.frame_time.min_us, 1);
        assert_eq!(r.frame_time.max_us, 100);
        // p50 should be around 50.
        assert!(r.frame_time.p50_us >= 49 && r.frame_time.p50_us <= 51);
        // p95 should be around 95.
        assert!(r.frame_time.p95_us >= 94 && r.frame_time.p95_us <= 96);
        // p99 should be around 99.
        assert!(r.frame_time.p99_us >= 98 && r.frame_time.p99_us <= 100);
    }

    #[test]
    fn jsonl_output_has_correct_line_count() {
        let mut c = FrameTimeCollector::new("test", 80, 24);
        for _ in 0..5 {
            c.record_frame(FrameRecord {
                elapsed: Duration::from_micros(100),
                dirty_cells: 1,
                patch_count: 1,
                bytes_uploaded: 16,
            });
        }
        let jsonl = c.to_jsonl();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 5);
        // Each line should be valid JSON.
        for line in &lines {
            assert!(serde_json::from_str::<serde_json::Value>(line).is_ok());
        }
    }

    #[test]
    fn report_json_is_valid() {
        let mut c = FrameTimeCollector::new("test", 80, 24);
        c.record_frame(FrameRecord {
            elapsed: Duration::from_micros(123),
            dirty_cells: 5,
            patch_count: 1,
            bytes_uploaded: 80,
        });
        let json = c.report().to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["run_id"], "test");
        assert_eq!(parsed["cols"], 80);
        assert_eq!(parsed["rows"], 24);
    }

    #[test]
    fn patch_stats_averages() {
        let mut c = FrameTimeCollector::new("test", 80, 24);
        c.record_frame(FrameRecord {
            elapsed: Duration::from_micros(100),
            dirty_cells: 10,
            patch_count: 2,
            bytes_uploaded: 160,
        });
        c.record_frame(FrameRecord {
            elapsed: Duration::from_micros(200),
            dirty_cells: 20,
            patch_count: 4,
            bytes_uploaded: 320,
        });

        let r = c.report();
        assert!((r.patch_stats.avg_dirty_per_frame - 15.0).abs() < f64::EPSILON);
        assert!((r.patch_stats.avg_patches_per_frame - 3.0).abs() < f64::EPSILON);
        assert!((r.patch_stats.avg_bytes_per_frame - 240.0).abs() < f64::EPSILON);
    }
}

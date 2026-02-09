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
//!         cpu_submit: None,
//!         gpu_time: None,
//!         dirty_cells: 42,
//!         patch_count: 3,
//!         bytes_uploaded: 42 * 16,
//!     });
//! }
//!
//! let report = collector.report();
//! println!("{}", report.to_json());
//! ```

use crate::renderer::{CellData, GridGeometry};
use serde::Serialize;
use std::time::Duration;

const E2E_JSONL_SCHEMA_VERSION: &str = "e2e-jsonl-v1";
const FRAME_HASH_ALGO: &str = "fnv1a64";
const FNV64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01B3;

/// A single frame's measurements.
#[derive(Debug, Clone, Copy)]
pub struct FrameRecord {
    /// Wall-clock time for the frame (CPU side).
    pub elapsed: Duration,
    /// CPU submit time for this frame, if measured separately from total elapsed.
    pub cpu_submit: Option<Duration>,
    /// GPU execution time for this frame, if timestamp queries are available.
    pub gpu_time: Option<Duration>,
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
        let mut cpu_submit_us: Vec<u64> = self
            .records
            .iter()
            .filter_map(|r| r.cpu_submit.map(|d| d.as_micros() as u64))
            .collect();
        cpu_submit_us.sort_unstable();
        let mut gpu_time_us: Vec<u64> = self
            .records
            .iter()
            .filter_map(|r| r.gpu_time.map(|d| d.as_micros() as u64))
            .collect();
        gpu_time_us.sort_unstable();

        let total_dirty: u64 = self.records.iter().map(|r| r.dirty_cells as u64).sum();
        let total_patches: u64 = self.records.iter().map(|r| r.patch_count as u64).sum();
        let total_bytes: u64 = self.records.iter().map(|r| r.bytes_uploaded).sum();

        let n = times_us.len();
        let histogram = histogram_or_default(&times_us);

        SessionReport {
            run_id: self.run_id.clone(),
            cols: self.cols,
            rows: self.rows,
            frame_time: histogram,
            cpu_submit_time: optional_histogram(&cpu_submit_us),
            gpu_time: optional_histogram(&gpu_time_us),
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
            let row = JsonlFrameRecord {
                run_id: &self.run_id,
                cols: self.cols,
                rows: self.rows,
                frame_idx: i,
                elapsed_us: r.elapsed.as_micros() as u64,
                cpu_submit_us: r.cpu_submit.map(|d| d.as_micros() as u64),
                gpu_time_us: r.gpu_time.map(|d| d.as_micros() as u64),
                dirty_cells: r.dirty_cells,
                patch_count: r.patch_count,
                bytes_uploaded: r.bytes_uploaded,
            };
            if let Ok(line) = serde_json::to_string(&row) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        out
    }
}

#[derive(Debug, Serialize)]
struct JsonlFrameRecord<'a> {
    run_id: &'a str,
    cols: u16,
    rows: u16,
    frame_idx: usize,
    elapsed_us: u64,
    cpu_submit_us: Option<u64>,
    gpu_time_us: Option<u64>,
    dirty_cells: u32,
    patch_count: u32,
    bytes_uploaded: u64,
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
    pub cpu_submit_time: Option<FrameTimeHistogram>,
    pub gpu_time: Option<FrameTimeHistogram>,
    pub patch_stats: PatchStats,
}

impl SessionReport {
    /// Serialize to a JSON string (machine-readable for CI gating).
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Deterministic geometry snapshot used by browser resize-storm traces.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct GeometrySnapshot {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub cell_width_px: f32,
    pub cell_height_px: f32,
    pub dpr: f32,
    pub zoom: f32,
}

impl From<GridGeometry> for GeometrySnapshot {
    fn from(value: GridGeometry) -> Self {
        Self {
            cols: value.cols,
            rows: value.rows,
            pixel_width: value.pixel_width,
            pixel_height: value.pixel_height,
            cell_width_px: value.cell_width_px,
            cell_height_px: value.cell_height_px,
            dpr: value.dpr,
            zoom: value.zoom,
        }
    }
}

#[derive(Debug, Serialize)]
struct ResizeStormFrameJsonlRecord<'a> {
    schema_version: &'static str,
    #[serde(rename = "type")]
    record_type: &'static str,
    timestamp: &'a str,
    run_id: &'a str,
    seed: u64,
    frame_idx: u64,
    hash_algo: &'static str,
    frame_hash: String,
    cols: u16,
    rows: u16,
    geometry: GeometrySnapshot,
}

#[must_use]
fn fnv1a64_extend(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV64_PRIME);
    }
    hash
}

#[must_use]
fn hash_geometry(mut hash: u64, geometry: GeometrySnapshot) -> u64 {
    hash = fnv1a64_extend(hash, &geometry.cols.to_le_bytes());
    hash = fnv1a64_extend(hash, &geometry.rows.to_le_bytes());
    hash = fnv1a64_extend(hash, &geometry.pixel_width.to_le_bytes());
    hash = fnv1a64_extend(hash, &geometry.pixel_height.to_le_bytes());
    hash = fnv1a64_extend(hash, &geometry.cell_width_px.to_bits().to_le_bytes());
    hash = fnv1a64_extend(hash, &geometry.cell_height_px.to_bits().to_le_bytes());
    hash = fnv1a64_extend(hash, &geometry.dpr.to_bits().to_le_bytes());
    fnv1a64_extend(hash, &geometry.zoom.to_bits().to_le_bytes())
}

/// Compute a deterministic frame hash over geometry + cell payload.
///
/// The hash is stable across runs and platforms for identical inputs.
#[must_use]
pub fn stable_frame_hash(cells: &[CellData], geometry: GeometrySnapshot) -> String {
    let mut hash = FNV64_OFFSET_BASIS;
    hash = hash_geometry(hash, geometry);
    for cell in cells {
        hash = fnv1a64_extend(hash, &cell.to_bytes());
    }
    format!("{FRAME_HASH_ALGO}:{hash:016x}")
}

/// Build one JSONL `frame` record for browser resize-storm traces.
///
/// The output conforms to the shared E2E schema fields and includes a
/// geometry snapshot payload for post-run diagnosis.
#[must_use]
pub fn resize_storm_frame_jsonl(
    run_id: &str,
    seed: u64,
    timestamp: &str,
    frame_idx: u64,
    geometry: GeometrySnapshot,
    cells: &[CellData],
) -> String {
    let frame_hash = stable_frame_hash(cells, geometry);
    let row = ResizeStormFrameJsonlRecord {
        schema_version: E2E_JSONL_SCHEMA_VERSION,
        record_type: "frame",
        timestamp,
        run_id,
        seed,
        frame_idx,
        hash_algo: FRAME_HASH_ALGO,
        frame_hash,
        cols: geometry.cols,
        rows: geometry.rows,
        geometry,
    };
    serde_json::to_string(&row).unwrap_or_else(|_| "{}".to_string())
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 * p) as usize).min(sorted.len() - 1);
    sorted[idx]
}

fn histogram_or_default(samples: &[u64]) -> FrameTimeHistogram {
    if samples.is_empty() {
        return FrameTimeHistogram::default();
    }
    FrameTimeHistogram {
        count: samples.len() as u64,
        min_us: samples[0],
        max_us: samples[samples.len() - 1],
        p50_us: percentile(samples, 0.50),
        p95_us: percentile(samples, 0.95),
        p99_us: percentile(samples, 0.99),
        mean_us: samples.iter().sum::<u64>() / samples.len() as u64,
    }
}

fn optional_histogram(samples: &[u64]) -> Option<FrameTimeHistogram> {
    if samples.is_empty() {
        None
    } else {
        Some(histogram_or_default(samples))
    }
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
            cpu_submit: None,
            gpu_time: None,
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
                cpu_submit: None,
                gpu_time: None,
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
                cpu_submit: None,
                gpu_time: None,
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
            cpu_submit: None,
            gpu_time: None,
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
            cpu_submit: None,
            gpu_time: None,
            dirty_cells: 10,
            patch_count: 2,
            bytes_uploaded: 160,
        });
        c.record_frame(FrameRecord {
            elapsed: Duration::from_micros(200),
            cpu_submit: None,
            gpu_time: None,
            dirty_cells: 20,
            patch_count: 4,
            bytes_uploaded: 320,
        });

        let r = c.report();
        assert!((r.patch_stats.avg_dirty_per_frame - 15.0).abs() < f64::EPSILON);
        assert!((r.patch_stats.avg_patches_per_frame - 3.0).abs() < f64::EPSILON);
        assert!((r.patch_stats.avg_bytes_per_frame - 240.0).abs() < f64::EPSILON);
    }

    #[test]
    fn optional_timing_histograms_are_reported_when_present() {
        let mut c = FrameTimeCollector::new("timed", 80, 24);
        c.record_frame(FrameRecord {
            elapsed: Duration::from_micros(400),
            cpu_submit: Some(Duration::from_micros(150)),
            gpu_time: Some(Duration::from_micros(220)),
            dirty_cells: 10,
            patch_count: 2,
            bytes_uploaded: 160,
        });
        c.record_frame(FrameRecord {
            elapsed: Duration::from_micros(500),
            cpu_submit: None,
            gpu_time: Some(Duration::from_micros(260)),
            dirty_cells: 12,
            patch_count: 3,
            bytes_uploaded: 192,
        });

        let r = c.report();
        let cpu = r.cpu_submit_time.expect("cpu timing should be present");
        let gpu = r.gpu_time.expect("gpu timing should be present");
        assert_eq!(cpu.count, 1);
        assert_eq!(cpu.min_us, 150);
        assert_eq!(gpu.count, 2);
        assert_eq!(gpu.min_us, 220);
        assert_eq!(gpu.max_us, 260);
    }

    #[test]
    fn optional_timing_histograms_absent_when_not_recorded() {
        let mut c = FrameTimeCollector::new("untimed", 80, 24);
        c.record_frame(FrameRecord {
            elapsed: Duration::from_micros(250),
            cpu_submit: None,
            gpu_time: None,
            dirty_cells: 1,
            patch_count: 1,
            bytes_uploaded: 16,
        });

        let r = c.report();
        assert!(r.cpu_submit_time.is_none());
        assert!(r.gpu_time.is_none());
    }

    #[test]
    fn jsonl_escapes_run_id() {
        let mut c = FrameTimeCollector::new("bench\"alpha\nbeta", 80, 24);
        c.record_frame(FrameRecord {
            elapsed: Duration::from_micros(123),
            cpu_submit: None,
            gpu_time: None,
            dirty_cells: 3,
            patch_count: 1,
            bytes_uploaded: 48,
        });

        let jsonl = c.to_jsonl();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["run_id"], "bench\"alpha\nbeta");
    }

    #[test]
    fn stable_frame_hash_is_deterministic() {
        let geometry = GeometrySnapshot {
            cols: 80,
            rows: 24,
            pixel_width: 640,
            pixel_height: 384,
            cell_width_px: 8.0,
            cell_height_px: 16.0,
            dpr: 1.0,
            zoom: 1.0,
        };
        let cells = vec![
            CellData::EMPTY,
            CellData {
                bg_rgba: 0x1122_33FF,
                fg_rgba: 0xAABB_CCFF,
                glyph_id: 42,
                attrs: 3,
            },
        ];
        let a = stable_frame_hash(&cells, geometry);
        let b = stable_frame_hash(&cells, geometry);
        assert_eq!(a, b);
    }

    #[test]
    fn stable_frame_hash_changes_when_inputs_change() {
        let geometry = GeometrySnapshot {
            cols: 80,
            rows: 24,
            pixel_width: 640,
            pixel_height: 384,
            cell_width_px: 8.0,
            cell_height_px: 16.0,
            dpr: 1.0,
            zoom: 1.0,
        };
        let mut cells = vec![CellData::EMPTY; 2];
        cells[1].glyph_id = 7;
        let base = stable_frame_hash(&cells, geometry);

        let mut changed_cells = cells.clone();
        changed_cells[1].glyph_id = 8;
        let changed = stable_frame_hash(&changed_cells, geometry);
        assert_ne!(base, changed);

        let mut changed_geometry = geometry;
        changed_geometry.zoom = 1.25;
        let changed_geom_hash = stable_frame_hash(&cells, changed_geometry);
        assert_ne!(base, changed_geom_hash);
    }

    #[test]
    fn resize_storm_frame_jsonl_contains_geometry_and_hash() {
        let geometry = GeometrySnapshot {
            cols: 120,
            rows: 40,
            pixel_width: 1200,
            pixel_height: 800,
            cell_width_px: 10.0,
            cell_height_px: 20.0,
            dpr: 2.0,
            zoom: 1.0,
        };
        let cells = vec![CellData::EMPTY; 4];
        let line = resize_storm_frame_jsonl("run-1", 42, "T000001", 3, geometry, &cells);
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["schema_version"], "e2e-jsonl-v1");
        assert_eq!(parsed["type"], "frame");
        assert_eq!(parsed["run_id"], "run-1");
        assert_eq!(parsed["seed"], 42);
        assert_eq!(parsed["frame_idx"], 3);
        assert_eq!(parsed["cols"], 120);
        assert_eq!(parsed["rows"], 40);
        assert_eq!(parsed["hash_algo"], "fnv1a64");
        assert!(
            parsed["frame_hash"]
                .as_str()
                .unwrap_or_default()
                .starts_with("fnv1a64:")
        );
        assert_eq!(parsed["geometry"]["pixel_width"], 1200);
        assert_eq!(parsed["geometry"]["pixel_height"], 800);
    }
}

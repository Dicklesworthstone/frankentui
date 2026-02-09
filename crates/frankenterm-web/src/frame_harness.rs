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

use crate::renderer::{CellData, GridGeometry, cell_attr_link_id, cell_attr_style_bits};
use frankenterm_core::ScrollbackWindow;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    interaction_hash: Option<String>,
    cols: u16,
    rows: u16,
    geometry: GeometrySnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    hovered_link_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor_offset: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor_style: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection_start: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection_end: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text_shaping_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text_shaping_engine: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    screen_reader_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    high_contrast_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reduced_motion_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focused: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ScrollbackVirtualizationJsonlRecord<'a> {
    schema_version: &'static str,
    #[serde(rename = "type")]
    record_type: &'static str,
    timestamp: &'a str,
    run_id: &'a str,
    frame_idx: u64,
    scrollback_lines: usize,
    viewport_start: usize,
    viewport_end: usize,
    render_start: usize,
    render_end: usize,
    viewport_lines: usize,
    render_lines: usize,
    overscan_before: usize,
    overscan_after: usize,
    render_cost_us: u64,
}

/// Deterministic link-click snapshot for E2E JSONL traces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkClickSnapshot {
    pub x: u16,
    pub y: u16,
    pub button: Option<u8>,
    pub link_id: u32,
    pub url: Option<String>,
    pub open_allowed: bool,
    pub open_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct LinkClickJsonlRecord<'a> {
    schema_version: &'static str,
    #[serde(rename = "type")]
    record_type: &'static str,
    timestamp: &'a str,
    run_id: &'a str,
    seed: u64,
    event_idx: u64,
    x: u16,
    y: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    button: Option<u8>,
    link_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
    open_allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_reason: Option<&'a str>,
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

/// Overlay interaction state that affects visual rendering.
///
/// These fields mirror renderer interaction uniforms so tests can checksum
/// cursor/selection/hyperlink overlays deterministically.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct InteractionSnapshot {
    pub hovered_link_id: u32,
    pub cursor_offset: u32,
    pub cursor_style: u32,
    pub selection_active: bool,
    pub selection_start: u32,
    pub selection_end: u32,
    pub text_shaping_enabled: bool,
    pub text_shaping_engine: u32,
    pub screen_reader_enabled: bool,
    pub high_contrast_enabled: bool,
    pub reduced_motion_enabled: bool,
    pub focused: bool,
}

impl InteractionSnapshot {
    #[must_use]
    const fn bool_u32(value: bool) -> u32 {
        if value { 1 } else { 0 }
    }

    #[must_use]
    const fn selection_active_u32(self) -> u32 {
        Self::bool_u32(self.selection_active)
    }

    #[must_use]
    const fn text_shaping_enabled_u32(self) -> u32 {
        Self::bool_u32(self.text_shaping_enabled)
    }

    #[must_use]
    const fn screen_reader_enabled_u32(self) -> u32 {
        Self::bool_u32(self.screen_reader_enabled)
    }

    #[must_use]
    const fn high_contrast_enabled_u32(self) -> u32 {
        Self::bool_u32(self.high_contrast_enabled)
    }

    #[must_use]
    const fn reduced_motion_enabled_u32(self) -> u32 {
        Self::bool_u32(self.reduced_motion_enabled)
    }

    #[must_use]
    const fn focused_u32(self) -> u32 {
        Self::bool_u32(self.focused)
    }
}

/// Compute a deterministic frame hash over geometry + cells + interaction state.
///
/// This extends [`stable_frame_hash`] with overlay state so tests can lock
/// cursor/selection/link-hover behavior using checksum assertions.
#[must_use]
pub fn stable_frame_hash_with_interaction(
    cells: &[CellData],
    geometry: GeometrySnapshot,
    interaction: InteractionSnapshot,
) -> String {
    let mut hash = FNV64_OFFSET_BASIS;
    hash = hash_geometry(hash, geometry);
    for cell in cells {
        hash = fnv1a64_extend(hash, &cell.to_bytes());
    }
    hash = fnv1a64_extend(hash, &interaction.hovered_link_id.to_le_bytes());
    hash = fnv1a64_extend(hash, &interaction.cursor_offset.to_le_bytes());
    hash = fnv1a64_extend(hash, &interaction.cursor_style.to_le_bytes());
    hash = fnv1a64_extend(hash, &interaction.selection_active_u32().to_le_bytes());
    hash = fnv1a64_extend(hash, &interaction.selection_start.to_le_bytes());
    hash = fnv1a64_extend(hash, &interaction.selection_end.to_le_bytes());
    hash = fnv1a64_extend(hash, &interaction.text_shaping_enabled_u32().to_le_bytes());
    hash = fnv1a64_extend(hash, &interaction.text_shaping_engine.to_le_bytes());
    hash = fnv1a64_extend(hash, &interaction.screen_reader_enabled_u32().to_le_bytes());
    hash = fnv1a64_extend(hash, &interaction.high_contrast_enabled_u32().to_le_bytes());
    hash = fnv1a64_extend(
        hash,
        &interaction.reduced_motion_enabled_u32().to_le_bytes(),
    );
    hash = fnv1a64_extend(hash, &interaction.focused_u32().to_le_bytes());
    format!("{FRAME_HASH_ALGO}:{hash:016x}")
}

/// Borrowed frame payload used for golden checksum verification.
#[derive(Debug, Clone, Copy)]
pub struct FrameGoldenActual<'a> {
    pub geometry: GeometrySnapshot,
    pub cells: &'a [CellData],
}

/// Compact diagnostics for the rendered region when a golden mismatch occurs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct FrameRegionSummary {
    pub cols: u16,
    pub rows: u16,
    pub total_cells: usize,
    pub non_empty_cells: usize,
    pub glyph_cells: usize,
    pub styled_cells: usize,
    pub linked_cells: usize,
    pub active_min_col: Option<u16>,
    pub active_max_col: Option<u16>,
    pub active_min_row: Option<u16>,
    pub active_max_row: Option<u16>,
}

/// Actionable mismatch payload for golden frame checksum verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrameGoldenMismatch {
    pub frame_idx: usize,
    pub expected_hash: String,
    pub actual_hash: String,
    pub region_summary: FrameRegionSummary,
    pub reproduction_trace_id: String,
    pub expected_frame_count: usize,
    pub actual_frame_count: usize,
}

impl FrameGoldenMismatch {
    /// Serialize mismatch details for JSONL logging.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

impl std::fmt::Display for FrameGoldenMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let summary = &self.region_summary;
        write!(
            f,
            "golden frame mismatch: frame_idx={} expected_hash={} actual_hash={} reproduction_trace_id={} expected_frames={} actual_frames={} region_summary={{cols:{},rows:{},total_cells:{},non_empty_cells:{},glyph_cells:{},styled_cells:{},linked_cells:{},active_min_col:{:?},active_max_col:{:?},active_min_row:{:?},active_max_row:{:?}}}",
            self.frame_idx,
            self.expected_hash,
            self.actual_hash,
            self.reproduction_trace_id,
            self.expected_frame_count,
            self.actual_frame_count,
            summary.cols,
            summary.rows,
            summary.total_cells,
            summary.non_empty_cells,
            summary.glyph_cells,
            summary.styled_cells,
            summary.linked_cells,
            summary.active_min_col,
            summary.active_max_col,
            summary.active_min_row,
            summary.active_max_row,
        )
    }
}

impl std::error::Error for FrameGoldenMismatch {}

#[must_use]
fn reproduction_trace_id(run_id: &str, frame_idx: usize) -> String {
    format!("{run_id}#frame-{frame_idx}")
}

/// Build a compact, deterministic summary for a rendered frame region.
#[must_use]
pub fn summarize_frame_region(
    cells: &[CellData],
    geometry: GeometrySnapshot,
) -> FrameRegionSummary {
    let mut summary = FrameRegionSummary {
        cols: geometry.cols,
        rows: geometry.rows,
        total_cells: cells.len(),
        ..FrameRegionSummary::default()
    };

    let cols = usize::from(geometry.cols);
    for (idx, cell) in cells.iter().enumerate() {
        if *cell != CellData::EMPTY {
            summary.non_empty_cells = summary.non_empty_cells.saturating_add(1);
            if cols > 0 {
                let Ok(x) = u16::try_from(idx % cols) else {
                    continue;
                };
                let Ok(y) = u16::try_from(idx / cols) else {
                    continue;
                };
                summary.active_min_col = Some(summary.active_min_col.map_or(x, |v| v.min(x)));
                summary.active_max_col = Some(summary.active_max_col.map_or(x, |v| v.max(x)));
                summary.active_min_row = Some(summary.active_min_row.map_or(y, |v| v.min(y)));
                summary.active_max_row = Some(summary.active_max_row.map_or(y, |v| v.max(y)));
            }
        }
        if cell.glyph_id != 0 {
            summary.glyph_cells = summary.glyph_cells.saturating_add(1);
        }
        if cell_attr_style_bits(cell.attrs) != 0 {
            summary.styled_cells = summary.styled_cells.saturating_add(1);
        }
        if cell_attr_link_id(cell.attrs) != 0 {
            summary.linked_cells = summary.linked_cells.saturating_add(1);
        }
    }

    summary
}

/// Verify a rendered frame sequence against deterministic golden frame hashes.
///
/// On mismatch, returns structured diagnostics containing frame index,
/// region summary, and a reproduction trace id suitable for CI artifacts.
pub fn verify_golden_frame_hashes(
    run_id: &str,
    expected_hashes: &[String],
    actual_frames: &[FrameGoldenActual<'_>],
) -> Result<(), Box<FrameGoldenMismatch>> {
    let min_len = expected_hashes.len().min(actual_frames.len());
    for frame_idx in 0..min_len {
        let actual = actual_frames[frame_idx];
        let actual_hash = stable_frame_hash(actual.cells, actual.geometry);
        if actual_hash != expected_hashes[frame_idx] {
            return Err(Box::new(FrameGoldenMismatch {
                frame_idx,
                expected_hash: expected_hashes[frame_idx].clone(),
                actual_hash,
                region_summary: summarize_frame_region(actual.cells, actual.geometry),
                reproduction_trace_id: reproduction_trace_id(run_id, frame_idx),
                expected_frame_count: expected_hashes.len(),
                actual_frame_count: actual_frames.len(),
            }));
        }
    }

    if expected_hashes.len() > actual_frames.len() {
        let frame_idx = actual_frames.len();
        let expected_hash = expected_hashes
            .get(frame_idx)
            .cloned()
            .unwrap_or_else(|| "missing".to_string());
        return Err(Box::new(FrameGoldenMismatch {
            frame_idx,
            expected_hash,
            actual_hash: "missing".to_string(),
            region_summary: FrameRegionSummary::default(),
            reproduction_trace_id: reproduction_trace_id(run_id, frame_idx),
            expected_frame_count: expected_hashes.len(),
            actual_frame_count: actual_frames.len(),
        }));
    }

    if actual_frames.len() > expected_hashes.len() {
        let frame_idx = expected_hashes.len();
        let actual = actual_frames[frame_idx];
        return Err(Box::new(FrameGoldenMismatch {
            frame_idx,
            expected_hash: "missing".to_string(),
            actual_hash: stable_frame_hash(actual.cells, actual.geometry),
            region_summary: summarize_frame_region(actual.cells, actual.geometry),
            reproduction_trace_id: reproduction_trace_id(run_id, frame_idx),
            expected_frame_count: expected_hashes.len(),
            actual_frame_count: actual_frames.len(),
        }));
    }

    Ok(())
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
    resize_storm_frame_jsonl_with_interaction(
        run_id, seed, timestamp, frame_idx, geometry, cells, None,
    )
}

/// Build one JSONL `frame` record and include optional interaction overlay state.
///
/// When `interaction` is present, this additionally emits:
/// - `interaction_hash` (geometry + cells + overlay state)
/// - raw overlay fields (`hovered_link_id`, `cursor_*`, `selection_*`, `text_shaping_*`)
/// - accessibility state flags (`screen_reader_enabled`, `high_contrast_enabled`,
///   `reduced_motion_enabled`, `focused`)
#[must_use]
pub fn resize_storm_frame_jsonl_with_interaction(
    run_id: &str,
    seed: u64,
    timestamp: &str,
    frame_idx: u64,
    geometry: GeometrySnapshot,
    cells: &[CellData],
    interaction: Option<InteractionSnapshot>,
) -> String {
    let frame_hash = stable_frame_hash(cells, geometry);
    let (
        interaction_hash,
        hovered_link_id,
        cursor_offset,
        cursor_style,
        selection_active,
        selection_start,
        selection_end,
        text_shaping_enabled,
        text_shaping_engine,
        screen_reader_enabled,
        high_contrast_enabled,
        reduced_motion_enabled,
        focused,
    ) = if let Some(state) = interaction {
        (
            Some(stable_frame_hash_with_interaction(cells, geometry, state)),
            Some(state.hovered_link_id),
            Some(state.cursor_offset),
            Some(state.cursor_style),
            Some(state.selection_active),
            Some(state.selection_start),
            Some(state.selection_end),
            Some(state.text_shaping_enabled),
            Some(state.text_shaping_engine),
            Some(state.screen_reader_enabled),
            Some(state.high_contrast_enabled),
            Some(state.reduced_motion_enabled),
            Some(state.focused),
        )
    } else {
        (
            None, None, None, None, None, None, None, None, None, None, None, None, None,
        )
    };

    let row = ResizeStormFrameJsonlRecord {
        schema_version: E2E_JSONL_SCHEMA_VERSION,
        record_type: "frame",
        timestamp,
        run_id,
        seed,
        frame_idx,
        hash_algo: FRAME_HASH_ALGO,
        frame_hash,
        interaction_hash,
        cols: geometry.cols,
        rows: geometry.rows,
        geometry,
        hovered_link_id,
        cursor_offset,
        cursor_style,
        selection_active,
        selection_start,
        selection_end,
        text_shaping_enabled,
        text_shaping_engine,
        screen_reader_enabled,
        high_contrast_enabled,
        reduced_motion_enabled,
        focused,
    };
    serde_json::to_string(&row).unwrap_or_else(|_| "{}".to_string())
}

/// Build one JSONL `scrollback_frame` record for virtualized scrollback telemetry.
///
/// The record is designed for E2E/perf harnesses and includes:
/// - total scrollback size,
/// - viewport/render ranges,
/// - overscan extents,
/// - render cost in microseconds.
#[must_use]
pub fn scrollback_virtualization_frame_jsonl(
    run_id: &str,
    timestamp: &str,
    frame_idx: u64,
    window: ScrollbackWindow,
    render_cost: Duration,
) -> String {
    let row = ScrollbackVirtualizationJsonlRecord {
        schema_version: E2E_JSONL_SCHEMA_VERSION,
        record_type: "scrollback_frame",
        timestamp,
        run_id,
        frame_idx,
        scrollback_lines: window.total_lines,
        viewport_start: window.viewport_start,
        viewport_end: window.viewport_end,
        render_start: window.render_start,
        render_end: window.render_end,
        viewport_lines: window.viewport_len(),
        render_lines: window.render_len(),
        overscan_before: window.viewport_start.saturating_sub(window.render_start),
        overscan_after: window.render_end.saturating_sub(window.viewport_end),
        render_cost_us: render_cost.as_micros() as u64,
    };
    serde_json::to_string(&row).unwrap_or_else(|_| "{}".to_string())
}

/// Build one JSONL `link_click` record for host-side click/open policy traces.
#[must_use]
pub fn link_click_jsonl(
    run_id: &str,
    seed: u64,
    timestamp: &str,
    event_idx: u64,
    click: &LinkClickSnapshot,
) -> String {
    let row = LinkClickJsonlRecord {
        schema_version: E2E_JSONL_SCHEMA_VERSION,
        record_type: "link_click",
        timestamp,
        run_id,
        seed,
        event_idx,
        x: click.x,
        y: click.y,
        button: click.button,
        link_id: click.link_id,
        url: click.url.as_deref(),
        open_allowed: click.open_allowed,
        open_reason: click.open_reason.as_deref(),
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

    fn unicode_fixture_cells() -> Vec<CellData> {
        vec![
            CellData {
                glyph_id: u32::from('界'),
                ..CellData::EMPTY
            },
            CellData {
                glyph_id: u32::from('\u{0301}'),
                ..CellData::EMPTY
            },
            CellData {
                glyph_id: u32::from('🧪'),
                attrs: (5u32 << 8) | 0x1,
                ..CellData::EMPTY
            },
            CellData {
                glyph_id: u32::from('\u{200D}'),
                ..CellData::EMPTY
            },
            CellData {
                glyph_id: u32::from('\u{FE0F}'),
                ..CellData::EMPTY
            },
        ]
    }

    #[test]
    fn stable_frame_hash_unicode_fixture_is_deterministic_and_sensitive() {
        let geometry = GeometrySnapshot {
            cols: 5,
            rows: 1,
            pixel_width: 50,
            pixel_height: 10,
            cell_width_px: 10.0,
            cell_height_px: 10.0,
            dpr: 1.0,
            zoom: 1.0,
        };
        let cells = unicode_fixture_cells();
        let a = stable_frame_hash(&cells, geometry);
        let b = stable_frame_hash(&cells, geometry);
        assert_eq!(a, b);
        assert!(a.starts_with("fnv1a64:"));

        let mut changed = cells.clone();
        changed[2].glyph_id = u32::from('A');
        assert_ne!(a, stable_frame_hash(&changed, geometry));
    }

    #[test]
    fn resize_storm_jsonl_unicode_fixture_has_stable_interaction_hash() {
        let geometry = GeometrySnapshot {
            cols: 5,
            rows: 1,
            pixel_width: 50,
            pixel_height: 10,
            cell_width_px: 10.0,
            cell_height_px: 10.0,
            dpr: 1.0,
            zoom: 1.0,
        };
        let cells = unicode_fixture_cells();
        let interaction = InteractionSnapshot {
            hovered_link_id: 5,
            cursor_offset: 2,
            cursor_style: 1,
            selection_active: true,
            selection_start: 1,
            selection_end: 4,
            text_shaping_enabled: false,
            text_shaping_engine: 0,
            screen_reader_enabled: true,
            high_contrast_enabled: false,
            reduced_motion_enabled: true,
            focused: true,
        };

        let line_a = resize_storm_frame_jsonl_with_interaction(
            "unicode-run",
            77,
            "2026-02-09T09:52:00Z",
            2,
            geometry,
            &cells,
            Some(interaction),
        );
        let line_b = resize_storm_frame_jsonl_with_interaction(
            "unicode-run",
            77,
            "2026-02-09T09:52:00Z",
            2,
            geometry,
            &cells,
            Some(interaction),
        );
        let parsed_a: serde_json::Value = serde_json::from_str(&line_a).unwrap();
        let parsed_b: serde_json::Value = serde_json::from_str(&line_b).unwrap();

        assert_eq!(parsed_a["frame_hash"], parsed_b["frame_hash"]);
        assert_eq!(parsed_a["interaction_hash"], parsed_b["interaction_hash"]);
        assert_ne!(parsed_a["frame_hash"], parsed_a["interaction_hash"]);
        assert_eq!(parsed_a["hovered_link_id"], 5);
        assert_eq!(parsed_a["cursor_offset"], 2);
        assert_eq!(parsed_a["selection_active"], true);
        assert_eq!(parsed_a["selection_start"], 1);
        assert_eq!(parsed_a["selection_end"], 4);
        assert_eq!(parsed_a["text_shaping_enabled"], false);
        assert_eq!(parsed_a["text_shaping_engine"], 0);
        assert_eq!(parsed_a["screen_reader_enabled"], true);
        assert_eq!(parsed_a["high_contrast_enabled"], false);
        assert_eq!(parsed_a["reduced_motion_enabled"], true);
        assert_eq!(parsed_a["focused"], true);
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
    fn stable_frame_hash_with_interaction_is_deterministic() {
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
                attrs: 0x0201, // style bit + link id
            },
        ];
        let interaction = InteractionSnapshot {
            hovered_link_id: 2,
            cursor_offset: 1,
            cursor_style: 1,
            selection_active: true,
            selection_start: 0,
            selection_end: 2,
            text_shaping_enabled: false,
            text_shaping_engine: 0,
            screen_reader_enabled: false,
            high_contrast_enabled: false,
            reduced_motion_enabled: false,
            focused: false,
        };
        let a = stable_frame_hash_with_interaction(&cells, geometry, interaction);
        let b = stable_frame_hash_with_interaction(&cells, geometry, interaction);
        assert_eq!(a, b);
    }

    #[test]
    fn stable_frame_hash_with_interaction_changes_when_overlay_changes() {
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
                glyph_id: 11,
                attrs: 0x0300, // link id = 3
                ..CellData::EMPTY
            },
        ];
        let none = InteractionSnapshot::default();
        let hover = InteractionSnapshot {
            hovered_link_id: 3,
            ..none
        };
        let cursor_block = InteractionSnapshot {
            cursor_offset: 1,
            cursor_style: 1,
            ..none
        };
        let cursor_bar = InteractionSnapshot {
            cursor_offset: 1,
            cursor_style: 2,
            ..none
        };
        let selection = InteractionSnapshot {
            selection_active: true,
            selection_start: 0,
            selection_end: 2,
            ..none
        };
        let shaping_enabled = InteractionSnapshot {
            text_shaping_enabled: true,
            ..none
        };
        let shaping_harfbuzz = InteractionSnapshot {
            text_shaping_enabled: true,
            text_shaping_engine: 1,
            ..none
        };
        let high_contrast = InteractionSnapshot {
            high_contrast_enabled: true,
            ..none
        };

        let none_hash = stable_frame_hash_with_interaction(&cells, geometry, none);
        let hover_hash = stable_frame_hash_with_interaction(&cells, geometry, hover);
        let block_hash = stable_frame_hash_with_interaction(&cells, geometry, cursor_block);
        let bar_hash = stable_frame_hash_with_interaction(&cells, geometry, cursor_bar);
        let selection_hash = stable_frame_hash_with_interaction(&cells, geometry, selection);
        let shaping_enabled_hash =
            stable_frame_hash_with_interaction(&cells, geometry, shaping_enabled);
        let shaping_harfbuzz_hash =
            stable_frame_hash_with_interaction(&cells, geometry, shaping_harfbuzz);
        let high_contrast_hash =
            stable_frame_hash_with_interaction(&cells, geometry, high_contrast);

        assert_ne!(none_hash, hover_hash);
        assert_ne!(none_hash, block_hash);
        assert_ne!(block_hash, bar_hash);
        assert_ne!(none_hash, selection_hash);
        assert_ne!(none_hash, shaping_enabled_hash);
        assert_ne!(shaping_enabled_hash, shaping_harfbuzz_hash);
        assert_ne!(none_hash, high_contrast_hash);
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

    #[test]
    fn resize_storm_frame_jsonl_with_interaction_includes_overlay_fields() {
        let geometry = GeometrySnapshot {
            cols: 4,
            rows: 1,
            pixel_width: 40,
            pixel_height: 10,
            cell_width_px: 10.0,
            cell_height_px: 10.0,
            dpr: 1.0,
            zoom: 1.0,
        };
        let cells = vec![
            CellData::EMPTY,
            CellData {
                glyph_id: u32::from('A'),
                attrs: 0x0300, // link id = 3
                ..CellData::EMPTY
            },
            CellData::EMPTY,
            CellData::EMPTY,
        ];
        let interaction = InteractionSnapshot {
            hovered_link_id: 3,
            cursor_offset: 1,
            cursor_style: 2,
            selection_active: true,
            selection_start: 1,
            selection_end: 3,
            text_shaping_enabled: true,
            text_shaping_engine: 1,
            screen_reader_enabled: true,
            high_contrast_enabled: true,
            reduced_motion_enabled: false,
            focused: true,
        };

        let base = resize_storm_frame_jsonl("run-2", 7, "T000002", 4, geometry, &cells);
        let with = resize_storm_frame_jsonl_with_interaction(
            "run-2",
            7,
            "T000002",
            4,
            geometry,
            &cells,
            Some(interaction),
        );
        let base_parsed: serde_json::Value = serde_json::from_str(&base).unwrap();
        let with_parsed: serde_json::Value = serde_json::from_str(&with).unwrap();

        assert!(base_parsed.get("interaction_hash").is_none());
        assert!(
            with_parsed["interaction_hash"]
                .as_str()
                .unwrap_or_default()
                .starts_with("fnv1a64:")
        );
        assert_ne!(with_parsed["frame_hash"], with_parsed["interaction_hash"]);
        assert_eq!(with_parsed["hovered_link_id"], 3);
        assert_eq!(with_parsed["cursor_offset"], 1);
        assert_eq!(with_parsed["cursor_style"], 2);
        assert_eq!(with_parsed["selection_active"], true);
        assert_eq!(with_parsed["selection_start"], 1);
        assert_eq!(with_parsed["selection_end"], 3);
        assert_eq!(with_parsed["text_shaping_enabled"], true);
        assert_eq!(with_parsed["text_shaping_engine"], 1);
        assert_eq!(with_parsed["screen_reader_enabled"], true);
        assert_eq!(with_parsed["high_contrast_enabled"], true);
        assert_eq!(with_parsed["reduced_motion_enabled"], false);
        assert_eq!(with_parsed["focused"], true);
    }

    #[test]
    fn scrollback_virtualization_frame_jsonl_contains_ranges_and_cost() {
        let window = ScrollbackWindow {
            total_lines: 100_000,
            max_scroll_offset: 99_960,
            scroll_offset_from_bottom: 123,
            viewport_start: 10_000,
            viewport_end: 10_040,
            render_start: 9_992,
            render_end: 10_048,
        };
        let line = scrollback_virtualization_frame_jsonl(
            "run-vscroll",
            "2026-02-09T04:30:00Z",
            17,
            window,
            Duration::from_micros(2314),
        );
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();

        assert_eq!(parsed["schema_version"], "e2e-jsonl-v1");
        assert_eq!(parsed["type"], "scrollback_frame");
        assert_eq!(parsed["run_id"], "run-vscroll");
        assert_eq!(parsed["frame_idx"], 17);
        assert_eq!(parsed["scrollback_lines"], 100000);
        assert_eq!(parsed["viewport_start"], 10000);
        assert_eq!(parsed["viewport_end"], 10040);
        assert_eq!(parsed["render_start"], 9992);
        assert_eq!(parsed["render_end"], 10048);
        assert_eq!(parsed["viewport_lines"], 40);
        assert_eq!(parsed["render_lines"], 56);
        assert_eq!(parsed["overscan_before"], 8);
        assert_eq!(parsed["overscan_after"], 8);
        assert_eq!(parsed["render_cost_us"], 2314);
    }

    #[test]
    fn link_click_jsonl_contains_policy_fields() {
        let click = LinkClickSnapshot {
            x: 12,
            y: 3,
            button: Some(0),
            link_id: 0x00F0_0001,
            url: Some("https://example.test/docs".to_string()),
            open_allowed: true,
            open_reason: None,
        };
        let line = link_click_jsonl("run-link", 7, "T000321", 4, &click);
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();

        assert_eq!(parsed["schema_version"], "e2e-jsonl-v1");
        assert_eq!(parsed["type"], "link_click");
        assert_eq!(parsed["run_id"], "run-link");
        assert_eq!(parsed["seed"], 7);
        assert_eq!(parsed["event_idx"], 4);
        assert_eq!(parsed["x"], 12);
        assert_eq!(parsed["y"], 3);
        assert_eq!(parsed["button"], 0);
        assert_eq!(parsed["link_id"], 0x00F0_0001u64);
        assert_eq!(parsed["url"], "https://example.test/docs");
        assert_eq!(parsed["open_allowed"], true);
        assert!(parsed.get("open_reason").is_none());
    }

    #[test]
    fn link_click_jsonl_is_stable_for_identical_input() {
        let click = LinkClickSnapshot {
            x: 1,
            y: 0,
            button: None,
            link_id: 77,
            url: None,
            open_allowed: false,
            open_reason: Some("url_unavailable".to_string()),
        };
        let a = link_click_jsonl("run-a", 0, "T000010", 0, &click);
        let b = link_click_jsonl("run-a", 0, "T000010", 0, &click);
        assert_eq!(a, b);
    }

    #[test]
    fn verify_golden_frame_hashes_accepts_matching_sequence() {
        let geometry = GeometrySnapshot {
            cols: 2,
            rows: 1,
            pixel_width: 20,
            pixel_height: 10,
            cell_width_px: 10.0,
            cell_height_px: 10.0,
            dpr: 1.0,
            zoom: 1.0,
        };
        let frame0 = vec![CellData::EMPTY, CellData::EMPTY];
        let frame1 = vec![
            CellData::EMPTY,
            CellData {
                glyph_id: 7,
                attrs: 0,
                ..CellData::EMPTY
            },
        ];
        let expected = vec![
            stable_frame_hash(&frame0, geometry),
            stable_frame_hash(&frame1, geometry),
        ];
        let actual = vec![
            FrameGoldenActual {
                geometry,
                cells: &frame0,
            },
            FrameGoldenActual {
                geometry,
                cells: &frame1,
            },
        ];

        assert!(verify_golden_frame_hashes("run-pass", &expected, &actual).is_ok());
    }

    #[test]
    fn verify_golden_frame_hashes_reports_frame_index_region_and_repro_id() {
        let geometry = GeometrySnapshot {
            cols: 3,
            rows: 1,
            pixel_width: 30,
            pixel_height: 10,
            cell_width_px: 10.0,
            cell_height_px: 10.0,
            dpr: 1.0,
            zoom: 1.0,
        };
        let frame0 = vec![CellData::EMPTY; 3];
        let frame1 = vec![
            CellData::EMPTY,
            CellData {
                glyph_id: 9,
                attrs: 0x0200, // link_id = 2
                ..CellData::EMPTY
            },
            CellData {
                glyph_id: 0,
                attrs: 0x0001, // style bits
                ..CellData::EMPTY
            },
        ];

        let expected = vec![
            stable_frame_hash(&frame0, geometry),
            "fnv1a64:0000000000000000".to_string(),
        ];
        let actual = vec![
            FrameGoldenActual {
                geometry,
                cells: &frame0,
            },
            FrameGoldenActual {
                geometry,
                cells: &frame1,
            },
        ];

        let mismatch =
            *verify_golden_frame_hashes("run-mismatch", &expected, &actual).expect_err("mismatch");
        assert_eq!(mismatch.frame_idx, 1);
        assert_eq!(mismatch.reproduction_trace_id, "run-mismatch#frame-1");
        assert_eq!(mismatch.region_summary.cols, 3);
        assert_eq!(mismatch.region_summary.rows, 1);
        assert_eq!(mismatch.region_summary.total_cells, 3);
        assert_eq!(mismatch.region_summary.non_empty_cells, 2);
        assert_eq!(mismatch.region_summary.glyph_cells, 1);
        assert_eq!(mismatch.region_summary.styled_cells, 1);
        assert_eq!(mismatch.region_summary.linked_cells, 1);
        assert_eq!(mismatch.region_summary.active_min_col, Some(1));
        assert_eq!(mismatch.region_summary.active_max_col, Some(2));
        assert_eq!(mismatch.region_summary.active_min_row, Some(0));
        assert_eq!(mismatch.region_summary.active_max_row, Some(0));
        let json = mismatch.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["frame_idx"], 1);
        assert_eq!(parsed["reproduction_trace_id"], "run-mismatch#frame-1");
    }

    #[test]
    fn verify_golden_frame_hashes_reports_length_mismatch() {
        let geometry = GeometrySnapshot {
            cols: 1,
            rows: 1,
            pixel_width: 10,
            pixel_height: 10,
            cell_width_px: 10.0,
            cell_height_px: 10.0,
            dpr: 1.0,
            zoom: 1.0,
        };
        let frame0 = vec![CellData::EMPTY];
        let expected = vec![
            stable_frame_hash(&frame0, geometry),
            "fnv1a64:ffff000000000000".to_string(),
        ];
        let actual = vec![FrameGoldenActual {
            geometry,
            cells: &frame0,
        }];

        let mismatch =
            *verify_golden_frame_hashes("run-short", &expected, &actual).expect_err("mismatch");
        assert_eq!(mismatch.frame_idx, 1);
        assert_eq!(mismatch.expected_hash, "fnv1a64:ffff000000000000");
        assert_eq!(mismatch.actual_hash, "missing");
        assert_eq!(mismatch.reproduction_trace_id, "run-short#frame-1");
        assert_eq!(mismatch.expected_frame_count, 2);
        assert_eq!(mismatch.actual_frame_count, 1);
    }
}

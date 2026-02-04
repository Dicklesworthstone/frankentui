#![forbid(unsafe_code)]

//! Diff computation between buffers.
//!
//! The `BufferDiff` computes the minimal set of changed cells between two
//! buffers using a row-major scan for optimal cache efficiency.
//!
//! # Algorithm
//!
//! Row-major scan for cache efficiency:
//! 1. Iterate y from 0 to height
//! 2. Fast-path skip rows where the full slice is equal
//! 3. For changed rows, scan in coarse blocks and skip unchanged blocks
//! 4. Within dirty blocks, compare cells using `bits_eq`
//!
//! This ensures sequential memory access since cells are stored row-by-row.
//! With 4 cells per cache line, the prefetcher can anticipate next access.
//!
//! # Usage
//!
//! ```
//! use ftui_render::buffer::Buffer;
//! use ftui_render::cell::Cell;
//! use ftui_render::diff::BufferDiff;
//!
//! let mut old = Buffer::new(80, 24);
//! let mut new = Buffer::new(80, 24);
//!
//! // Make some changes
//! new.set_raw(5, 5, Cell::from_char('X'));
//! new.set_raw(6, 5, Cell::from_char('Y'));
//!
//! let diff = BufferDiff::compute(&old, &new);
//! assert_eq!(diff.len(), 2);
//!
//! // Coalesce into runs for efficient emission
//! let runs = diff.runs();
//! assert_eq!(runs.len(), 1); // Adjacent cells form one run
//! ```

use crate::buffer::{Buffer, DirtySpan};
use crate::cell::Cell;

// =============================================================================
// Block-based Row Scanning (autovec-friendly)
// =============================================================================

/// Block size for vectorized comparison (4 cells = 64 bytes).
/// Chosen to match common SIMD register width (256-bit / 512-bit).
const BLOCK_SIZE: usize = 4;

/// Row block size for coarse blockwise skip (32 cells = 512 bytes).
/// This lets us skip large unchanged regions in sparse rows while
/// preserving row-major iteration order.
const ROW_BLOCK_SIZE: usize = 32;

/// Scan a row slice for changed cells, appending positions to `changes`.
///
/// `x_offset` is added to each recorded x coordinate.
#[inline]
fn scan_row_changes_range(
    old_row: &[Cell],
    new_row: &[Cell],
    y: u16,
    x_offset: u16,
    changes: &mut Vec<(u16, u16)>,
) {
    debug_assert_eq!(old_row.len(), new_row.len());
    let len = old_row.len();
    let blocks = len / BLOCK_SIZE;
    let remainder = len % BLOCK_SIZE;

    // Process full blocks
    for block_idx in 0..blocks {
        let base = block_idx * BLOCK_SIZE;
        let base_x = x_offset + base as u16;
        let old_block = &old_row[base..base + BLOCK_SIZE];
        let new_block = &new_row[base..base + BLOCK_SIZE];

        // Compare each cell and push changes directly (unrolled for BLOCK_SIZE=4).
        if !old_block[0].bits_eq(&new_block[0]) {
            changes.push((base_x, y));
        }
        if !old_block[1].bits_eq(&new_block[1]) {
            changes.push((base_x + 1, y));
        }
        if !old_block[2].bits_eq(&new_block[2]) {
            changes.push((base_x + 2, y));
        }
        if !old_block[3].bits_eq(&new_block[3]) {
            changes.push((base_x + 3, y));
        }
    }

    // Process remainder cells
    let rem_base = blocks * BLOCK_SIZE;
    let rem_base_x = x_offset + rem_base as u16;
    for i in 0..remainder {
        if !old_row[rem_base + i].bits_eq(&new_row[rem_base + i]) {
            changes.push((rem_base_x + i as u16, y));
        }
    }
}

/// Scan a row pair for changed cells, appending positions to `changes`.
///
/// Uses coarse block skipping to avoid full per-cell scans in sparse rows,
/// then falls back to fine-grained scanning inside dirty blocks.
#[inline]
fn scan_row_changes(old_row: &[Cell], new_row: &[Cell], y: u16, changes: &mut Vec<(u16, u16)>) {
    scan_row_changes_blockwise(old_row, new_row, y, changes);
}

/// Scan a row in coarse blocks, skipping unchanged blocks, and falling back
/// to fine-grained scan for blocks that differ.
#[inline]
fn scan_row_changes_blockwise(
    old_row: &[Cell],
    new_row: &[Cell],
    y: u16,
    changes: &mut Vec<(u16, u16)>,
) {
    debug_assert_eq!(old_row.len(), new_row.len());
    let len = old_row.len();
    let blocks = len / ROW_BLOCK_SIZE;
    let remainder = len % ROW_BLOCK_SIZE;

    for block_idx in 0..blocks {
        let base = block_idx * ROW_BLOCK_SIZE;
        let old_block = &old_row[base..base + ROW_BLOCK_SIZE];
        let new_block = &new_row[base..base + ROW_BLOCK_SIZE];
        if old_block == new_block {
            continue;
        }
        scan_row_changes_range(old_block, new_block, y, base as u16, changes);
    }

    if remainder > 0 {
        let base = blocks * ROW_BLOCK_SIZE;
        let old_block = &old_row[base..base + remainder];
        let new_block = &new_row[base..base + remainder];
        if old_block != new_block {
            scan_row_changes_range(old_block, new_block, y, base as u16, changes);
        }
    }
}

/// Scan only dirty spans within a row, preserving row-major ordering.
#[inline]
fn scan_row_changes_spans(
    old_row: &[Cell],
    new_row: &[Cell],
    y: u16,
    spans: &[DirtySpan],
    changes: &mut Vec<(u16, u16)>,
) {
    for span in spans {
        let start = span.x0 as usize;
        let end = span.x1 as usize;
        if start >= end || start >= old_row.len() {
            continue;
        }
        let end = end.min(old_row.len());
        let old_slice = &old_row[start..end];
        let new_slice = &new_row[start..end];
        if old_slice == new_slice {
            continue;
        }
        scan_row_changes_range(old_slice, new_slice, y, span.x0, changes);
    }
}

#[inline]
fn scan_row_changes_range_if_needed(
    old_row: &[Cell],
    new_row: &[Cell],
    y: u16,
    start: usize,
    end: usize,
    changes: &mut Vec<(u16, u16)>,
) {
    if start >= end || start >= old_row.len() {
        return;
    }
    let end = end.min(old_row.len());
    let old_slice = &old_row[start..end];
    let new_slice = &new_row[start..end];
    if old_slice == new_slice {
        return;
    }
    scan_row_changes_range(old_slice, new_slice, y, start as u16, changes);
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn scan_row_tiles(
    old_row: &[Cell],
    new_row: &[Cell],
    y: u16,
    width: usize,
    tile_w: usize,
    tiles_x: usize,
    tile_row_base: usize,
    dirty_tiles: &[bool],
    changes: &mut Vec<(u16, u16)>,
) {
    for tile_x in 0..tiles_x {
        let tile_idx = tile_row_base + tile_x;
        if !dirty_tiles[tile_idx] {
            continue;
        }
        let start = tile_x * tile_w;
        let end = ((tile_x + 1) * tile_w).min(width);
        scan_row_changes_range_if_needed(old_row, new_row, y, start, end, changes);
    }
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn scan_row_tiles_spans(
    old_row: &[Cell],
    new_row: &[Cell],
    y: u16,
    width: usize,
    tile_w: usize,
    tiles_x: usize,
    tile_row_base: usize,
    dirty_tiles: &[bool],
    spans: &[DirtySpan],
    changes: &mut Vec<(u16, u16)>,
) {
    if spans.is_empty() {
        return;
    }
    let max_x = width.saturating_sub(1);
    for span in spans {
        let span_start = span.x0 as usize;
        let span_end_exclusive = span.x1 as usize;
        if span_start >= span_end_exclusive || span_start > max_x {
            continue;
        }
        let span_end = span_end_exclusive.saturating_sub(1).min(max_x);
        let tile_x_start = span_start / tile_w;
        let tile_x_end = span_end / tile_w;
        for tile_x in tile_x_start..=tile_x_end {
            if tile_x >= tiles_x {
                break;
            }
            let tile_idx = tile_row_base + tile_x;
            if !dirty_tiles[tile_idx] {
                continue;
            }
            let tile_start = tile_x * tile_w;
            let tile_end = ((tile_x + 1) * tile_w).min(width);
            let seg_start = span_start.max(tile_start);
            let seg_end = span_end.min(tile_end.saturating_sub(1));
            if seg_start > seg_end {
                continue;
            }
            scan_row_changes_range_if_needed(old_row, new_row, y, seg_start, seg_end + 1, changes);
        }
    }
}

// =============================================================================
// Tile-based Skip (Summed-Area Table)
// =============================================================================

const TILE_SIZE_MIN: u16 = 8;
const TILE_SIZE_MAX: u16 = 64;

#[inline]
fn clamp_tile_size(value: u16) -> u16 {
    value.clamp(TILE_SIZE_MIN, TILE_SIZE_MAX)
}

#[inline]
fn div_ceil_usize(n: usize, d: usize) -> usize {
    debug_assert!(d > 0);
    n.div_ceil(d)
}

/// Configuration for tile-based diff skipping.
#[derive(Debug, Clone)]
pub struct TileDiffConfig {
    /// Whether tile-based skipping is enabled.
    pub enabled: bool,
    /// Tile width in cells (clamped to [8, 64]).
    pub tile_w: u16,
    /// Tile height in cells (clamped to [8, 64]).
    pub tile_h: u16,
    /// Minimum total cells required before enabling tiles.
    pub min_cells_for_tiles: usize,
    /// Dense cell ratio threshold for falling back to non-tile diff.
    pub dense_cell_ratio: f64,
    /// Dense tile ratio threshold for falling back to non-tile diff.
    pub dense_tile_ratio: f64,
    /// Maximum number of tiles allowed (fallback if exceeded).
    pub max_tiles: usize,
}

impl Default for TileDiffConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tile_w: 16,
            tile_h: 8,
            min_cells_for_tiles: 12_000,
            dense_cell_ratio: 0.25,
            dense_tile_ratio: 0.60,
            max_tiles: 4096,
        }
    }
}

/// Reason the tile path fell back to a non-tile diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileDiffFallback {
    Disabled,
    SmallScreen,
    DirtyAll,
    DenseCells,
    DenseTiles,
    TooManyTiles,
    Overflow,
}

impl TileDiffFallback {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::SmallScreen => "small_screen",
            Self::DirtyAll => "dirty_all",
            Self::DenseCells => "dense_cells",
            Self::DenseTiles => "dense_tiles",
            Self::TooManyTiles => "too_many_tiles",
            Self::Overflow => "overflow",
        }
    }
}

/// Tile parameters derived from the current buffer dimensions.
#[derive(Debug, Clone, Copy)]
pub struct TileParams {
    pub width: u16,
    pub height: u16,
    pub tile_w: u16,
    pub tile_h: u16,
    pub tiles_x: usize,
    pub tiles_y: usize,
}

impl TileParams {
    #[inline]
    pub fn total_tiles(self) -> usize {
        self.tiles_x * self.tiles_y
    }

    #[inline]
    pub fn total_cells(self) -> usize {
        self.width as usize * self.height as usize
    }
}

/// Summary statistics from building a tile SAT.
#[derive(Debug, Clone, Copy)]
pub struct TileDiffStats {
    pub width: u16,
    pub height: u16,
    pub tile_w: u16,
    pub tile_h: u16,
    pub tiles_x: usize,
    pub tiles_y: usize,
    pub total_tiles: usize,
    pub dirty_cells: usize,
    pub dirty_tiles: usize,
    pub dirty_cell_ratio: f64,
    pub dirty_tile_ratio: f64,
    pub scanned_tiles: usize,
    pub skipped_tiles: usize,
    pub sat_build_cells: usize,
    pub scan_cells_estimate: usize,
    pub fallback: Option<TileDiffFallback>,
}

/// Reusable builder for tile counts and SAT.
#[derive(Debug, Default, Clone)]
pub struct TileDiffBuilder {
    tile_counts: Vec<u32>,
    sat: Vec<u32>,
    dirty_tiles: Vec<bool>,
}

/// Successful tile build with reusable buffers.
#[derive(Debug, Clone)]
pub struct TileDiffPlan<'a> {
    pub params: TileParams,
    pub stats: TileDiffStats,
    pub dirty_tiles: &'a [bool],
    pub tile_counts: &'a [u32],
    pub sat: &'a [u32],
}

/// Result of a tile build attempt.
#[derive(Debug, Clone)]
pub enum TileDiffBuild<'a> {
    UseTiles(TileDiffPlan<'a>),
    Fallback(TileDiffStats),
}

impl TileDiffBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn build<'a>(
        &'a mut self,
        config: &TileDiffConfig,
        width: u16,
        height: u16,
        dirty_bits: &[u8],
        dirty_cells: usize,
        dirty_all: bool,
    ) -> TileDiffBuild<'a> {
        let tile_w = clamp_tile_size(config.tile_w);
        let tile_h = clamp_tile_size(config.tile_h);
        let width_usize = width as usize;
        let height_usize = height as usize;
        let tiles_x = div_ceil_usize(width_usize, tile_w as usize);
        let tiles_y = div_ceil_usize(height_usize, tile_h as usize);
        let total_tiles = tiles_x * tiles_y;
        let total_cells = width_usize * height_usize;
        let dirty_cell_ratio = if total_cells == 0 {
            0.0
        } else {
            dirty_cells as f64 / total_cells as f64
        };

        let mut stats = TileDiffStats {
            width,
            height,
            tile_w,
            tile_h,
            tiles_x,
            tiles_y,
            total_tiles,
            dirty_cells,
            dirty_tiles: 0,
            dirty_cell_ratio,
            dirty_tile_ratio: 0.0,
            scanned_tiles: 0,
            skipped_tiles: total_tiles,
            sat_build_cells: 0,
            scan_cells_estimate: 0,
            fallback: None,
        };

        if !config.enabled {
            stats.fallback = Some(TileDiffFallback::Disabled);
            return TileDiffBuild::Fallback(stats);
        }

        if total_cells < config.min_cells_for_tiles {
            stats.fallback = Some(TileDiffFallback::SmallScreen);
            return TileDiffBuild::Fallback(stats);
        }

        if dirty_all {
            stats.fallback = Some(TileDiffFallback::DirtyAll);
            return TileDiffBuild::Fallback(stats);
        }

        if dirty_cell_ratio >= config.dense_cell_ratio {
            stats.fallback = Some(TileDiffFallback::DenseCells);
            return TileDiffBuild::Fallback(stats);
        }

        if total_tiles > config.max_tiles {
            stats.fallback = Some(TileDiffFallback::TooManyTiles);
            return TileDiffBuild::Fallback(stats);
        }

        debug_assert_eq!(dirty_bits.len(), total_cells);
        if dirty_bits.len() < total_cells {
            stats.fallback = Some(TileDiffFallback::Overflow);
            return TileDiffBuild::Fallback(stats);
        }

        self.tile_counts.resize(total_tiles, 0);
        self.tile_counts.fill(0);
        self.dirty_tiles.resize(total_tiles, false);
        self.dirty_tiles.fill(false);

        let tile_w_usize = tile_w as usize;
        let tile_h_usize = tile_h as usize;
        let mut overflow = false;

        for y in 0..height_usize {
            let row_start = y * width_usize;
            let tile_y = y / tile_h_usize;
            for x in 0..width_usize {
                let idx = row_start + x;
                if dirty_bits[idx] == 0 {
                    continue;
                }
                let tile_x = x / tile_w_usize;
                let tile_idx = tile_y * tiles_x + tile_x;
                match self.tile_counts[tile_idx].checked_add(1) {
                    Some(value) => self.tile_counts[tile_idx] = value,
                    None => {
                        overflow = true;
                        break;
                    }
                }
            }
            if overflow {
                break;
            }
        }

        if overflow {
            stats.fallback = Some(TileDiffFallback::Overflow);
            return TileDiffBuild::Fallback(stats);
        }

        let mut dirty_tiles = 0usize;
        for (idx, count) in self.tile_counts.iter().enumerate() {
            if *count > 0 {
                self.dirty_tiles[idx] = true;
                dirty_tiles += 1;
            }
        }

        stats.dirty_tiles = dirty_tiles;
        stats.dirty_tile_ratio = if total_tiles == 0 {
            0.0
        } else {
            dirty_tiles as f64 / total_tiles as f64
        };
        stats.scanned_tiles = dirty_tiles;
        stats.skipped_tiles = total_tiles.saturating_sub(dirty_tiles);
        stats.sat_build_cells = total_cells;
        stats.scan_cells_estimate = dirty_tiles * tile_w_usize * tile_h_usize;

        if stats.dirty_tile_ratio >= config.dense_tile_ratio {
            stats.fallback = Some(TileDiffFallback::DenseTiles);
            return TileDiffBuild::Fallback(stats);
        }

        let sat_w = tiles_x + 1;
        let sat_h = tiles_y + 1;
        let sat_len = sat_w * sat_h;
        self.sat.resize(sat_len, 0);
        self.sat.fill(0);

        for ty in 0..tiles_y {
            let row_base = (ty + 1) * sat_w;
            let prev_base = ty * sat_w;
            for tx in 0..tiles_x {
                let count = self.tile_counts[ty * tiles_x + tx] as u64;
                let above = self.sat[prev_base + tx + 1] as u64;
                let left = self.sat[row_base + tx] as u64;
                let diag = self.sat[prev_base + tx] as u64;
                let value = count + above + left - diag;
                if value > u32::MAX as u64 {
                    stats.fallback = Some(TileDiffFallback::Overflow);
                    return TileDiffBuild::Fallback(stats);
                }
                self.sat[row_base + tx + 1] = value as u32;
            }
        }

        let params = TileParams {
            width,
            height,
            tile_w,
            tile_h,
            tiles_x,
            tiles_y,
        };

        TileDiffBuild::UseTiles(TileDiffPlan {
            params,
            stats,
            dirty_tiles: &self.dirty_tiles,
            tile_counts: &self.tile_counts,
            sat: &self.sat,
        })
    }
}

#[inline]
fn reserve_changes_capacity(width: u16, height: u16, changes: &mut Vec<(u16, u16)>) {
    // Estimate capacity: assume ~5% of cells change on average.
    let estimated_changes = (width as usize * height as usize) / 20;
    let additional = estimated_changes.saturating_sub(changes.len());
    if additional > 0 {
        changes.reserve(additional);
    }
}

fn compute_changes(old: &Buffer, new: &Buffer, changes: &mut Vec<(u16, u16)>) {
    #[cfg(feature = "tracing")]
    let _span = tracing::debug_span!("diff_compute", width = old.width(), height = old.height());
    #[cfg(feature = "tracing")]
    let _guard = _span.enter();

    assert_eq!(old.width(), new.width(), "buffer widths must match");
    assert_eq!(old.height(), new.height(), "buffer heights must match");

    let width = old.width();
    let height = old.height();
    let w = width as usize;

    changes.clear();
    reserve_changes_capacity(width, height, changes);

    let old_cells = old.cells();
    let new_cells = new.cells();

    // Row-major scan with row-skip fast path
    for y in 0..height {
        let row_start = y as usize * w;
        let old_row = &old_cells[row_start..row_start + w];
        let new_row = &new_cells[row_start..row_start + w];

        // Fast path: skip entirely unchanged rows.
        if old_row == new_row {
            continue;
        }

        // Scan for changed cells using blockwise row scan
        scan_row_changes(old_row, new_row, y, changes);
    }

    #[cfg(feature = "tracing")]
    tracing::trace!(changes = changes.len(), "diff computed");
}

fn compute_dirty_changes(
    old: &Buffer,
    new: &Buffer,
    changes: &mut Vec<(u16, u16)>,
    tile_builder: &mut TileDiffBuilder,
    tile_config: &TileDiffConfig,
    tile_stats_out: &mut Option<TileDiffStats>,
) {
    assert_eq!(old.width(), new.width(), "buffer widths must match");
    assert_eq!(old.height(), new.height(), "buffer heights must match");

    let width = old.width();
    let height = old.height();
    let w = width as usize;

    changes.clear();
    reserve_changes_capacity(width, height, changes);

    let old_cells = old.cells();
    let new_cells = new.cells();
    let dirty = new.dirty_rows();

    *tile_stats_out = None;
    let tile_build = tile_builder.build(
        tile_config,
        width,
        height,
        new.dirty_bits(),
        new.dirty_cell_count(),
        new.dirty_all(),
    );

    if let TileDiffBuild::UseTiles(plan) = tile_build {
        *tile_stats_out = Some(plan.stats);
        let tile_w = plan.params.tile_w as usize;
        let tile_h = plan.params.tile_h as usize;
        let tiles_x = plan.params.tiles_x;
        let dirty_tiles = plan.dirty_tiles;

        for y in 0..height {
            if !dirty[y as usize] {
                continue;
            }

            let row_start = y as usize * w;
            let old_row = &old_cells[row_start..row_start + w];
            let new_row = &new_cells[row_start..row_start + w];

            if old_row == new_row {
                continue;
            }

            let tile_y = y as usize / tile_h;
            let tile_row_base = tile_y * tiles_x;
            debug_assert!(tile_row_base + tiles_x <= dirty_tiles.len());

            let span_row = new.dirty_span_row(y);
            if let Some(span_row) = span_row {
                if span_row.is_full() {
                    scan_row_tiles(
                        old_row,
                        new_row,
                        y,
                        w,
                        tile_w,
                        tiles_x,
                        tile_row_base,
                        dirty_tiles,
                        changes,
                    );
                    continue;
                }
                let spans = span_row.spans();
                if spans.is_empty() {
                    scan_row_tiles(
                        old_row,
                        new_row,
                        y,
                        w,
                        tile_w,
                        tiles_x,
                        tile_row_base,
                        dirty_tiles,
                        changes,
                    );
                    continue;
                }
                scan_row_tiles_spans(
                    old_row,
                    new_row,
                    y,
                    w,
                    tile_w,
                    tiles_x,
                    tile_row_base,
                    dirty_tiles,
                    spans,
                    changes,
                );
            } else {
                scan_row_tiles(
                    old_row,
                    new_row,
                    y,
                    w,
                    tile_w,
                    tiles_x,
                    tile_row_base,
                    dirty_tiles,
                    changes,
                );
            }
        }
        return;
    }

    if let TileDiffBuild::Fallback(stats) = tile_build {
        *tile_stats_out = Some(stats);
    }

    for y in 0..height {
        // Skip clean rows (the key optimization).
        if !dirty[y as usize] {
            continue;
        }

        let row_start = y as usize * w;
        let old_row = &old_cells[row_start..row_start + w];
        let new_row = &new_cells[row_start..row_start + w];

        // Even for dirty rows, row-skip fast path applies:
        // a row may be marked dirty but end up identical after compositing.
        if old_row == new_row {
            continue;
        }

        let span_row = new.dirty_span_row(y);
        if let Some(span_row) = span_row {
            if span_row.is_full() {
                scan_row_changes(old_row, new_row, y, changes);
                continue;
            }
            let spans = span_row.spans();
            if spans.is_empty() {
                scan_row_changes(old_row, new_row, y, changes);
                continue;
            }
            scan_row_changes_spans(old_row, new_row, y, spans, changes);
        } else {
            scan_row_changes(old_row, new_row, y, changes);
        }
    }
}

/// A contiguous run of changed cells on a single row.
///
/// Used by the presenter to emit efficient cursor positioning.
/// Instead of positioning for each cell, position once and emit the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeRun {
    /// Row index.
    pub y: u16,
    /// Start column (inclusive).
    pub x0: u16,
    /// End column (inclusive).
    pub x1: u16,
}

impl ChangeRun {
    /// Create a new change run.
    #[inline]
    pub const fn new(y: u16, x0: u16, x1: u16) -> Self {
        debug_assert!(x0 <= x1);
        Self { y, x0, x1 }
    }

    /// Number of cells in this run.
    #[inline]
    pub const fn len(&self) -> u16 {
        self.x1 - self.x0 + 1
    }

    /// Check if this run is empty (should never happen in practice).
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.x1 < self.x0
    }
}

/// The diff between two buffers.
///
/// Contains the list of (x, y) positions where cells differ.
#[derive(Debug, Clone, Default)]
pub struct BufferDiff {
    /// List of changed cell positions (x, y).
    changes: Vec<(u16, u16)>,
    /// Reusable tile builder for SAT-based diff skipping.
    tile_builder: TileDiffBuilder,
    /// Tile diff configuration (thresholds + sizes).
    tile_config: TileDiffConfig,
    /// Last tile diagnostics from a dirty diff pass.
    last_tile_stats: Option<TileDiffStats>,
}

impl BufferDiff {
    /// Create an empty diff.
    pub fn new() -> Self {
        Self {
            changes: Vec::new(),
            tile_builder: TileDiffBuilder::default(),
            tile_config: TileDiffConfig::default(),
            last_tile_stats: None,
        }
    }

    /// Create a diff with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        let mut diff = Self::new();
        diff.changes = Vec::with_capacity(capacity);
        diff
    }

    /// Create a diff that marks every cell as changed.
    ///
    /// Useful for full-screen redraws where the previous buffer is unknown
    /// (e.g., after resize or initial present).
    pub fn full(width: u16, height: u16) -> Self {
        if width == 0 || height == 0 {
            return Self::new();
        }

        let total = width as usize * height as usize;
        let mut changes = Vec::with_capacity(total);
        for y in 0..height {
            for x in 0..width {
                changes.push((x, y));
            }
        }
        let mut diff = Self::new();
        diff.changes = changes;
        diff
    }

    /// Compute the diff between two buffers.
    ///
    /// Uses row-major scan for cache efficiency. Both buffers must have
    /// the same dimensions.
    ///
    /// # Optimizations
    ///
    /// - **Row-skip fast path**: unchanged rows are detected via slice
    ///   equality and skipped entirely. For typical UI updates where most
    ///   rows are static, this eliminates the majority of per-cell work.
    /// - **Blockwise row scan**: rows with sparse edits are scanned in
    ///   coarse blocks, skipping unchanged blocks and only diving into
    ///   the blocks that differ.
    /// - **Direct slice iteration**: row slices are computed once per row
    ///   instead of calling `get_unchecked(x, y)` per cell, eliminating
    ///   repeated `y * width + x` index arithmetic.
    /// - **Branchless cell comparison**: `bits_eq` uses bitwise AND to
    ///   avoid branch mispredictions in the inner loop.
    ///
    /// # Panics
    ///
    /// Debug-asserts that both buffers have identical dimensions.
    pub fn compute(old: &Buffer, new: &Buffer) -> Self {
        let mut diff = Self::new();
        diff.compute_into(old, new);
        diff
    }

    /// Compute the diff into an existing buffer to reuse allocation.
    pub fn compute_into(&mut self, old: &Buffer, new: &Buffer) {
        self.last_tile_stats = None;
        compute_changes(old, new, &mut self.changes);
    }

    /// Compute the diff between two buffers using dirty-row hints.
    ///
    /// Only rows marked dirty in `new` are compared cell-by-cell.
    /// Clean rows are skipped entirely (O(1) per clean row).
    ///
    /// This is sound provided the dirty tracking invariant holds:
    /// for all y, if any cell in row y changed, then `new.is_row_dirty(y)`.
    ///
    /// Falls back to full comparison for rows marked dirty, so false positives
    /// (marking a row dirty when it didn't actually change) are safe — they
    /// only cost the per-cell scan for that row.
    pub fn compute_dirty(old: &Buffer, new: &Buffer) -> Self {
        let mut diff = Self::new();
        diff.compute_dirty_into(old, new);
        diff
    }

    /// Compute the dirty-row diff into an existing buffer to reuse allocation.
    pub fn compute_dirty_into(&mut self, old: &Buffer, new: &Buffer) {
        compute_dirty_changes(
            old,
            new,
            &mut self.changes,
            &mut self.tile_builder,
            &self.tile_config,
            &mut self.last_tile_stats,
        );
    }

    /// Number of changed cells.
    #[inline]
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Check if no cells changed.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Get the list of changed positions.
    #[inline]
    pub fn changes(&self) -> &[(u16, u16)] {
        &self.changes
    }

    /// Access the last tile diagnostics from a dirty diff pass.
    #[inline]
    pub fn last_tile_stats(&self) -> Option<TileDiffStats> {
        self.last_tile_stats
    }

    /// Mutably access tile diff configuration.
    #[inline]
    pub fn tile_config_mut(&mut self) -> &mut TileDiffConfig {
        &mut self.tile_config
    }

    /// Convert point changes into contiguous runs.
    ///
    /// Consecutive x positions on the same row are coalesced into a single run.
    /// This enables efficient cursor positioning in the presenter.
    pub fn runs(&self) -> Vec<ChangeRun> {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!("diff_runs", changes = self.changes.len());
        #[cfg(feature = "tracing")]
        let _guard = _span.enter();

        if self.changes.is_empty() {
            return Vec::new();
        }

        // Changes are already sorted by (y, x) from row-major scan
        // so we don't need to sort again.
        let sorted = &self.changes;
        let len = sorted.len();

        // Worst case: every change is isolated, so runs == changes.
        // Pre-alloc to avoid repeated growth in hot paths.
        let mut runs = Vec::with_capacity(len);

        let mut i = 0;

        while i < len {
            let (x0, y) = sorted[i];
            let mut x1 = x0;
            i += 1;

            // Coalesce consecutive x positions on the same row
            while i < len {
                let (x, yy) = sorted[i];
                if yy != y || x != x1.saturating_add(1) {
                    break;
                }
                x1 = x;
                i += 1;
            }

            runs.push(ChangeRun::new(y, x0, x1));
        }

        #[cfg(feature = "tracing")]
        tracing::trace!(run_count = runs.len(), "runs coalesced");

        runs
    }

    /// Iterate over changed positions.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.changes.iter().copied()
    }

    /// Clear the diff, removing all recorded changes.
    pub fn clear(&mut self) {
        self.changes.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::{Cell, PackedRgba};

    #[test]
    fn empty_diff_when_buffers_identical() {
        let buf1 = Buffer::new(10, 10);
        let buf2 = Buffer::new(10, 10);
        let diff = BufferDiff::compute(&buf1, &buf2);

        assert!(diff.is_empty());
        assert_eq!(diff.len(), 0);
    }

    #[test]
    fn full_diff_marks_all_cells() {
        let diff = BufferDiff::full(3, 2);
        assert_eq!(diff.len(), 6);
        assert_eq!(diff.changes()[0], (0, 0));
        assert_eq!(diff.changes()[5], (2, 1));
    }

    #[test]
    fn single_cell_change_detected() {
        let old = Buffer::new(10, 10);
        let mut new = Buffer::new(10, 10);

        new.set_raw(5, 5, Cell::from_char('X'));
        let diff = BufferDiff::compute(&old, &new);

        assert_eq!(diff.len(), 1);
        assert_eq!(diff.changes(), &[(5, 5)]);
    }

    #[test]
    fn multiple_scattered_changes_detected() {
        let old = Buffer::new(10, 10);
        let mut new = Buffer::new(10, 10);

        new.set_raw(0, 0, Cell::from_char('A'));
        new.set_raw(9, 9, Cell::from_char('B'));
        new.set_raw(5, 3, Cell::from_char('C'));

        let diff = BufferDiff::compute(&old, &new);

        assert_eq!(diff.len(), 3);
        // Sorted by row-major order: (0,0), (5,3), (9,9)
        let changes = diff.changes();
        assert!(changes.contains(&(0, 0)));
        assert!(changes.contains(&(9, 9)));
        assert!(changes.contains(&(5, 3)));
    }

    #[test]
    fn runs_coalesces_adjacent_cells() {
        let old = Buffer::new(10, 10);
        let mut new = Buffer::new(10, 10);

        // Three adjacent cells on row 5
        new.set_raw(3, 5, Cell::from_char('A'));
        new.set_raw(4, 5, Cell::from_char('B'));
        new.set_raw(5, 5, Cell::from_char('C'));

        let diff = BufferDiff::compute(&old, &new);
        let runs = diff.runs();

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].y, 5);
        assert_eq!(runs[0].x0, 3);
        assert_eq!(runs[0].x1, 5);
        assert_eq!(runs[0].len(), 3);
    }

    #[test]
    fn runs_handles_gaps_correctly() {
        let old = Buffer::new(10, 10);
        let mut new = Buffer::new(10, 10);

        // Two groups with a gap
        new.set_raw(0, 0, Cell::from_char('A'));
        new.set_raw(1, 0, Cell::from_char('B'));
        // gap at x=2
        new.set_raw(3, 0, Cell::from_char('C'));
        new.set_raw(4, 0, Cell::from_char('D'));

        let diff = BufferDiff::compute(&old, &new);
        let runs = diff.runs();

        assert_eq!(runs.len(), 2);

        assert_eq!(runs[0].y, 0);
        assert_eq!(runs[0].x0, 0);
        assert_eq!(runs[0].x1, 1);

        assert_eq!(runs[1].y, 0);
        assert_eq!(runs[1].x0, 3);
        assert_eq!(runs[1].x1, 4);
    }

    #[test]
    fn runs_handles_max_column_without_overflow() {
        let mut diff = BufferDiff::new();
        diff.changes = vec![(u16::MAX, 0)];

        let runs = diff.runs();

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0], ChangeRun::new(0, u16::MAX, u16::MAX));
    }

    #[test]
    fn runs_handles_multiple_rows() {
        let old = Buffer::new(10, 10);
        let mut new = Buffer::new(10, 10);

        // Changes on multiple rows
        new.set_raw(0, 0, Cell::from_char('A'));
        new.set_raw(1, 0, Cell::from_char('B'));
        new.set_raw(5, 2, Cell::from_char('C'));
        new.set_raw(0, 5, Cell::from_char('D'));

        let diff = BufferDiff::compute(&old, &new);
        let runs = diff.runs();

        assert_eq!(runs.len(), 3);

        // Row 0: (0-1)
        assert_eq!(runs[0].y, 0);
        assert_eq!(runs[0].x0, 0);
        assert_eq!(runs[0].x1, 1);

        // Row 2: (5)
        assert_eq!(runs[1].y, 2);
        assert_eq!(runs[1].x0, 5);
        assert_eq!(runs[1].x1, 5);

        // Row 5: (0)
        assert_eq!(runs[2].y, 5);
        assert_eq!(runs[2].x0, 0);
        assert_eq!(runs[2].x1, 0);
    }

    #[test]
    fn empty_runs_from_empty_diff() {
        let diff = BufferDiff::new();
        let runs = diff.runs();
        assert!(runs.is_empty());
    }

    #[test]
    fn change_run_len() {
        let run = ChangeRun::new(0, 5, 10);
        assert_eq!(run.len(), 6);

        let single = ChangeRun::new(0, 5, 5);
        assert_eq!(single.len(), 1);
    }

    #[test]
    fn color_changes_detected() {
        let old = Buffer::new(10, 10);
        let mut new = Buffer::new(10, 10);

        // Same empty content but different color
        new.set_raw(5, 5, Cell::default().with_fg(PackedRgba::rgb(255, 0, 0)));

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 1);
    }

    #[test]
    fn diff_iter() {
        let old = Buffer::new(5, 5);
        let mut new = Buffer::new(5, 5);
        new.set_raw(1, 1, Cell::from_char('X'));
        new.set_raw(2, 2, Cell::from_char('Y'));

        let diff = BufferDiff::compute(&old, &new);
        let positions: Vec<_> = diff.iter().collect();

        assert_eq!(positions.len(), 2);
        assert!(positions.contains(&(1, 1)));
        assert!(positions.contains(&(2, 2)));
    }

    #[test]
    fn diff_clear() {
        let old = Buffer::new(5, 5);
        let mut new = Buffer::new(5, 5);
        new.set_raw(1, 1, Cell::from_char('X'));

        let mut diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 1);

        diff.clear();
        assert!(diff.is_empty());
    }

    #[test]
    fn with_capacity() {
        let diff = BufferDiff::with_capacity(100);
        assert!(diff.is_empty());
    }

    #[test]
    fn full_buffer_change() {
        let old = Buffer::new(5, 5);
        let mut new = Buffer::new(5, 5);

        // Change every cell
        for y in 0..5 {
            for x in 0..5 {
                new.set_raw(x, y, Cell::from_char('#'));
            }
        }

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 25);

        // Should coalesce into 5 runs (one per row)
        let runs = diff.runs();
        assert_eq!(runs.len(), 5);

        for (i, run) in runs.iter().enumerate() {
            assert_eq!(run.y, i as u16);
            assert_eq!(run.x0, 0);
            assert_eq!(run.x1, 4);
            assert_eq!(run.len(), 5);
        }
    }

    #[test]
    fn row_major_order_preserved() {
        let old = Buffer::new(3, 3);
        let mut new = Buffer::new(3, 3);

        // Set cells in non-row-major order
        new.set_raw(2, 2, Cell::from_char('C'));
        new.set_raw(0, 0, Cell::from_char('A'));
        new.set_raw(1, 1, Cell::from_char('B'));

        let diff = BufferDiff::compute(&old, &new);

        // Row-major scan should produce (0,0), (1,1), (2,2)
        let changes = diff.changes();
        assert_eq!(changes[0], (0, 0));
        assert_eq!(changes[1], (1, 1));
        assert_eq!(changes[2], (2, 2));
    }

    #[test]
    fn blockwise_scan_preserves_sparse_row_changes() {
        let old = Buffer::new(64, 2);
        let mut new = old.clone();

        new.set_raw(1, 0, Cell::from_char('A'));
        new.set_raw(33, 0, Cell::from_char('B'));
        new.set_raw(62, 1, Cell::from_char('C'));

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.changes(), &[(1, 0), (33, 0), (62, 1)]);
    }

    #[test]
    fn rows_with_no_changes_are_skipped() {
        let old = Buffer::new(4, 3);
        let mut new = old.clone();

        new.set_raw(1, 1, Cell::from_char('X'));
        new.set_raw(3, 1, Cell::from_char('Y'));

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 2);
        assert!(diff.changes().iter().all(|&(_, y)| y == 1));
    }

    #[test]
    fn clear_retains_capacity_for_reuse() {
        let mut diff = BufferDiff::with_capacity(16);
        diff.changes.extend_from_slice(&[(0, 0), (1, 0), (2, 0)]);
        let capacity = diff.changes.capacity();

        diff.clear();

        assert!(diff.is_empty());
        assert!(diff.changes.capacity() >= capacity);
    }

    #[test]
    #[should_panic(expected = "buffer widths must match")]
    fn compute_panics_on_width_mismatch() {
        let old = Buffer::new(5, 5);
        let new = Buffer::new(4, 5);
        let _ = BufferDiff::compute(&old, &new);
    }

    // =========================================================================
    // Block-based Row Scan Tests (bd-4kq0.1.2)
    // =========================================================================

    #[test]
    fn block_scan_alignment_exact_block() {
        // Width = 4 (exactly one block, no remainder)
        let old = Buffer::new(4, 1);
        let mut new = Buffer::new(4, 1);
        new.set_raw(2, 0, Cell::from_char('X'));

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff.changes(), &[(2, 0)]);
    }

    #[test]
    fn block_scan_alignment_remainder() {
        // Width = 7 (one full block + 3 remainder)
        let old = Buffer::new(7, 1);
        let mut new = Buffer::new(7, 1);
        // Change in full block part
        new.set_raw(1, 0, Cell::from_char('A'));
        // Change in remainder part
        new.set_raw(5, 0, Cell::from_char('B'));
        new.set_raw(6, 0, Cell::from_char('C'));

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 3);
        assert_eq!(diff.changes(), &[(1, 0), (5, 0), (6, 0)]);
    }

    #[test]
    fn block_scan_single_cell_row() {
        // Width = 1 (pure remainder, no full blocks)
        let old = Buffer::new(1, 1);
        let mut new = Buffer::new(1, 1);
        new.set_raw(0, 0, Cell::from_char('X'));

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff.changes(), &[(0, 0)]);
    }

    #[test]
    fn block_scan_two_cell_row() {
        // Width = 2 (pure remainder)
        let old = Buffer::new(2, 1);
        let mut new = Buffer::new(2, 1);
        new.set_raw(0, 0, Cell::from_char('A'));
        new.set_raw(1, 0, Cell::from_char('B'));

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 2);
        assert_eq!(diff.changes(), &[(0, 0), (1, 0)]);
    }

    #[test]
    fn block_scan_three_cell_row() {
        // Width = 3 (pure remainder)
        let old = Buffer::new(3, 1);
        let mut new = Buffer::new(3, 1);
        new.set_raw(2, 0, Cell::from_char('X'));

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff.changes(), &[(2, 0)]);
    }

    #[test]
    fn block_scan_multiple_blocks_sparse() {
        // Width = 80 (20 full blocks), changes scattered across blocks
        let old = Buffer::new(80, 1);
        let mut new = Buffer::new(80, 1);

        // One change per block in every other block
        for block in (0..20).step_by(2) {
            let x = (block * 4 + 1) as u16;
            new.set_raw(x, 0, Cell::from_char('X'));
        }

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 10);
        assert_eq!(
            diff.changes(),
            &[
                (1, 0),
                (5, 0),
                (9, 0),
                (13, 0),
                (17, 0),
                (21, 0),
                (25, 0),
                (29, 0),
                (33, 0),
                (37, 0)
            ]
        );
    }

    #[test]
    fn block_scan_full_block_unchanged_skip() {
        // Verify blocks with no changes are skipped efficiently
        let old = Buffer::new(20, 1);
        let mut new = Buffer::new(20, 1);

        // Only change one cell in the last block
        new.set_raw(19, 0, Cell::from_char('Z'));

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff.changes(), &[(19, 0)]);
    }

    #[test]
    fn block_scan_wide_row_all_changed() {
        // All cells changed in a wide row
        let old = Buffer::new(120, 1);
        let mut new = Buffer::new(120, 1);
        for x in 0..120 {
            new.set_raw(x, 0, Cell::from_char('#'));
        }

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.len(), 120);
    }

    #[test]
    fn perf_block_scan_vs_scalar_baseline() {
        // Verify that block scan works correctly on large buffers
        // and measure relative performance
        use std::time::Instant;

        let width = 200u16;
        let height = 50u16;
        let old = Buffer::new(width, height);
        let mut new = Buffer::new(width, height);

        // ~10% cells changed
        for i in 0..1000 {
            let x = (i * 7 + 3) as u16 % width;
            let y = (i * 11 + 5) as u16 % height;
            let ch = char::from_u32(('A' as u32) + (i as u32 % 26)).unwrap();
            new.set_raw(x, y, Cell::from_char(ch));
        }

        let iterations = 1000;
        let start = Instant::now();
        for _ in 0..iterations {
            let diff = BufferDiff::compute(&old, &new);
            assert!(!diff.is_empty());
        }
        let elapsed = start.elapsed();

        // Should complete 1000 iterations of 200x50 diff in < 500ms
        assert!(
            elapsed.as_millis() < 500,
            "Diff too slow: {elapsed:?} for {iterations} iterations of {width}x{height}"
        );
    }
    // =========================================================================
    // Run Coalescing Invariants (bd-4kq0.1.3)
    // =========================================================================

    #[test]
    fn unit_run_coalescing_invariants() {
        // Verify runs preserve order, coverage, and contiguity for a
        // complex multi-row change pattern.
        let old = Buffer::new(80, 24);
        let mut new = Buffer::new(80, 24);

        // Row 0: two separate runs (0-2) and (10-12)
        for x in 0..=2 {
            new.set_raw(x, 0, Cell::from_char('A'));
        }
        for x in 10..=12 {
            new.set_raw(x, 0, Cell::from_char('B'));
        }
        // Row 5: single run (40-45)
        for x in 40..=45 {
            new.set_raw(x, 5, Cell::from_char('C'));
        }
        // Row 23: single cell at end
        new.set_raw(79, 23, Cell::from_char('Z'));

        let diff = BufferDiff::compute(&old, &new);
        let runs = diff.runs();

        // Invariant 1: runs are sorted by (y, x0)
        for w in runs.windows(2) {
            assert!(
                (w[0].y, w[0].x0) < (w[1].y, w[1].x0),
                "runs must be sorted: {:?} should precede {:?}",
                w[0],
                w[1]
            );
        }

        // Invariant 2: total cells in runs == diff.len()
        let total_cells: usize = runs.iter().map(|r| r.len() as usize).sum();
        assert_eq!(
            total_cells,
            diff.len(),
            "runs must cover all changes exactly"
        );

        // Invariant 3: no two runs on the same row are adjacent (should have merged)
        for w in runs.windows(2) {
            if w[0].y == w[1].y {
                assert!(
                    w[1].x0 > w[0].x1.saturating_add(1),
                    "adjacent runs on same row should be merged: {:?} and {:?}",
                    w[0],
                    w[1]
                );
            }
        }

        // Invariant 4: expected structure
        assert_eq!(runs.len(), 4);
        assert_eq!(runs[0], ChangeRun::new(0, 0, 2));
        assert_eq!(runs[1], ChangeRun::new(0, 10, 12));
        assert_eq!(runs[2], ChangeRun::new(5, 40, 45));
        assert_eq!(runs[3], ChangeRun::new(23, 79, 79));
    }

    // =========================================================================
    // Golden Output Fixtures (bd-4kq0.1.3)
    // =========================================================================

    /// FNV-1a hash for deterministic checksums (no external dependency).
    fn fnv1a_hash(data: &[(u16, u16)]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &(x, y) in data {
            for byte in x.to_le_bytes().iter().chain(y.to_le_bytes().iter()) {
                hash ^= *byte as u64;
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
        hash
    }

    /// Build a canonical "dashboard-like" scene: header row, status bar,
    /// scattered content cells.
    fn build_golden_scene(width: u16, height: u16, seed: u64) -> Buffer {
        let mut buf = Buffer::new(width, height);
        let mut rng = seed;

        // Header row: all cells set
        for x in 0..width {
            buf.set_raw(x, 0, Cell::from_char('='));
        }

        // Status bar (last row)
        for x in 0..width {
            buf.set_raw(x, height - 1, Cell::from_char('-'));
        }

        // Scattered content using simple LCG
        let count = (width as u64 * height as u64 / 10).max(5);
        for _ in 0..count {
            rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let x = ((rng >> 16) as u16) % width;
            rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let y = ((rng >> 16) as u16) % height;
            rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let ch = char::from_u32('A' as u32 + (rng % 26) as u32).unwrap();
            buf.set_raw(x, y, Cell::from_char(ch));
        }

        buf
    }

    #[test]
    fn golden_diff_80x24() {
        let old = Buffer::new(80, 24);
        let new = build_golden_scene(80, 24, 0x000D_01DE_5EED_0001);

        let diff = BufferDiff::compute(&old, &new);
        let checksum = fnv1a_hash(diff.changes());

        // Verify determinism: same inputs → same output
        let diff2 = BufferDiff::compute(&old, &new);
        assert_eq!(
            fnv1a_hash(diff2.changes()),
            checksum,
            "diff must be deterministic"
        );

        // Sanity: scene has header + status + scattered content
        assert!(
            diff.len() >= 160,
            "80x24 golden scene should have at least 160 changes (header+status), got {}",
            diff.len()
        );

        let runs = diff.runs();
        // Header and status rows should each be one run
        assert_eq!(runs[0].y, 0, "first run should be header row");
        assert_eq!(runs[0].x0, 0);
        assert_eq!(runs[0].x1, 79);
        assert!(
            runs.last().unwrap().y == 23,
            "last row should contain status bar"
        );
    }

    #[test]
    fn golden_diff_120x40() {
        let old = Buffer::new(120, 40);
        let new = build_golden_scene(120, 40, 0x000D_01DE_5EED_0002);

        let diff = BufferDiff::compute(&old, &new);
        let checksum = fnv1a_hash(diff.changes());

        // Determinism
        let diff2 = BufferDiff::compute(&old, &new);
        assert_eq!(fnv1a_hash(diff2.changes()), checksum);

        // Sanity
        assert!(
            diff.len() >= 240,
            "120x40 golden scene should have >=240 changes, got {}",
            diff.len()
        );

        // Dirty diff must match
        let dirty = BufferDiff::compute_dirty(&old, &new);
        assert_eq!(
            fnv1a_hash(dirty.changes()),
            checksum,
            "dirty diff must produce identical changes"
        );
    }

    #[test]
    fn golden_sparse_update() {
        // Start from a populated scene, apply a small update
        let old = build_golden_scene(80, 24, 0x000D_01DE_5EED_0003);
        let mut new = old.clone();

        // Apply 5 deterministic changes
        new.set_raw(10, 5, Cell::from_char('!'));
        new.set_raw(11, 5, Cell::from_char('@'));
        new.set_raw(40, 12, Cell::from_char('#'));
        new.set_raw(70, 20, Cell::from_char('$'));
        new.set_raw(0, 23, Cell::from_char('%'));

        let diff = BufferDiff::compute(&old, &new);
        let checksum = fnv1a_hash(diff.changes());

        // Determinism
        let diff2 = BufferDiff::compute(&old, &new);
        assert_eq!(fnv1a_hash(diff2.changes()), checksum);

        // Exactly 5 changes (or fewer if some cells happened to already have that value)
        assert!(
            diff.len() <= 5,
            "sparse update should have <=5 changes, got {}",
            diff.len()
        );
        assert!(
            diff.len() >= 3,
            "sparse update should have >=3 changes, got {}",
            diff.len()
        );
    }

    // =========================================================================
    // E2E Random Scene Replay (bd-4kq0.1.3)
    // =========================================================================

    #[test]
    fn e2e_random_scene_replay() {
        // 10 frames of seeded scene evolution, verify checksums are
        // deterministic across replays and dirty/full paths agree.
        let width = 80u16;
        let height = 24u16;
        let base_seed: u64 = 0x5C3E_E3E1_A442u64;

        let mut checksums = Vec::new();

        for frame in 0..10u64 {
            let seed = base_seed.wrapping_add(frame.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let old = build_golden_scene(width, height, seed);
            let new = build_golden_scene(width, height, seed.wrapping_add(1));

            let diff = BufferDiff::compute(&old, &new);
            let dirty_diff = BufferDiff::compute_dirty(&old, &new);

            // Full and dirty must agree
            assert_eq!(
                diff.changes(),
                dirty_diff.changes(),
                "frame {frame}: dirty diff must match full diff"
            );

            checksums.push(fnv1a_hash(diff.changes()));
        }

        // Replay: same seeds must produce identical checksums
        for frame in 0..10u64 {
            let seed = base_seed.wrapping_add(frame.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let old = build_golden_scene(width, height, seed);
            let new = build_golden_scene(width, height, seed.wrapping_add(1));

            let diff = BufferDiff::compute(&old, &new);
            assert_eq!(
                fnv1a_hash(diff.changes()),
                checksums[frame as usize],
                "frame {frame}: checksum mismatch on replay"
            );
        }
    }

    // =========================================================================
    // Perf Microbench with JSONL (bd-4kq0.1.3)
    // =========================================================================

    #[test]
    fn perf_diff_microbench() {
        use std::time::Instant;

        let scenarios: &[(u16, u16, &str, u64)] = &[
            (80, 24, "full_frame", 0xBE4C_0001u64),
            (80, 24, "sparse_update", 0xBE4C_0002u64),
            (120, 40, "full_frame", 0xBE4C_0003u64),
            (120, 40, "sparse_update", 0xBE4C_0004u64),
        ];

        let iterations = std::env::var("FTUI_DIFF_BENCH_ITERS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(50u32);

        for &(width, height, scene_type, seed) in scenarios {
            let old = Buffer::new(width, height);
            let new = match scene_type {
                "full_frame" => build_golden_scene(width, height, seed),
                "sparse_update" => {
                    let mut buf = old.clone();
                    buf.set_raw(10, 5, Cell::from_char('!'));
                    buf.set_raw(40, 12, Cell::from_char('#'));
                    buf.set_raw(70 % width, 20 % height, Cell::from_char('$'));
                    buf
                }
                _ => unreachable!(),
            };

            let mut times_us = Vec::with_capacity(iterations as usize);
            let mut last_changes = 0usize;
            let mut last_runs = 0usize;
            let mut last_checksum = 0u64;

            for _ in 0..iterations {
                let start = Instant::now();
                let diff = BufferDiff::compute(&old, &new);
                let runs = diff.runs();
                let elapsed = start.elapsed();

                last_changes = diff.len();
                last_runs = runs.len();
                last_checksum = fnv1a_hash(diff.changes());
                times_us.push(elapsed.as_micros() as u64);
            }

            times_us.sort();
            let len = times_us.len();
            let p50 = times_us[len / 2];
            let p95 = times_us[((len as f64 * 0.95) as usize).min(len.saturating_sub(1))];
            let p99 = times_us[((len as f64 * 0.99) as usize).min(len.saturating_sub(1))];

            // JSONL log line (captured by --nocapture or CI artifact)
            eprintln!(
                "{{\"ts\":\"2026-02-03T00:00:00Z\",\"seed\":{},\"width\":{},\"height\":{},\"scene\":\"{}\",\"changes\":{},\"runs\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"checksum\":\"0x{:016x}\"}}",
                seed,
                width,
                height,
                scene_type,
                last_changes,
                last_runs,
                p50,
                p95,
                p99,
                last_checksum
            );

            // Budget: full 80x24 diff should be < 100µs at p95
            // Budget: full 120x40 diff should be < 200µs at p95
            let budget_us = match (width, height) {
                (80, 24) => 500,   // generous for CI variance
                (120, 40) => 1000, // generous for CI variance
                _ => 2000,
            };

            // Checksum must be identical across all iterations (determinism)
            for _ in 0..3 {
                let diff = BufferDiff::compute(&old, &new);
                assert_eq!(
                    fnv1a_hash(diff.changes()),
                    last_checksum,
                    "diff must be deterministic"
                );
            }

            // Soft budget assertion (warn but don't fail on slow CI)
            if p95 > budget_us {
                eprintln!(
                    "WARN: {scene_type} {width}x{height} p95={p95}µs exceeds budget {budget_us}µs"
                );
            }
        }
    }

    // =========================================================================
    // Dirty vs Full Diff Regression Gate (bd-3e1t.1.6)
    // =========================================================================

    #[test]
    fn perf_dirty_diff_large_screen_regression() {
        use std::time::Instant;

        let iterations = std::env::var("FTUI_DIFF_BENCH_ITERS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(50u32);

        let max_slowdown = std::env::var("FTUI_DIRTY_DIFF_MAX_SLOWDOWN")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(2.0);

        let cases: &[(u16, u16, &str, f64)] = &[
            (200, 60, "sparse_5pct", 5.0),
            (240, 80, "sparse_5pct", 5.0),
            (200, 60, "single_row", 0.0),
            (240, 80, "single_row", 0.0),
        ];

        for &(width, height, pattern, pct) in cases {
            let old = Buffer::new(width, height);
            let mut new = old.clone();

            if pattern == "single_row" {
                for x in 0..width {
                    new.set_raw(x, 0, Cell::from_char('X'));
                }
            } else {
                let total = width as usize * height as usize;
                let to_change = ((total as f64) * pct / 100.0) as usize;
                for i in 0..to_change {
                    let x = (i * 7 + 3) as u16 % width;
                    let y = (i * 11 + 5) as u16 % height;
                    let ch = char::from_u32(('A' as u32) + (i as u32 % 26)).unwrap();
                    new.set_raw(x, y, Cell::from_char(ch));
                }
            }

            // Sanity: dirty and full diffs must agree.
            let full = BufferDiff::compute(&old, &new);
            let dirty = BufferDiff::compute_dirty(&old, &new);
            let change_count = full.len();
            let dirty_rows = new.dirty_row_count();
            assert_eq!(
                full.changes(),
                dirty.changes(),
                "dirty diff must match full diff for {width}x{height} {pattern}"
            );

            let mut full_times = Vec::with_capacity(iterations as usize);
            let mut dirty_times = Vec::with_capacity(iterations as usize);

            for _ in 0..iterations {
                let start = Instant::now();
                let diff = BufferDiff::compute(&old, &new);
                std::hint::black_box(diff.len());
                full_times.push(start.elapsed().as_micros() as u64);

                let start = Instant::now();
                let diff = BufferDiff::compute_dirty(&old, &new);
                std::hint::black_box(diff.len());
                dirty_times.push(start.elapsed().as_micros() as u64);
            }

            full_times.sort();
            dirty_times.sort();

            let len = full_times.len();
            let p50_idx = len / 2;
            let p95_idx = ((len as f64 * 0.95) as usize).min(len.saturating_sub(1));

            let full_p50 = full_times[p50_idx];
            let full_p95 = full_times[p95_idx];
            let dirty_p50 = dirty_times[p50_idx];
            let dirty_p95 = dirty_times[p95_idx];

            let denom = full_p50.max(1) as f64;
            let ratio = dirty_p50 as f64 / denom;

            eprintln!(
                "{{\"ts\":\"2026-02-03T00:00:00Z\",\"event\":\"diff_regression\",\"width\":{},\"height\":{},\"pattern\":\"{}\",\"iterations\":{},\"changes\":{},\"dirty_rows\":{},\"full_p50_us\":{},\"full_p95_us\":{},\"dirty_p50_us\":{},\"dirty_p95_us\":{},\"slowdown_ratio\":{:.3},\"max_slowdown\":{}}}",
                width,
                height,
                pattern,
                iterations,
                change_count,
                dirty_rows,
                full_p50,
                full_p95,
                dirty_p50,
                dirty_p95,
                ratio,
                max_slowdown
            );

            assert!(
                ratio <= max_slowdown,
                "dirty diff regression: {width}x{height} {pattern} ratio {ratio:.2} exceeds {max_slowdown}"
            );
        }
    }

    #[test]
    fn tile_diff_matches_compute_for_sparse_tiles() {
        let width = 200;
        let height = 60;
        let old = Buffer::new(width, height);
        let mut new = old.clone();

        new.clear_dirty();
        for x in 0..10u16 {
            new.set_raw(x, 0, Cell::from_char('T'));
        }

        let full = BufferDiff::compute(&old, &new);
        let dirty = BufferDiff::compute_dirty(&old, &new);

        assert_eq!(full.changes(), dirty.changes());
        let stats = dirty
            .last_tile_stats()
            .expect("tile stats should be recorded");
        assert!(
            stats.fallback.is_none(),
            "tile path should be used for sparse tiles"
        );
    }

    fn lcg_next(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *state
    }

    fn apply_random_changes(buf: &mut Buffer, seed: u64, count: usize) {
        let width = buf.width().max(1) as u64;
        let height = buf.height().max(1) as u64;
        let mut state = seed;
        for i in 0..count {
            let v = lcg_next(&mut state);
            let x = (v % width) as u16;
            let y = ((v >> 32) % height) as u16;
            let ch = char::from_u32(('A' as u32) + ((i as u32) % 26)).unwrap();
            buf.set_raw(x, y, Cell::from_char(ch));
        }
    }

    fn tile_diag(stats: &TileDiffStats) -> String {
        let tile_size = stats.tile_w as usize * stats.tile_h as usize;
        format!(
            "tile_size={tile_size}, dirty_tiles={}, skipped_tiles={}, dirty_cells={}, dirty_tile_ratio={:.3}, dirty_cell_ratio={:.3}, scanned_tiles={}, fallback={:?}",
            stats.dirty_tiles,
            stats.skipped_tiles,
            stats.dirty_cells,
            stats.dirty_tile_ratio,
            stats.dirty_cell_ratio,
            stats.scanned_tiles,
            stats.fallback
        )
    }

    fn diff_with_forced_tiles(old: &Buffer, new: &Buffer) -> (BufferDiff, TileDiffStats) {
        let mut diff = BufferDiff::new();
        {
            let config = diff.tile_config_mut();
            config.enabled = true;
            config.tile_w = 8;
            config.tile_h = 8;
            config.min_cells_for_tiles = 0;
            config.dense_cell_ratio = 1.1;
            config.dense_tile_ratio = 1.1;
            config.max_tiles = usize::MAX / 4;
        }
        diff.compute_dirty_into(old, new);
        let stats = diff
            .last_tile_stats()
            .expect("tile stats should be recorded");
        (diff, stats)
    }

    fn assert_tile_diff_equivalence(old: &Buffer, new: &Buffer, label: &str) {
        let full = BufferDiff::compute(old, new);
        let (dirty, stats) = diff_with_forced_tiles(old, new);
        let diag = tile_diag(&stats);
        assert!(
            stats.fallback.is_none(),
            "tile diff fallback ({label}) {w}x{h}: {diag}",
            w = old.width(),
            h = old.height()
        );
        assert!(
            full.changes() == dirty.changes(),
            "tile diff mismatch ({label}) {w}x{h}: {diag}",
            w = old.width(),
            h = old.height()
        );
    }

    #[test]
    fn tile_diff_equivalence_small_and_odd_sizes() {
        let cases: &[(u16, u16, usize)] = &[
            (1, 1, 1),
            (2, 1, 1),
            (1, 2, 1),
            (5, 3, 4),
            (7, 13, 12),
            (15, 9, 20),
            (31, 5, 12),
        ];

        for (idx, &(width, height, changes)) in cases.iter().enumerate() {
            let old = Buffer::new(width, height);
            let mut new = old.clone();
            new.clear_dirty();
            apply_random_changes(&mut new, 0xC0FFEE_u64 + idx as u64, changes);
            assert_tile_diff_equivalence(&old, &new, "small_odd");
        }
    }

    #[test]
    fn tile_diff_equivalence_large_sparse_random() {
        let cases: &[(u16, u16)] = &[(200, 60), (240, 80)];
        for (idx, &(width, height)) in cases.iter().enumerate() {
            let old = Buffer::new(width, height);
            let mut new = old.clone();
            new.clear_dirty();
            let total = width as usize * height as usize;
            let changes = (total / 100).max(1);
            apply_random_changes(&mut new, 0xDEADBEEF_u64 + idx as u64, changes);
            assert_tile_diff_equivalence(&old, &new, "large_sparse");
        }
    }

    #[test]
    fn tile_diff_equivalence_row_and_full_buffer() {
        let width = 200u16;
        let height = 60u16;
        let old = Buffer::new(width, height);

        let mut row = old.clone();
        row.clear_dirty();
        for x in 0..width {
            row.set_raw(x, 0, Cell::from_char('R'));
        }
        assert_tile_diff_equivalence(&old, &row, "single_row");

        let mut full = old.clone();
        full.clear_dirty();
        for y in 0..height {
            for x in 0..width {
                full.set_raw(x, y, Cell::from_char('F'));
            }
        }
        assert_tile_diff_equivalence(&old, &full, "full_buffer");
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use crate::cell::Cell;
    use ftui_core::geometry::Rect;
    use proptest::prelude::*;

    // Property: Applying diff changes to old buffer produces new buffer.
    #[test]
    fn diff_apply_produces_target() {
        proptest::proptest!(|(
            width in 5u16..50,
            height in 5u16..30,
            num_changes in 0usize..200,
        )| {
            // Create old buffer (all spaces)
            let old = Buffer::new(width, height);

            // Create new buffer by making random changes
            let mut new = old.clone();
            for i in 0..num_changes {
                let x = (i * 7 + 3) as u16 % width;
                let y = (i * 11 + 5) as u16 % height;
                let ch = char::from_u32(('A' as u32) + (i as u32 % 26)).unwrap();
                new.set_raw(x, y, Cell::from_char(ch));
            }

            // Compute diff
            let diff = BufferDiff::compute(&old, &new);

            // Apply diff to old should produce new
            let mut result = old.clone();
            for (x, y) in diff.iter() {
                let cell = *new.get_unchecked(x, y);
                result.set_raw(x, y, cell);
            }

            // Verify buffers match
            for y in 0..height {
                for x in 0..width {
                    let result_cell = result.get_unchecked(x, y);
                    let new_cell = new.get_unchecked(x, y);
                    prop_assert!(
                        result_cell.bits_eq(new_cell),
                        "Mismatch at ({}, {})",
                        x,
                        y
                    );
                }
            }
        });
    }

    // Property: Diff is empty when buffers are identical.
    #[test]
    fn identical_buffers_empty_diff() {
        proptest::proptest!(|(width in 1u16..100, height in 1u16..50)| {
            let buf = Buffer::new(width, height);
            let diff = BufferDiff::compute(&buf, &buf);
            prop_assert!(diff.is_empty(), "Identical buffers should have empty diff");
        });
    }

    // Property: Every change in diff corresponds to an actual difference.
    #[test]
    fn diff_contains_only_real_changes() {
        proptest::proptest!(|(
            width in 5u16..50,
            height in 5u16..30,
            num_changes in 0usize..100,
        )| {
            let old = Buffer::new(width, height);
            let mut new = old.clone();

            for i in 0..num_changes {
                let x = (i * 7 + 3) as u16 % width;
                let y = (i * 11 + 5) as u16 % height;
                new.set_raw(x, y, Cell::from_char('X'));
            }

            let diff = BufferDiff::compute(&old, &new);

            // Every change position should actually differ
            for (x, y) in diff.iter() {
                let old_cell = old.get_unchecked(x, y);
                let new_cell = new.get_unchecked(x, y);
                prop_assert!(
                    !old_cell.bits_eq(new_cell),
                    "Diff includes unchanged cell at ({}, {})",
                    x,
                    y
                );
            }
        });
    }

    // Property: Runs correctly coalesce adjacent changes.
    #[test]
    fn runs_are_contiguous() {
        proptest::proptest!(|(width in 10u16..80, height in 5u16..30)| {
            let old = Buffer::new(width, height);
            let mut new = old.clone();

            // Create some horizontal runs
            for y in 0..height.min(5) {
                for x in 0..width.min(10) {
                    new.set_raw(x, y, Cell::from_char('#'));
                }
            }

            let diff = BufferDiff::compute(&old, &new);
            let runs = diff.runs();

            // Verify each run is contiguous
            for run in runs {
                prop_assert!(run.x1 >= run.x0, "Run has invalid range");
                prop_assert!(!run.is_empty(), "Run should not be empty");

                // Verify all cells in run are actually changed
                for x in run.x0..=run.x1 {
                    let old_cell = old.get_unchecked(x, run.y);
                    let new_cell = new.get_unchecked(x, run.y);
                    prop_assert!(
                        !old_cell.bits_eq(new_cell),
                        "Run includes unchanged cell at ({}, {})",
                        x,
                        run.y
                    );
                }
            }
        });
    }

    // Property: Runs cover all changes exactly once.
    #[test]
    fn runs_cover_all_changes() {
        proptest::proptest!(|(
            width in 10u16..60,
            height in 5u16..30,
            num_changes in 1usize..100,
        )| {
            let old = Buffer::new(width, height);
            let mut new = old.clone();

            for i in 0..num_changes {
                let x = (i * 13 + 7) as u16 % width;
                let y = (i * 17 + 3) as u16 % height;
                new.set_raw(x, y, Cell::from_char('X'));
            }

            let diff = BufferDiff::compute(&old, &new);
            let runs = diff.runs();

            // Count cells covered by runs
            let mut run_cells: std::collections::HashSet<(u16, u16)> =
                std::collections::HashSet::new();
            for run in &runs {
                for x in run.x0..=run.x1 {
                    let was_new = run_cells.insert((x, run.y));
                    prop_assert!(was_new, "Duplicate cell ({}, {}) in runs", x, run.y);
                }
            }

            // Verify runs cover exactly the changes
            for (x, y) in diff.iter() {
                prop_assert!(
                    run_cells.contains(&(x, y)),
                    "Change at ({}, {}) not covered by runs",
                    x,
                    y
                );
            }

            prop_assert_eq!(
                run_cells.len(),
                diff.len(),
                "Run cell count should match diff change count"
            );
        });
    }

    // Property (bd-4kq0.1.2): Block-based scan matches scalar scan
    // for random row widths and change patterns. This verifies the
    // block/remainder handling is correct for all alignment cases.
    #[test]
    fn block_scan_matches_scalar() {
        proptest::proptest!(|(
            width in 1u16..200,
            height in 1u16..20,
            num_changes in 0usize..200,
        )| {
            use crate::cell::PackedRgba;

            let old = Buffer::new(width, height);
            let mut new = Buffer::new(width, height);

            for i in 0..num_changes {
                let x = (i * 13 + 7) as u16 % width;
                let y = (i * 17 + 3) as u16 % height;
                let fg = PackedRgba::rgb(
                    ((i * 31) % 256) as u8,
                    ((i * 47) % 256) as u8,
                    ((i * 71) % 256) as u8,
                );
                new.set_raw(x, y, Cell::from_char('X').with_fg(fg));
            }

            let diff = BufferDiff::compute(&old, &new);

            // Verify against manual scalar scan
            let mut scalar_changes = Vec::new();
            for y in 0..height {
                for x in 0..width {
                    let old_cell = old.get_unchecked(x, y);
                    let new_cell = new.get_unchecked(x, y);
                    if !old_cell.bits_eq(new_cell) {
                        scalar_changes.push((x, y));
                    }
                }
            }

            prop_assert_eq!(
                diff.changes(),
                &scalar_changes[..],
                "Block scan should match scalar scan"
            );
        });
    }

    // ========== Diff Equivalence: dirty+block vs full scan (bd-4kq0.1.3) ==========

    // Property: compute_dirty with all rows dirty matches compute exactly.
    // This verifies the block-scan + dirty-row path is semantically
    // equivalent to the full scan for random buffers.
    #[test]
    fn property_diff_equivalence() {
        proptest::proptest!(|(
            width in 1u16..120,
            height in 1u16..40,
            num_changes in 0usize..300,
        )| {
            let old = Buffer::new(width, height);
            let mut new = Buffer::new(width, height);

            // Apply deterministic pseudo-random changes
            for i in 0..num_changes {
                let x = (i * 13 + 7) as u16 % width;
                let y = (i * 17 + 3) as u16 % height;
                let ch = char::from_u32(('A' as u32) + (i as u32 % 26)).unwrap();
                let fg = crate::cell::PackedRgba::rgb(
                    ((i * 31) % 256) as u8,
                    ((i * 47) % 256) as u8,
                    ((i * 71) % 256) as u8,
                );
                new.set_raw(x, y, Cell::from_char(ch).with_fg(fg));
            }

            let full = BufferDiff::compute(&old, &new);
            let dirty = BufferDiff::compute_dirty(&old, &new);

            prop_assert_eq!(
                full.changes(),
                dirty.changes(),
                "dirty diff must match full diff (width={}, height={}, changes={})",
                width,
                height,
                num_changes
            );

            // Also verify run coalescing is identical
            let full_runs = full.runs();
            let dirty_runs = dirty.runs();
            prop_assert_eq!(full_runs.len(), dirty_runs.len(), "run count must match");
            for (fr, dr) in full_runs.iter().zip(dirty_runs.iter()) {
                prop_assert_eq!(fr, dr, "run mismatch");
            }
        });
    }

    // Property: compute_dirty matches compute for random fill/set operations.
    // This exercises span merging and complex dirty patterns (bd-3e1t.6.4).
    #[test]
    fn property_diff_equivalence_complex_spans() {
        proptest::proptest!(|(
            width in 10u16..100,
            height in 10u16..50,
            ops in proptest::collection::vec(
                prop_oneof![
                    // Single cell set
                    (Just(0u8), any::<u16>(), any::<u16>(), any::<char>()),
                    // Region fill (small rects)
                    (Just(1u8), any::<u16>(), any::<u16>(), any::<char>()),
                ],
                1..50
            )
        )| {
            let old = Buffer::new(width, height);
            let mut new = old.clone();

            // Clear dirty state on new so we start fresh tracking
            new.clear_dirty();

            for (op_type, x, y, ch) in ops {
                let x = x % width;
                let y = y % height;
                let cell = Cell::from_char(ch);

                match op_type {
                    0 => new.set(x, y, cell),
                    1 => {
                        // Random small rect
                        let w = ((x + 10).min(width) - x).max(1);
                        let h = ((y + 5).min(height) - y).max(1);
                        new.fill(Rect::new(x, y, w, h), cell);
                    }
                    _ => unreachable!(),
                }
            }

            let full = BufferDiff::compute(&old, &new);
            let dirty = BufferDiff::compute_dirty(&old, &new);

            prop_assert_eq!(
                full.changes(),
                dirty.changes(),
                "dirty diff (spans) must match full diff"
            );
        });
    }

    // ========== Idempotence Property (bd-1rz0.6) ==========

    // Property: Diff is idempotent - computing diff between identical buffers
    // produces empty diff, and applying diff twice has no additional effect.
    //
    // Invariant: For any buffers A and B:
    //   apply(apply(A, diff(A,B)), diff(A,B)) == apply(A, diff(A,B))
    #[test]
    fn diff_is_idempotent() {
        proptest::proptest!(|(
            width in 5u16..60,
            height in 5u16..30,
            num_changes in 0usize..100,
        )| {
            let mut buf_a = Buffer::new(width, height);
            let mut buf_b = Buffer::new(width, height);

            // Make buf_b different from buf_a
            for i in 0..num_changes {
                let x = (i * 13 + 7) as u16 % width;
                let y = (i * 17 + 3) as u16 % height;
                buf_b.set_raw(x, y, Cell::from_char('X'));
            }

            // Compute diff from A to B
            let diff = BufferDiff::compute(&buf_a, &buf_b);

            // Apply diff to A once
            for (x, y) in diff.iter() {
                let cell = *buf_b.get_unchecked(x, y);
                buf_a.set_raw(x, y, cell);
            }

            // Now buf_a should equal buf_b
            let diff_after_first = BufferDiff::compute(&buf_a, &buf_b);
            prop_assert!(
                diff_after_first.is_empty(),
                "After applying diff once, buffers should be identical (diff was {} changes)",
                diff_after_first.len()
            );

            // Apply diff again (should be no-op since buffers are now equal)
            let before_second = buf_a.clone();
            for (x, y) in diff.iter() {
                let cell = *buf_b.get_unchecked(x, y);
                buf_a.set_raw(x, y, cell);
            }

            // Verify no change from second application
            let diff_after_second = BufferDiff::compute(&before_second, &buf_a);
            prop_assert!(
                diff_after_second.is_empty(),
                "Second diff application should be a no-op"
            );
        });
    }

    // ========== No-Ghosting After Clear Property (bd-1rz0.6) ==========

    // Property: After a full buffer clear (simulating resize), diffing
    // against a blank old buffer captures all content cells.
    //
    // This simulates the no-ghosting invariant: when terminal shrinks,
    // we present against a fresh blank buffer, ensuring no old content
    // persists. The key is that all non-blank cells in the new buffer
    // appear in the diff.
    //
    // Failure mode: If we diff against stale buffer state after resize,
    // some cells might be incorrectly marked as unchanged.
    #[test]
    fn no_ghosting_after_clear() {
        proptest::proptest!(|(
            width in 10u16..80,
            height in 5u16..30,
            num_content_cells in 1usize..200,
        )| {
            // Old buffer is blank (simulating post-resize cleared state)
            let old = Buffer::new(width, height);

            // New buffer has content (the UI to render)
            let mut new = Buffer::new(width, height);
            let mut expected_changes = std::collections::HashSet::new();

            for i in 0..num_content_cells {
                let x = (i * 13 + 7) as u16 % width;
                let y = (i * 17 + 3) as u16 % height;
                new.set_raw(x, y, Cell::from_char('#'));
                expected_changes.insert((x, y));
            }

            let diff = BufferDiff::compute(&old, &new);

            // Every non-blank cell should be in the diff
            // This ensures no "ghosting" - all visible content is explicitly rendered
            for (x, y) in expected_changes {
                let in_diff = diff.iter().any(|(dx, dy)| dx == x && dy == y);
                prop_assert!(
                    in_diff,
                    "Content cell at ({}, {}) missing from diff - would ghost",
                    x,
                    y
                );
            }

            // Also verify the diff doesn't include any extra cells
            for (x, y) in diff.iter() {
                let old_cell = old.get_unchecked(x, y);
                let new_cell = new.get_unchecked(x, y);
                prop_assert!(
                    !old_cell.bits_eq(new_cell),
                    "Diff includes unchanged cell at ({}, {})",
                    x,
                    y
                );
            }
        });
    }

    // ========== Monotonicity Property (bd-1rz0.6) ==========

    // Property: Diff changes are monotonically ordered (row-major).
    // This ensures deterministic iteration order for presentation.
    //
    // Invariant: For consecutive changes (x1,y1) and (x2,y2):
    //   y1 < y2 OR (y1 == y2 AND x1 < x2)
    #[test]
    fn diff_changes_are_monotonic() {
        proptest::proptest!(|(
            width in 10u16..80,
            height in 5u16..30,
            num_changes in 1usize..200,
        )| {
            let old = Buffer::new(width, height);
            let mut new = old.clone();

            // Apply changes in random positions
            for i in 0..num_changes {
                let x = (i * 37 + 11) as u16 % width;
                let y = (i * 53 + 7) as u16 % height;
                new.set_raw(x, y, Cell::from_char('M'));
            }

            let diff = BufferDiff::compute(&old, &new);
            let changes: Vec<_> = diff.iter().collect();

            // Verify monotonic ordering
            for window in changes.windows(2) {
                let (x1, y1) = window[0];
                let (x2, y2) = window[1];

                let is_monotonic = y1 < y2 || (y1 == y2 && x1 < x2);
                prop_assert!(
                    is_monotonic,
                    "Changes not monotonic: ({}, {}) should come before ({}, {})",
                    x1,
                    y1,
                    x2,
                    y2
                );
            }
        });
    }
}

#[cfg(test)]
mod span_edge_cases {
    use super::*;
    use crate::cell::Cell;
    use proptest::prelude::*;

    #[test]
    fn test_span_diff_u16_max_width() {
        // Test near u16::MAX limit (65535)
        let width = 65000;
        let height = 1;
        let old = Buffer::new(width, height);
        let mut new = Buffer::new(width, height);

        // We must clear dirty because Buffer::new starts with all rows dirty
        new.clear_dirty();

        // Set changes at start, middle, end
        new.set_raw(0, 0, Cell::from_char('A'));
        new.set_raw(32500, 0, Cell::from_char('B'));
        new.set_raw(64999, 0, Cell::from_char('C'));

        let full = BufferDiff::compute(&old, &new);
        let dirty = BufferDiff::compute_dirty(&old, &new);

        assert_eq!(full.changes(), dirty.changes());
        assert_eq!(full.len(), 3);

        // Verify changes are what we expect
        let changes = full.changes();
        assert!(changes.contains(&(0, 0)));
        assert!(changes.contains(&(32500, 0)));
        assert!(changes.contains(&(64999, 0)));
    }

    #[test]
    fn test_span_full_row_dirty_overflow() {
        let width = 1000;
        let height = 1;
        let old = Buffer::new(width, height);
        let mut new = Buffer::new(width, height);
        new.clear_dirty(); // All clean

        // Create > 64 spans to force overflow
        // DIRTY_SPAN_MAX_SPANS_PER_ROW is 64
        for i in 0..70 {
            let x = (i * 10) as u16;
            new.set_raw(x, 0, Cell::from_char('X'));
        }

        // Verify it overflowed
        let stats = new.dirty_span_stats();
        assert!(
            stats.rows_full_dirty > 0,
            "Should have overflowed to full row"
        );
        assert_eq!(
            stats.rows_with_spans, 0,
            "Should have cleared spans on overflow"
        );

        let full = BufferDiff::compute(&old, &new);
        let dirty = BufferDiff::compute_dirty(&old, &new);

        assert_eq!(full.changes(), dirty.changes());
        assert_eq!(full.len(), 70);
    }

    #[test]
    fn test_span_diff_empty_rows() {
        let width = 100;
        let height = 10;
        let old = Buffer::new(width, height);
        let mut new = Buffer::new(width, height);
        new.clear_dirty(); // All clean

        // No changes
        let dirty = BufferDiff::compute_dirty(&old, &new);
        assert!(dirty.is_empty());
    }

    proptest! {
        #[test]
        fn property_span_diff_equivalence_large(
            width in 1000u16..5000,
            height in 10u16..50,
            changes in proptest::collection::vec((0u16..5000, 0u16..50), 0..100)
        ) {
            // Cap width/height to avoid OOM in test runner
            let w = width.min(2000);
            let h = height.min(50);

            let old = Buffer::new(w, h);
            let mut new = Buffer::new(w, h);
            new.clear_dirty();

            // Apply changes
            for (raw_x, raw_y) in changes {
                let x = raw_x % w;
                let y = raw_y % h;
                new.set_raw(x, y, Cell::from_char('Z'));
            }

            let full = BufferDiff::compute(&old, &new);
            let dirty = BufferDiff::compute_dirty(&old, &new);

            prop_assert_eq!(
                full.changes(),
                dirty.changes(),
                "Large buffer mismatch: w={}, h={}, spans={:?}",
                w,
                h,
                new.dirty_span_stats()
            );
        }
    }
}

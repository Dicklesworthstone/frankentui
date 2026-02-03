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
//! 2. Iterate x from 0 to width
//! 3. Compare old[x,y] with new[x,y] using `bits_eq`
//! 4. Record position if different
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

use crate::buffer::Buffer;
use crate::cell::Cell;

// =============================================================================
// Block-based Row Scanning (autovec-friendly)
// =============================================================================

/// Block size for vectorized comparison (4 cells = 64 bytes).
/// Chosen to match common SIMD register width (256-bit / 512-bit).
const BLOCK_SIZE: usize = 4;

/// Scan a row pair for changed cells, appending positions to `changes`.
///
/// Processes cells in blocks of 4 for autovectorization. The compiler will
/// typically emit SIMD instructions (SSE2/AVX2/NEON) for the inner comparison
/// loops since each cell is 16 bytes (4 × u32) and we compare blocks of 4 cells
/// (64 bytes = one AVX-512 register or two AVX2 registers).
///
/// The block approach reduces branch misprediction overhead: we first compute
/// a change mask for the block, then only branch on non-zero masks.
#[inline]
fn scan_row_changes(old_row: &[Cell], new_row: &[Cell], y: u16, changes: &mut Vec<(u16, u16)>) {
    debug_assert_eq!(old_row.len(), new_row.len());
    let len = old_row.len();
    let blocks = len / BLOCK_SIZE;
    let remainder = len % BLOCK_SIZE;

    // Process full blocks
    for block_idx in 0..blocks {
        let base = block_idx * BLOCK_SIZE;
        let old_block = &old_row[base..base + BLOCK_SIZE];
        let new_block = &new_row[base..base + BLOCK_SIZE];

        // Compute change mask: 1 bit per changed cell in this block.
        // This tight loop is autovec-friendly: each iteration compares
        // four u32 fields and accumulates a boolean.
        let mut mask: u32 = 0;
        for i in 0..BLOCK_SIZE {
            if !old_block[i].bits_eq(&new_block[i]) {
                mask |= 1 << i;
            }
        }

        // Fast skip: if no cells changed in this block, move on.
        if mask == 0 {
            continue;
        }

        // Expand mask into change positions
        for i in 0..BLOCK_SIZE {
            if mask & (1 << i) != 0 {
                changes.push(((base + i) as u16, y));
            }
        }
    }

    // Process remainder cells
    let rem_base = blocks * BLOCK_SIZE;
    for i in 0..remainder {
        if !old_row[rem_base + i].bits_eq(&new_row[rem_base + i]) {
            changes.push(((rem_base + i) as u16, y));
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
}

impl BufferDiff {
    /// Create an empty diff.
    pub fn new() -> Self {
        Self {
            changes: Vec::new(),
        }
    }

    /// Create a diff with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            changes: Vec::with_capacity(capacity),
        }
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
        #[cfg(feature = "tracing")]
        let _span =
            tracing::debug_span!("diff_compute", width = old.width(), height = old.height());
        #[cfg(feature = "tracing")]
        let _guard = _span.enter();

        assert_eq!(old.width(), new.width(), "buffer widths must match");
        assert_eq!(old.height(), new.height(), "buffer heights must match");

        let width = old.width();
        let height = old.height();
        let w = width as usize;

        // Estimate capacity: assume ~5% of cells change on average
        let estimated_changes = (w * height as usize) / 20;
        let mut changes = Vec::with_capacity(estimated_changes);

        let old_cells = old.cells();
        let new_cells = new.cells();

        // Row-major scan with row-skip fast path
        for y in 0..height {
            let row_start = y as usize * w;
            let old_row = &old_cells[row_start..row_start + w];
            let new_row = &new_cells[row_start..row_start + w];

            // Fast path: skip entirely unchanged rows.
            // Cell derives PartialEq over four u32 fields, so slice
            // equality compiles to tight element-wise comparison that
            // LLVM can auto-vectorize for 16-byte aligned cells.
            if old_row == new_row {
                continue;
            }

            // Scan for changed cells using block-based comparison
            scan_row_changes(old_row, new_row, y, &mut changes);
        }

        #[cfg(feature = "tracing")]
        tracing::trace!(changes = changes.len(), "diff computed");

        Self { changes }
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
        assert_eq!(old.width(), new.width(), "buffer widths must match");
        assert_eq!(old.height(), new.height(), "buffer heights must match");

        let width = old.width();
        let height = old.height();
        let w = width as usize;

        let estimated_changes = (w * height as usize) / 20;
        let mut changes = Vec::with_capacity(estimated_changes);

        let old_cells = old.cells();
        let new_cells = new.cells();
        let dirty = new.dirty_rows();

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

            // Scan for changed cells using block-based comparison
            scan_row_changes(old_row, new_row, y, &mut changes);
        }

        Self { changes }
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
        let diff = BufferDiff {
            changes: vec![(u16::MAX, 0)],
        };

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
            let p50 = times_us[times_us.len() / 2];
            let p95 = times_us[(times_us.len() as f64 * 0.95) as usize];
            let p99 = times_us[(times_us.len() as f64 * 0.99) as usize];

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
}

#[cfg(test)]
mod proptests {
    use super::*;
    use crate::cell::Cell;
    use proptest::prelude::*;

    proptest! {
        /// Property: Applying diff changes to old buffer produces new buffer.
        ///
        /// This is the fundamental correctness property of the diff algorithm:
        /// for any pair of buffers, the diff captures all and only the changes.
        #[test]
        fn diff_apply_produces_target(
            width in 5u16..50,
            height in 5u16..30,
            num_changes in 0usize..200,
        ) {
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
                        "Mismatch at ({}, {})", x, y
                    );
                }
            }
        }

        /// Property: Diff is empty when buffers are identical.
        #[test]
        fn identical_buffers_empty_diff(
            width in 1u16..100,
            height in 1u16..50,
        ) {
            let buf = Buffer::new(width, height);
            let diff = BufferDiff::compute(&buf, &buf);
            prop_assert!(diff.is_empty(), "Identical buffers should have empty diff");
        }

        /// Property: Every change in diff corresponds to an actual difference.
        #[test]
        fn diff_contains_only_real_changes(
            width in 5u16..50,
            height in 5u16..30,
            num_changes in 0usize..100,
        ) {
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
                    "Diff includes unchanged cell at ({}, {})", x, y
                );
            }
        }

        /// Property: Runs correctly coalesce adjacent changes.
        #[test]
        fn runs_are_contiguous(
            width in 10u16..80,
            height in 5u16..30,
        ) {
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
                        "Run includes unchanged cell at ({}, {})", x, run.y
                    );
                }
            }
        }

        /// Property: Runs cover all changes exactly once.
        #[test]
        fn runs_cover_all_changes(
            width in 10u16..60,
            height in 5u16..30,
            num_changes in 1usize..100,
        ) {
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
            let mut run_cells: std::collections::HashSet<(u16, u16)> = std::collections::HashSet::new();
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
                    "Change at ({}, {}) not covered by runs", x, y
                );
            }

            prop_assert_eq!(
                run_cells.len(),
                diff.len(),
                "Run cell count should match diff change count"
            );
        }

        /// Property (bd-4kq0.1.2): Block-based scan matches scalar scan
        /// for random row widths and change patterns. This verifies the
        /// block/remainder handling is correct for all alignment cases.
        #[test]
        fn block_scan_matches_scalar(
            width in 1u16..200,
            height in 1u16..20,
            num_changes in 0usize..200,
        ) {
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
        }


        // ========== Diff Equivalence: dirty+block vs full scan (bd-4kq0.1.3) ==========

        /// Property: compute_dirty with all rows dirty matches compute exactly.
        /// This verifies the block-scan + dirty-row path is semantically
        /// equivalent to the full scan for random buffers.
        #[test]
        fn property_diff_equivalence(
            width in 1u16..120,
            height in 1u16..40,
            num_changes in 0usize..300,
        ) {
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
                width, height, num_changes
            );

            // Also verify run coalescing is identical
            let full_runs = full.runs();
            let dirty_runs = dirty.runs();
            prop_assert_eq!(
                full_runs.len(),
                dirty_runs.len(),
                "run count must match"
            );
            for (fr, dr) in full_runs.iter().zip(dirty_runs.iter()) {
                prop_assert_eq!(fr, dr, "run mismatch");
            }
        }

        // ========== Idempotence Property (bd-1rz0.6) ==========

        /// Property: Diff is idempotent - computing diff between identical buffers
        /// produces empty diff, and applying diff twice has no additional effect.
        ///
        /// Invariant: For any buffers A and B:
        ///   apply(apply(A, diff(A,B)), diff(A,B)) == apply(A, diff(A,B))
        #[test]
        fn diff_is_idempotent(
            width in 5u16..60,
            height in 5u16..30,
            num_changes in 0usize..100,
        ) {
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
        }

        // ========== No-Ghosting After Clear Property (bd-1rz0.6) ==========

        /// Property: After a full buffer clear (simulating resize), diffing
        /// against a blank old buffer captures all content cells.
        ///
        /// This simulates the no-ghosting invariant: when terminal shrinks,
        /// we present against a fresh blank buffer, ensuring no old content
        /// persists. The key is that all non-blank cells in the new buffer
        /// appear in the diff.
        ///
        /// Failure mode: If we diff against stale buffer state after resize,
        /// some cells might be incorrectly marked as unchanged.
        #[test]
        fn no_ghosting_after_clear(
            width in 10u16..80,
            height in 5u16..30,
            num_content_cells in 1usize..200,
        ) {
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
                    "Content cell at ({}, {}) missing from diff - would ghost", x, y
                );
            }

            // Also verify the diff doesn't include any extra cells
            for (x, y) in diff.iter() {
                let old_cell = old.get_unchecked(x, y);
                let new_cell = new.get_unchecked(x, y);
                prop_assert!(
                    !old_cell.bits_eq(new_cell),
                    "Diff includes unchanged cell at ({}, {})", x, y
                );
            }
        }

        // ========== Monotonicity Property (bd-1rz0.6) ==========

        /// Property: Diff changes are monotonically ordered (row-major).
        /// This ensures deterministic iteration order for presentation.
        ///
        /// Invariant: For consecutive changes (x1,y1) and (x2,y2):
        ///   y1 < y2 OR (y1 == y2 AND x1 < x2)
        #[test]
        fn diff_changes_are_monotonic(
            width in 10u16..80,
            height in 5u16..30,
            num_changes in 1usize..200,
        ) {
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
                    x1, y1, x2, y2
                );
            }
        }
    }

    // ========== Dirty-Aware Diff Tests (bd-4kq0.1.1) ==========

    #[test]
    fn dirty_diff_matches_full_diff() {
        let old = Buffer::new(10, 10);
        let mut new = Buffer::new(10, 10);
        new.set_raw(3, 2, Cell::from_char('A'));
        new.set_raw(7, 5, Cell::from_char('B'));
        new.set_raw(0, 9, Cell::from_char('C'));

        let full = BufferDiff::compute(&old, &new);
        let dirty = BufferDiff::compute_dirty(&old, &new);
        assert_eq!(full.changes(), dirty.changes());
    }

    #[test]
    fn dirty_diff_skips_clean_rows() {
        let old = Buffer::new(10, 10);
        let mut new = Buffer::new(10, 10);
        // Write to row 3 then clear dirty, write to row 7
        new.set_raw(0, 3, Cell::from_char('X'));
        new.clear_dirty();
        new.set_raw(0, 7, Cell::from_char('Y'));

        // Full diff sees both changes
        let full = BufferDiff::compute(&old, &new);
        assert_eq!(full.len(), 2);

        // Dirty diff only sees row 7 (row 3's dirty flag was cleared)
        let dirty = BufferDiff::compute_dirty(&old, &new);
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty.changes(), &[(0, 7)]);
    }

    #[test]
    fn dirty_diff_empty_when_all_clean() {
        let old = Buffer::new(10, 10);
        let mut new = Buffer::new(10, 10);
        new.set_raw(5, 5, Cell::from_char('Z'));
        new.clear_dirty(); // Clear all dirty flags

        let dirty = BufferDiff::compute_dirty(&old, &new);
        assert!(
            dirty.is_empty(),
            "should produce no changes when all rows are clean"
        );
    }

    #[test]
    fn dirty_diff_false_positive_is_safe() {
        // A row marked dirty but actually identical should still produce correct diff
        let mut old = Buffer::new(10, 5);
        old.set_raw(0, 2, Cell::from_char('A'));

        let mut new = Buffer::new(10, 5);
        new.set_raw(0, 2, Cell::from_char('A')); // Same content, but row is dirty

        let dirty = BufferDiff::compute_dirty(&old, &new);
        assert!(
            dirty.is_empty(),
            "identical content should produce empty diff even if dirty"
        );
    }
}

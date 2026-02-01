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

        debug_assert_eq!(old.width(), new.width(), "buffer widths must match");
        debug_assert_eq!(old.height(), new.height(), "buffer heights must match");

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

            // Scan for changed cells within this row
            for (x, (old_cell, new_cell)) in old_row.iter().zip(new_row.iter()).enumerate() {
                if !old_cell.bits_eq(new_cell) {
                    changes.push((x as u16, y));
                }
            }
        }

        #[cfg(feature = "tracing")]
        tracing::trace!(changes = changes.len(), "diff computed");

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

        let mut runs = Vec::new();
        let mut i = 0;

        while i < sorted.len() {
            let (x0, y) = sorted[i];
            let mut x1 = x0;
            i += 1;

            // Coalesce consecutive x positions on the same row
            while i < sorted.len() {
                let (x, yy) = sorted[i];
                if yy != y || x != x1 + 1 {
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
    }
}

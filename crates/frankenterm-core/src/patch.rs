//! Incremental patch API: dirty tracking, grid diff, and change runs.
//!
//! The patch module provides an efficient change representation so renderers
//! (native ANSI presenter, WebGPU, trace replayer) can update incrementally.
//!
//! # Architecture
//!
//! ```text
//! Grid mutations → DirtyTracker (row + span hints)
//!                       ↓
//! GridDiff::diff_dirty(old, new, tracker) → Patch
//!                       ↓
//! Patch::runs() → Vec<ChangeRun> (for cursor-based output)
//! Patch::updates  → Vec<CellUpdate> (for instance-based GPU upload)
//! ```
//!
//! # Ordering guarantee
//!
//! All outputs are in **stable row-major order**: row ascending, then column
//! ascending within each row. This is deterministic for identical inputs.

use crate::cell::Cell;
use crate::grid::Grid;

// ── Dirty span ───────────────────────────────────────────────────────

/// A half-open column range `[start, end)` marking dirty cells in a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtySpan {
    /// Start column (inclusive).
    pub start: u16,
    /// End column (exclusive).
    pub end: u16,
}

impl DirtySpan {
    /// Create a new dirty span.
    pub fn new(start: u16, end: u16) -> Self {
        Self { start, end }
    }

    /// Number of columns in this span.
    #[inline]
    pub fn len(&self) -> u16 {
        self.end.saturating_sub(self.start)
    }

    /// Whether this span is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// Whether two spans overlap or are adjacent (gap <= merge_gap).
    fn mergeable(&self, other: &Self, merge_gap: u16) -> bool {
        // Saturating math avoids overflow if callers pass large spans/gaps.
        let other_end = other.end.saturating_add(merge_gap);
        let self_end = self.end.saturating_add(merge_gap);
        !(self.start > other_end || other.start > self_end)
    }

    /// Merge another span into this one (union).
    fn merge(&mut self, other: &Self) {
        self.start = self.start.min(other.start);
        self.end = self.end.max(other.end);
    }
}

// ── Dirty row state ──────────────────────────────────────────────────

/// Per-row dirty state: spans of modified columns.
#[derive(Debug, Clone)]
struct DirtyRow {
    /// Whether the entire row is dirty (optimization: skip span check).
    full: bool,
    /// Sorted, non-overlapping dirty spans. Empty if `full` is true.
    spans: Vec<DirtySpan>,
}

impl DirtyRow {
    fn new() -> Self {
        Self {
            full: false,
            spans: Vec::new(),
        }
    }

    fn is_dirty(&self) -> bool {
        self.full || !self.spans.is_empty()
    }

    fn mark_full(&mut self) {
        self.full = true;
        self.spans.clear();
    }

    fn mark_span(&mut self, start: u16, end: u16, merge_gap: u16) {
        if self.full {
            return;
        }
        let new_span = DirtySpan::new(start, end);
        // Try to merge with existing spans.
        let mut merged = false;
        for span in &mut self.spans {
            if span.mergeable(&new_span, merge_gap) {
                span.merge(&new_span);
                merged = true;
                break;
            }
        }
        if !merged {
            self.spans.push(new_span);
        }
        // Re-sort and coalesce after insertion.
        self.coalesce(merge_gap);
    }

    fn coalesce(&mut self, merge_gap: u16) {
        if self.spans.len() < 2 {
            return;
        }
        self.spans.sort_by_key(|s| s.start);
        let mut write = 0;
        for read in 1..self.spans.len() {
            if self.spans[write].mergeable(&self.spans[read], merge_gap) {
                let other = self.spans[read];
                self.spans[write].merge(&other);
            } else {
                write += 1;
                self.spans[write] = self.spans[read];
            }
        }
        self.spans.truncate(write + 1);
    }

    fn clear(&mut self) {
        self.full = false;
        self.spans.clear();
    }
}

// ── Dirty tracker ────────────────────────────────────────────────────

/// Tracks which cells have been modified since the last frame.
///
/// Used by `GridDiff::diff_dirty` to skip unchanged rows and focus only on
/// dirty spans within changed rows, achieving sub-linear diff cost for
/// typical workloads (1-5 rows changed per frame).
///
/// # Merge gap
///
/// When two dirty spans are separated by `merge_gap` or fewer clean cells,
/// they are merged into a single span. This reduces overhead from many
/// tiny spans (e.g., character-by-character typing). Default: 1.
#[derive(Debug, Clone)]
pub struct DirtyTracker {
    rows: Vec<DirtyRow>,
    cols: u16,
    merge_gap: u16,
    /// Number of dirty rows (cached for fast "any dirty?" checks).
    dirty_count: usize,
}

impl DirtyTracker {
    /// Create a new tracker for a grid of the given dimensions.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            rows: (0..rows).map(|_| DirtyRow::new()).collect(),
            cols,
            merge_gap: 1,
            dirty_count: 0,
        }
    }

    /// Number of rows tracked.
    pub fn row_count(&self) -> u16 {
        self.rows.len() as u16
    }

    /// Set the merge gap for span coalescing.
    pub fn set_merge_gap(&mut self, gap: u16) {
        self.merge_gap = gap;
    }

    /// Whether any cell is dirty.
    pub fn is_dirty(&self) -> bool {
        self.dirty_count > 0
    }

    /// Number of dirty rows.
    pub fn dirty_row_count(&self) -> usize {
        self.dirty_count
    }

    /// Mark a single cell as dirty.
    pub fn mark_cell(&mut self, row: u16, col: u16) {
        if col >= self.cols {
            return;
        }
        if let Some(dr) = self.rows.get_mut(row as usize) {
            let was_dirty = dr.is_dirty();
            let end = col.saturating_add(1).min(self.cols);
            dr.mark_span(col, end, self.merge_gap);
            if !was_dirty && dr.is_dirty() {
                self.dirty_count += 1;
            }
        }
    }

    /// Mark a horizontal range `[start_col, end_col)` as dirty.
    pub fn mark_span(&mut self, row: u16, start_col: u16, end_col: u16) {
        if start_col >= self.cols || end_col <= start_col {
            return;
        }
        let end = end_col.min(self.cols);
        if end <= start_col {
            return;
        }
        if let Some(dr) = self.rows.get_mut(row as usize) {
            let was_dirty = dr.is_dirty();
            dr.mark_span(start_col, end, self.merge_gap);
            if !was_dirty && dr.is_dirty() {
                self.dirty_count += 1;
            }
        }
    }

    /// Mark an entire row as dirty.
    pub fn mark_row(&mut self, row: u16) {
        if let Some(dr) = self.rows.get_mut(row as usize) {
            let was_dirty = dr.is_dirty();
            dr.mark_full();
            if !was_dirty {
                self.dirty_count += 1;
            }
        }
    }

    /// Mark all rows as dirty (forces full redraw).
    pub fn mark_all(&mut self) {
        for dr in &mut self.rows {
            dr.mark_full();
        }
        self.dirty_count = self.rows.len();
    }

    /// Clear all dirty state for the next frame.
    pub fn clear(&mut self) {
        for dr in &mut self.rows {
            dr.clear();
        }
        self.dirty_count = 0;
    }

    /// Whether a specific row is dirty.
    pub fn is_row_dirty(&self, row: u16) -> bool {
        self.rows.get(row as usize).is_some_and(|dr| dr.is_dirty())
    }

    /// Get dirty spans for a row.
    ///
    /// Returns `None` if the entire row is dirty (caller should scan all columns).
    /// Returns `Some(&[DirtySpan])` for partial dirty rows.
    /// Returns `Some(&[])` for clean rows.
    pub fn row_spans(&self, row: u16) -> Option<&[DirtySpan]> {
        self.rows.get(row as usize).and_then(|dr| {
            if dr.full {
                None // entire row dirty — scan all columns
            } else {
                Some(dr.spans.as_slice())
            }
        })
    }

    /// Resize the tracker for a new grid size.
    pub fn resize(&mut self, new_cols: u16, new_rows: u16) {
        self.cols = new_cols;
        self.rows.resize_with(new_rows as usize, DirtyRow::new);
        // Mark everything dirty after resize.
        self.mark_all();
    }
}

// ── Change run ───────────────────────────────────────────────────────

/// A contiguous horizontal span of changed cells on a single row.
///
/// Used by native presenters for efficient cursor positioning:
/// instead of per-cell cursor moves, emit runs and minimize ANSI overhead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRun {
    /// Row index.
    pub row: u16,
    /// Start column (inclusive).
    pub start_col: u16,
    /// End column (exclusive).
    pub end_col: u16,
}

impl ChangeRun {
    /// Number of cells in this run.
    #[inline]
    pub fn len(&self) -> u16 {
        self.end_col.saturating_sub(self.start_col)
    }

    /// Whether this run is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.end_col <= self.start_col
    }
}

// ── Cell update ──────────────────────────────────────────────────────

/// A single cell update at (row, col).
///
/// Used by GPU renderers for per-instance upload and by trace replayers
/// for exact state reconstruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellUpdate {
    pub row: u16,
    pub col: u16,
    pub cell: Cell,
}

// ── Patch ────────────────────────────────────────────────────────────

/// A minimal set of changes between two grid states.
///
/// Updates are stored in **row-major order** (row ascending, column ascending).
/// This ordering is stable and deterministic for identical input grids.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Patch {
    /// Grid width at the time the patch was computed.
    pub cols: u16,
    /// Grid height at the time the patch was computed.
    pub rows: u16,
    /// Individual cell updates in row-major order.
    pub updates: Vec<CellUpdate>,
}

impl Patch {
    /// Create an empty patch for a given grid shape.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            updates: Vec::new(),
        }
    }

    /// Whether the patch has no updates.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty()
    }

    /// Number of changed cells.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.updates.len()
    }

    /// Append a single cell update.
    pub fn push(&mut self, row: u16, col: u16, cell: Cell) {
        self.updates.push(CellUpdate { row, col, cell });
    }

    /// Coalesce updates into contiguous horizontal runs.
    ///
    /// Runs are in row-major order. Adjacent columns on the same row are
    /// merged into a single run.
    #[must_use]
    pub fn runs(&self) -> Vec<ChangeRun> {
        let mut runs = Vec::new();
        self.runs_into(&mut runs);
        runs
    }

    /// Coalesce updates into runs, appending to the provided buffer.
    ///
    /// Reuse the buffer across frames to avoid allocation.
    pub fn runs_into(&self, out: &mut Vec<ChangeRun>) {
        out.clear();
        if self.updates.is_empty() {
            return;
        }

        let mut current_row = self.updates[0].row;
        let mut start_col = self.updates[0].col;
        let mut end_col = self.updates[0].col.saturating_add(1);

        for update in &self.updates[1..] {
            if update.row == current_row && update.col == end_col {
                // Extend current run.
                end_col = update.col.saturating_add(1);
            } else {
                // Emit current run.
                out.push(ChangeRun {
                    row: current_row,
                    start_col,
                    end_col,
                });
                current_row = update.row;
                start_col = update.col;
                end_col = update.col.saturating_add(1);
            }
        }
        // Emit final run.
        out.push(ChangeRun {
            row: current_row,
            start_col,
            end_col,
        });
    }

    /// Ratio of changed cells to total grid cells.
    ///
    /// Returns 0.0 for empty grids.
    #[must_use]
    pub fn density(&self) -> f64 {
        let total = self.cols as usize * self.rows as usize;
        if total == 0 {
            return 0.0;
        }
        self.updates.len() as f64 / total as f64
    }
}

// ── Grid diff ────────────────────────────────────────────────────────

/// Compute diffs between two grids.
pub struct GridDiff;

impl GridDiff {
    /// Full diff: compare every cell between `old` and `new`.
    ///
    /// O(rows * cols). Use `diff_dirty` for typical sub-linear performance.
    pub fn diff(old: &Grid, new: &Grid) -> Patch {
        let cols = new.cols();
        let rows = new.rows();
        let mut patch = Patch::new(cols, rows);

        for r in 0..rows {
            for c in 0..cols {
                let old_cell = old.cell(r, c);
                let new_cell = new.cell(r, c);
                match (old_cell, new_cell) {
                    (Some(o), Some(n)) if o != n => {
                        patch.push(r, c, *n);
                    }
                    (None, Some(n)) => {
                        patch.push(r, c, *n);
                    }
                    _ => {}
                }
            }
        }

        patch
    }

    /// Dirty-hinted diff: only compare cells in dirty rows/spans.
    ///
    /// Skips clean rows entirely and within dirty rows only checks the
    /// indicated spans. This achieves sub-linear performance for typical
    /// workloads where only 1-5 rows change per frame.
    ///
    /// **Soundness**: the caller must ensure the `DirtyTracker` has been
    /// correctly maintained (all modified cells are marked dirty). If a cell
    /// was modified but not marked, the change will be missed.
    pub fn diff_dirty(old: &Grid, new: &Grid, tracker: &DirtyTracker) -> Patch {
        let cols = new.cols();
        let rows = new.rows();
        let mut patch = Patch::new(cols, rows);

        if !tracker.is_dirty() {
            return patch;
        }

        for r in 0..rows {
            if !tracker.is_row_dirty(r) {
                continue;
            }

            match tracker.row_spans(r) {
                None => {
                    // Entire row is dirty — scan all columns.
                    for c in 0..cols {
                        let old_cell = old.cell(r, c);
                        let new_cell = new.cell(r, c);
                        match (old_cell, new_cell) {
                            (Some(o), Some(n)) if o != n => {
                                patch.push(r, c, *n);
                            }
                            (None, Some(n)) => {
                                patch.push(r, c, *n);
                            }
                            _ => {}
                        }
                    }
                }
                Some(spans) => {
                    // Only check dirty spans.
                    for span in spans {
                        let start = span.start.min(cols);
                        let end = span.end.min(cols);
                        for c in start..end {
                            let old_cell = old.cell(r, c);
                            let new_cell = new.cell(r, c);
                            match (old_cell, new_cell) {
                                (Some(o), Some(n)) if o != n => {
                                    patch.push(r, c, *n);
                                }
                                (None, Some(n)) => {
                                    patch.push(r, c, *n);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        patch
    }

    /// Quick full-diff into a reusable patch (avoids allocation).
    pub fn diff_into(old: &Grid, new: &Grid, patch: &mut Patch) {
        patch.cols = new.cols();
        patch.rows = new.rows();
        patch.updates.clear();

        let cols = new.cols();
        let rows = new.rows();

        for r in 0..rows {
            for c in 0..cols {
                let old_cell = old.cell(r, c);
                let new_cell = new.cell(r, c);
                match (old_cell, new_cell) {
                    (Some(o), Some(n)) if o != n => {
                        patch.push(r, c, *n);
                    }
                    (None, Some(n)) => {
                        patch.push(r, c, *n);
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // ── Patch basics ─────────────────────────────────────────────────

    #[test]
    fn empty_patch_is_empty() {
        let p = Patch::new(80, 24);
        assert!(p.is_empty());
        assert_eq!(p.len(), 0);
        assert_eq!(p.cols, 80);
        assert_eq!(p.rows, 24);
    }

    #[test]
    fn push_adds_update() {
        let mut p = Patch::new(2, 2);
        p.push(0, 1, Cell::new('X'));
        assert_eq!(p.len(), 1);
        assert_eq!(p.updates[0].row, 0);
        assert_eq!(p.updates[0].col, 1);
        assert_eq!(p.updates[0].cell.content(), 'X');
    }

    #[test]
    fn density_calculation() {
        let mut p = Patch::new(10, 10);
        for i in 0..25u16 {
            p.push(i / 10, i % 10, Cell::new('X'));
        }
        let d = p.density();
        assert!((d - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn density_empty_grid() {
        let p = Patch::new(0, 0);
        assert_eq!(p.density(), 0.0);
    }

    // ── Run coalescing ───────────────────────────────────────────────

    #[test]
    fn runs_empty_patch() {
        let p = Patch::new(10, 5);
        assert!(p.runs().is_empty());
    }

    #[test]
    fn runs_single_cell() {
        let mut p = Patch::new(10, 5);
        p.push(2, 5, Cell::new('A'));
        let runs = p.runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].row, 2);
        assert_eq!(runs[0].start_col, 5);
        assert_eq!(runs[0].end_col, 6);
        assert_eq!(runs[0].len(), 1);
    }

    #[test]
    fn runs_coalesces_adjacent_cells() {
        let mut p = Patch::new(10, 5);
        p.push(1, 3, Cell::new('A'));
        p.push(1, 4, Cell::new('B'));
        p.push(1, 5, Cell::new('C'));
        let runs = p.runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].row, 1);
        assert_eq!(runs[0].start_col, 3);
        assert_eq!(runs[0].end_col, 6);
        assert_eq!(runs[0].len(), 3);
    }

    #[test]
    fn runs_gap_creates_separate_runs() {
        let mut p = Patch::new(10, 5);
        p.push(0, 1, Cell::new('A'));
        p.push(0, 5, Cell::new('B'));
        let runs = p.runs();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].start_col, 1);
        assert_eq!(runs[0].end_col, 2);
        assert_eq!(runs[1].start_col, 5);
        assert_eq!(runs[1].end_col, 6);
    }

    #[test]
    fn runs_different_rows() {
        let mut p = Patch::new(10, 5);
        p.push(0, 0, Cell::new('A'));
        p.push(0, 1, Cell::new('B'));
        p.push(2, 3, Cell::new('C'));
        let runs = p.runs();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].row, 0);
        assert_eq!(runs[0].start_col, 0);
        assert_eq!(runs[0].end_col, 2);
        assert_eq!(runs[1].row, 2);
        assert_eq!(runs[1].start_col, 3);
        assert_eq!(runs[1].end_col, 4);
    }

    #[test]
    fn runs_into_reuses_buffer() {
        let mut p = Patch::new(10, 5);
        p.push(0, 0, Cell::new('A'));
        p.push(0, 1, Cell::new('B'));

        let mut buf = Vec::new();
        p.runs_into(&mut buf);
        assert_eq!(buf.len(), 1);
        // Reuse same buffer.
        p.runs_into(&mut buf);
        assert_eq!(buf.len(), 1); // cleared and refilled
    }

    // ── DirtyTracker ─────────────────────────────────────────────────

    #[test]
    fn tracker_new_is_clean() {
        let t = DirtyTracker::new(80, 24);
        assert!(!t.is_dirty());
        assert_eq!(t.dirty_row_count(), 0);
        assert_eq!(t.row_count(), 24);
    }

    #[test]
    fn tracker_mark_cell() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_cell(5, 10);
        assert!(t.is_dirty());
        assert!(t.is_row_dirty(5));
        assert!(!t.is_row_dirty(4));
        assert_eq!(t.dirty_row_count(), 1);

        let spans = t.row_spans(5).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 10);
        assert_eq!(spans[0].end, 11);
    }

    #[test]
    fn tracker_mark_span() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_span(3, 5, 15);
        let spans = t.row_spans(3).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 5);
        assert_eq!(spans[0].end, 15);
    }

    #[test]
    fn tracker_mark_row() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_row(10);
        assert!(t.is_row_dirty(10));
        // Full row → row_spans returns None.
        assert!(t.row_spans(10).is_none());
    }

    #[test]
    fn tracker_mark_all() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_all();
        assert_eq!(t.dirty_row_count(), 24);
        for r in 0..24 {
            assert!(t.is_row_dirty(r));
        }
    }

    #[test]
    fn tracker_clear() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_all();
        t.clear();
        assert!(!t.is_dirty());
        assert_eq!(t.dirty_row_count(), 0);
    }

    #[test]
    fn tracker_adjacent_spans_merge() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_span(0, 5, 10);
        t.mark_span(0, 10, 15); // adjacent to previous
        let spans = t.row_spans(0).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 5);
        assert_eq!(spans[0].end, 15);
    }

    #[test]
    fn tracker_gap_within_merge_gap() {
        let mut t = DirtyTracker::new(80, 24);
        t.set_merge_gap(2);
        t.mark_span(0, 5, 8);
        t.mark_span(0, 10, 15); // gap of 2 → should merge
        let spans = t.row_spans(0).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 5);
        assert_eq!(spans[0].end, 15);
    }

    #[test]
    fn tracker_gap_beyond_merge_gap() {
        let mut t = DirtyTracker::new(80, 24);
        t.set_merge_gap(1);
        t.mark_span(0, 5, 8);
        t.mark_span(0, 20, 25); // gap of 12 → separate
        let spans = t.row_spans(0).unwrap();
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn tracker_out_of_bounds_ignored() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_cell(99, 99);
        assert!(!t.is_dirty());
    }

    #[test]
    fn tracker_out_of_bounds_col_ignored() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_cell(0, u16::MAX);
        t.mark_cell(0, 80); // col == cols
        t.mark_span(0, 80, 81); // start == cols
        t.mark_span(0, 10, 10); // empty span
        assert!(!t.is_dirty());
    }

    #[test]
    fn tracker_resize_marks_all_dirty() {
        let mut t = DirtyTracker::new(80, 24);
        t.resize(120, 40);
        assert_eq!(t.row_count(), 40);
        assert_eq!(t.dirty_row_count(), 40);
    }

    // ── GridDiff ─────────────────────────────────────────────────────

    #[test]
    fn diff_identical_grids_empty_patch() {
        let a = Grid::new(10, 5);
        let b = Grid::new(10, 5);
        let patch = GridDiff::diff(&a, &b);
        assert!(patch.is_empty());
    }

    #[test]
    fn diff_detects_single_change() {
        let a = Grid::new(10, 5);
        let mut b = Grid::new(10, 5);
        b.cell_mut(2, 3).unwrap().set_content('X', 1);
        let patch = GridDiff::diff(&a, &b);
        assert_eq!(patch.len(), 1);
        assert_eq!(patch.updates[0].row, 2);
        assert_eq!(patch.updates[0].col, 3);
        assert_eq!(patch.updates[0].cell.content(), 'X');
    }

    #[test]
    fn diff_detects_multiple_changes() {
        let a = Grid::new(5, 3);
        let mut b = Grid::new(5, 3);
        b.cell_mut(0, 0).unwrap().set_content('A', 1);
        b.cell_mut(1, 2).unwrap().set_content('B', 1);
        b.cell_mut(2, 4).unwrap().set_content('C', 1);
        let patch = GridDiff::diff(&a, &b);
        assert_eq!(patch.len(), 3);
        // Row-major order.
        assert_eq!(patch.updates[0].row, 0);
        assert_eq!(patch.updates[1].row, 1);
        assert_eq!(patch.updates[2].row, 2);
    }

    #[test]
    fn diff_into_reuses_patch() {
        let a = Grid::new(5, 3);
        let mut b = Grid::new(5, 3);
        b.cell_mut(1, 1).unwrap().set_content('Z', 1);

        let mut patch = Patch::new(0, 0);
        GridDiff::diff_into(&a, &b, &mut patch);
        assert_eq!(patch.len(), 1);
        assert_eq!(patch.cols, 5);
        assert_eq!(patch.rows, 3);
    }

    #[test]
    fn diff_dirty_skips_clean_rows() {
        let a = Grid::new(10, 5);
        let mut b = Grid::new(10, 5);
        // Change cells on rows 1 and 3.
        b.cell_mut(1, 0).unwrap().set_content('A', 1);
        b.cell_mut(3, 5).unwrap().set_content('B', 1);

        // Only mark row 1 as dirty (not row 3).
        let mut tracker = DirtyTracker::new(10, 5);
        tracker.mark_row(1);

        let patch = GridDiff::diff_dirty(&a, &b, &tracker);
        // Should only find the change on row 1.
        assert_eq!(patch.len(), 1);
        assert_eq!(patch.updates[0].row, 1);
        assert_eq!(patch.updates[0].col, 0);
    }

    #[test]
    fn diff_dirty_uses_spans() {
        let a = Grid::new(10, 5);
        let mut b = Grid::new(10, 5);
        // Change at col 2 and col 8.
        b.cell_mut(0, 2).unwrap().set_content('A', 1);
        b.cell_mut(0, 8).unwrap().set_content('B', 1);

        let mut tracker = DirtyTracker::new(10, 5);
        // Only mark span [0, 5) — col 8 is outside.
        tracker.mark_span(0, 0, 5);

        let patch = GridDiff::diff_dirty(&a, &b, &tracker);
        // Only col 2 should be found (col 8 is outside the dirty span).
        assert_eq!(patch.len(), 1);
        assert_eq!(patch.updates[0].col, 2);
    }

    #[test]
    fn diff_dirty_clean_tracker_empty_patch() {
        let a = Grid::new(10, 5);
        let mut b = Grid::new(10, 5);
        b.cell_mut(0, 0).unwrap().set_content('X', 1);

        let tracker = DirtyTracker::new(10, 5); // no dirty marks
        let patch = GridDiff::diff_dirty(&a, &b, &tracker);
        assert!(patch.is_empty());
    }

    #[test]
    fn diff_detects_attribute_changes() {
        let a = Grid::new(5, 1);
        let mut b = Grid::new(5, 1);
        // Same content, different attrs.
        let cell = b.cell_mut(0, 2).unwrap();
        cell.attrs.flags = crate::cell::SgrFlags::BOLD;

        let patch = GridDiff::diff(&a, &b);
        assert_eq!(patch.len(), 1);
        assert_eq!(patch.updates[0].col, 2);
    }

    #[test]
    fn diff_row_major_order_guaranteed() {
        let a = Grid::new(3, 3);
        let mut b = Grid::new(3, 3);
        // Add changes in reverse order.
        b.cell_mut(2, 2).unwrap().set_content('Z', 1);
        b.cell_mut(1, 1).unwrap().set_content('Y', 1);
        b.cell_mut(0, 0).unwrap().set_content('X', 1);

        let patch = GridDiff::diff(&a, &b);
        assert_eq!(patch.len(), 3);
        // Must be row-major.
        assert!(patch.updates[0].row <= patch.updates[1].row);
        assert!(patch.updates[1].row <= patch.updates[2].row);
    }

    // ── DirtySpan ────────────────────────────────────────────────────

    #[test]
    fn dirty_span_len() {
        let span = DirtySpan::new(5, 10);
        assert_eq!(span.len(), 5);
        assert!(!span.is_empty());

        let empty = DirtySpan::new(5, 5);
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
    }

    // ── ChangeRun ────────────────────────────────────────────────────

    #[test]
    fn change_run_len() {
        let run = ChangeRun {
            row: 0,
            start_col: 3,
            end_col: 7,
        };
        assert_eq!(run.len(), 4);
        assert!(!run.is_empty());
    }

    // ── Integration: full pipeline ───────────────────────────────────

    #[test]
    fn pipeline_track_diff_coalesce() {
        // Simulate: build new grid, track dirty, diff, coalesce to runs.
        let old = Grid::new(10, 3);
        let mut new = Grid::new(10, 3);
        let mut tracker = DirtyTracker::new(10, 3);

        // Simulate typing "hello" at row 1, cols 0-4.
        for (i, ch) in "hello".chars().enumerate() {
            new.cell_mut(1, i as u16).unwrap().set_content(ch, 1);
            tracker.mark_cell(1, i as u16);
        }

        let patch = GridDiff::diff_dirty(&old, &new, &tracker);
        assert_eq!(patch.len(), 5);

        let runs = patch.runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].row, 1);
        assert_eq!(runs[0].start_col, 0);
        assert_eq!(runs[0].end_col, 5);
        assert_eq!(runs[0].len(), 5);
    }

    #[test]
    fn pipeline_sparse_changes() {
        let old = Grid::new(80, 24);
        let mut new = Grid::new(80, 24);
        let mut tracker = DirtyTracker::new(80, 24);

        // Simulate a status bar update at row 23 and cursor at row 5.
        new.cell_mut(5, 10).unwrap().set_content('>', 1);
        tracker.mark_cell(5, 10);

        for c in 0..80u16 {
            new.cell_mut(23, c).unwrap().set_content('-', 1);
        }
        tracker.mark_row(23);

        let patch = GridDiff::diff_dirty(&old, &new, &tracker);
        assert_eq!(patch.len(), 81); // 1 cursor + 80 status bar

        let runs = patch.runs();
        assert_eq!(runs.len(), 2); // one run on row 5, one on row 23
        assert_eq!(runs[0].row, 5);
        assert_eq!(runs[0].len(), 1);
        assert_eq!(runs[1].row, 23);
        assert_eq!(runs[1].len(), 80);

        // Density should be small (81 / 1920 ≈ 4.2%).
        assert!(patch.density() < 0.05);
    }

    // ── Edge-case tests (bd-2iaas) ──────────────────────────────────

    // ── DirtySpan edge cases ────────────────────────────────────────

    #[test]
    fn dirty_span_reversed_is_empty() {
        let span = DirtySpan::new(10, 5);
        assert!(span.is_empty());
        assert_eq!(span.len(), 0);
    }

    #[test]
    fn dirty_span_u16_max() {
        let span = DirtySpan::new(0, u16::MAX);
        assert_eq!(span.len(), u16::MAX);
        assert!(!span.is_empty());
    }

    #[test]
    fn dirty_span_full_range() {
        let span = DirtySpan::new(0, u16::MAX);
        let other = DirtySpan::new(u16::MAX - 1, u16::MAX);
        // Should be mergeable with gap 0 since they're adjacent.
        assert!(span.mergeable(&other, 0));
    }

    #[test]
    fn dirty_span_mergeable_exact_boundary() {
        // Spans [0,5) and [5,10) with merge_gap=0 → adjacent → mergeable.
        let a = DirtySpan::new(0, 5);
        let b = DirtySpan::new(5, 10);
        assert!(a.mergeable(&b, 0));
        assert!(b.mergeable(&a, 0));
    }

    #[test]
    fn dirty_span_not_mergeable_gap_1_no_merge_gap() {
        // Spans [0,5) and [6,10) with merge_gap=0 → gap of 1 → not mergeable.
        let a = DirtySpan::new(0, 5);
        let b = DirtySpan::new(6, 10);
        assert!(!a.mergeable(&b, 0));
    }

    #[test]
    fn dirty_span_mergeable_gap_1_with_merge_gap_1() {
        // Spans [0,5) and [6,10) with merge_gap=1 → gap of 1 → mergeable.
        let a = DirtySpan::new(0, 5);
        let b = DirtySpan::new(6, 10);
        assert!(a.mergeable(&b, 1));
    }

    #[test]
    fn dirty_span_merge_union() {
        let mut a = DirtySpan::new(3, 7);
        let b = DirtySpan::new(5, 12);
        a.merge(&b);
        assert_eq!(a.start, 3);
        assert_eq!(a.end, 12);
    }

    #[test]
    fn dirty_span_merge_disjoint() {
        let mut a = DirtySpan::new(0, 5);
        let b = DirtySpan::new(10, 20);
        a.merge(&b);
        // Union covers [0, 20) even if gap exists.
        assert_eq!(a.start, 0);
        assert_eq!(a.end, 20);
    }

    #[test]
    fn dirty_span_merge_subset() {
        let mut a = DirtySpan::new(0, 20);
        let b = DirtySpan::new(5, 10);
        a.merge(&b);
        // Larger span unchanged.
        assert_eq!(a.start, 0);
        assert_eq!(a.end, 20);
    }

    // ── DirtyTracker edge cases ─────────────────────────────────────

    #[test]
    fn tracker_mark_cell_at_last_col() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_cell(0, 79); // cols - 1
        assert!(t.is_dirty());
        let spans = t.row_spans(0).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 79);
        assert_eq!(spans[0].end, 80);
    }

    #[test]
    fn tracker_mark_span_clamped_to_cols() {
        let mut t = DirtyTracker::new(10, 1);
        t.mark_span(0, 5, 100); // end_col > cols → clamped to 10
        let spans = t.row_spans(0).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 5);
        assert_eq!(spans[0].end, 10);
    }

    #[test]
    fn tracker_mark_row_then_mark_cell_stays_full() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_row(0);
        t.mark_cell(0, 10);
        // Row still full — mark_cell on full row is a no-op.
        assert!(t.row_spans(0).is_none());
        assert_eq!(t.dirty_row_count(), 1);
    }

    #[test]
    fn tracker_mark_row_twice_no_double_count() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_row(5);
        t.mark_row(5);
        assert_eq!(t.dirty_row_count(), 1);
    }

    #[test]
    fn tracker_mark_cell_twice_same_cell() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_cell(3, 10);
        t.mark_cell(3, 10);
        assert_eq!(t.dirty_row_count(), 1);
        let spans = t.row_spans(3).unwrap();
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn tracker_resize_to_zero_rows() {
        let mut t = DirtyTracker::new(80, 24);
        t.resize(80, 0);
        assert_eq!(t.row_count(), 0);
        assert_eq!(t.dirty_row_count(), 0);
        assert!(!t.is_dirty());
    }

    #[test]
    fn tracker_resize_to_zero_cols() {
        let mut t = DirtyTracker::new(80, 24);
        t.resize(0, 24);
        assert_eq!(t.row_count(), 24);
        // All rows marked dirty by resize.
        assert_eq!(t.dirty_row_count(), 24);
    }

    #[test]
    fn tracker_clear_then_mark_cell() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_all();
        t.clear();
        t.mark_cell(0, 0);
        assert_eq!(t.dirty_row_count(), 1);
        // Row 0 should have spans, not full.
        let spans = t.row_spans(0).unwrap();
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn tracker_clone_independence() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_cell(0, 5);
        let mut t2 = t.clone();
        t2.mark_cell(1, 10);
        assert_eq!(t.dirty_row_count(), 1);
        assert_eq!(t2.dirty_row_count(), 2);
    }

    #[test]
    fn tracker_row_spans_out_of_bounds() {
        let t = DirtyTracker::new(80, 24);
        assert_eq!(t.row_spans(24), None);
        assert_eq!(t.row_spans(100), None);
    }

    #[test]
    fn tracker_is_row_dirty_out_of_bounds() {
        let t = DirtyTracker::new(80, 24);
        assert!(!t.is_row_dirty(24));
        assert!(!t.is_row_dirty(u16::MAX));
    }

    #[test]
    fn tracker_multiple_spans_coalesce_three_way() {
        let mut t = DirtyTracker::new(80, 24);
        t.set_merge_gap(0);
        t.mark_span(0, 0, 3);
        t.mark_span(0, 6, 9);
        // Now add a span that bridges the gap.
        t.mark_span(0, 3, 6);
        let spans = t.row_spans(0).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, 9);
    }

    #[test]
    fn tracker_mark_span_empty_range_no_op() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_span(0, 5, 5); // start == end
        assert!(!t.is_dirty());
    }

    #[test]
    fn tracker_mark_span_reversed_range_no_op() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_span(0, 10, 5); // start > end
        assert!(!t.is_dirty());
    }

    #[test]
    fn tracker_resize_shrink_then_grow() {
        let mut t = DirtyTracker::new(80, 24);
        t.mark_cell(20, 0);
        t.resize(80, 10); // Shrinks — row 20 gone.
        assert_eq!(t.row_count(), 10);
        t.clear();
        t.resize(80, 30); // Grows.
        assert_eq!(t.row_count(), 30);
        assert_eq!(t.dirty_row_count(), 30); // Resize marks all dirty.
    }

    // ── ChangeRun edge cases ────────────────────────────────────────

    #[test]
    fn change_run_empty() {
        let run = ChangeRun {
            row: 0,
            start_col: 5,
            end_col: 5,
        };
        assert!(run.is_empty());
        assert_eq!(run.len(), 0);
    }

    #[test]
    fn change_run_single_cell() {
        let run = ChangeRun {
            row: 0,
            start_col: 0,
            end_col: 1,
        };
        assert!(!run.is_empty());
        assert_eq!(run.len(), 1);
    }

    #[test]
    fn change_run_clone_and_eq() {
        let run = ChangeRun {
            row: 5,
            start_col: 10,
            end_col: 20,
        };
        let run2 = run.clone();
        assert_eq!(run, run2);
    }

    // ── Patch edge cases ────────────────────────────────────────────

    #[test]
    fn patch_default_trait() {
        let p = Patch::default();
        assert_eq!(p.cols, 0);
        assert_eq!(p.rows, 0);
        assert!(p.is_empty());
    }

    #[test]
    fn patch_density_full_grid() {
        let mut p = Patch::new(2, 2);
        for r in 0..2u16 {
            for c in 0..2u16 {
                p.push(r, c, Cell::new('X'));
            }
        }
        assert!((p.density() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn patch_density_zero_cols_nonzero_rows() {
        let p = Patch::new(0, 10);
        assert_eq!(p.density(), 0.0);
    }

    #[test]
    fn patch_density_nonzero_cols_zero_rows() {
        let p = Patch::new(10, 0);
        assert_eq!(p.density(), 0.0);
    }

    #[test]
    fn patch_runs_single_update() {
        let mut p = Patch::new(80, 24);
        p.push(12, 40, Cell::new('Z'));
        let runs = p.runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].row, 12);
        assert_eq!(runs[0].start_col, 40);
        assert_eq!(runs[0].end_col, 41);
    }

    #[test]
    fn patch_runs_gap_of_one_splits() {
        let mut p = Patch::new(10, 1);
        p.push(0, 0, Cell::new('A'));
        // col 1 is skipped
        p.push(0, 2, Cell::new('B'));
        let runs = p.runs();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].end_col, 1);
        assert_eq!(runs[1].start_col, 2);
    }

    #[test]
    fn patch_runs_into_clears_buffer() {
        let mut p = Patch::new(10, 1);
        p.push(0, 0, Cell::new('A'));

        let mut buf = vec![ChangeRun {
            row: 99,
            start_col: 99,
            end_col: 100,
        }];
        p.runs_into(&mut buf);
        // Buffer should have been cleared and contain only the new run.
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].row, 0);
    }

    #[test]
    fn patch_clone_and_eq() {
        let mut p = Patch::new(5, 5);
        p.push(0, 0, Cell::new('X'));
        let p2 = p.clone();
        assert_eq!(p, p2);
    }

    #[test]
    fn patch_runs_three_rows_one_cell_each() {
        let mut p = Patch::new(80, 24);
        p.push(0, 10, Cell::new('A'));
        p.push(5, 20, Cell::new('B'));
        p.push(10, 30, Cell::new('C'));
        let runs = p.runs();
        assert_eq!(runs.len(), 3);
        for run in &runs {
            assert_eq!(run.len(), 1);
        }
    }

    // ── GridDiff edge cases ─────────────────────────────────────────

    #[test]
    fn diff_old_smaller_than_new() {
        let old = Grid::new(5, 3);
        let mut new = Grid::new(10, 5);
        new.cell_mut(4, 9).unwrap().set_content('Z', 1);
        let patch = GridDiff::diff(&old, &new);
        // Cells beyond old's bounds (old returns None, new returns Some) → changes.
        assert!(!patch.is_empty());
        // Should include the cell at (4,9) at minimum.
        assert!(patch.updates.iter().any(|u| u.row == 4 && u.col == 9));
    }

    #[test]
    fn diff_new_smaller_than_old() {
        let mut old = Grid::new(10, 5);
        old.cell_mut(4, 9).unwrap().set_content('A', 1);
        let new = Grid::new(5, 3);
        // Diff uses new's dimensions, so (4,9) is out of range.
        let patch = GridDiff::diff(&old, &new);
        assert_eq!(patch.cols, 5);
        assert_eq!(patch.rows, 3);
        // No update at (4,9) since it's beyond new's bounds.
        assert!(patch.updates.iter().all(|u| u.row < 3 && u.col < 5));
    }

    #[test]
    fn diff_into_called_twice() {
        let a = Grid::new(5, 3);
        let mut b = Grid::new(5, 3);
        b.cell_mut(0, 0).unwrap().set_content('X', 1);

        let mut patch = Patch::new(0, 0);
        GridDiff::diff_into(&a, &b, &mut patch);
        assert_eq!(patch.len(), 1);

        // Second call with identical grids → patch should be empty.
        GridDiff::diff_into(&b, &b, &mut patch);
        assert_eq!(patch.len(), 0);
        assert_eq!(patch.cols, 5);
    }

    #[test]
    fn diff_dirty_multiple_spans_same_row() {
        let a = Grid::new(20, 1);
        let mut b = Grid::new(20, 1);
        b.cell_mut(0, 2).unwrap().set_content('A', 1);
        b.cell_mut(0, 15).unwrap().set_content('B', 1);

        let mut tracker = DirtyTracker::new(20, 1);
        tracker.set_merge_gap(0);
        tracker.mark_span(0, 0, 5);
        tracker.mark_span(0, 12, 18);

        let patch = GridDiff::diff_dirty(&a, &b, &tracker);
        assert_eq!(patch.len(), 2);
        assert_eq!(patch.updates[0].col, 2);
        assert_eq!(patch.updates[1].col, 15);
    }

    #[test]
    fn diff_dirty_full_row_vs_span_row() {
        let a = Grid::new(10, 3);
        let mut b = Grid::new(10, 3);
        // Row 0: full dirty, one change.
        b.cell_mut(0, 5).unwrap().set_content('A', 1);
        // Row 2: span dirty, one change.
        b.cell_mut(2, 3).unwrap().set_content('B', 1);

        let mut tracker = DirtyTracker::new(10, 3);
        tracker.mark_row(0);
        tracker.mark_span(2, 0, 5);

        let patch = GridDiff::diff_dirty(&a, &b, &tracker);
        assert_eq!(patch.len(), 2);
        assert_eq!(patch.updates[0].row, 0);
        assert_eq!(patch.updates[1].row, 2);
    }

    #[test]
    fn diff_all_cells_changed() {
        let a = Grid::new(3, 3);
        let mut b = Grid::new(3, 3);
        for r in 0..3u16 {
            for c in 0..3u16 {
                b.cell_mut(r, c).unwrap().set_content('X', 1);
            }
        }
        let patch = GridDiff::diff(&a, &b);
        assert_eq!(patch.len(), 9);
        assert!((patch.density() - 1.0).abs() < f64::EPSILON);
        let runs = patch.runs();
        assert_eq!(runs.len(), 3); // One run per row.
        for run in &runs {
            assert_eq!(run.len(), 3);
        }
    }

    #[test]
    fn diff_dirty_span_beyond_grid_cols() {
        let a = Grid::new(5, 1);
        let mut b = Grid::new(5, 1);
        b.cell_mut(0, 3).unwrap().set_content('Z', 1);

        let mut tracker = DirtyTracker::new(5, 1);
        // Mark a span that extends beyond grid cols.
        tracker.mark_span(0, 0, 100);
        let patch = GridDiff::diff_dirty(&a, &b, &tracker);
        assert_eq!(patch.len(), 1);
        assert_eq!(patch.updates[0].col, 3);
    }

    // ── CellUpdate edge cases ───────────────────────────────────────

    #[test]
    fn cell_update_clone_and_eq() {
        let u = CellUpdate {
            row: 5,
            col: 10,
            cell: Cell::new('X'),
        };
        let u2 = u.clone();
        assert_eq!(u, u2);
        assert_eq!(u.row, u2.row);
        assert_eq!(u.col, u2.col);
    }

    #[test]
    fn cell_update_debug_format() {
        let u = CellUpdate {
            row: 0,
            col: 0,
            cell: Cell::new('A'),
        };
        let dbg = format!("{u:?}");
        assert!(dbg.contains("CellUpdate"));
    }

    // ── DirtyTracker + merge_gap interactions ───────────────────────

    #[test]
    fn tracker_merge_gap_zero_keeps_separate() {
        let mut t = DirtyTracker::new(80, 24);
        t.set_merge_gap(0);
        t.mark_span(0, 0, 5);
        t.mark_span(0, 5, 10); // adjacent at col 5
        let spans = t.row_spans(0).unwrap();
        // With merge_gap=0, adjacent spans [0,5) and [5,10) should merge.
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, 10);
    }

    #[test]
    fn tracker_merge_gap_large_merges_everything() {
        let mut t = DirtyTracker::new(80, 24);
        t.set_merge_gap(u16::MAX);
        t.mark_span(0, 0, 5);
        t.mark_span(0, 70, 80);
        let spans = t.row_spans(0).unwrap();
        // Large merge gap should merge everything.
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, 80);
    }

    #[test]
    fn tracker_new_zero_dimensions() {
        let t = DirtyTracker::new(0, 0);
        assert_eq!(t.row_count(), 0);
        assert!(!t.is_dirty());
        // Out-of-bounds operations are no-ops.
        let mut t = t;
        t.mark_cell(0, 0);
        assert!(!t.is_dirty());
    }
}

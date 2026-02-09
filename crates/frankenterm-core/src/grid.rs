//! Terminal grid: 2D cell matrix representing the visible viewport.
//!
//! The grid is the primary data model for the terminal. It owns a flat vector
//! of cells indexed by `(row, col)` and provides methods for the operations
//! that the VT parser dispatches (print, erase, scroll, resize).

use crate::cell::{Cell, Color, HyperlinkRegistry, SgrAttrs};
use crate::scrollback::Scrollback;

/// 2D terminal cell grid.
///
/// Cells are stored in row-major order in a flat `Vec<Cell>`.
/// The grid does not own scrollback — see [`Scrollback`](crate::Scrollback).
#[derive(Debug, Clone)]
pub struct Grid {
    cells: Vec<Cell>,
    cols: u16,
    rows: u16,
}

impl Grid {
    /// Create a new grid filled with default (blank) cells.
    pub fn new(cols: u16, rows: u16) -> Self {
        let len = (cols as usize) * (rows as usize);
        Self {
            cells: vec![Cell::default(); len],
            cols,
            rows,
        }
    }

    /// Number of columns.
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Number of rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Get a reference to the cell at `(row, col)`.
    ///
    /// Returns `None` if out of bounds.
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if row < self.rows && col < self.cols {
            Some(&self.cells[self.index(row, col)])
        } else {
            None
        }
    }

    /// Get a mutable reference to the cell at `(row, col)`.
    ///
    /// Returns `None` if out of bounds.
    pub fn cell_mut(&mut self, row: u16, col: u16) -> Option<&mut Cell> {
        if row < self.rows && col < self.cols {
            let idx = self.index(row, col);
            Some(&mut self.cells[idx])
        } else {
            None
        }
    }

    /// Map a grid position to a hyperlink URI via the registry (OSC 8).
    ///
    /// This is intended for click/hover hit-testing in host renderers.
    pub fn hyperlink_uri_at<'a>(
        &self,
        row: u16,
        col: u16,
        registry: &'a HyperlinkRegistry,
    ) -> Option<&'a str> {
        let id = self.cell(row, col)?.hyperlink;
        registry.get(id)
    }

    /// Get a slice of cells for the given row.
    ///
    /// Returns `None` if `row` is out of bounds.
    pub fn row_cells(&self, row: u16) -> Option<&[Cell]> {
        if row < self.rows {
            let start = (row as usize) * (self.cols as usize);
            let end = start + (self.cols as usize);
            Some(&self.cells[start..end])
        } else {
            None
        }
    }

    /// Get a mutable slice of cells for the given row.
    pub fn row_cells_mut(&mut self, row: u16) -> Option<&mut [Cell]> {
        if row < self.rows {
            let start = (row as usize) * (self.cols as usize);
            let end = start + (self.cols as usize);
            Some(&mut self.cells[start..end])
        } else {
            None
        }
    }

    // ── Erase operations ────────────────────────────────────────────

    /// ED 0: Erase from cursor to end of display.
    pub fn erase_below(&mut self, row: u16, col: u16, bg: Color) {
        if row >= self.rows {
            return;
        }
        // Erase from cursor to end of current row.
        self.erase_range(row, col, row, self.cols, bg);
        // Erase all rows below.
        self.erase_range(row + 1, 0, self.rows, 0, bg);
    }

    /// ED 1: Erase from start of display to cursor (inclusive).
    pub fn erase_above(&mut self, row: u16, col: u16, bg: Color) {
        if row >= self.rows {
            return;
        }
        // Erase all rows above.
        if row > 0 {
            self.erase_range(0, 0, row, 0, bg);
        }
        // Erase from start of current row through cursor (inclusive).
        let ec = (col + 1).min(self.cols);
        self.erase_range(row, 0, row, ec, bg);
    }

    /// ED 2: Erase entire display.
    pub fn erase_all(&mut self, bg: Color) {
        for cell in &mut self.cells {
            cell.erase(bg);
        }
    }

    /// EL 0: Erase from cursor to end of line.
    pub fn erase_line_right(&mut self, row: u16, col: u16, bg: Color) {
        self.erase_range(row, col, row, self.cols, bg);
    }

    /// EL 1: Erase from start of line to cursor (inclusive).
    pub fn erase_line_left(&mut self, row: u16, col: u16, bg: Color) {
        let ec = (col + 1).min(self.cols);
        self.erase_range(row, 0, row, ec, bg);
    }

    /// EL 2: Erase entire line.
    pub fn erase_line(&mut self, row: u16, bg: Color) {
        self.erase_range(row, 0, row, self.cols, bg);
    }

    /// ECH: Erase `count` characters starting at `(row, col)`.
    pub fn erase_chars(&mut self, row: u16, col: u16, count: u16, bg: Color) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let end = (col + count).min(self.cols);
        self.erase_range(row, col, row, end, bg);
    }

    /// Erase a rectangular region. Single row if `end_row == start_row`,
    /// or full rows if `end_col == 0` for row > start_row.
    fn erase_range(
        &mut self,
        start_row: u16,
        start_col: u16,
        end_row: u16,
        end_col: u16,
        bg: Color,
    ) {
        let sr = start_row.min(self.rows);
        let er = end_row.min(self.rows);

        // Both bounds clamped to self.rows means nothing to erase.
        if sr >= self.rows {
            return;
        }

        if sr == er {
            // Single row partial erase.
            let sc = start_col.min(self.cols);
            let ec = end_col.min(self.cols);

            // Wide-char fixup (left): if erasing starts at a continuation
            // cell, its head is outside the range and becomes orphaned.
            if sc > 0 && sc < self.cols {
                let idx = self.index(sr, sc);
                if self.cells[idx].is_wide_continuation() {
                    let head_idx = self.index(sr, sc - 1);
                    self.cells[head_idx].erase(bg);
                }
            }
            // Wide-char fixup (right): if the cell just past the erased
            // range is a continuation, its head is being erased.
            if ec < self.cols {
                let idx = self.index(sr, ec);
                if self.cells[idx].is_wide_continuation() {
                    self.cells[idx].erase(bg);
                }
            }

            for c in sc..ec {
                let idx = self.index(sr, c);
                self.cells[idx].erase(bg);
            }
        } else {
            // First row partial.
            let sc = start_col.min(self.cols);

            // Wide-char fixup (left) for first row.
            if sc > 0 && sc < self.cols {
                let idx = self.index(sr, sc);
                if self.cells[idx].is_wide_continuation() {
                    let head_idx = self.index(sr, sc - 1);
                    self.cells[head_idx].erase(bg);
                }
            }

            for c in sc..self.cols {
                let idx = self.index(sr, c);
                self.cells[idx].erase(bg);
            }
            // Full rows in between.
            for r in (sr + 1)..er {
                for c in 0..self.cols {
                    let idx = self.index(r, c);
                    self.cells[idx].erase(bg);
                }
            }
            // Last row partial (if end_col > 0).
            if end_col > 0 && er < self.rows {
                let ec = end_col.min(self.cols);

                // Wide-char fixup (right) for last row.
                if ec < self.cols {
                    let idx = self.index(er, ec);
                    if self.cells[idx].is_wide_continuation() {
                        self.cells[idx].erase(bg);
                    }
                }

                for c in 0..ec {
                    let idx = self.index(er, c);
                    self.cells[idx].erase(bg);
                }
            }
        }
    }

    // ── Fill / clear ────────────────────────────────────────────────

    /// Fill a region of cells with defaults (erase with default bg).
    ///
    /// Coordinates are clamped to grid bounds.
    pub fn clear_region(&mut self, start_row: u16, start_col: u16, end_row: u16, end_col: u16) {
        let sr = start_row.min(self.rows);
        let er = end_row.min(self.rows);
        let sc = start_col.min(self.cols);
        let ec = end_col.min(self.cols);

        for r in sr..er {
            for c in sc..ec {
                let idx = self.index(r, c);
                self.cells[idx] = Cell::default();
            }
        }
    }

    /// Clear the entire grid.
    pub fn clear(&mut self) {
        for cell in &mut self.cells {
            *cell = Cell::default();
        }
    }

    /// Fill every cell with the given character and default attributes.
    ///
    /// Used by DECALN (Screen Alignment Test) which fills the screen with 'E'.
    pub fn fill_all(&mut self, ch: char) {
        for cell in &mut self.cells {
            *cell = Cell::default();
            cell.set_content(ch, 1);
        }
    }

    // ── Insert / delete characters ──────────────────────────────────

    /// ICH: Insert `count` blank cells at `(row, col)`, shifting existing
    /// cells to the right. Cells that shift past the right margin are lost.
    pub fn insert_chars(&mut self, row: u16, col: u16, count: u16, bg: Color) {
        if row >= self.rows || col >= self.cols || count == 0 {
            return;
        }
        let cols = self.cols as usize;
        let c = col as usize;
        let n = (count as usize).min(cols - c);
        let start = self.index(row, 0);
        let row_slice = &mut self.cells[start..start + cols];

        // Wide-char fixup: if inserting at a continuation cell, the head
        // at col-1 loses its pair and must be erased.
        let was_continuation = row_slice[c].is_wide_continuation();
        if was_continuation && c > 0 {
            row_slice[c - 1].erase(bg);
        }

        // Shift right: copy from right to left to avoid overlap issues.
        for i in (c + n..cols).rev() {
            row_slice[i] = row_slice[i - n];
        }
        // Blank the inserted positions.
        for cell in &mut row_slice[c..c + n] {
            cell.erase(bg);
        }

        // Wide-char fixup: the continuation that was at col shifted to
        // col+n; since its head was erased, clean it up.
        if was_continuation && c + n < cols && row_slice[c + n].is_wide_continuation() {
            row_slice[c + n].erase(bg);
        }

        // Wide-char fixup: if a wide head shifted to the last column,
        // its continuation fell off the right margin.
        if row_slice[cols - 1].is_wide() {
            row_slice[cols - 1].erase(bg);
        }
    }

    /// DCH: Delete `count` cells at `(row, col)`, shifting remaining cells
    /// left. Blank cells are inserted at the right margin.
    pub fn delete_chars(&mut self, row: u16, col: u16, count: u16, bg: Color) {
        if row >= self.rows || col >= self.cols || count == 0 {
            return;
        }
        let cols = self.cols as usize;
        let c = col as usize;
        let n = (count as usize).min(cols - c);
        let start = self.index(row, 0);
        let row_slice = &mut self.cells[start..start + cols];

        // Wide-char fixup: if deleting at a continuation cell, the head
        // at col-1 loses its pair and must be erased.
        if row_slice[c].is_wide_continuation() && c > 0 {
            row_slice[c - 1].erase(bg);
        }

        // Shift left.
        for i in c..cols - n {
            row_slice[i] = row_slice[i + n];
        }
        // Blank the vacated positions at the right.
        for cell in &mut row_slice[cols - n..] {
            cell.erase(bg);
        }

        // Wide-char fixup: after shift, if cell at col is an orphaned
        // continuation (its head was deleted), clean it up.
        if c < cols && row_slice[c].is_wide_continuation() {
            row_slice[c].erase(bg);
        }
    }

    // ── Scroll operations ───────────────────────────────────────────

    /// Scroll lines up: remove `count` rows starting at `top`, shift everything
    /// above `bottom` up, and fill the gap at the bottom with blanks.
    ///
    /// `top` and `bottom` define the scroll region (0-indexed, exclusive bottom).
    pub fn scroll_up(&mut self, top: u16, bottom: u16, count: u16, bg: Color) {
        let top = top.min(self.rows);
        let bottom = bottom.min(self.rows);
        if top >= bottom || count == 0 {
            return;
        }
        let count = count.min(bottom - top);
        let cols = self.cols as usize;

        // Shift rows up.
        let src_start = (top + count) as usize * cols;
        let dst_start = top as usize * cols;
        let move_len = (bottom - top - count) as usize * cols;
        self.cells
            .copy_within(src_start..src_start + move_len, dst_start);

        // Blank the vacated rows at the bottom (BCE: inherit cursor bg).
        let blank_start = (bottom - count) as usize * cols;
        let blank_end = bottom as usize * cols;
        for cell in &mut self.cells[blank_start..blank_end] {
            cell.erase(bg);
        }
    }

    /// Scroll lines down: insert `count` blank rows at `top`, shifting
    /// everything down and discarding rows that fall past `bottom`.
    pub fn scroll_down(&mut self, top: u16, bottom: u16, count: u16, bg: Color) {
        let top = top.min(self.rows);
        let bottom = bottom.min(self.rows);
        if top >= bottom || count == 0 {
            return;
        }
        let count = count.min(bottom - top);
        let cols = self.cols as usize;

        // Shift rows down.
        let src_start = top as usize * cols;
        let src_len = (bottom - top - count) as usize * cols;
        let dst_start = (top + count) as usize * cols;
        self.cells
            .copy_within(src_start..src_start + src_len, dst_start);

        // Blank the vacated rows at the top (BCE: inherit cursor bg).
        let blank_end = (top + count) as usize * cols;
        for cell in &mut self.cells[top as usize * cols..blank_end] {
            cell.erase(bg);
        }
    }

    /// Scroll up, pushing the evicted top rows into a scrollback buffer.
    ///
    /// This is the normal "content scrolls up" operation triggered by a newline
    /// at the bottom of the scroll region. The topmost `count` rows within
    /// `[top, bottom)` are pushed to `scrollback` before being discarded.
    pub fn scroll_up_into(
        &mut self,
        top: u16,
        bottom: u16,
        count: u16,
        scrollback: &mut Scrollback,
        bg: Color,
    ) {
        let top = top.min(self.rows);
        let bottom = bottom.min(self.rows);
        if top >= bottom || count == 0 {
            return;
        }
        let count = count.min(bottom - top);

        // Push evicted rows to scrollback.
        for r in top..top + count {
            if let Some(row) = self.row_cells(r) {
                let _ = scrollback.push_row(row, false);
            }
        }

        // Now do the normal scroll-up.
        self.scroll_up(top, bottom, count, bg);
    }

    /// Scroll down, pulling lines from scrollback into the vacated rows at top.
    ///
    /// This is the reverse of `scroll_up_into`: content shifts down and
    /// scrollback lines are restored at the top of the scroll region.
    pub fn scroll_down_from(
        &mut self,
        top: u16,
        bottom: u16,
        count: u16,
        scrollback: &mut Scrollback,
        bg: Color,
    ) {
        let top = top.min(self.rows);
        let bottom = bottom.min(self.rows);
        if top >= bottom || count == 0 {
            return;
        }
        let count = count.min(bottom - top);

        // Normal scroll-down first (creates blank rows at top).
        self.scroll_down(top, bottom, count, bg);

        // Fill the vacated top rows from scrollback (newest first).
        for r in (top..top + count).rev() {
            if let Some(line) = scrollback.pop_newest() {
                let row_start = self.index(r, 0);
                let cols = self.cols as usize;
                let copy_len = line.cells.len().min(cols);
                self.cells[row_start..row_start + copy_len]
                    .copy_from_slice(&line.cells[..copy_len]);
                // If the scrollback line is shorter than cols, the rest stays blank.
            }
        }
    }

    /// IL: Insert `count` blank lines at `row` within the scroll region
    /// `[top, bottom)`. Lines that fall past `bottom` are discarded.
    pub fn insert_lines(&mut self, row: u16, count: u16, top: u16, bottom: u16, bg: Color) {
        if row < top || row >= bottom {
            return;
        }
        self.scroll_down(row, bottom, count, bg);
    }

    /// DL: Delete `count` lines at `row` within the scroll region
    /// `[top, bottom)`. Blank lines appear at `bottom - count`.
    pub fn delete_lines(&mut self, row: u16, count: u16, top: u16, bottom: u16, bg: Color) {
        if row < top || row >= bottom {
            return;
        }
        self.scroll_up(row, bottom, count, bg);
    }

    // ── Wide character handling ──────────────────────────────────────

    /// Write a wide (2-column) character at `(row, col)`.
    ///
    /// Sets the leading cell at `col` and the continuation cell at `col+1`.
    /// If `col+1` is past the right margin, no write occurs.
    /// Also clears any existing wide char that this write would partially
    /// overwrite (the "wide char fixup").
    pub fn write_wide_char(&mut self, row: u16, col: u16, ch: char, attrs: SgrAttrs) {
        if row >= self.rows || col + 1 >= self.cols {
            return;
        }
        // Fixup: if we're overwriting the continuation of a wide char at col,
        // clear the leading cell at col-1.
        if col > 0 {
            let prev_idx = self.index(row, col - 1);
            if self.cells[prev_idx].is_wide() {
                self.cells[prev_idx].clear();
            }
        }
        // Fixup: if we're overwriting the leading cell of a wide char at col+1,
        // clear the continuation at col+2.
        let next_idx = self.index(row, col + 1);
        if self.cells[next_idx].is_wide() && col + 2 < self.cols {
            let cont_idx = self.index(row, col + 2);
            self.cells[cont_idx].clear();
        }

        let (lead, cont) = Cell::wide(ch, attrs);
        let lead_idx = self.index(row, col);
        self.cells[lead_idx] = lead;
        self.cells[next_idx] = cont;
    }

    /// Write one printable Unicode scalar with terminal-width semantics.
    ///
    /// Returns the written display width:
    /// - `0` for non-spacing marks/format controls (fallback: ignored)
    /// - `1` for narrow cells
    /// - `2` for wide cells
    ///
    /// If a wide character does not fit at `col` (i.e. `col+1 >= cols`), this
    /// method returns `0` and leaves the grid unchanged. Callers are responsible
    /// for wrap policy decisions.
    pub fn write_printable(&mut self, row: u16, col: u16, ch: char, attrs: SgrAttrs) -> u8 {
        if row >= self.rows || col >= self.cols {
            return 0;
        }

        let width = Cell::display_width(ch);
        match width {
            0 => 0,
            1 => {
                // If we overwrite the continuation of a wide char, clear its head.
                if col > 0 {
                    let prev_idx = self.index(row, col - 1);
                    if self.cells[prev_idx].is_wide() {
                        self.cells[prev_idx].clear();
                    }
                }

                // If the current cell is a wide head, clear its continuation.
                let idx = self.index(row, col);
                if self.cells[idx].is_wide() && col + 1 < self.cols {
                    let cont_idx = self.index(row, col + 1);
                    self.cells[cont_idx].clear();
                }

                if let Some(cell) = self.cell_mut(row, col) {
                    cell.set_content(ch, 1);
                    cell.attrs = attrs;
                }
                1
            }
            _ => {
                if col + 1 >= self.cols {
                    return 0;
                }
                self.write_wide_char(row, col, ch, attrs);
                2
            }
        }
    }

    // ── Resize ──────────────────────────────────────────────────────

    /// Resize the grid to new dimensions.
    ///
    /// Content is preserved where possible: rows/columns that fit in the
    /// new dimensions are kept, extras are truncated, new space is blanked.
    pub fn resize(&mut self, new_cols: u16, new_rows: u16) {
        if new_cols == self.cols && new_rows == self.rows {
            return;
        }
        let mut new_cells = vec![Cell::default(); new_cols as usize * new_rows as usize];
        let copy_rows = self.rows.min(new_rows);
        let copy_cols = self.cols.min(new_cols);

        for r in 0..copy_rows {
            let old_start = (r as usize) * (self.cols as usize);
            let new_start = (r as usize) * (new_cols as usize);
            new_cells[new_start..new_start + copy_cols as usize]
                .copy_from_slice(&self.cells[old_start..old_start + copy_cols as usize]);
        }

        self.cells = new_cells;
        self.cols = new_cols;
        self.rows = new_rows;
    }

    /// Resize with scrollback integration.
    ///
    /// # Reflow policy: truncate/extend (no soft-wrap reflow)
    ///
    /// - **Width decrease**: cells past the new width are discarded.
    /// - **Width increase**: new columns are filled with blanks.
    /// - **Height decrease**: excess rows at the top are pushed to scrollback,
    ///   keeping the cursor anchored at its current absolute position.
    /// - **Height increase**: rows are pulled back from scrollback to fill the
    ///   new space at the top.
    ///
    /// Returns the new cursor row after adjustment.
    pub fn resize_with_scrollback(
        &mut self,
        new_cols: u16,
        new_rows: u16,
        cursor_row: u16,
        scrollback: &mut Scrollback,
    ) -> u16 {
        if new_cols == self.cols && new_rows == self.rows {
            return cursor_row;
        }

        let old_rows = self.rows;
        let mut new_cursor_row = cursor_row;

        // ── Handle height decrease: push excess top rows to scrollback ──
        if new_rows < old_rows {
            // We want to keep the content around the cursor visible.
            // Push rows from the top that won't fit.
            let excess = old_rows - new_rows;
            // The cursor should remain in the viewport. Calculate how many
            // rows above the cursor we can afford to keep.
            let rows_above_cursor = cursor_row;
            let rows_to_push = excess.min(rows_above_cursor);

            for r in 0..rows_to_push {
                if let Some(row) = self.row_cells(r) {
                    let _ = scrollback.push_row(row, false);
                }
            }

            if rows_to_push > 0 {
                // Shift remaining content up.
                let cols = self.cols as usize;
                let src = rows_to_push as usize * cols;
                let len = (old_rows - rows_to_push) as usize * cols;
                self.cells.copy_within(src..src + len, 0);
                new_cursor_row = cursor_row - rows_to_push;
            }
        }

        // ── Handle height increase: pull rows from scrollback ──
        let mut pulled_from_scrollback: u16 = 0;
        if new_rows > old_rows {
            let extra = new_rows - old_rows;
            // Pull up to `extra` lines from scrollback.
            let available = scrollback.len().min(extra as usize) as u16;
            pulled_from_scrollback = available;
        }

        // ── Build new cell buffer ──
        let new_total = new_cols as usize * new_rows as usize;
        let mut new_cells = vec![Cell::default(); new_total];

        // If we pulled lines from scrollback, place them at the top.
        let mut dest_row: u16 = 0;
        if pulled_from_scrollback > 0 {
            // Collect lines from scrollback (newest = bottom of the pulled region).
            let mut pulled_lines = Vec::with_capacity(pulled_from_scrollback as usize);
            for _ in 0..pulled_from_scrollback {
                if let Some(line) = scrollback.pop_newest() {
                    pulled_lines.push(line);
                }
            }
            // Reverse so oldest is at top.
            pulled_lines.reverse();

            for line in &pulled_lines {
                let new_start = dest_row as usize * new_cols as usize;
                let copy_len = line.cells.len().min(new_cols as usize);
                new_cells[new_start..new_start + copy_len].copy_from_slice(&line.cells[..copy_len]);
                dest_row += 1;
            }
            new_cursor_row = cursor_row + pulled_from_scrollback;
        }

        // Copy existing rows (after any top-push) into the new buffer.
        let copy_cols = self.cols.min(new_cols) as usize;
        let src_row_start = if new_rows < old_rows {
            // We already shifted content up, so start from row 0 of the
            // (now-compacted) old buffer.
            0u16
        } else {
            0u16
        };
        let src_rows_available = if new_rows < old_rows {
            (old_rows - (cursor_row.saturating_sub(new_cursor_row))).min(new_rows)
        } else {
            old_rows
        };
        let dest_rows_remaining = new_rows - dest_row;
        let copy_rows = src_rows_available.min(dest_rows_remaining);

        for r in 0..copy_rows {
            let old_start = (src_row_start + r) as usize * self.cols as usize;
            let new_start = (dest_row + r) as usize * new_cols as usize;
            if old_start + copy_cols <= self.cells.len() && new_start + copy_cols <= new_cells.len()
            {
                new_cells[new_start..new_start + copy_cols]
                    .copy_from_slice(&self.cells[old_start..old_start + copy_cols]);
            }
        }

        self.cells = new_cells;
        self.cols = new_cols;
        self.rows = new_rows;

        // Clamp cursor row to new bounds.
        new_cursor_row.min(new_rows.saturating_sub(1))
    }

    /// Convert (row, col) to flat index.
    #[inline]
    fn index(&self, row: u16, col: u16) -> usize {
        (row as usize) * (self.cols as usize) + (col as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::SgrAttrs;

    #[test]
    fn new_grid_has_correct_dimensions() {
        let g = Grid::new(80, 24);
        assert_eq!(g.cols(), 80);
        assert_eq!(g.rows(), 24);
    }

    #[test]
    fn cells_default_to_space() {
        let g = Grid::new(10, 5);
        let cell = g.cell(0, 0).unwrap();
        assert_eq!(cell.content(), ' ');
    }

    #[test]
    fn cell_mut_allows_modification() {
        let mut g = Grid::new(10, 5);
        if let Some(cell) = g.cell_mut(2, 3) {
            cell.set_content('X', 1);
        }
        assert_eq!(g.cell(2, 3).unwrap().content(), 'X');
    }

    #[test]
    fn out_of_bounds_returns_none() {
        let g = Grid::new(10, 5);
        assert!(g.cell(5, 0).is_none());
        assert!(g.cell(0, 10).is_none());
    }

    #[test]
    fn row_cells_returns_correct_slice() {
        let mut g = Grid::new(3, 2);
        g.cell_mut(1, 0).unwrap().set_content('A', 1);
        g.cell_mut(1, 1).unwrap().set_content('B', 1);
        g.cell_mut(1, 2).unwrap().set_content('C', 1);
        let row = g.row_cells(1).unwrap();
        assert_eq!(row.len(), 3);
        assert_eq!(row[0].content(), 'A');
        assert_eq!(row[1].content(), 'B');
        assert_eq!(row[2].content(), 'C');
    }

    #[test]
    fn clear_region_erases_cells() {
        let mut g = Grid::new(5, 5);
        g.cell_mut(1, 1).unwrap().set_content('X', 1);
        g.cell_mut(2, 2).unwrap().set_content('Y', 1);
        g.clear_region(1, 1, 3, 3);
        assert_eq!(g.cell(1, 1).unwrap().content(), ' ');
        assert_eq!(g.cell(2, 2).unwrap().content(), ' ');
    }

    #[test]
    fn scroll_up_shifts_and_blanks() {
        let mut g = Grid::new(3, 4);
        for r in 0..4u16 {
            let ch = (b'A' + r as u8) as char;
            for c in 0..3u16 {
                g.cell_mut(r, c).unwrap().set_content(ch, 1);
            }
        }
        g.scroll_up(0, 4, 1, Color::Default);
        assert_eq!(g.cell(0, 0).unwrap().content(), 'B');
        assert_eq!(g.cell(1, 0).unwrap().content(), 'C');
        assert_eq!(g.cell(2, 0).unwrap().content(), 'D');
        assert_eq!(g.cell(3, 0).unwrap().content(), ' ');
    }

    #[test]
    fn scroll_down_shifts_and_blanks() {
        let mut g = Grid::new(3, 4);
        for r in 0..4u16 {
            let ch = (b'A' + r as u8) as char;
            for c in 0..3u16 {
                g.cell_mut(r, c).unwrap().set_content(ch, 1);
            }
        }
        g.scroll_down(0, 4, 1, Color::Default);
        assert_eq!(g.cell(0, 0).unwrap().content(), ' ');
        assert_eq!(g.cell(1, 0).unwrap().content(), 'A');
        assert_eq!(g.cell(2, 0).unwrap().content(), 'B');
        assert_eq!(g.cell(3, 0).unwrap().content(), 'C');
    }

    // ── Erase operations ────────────────────────────────────────────

    #[test]
    fn erase_below_from_mid_row() {
        let mut g = Grid::new(5, 3);
        for r in 0..3u16 {
            for c in 0..5u16 {
                g.cell_mut(r, c).unwrap().set_content('X', 1);
            }
        }
        g.erase_below(1, 2, Color::Default);
        // Row 0 untouched.
        assert_eq!(g.cell(0, 4).unwrap().content(), 'X');
        // Row 1 cols 0-1 untouched, cols 2-4 erased.
        assert_eq!(g.cell(1, 1).unwrap().content(), 'X');
        assert_eq!(g.cell(1, 2).unwrap().content(), ' ');
        assert_eq!(g.cell(1, 4).unwrap().content(), ' ');
        // Row 2 fully erased.
        assert_eq!(g.cell(2, 0).unwrap().content(), ' ');
    }

    #[test]
    fn erase_above_from_mid_row() {
        let mut g = Grid::new(5, 3);
        for r in 0..3u16 {
            for c in 0..5u16 {
                g.cell_mut(r, c).unwrap().set_content('X', 1);
            }
        }
        g.erase_above(1, 2, Color::Default);
        // Row 0 fully erased.
        assert_eq!(g.cell(0, 0).unwrap().content(), ' ');
        // Row 1 cols 0-2 erased, cols 3-4 untouched.
        assert_eq!(g.cell(1, 2).unwrap().content(), ' ');
        assert_eq!(g.cell(1, 3).unwrap().content(), 'X');
        // Row 2 untouched.
        assert_eq!(g.cell(2, 0).unwrap().content(), 'X');
    }

    #[test]
    fn erase_all_clears_grid() {
        let mut g = Grid::new(3, 3);
        g.cell_mut(1, 1).unwrap().set_content('Y', 1);
        g.erase_all(Color::Named(4));
        assert_eq!(g.cell(1, 1).unwrap().content(), ' ');
        assert_eq!(g.cell(1, 1).unwrap().attrs.bg, Color::Named(4));
    }

    #[test]
    fn erase_line_right() {
        let mut g = Grid::new(5, 1);
        for c in 0..5u16 {
            g.cell_mut(0, c)
                .unwrap()
                .set_content((b'A' + c as u8) as char, 1);
        }
        g.erase_line_right(0, 2, Color::Default);
        assert_eq!(g.cell(0, 0).unwrap().content(), 'A');
        assert_eq!(g.cell(0, 1).unwrap().content(), 'B');
        assert_eq!(g.cell(0, 2).unwrap().content(), ' ');
        assert_eq!(g.cell(0, 4).unwrap().content(), ' ');
    }

    #[test]
    fn erase_line_left() {
        let mut g = Grid::new(5, 1);
        for c in 0..5u16 {
            g.cell_mut(0, c)
                .unwrap()
                .set_content((b'A' + c as u8) as char, 1);
        }
        g.erase_line_left(0, 2, Color::Default);
        assert_eq!(g.cell(0, 0).unwrap().content(), ' ');
        assert_eq!(g.cell(0, 2).unwrap().content(), ' ');
        assert_eq!(g.cell(0, 3).unwrap().content(), 'D');
    }

    #[test]
    fn erase_chars_within_row() {
        let mut g = Grid::new(5, 1);
        for c in 0..5u16 {
            g.cell_mut(0, c).unwrap().set_content('X', 1);
        }
        g.erase_chars(0, 1, 2, Color::Default);
        assert_eq!(g.cell(0, 0).unwrap().content(), 'X');
        assert_eq!(g.cell(0, 1).unwrap().content(), ' ');
        assert_eq!(g.cell(0, 2).unwrap().content(), ' ');
        assert_eq!(g.cell(0, 3).unwrap().content(), 'X');
    }

    // ── Insert/delete characters ────────────────────────────────────

    #[test]
    fn insert_chars_shifts_right() {
        let mut g = Grid::new(5, 1);
        for c in 0..5u16 {
            g.cell_mut(0, c)
                .unwrap()
                .set_content((b'A' + c as u8) as char, 1);
        }
        // Insert 2 blanks at col 1: A _ _ B C (D and E lost)
        g.insert_chars(0, 1, 2, Color::Default);
        assert_eq!(g.cell(0, 0).unwrap().content(), 'A');
        assert_eq!(g.cell(0, 1).unwrap().content(), ' ');
        assert_eq!(g.cell(0, 2).unwrap().content(), ' ');
        assert_eq!(g.cell(0, 3).unwrap().content(), 'B');
        assert_eq!(g.cell(0, 4).unwrap().content(), 'C');
    }

    #[test]
    fn delete_chars_shifts_left() {
        let mut g = Grid::new(5, 1);
        for c in 0..5u16 {
            g.cell_mut(0, c)
                .unwrap()
                .set_content((b'A' + c as u8) as char, 1);
        }
        // Delete 2 at col 1: A D E _ _
        g.delete_chars(0, 1, 2, Color::Default);
        assert_eq!(g.cell(0, 0).unwrap().content(), 'A');
        assert_eq!(g.cell(0, 1).unwrap().content(), 'D');
        assert_eq!(g.cell(0, 2).unwrap().content(), 'E');
        assert_eq!(g.cell(0, 3).unwrap().content(), ' ');
        assert_eq!(g.cell(0, 4).unwrap().content(), ' ');
    }

    // ── Insert/delete lines ─────────────────────────────────────────

    #[test]
    fn insert_lines_within_region() {
        let mut g = Grid::new(2, 4);
        for r in 0..4u16 {
            let ch = (b'A' + r as u8) as char;
            for c in 0..2u16 {
                g.cell_mut(r, c).unwrap().set_content(ch, 1);
            }
        }
        // Insert 1 line at row 1 within region [0, 4)
        g.insert_lines(1, 1, 0, 4, Color::Default);
        // Result: A _ B C (D lost)
        assert_eq!(g.cell(0, 0).unwrap().content(), 'A');
        assert_eq!(g.cell(1, 0).unwrap().content(), ' ');
        assert_eq!(g.cell(2, 0).unwrap().content(), 'B');
        assert_eq!(g.cell(3, 0).unwrap().content(), 'C');
    }

    #[test]
    fn delete_lines_within_region() {
        let mut g = Grid::new(2, 4);
        for r in 0..4u16 {
            let ch = (b'A' + r as u8) as char;
            for c in 0..2u16 {
                g.cell_mut(r, c).unwrap().set_content(ch, 1);
            }
        }
        // Delete 1 line at row 1 within region [0, 4)
        g.delete_lines(1, 1, 0, 4, Color::Default);
        // Result: A C D _
        assert_eq!(g.cell(0, 0).unwrap().content(), 'A');
        assert_eq!(g.cell(1, 0).unwrap().content(), 'C');
        assert_eq!(g.cell(2, 0).unwrap().content(), 'D');
        assert_eq!(g.cell(3, 0).unwrap().content(), ' ');
    }

    // ── Wide characters ─────────────────────────────────────────────

    #[test]
    fn write_wide_char_sets_two_cells() {
        let mut g = Grid::new(10, 1);
        g.write_wide_char(0, 3, '中', SgrAttrs::default());
        assert!(g.cell(0, 3).unwrap().is_wide());
        assert_eq!(g.cell(0, 3).unwrap().content(), '中');
        assert!(g.cell(0, 4).unwrap().is_wide_continuation());
    }

    #[test]
    fn write_wide_char_at_right_margin_is_noop() {
        let mut g = Grid::new(5, 1);
        // col + 1 >= cols, so no write.
        g.write_wide_char(0, 4, '中', SgrAttrs::default());
        assert_eq!(g.cell(0, 4).unwrap().content(), ' ');
    }

    #[test]
    fn overwrite_wide_continuation_clears_leading() {
        let mut g = Grid::new(10, 1);
        g.write_wide_char(0, 2, '中', SgrAttrs::default());
        // Now overwrite at col 3 (continuation of '中').
        g.write_wide_char(0, 3, '国', SgrAttrs::default());
        // The old leading cell at col 2 should be cleared.
        assert_eq!(g.cell(0, 2).unwrap().content(), ' ');
        assert!(!g.cell(0, 2).unwrap().is_wide());
        // New wide char at 3-4.
        assert!(g.cell(0, 3).unwrap().is_wide());
        assert!(g.cell(0, 4).unwrap().is_wide_continuation());
    }

    #[test]
    fn write_printable_handles_single_wide_and_zero_width_scalars() {
        let attrs = SgrAttrs::default();
        let mut g = Grid::new(8, 1);

        // single-width
        assert_eq!(g.write_printable(0, 0, 'A', attrs), 1);
        assert_eq!(g.cell(0, 0).unwrap().content(), 'A');
        assert_eq!(g.cell(0, 0).unwrap().width(), 1);

        // wide-width
        assert_eq!(g.write_printable(0, 1, '中', attrs), 2);
        assert_eq!(g.cell(0, 1).unwrap().content(), '中');
        assert!(g.cell(0, 1).unwrap().is_wide());
        assert!(g.cell(0, 2).unwrap().is_wide_continuation());

        // zero-width mark fallback: ignored (no write, no advance)
        assert_eq!(g.write_printable(0, 3, '\u{0301}', attrs), 0);
        assert_eq!(g.cell(0, 3).unwrap().content(), ' ');
    }

    #[test]
    fn write_printable_single_overwrites_wide_fixes_continuation() {
        let attrs = SgrAttrs::default();
        let mut g = Grid::new(6, 1);
        g.write_wide_char(0, 1, '中', attrs);

        assert_eq!(g.write_printable(0, 1, 'X', attrs), 1);
        assert_eq!(g.cell(0, 1).unwrap().content(), 'X');
        assert_eq!(g.cell(0, 2).unwrap().content(), ' ');
        assert!(!g.cell(0, 2).unwrap().is_wide_continuation());
    }

    // ── Resize ──────────────────────────────────────────────────────

    #[test]
    fn resize_larger_preserves_content() {
        let mut g = Grid::new(3, 2);
        g.cell_mut(0, 0).unwrap().set_content('A', 1);
        g.cell_mut(1, 2).unwrap().set_content('Z', 1);
        g.resize(5, 4);
        assert_eq!(g.cols(), 5);
        assert_eq!(g.rows(), 4);
        assert_eq!(g.cell(0, 0).unwrap().content(), 'A');
        assert_eq!(g.cell(1, 2).unwrap().content(), 'Z');
        assert_eq!(g.cell(3, 4).unwrap().content(), ' ');
    }

    #[test]
    fn resize_smaller_truncates() {
        let mut g = Grid::new(5, 5);
        g.cell_mut(4, 4).unwrap().set_content('X', 1);
        g.resize(3, 3);
        assert_eq!(g.cols(), 3);
        assert_eq!(g.rows(), 3);
        assert!(g.cell(4, 4).is_none());
    }

    #[test]
    fn resize_same_is_noop() {
        let mut g = Grid::new(10, 5);
        g.cell_mut(0, 0).unwrap().set_content('A', 1);
        g.resize(10, 5);
        assert_eq!(g.cell(0, 0).unwrap().content(), 'A');
    }

    // ── Edge cases ──────────────────────────────────────────────────

    #[test]
    fn zero_size_grid() {
        let g = Grid::new(0, 0);
        assert_eq!(g.cols(), 0);
        assert_eq!(g.rows(), 0);
        assert!(g.cell(0, 0).is_none());
    }

    #[test]
    fn one_by_one_grid() {
        let mut g = Grid::new(1, 1);
        g.cell_mut(0, 0).unwrap().set_content('X', 1);
        assert_eq!(g.cell(0, 0).unwrap().content(), 'X');
        g.erase_all(Color::Default);
        assert_eq!(g.cell(0, 0).unwrap().content(), ' ');
    }

    #[test]
    fn scroll_zero_count_is_noop() {
        let mut g = Grid::new(3, 3);
        g.cell_mut(0, 0).unwrap().set_content('A', 1);
        g.scroll_up(0, 3, 0, Color::Default);
        assert_eq!(g.cell(0, 0).unwrap().content(), 'A');
    }

    #[test]
    fn insert_chars_at_last_col() {
        let mut g = Grid::new(3, 1);
        g.cell_mut(0, 0).unwrap().set_content('A', 1);
        g.cell_mut(0, 1).unwrap().set_content('B', 1);
        g.cell_mut(0, 2).unwrap().set_content('C', 1);
        g.insert_chars(0, 2, 5, Color::Default);
        // Only 1 cell can be inserted at col 2 (col 2 is last).
        assert_eq!(g.cell(0, 0).unwrap().content(), 'A');
        assert_eq!(g.cell(0, 1).unwrap().content(), 'B');
        assert_eq!(g.cell(0, 2).unwrap().content(), ' ');
    }

    #[test]
    fn delete_chars_more_than_remaining() {
        let mut g = Grid::new(5, 1);
        for c in 0..5u16 {
            g.cell_mut(0, c).unwrap().set_content('X', 1);
        }
        g.delete_chars(0, 3, 100, Color::Default);
        assert_eq!(g.cell(0, 3).unwrap().content(), ' ');
        assert_eq!(g.cell(0, 4).unwrap().content(), ' ');
    }

    #[test]
    fn erase_out_of_bounds_is_safe() {
        let mut g = Grid::new(5, 3);
        // None of these should panic.
        g.erase_below(99, 99, Color::Default);
        g.erase_above(99, 99, Color::Default);
        g.erase_chars(99, 99, 10, Color::Default);
        g.erase_line_right(99, 99, Color::Default);
    }

    #[test]
    fn insert_lines_outside_region_is_noop() {
        let mut g = Grid::new(2, 4);
        for r in 0..4u16 {
            g.cell_mut(r, 0)
                .unwrap()
                .set_content((b'A' + r as u8) as char, 1);
        }
        // Insert at row 0, but region is [1, 3) — row 0 is outside.
        g.insert_lines(0, 1, 1, 3, Color::Default);
        assert_eq!(g.cell(0, 0).unwrap().content(), 'A');
    }

    // ── Scrollback integration ───────────────────────────────────────

    fn row_text(g: &Grid, row: u16) -> String {
        g.row_cells(row)
            .unwrap()
            .iter()
            .map(|c| c.content())
            .collect()
    }

    fn fill_grid_letters(g: &mut Grid) {
        for r in 0..g.rows() {
            let ch = (b'A' + r as u8) as char;
            for c in 0..g.cols() {
                g.cell_mut(r, c).unwrap().set_content(ch, 1);
            }
        }
    }

    #[test]
    fn scroll_up_into_pushes_to_scrollback() {
        let mut g = Grid::new(3, 4);
        fill_grid_letters(&mut g);
        let mut sb = Scrollback::new(100);
        g.scroll_up_into(0, 4, 2, &mut sb, Color::Default);
        // Rows A and B should be in scrollback.
        assert_eq!(sb.len(), 2);
        assert_eq!(
            sb.get(0)
                .unwrap()
                .cells
                .iter()
                .map(|c| c.content())
                .collect::<String>(),
            "AAA"
        );
        assert_eq!(
            sb.get(1)
                .unwrap()
                .cells
                .iter()
                .map(|c| c.content())
                .collect::<String>(),
            "BBB"
        );
        // Grid should now have C, D, blank, blank.
        assert_eq!(row_text(&g, 0), "CCC");
        assert_eq!(row_text(&g, 1), "DDD");
        assert_eq!(row_text(&g, 2), "   ");
        assert_eq!(row_text(&g, 3), "   ");
    }

    #[test]
    fn scroll_down_from_pulls_from_scrollback() {
        let mut g = Grid::new(3, 4);
        fill_grid_letters(&mut g);
        let mut sb = Scrollback::new(100);
        // Put some lines in scrollback.
        let _ = sb.push_row(&[Cell::new('X'), Cell::new('X'), Cell::new('X')], false);
        let _ = sb.push_row(&[Cell::new('Y'), Cell::new('Y'), Cell::new('Y')], false);

        g.scroll_down_from(0, 4, 2, &mut sb, Color::Default);
        // Y then X should be at top (newest popped first, placed bottom-up).
        assert_eq!(row_text(&g, 0), "XXX");
        assert_eq!(row_text(&g, 1), "YYY");
        // Original A and B shifted down.
        assert_eq!(row_text(&g, 2), "AAA");
        assert_eq!(row_text(&g, 3), "BBB");
        // Scrollback should be empty now.
        assert!(sb.is_empty());
    }

    #[test]
    fn scroll_up_into_with_scroll_region() {
        let mut g = Grid::new(3, 4);
        fill_grid_letters(&mut g);
        let mut sb = Scrollback::new(100);
        // Only scroll within region [1, 3).
        g.scroll_up_into(1, 3, 1, &mut sb, Color::Default);
        assert_eq!(sb.len(), 1);
        assert_eq!(
            sb.get(0)
                .unwrap()
                .cells
                .iter()
                .map(|c| c.content())
                .collect::<String>(),
            "BBB"
        );
        // Row 0 and 3 untouched, row 1 now has C content, row 2 blank.
        assert_eq!(row_text(&g, 0), "AAA");
        assert_eq!(row_text(&g, 1), "CCC");
        assert_eq!(row_text(&g, 2), "   ");
        assert_eq!(row_text(&g, 3), "DDD");
    }

    #[test]
    fn scroll_up_into_zero_count_is_noop() {
        let mut g = Grid::new(3, 2);
        fill_grid_letters(&mut g);
        let mut sb = Scrollback::new(100);
        g.scroll_up_into(0, 2, 0, &mut sb, Color::Default);
        assert!(sb.is_empty());
        assert_eq!(row_text(&g, 0), "AAA");
    }

    #[test]
    fn scroll_down_from_more_than_scrollback() {
        let mut g = Grid::new(3, 4);
        fill_grid_letters(&mut g);
        let mut sb = Scrollback::new(100);
        let _ = sb.push_row(&[Cell::new('Z'), Cell::new('Z'), Cell::new('Z')], false);

        // Request 3 rows from scrollback but only 1 is available.
        g.scroll_down_from(0, 4, 3, &mut sb, Color::Default);
        // scroll_down shifts A,B,C,D down by 3: only A survives at row 3.
        // Then fill from scrollback in reverse order (row 2, 1, 0):
        // Z goes to row 2, rows 0-1 remain blank (no more scrollback).
        assert_eq!(row_text(&g, 0), "   ");
        assert_eq!(row_text(&g, 1), "   ");
        assert_eq!(row_text(&g, 2), "ZZZ");
        assert_eq!(row_text(&g, 3), "AAA");
    }

    // ── Resize with scrollback ───────────────────────────────────────

    #[test]
    fn resize_with_scrollback_height_decrease_pushes_top() {
        let mut g = Grid::new(3, 4);
        fill_grid_letters(&mut g);
        let mut sb = Scrollback::new(100);
        // Cursor at row 2, shrink from 4 to 2 rows.
        let new_row = g.resize_with_scrollback(3, 2, 2, &mut sb);
        assert_eq!(g.rows(), 2);
        // 2 excess rows; cursor was at row 2, so rows 0-1 (A, B) go to scrollback.
        assert_eq!(sb.len(), 2);
        assert_eq!(row_text(&g, 0), "CCC");
        assert_eq!(row_text(&g, 1), "DDD");
        assert_eq!(new_row, 0); // cursor shifted from row 2 down to row 0
    }

    #[test]
    fn resize_with_scrollback_height_increase_pulls_back() {
        let mut g = Grid::new(3, 2);
        fill_grid_letters(&mut g); // A, B
        let mut sb = Scrollback::new(100);
        let _ = sb.push_row(&[Cell::new('X'), Cell::new('X'), Cell::new('X')], false);
        let _ = sb.push_row(&[Cell::new('Y'), Cell::new('Y'), Cell::new('Y')], false);

        // Cursor at row 1, grow from 2 to 4 rows.
        let new_row = g.resize_with_scrollback(3, 4, 1, &mut sb);
        assert_eq!(g.rows(), 4);
        // Should have pulled 2 lines from scrollback to fill top.
        assert_eq!(row_text(&g, 0), "XXX");
        assert_eq!(row_text(&g, 1), "YYY");
        assert_eq!(row_text(&g, 2), "AAA");
        assert_eq!(row_text(&g, 3), "BBB");
        assert_eq!(new_row, 3); // cursor shifted from 1 to 3
        assert!(sb.is_empty());
    }

    #[test]
    fn resize_with_scrollback_width_change() {
        let mut g = Grid::new(5, 2);
        for c in 0..5u16 {
            g.cell_mut(0, c)
                .unwrap()
                .set_content((b'A' + c as u8) as char, 1);
        }
        let mut sb = Scrollback::new(100);
        let new_row = g.resize_with_scrollback(3, 2, 0, &mut sb);
        assert_eq!(g.cols(), 3);
        // Only first 3 columns preserved.
        assert_eq!(row_text(&g, 0), "ABC");
        assert_eq!(new_row, 0);
    }

    #[test]
    fn resize_with_scrollback_same_size_is_noop() {
        let mut g = Grid::new(3, 3);
        fill_grid_letters(&mut g);
        let mut sb = Scrollback::new(100);
        let new_row = g.resize_with_scrollback(3, 3, 1, &mut sb);
        assert_eq!(new_row, 1);
        assert!(sb.is_empty());
        assert_eq!(row_text(&g, 0), "AAA");
    }

    #[test]
    fn resize_with_scrollback_cursor_at_top() {
        let mut g = Grid::new(3, 4);
        fill_grid_letters(&mut g);
        let mut sb = Scrollback::new(100);
        // Cursor at row 0, shrink to 2 rows.
        // Cannot push any rows above cursor (rows_above_cursor = 0).
        let new_row = g.resize_with_scrollback(3, 2, 0, &mut sb);
        assert_eq!(g.rows(), 2);
        assert!(sb.is_empty()); // nothing pushed since cursor is at top
        assert_eq!(row_text(&g, 0), "AAA");
        assert_eq!(row_text(&g, 1), "BBB");
        assert_eq!(new_row, 0);
    }

    #[test]
    fn resize_storm_deterministic() {
        // Rapidly resize up and down, verify invariants.
        let mut g = Grid::new(10, 5);
        let mut sb = Scrollback::new(1000);
        for c in 0..10u16 {
            g.cell_mut(0, c)
                .unwrap()
                .set_content((b'0' + (c % 10) as u8) as char, 1);
        }
        let mut cursor_row: u16 = 2;

        // Grow.
        cursor_row = g.resize_with_scrollback(10, 8, cursor_row, &mut sb);
        assert_eq!(g.rows(), 8);

        // Shrink.
        cursor_row = g.resize_with_scrollback(10, 3, cursor_row, &mut sb);
        assert_eq!(g.rows(), 3);

        // Grow back.
        cursor_row = g.resize_with_scrollback(10, 8, cursor_row, &mut sb);
        assert_eq!(g.rows(), 8);

        // Cursor should always be in bounds.
        assert!(cursor_row < g.rows());
    }

    // ── fill_all (DECALN) ──────────────────────────────────────────────

    #[test]
    fn fill_all_fills_every_cell() {
        let mut g = Grid::new(5, 3);
        g.cell_mut(0, 0).unwrap().set_content('X', 1);
        g.fill_all('E');
        for r in 0..3u16 {
            for c in 0..5u16 {
                assert_eq!(g.cell(r, c).unwrap().content(), 'E');
            }
        }
    }

    #[test]
    fn fill_all_on_empty_grid() {
        let mut g = Grid::new(3, 2);
        g.fill_all('Z');
        assert_eq!(g.cell(0, 0).unwrap().content(), 'Z');
        assert_eq!(g.cell(1, 2).unwrap().content(), 'Z');
    }
}

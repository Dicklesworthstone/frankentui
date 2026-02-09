#![forbid(unsafe_code)]
//! Selection model + copy extraction for terminal grid + scrollback.
//!
//! This is a pure data/logic layer:
//! - no I/O
//! - deterministic output given the same buffer state
//!
//! Selection coordinates are defined over the *combined* buffer:
//! `0..scrollback.len()` are scrollback lines (oldest → newest), followed by
//! `grid.rows()` viewport lines (top → bottom).

use crate::cell::Cell;
use crate::grid::Grid;
use crate::scrollback::Scrollback;

/// A cell position in the combined buffer (scrollback + viewport).
///
/// - `line`: 0-indexed line index in the combined buffer.
/// - `col`:  0-indexed column in the current viewport coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferPos {
    pub line: u32,
    pub col: u16,
}

impl BufferPos {
    #[must_use]
    pub const fn new(line: u32, col: u16) -> Self {
        Self { line, col }
    }

    /// Convert a viewport (row, col) into a combined-buffer position.
    #[must_use]
    pub fn from_viewport(scrollback_lines: usize, row: u16, col: u16) -> Self {
        Self {
            line: scrollback_lines as u32 + row as u32,
            col,
        }
    }
}

/// Inclusive selection over the combined buffer.
///
/// Invariant: after normalization, `(start.line, start.col) <= (end.line, end.col)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: BufferPos,
    pub end: BufferPos,
}

impl Selection {
    #[must_use]
    pub const fn new(start: BufferPos, end: BufferPos) -> Self {
        Self { start, end }
    }

    /// Normalize start/end ordering.
    #[must_use]
    pub fn normalized(self) -> Self {
        if (self.start.line, self.start.col) <= (self.end.line, self.end.col) {
            self
        } else {
            Self {
                start: self.end,
                end: self.start,
            }
        }
    }

    /// Select exactly one character cell (wide chars expand to include both columns).
    #[must_use]
    pub fn char_at(pos: BufferPos, grid: &Grid, scrollback: &Scrollback) -> Self {
        let cols = grid.cols();
        if cols == 0 {
            return Self::new(pos, pos);
        }

        let line = pos.line;
        let col = pos.col.min(cols.saturating_sub(1));
        let lead_col = normalize_to_wide_lead(line, col, grid, scrollback);
        let end_col = wide_end_col(line, lead_col, grid, scrollback, cols);
        Self::new(
            BufferPos::new(line, lead_col),
            BufferPos::new(line, end_col),
        )
    }

    /// Select the whole logical line (all columns).
    #[must_use]
    pub fn line_at(line: u32, grid: &Grid, scrollback: &Scrollback) -> Self {
        let cols = grid.cols();
        if cols == 0 || total_lines(grid, scrollback) == 0 {
            let p = BufferPos::new(line, 0);
            return Self::new(p, p);
        }
        let max_line = total_lines(grid, scrollback).saturating_sub(1);
        let line = line.min(max_line);
        Self::new(
            BufferPos::new(line, 0),
            BufferPos::new(line, cols.saturating_sub(1)),
        )
    }

    /// Select a "word" at the given position.
    ///
    /// Heuristics: contiguous run of `is_word_char` characters, or contiguous
    /// whitespace if the clicked cell is whitespace.
    #[must_use]
    pub fn word_at(pos: BufferPos, grid: &Grid, scrollback: &Scrollback) -> Self {
        let cols = grid.cols();
        if cols == 0 || total_lines(grid, scrollback) == 0 {
            return Self::new(pos, pos);
        }

        let max_line = total_lines(grid, scrollback).saturating_sub(1);
        let line = pos.line.min(max_line);
        let col = pos.col.min(cols.saturating_sub(1));
        let col = normalize_to_wide_lead(line, col, grid, scrollback);

        let ch = cell_char(line, col, grid, scrollback).unwrap_or(' ');
        let target_class = classify_char(ch);

        // Seed selection with the current char span.
        let mut start_col = col;
        let mut end_col = wide_end_col(line, col, grid, scrollback, cols);

        // Expand left.
        while start_col > 0 {
            let probe = start_col - 1;
            let probe = normalize_to_wide_lead(line, probe, grid, scrollback);
            let ch = cell_char(line, probe, grid, scrollback).unwrap_or(' ');
            if classify_char(ch) != target_class {
                break;
            }
            start_col = probe;
        }

        // Expand right.
        loop {
            let next = end_col.saturating_add(1);
            if next >= cols {
                break;
            }
            let next = normalize_to_wide_lead(line, next, grid, scrollback);
            let ch = cell_char(line, next, grid, scrollback).unwrap_or(' ');
            if classify_char(ch) != target_class {
                break;
            }
            end_col = wide_end_col(line, next, grid, scrollback, cols);
            if end_col >= cols.saturating_sub(1) {
                break;
            }
        }

        Self::new(
            BufferPos::new(line, start_col),
            BufferPos::new(line, end_col),
        )
    }

    /// Extract selected text from the buffer (scrollback + viewport).
    ///
    /// - Wide continuation cells are skipped (wide chars appear once).
    /// - Trailing spaces on each emitted line are trimmed.
    /// - Soft-wrapped scrollback lines (where the *next* line has `wrapped=true`)
    ///   are joined without inserting a newline.
    #[must_use]
    pub fn extract_text(&self, grid: &Grid, scrollback: &Scrollback) -> String {
        let cols = grid.cols();
        if cols == 0 {
            return String::new();
        }

        let total = total_lines(grid, scrollback);
        if total == 0 {
            return String::new();
        }

        let sel = self.normalized();
        let start_line = sel.start.line.min(total.saturating_sub(1));
        let end_line = sel.end.line.min(total.saturating_sub(1));

        let mut out = String::new();

        for line in start_line..=end_line {
            let sc = if line == start_line {
                sel.start.col.min(cols.saturating_sub(1))
            } else {
                0
            };
            let ec = if line == end_line {
                sel.end.col.min(cols.saturating_sub(1))
            } else {
                cols.saturating_sub(1)
            };

            let mut line_buf = String::new();
            if sc <= ec {
                for col in sc..=ec {
                    if let Some(cell) = cell_at(line, col, grid, scrollback) {
                        if cell.is_wide_continuation() {
                            continue;
                        }
                        line_buf.push(cell.content());
                    } else {
                        line_buf.push(' ');
                    }
                }
            }
            trim_trailing_spaces(&mut line_buf);
            out.push_str(&line_buf);

            if line != end_line && should_insert_newline(line + 1, scrollback) {
                out.push('\n');
            }
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Whitespace,
    Other,
}

fn classify_char(ch: char) -> CharClass {
    if ch.is_whitespace() {
        return CharClass::Whitespace;
    }
    if is_word_char(ch) {
        return CharClass::Word;
    }
    CharClass::Other
}

fn is_word_char(ch: char) -> bool {
    // Tuned for "code + paths" selection.
    //
    // - Identifiers: letters/digits/underscore
    // - Paths/URLs:  - . / \\ : @
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\' | ':' | '@')
}

fn trim_trailing_spaces(s: &mut String) {
    while s.ends_with(' ') {
        s.pop();
    }
}

fn total_lines(grid: &Grid, scrollback: &Scrollback) -> u32 {
    (scrollback.len() + grid.rows() as usize) as u32
}

fn should_insert_newline(next_line: u32, scrollback: &Scrollback) -> bool {
    let sb_len = scrollback.len() as u32;
    if next_line < sb_len {
        // wrapped=true means "this line continues the previous line".
        return !scrollback
            .get(next_line as usize)
            .map(|l| l.wrapped)
            .unwrap_or(false);
    }
    true
}

fn cell_at<'a>(
    line: u32,
    col: u16,
    grid: &'a Grid,
    scrollback: &'a Scrollback,
) -> Option<&'a Cell> {
    let sb_len = scrollback.len() as u32;
    if line < sb_len {
        scrollback
            .get(line as usize)
            .and_then(|l| l.cells.get(col as usize))
    } else {
        let row = (line - sb_len) as u16;
        grid.cell(row, col)
    }
}

fn cell_char(line: u32, col: u16, grid: &Grid, scrollback: &Scrollback) -> Option<char> {
    cell_at(line, col, grid, scrollback).map(Cell::content)
}

fn normalize_to_wide_lead(line: u32, col: u16, grid: &Grid, scrollback: &Scrollback) -> u16 {
    if col == 0 {
        return col;
    }
    let Some(cell) = cell_at(line, col, grid, scrollback) else {
        return col;
    };
    if cell.is_wide_continuation() {
        col - 1
    } else {
        col
    }
}

fn wide_end_col(line: u32, lead_col: u16, grid: &Grid, scrollback: &Scrollback, cols: u16) -> u16 {
    let Some(cell) = cell_at(line, lead_col, grid, scrollback) else {
        return lead_col;
    };
    if cell.is_wide() {
        // Include the continuation column when available.
        lead_col.saturating_add(1).min(cols.saturating_sub(1))
    } else {
        lead_col
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;

    fn grid_from_lines(cols: u16, lines: &[&str]) -> Grid {
        let rows = lines.len() as u16;
        let mut g = Grid::new(cols, rows);
        for (r, text) in lines.iter().enumerate() {
            for (c, ch) in text.chars().enumerate() {
                if c >= cols as usize {
                    break;
                }
                g.cell_mut(r as u16, c as u16).unwrap().set_content(ch, 1);
            }
        }
        g
    }

    fn scrollback_from_lines(lines: &[(&str, bool)]) -> Scrollback {
        let mut sb = Scrollback::new(64);
        for (text, wrapped) in lines {
            let cells: Vec<Cell> = text.chars().map(Cell::new).collect();
            sb.push_row(&cells, *wrapped);
        }
        sb
    }

    #[test]
    fn extract_joins_soft_wrapped_scrollback_lines_without_newline() {
        let sb = scrollback_from_lines(&[("foo", false), ("bar", true)]);
        let grid = grid_from_lines(10, &["baz"]);
        let sel = Selection::new(BufferPos::new(0, 0), BufferPos::new(1, 2));
        assert_eq!(sel.extract_text(&grid, &sb), "foobar");
    }

    #[test]
    fn extract_spans_scrollback_and_viewport_with_newlines() {
        let sb = scrollback_from_lines(&[("aa", false), ("bb", false)]);
        let grid = grid_from_lines(10, &["cc", "dd"]);
        let start = BufferPos::new(1, 0); // "bb"
        let end = BufferPos::new(3, 1); // "dd" (viewport row 1)
        let sel = Selection::new(start, end);
        assert_eq!(sel.extract_text(&grid, &sb), "bb\ncc\ndd");
    }

    #[test]
    fn word_selection_is_tuned_for_paths() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(40, &["foo-bar/baz"]);
        let sel = Selection::word_at(BufferPos::new(0, 4), &grid, &sb);
        assert_eq!(sel.extract_text(&grid, &sb), "foo-bar/baz");
    }

    #[test]
    fn word_selection_stops_at_whitespace() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(40, &["abc def"]);
        let sel = Selection::word_at(BufferPos::new(0, 5), &grid, &sb);
        assert_eq!(sel.extract_text(&grid, &sb), "def");
    }

    #[test]
    fn selection_coordinates_stay_valid_after_resize_with_scrollback_pull() {
        let mut sb = scrollback_from_lines(&[("top", false)]);
        let mut grid = grid_from_lines(10, &["aa", "bb"]);

        // Grow height: should pull the newest scrollback line into the top row.
        let _new_cursor_row = grid.resize_with_scrollback(10, 3, 1, &mut sb);
        assert_eq!(sb.len(), 0);
        assert_eq!(grid.rows(), 3);

        let start = BufferPos::from_viewport(sb.len(), 0, 0);
        let end = BufferPos::from_viewport(sb.len(), 0, 2);
        let sel = Selection::new(start, end);
        assert_eq!(sel.extract_text(&grid, &sb), "top");
    }

    // ── BufferPos tests ─────────────────────────────────────────────

    #[test]
    fn buffer_pos_new_stores_line_and_col() {
        let pos = BufferPos::new(42, 7);
        assert_eq!(pos.line, 42);
        assert_eq!(pos.col, 7);
    }

    #[test]
    fn buffer_pos_from_viewport_adds_scrollback_offset() {
        let pos = BufferPos::from_viewport(10, 3, 5);
        assert_eq!(pos.line, 13); // scrollback_lines(10) + row(3)
        assert_eq!(pos.col, 5);
    }

    #[test]
    fn buffer_pos_from_viewport_zero_scrollback() {
        let pos = BufferPos::from_viewport(0, 0, 0);
        assert_eq!(pos.line, 0);
        assert_eq!(pos.col, 0);
    }

    // ── Selection::normalized tests ─────────────────────────────────

    #[test]
    fn normalized_preserves_already_ordered_selection() {
        let sel = Selection::new(BufferPos::new(0, 0), BufferPos::new(1, 5));
        let norm = sel.normalized();
        assert_eq!(norm.start, sel.start);
        assert_eq!(norm.end, sel.end);
    }

    #[test]
    fn normalized_swaps_reversed_selection() {
        let sel = Selection::new(BufferPos::new(3, 10), BufferPos::new(1, 2));
        let norm = sel.normalized();
        assert_eq!(norm.start, BufferPos::new(1, 2));
        assert_eq!(norm.end, BufferPos::new(3, 10));
    }

    #[test]
    fn normalized_swaps_same_line_reversed_cols() {
        let sel = Selection::new(BufferPos::new(5, 8), BufferPos::new(5, 2));
        let norm = sel.normalized();
        assert_eq!(norm.start.col, 2);
        assert_eq!(norm.end.col, 8);
    }

    #[test]
    fn normalized_identity_when_equal() {
        let pos = BufferPos::new(3, 3);
        let sel = Selection::new(pos, pos);
        let norm = sel.normalized();
        assert_eq!(norm.start, pos);
        assert_eq!(norm.end, pos);
    }

    // ── Selection::char_at tests ────────────────────────────────────

    #[test]
    fn char_at_regular_char_selects_single_cell() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(10, &["hello"]);
        let sel = Selection::char_at(BufferPos::new(0, 2), &grid, &sb);
        assert_eq!(sel.start.col, 2);
        assert_eq!(sel.end.col, 2);
        assert_eq!(sel.extract_text(&grid, &sb), "l");
    }

    #[test]
    fn char_at_wide_char_expands_to_two_columns() {
        let sb = Scrollback::new(0);
        let mut grid = Grid::new(10, 1);
        let (lead, cont) = Cell::wide('中', crate::cell::SgrAttrs::default());
        *grid.cell_mut(0, 2).unwrap() = lead;
        *grid.cell_mut(0, 3).unwrap() = cont;

        // Click on leading cell
        let sel = Selection::char_at(BufferPos::new(0, 2), &grid, &sb);
        assert_eq!(sel.start.col, 2);
        assert_eq!(sel.end.col, 3);

        // Click on continuation cell → should snap back to lead
        let sel = Selection::char_at(BufferPos::new(0, 3), &grid, &sb);
        assert_eq!(sel.start.col, 2);
        assert_eq!(sel.end.col, 3);
    }

    #[test]
    fn char_at_zero_cols_grid_returns_degenerate() {
        let sb = Scrollback::new(0);
        let grid = Grid::new(0, 1);
        let pos = BufferPos::new(0, 0);
        let sel = Selection::char_at(pos, &grid, &sb);
        assert_eq!(sel.start, pos);
        assert_eq!(sel.end, pos);
    }

    #[test]
    fn char_at_clamps_col_beyond_grid_width() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(5, &["abcde"]);
        // Col 99 should be clamped to max valid col (4)
        let sel = Selection::char_at(BufferPos::new(0, 99), &grid, &sb);
        assert!(sel.start.col <= 4);
        assert!(sel.end.col <= 4);
    }

    // ── Selection::line_at tests ────────────────────────────────────

    #[test]
    fn line_at_selects_full_row_width() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(8, &["hello   "]);
        let sel = Selection::line_at(0, &grid, &sb);
        assert_eq!(sel.start.col, 0);
        assert_eq!(sel.end.col, 7); // cols - 1
        assert_eq!(sel.start.line, 0);
        assert_eq!(sel.end.line, 0);
    }

    #[test]
    fn line_at_clamps_beyond_total_lines() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(10, &["only"]);
        let sel = Selection::line_at(999, &grid, &sb);
        // Clamped to line 0 (the only line)
        assert_eq!(sel.start.line, 0);
        assert_eq!(sel.end.line, 0);
    }

    #[test]
    fn line_at_scrollback_line() {
        let sb = scrollback_from_lines(&[("sb-line", false)]);
        let grid = grid_from_lines(10, &["vp-line"]);
        let sel = Selection::line_at(0, &grid, &sb);
        assert_eq!(sel.start.line, 0);
        assert_eq!(sel.extract_text(&grid, &sb), "sb-line");
    }

    #[test]
    fn line_at_zero_cols_grid() {
        let sb = Scrollback::new(0);
        let grid = Grid::new(0, 1);
        let sel = Selection::line_at(0, &grid, &sb);
        assert_eq!(sel.start, sel.end);
    }

    // ── Selection::word_at additional tests ──────────────────────────

    #[test]
    fn word_at_punctuation_boundary() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(20, &["hello(world)"]);
        let sel = Selection::word_at(BufferPos::new(0, 0), &grid, &sb);
        assert_eq!(sel.extract_text(&grid, &sb), "hello");
    }

    #[test]
    fn word_at_selects_whitespace_run() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(20, &["a   b"]);
        let sel = Selection::word_at(BufferPos::new(0, 2), &grid, &sb);
        // Whitespace run spans cols 1..3 (between 'a' at 0 and 'b' at 4).
        // extract_text trims trailing spaces, so verify boundaries directly.
        assert_eq!(sel.start.col, 1);
        assert_eq!(sel.end.col, 3);
    }

    #[test]
    fn word_at_single_char_line() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(5, &["x"]);
        let sel = Selection::word_at(BufferPos::new(0, 0), &grid, &sb);
        assert_eq!(sel.extract_text(&grid, &sb), "x");
    }

    #[test]
    fn word_at_empty_grid() {
        let sb = Scrollback::new(0);
        let grid = Grid::new(0, 0);
        let pos = BufferPos::new(0, 0);
        let sel = Selection::word_at(pos, &grid, &sb);
        assert_eq!(sel.start, pos);
        assert_eq!(sel.end, pos);
    }

    #[test]
    fn word_at_url_characters() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(40, &["see https://example.com:8080/path ok"]);
        let sel = Selection::word_at(BufferPos::new(0, 10), &grid, &sb);
        assert_eq!(
            sel.extract_text(&grid, &sb),
            "https://example.com:8080/path"
        );
    }

    // ── Selection::extract_text additional tests ────────────────────

    #[test]
    fn extract_text_trims_trailing_spaces() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(10, &["hi        "]);
        let sel = Selection::new(BufferPos::new(0, 0), BufferPos::new(0, 9));
        assert_eq!(sel.extract_text(&grid, &sb), "hi");
    }

    #[test]
    fn extract_text_empty_grid_returns_empty() {
        let sb = Scrollback::new(0);
        let grid = Grid::new(0, 0);
        let sel = Selection::new(BufferPos::new(0, 0), BufferPos::new(0, 0));
        assert_eq!(sel.extract_text(&grid, &sb), "");
    }

    #[test]
    fn extract_text_reversed_selection_still_works() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(10, &["abcdef"]);
        // Reversed: end before start
        let sel = Selection::new(BufferPos::new(0, 4), BufferPos::new(0, 1));
        assert_eq!(sel.extract_text(&grid, &sb), "bcde");
    }

    #[test]
    fn extract_text_single_cell() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(5, &["hello"]);
        let sel = Selection::new(BufferPos::new(0, 2), BufferPos::new(0, 2));
        assert_eq!(sel.extract_text(&grid, &sb), "l");
    }

    #[test]
    fn extract_text_wide_char_not_doubled() {
        let sb = Scrollback::new(0);
        let mut grid = Grid::new(10, 1);
        let (lead, cont) = Cell::wide('漢', crate::cell::SgrAttrs::default());
        *grid.cell_mut(0, 0).unwrap() = lead;
        *grid.cell_mut(0, 1).unwrap() = cont;
        grid.cell_mut(0, 2).unwrap().set_content('x', 1);

        let sel = Selection::new(BufferPos::new(0, 0), BufferPos::new(0, 2));
        let text = sel.extract_text(&grid, &sb);
        // Wide char appears once, continuation cell skipped
        assert_eq!(text, "漢x");
    }

    #[test]
    fn extract_text_multiline_with_trailing_trim() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(10, &["abc       ", "def       "]);
        let sel = Selection::new(BufferPos::new(0, 0), BufferPos::new(1, 9));
        assert_eq!(sel.extract_text(&grid, &sb), "abc\ndef");
    }

    #[test]
    fn extract_text_out_of_bounds_clamped() {
        let sb = Scrollback::new(0);
        let grid = grid_from_lines(5, &["hi"]);
        // Selection extends beyond grid bounds
        let sel = Selection::new(BufferPos::new(0, 0), BufferPos::new(99, 99));
        // Should not panic; clamped to last valid line
        let text = sel.extract_text(&grid, &sb);
        assert!(text.contains("hi"));
    }

    // ── Helper function tests ───────────────────────────────────────

    #[test]
    fn classify_char_word_letters_digits_underscore() {
        assert_eq!(classify_char('a'), CharClass::Word);
        assert_eq!(classify_char('Z'), CharClass::Word);
        assert_eq!(classify_char('5'), CharClass::Word);
        assert_eq!(classify_char('_'), CharClass::Word);
    }

    #[test]
    fn classify_char_word_path_chars() {
        assert_eq!(classify_char('-'), CharClass::Word);
        assert_eq!(classify_char('.'), CharClass::Word);
        assert_eq!(classify_char('/'), CharClass::Word);
        assert_eq!(classify_char('\\'), CharClass::Word);
        assert_eq!(classify_char(':'), CharClass::Word);
        assert_eq!(classify_char('@'), CharClass::Word);
    }

    #[test]
    fn classify_char_whitespace() {
        assert_eq!(classify_char(' '), CharClass::Whitespace);
        assert_eq!(classify_char('\t'), CharClass::Whitespace);
        assert_eq!(classify_char('\n'), CharClass::Whitespace);
    }

    #[test]
    fn classify_char_other_punctuation() {
        assert_eq!(classify_char('('), CharClass::Other);
        assert_eq!(classify_char(')'), CharClass::Other);
        assert_eq!(classify_char('{'), CharClass::Other);
        assert_eq!(classify_char('!'), CharClass::Other);
        assert_eq!(classify_char('#'), CharClass::Other);
    }

    #[test]
    fn is_word_char_accepts_identifiers_and_paths() {
        assert!(is_word_char('a'));
        assert!(is_word_char('0'));
        assert!(is_word_char('_'));
        assert!(is_word_char('-'));
        assert!(is_word_char('.'));
        assert!(is_word_char('/'));
        assert!(is_word_char('\\'));
        assert!(is_word_char(':'));
        assert!(is_word_char('@'));
    }

    #[test]
    fn is_word_char_rejects_punctuation_and_whitespace() {
        assert!(!is_word_char(' '));
        assert!(!is_word_char('('));
        assert!(!is_word_char(')'));
        assert!(!is_word_char('{'));
        assert!(!is_word_char('\t'));
        assert!(!is_word_char('!'));
    }

    #[test]
    fn trim_trailing_spaces_removes_only_trailing() {
        let mut s = String::from("  hello   ");
        trim_trailing_spaces(&mut s);
        assert_eq!(s, "  hello");
    }

    #[test]
    fn trim_trailing_spaces_noop_for_no_trailing() {
        let mut s = String::from("hello");
        trim_trailing_spaces(&mut s);
        assert_eq!(s, "hello");
    }

    #[test]
    fn trim_trailing_spaces_empties_all_spaces() {
        let mut s = String::from("   ");
        trim_trailing_spaces(&mut s);
        assert_eq!(s, "");
    }
}

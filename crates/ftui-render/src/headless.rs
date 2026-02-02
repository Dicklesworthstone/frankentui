#![forbid(unsafe_code)]

//! Headless terminal for CI testing.
//!
//! `HeadlessTerm` wraps [`TerminalModel`] to provide a high-level test harness
//! that works without a real terminal or PTY. It is designed for:
//!
//! - **CI environments** where PTY tests are slow or unavailable
//! - **Snapshot testing** with human-readable diff output
//! - **Render pipeline verification** by feeding presenter output through
//!   the terminal model and checking the result
//!
//! # Example
//!
//! ```
//! use ftui_render::headless::HeadlessTerm;
//!
//! let mut term = HeadlessTerm::new(20, 5);
//! term.process(b"\x1b[1;1HHello, world!");
//! assert_eq!(term.row_text(0), "Hello, world!");
//!
//! term.assert_matches(&[
//!     "Hello, world!",
//!     "",
//!     "",
//!     "",
//!     "",
//! ]);
//! ```

use crate::terminal_model::TerminalModel;
use std::fmt;
use std::io;
use std::path::Path;

/// A headless terminal for testing without real terminal I/O.
///
/// Processes ANSI escape sequences through [`TerminalModel`] and provides
/// assertion helpers, snapshot comparison, and export capabilities.
#[derive(Debug)]
pub struct HeadlessTerm {
    model: TerminalModel,
    captured_output: Vec<u8>,
}

impl HeadlessTerm {
    /// Create a new headless terminal with the given dimensions.
    ///
    /// # Panics
    ///
    /// Panics if width or height is 0.
    pub fn new(width: u16, height: u16) -> Self {
        assert!(width > 0, "width must be > 0");
        assert!(height > 0, "height must be > 0");
        Self {
            model: TerminalModel::new(width as usize, height as usize),
            captured_output: Vec::new(),
        }
    }

    /// Terminal width in columns.
    pub fn width(&self) -> u16 {
        self.model.width() as u16
    }

    /// Terminal height in rows.
    pub fn height(&self) -> u16 {
        self.model.height() as u16
    }

    /// Current cursor position as (column, row), 0-indexed.
    pub fn cursor(&self) -> (u16, u16) {
        let (x, y) = self.model.cursor();
        (x as u16, y as u16)
    }

    /// Process raw bytes through the terminal emulator.
    ///
    /// Bytes are parsed as ANSI escape sequences and applied to the
    /// internal grid, just as a real terminal would.
    pub fn process(&mut self, bytes: &[u8]) {
        self.captured_output.extend_from_slice(bytes);
        self.model.process(bytes);
    }

    /// Get the text content of a single row, trimmed of trailing spaces.
    ///
    /// Returns an empty string for out-of-bounds rows.
    pub fn row_text(&self, row: usize) -> String {
        self.model.row_text(row).unwrap_or_default()
    }

    /// Get all rows as text, trimmed of trailing spaces.
    pub fn screen_text(&self) -> Vec<String> {
        (0..self.model.height())
            .map(|y| self.model.row_text(y).unwrap_or_default())
            .collect()
    }

    /// Get all rows as a single string joined by newlines.
    pub fn screen_string(&self) -> String {
        self.screen_text().join("\n")
    }

    /// Access the underlying `TerminalModel` for advanced queries.
    pub fn model(&self) -> &TerminalModel {
        &self.model
    }

    /// Access all captured output bytes (everything passed to `process`).
    pub fn captured_output(&self) -> &[u8] {
        &self.captured_output
    }

    /// Reset the terminal to its initial state (blank screen, cursor at origin).
    pub fn reset(&mut self) {
        self.model.reset();
        self.captured_output.clear();
    }

    // --- Assertion helpers ---

    /// Assert that the screen content matches the expected lines exactly.
    ///
    /// Trailing spaces in both actual and expected lines are trimmed before
    /// comparison. The number of expected lines must match the terminal height.
    ///
    /// # Panics
    ///
    /// Panics with a human-readable diff if the content doesn't match.
    pub fn assert_matches(&self, expected: &[&str]) {
        let actual = self.screen_text();

        assert_eq!(
            actual.len(),
            expected.len(),
            "HeadlessTerm: line count mismatch: got {} lines, expected {} lines\n\
             Hint: expected slice length must equal terminal height ({})",
            actual.len(),
            expected.len(),
            self.height(),
        );

        let mismatches: Vec<LineDiff> = actual
            .iter()
            .zip(expected.iter())
            .enumerate()
            .filter_map(|(i, (got, want))| {
                let want_trimmed = want.trim_end();
                if got.as_str() != want_trimmed {
                    Some(LineDiff {
                        line: i,
                        got: got.clone(),
                        want: want_trimmed.to_string(),
                    })
                } else {
                    None
                }
            })
            .collect();

        if !mismatches.is_empty() {
            panic!(
                "HeadlessTerm: screen content mismatch\n{}",
                format_diff(&mismatches),
            );
        }
    }

    /// Assert that a specific row matches the expected text.
    ///
    /// Trailing spaces are trimmed before comparison.
    ///
    /// # Panics
    ///
    /// Panics if the row content doesn't match.
    pub fn assert_row(&self, row: usize, expected: &str) {
        let actual = self.row_text(row);
        let expected_trimmed = expected.trim_end();
        assert_eq!(
            actual, expected_trimmed,
            "HeadlessTerm: row {row} mismatch\n  got:  {actual:?}\n  want: {expected_trimmed:?}",
        );
    }

    /// Assert that the cursor is at the expected position (column, row), 0-indexed.
    ///
    /// # Panics
    ///
    /// Panics if the cursor position doesn't match.
    pub fn assert_cursor(&self, col: u16, row: u16) {
        let (actual_col, actual_row) = self.cursor();
        assert_eq!(
            (actual_col, actual_row),
            (col, row),
            "HeadlessTerm: cursor position mismatch\n  got:  ({actual_col}, {actual_row})\n  want: ({col}, {row})",
        );
    }

    /// Compare screen content with expected lines and return the diff.
    ///
    /// Returns `None` if the content matches exactly.
    pub fn diff(&self, expected: &[&str]) -> Option<ScreenDiff> {
        let actual = self.screen_text();
        let mismatches: Vec<LineDiff> = actual
            .iter()
            .zip(expected.iter())
            .enumerate()
            .filter_map(|(i, (got, want))| {
                let want_trimmed = want.trim_end();
                if got.as_str() != want_trimmed {
                    Some(LineDiff {
                        line: i,
                        got: got.clone(),
                        want: want_trimmed.to_string(),
                    })
                } else {
                    None
                }
            })
            .collect();

        let line_count_mismatch = actual.len() != expected.len();

        if mismatches.is_empty() && !line_count_mismatch {
            None
        } else {
            Some(ScreenDiff {
                actual_lines: actual.len(),
                expected_lines: expected.len(),
                mismatches,
            })
        }
    }

    // --- Export ---

    /// Export the screen content to a file for debugging.
    ///
    /// Writes a human-readable text representation including:
    /// - Terminal dimensions
    /// - Cursor position
    /// - Screen content (with line numbers)
    /// - Captured output size
    pub fn export(&self, path: &Path) -> io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(path)?;

        writeln!(file, "=== HeadlessTerm Export ===")?;
        writeln!(file, "Size: {}x{}", self.width(), self.height())?;
        let (cx, cy) = self.cursor();
        writeln!(file, "Cursor: ({cx}, {cy})")?;
        writeln!(
            file,
            "Captured output: {} bytes",
            self.captured_output.len()
        )?;
        writeln!(file)?;
        writeln!(file, "--- Screen Content ---")?;

        for y in 0..self.model.height() {
            let text = self.row_text(y);
            writeln!(file, "{y:3}| {text}")?;
        }

        writeln!(file)?;
        writeln!(file, "--- ANSI Dump ---")?;
        writeln!(
            file,
            "{}",
            TerminalModel::dump_sequences(&self.captured_output)
        )?;

        Ok(())
    }

    /// Export the screen content as a formatted string (for inline debugging).
    pub fn export_string(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("{}x{}", self.width(), self.height()));
        let (cx, cy) = self.cursor();
        out.push_str(&format!(" cursor=({cx},{cy})\n"));

        for y in 0..self.model.height() {
            let text = self.row_text(y);
            out.push_str(&format!("{y:3}| {text}\n"));
        }
        out
    }
}

/// A single line difference in a screen comparison.
#[derive(Debug, Clone)]
pub struct LineDiff {
    /// 0-based line index.
    pub line: usize,
    /// Actual content.
    pub got: String,
    /// Expected content.
    pub want: String,
}

/// Result of comparing screen content with expected lines.
#[derive(Debug, Clone)]
pub struct ScreenDiff {
    /// Number of lines in the actual screen.
    pub actual_lines: usize,
    /// Number of lines in the expected slice.
    pub expected_lines: usize,
    /// Per-line mismatches.
    pub mismatches: Vec<LineDiff>,
}

impl fmt::Display for ScreenDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.actual_lines != self.expected_lines {
            writeln!(
                f,
                "Line count: got {}, expected {}",
                self.actual_lines, self.expected_lines,
            )?;
        }
        write!(f, "{}", format_diff(&self.mismatches))
    }
}

fn format_diff(mismatches: &[LineDiff]) -> String {
    let mut out = String::new();
    for d in mismatches {
        out.push_str(&format!("  line {}:\n", d.line));
        out.push_str(&format!("    got:  {:?}\n", d.got));
        out.push_str(&format!("    want: {:?}\n", d.want));

        // Character-level diff hint
        let diff_col = d.got.chars().zip(d.want.chars()).position(|(a, b)| a != b);
        if let Some(col) = diff_col {
            out.push_str(&format!("    first difference at column {col}\n"));
        } else if d.got.len() != d.want.len() {
            let shorter = d.got.len().min(d.want.len());
            out.push_str(&format!(
                "    diverges at column {shorter} (length difference)\n"
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_blank_screen() {
        let term = HeadlessTerm::new(80, 24);
        assert_eq!(term.width(), 80);
        assert_eq!(term.height(), 24);
        assert_eq!(term.cursor(), (0, 0));

        let text = term.screen_text();
        assert_eq!(text.len(), 24);
        assert!(text.iter().all(|line| line.is_empty()));
    }

    #[test]
    fn process_writes_text() {
        let mut term = HeadlessTerm::new(20, 5);
        term.process(b"Hello, world!");
        assert_eq!(term.row_text(0), "Hello, world!");
        assert_eq!(term.cursor(), (13, 0));
    }

    #[test]
    fn process_cup_and_text() {
        let mut term = HeadlessTerm::new(20, 5);
        term.process(b"\x1b[2;3HTest"); // Row 2, Col 3 (1-indexed)
        assert_eq!(term.row_text(1), "  Test");
        assert_eq!(term.cursor(), (6, 1));
    }

    #[test]
    fn screen_text_returns_all_rows() {
        let mut term = HeadlessTerm::new(10, 3);
        term.process(b"\x1b[1;1HLine 1");
        term.process(b"\x1b[2;1HLine 2");
        term.process(b"\x1b[3;1HLine 3");

        let text = term.screen_text();
        assert_eq!(text, vec!["Line 1", "Line 2", "Line 3"]);
    }

    #[test]
    fn screen_string_joins_with_newlines() {
        let mut term = HeadlessTerm::new(10, 3);
        term.process(b"\x1b[1;1HAB");
        term.process(b"\x1b[2;1HCD");

        assert_eq!(term.screen_string(), "AB\nCD\n");
    }

    #[test]
    fn assert_matches_passes_on_match() {
        let mut term = HeadlessTerm::new(10, 3);
        term.process(b"\x1b[1;1HHello");
        term.process(b"\x1b[3;1HWorld");

        term.assert_matches(&["Hello", "", "World"]);
    }

    #[test]
    #[should_panic(expected = "screen content mismatch")]
    fn assert_matches_panics_on_mismatch() {
        let mut term = HeadlessTerm::new(10, 3);
        term.process(b"Hello");

        term.assert_matches(&["Wrong", "", ""]);
    }

    #[test]
    #[should_panic(expected = "line count mismatch")]
    fn assert_matches_panics_on_wrong_line_count() {
        let term = HeadlessTerm::new(10, 3);
        term.assert_matches(&["", ""]); // 2 lines for 3-row terminal
    }

    #[test]
    fn assert_row_passes_on_match() {
        let mut term = HeadlessTerm::new(10, 3);
        term.process(b"Hello");
        term.assert_row(0, "Hello");
    }

    #[test]
    #[should_panic(expected = "row 0 mismatch")]
    fn assert_row_panics_on_mismatch() {
        let mut term = HeadlessTerm::new(10, 3);
        term.process(b"Hello");
        term.assert_row(0, "World");
    }

    #[test]
    fn assert_cursor_passes_on_match() {
        let mut term = HeadlessTerm::new(20, 5);
        term.process(b"\x1b[3;5H");
        term.assert_cursor(4, 2); // 0-indexed
    }

    #[test]
    #[should_panic(expected = "cursor position mismatch")]
    fn assert_cursor_panics_on_mismatch() {
        let term = HeadlessTerm::new(20, 5);
        term.assert_cursor(5, 5);
    }

    #[test]
    fn diff_returns_none_on_match() {
        let mut term = HeadlessTerm::new(10, 2);
        term.process(b"AB");
        assert!(term.diff(&["AB", ""]).is_none());
    }

    #[test]
    fn diff_returns_mismatches() {
        let mut term = HeadlessTerm::new(10, 3);
        term.process(b"\x1b[1;1HHello");
        term.process(b"\x1b[3;1HWorld");

        let diff = term.diff(&["Hello", "X", "World"]).unwrap();
        assert_eq!(diff.mismatches.len(), 1);
        assert_eq!(diff.mismatches[0].line, 1);
        assert_eq!(diff.mismatches[0].got, "");
        assert_eq!(diff.mismatches[0].want, "X");
    }

    #[test]
    fn diff_detects_character_difference() {
        let mut term = HeadlessTerm::new(10, 1);
        term.process(b"ABCXEF");

        let diff = term.diff(&["ABCDEF"]).unwrap();
        assert_eq!(diff.mismatches[0].line, 0);
    }

    #[test]
    fn reset_clears_everything() {
        let mut term = HeadlessTerm::new(10, 3);
        term.process(b"Hello");
        term.reset();

        assert_eq!(term.cursor(), (0, 0));
        assert!(term.captured_output().is_empty());
        assert!(term.screen_text().iter().all(|l| l.is_empty()));
    }

    #[test]
    fn captured_output_records_all_bytes() {
        let mut term = HeadlessTerm::new(10, 3);
        term.process(b"\x1b[1mHello");
        term.process(b"\x1b[0m");

        assert_eq!(term.captured_output(), b"\x1b[1mHello\x1b[0m");
    }

    #[test]
    fn export_string_contains_dimensions_and_content() {
        let mut term = HeadlessTerm::new(10, 3);
        term.process(b"Test");

        let export = term.export_string();
        assert!(export.contains("10x3"));
        assert!(export.contains("Test"));
    }

    #[test]
    fn export_to_file() {
        use std::time::{SystemTime, UNIX_EPOCH};
        // Use unique directory name to prevent race conditions in parallel tests
        let unique_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("ftui_headless_test_{unique_id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("export_test.txt");

        let mut term = HeadlessTerm::new(20, 5);
        term.process(b"\x1b[1;1HExported content");
        term.export(&path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("HeadlessTerm Export"));
        assert!(contents.contains("20x5"));
        assert!(contents.contains("Exported content"));
        assert!(contents.contains("ANSI Dump"));

        // Clean up
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sgr_styling_tracked() {
        let mut term = HeadlessTerm::new(20, 5);
        term.process(b"\x1b[1;31mBold Red\x1b[0m");

        // Verify text content
        assert_eq!(term.row_text(0), "Bold Red");

        // Verify styling via model
        let cell = term.model().cell(0, 0).unwrap();
        assert!(cell.attrs.has_flag(crate::cell::StyleFlags::BOLD));
    }

    #[test]
    fn multiline_content() {
        let mut term = HeadlessTerm::new(20, 5);
        term.process(b"Line 1\r\nLine 2\r\nLine 3");

        term.assert_matches(&["Line 1", "Line 2", "Line 3", "", ""]);
    }

    #[test]
    fn erase_operations_work() {
        let mut term = HeadlessTerm::new(10, 3);
        term.process(b"XXXXXXXXXX");
        term.process(b"\x1b[1;1H"); // Cursor to top-left
        term.process(b"\x1b[2J"); // Erase entire screen

        term.assert_matches(&["", "", ""]);
    }

    #[test]
    fn line_wrap_at_boundary() {
        let mut term = HeadlessTerm::new(5, 3);
        term.process(b"ABCDEFGH");

        assert_eq!(term.row_text(0), "ABCDE");
        assert_eq!(term.row_text(1), "FGH");
    }

    #[test]
    fn hyperlink_tracking() {
        let mut term = HeadlessTerm::new(20, 5);
        term.process(b"\x1b]8;;https://example.com\x07Link\x1b]8;;\x07");

        assert_eq!(term.row_text(0), "Link");
        assert!(!term.model().has_dangling_link());
    }

    #[test]
    fn screen_diff_display_format() {
        let diff = ScreenDiff {
            actual_lines: 3,
            expected_lines: 3,
            mismatches: vec![LineDiff {
                line: 1,
                got: "actual".to_string(),
                want: "expected".to_string(),
            }],
        };

        let display = format!("{diff}");
        assert!(display.contains("line 1"));
        assert!(display.contains("actual"));
        assert!(display.contains("expected"));
    }

    #[test]
    fn format_diff_shows_column_of_first_difference() {
        let diffs = vec![LineDiff {
            line: 0,
            got: "ABCXEF".to_string(),
            want: "ABCDEF".to_string(),
        }];

        let formatted = format_diff(&diffs);
        assert!(formatted.contains("first difference at column 3"));
    }

    #[test]
    fn format_diff_shows_length_difference() {
        let diffs = vec![LineDiff {
            line: 0,
            got: "ABC".to_string(),
            want: "ABCDEF".to_string(),
        }];

        let formatted = format_diff(&diffs);
        assert!(formatted.contains("diverges at column 3"));
    }

    // --- Integration with presenter pipeline ---

    #[test]
    fn presenter_output_roundtrips() {
        use crate::buffer::Buffer;
        use crate::cell::Cell;
        use crate::diff::BufferDiff;
        use crate::presenter::{Presenter, TerminalCapabilities};

        // Create two buffers simulating a frame update
        let prev = Buffer::new(10, 3);
        let mut next = Buffer::new(10, 3);

        // Write "Hello" on line 0 of the next buffer
        for (i, ch) in "Hello".chars().enumerate() {
            next.set(i as u16, 0, Cell::from_char(ch));
        }

        // Compute diff
        let diff = BufferDiff::compute(&prev, &next);

        // Emit ANSI via presenter into a Vec<u8>
        let output = {
            let mut buf = Vec::new();
            let caps = TerminalCapabilities::default();
            let mut presenter = Presenter::new(&mut buf, caps);
            presenter.present(&next, &diff).unwrap();
            drop(presenter); // flush on drop
            buf
        };

        // Feed the output into HeadlessTerm
        let mut term = HeadlessTerm::new(10, 3);
        term.process(&output);

        // Verify the round-trip
        term.assert_row(0, "Hello");
    }

    #[test]
    fn presenter_incremental_update_roundtrips() {
        use crate::buffer::Buffer;
        use crate::cell::Cell;
        use crate::diff::BufferDiff;
        use crate::presenter::{Presenter, TerminalCapabilities};

        let mut term = HeadlessTerm::new(10, 3);

        // Frame 1: write "Hello"
        let prev = Buffer::new(10, 3);
        let mut next = Buffer::new(10, 3);
        for (i, ch) in "Hello".chars().enumerate() {
            next.set(i as u16, 0, Cell::from_char(ch));
        }

        let diff = BufferDiff::compute(&prev, &next);
        let output = {
            let mut buf = Vec::new();
            let caps = TerminalCapabilities::default();
            let mut presenter = Presenter::new(&mut buf, caps);
            presenter.present(&next, &diff).unwrap();
            drop(presenter);
            buf
        };
        term.process(&output);
        term.assert_row(0, "Hello");

        // Frame 2: change "Hello" to "World"
        let prev2 = next;
        let mut next2 = Buffer::new(10, 3);
        for (i, ch) in "World".chars().enumerate() {
            next2.set(i as u16, 0, Cell::from_char(ch));
        }

        let diff2 = BufferDiff::compute(&prev2, &next2);
        let output2 = {
            let mut buf = Vec::new();
            let caps = TerminalCapabilities::default();
            let mut presenter = Presenter::new(&mut buf, caps);
            presenter.present(&next2, &diff2).unwrap();
            drop(presenter);
            buf
        };
        term.process(&output2);
        term.assert_row(0, "World");
    }

    // --- Cursor direction movement (CSI A/B/C/D) ---

    #[test]
    fn cursor_move_up() {
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[5;5H"); // Row 5, Col 5 (1-indexed) → (4, 4) 0-indexed
        term.assert_cursor(4, 4);
        term.process(b"\x1b[2A"); // Move up 2
        term.assert_cursor(4, 2);
    }

    #[test]
    fn cursor_move_down() {
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[1;1H"); // Top-left
        term.assert_cursor(0, 0);
        term.process(b"\x1b[3B"); // Move down 3
        term.assert_cursor(0, 3);
    }

    #[test]
    fn cursor_move_forward() {
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[1;1H"); // Top-left
        term.assert_cursor(0, 0);
        term.process(b"\x1b[5C"); // Move right 5
        term.assert_cursor(5, 0);
    }

    #[test]
    fn cursor_move_back() {
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[1;10H"); // Row 1, Col 10 → (9, 0) 0-indexed
        term.assert_cursor(9, 0);
        term.process(b"\x1b[4D"); // Move left 4
        term.assert_cursor(5, 0);
    }

    #[test]
    fn cursor_move_default_count() {
        // When no count is given, CSI A/B/C/D default to 1
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[5;5H"); // → (4, 4)
        term.process(b"\x1b[A"); // Up 1
        term.assert_cursor(4, 3);
        term.process(b"\x1b[C"); // Right 1
        term.assert_cursor(5, 3);
        term.process(b"\x1b[B"); // Down 1
        term.assert_cursor(5, 4);
        term.process(b"\x1b[D"); // Left 1
        term.assert_cursor(4, 4);
    }

    #[test]
    fn cursor_multiple_directions() {
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[1;1H"); // Start at origin
        term.process(b"\x1b[3C"); // Right 3
        term.process(b"\x1b[2B"); // Down 2
        term.process(b"\x1b[1D"); // Left 1
        term.process(b"\x1b[1A"); // Up 1
        term.assert_cursor(2, 1);
    }

    #[test]
    fn cursor_clamped_at_top() {
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[1;1H"); // Top-left
        term.process(b"\x1b[99A"); // Try to go up 99 from row 0
        term.assert_cursor(0, 0); // Should stay at top
    }

    #[test]
    fn cursor_clamped_at_left() {
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[1;1H"); // Top-left
        term.process(b"\x1b[99D"); // Try to go left 99 from col 0
        term.assert_cursor(0, 0); // Should stay at left
    }

    #[test]
    fn cursor_clamped_at_bottom() {
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[10;1H"); // Last row (1-indexed)
        term.process(b"\x1b[99B"); // Try to go down 99
        let (_, row) = term.cursor();
        assert!(row <= 9, "cursor row {row} should be <= 9 (height - 1)");
    }

    #[test]
    fn cursor_clamped_at_right() {
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[1;20H"); // Last column (1-indexed)
        term.process(b"\x1b[99C"); // Try to go right 99
        let (col, _) = term.cursor();
        assert!(col <= 19, "cursor col {col} should be <= 19 (width - 1)");
    }

    #[test]
    fn cursor_absolute_column_cha() {
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[3;1H"); // Row 3
        term.process(b"\x1b[8G"); // CHA: set column to 8 (1-indexed → col 7)
        term.assert_cursor(7, 2);
    }

    #[test]
    fn cursor_absolute_row_vpa() {
        let mut term = HeadlessTerm::new(20, 10);
        term.process(b"\x1b[1;5H"); // Col 5
        term.process(b"\x1b[6d"); // VPA: set row to 6 (1-indexed → row 5)
        term.assert_cursor(4, 5);
    }

    #[test]
    fn cursor_move_then_write() {
        let mut term = HeadlessTerm::new(20, 5);
        term.process(b"\x1b[3;1H"); // Move to row 3 col 1
        term.process(b"ABC");
        term.process(b"\x1b[2A"); // Up 2
        term.process(b"XY");
        term.assert_row(0, "   XY");
        term.assert_row(2, "ABC");
    }
}

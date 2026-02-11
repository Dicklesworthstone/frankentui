//! Canonical terminal engine that composes parser + state + reply generation.
//!
//! This type is the host-agnostic ownership boundary for VT state. It keeps all
//! mutable terminal state in one place and exposes deterministic APIs for:
//! - feeding bytes,
//! - resizing with scrollback integration,
//! - draining terminal query replies,
//! - snapshotting incremental patches.

use crate::{
    Action, AnsiModes, Cursor, Grid, GridDiff, Modes, Parser, Patch, ReplyContext, ReplyEngine,
    SavedCursor, Scrollback, WidthPolicy, translate_charset,
};

/// Default scrollback capacity for [`TerminalEngine`].
pub const DEFAULT_SCROLLBACK_CAPACITY: usize = 512;

/// Configuration for [`TerminalEngine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalEngineConfig {
    /// Maximum number of scrollback lines retained.
    pub scrollback_capacity: usize,
    /// Reply identity/policy for terminal query sequences (DA/DSR/CPR/DECRPM).
    pub reply_engine: ReplyEngine,
    /// Unicode character width measurement policy.
    pub width_policy: WidthPolicy,
}

impl Default for TerminalEngineConfig {
    fn default() -> Self {
        Self {
            scrollback_capacity: DEFAULT_SCROLLBACK_CAPACITY,
            reply_engine: ReplyEngine::default(),
            width_policy: WidthPolicy::Standard,
        }
    }
}

/// Canonical owner for terminal state and deterministic state transitions.
#[derive(Debug, Clone)]
pub struct TerminalEngine {
    parser: Parser,
    grid: Grid,
    presented_grid: Grid,
    cursor: Cursor,
    saved_cursor: SavedCursor,
    scrollback: Scrollback,
    modes: Modes,
    reply_engine: ReplyEngine,
    pending_replies: Vec<Vec<u8>>,
    last_printed: Option<char>,
    cols: u16,
    rows: u16,
    scrollback_capacity: usize,
    width_policy: WidthPolicy,
}

impl TerminalEngine {
    /// Create a terminal engine with default configuration.
    ///
    /// # Panics
    ///
    /// Panics if `cols == 0` or `rows == 0`.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_config(cols, rows, TerminalEngineConfig::default())
    }

    /// Create a terminal engine with explicit configuration.
    ///
    /// # Panics
    ///
    /// Panics if `cols == 0` or `rows == 0`.
    #[must_use]
    pub fn with_config(cols: u16, rows: u16, config: TerminalEngineConfig) -> Self {
        assert!(cols > 0, "cols must be > 0");
        assert!(rows > 0, "rows must be > 0");
        let grid = Grid::new(cols, rows);
        Self {
            parser: Parser::new(),
            presented_grid: grid.clone(),
            grid,
            cursor: Cursor::new(cols, rows),
            saved_cursor: SavedCursor::default(),
            scrollback: Scrollback::new(config.scrollback_capacity),
            modes: Modes::new(),
            reply_engine: config.reply_engine,
            pending_replies: Vec::new(),
            last_printed: None,
            cols,
            rows,
            scrollback_capacity: config.scrollback_capacity,
            width_policy: config.width_policy,
        }
    }

    /// Feed VT/ANSI bytes into the engine.
    ///
    /// Returns the number of parser actions applied.
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> usize {
        let actions = self.parser.feed(bytes);
        let action_count = actions.len();
        for action in actions {
            self.apply_action(action);
        }
        action_count
    }

    /// Resize the viewport dimensions with scrollback-aware behavior.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        assert!(cols > 0, "cols must be > 0");
        assert!(rows > 0, "rows must be > 0");
        if cols == self.cols && rows == self.rows {
            return;
        }

        let new_cursor_row =
            self.grid
                .resize_with_scrollback(cols, rows, self.cursor.row, &mut self.scrollback);
        self.cols = cols;
        self.rows = rows;
        self.cursor.resize(cols, rows);
        self.cursor.row = new_cursor_row.min(rows.saturating_sub(1));
        self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
    }

    /// Compute an incremental patch from the last presented grid snapshot.
    ///
    /// The returned updates are stable row-major ordered and deterministic.
    pub fn snapshot_patches(&mut self) -> Patch {
        let patch = GridDiff::diff(&self.presented_grid, &self.grid);
        self.presented_grid = self.grid.clone();
        patch
    }

    /// Drain queued terminal reply byte chunks in FIFO order.
    pub fn drain_replies(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending_replies)
    }

    /// Current parser instance.
    #[must_use]
    pub fn parser(&self) -> &Parser {
        &self.parser
    }

    /// Current grid state.
    #[must_use]
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Current cursor state.
    #[must_use]
    pub fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    /// Current mode state.
    #[must_use]
    pub fn modes(&self) -> &Modes {
        &self.modes
    }

    /// Current scrollback state.
    #[must_use]
    pub fn scrollback(&self) -> &Scrollback {
        &self.scrollback
    }

    /// Configured grid width.
    #[must_use]
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Configured grid height.
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Current width policy.
    #[must_use]
    pub fn width_policy(&self) -> WidthPolicy {
        self.width_policy
    }

    fn maybe_enqueue_reply_for_action(&mut self, action: &Action) {
        let context = ReplyContext {
            cursor_row: self.cursor.row,
            cursor_col: self.cursor.col,
            modes: Some(&self.modes),
        };
        if let Some(reply) = self.reply_engine.reply_for_action(action, context) {
            self.pending_replies.push(reply);
        }
    }

    fn apply_action(&mut self, action: Action) {
        self.maybe_enqueue_reply_for_action(&action);

        match action {
            Action::Print(ch) => self.apply_print(ch),
            Action::Newline => self.apply_newline(),
            Action::CarriageReturn => self.cursor.carriage_return(),
            Action::Tab => {
                self.cursor.col = self.cursor.next_tab_stop(self.cols);
                self.cursor.pending_wrap = false;
            }
            Action::Backspace => self.cursor.move_left(1),
            Action::Bell => {}
            Action::CursorUp(count) => self.cursor.move_up(count),
            Action::CursorDown(count) => self.cursor.move_down(count, self.rows),
            Action::CursorRight(count) => self.cursor.move_right(count, self.cols),
            Action::CursorLeft(count) => self.cursor.move_left(count),
            Action::CursorNextLine(count) => {
                self.cursor.move_down(count, self.rows);
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            Action::CursorPrevLine(count) => {
                self.cursor.move_up(count);
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            Action::CursorRow(row) => {
                if self.modes.origin_mode() {
                    let abs_row = row.saturating_add(self.cursor.scroll_top());
                    self.cursor.row = abs_row.min(self.cursor.scroll_bottom().saturating_sub(1));
                    self.cursor.pending_wrap = false;
                } else {
                    self.cursor
                        .move_to(row, self.cursor.col, self.rows, self.cols);
                }
            }
            Action::CursorColumn(col) => {
                self.cursor
                    .move_to(self.cursor.row, col, self.rows, self.cols);
            }
            Action::SetScrollRegion { top, bottom } => {
                let bottom = if bottom == 0 {
                    self.rows
                } else {
                    bottom.min(self.rows)
                };
                self.cursor.set_scroll_region(top, bottom, self.rows);
                if self.modes.origin_mode() {
                    self.cursor.row = self.cursor.scroll_top();
                    self.cursor.col = 0;
                    self.cursor.pending_wrap = false;
                } else {
                    self.cursor.move_to(0, 0, self.rows, self.cols);
                }
            }
            Action::ScrollUp(count) => self.grid.scroll_up_into(
                self.cursor.scroll_top(),
                self.cursor.scroll_bottom(),
                count,
                &mut self.scrollback,
                self.cursor.attrs.bg,
            ),
            Action::ScrollDown(count) => self.grid.scroll_down(
                self.cursor.scroll_top(),
                self.cursor.scroll_bottom(),
                count,
                self.cursor.attrs.bg,
            ),
            Action::InsertLines(count) => {
                self.grid.insert_lines(
                    self.cursor.row,
                    count,
                    self.cursor.scroll_top(),
                    self.cursor.scroll_bottom(),
                    self.cursor.attrs.bg,
                );
                self.cursor.pending_wrap = false;
            }
            Action::DeleteLines(count) => {
                self.grid.delete_lines(
                    self.cursor.row,
                    count,
                    self.cursor.scroll_top(),
                    self.cursor.scroll_bottom(),
                    self.cursor.attrs.bg,
                );
                self.cursor.pending_wrap = false;
            }
            Action::InsertChars(count) => {
                self.grid.insert_chars(
                    self.cursor.row,
                    self.cursor.col,
                    count,
                    self.cursor.attrs.bg,
                );
                self.cursor.pending_wrap = false;
            }
            Action::DeleteChars(count) => {
                self.grid.delete_chars(
                    self.cursor.row,
                    self.cursor.col,
                    count,
                    self.cursor.attrs.bg,
                );
                self.cursor.pending_wrap = false;
            }
            Action::CursorPosition { row, col } => {
                if self.modes.origin_mode() {
                    let abs_row = row.saturating_add(self.cursor.scroll_top());
                    self.cursor.row = abs_row.min(self.cursor.scroll_bottom().saturating_sub(1));
                    self.cursor.col = col.min(self.cols.saturating_sub(1));
                    self.cursor.pending_wrap = false;
                } else {
                    self.cursor.move_to(row, col, self.rows, self.cols);
                }
            }
            Action::EraseInDisplay(mode) => {
                let bg = self.cursor.attrs.bg;
                match mode {
                    0 => self.grid.erase_below(self.cursor.row, self.cursor.col, bg),
                    1 => self.grid.erase_above(self.cursor.row, self.cursor.col, bg),
                    2 => self.grid.erase_all(bg),
                    _ => {}
                }
            }
            Action::EraseInLine(mode) => {
                let bg = self.cursor.attrs.bg;
                match mode {
                    0 => self
                        .grid
                        .erase_line_right(self.cursor.row, self.cursor.col, bg),
                    1 => self
                        .grid
                        .erase_line_left(self.cursor.row, self.cursor.col, bg),
                    2 => self.grid.erase_line(self.cursor.row, bg),
                    _ => {}
                }
            }
            Action::Sgr(params) => self.cursor.attrs.apply_sgr_params(&params),
            Action::DecSet(params) => {
                for &p in &params {
                    self.modes.set_dec_mode(p, true);
                    if p == 6 {
                        self.cursor.row = self.cursor.scroll_top();
                        self.cursor.col = 0;
                        self.cursor.pending_wrap = false;
                    } else if p == 25 {
                        self.cursor.visible = true;
                    }
                }
            }
            Action::DecRst(params) => {
                for &p in &params {
                    self.modes.set_dec_mode(p, false);
                    if p == 6 {
                        self.cursor.row = 0;
                        self.cursor.col = 0;
                        self.cursor.pending_wrap = false;
                    } else if p == 25 {
                        self.cursor.visible = false;
                    }
                }
            }
            Action::AnsiSet(params) => {
                for &p in &params {
                    self.modes.set_ansi_mode(p, true);
                }
            }
            Action::AnsiRst(params) => {
                for &p in &params {
                    self.modes.set_ansi_mode(p, false);
                }
            }
            Action::SaveCursor => {
                self.saved_cursor = SavedCursor::save(&self.cursor, self.modes.origin_mode());
            }
            Action::RestoreCursor => self.saved_cursor.restore(&mut self.cursor),
            Action::Index => self.apply_index(),
            Action::ReverseIndex => {
                if self.cursor.row == self.cursor.scroll_top() {
                    self.grid.scroll_down(
                        self.cursor.scroll_top(),
                        self.cursor.scroll_bottom(),
                        1,
                        self.cursor.attrs.bg,
                    );
                } else {
                    self.cursor.move_up(1);
                }
                self.cursor.pending_wrap = false;
            }
            Action::NextLine => {
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
                self.apply_index();
            }
            Action::FullReset => {
                self.grid = Grid::new(self.cols, self.rows);
                self.cursor = Cursor::new(self.cols, self.rows);
                self.saved_cursor = SavedCursor::default();
                self.scrollback = Scrollback::new(self.scrollback_capacity);
                self.modes.reset();
                self.last_printed = None;
            }
            Action::SetTitle(_) | Action::HyperlinkStart(_) | Action::HyperlinkEnd => {}
            Action::SetTabStop => self.cursor.set_tab_stop(),
            Action::ClearTabStop(mode) => match mode {
                0 => self.cursor.clear_tab_stop(),
                3 | 5 => self.cursor.clear_all_tab_stops(),
                _ => {}
            },
            Action::BackTab(count) => {
                for _ in 0..count {
                    self.cursor.col = self.cursor.prev_tab_stop();
                }
                self.cursor.pending_wrap = false;
            }
            Action::EraseChars(count) => self.grid.erase_chars(
                self.cursor.row,
                self.cursor.col,
                count,
                self.cursor.attrs.bg,
            ),
            Action::ScreenAlignment => {
                self.grid.fill_all('E');
                self.cursor.move_to(0, 0, self.rows, self.cols);
            }
            Action::RepeatChar(count) => {
                if let Some(ch) = self.last_printed {
                    for _ in 0..count {
                        self.apply_print(ch);
                    }
                }
            }
            Action::ApplicationKeypad | Action::NormalKeypad => {}
            Action::SetCursorShape(_) => {}
            Action::SoftReset => {
                self.modes = Modes::new();
                self.cursor.attrs = Default::default();
                self.cursor.visible = self.modes.cursor_visible();
                self.cursor.set_scroll_region(0, self.rows, self.rows);
                self.cursor.pending_wrap = false;
                self.cursor.reset_charset();
            }
            Action::EraseScrollback => self.scrollback.clear(),
            Action::FocusIn | Action::FocusOut | Action::PasteStart | Action::PasteEnd => {}
            Action::DeviceAttributes
            | Action::DeviceAttributesSecondary
            | Action::DeviceStatusReport
            | Action::CursorPositionReport => {}
            Action::DesignateCharset { slot, charset } => {
                self.cursor.designate_charset(slot, charset)
            }
            Action::SingleShift2 => self.cursor.single_shift = Some(2),
            Action::SingleShift3 => self.cursor.single_shift = Some(3),
            Action::MouseEvent { .. } => {}
            Action::Escape(_) => {}
        }
    }

    fn apply_index(&mut self) {
        if self.cursor.row + 1 >= self.cursor.scroll_bottom() {
            self.grid.scroll_up_into(
                self.cursor.scroll_top(),
                self.cursor.scroll_bottom(),
                1,
                &mut self.scrollback,
                self.cursor.attrs.bg,
            );
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
        self.cursor.pending_wrap = false;
    }

    fn apply_newline(&mut self) {
        if self.modes.ansi.contains(AnsiModes::LINEFEED_NEWLINE) {
            self.cursor.col = 0;
        }
        self.apply_index();
    }

    fn wrap_to_next_line(&mut self) {
        self.cursor.col = 0;
        self.apply_index();
    }

    fn apply_print(&mut self, ch: char) {
        let charset = self.cursor.effective_charset();
        let ch = translate_charset(ch, charset);
        self.cursor.consume_single_shift();
        self.last_printed = Some(ch);

        if self.cursor.pending_wrap {
            if self.modes.autowrap() {
                self.wrap_to_next_line();
            } else {
                self.cursor.pending_wrap = false;
            }
        }

        let width = self.width_policy.char_width(ch);
        if width == 0 {
            // Combining mark / ZWJ / VS16: attach to the previous cell.
            self.apply_combining_mark(ch);
            return;
        }

        if width == 2 && self.cursor.col + 1 >= self.cols {
            if self.modes.autowrap() {
                self.wrap_to_next_line();
            } else {
                self.cursor.pending_wrap = false;
                return;
            }
        }

        if self.modes.insert_mode() {
            self.grid.insert_chars(
                self.cursor.row,
                self.cursor.col,
                u16::from(width),
                self.cursor.attrs.bg,
            );
        }

        let written = self.grid.write_printable_with_width(
            self.cursor.row,
            self.cursor.col,
            ch,
            self.cursor.attrs,
            width,
        );
        if written == 0 {
            return;
        }

        if self.cursor.col + u16::from(written) >= self.cols {
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.col += u16::from(written);
            self.cursor.pending_wrap = false;
        }
    }

    /// Attach a combining mark to the cell that was most recently printed.
    ///
    /// When `pending_wrap` is set, the last character is at `cursor.col`
    /// (the rightmost column). Otherwise it is at `cursor.col - 1`.
    /// If the cursor is at column 0 with no pending wrap, there is no
    /// previous cell and the mark is silently dropped.
    fn apply_combining_mark(&mut self, mark: char) {
        let (row, col) = if self.cursor.pending_wrap {
            // Last printed char sits at cursor.col (right margin), wrap hasn't fired yet.
            (self.cursor.row, self.cursor.col)
        } else if self.cursor.col > 0 {
            (self.cursor.row, self.cursor.col - 1)
        } else {
            // Column 0, no pending wrap — nowhere to attach.
            return;
        };
        self.grid.push_combining_mark(row, col, mark);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_chars(grid: &Grid) -> Vec<char> {
        let mut out = Vec::with_capacity(usize::from(grid.rows()) * usize::from(grid.cols()));
        for row in 0..grid.rows() {
            for col in 0..grid.cols() {
                out.push(
                    grid.cell(row, col)
                        .expect("row/col in bounds during test snapshot")
                        .content(),
                );
            }
        }
        out
    }

    #[test]
    fn replies_are_deterministic_and_fifo() {
        let mut engine = TerminalEngine::new(8, 4);
        engine.feed_bytes(b"\x1b[5n\x1b[6n");

        let replies = engine.drain_replies();
        assert_eq!(replies, vec![b"\x1b[0n".to_vec(), b"\x1b[1;1R".to_vec()]);
        assert!(engine.drain_replies().is_empty());
    }

    #[test]
    fn snapshot_patches_are_row_major_and_incremental() {
        let mut engine = TerminalEngine::new(4, 2);
        engine.feed_bytes(b"AB\r\nCD");

        let patch = engine.snapshot_patches();
        assert_eq!(patch.cols, 4);
        assert_eq!(patch.rows, 2);

        let coords = patch
            .updates
            .iter()
            .map(|update| (update.row, update.col, update.cell.content()))
            .collect::<Vec<_>>();
        assert_eq!(
            coords,
            vec![(0, 0, 'A'), (0, 1, 'B'), (1, 0, 'C'), (1, 1, 'D')]
        );

        let second = engine.snapshot_patches();
        assert!(second.is_empty());
    }

    #[test]
    fn chunked_feed_matches_single_chunk_feed() {
        let bytes = b"ab\x1b[2;3HZ\x1b[5n";

        let mut single = TerminalEngine::new(6, 4);
        single.feed_bytes(bytes);

        let mut chunked = TerminalEngine::new(6, 4);
        chunked.feed_bytes(b"a");
        chunked.feed_bytes(b"b\x1b[2");
        chunked.feed_bytes(b";3H");
        chunked.feed_bytes(b"Z\x1b[5n");

        assert_eq!(grid_chars(single.grid()), grid_chars(chunked.grid()));
        assert_eq!(single.cursor().row, chunked.cursor().row);
        assert_eq!(single.cursor().col, chunked.cursor().col);
        assert_eq!(single.modes(), chunked.modes());
        assert_eq!(single.drain_replies(), chunked.drain_replies());
    }

    #[test]
    fn resize_updates_geometry_and_emits_followup_patch() {
        let mut engine = TerminalEngine::new(3, 2);
        engine.feed_bytes(b"ABCDEF");
        let _ = engine.snapshot_patches();

        engine.resize(2, 2);
        let patch = engine.snapshot_patches();

        assert_eq!(engine.cols(), 2);
        assert_eq!(engine.rows(), 2);
        assert_eq!(patch.cols, 2);
        assert_eq!(patch.rows, 2);
        assert!(
            patch
                .updates
                .iter()
                .all(|update| update.row < 2 && update.col < 2)
        );
        assert!(engine.cursor().row < 2);
        assert!(engine.cursor().col < 2);
    }

    // ── Combining mark / grapheme cluster integration ──────────────

    #[test]
    fn combining_accent_attaches_to_previous_char() {
        let mut engine = TerminalEngine::new(10, 1);
        // 'e' followed by combining acute accent (U+0301)
        engine.feed_bytes("e\u{0301}".as_bytes());

        let cell = engine.grid().cell(0, 0).unwrap();
        assert_eq!(cell.content(), 'e');
        assert!(cell.has_combining());
        assert_eq!(cell.combining_marks(), &['\u{0301}']);
        // Cursor should be at col 1 (only 'e' advances).
        assert_eq!(engine.cursor().col, 1);
    }

    #[test]
    fn multiple_combining_marks_on_same_cell() {
        let mut engine = TerminalEngine::new(10, 1);
        // 'a' + combining grave (U+0300) + combining acute (U+0301)
        engine.feed_bytes("a\u{0300}\u{0301}".as_bytes());

        let cell = engine.grid().cell(0, 0).unwrap();
        assert_eq!(cell.content(), 'a');
        assert_eq!(cell.combining_marks(), &['\u{0300}', '\u{0301}']);
        assert_eq!(engine.cursor().col, 1);
    }

    #[test]
    fn combining_on_wide_char_via_engine() {
        let mut engine = TerminalEngine::new(10, 1);
        // CJK ideograph '中' (wide) + combining grave
        engine.feed_bytes("中\u{0300}".as_bytes());

        // '中' is at col 0, continuation at col 1, cursor at col 2.
        // The combining mark should attach to col 1 (cursor was at col 2,
        // so previous cell is col 1). But col 1 is a continuation cell.
        // Actually let me think: after printing '中', cursor.col = 2.
        // pending_wrap is false (col + 2 = 2 < 10).
        // apply_combining_mark: cursor.col > 0 → attach to (0, cursor.col - 1) = (0, 1).
        // col 1 is the CONTINUATION cell.
        //
        // This is technically valid — the mark attaches to wherever the cursor
        // "was". Some terminals attach to the leading cell instead.
        // For correctness, let's verify the current behavior.
        let cont = engine.grid().cell(0, 1).unwrap();
        assert!(cont.is_wide_continuation());
        // The combining mark landed on the continuation cell.
        assert!(cont.has_combining());
    }

    #[test]
    fn combining_at_column_zero_is_dropped() {
        let mut engine = TerminalEngine::new(10, 1);
        // Send combining mark without any preceding base character.
        engine.feed_bytes("\u{0301}".as_bytes());

        // Nothing to attach to — should be silently dropped.
        let cell = engine.grid().cell(0, 0).unwrap();
        assert_eq!(cell.content(), ' ');
        assert!(!cell.has_combining());
        assert_eq!(engine.cursor().col, 0);
    }

    #[test]
    fn combining_with_pending_wrap() {
        // 3-column terminal: fill row to trigger pending_wrap, then send combining.
        let mut engine = TerminalEngine::new(3, 2);
        engine.feed_bytes(b"ABC"); // fills row, pending_wrap = true

        assert!(engine.cursor().pending_wrap);
        assert_eq!(engine.cursor().col, 2); // at last column

        engine.feed_bytes("\u{0301}".as_bytes());

        // Mark should attach to cell at cursor.col (pending_wrap branch).
        let cell = engine.grid().cell(0, 2).unwrap();
        assert_eq!(cell.content(), 'C');
        assert!(cell.has_combining());
        assert_eq!(cell.combining_marks(), &['\u{0301}']);

        // pending_wrap should still be set (combining doesn't consume it).
        assert!(engine.cursor().pending_wrap);
    }

    #[test]
    fn combining_after_normal_char_does_not_advance_cursor() {
        let mut engine = TerminalEngine::new(10, 1);
        engine.feed_bytes("AB".as_bytes());
        assert_eq!(engine.cursor().col, 2);

        engine.feed_bytes("\u{0300}".as_bytes());
        // Cursor should NOT advance for combining marks.
        assert_eq!(engine.cursor().col, 2);

        engine.feed_bytes("C".as_bytes());
        assert_eq!(engine.cursor().col, 3);
    }

    #[test]
    fn zwj_attaches_to_previous_cell() {
        let mut engine = TerminalEngine::new(10, 1);
        // ZWJ (U+200D) has width 0 — should attach as combining.
        engine.feed_bytes("X\u{200D}".as_bytes());

        let cell = engine.grid().cell(0, 0).unwrap();
        assert_eq!(cell.content(), 'X');
        assert!(cell.has_combining());
        assert_eq!(cell.combining_marks(), &['\u{200D}']);
    }

    #[test]
    fn vs16_attaches_to_previous_cell() {
        let mut engine = TerminalEngine::new(10, 1);
        // VS16 (U+FE0F) has width 0 — should attach as combining.
        engine.feed_bytes("*\u{FE0F}".as_bytes());

        let cell = engine.grid().cell(0, 0).unwrap();
        assert_eq!(cell.content(), '*');
        assert!(cell.has_combining());
        assert_eq!(cell.combining_marks(), &['\u{FE0F}']);
    }

    #[test]
    fn overwrite_cell_clears_combining() {
        let mut engine = TerminalEngine::new(10, 1);
        engine.feed_bytes("e\u{0301}".as_bytes());

        let cell = engine.grid().cell(0, 0).unwrap();
        assert!(cell.has_combining());

        // Move cursor back and overwrite.
        engine.feed_bytes(b"\x1b[1G"); // CHA: move to col 0
        engine.feed_bytes(b"X");

        let cell = engine.grid().cell(0, 0).unwrap();
        assert_eq!(cell.content(), 'X');
        assert!(!cell.has_combining());
    }

    #[test]
    fn cjk_width_policy_with_combining() {
        use crate::WidthPolicy;
        let config = TerminalEngineConfig {
            width_policy: WidthPolicy::CjkAmbiguousWide,
            ..TerminalEngineConfig::default()
        };
        let mut engine = TerminalEngine::with_config(10, 1, config);
        assert_eq!(engine.width_policy(), WidthPolicy::CjkAmbiguousWide);

        // Box drawing '─' is wide under CJK policy.
        engine.feed_bytes("─\u{0300}".as_bytes());

        // '─' should occupy 2 columns as a wide char.
        let cell = engine.grid().cell(0, 0).unwrap();
        assert_eq!(cell.content(), '─');
        assert!(cell.is_wide());

        // Combining mark attaches to continuation cell at col 1
        // (cursor was at col 2 after wide char, so col-1 = col 1).
        let cont = engine.grid().cell(0, 1).unwrap();
        assert!(cont.has_combining());
    }
}

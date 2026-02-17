//! Terminal cursor: position, visibility, movement, and saved state.
//!
//! The cursor tracks the current writing position in the grid and manages
//! saved/restored state for DECSC/DECRC sequences. It also tracks the
//! scroll region (top/bottom margins) and tab stops.

use crate::cell::SgrAttrs;

/// Default tab stop interval (every 8 columns).
const DEFAULT_TAB_INTERVAL: u16 = 8;

/// Terminal cursor state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// Current row (0-indexed from top of viewport).
    pub row: u16,
    /// Current column (0-indexed from left).
    pub col: u16,
    /// Whether the cursor is visible (DECTCEM).
    pub visible: bool,
    /// Pending wrap: the cursor is at the right margin and the next printable
    /// character should trigger a line wrap. This avoids the xterm off-by-one
    /// behavior where the cursor sits *past* the last column.
    pub pending_wrap: bool,
    /// Current SGR attributes applied to newly written characters.
    pub attrs: SgrAttrs,
    /// Top of scroll region (inclusive, 0-indexed).
    scroll_top: u16,
    /// Bottom of scroll region (exclusive, 0-indexed).
    scroll_bottom: u16,
    /// Tab stop positions (sorted). `true` at column index means tab stop set.
    tab_stops: Vec<bool>,
    /// Character set slots G0–G3. Each stores a charset designator byte:
    /// `b'B'` = ASCII (default), `b'0'` = DEC Special Graphics, `b'A'` = UK.
    pub charset_slots: [u8; 4],
    /// Active character set slot index (0 = G0, default).
    pub active_charset: u8,
    /// Single-shift override: if `Some(2)` or `Some(3)`, the next printed char
    /// uses G2 or G3 respectively, then clears back to `None`.
    pub single_shift: Option<u8>,
}

impl Cursor {
    /// Create a cursor for a grid of the given dimensions.
    pub fn new(cols: u16, rows: u16) -> Self {
        let mut tab_stops = vec![false; cols as usize];
        for i in (0..cols).step_by(DEFAULT_TAB_INTERVAL as usize) {
            tab_stops[i as usize] = true;
        }
        Self {
            row: 0,
            col: 0,
            visible: true,
            pending_wrap: false,
            attrs: SgrAttrs::default(),
            scroll_top: 0,
            scroll_bottom: rows,
            tab_stops,
            charset_slots: [b'B'; 4],
            active_charset: 0,
            single_shift: None,
        }
    }

    /// Create a cursor at the given position with default attributes.
    pub fn at(row: u16, col: u16) -> Self {
        Self {
            row,
            col,
            ..Self::default()
        }
    }

    // ── Scroll region ───────────────────────────────────────────────

    /// Top of scroll region (inclusive).
    pub fn scroll_top(&self) -> u16 {
        self.scroll_top
    }

    /// Bottom of scroll region (exclusive).
    pub fn scroll_bottom(&self) -> u16 {
        self.scroll_bottom
    }

    /// Set scroll region margins (DECSTBM). Both are 0-indexed.
    /// `top` is inclusive, `bottom` is exclusive.
    ///
    /// If `top >= bottom` or `bottom > rows`, the request is ignored.
    pub fn set_scroll_region(&mut self, top: u16, bottom: u16, rows: u16) {
        if top < bottom && bottom <= rows {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        }
    }

    /// Reset scroll region to full screen.
    pub fn reset_scroll_region(&mut self, rows: u16) {
        self.scroll_top = 0;
        self.scroll_bottom = rows;
    }

    /// Whether the cursor is inside the scroll region.
    pub fn in_scroll_region(&self) -> bool {
        self.row >= self.scroll_top && self.row < self.scroll_bottom
    }

    // ── Movement ────────────────────────────────────────────────────

    /// Clamp the cursor position to the given grid bounds.
    pub fn clamp(&mut self, rows: u16, cols: u16) {
        if rows > 0 {
            self.row = self.row.min(rows - 1);
        }
        if cols > 0 {
            self.col = self.col.min(cols - 1);
        }
        self.pending_wrap = false;
    }

    /// CUP: Move cursor to absolute position (0-indexed).
    /// Coordinates are clamped to grid bounds.
    pub fn move_to(&mut self, row: u16, col: u16, rows: u16, cols: u16) {
        self.row = row.min(rows.saturating_sub(1));
        self.col = col.min(cols.saturating_sub(1));
        self.pending_wrap = false;
    }

    /// CUU: Move cursor up by `count` rows, stopping at the top margin.
    pub fn move_up(&mut self, count: u16) {
        let limit = self.scroll_top;
        self.row = self.row.saturating_sub(count).max(limit);
        self.pending_wrap = false;
    }

    /// CUD: Move cursor down by `count` rows, stopping at the bottom margin.
    pub fn move_down(&mut self, count: u16, rows: u16) {
        let limit = self.scroll_bottom.min(rows).saturating_sub(1);
        self.row = self.row.saturating_add(count).min(limit);
        self.pending_wrap = false;
    }

    /// CUF: Move cursor right by `count` columns, stopping at the right margin.
    pub fn move_right(&mut self, count: u16, cols: u16) {
        self.col = self.col.saturating_add(count).min(cols.saturating_sub(1));
        self.pending_wrap = false;
    }

    /// CUB: Move cursor left by `count` columns, stopping at column 0.
    pub fn move_left(&mut self, count: u16) {
        self.col = self.col.saturating_sub(count);
        self.pending_wrap = false;
    }

    /// CR: Carriage return — move cursor to column 0.
    pub fn carriage_return(&mut self) {
        self.col = 0;
        self.pending_wrap = false;
    }

    // ── Tab stops ───────────────────────────────────────────────────

    /// Advance to the next tab stop. Returns the new column.
    pub fn next_tab_stop(&self, cols: u16) -> u16 {
        let start = (self.col as usize).saturating_add(1);
        for i in start..self.tab_stops.len().min(cols as usize) {
            if self.tab_stops[i] {
                return i as u16;
            }
        }
        // No tab stop found — go to last column.
        cols.saturating_sub(1)
    }

    /// Move back to the previous tab stop. Returns the new column.
    pub fn prev_tab_stop(&self) -> u16 {
        if self.col == 0 {
            return 0;
        }
        for i in (0..self.col as usize).rev() {
            if self.tab_stops[i] {
                return i as u16;
            }
        }
        0
    }

    /// Set a tab stop at the current column.
    pub fn set_tab_stop(&mut self) {
        if (self.col as usize) < self.tab_stops.len() {
            self.tab_stops[self.col as usize] = true;
        }
    }

    /// Clear the tab stop at the current column.
    pub fn clear_tab_stop(&mut self) {
        if (self.col as usize) < self.tab_stops.len() {
            self.tab_stops[self.col as usize] = false;
        }
    }

    /// Clear all tab stops.
    pub fn clear_all_tab_stops(&mut self) {
        for stop in &mut self.tab_stops {
            *stop = false;
        }
    }

    /// Reset tab stops to the default interval (every 8 columns).
    pub fn reset_tab_stops(&mut self, cols: u16) {
        self.tab_stops = vec![false; cols as usize];
        for i in (0..cols).step_by(DEFAULT_TAB_INTERVAL as usize) {
            self.tab_stops[i as usize] = true;
        }
    }

    // ── Resize ──────────────────────────────────────────────────────

    /// Adjust cursor state after a grid resize.
    pub fn resize(&mut self, new_cols: u16, new_rows: u16) {
        let old_cols = self.tab_stops.len();
        self.scroll_top = 0;
        self.scroll_bottom = new_rows;
        self.clamp(new_rows, new_cols);
        // Resize tab stops, preserving existing stops in the original range.
        self.tab_stops.resize(new_cols as usize, false);
        // Set default tab stops only on newly added columns.
        for i in (0..new_cols).step_by(DEFAULT_TAB_INTERVAL as usize) {
            let idx = i as usize;
            if idx >= old_cols {
                self.tab_stops[idx] = true;
            }
        }
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            row: 0,
            col: 0,
            visible: true,
            pending_wrap: false,
            attrs: SgrAttrs::default(),
            scroll_top: 0,
            scroll_bottom: 24, // reasonable default
            tab_stops: {
                let mut stops = vec![false; 80];
                for i in (0..80).step_by(DEFAULT_TAB_INTERVAL as usize) {
                    stops[i] = true;
                }
                stops
            },
            charset_slots: [b'B'; 4],
            active_charset: 0,
            single_shift: None,
        }
    }
}

/// Saved cursor state for DECSC / DECRC.
///
/// Captures the full cursor state so it can be restored exactly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SavedCursor {
    pub row: u16,
    pub col: u16,
    pub attrs: SgrAttrs,
    pub origin_mode: bool,
    pub pending_wrap: bool,
    pub charset_slots: [u8; 4],
    pub active_charset: u8,
}

impl SavedCursor {
    /// Capture the current cursor state.
    pub fn save(cursor: &Cursor, origin_mode: bool) -> Self {
        Self {
            row: cursor.row,
            col: cursor.col,
            attrs: cursor.attrs,
            origin_mode,
            pending_wrap: cursor.pending_wrap,
            charset_slots: cursor.charset_slots,
            active_charset: cursor.active_charset,
        }
    }

    /// Restore the saved state into the cursor.
    pub fn restore(&self, cursor: &mut Cursor) {
        cursor.row = self.row;
        cursor.col = self.col;
        cursor.attrs = self.attrs;
        cursor.pending_wrap = self.pending_wrap;
        cursor.charset_slots = self.charset_slots;
        cursor.active_charset = self.active_charset;
        cursor.single_shift = None;
    }
}

// ── Character set translation ─────────────────────────────────────────

/// Translate a character through the DEC Special Graphics charset (`ESC ( 0`).
///
/// Maps ASCII 0x60–0x7E to Unicode line-drawing and symbol characters.
/// Characters outside this range pass through unchanged.
fn dec_graphics_char(ch: char) -> char {
    match ch {
        '`' => '\u{25C6}', // ◆ diamond
        'a' => '\u{2592}', // ▒ checker board
        'b' => '\u{2409}', // ␉ HT symbol
        'c' => '\u{240C}', // ␌ FF symbol
        'd' => '\u{240D}', // ␍ CR symbol
        'e' => '\u{240A}', // ␊ LF symbol
        'f' => '\u{00B0}', // ° degree sign
        'g' => '\u{00B1}', // ± plus-minus
        'h' => '\u{2424}', // ␤ NL symbol
        'i' => '\u{240B}', // ␋ VT symbol
        'j' => '\u{2518}', // ┘ lower-right corner
        'k' => '\u{2510}', // ┐ upper-right corner
        'l' => '\u{250C}', // ┌ upper-left corner
        'm' => '\u{2514}', // └ lower-left corner
        'n' => '\u{253C}', // ┼ crossing lines
        'o' => '\u{23BA}', // ⎺ scan line 1
        'p' => '\u{23BB}', // ⎻ scan line 3
        'q' => '\u{2500}', // ─ horizontal line
        'r' => '\u{23BC}', // ⎼ scan line 7
        's' => '\u{23BD}', // ⎽ scan line 9
        't' => '\u{251C}', // ├ left tee
        'u' => '\u{2524}', // ┤ right tee
        'v' => '\u{2534}', // ┴ bottom tee
        'w' => '\u{252C}', // ┬ top tee
        'x' => '\u{2502}', // │ vertical line
        'y' => '\u{2264}', // ≤ less-than-or-equal
        'z' => '\u{2265}', // ≥ greater-than-or-equal
        '{' => '\u{03C0}', // π pi
        '|' => '\u{2260}', // ≠ not-equal
        '}' => '\u{00A3}', // £ pound sign
        '~' => '\u{00B7}', // · centered dot
        _ => ch,
    }
}

/// Translate a character through the given charset designator.
///
/// - `b'B'` (ASCII): pass-through
/// - `b'0'` (DEC Special Graphics): line-drawing substitution
/// - All others: pass-through (UK charset differences are negligible)
pub fn translate_charset(ch: char, charset_designator: u8) -> char {
    match charset_designator {
        b'0' => dec_graphics_char(ch),
        _ => ch,
    }
}

impl Cursor {
    /// Get the effective charset designator for the next printed character,
    /// accounting for single-shift overrides.
    pub fn effective_charset(&self) -> u8 {
        if let Some(shift) = self.single_shift {
            let slot = (shift as usize).min(3);
            self.charset_slots[slot]
        } else {
            self.charset_slots[self.active_charset as usize & 3]
        }
    }

    /// Consume the single-shift state (call after printing a character).
    pub fn consume_single_shift(&mut self) {
        self.single_shift = None;
    }

    /// Designate a charset for a slot.
    pub fn designate_charset(&mut self, slot: u8, charset: u8) {
        let idx = (slot as usize).min(3);
        self.charset_slots[idx] = charset;
    }

    /// Reset charset state to defaults (all ASCII, G0 active, no single-shift).
    pub fn reset_charset(&mut self) {
        self.charset_slots = [b'B'; 4];
        self.active_charset = 0;
        self.single_shift = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::SgrFlags;

    #[test]
    fn default_cursor_at_origin() {
        let c = Cursor::default();
        assert_eq!(c.row, 0);
        assert_eq!(c.col, 0);
        assert!(c.visible);
        assert!(!c.pending_wrap);
    }

    #[test]
    fn cursor_new_with_dimensions() {
        let c = Cursor::new(80, 24);
        assert_eq!(c.scroll_top(), 0);
        assert_eq!(c.scroll_bottom(), 24);
    }

    #[test]
    fn cursor_at_position() {
        let c = Cursor::at(5, 10);
        assert_eq!(c.row, 5);
        assert_eq!(c.col, 10);
    }

    #[test]
    fn cursor_clamp_to_bounds() {
        let mut c = Cursor::at(100, 200);
        c.clamp(24, 80);
        assert_eq!(c.row, 23);
        assert_eq!(c.col, 79);
        assert!(!c.pending_wrap);
    }

    #[test]
    fn move_to_clamps() {
        let mut c = Cursor::new(80, 24);
        c.move_to(999, 999, 24, 80);
        assert_eq!(c.row, 23);
        assert_eq!(c.col, 79);
    }

    #[test]
    fn move_up_stops_at_scroll_top() {
        let mut c = Cursor::new(80, 24);
        c.set_scroll_region(5, 20, 24);
        c.row = 7;
        c.move_up(10);
        assert_eq!(c.row, 5);
    }

    #[test]
    fn move_down_stops_at_scroll_bottom() {
        let mut c = Cursor::new(80, 24);
        c.set_scroll_region(0, 10, 24);
        c.row = 8;
        c.move_down(10, 24);
        assert_eq!(c.row, 9); // bottom - 1
    }

    #[test]
    fn move_left_stops_at_zero() {
        let mut c = Cursor::new(80, 24);
        c.col = 3;
        c.move_left(100);
        assert_eq!(c.col, 0);
    }

    #[test]
    fn move_right_stops_at_margin() {
        let mut c = Cursor::new(80, 24);
        c.col = 70;
        c.move_right(100, 80);
        assert_eq!(c.col, 79);
    }

    #[test]
    fn carriage_return() {
        let mut c = Cursor::new(80, 24);
        c.col = 42;
        c.pending_wrap = true;
        c.carriage_return();
        assert_eq!(c.col, 0);
        assert!(!c.pending_wrap);
    }

    // ── Scroll region ───────────────────────────────────────────────

    #[test]
    fn set_scroll_region() {
        let mut c = Cursor::new(80, 24);
        c.set_scroll_region(5, 20, 24);
        assert_eq!(c.scroll_top(), 5);
        assert_eq!(c.scroll_bottom(), 20);
    }

    #[test]
    fn invalid_scroll_region_is_ignored() {
        let mut c = Cursor::new(80, 24);
        c.set_scroll_region(20, 5, 24); // top >= bottom
        assert_eq!(c.scroll_top(), 0);
        assert_eq!(c.scroll_bottom(), 24);
    }

    #[test]
    fn reset_scroll_region() {
        let mut c = Cursor::new(80, 24);
        c.set_scroll_region(5, 20, 24);
        c.reset_scroll_region(24);
        assert_eq!(c.scroll_top(), 0);
        assert_eq!(c.scroll_bottom(), 24);
    }

    #[test]
    fn in_scroll_region() {
        let mut c = Cursor::new(80, 24);
        c.set_scroll_region(5, 15, 24);
        c.row = 10;
        assert!(c.in_scroll_region());
        c.row = 3;
        assert!(!c.in_scroll_region());
        c.row = 15; // exclusive boundary
        assert!(!c.in_scroll_region());
    }

    // ── Tab stops ───────────────────────────────────────────────────

    #[test]
    fn default_tab_stops_every_8() {
        let c = Cursor::new(80, 24);
        // Tab stops at 0, 8, 16, 24, ...
        assert!(c.tab_stops[0]);
        assert!(c.tab_stops[8]);
        assert!(!c.tab_stops[7]);
        assert!(c.tab_stops[16]);
    }

    #[test]
    fn next_tab_stop() {
        let c = Cursor::new(80, 24);
        // From col 0, next tab is col 8.
        let mut c2 = c.clone();
        c2.col = 0;
        assert_eq!(c2.next_tab_stop(80), 8);

        // From col 7, next tab is col 8.
        c2.col = 7;
        assert_eq!(c2.next_tab_stop(80), 8);

        // From col 8, next tab is col 16.
        c2.col = 8;
        assert_eq!(c2.next_tab_stop(80), 16);
    }

    #[test]
    fn prev_tab_stop() {
        let c = Cursor::new(80, 24);
        let mut c2 = c.clone();
        c2.col = 10;
        assert_eq!(c2.prev_tab_stop(), 8);

        c2.col = 8;
        assert_eq!(c2.prev_tab_stop(), 0);

        c2.col = 0;
        assert_eq!(c2.prev_tab_stop(), 0);
    }

    #[test]
    fn set_and_clear_tab_stop() {
        let mut c = Cursor::new(80, 24);
        c.col = 5;
        c.set_tab_stop();
        assert!(c.tab_stops[5]);

        c.col = 5;
        c.clear_tab_stop();
        assert!(!c.tab_stops[5]);
    }

    #[test]
    fn clear_all_tab_stops() {
        let mut c = Cursor::new(80, 24);
        c.clear_all_tab_stops();
        assert!(c.tab_stops.iter().all(|&s| !s));
    }

    // ── Save/restore ────────────────────────────────────────────────

    #[test]
    fn save_restore_roundtrip() {
        let mut cursor = Cursor::at(5, 10);
        cursor.attrs.flags = SgrFlags::BOLD;
        cursor.pending_wrap = true;

        let saved = SavedCursor::save(&cursor, true);
        assert_eq!(saved.row, 5);
        assert_eq!(saved.col, 10);
        assert!(saved.origin_mode);

        let mut new_cursor = Cursor::default();
        saved.restore(&mut new_cursor);
        assert_eq!(new_cursor.row, 5);
        assert_eq!(new_cursor.col, 10);
        assert!(new_cursor.pending_wrap);
        assert_eq!(new_cursor.attrs.flags, SgrFlags::BOLD);
    }

    // ── Edge cases ──────────────────────────────────────────────────

    #[test]
    fn move_to_zero_size_grid() {
        let mut c = Cursor::default();
        c.move_to(5, 5, 0, 0);
        // saturating_sub(1) on 0 gives 0, so row and col stay 0.
        assert_eq!(c.row, 0);
        assert_eq!(c.col, 0);
    }

    #[test]
    fn tab_stop_past_end_returns_last_col() {
        let mut c = Cursor::new(10, 1);
        c.clear_all_tab_stops();
        c.col = 5;
        assert_eq!(c.next_tab_stop(10), 9);
    }

    #[test]
    fn resize_wider_sets_tab_stops_on_new_columns() {
        let mut c = Cursor::new(80, 24);
        // Clear a user-set tab stop to verify it's preserved after resize.
        c.tab_stops[8] = false;

        c.resize(120, 24);

        // Original stops preserved: col 0 still set, col 8 still cleared.
        assert!(c.tab_stops[0], "original tab stop at 0 must be preserved");
        assert!(
            !c.tab_stops[8],
            "user-cleared tab stop at 8 must remain cleared"
        );
        // New columns get default tab stops at 8-column intervals.
        assert!(c.tab_stops[80], "new column 80 must get a default tab stop");
        assert!(c.tab_stops[88], "new column 88 must get a default tab stop");
        assert!(c.tab_stops[96], "new column 96 must get a default tab stop");
        assert!(!c.tab_stops[81], "new column 81 must not have a tab stop");
    }

    #[test]
    fn resize_narrower_preserves_existing_tab_stops() {
        let mut c = Cursor::new(80, 24);
        c.resize(40, 24);

        // Tab stops within the new width are preserved.
        assert!(c.tab_stops[0]);
        assert!(c.tab_stops[8]);
        assert!(c.tab_stops[16]);
        assert!(c.tab_stops[24]);
        assert!(c.tab_stops[32]);
        assert_eq!(c.tab_stops.len(), 40);
    }

    // ── Charset ──────────────────────────────────────────────────────

    #[test]
    fn default_charset_is_ascii() {
        let c = Cursor::new(80, 24);
        assert_eq!(c.charset_slots, [b'B'; 4]);
        assert_eq!(c.active_charset, 0);
        assert!(c.single_shift.is_none());
        assert_eq!(c.effective_charset(), b'B');
    }

    #[test]
    fn designate_charset_sets_slot() {
        let mut c = Cursor::new(80, 24);
        c.designate_charset(0, b'0'); // G0 = DEC Graphics
        assert_eq!(c.charset_slots[0], b'0');
        assert_eq!(c.effective_charset(), b'0');
        // G1 still ASCII
        assert_eq!(c.charset_slots[1], b'B');
    }

    #[test]
    fn single_shift_overrides_effective_charset() {
        let mut c = Cursor::new(80, 24);
        c.charset_slots[2] = b'0'; // G2 = DEC Graphics
        c.single_shift = Some(2);
        assert_eq!(c.effective_charset(), b'0');
        c.consume_single_shift();
        assert!(c.single_shift.is_none());
        // Back to G0 ASCII
        assert_eq!(c.effective_charset(), b'B');
    }

    #[test]
    fn reset_charset_restores_defaults() {
        let mut c = Cursor::new(80, 24);
        c.charset_slots = [b'0'; 4];
        c.active_charset = 2;
        c.single_shift = Some(3);
        c.reset_charset();
        assert_eq!(c.charset_slots, [b'B'; 4]);
        assert_eq!(c.active_charset, 0);
        assert!(c.single_shift.is_none());
    }

    #[test]
    fn dec_graphics_translation() {
        use super::translate_charset;
        // Line-drawing corners
        assert_eq!(translate_charset('j', b'0'), '┘');
        assert_eq!(translate_charset('k', b'0'), '┐');
        assert_eq!(translate_charset('l', b'0'), '┌');
        assert_eq!(translate_charset('m', b'0'), '└');
        assert_eq!(translate_charset('q', b'0'), '─');
        assert_eq!(translate_charset('x', b'0'), '│');
        assert_eq!(translate_charset('n', b'0'), '┼');
        // Tees
        assert_eq!(translate_charset('t', b'0'), '├');
        assert_eq!(translate_charset('u', b'0'), '┤');
        assert_eq!(translate_charset('v', b'0'), '┴');
        assert_eq!(translate_charset('w', b'0'), '┬');
        // Symbols
        assert_eq!(translate_charset('`', b'0'), '◆');
        assert_eq!(translate_charset('a', b'0'), '▒');
        assert_eq!(translate_charset('~', b'0'), '·');
        assert_eq!(translate_charset('{', b'0'), 'π');
        // Non-Graphics range passes through
        assert_eq!(translate_charset('A', b'0'), 'A');
        // ASCII charset is pass-through
        assert_eq!(translate_charset('q', b'B'), 'q');
    }

    #[test]
    fn save_restore_preserves_charset() {
        let mut cursor = Cursor::new(80, 24);
        cursor.designate_charset(0, b'0');
        cursor.designate_charset(1, b'A');
        cursor.active_charset = 1;

        let saved = SavedCursor::save(&cursor, false);
        assert_eq!(saved.charset_slots[0], b'0');
        assert_eq!(saved.charset_slots[1], b'A');
        assert_eq!(saved.active_charset, 1);

        let mut new_cursor = Cursor::new(80, 24);
        saved.restore(&mut new_cursor);
        assert_eq!(new_cursor.charset_slots[0], b'0');
        assert_eq!(new_cursor.charset_slots[1], b'A');
        assert_eq!(new_cursor.active_charset, 1);
        assert!(new_cursor.single_shift.is_none());
    }
}

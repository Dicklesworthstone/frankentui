//! In-memory virtual terminal state machine for testing.
//!
//! `VirtualTerminal` maintains a full terminal grid with cursor tracking,
//! ANSI sequence interpretation, scrollback buffer, and state inspection
//! methods. It is designed for testing terminal applications without
//! requiring a real PTY or terminal emulator.
//!
//! # Invariants
//!
//! 1. **Cursor always in bounds**: `cursor_x <= width`, `cursor_y < height`.
//!    When `cursor_x == width`, the terminal is in "pending wrap" state:
//!    the next character will wrap to the start of the next line. This is
//!    standard terminal behavior (DECAWM). Operations that would move the
//!    cursor out of bounds clamp to edges (except wrapping, which advances
//!    to the next line).
//!
//! 2. **Grid always fully populated**: `grid.len() == width * height`.
//!    Every cell is initialized to the default (space, no style).
//!
//! 3. **Scrollback is append-only during normal operation**: Lines pushed
//!    into scrollback are never modified (new lines are appended). The
//!    scrollback may be truncated from the front when `max_scrollback`
//!    is exceeded.
//!
//! 4. **Attribute state is sticky**: SGR attributes apply to all
//!    subsequent characters until explicitly reset.
//!
//! # Failure Modes
//!
//! | Failure | Cause | Behavior |
//! |---------|-------|----------|
//! | Unrecognized CSI | Unknown terminal sequence | Silently ignored |
//! | Scrollback overflow | Excessive output | Front-truncated to `max_scrollback` |
//! | Cursor wrap past bottom | Text output fills screen | Scroll up, top line to scrollback |
//!
//! # Evidence Ledger
//!
//! | Claim | Evidence |
//! |-------|----------|
//! | Quirks are explicit, not inferred | `QuirkSet` must be passed or set explicitly; no runtime detection |
//! | Default behavior unchanged | `VirtualTerminal::new` uses `QuirkSet::empty()` |
//!
//! # Behavioral Isomorphism (Performance)
//!
//! `QuirkSet::empty()` is the identity: quirk checks short-circuit and preserve
//! the pre-existing control flow. The quirk branches are constant-time boolean
//! guards, with no extra allocations or buffering. Golden output checksums are
//! emitted in E2E JSONL logs for reproducibility.
//!
//! # Performance Profile
//!
//! - Baseline: `QuirkSet::empty()` (default).
//! - Profiles: `tmux_nested`, `gnu_screen`, `windows_console`.
//! - Opportunity Matrix:
//!   - Branch locality: keep quirk checks adjacent to affected escape handlers.
//!   - Allocation: no new buffers; reuse the existing grid and scrollback.
//!   - Diff cost: avoid extra passes over the grid in quirk branches.

use std::collections::VecDeque;
use unicode_width::UnicodeWidthChar;

/// Sentinel character used for the continuation (right) cell of a wide character.
const WIDE_CONTINUATION: char = '\0';

/// Translate a character through the DEC Special Graphics charset.
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
fn translate_charset(ch: char, designator: u8) -> char {
    match designator {
        b'0' => dec_graphics_char(ch),
        _ => ch,
    }
}

/// RGB color value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Style attributes tracked per-cell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CellStyle {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub reverse: bool,
    pub strikethrough: bool,
    pub hidden: bool,
}

impl CellStyle {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// A single cell in the virtual terminal grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VCell {
    pub ch: char,
    pub style: CellStyle,
}

impl Default for VCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: CellStyle::default(),
        }
    }
}

/// Parser state for ANSI escape sequence interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseState {
    Ground,
    Escape,
    EscapeHash,
    EscapeCharset(u8),
    Csi,
    Osc,
}

/// Terminal quirks that can be simulated by the virtual terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalQuirk {
    /// Nested tmux sessions can drop DEC save/restore in alt-screen mode.
    TmuxNestedCursorSaveRestore,
    /// GNU screen-style immediate wrap on last column writes.
    ScreenImmediateWrap,
    /// Windows console without alternate screen support.
    WindowsNoAltScreen,
}

/// Set of terminal quirks to simulate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuirkSet {
    tmux_nested_cursor: bool,
    screen_immediate_wrap: bool,
    windows_no_alt_screen: bool,
}

impl Default for QuirkSet {
    fn default() -> Self {
        Self::empty()
    }
}

impl QuirkSet {
    /// No quirks enabled.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            tmux_nested_cursor: false,
            screen_immediate_wrap: false,
            windows_no_alt_screen: false,
        }
    }

    /// Simulate nested tmux cursor save/restore quirks in alt-screen.
    #[must_use]
    pub const fn tmux_nested() -> Self {
        Self {
            tmux_nested_cursor: true,
            ..Self::empty()
        }
    }

    /// Simulate GNU screen line-wrap behavior.
    #[must_use]
    pub const fn gnu_screen() -> Self {
        Self {
            screen_immediate_wrap: true,
            ..Self::empty()
        }
    }

    /// Simulate Windows console limitations (no alt-screen).
    #[must_use]
    pub const fn windows_console() -> Self {
        Self {
            windows_no_alt_screen: true,
            ..Self::empty()
        }
    }

    /// Enable or disable the tmux nested cursor quirk.
    #[must_use]
    pub const fn with_tmux_nested_cursor(mut self, enabled: bool) -> Self {
        self.tmux_nested_cursor = enabled;
        self
    }

    /// Enable or disable the immediate wrap quirk.
    #[must_use]
    pub const fn with_screen_immediate_wrap(mut self, enabled: bool) -> Self {
        self.screen_immediate_wrap = enabled;
        self
    }

    /// Enable or disable the Windows no-alt-screen quirk.
    #[must_use]
    pub const fn with_windows_no_alt_screen(mut self, enabled: bool) -> Self {
        self.windows_no_alt_screen = enabled;
        self
    }

    /// Check if a specific quirk is enabled.
    #[must_use]
    pub const fn has(self, quirk: TerminalQuirk) -> bool {
        match quirk {
            TerminalQuirk::TmuxNestedCursorSaveRestore => self.tmux_nested_cursor,
            TerminalQuirk::ScreenImmediateWrap => self.screen_immediate_wrap,
            TerminalQuirk::WindowsNoAltScreen => self.windows_no_alt_screen,
        }
    }
}

/// In-memory virtual terminal with cursor tracking and ANSI interpretation.
///
/// # Example
///
/// ```
/// use ftui_pty::virtual_terminal::VirtualTerminal;
///
/// let mut vt = VirtualTerminal::new(80, 24);
/// vt.feed(b"Hello, World!");
/// assert_eq!(vt.char_at(0, 0), Some('H'));
/// assert_eq!(vt.char_at(12, 0), Some('!'));
/// assert_eq!(vt.cursor(), (13, 0));
/// ```
pub struct VirtualTerminal {
    width: u16,
    height: u16,
    grid: Vec<VCell>,
    cursor_x: u16,
    cursor_y: u16,
    cursor_visible: bool,
    current_style: CellStyle,
    scrollback: VecDeque<Vec<VCell>>,
    max_scrollback: usize,
    // Saved cursor position (DEC save/restore)
    saved_cursor: Option<(u16, u16)>,
    // Scroll region (top, bottom) — 0-indexed, inclusive
    scroll_top: u16,
    scroll_bottom: u16,
    // Parser state
    parse_state: ParseState,
    csi_params: Vec<u16>,
    csi_intermediate: Vec<u8>,
    osc_data: Vec<u8>,
    // Modes
    alternate_screen: bool,
    alternate_grid: Option<Vec<VCell>>,
    alternate_cursor: Option<(u16, u16)>,
    // Title
    title: String,
    quirks: QuirkSet,
    /// DECOM (DEC private mode 6): origin mode — cursor addressing relative
    /// to scroll region.
    origin_mode: bool,
    /// Last printed character for REP (CSI b) support.
    last_char: Option<char>,
    /// UTF-8 accumulator for multi-byte character decoding.
    utf8_buf: [u8; 4],
    /// Number of bytes accumulated in `utf8_buf`.
    utf8_len: u8,
    /// Expected total bytes for current UTF-8 sequence.
    utf8_expected: u8,
    /// Tab stops — `tab_stops[col]` is true if col is a tab stop.
    tab_stops: Vec<bool>,
    /// IRM (Insert/Replace Mode, ANSI mode 4): when true, printed chars
    /// insert (shift existing text right) instead of overwriting.
    insert_mode: bool,
    /// DECAWM (Auto-Wrap Mode, DEC private mode 7): when true, printing
    /// past the right margin wraps to the next line. Default: true.
    autowrap: bool,
    /// Charset designators for G0–G3 slots. b'B' = ASCII, b'0' = DEC Special Graphics.
    charset_slots: [u8; 4],
    /// Active charset slot index (0 = G0, 1 = G1).
    active_charset: u8,
    /// Single-shift override: if Some(n), next printed char uses G<n> then reverts.
    single_shift: Option<u8>,
}

impl VirtualTerminal {
    /// Create a new virtual terminal with the given dimensions.
    ///
    /// # Panics
    ///
    /// Panics if width or height is 0.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self::with_quirks(width, height, QuirkSet::default())
    }

    /// Create a new virtual terminal with quirks enabled.
    ///
    /// # Panics
    ///
    /// Panics if width or height is 0.
    #[must_use]
    pub fn with_quirks(width: u16, height: u16, quirks: QuirkSet) -> Self {
        assert!(width > 0 && height > 0, "terminal dimensions must be > 0");
        let grid = vec![VCell::default(); usize::from(width) * usize::from(height)];
        Self {
            width,
            height,
            grid,
            cursor_x: 0,
            cursor_y: 0,
            cursor_visible: true,
            current_style: CellStyle::default(),
            scrollback: VecDeque::new(),
            max_scrollback: 1000,
            saved_cursor: None,
            scroll_top: 0,
            scroll_bottom: height.saturating_sub(1),
            parse_state: ParseState::Ground,
            csi_params: Vec::new(),
            csi_intermediate: Vec::new(),
            osc_data: Vec::new(),
            alternate_screen: false,
            alternate_grid: None,
            alternate_cursor: None,
            title: String::new(),
            quirks,
            origin_mode: false,
            last_char: None,
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
            tab_stops: Self::default_tab_stops(width),
            insert_mode: false,
            autowrap: true,
            charset_slots: [b'B'; 4],
            active_charset: 0,
            single_shift: None,
        }
    }

    /// Build default tab stops every 8 columns.
    fn default_tab_stops(width: u16) -> Vec<bool> {
        (0..width).map(|c| c > 0 && c % 8 == 0).collect()
    }

    // ── Dimensions & Cursor ─────────────────────────────────────────

    /// Terminal width in columns.
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Terminal height in rows.
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Current cursor position (x, y), 0-indexed.
    #[must_use]
    pub const fn cursor(&self) -> (u16, u16) {
        (self.cursor_x, self.cursor_y)
    }

    /// Whether the cursor is currently visible.
    #[must_use]
    pub const fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Whether alternate screen mode is active.
    #[must_use]
    pub const fn is_alternate_screen(&self) -> bool {
        self.alternate_screen
    }

    /// Current window title (set via OSC 0/2).
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Active quirk set.
    #[must_use]
    pub const fn quirks(&self) -> QuirkSet {
        self.quirks
    }

    /// Override the active quirk set.
    pub fn set_quirks(&mut self, quirks: QuirkSet) {
        self.quirks = quirks;
    }

    /// Number of lines in the scrollback buffer.
    #[must_use]
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Set the maximum scrollback lines.
    pub fn set_max_scrollback(&mut self, max: usize) {
        self.max_scrollback = max;
        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        }
    }

    // ── Cell Access ─────────────────────────────────────────────────

    /// Get the character at (x, y). Returns `None` if out of bounds.
    #[must_use]
    pub fn char_at(&self, x: u16, y: u16) -> Option<char> {
        self.cell_at(x, y).map(|c| c.ch)
    }

    /// Get the style at (x, y). Returns `None` if out of bounds.
    #[must_use]
    pub fn style_at(&self, x: u16, y: u16) -> Option<&CellStyle> {
        self.cell_at(x, y).map(|c| &c.style)
    }

    /// Get a reference to the cell at (x, y). Returns `None` if out of bounds.
    #[must_use]
    pub fn cell_at(&self, x: u16, y: u16) -> Option<&VCell> {
        if x < self.width && y < self.height {
            Some(&self.grid[self.idx(x, y)])
        } else {
            None
        }
    }

    /// Get the text content of a row (trailing spaces trimmed).
    #[must_use]
    pub fn row_text(&self, y: u16) -> String {
        if y >= self.height {
            return String::new();
        }
        let start = self.idx(0, y);
        let end = start + usize::from(self.width);
        let s: String = self.grid[start..end]
            .iter()
            .filter(|c| c.ch != WIDE_CONTINUATION)
            .map(|c| c.ch)
            .collect();
        s.trim_end().to_string()
    }

    /// Get all visible text as a string (rows separated by newlines).
    #[must_use]
    pub fn screen_text(&self) -> String {
        (0..self.height)
            .map(|y| self.row_text(y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Get a scrollback line by index (0 = oldest).
    #[must_use]
    pub fn scrollback_line(&self, idx: usize) -> Option<String> {
        self.scrollback.get(idx).map(|cells| {
            let s: String = cells
                .iter()
                .filter(|c| c.ch != WIDE_CONTINUATION)
                .map(|c| c.ch)
                .collect();
            s.trim_end().to_string()
        })
    }

    // ── Input Processing ────────────────────────────────────────────

    /// Feed raw bytes into the terminal (ANSI-aware).
    pub fn feed(&mut self, data: &[u8]) {
        for &byte in data {
            self.process_byte(byte);
        }
    }

    /// Feed a string into the terminal.
    pub fn feed_str(&mut self, s: &str) {
        self.feed(s.as_bytes());
    }

    // ── Query Responses ─────────────────────────────────────────────

    /// Generate a cursor position report (CPR) response.
    /// Format: `ESC [ Py ; Px R` (1-indexed).
    #[must_use]
    pub fn cpr_response(&self) -> Vec<u8> {
        format!("\x1b[{};{}R", self.cursor_y + 1, self.cursor_x + 1).into_bytes()
    }

    /// Generate a device attributes (DA1) response.
    /// Reports as a VT220 with ANSI color.
    #[must_use]
    pub fn da1_response(&self) -> Vec<u8> {
        b"\x1b[?62;22c".to_vec()
    }

    // ── Internal ────────────────────────────────────────────────────

    fn idx(&self, x: u16, y: u16) -> usize {
        usize::from(y) * usize::from(self.width) + usize::from(x)
    }

    fn process_byte(&mut self, byte: u8) {
        match self.parse_state {
            ParseState::Ground => self.ground(byte),
            ParseState::Escape => self.escape(byte),
            ParseState::EscapeHash => self.escape_hash(byte),
            ParseState::EscapeCharset(slot) => self.escape_charset(slot, byte),
            ParseState::Csi => self.csi(byte),
            ParseState::Osc => self.osc(byte),
        }
    }

    fn ground(&mut self, byte: u8) {
        match byte {
            0x1b => {
                self.parse_state = ParseState::Escape;
            }
            b'\n' => {
                self.linefeed();
            }
            b'\r' => {
                self.cursor_x = 0;
            }
            b'\x08' => {
                // Backspace
                self.cursor_x = self.cursor_x.saturating_sub(1);
            }
            b'\t' => {
                // Tab: advance to next tab stop
                let max_col = self.width.saturating_sub(1);
                let mut col = self.cursor_x + 1;
                while col < self.width {
                    if self.tab_stops[usize::from(col)] {
                        break;
                    }
                    col += 1;
                }
                self.cursor_x = col.min(max_col);
            }
            b'\x07' => {
                // Bell: ignored
            }
            b'\x0e' => {
                // SO: Shift Out — activate G1 charset
                self.active_charset = 1;
            }
            b'\x0f' => {
                // SI: Shift In — activate G0 charset
                self.active_charset = 0;
            }
            0x20..=0x7e => {
                self.put_char(byte as char);
            }
            0xc2..=0xdf => {
                // UTF-8 2-byte lead
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_expected = 2;
            }
            0xe0..=0xef => {
                // UTF-8 3-byte lead
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_expected = 3;
            }
            0xf0..=0xf4 => {
                // UTF-8 4-byte lead
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_expected = 4;
            }
            0x80..=0xbf if self.utf8_len > 0 => {
                // UTF-8 continuation byte
                let idx = usize::from(self.utf8_len);
                self.utf8_buf[idx] = byte;
                self.utf8_len += 1;
                if self.utf8_len == self.utf8_expected {
                    let len = usize::from(self.utf8_len);
                    let mut buf = [0u8; 4];
                    buf[..len].copy_from_slice(&self.utf8_buf[..len]);
                    self.utf8_len = 0;
                    self.utf8_expected = 0;
                    if let Ok(decoded) = std::str::from_utf8(&buf[..len]) {
                        for ch in decoded.chars() {
                            self.put_char(ch);
                        }
                    }
                }
            }
            _ => {
                // Invalid sequence or control char: reset UTF-8 accumulator
                self.utf8_len = 0;
                self.utf8_expected = 0;
            }
        }
    }

    fn escape(&mut self, byte: u8) {
        match byte {
            b'[' => {
                self.parse_state = ParseState::Csi;
                self.csi_params.clear();
                self.csi_intermediate.clear();
            }
            b']' => {
                self.parse_state = ParseState::Osc;
                self.osc_data.clear();
            }
            b'7' => {
                // DEC save cursor
                if !(self.quirks.tmux_nested_cursor && self.alternate_screen) {
                    self.saved_cursor = Some((self.cursor_x, self.cursor_y));
                }
                self.parse_state = ParseState::Ground;
            }
            b'8' => {
                // DEC restore cursor
                if !(self.quirks.tmux_nested_cursor && self.alternate_screen)
                    && let Some((x, y)) = self.saved_cursor
                {
                    self.cursor_x = x.min(self.width.saturating_sub(1));
                    self.cursor_y = y.min(self.height.saturating_sub(1));
                }
                self.parse_state = ParseState::Ground;
            }
            b'H' => {
                // HTS: set tab stop at current cursor column
                let col = usize::from(self.cursor_x);
                if col < self.tab_stops.len() {
                    self.tab_stops[col] = true;
                }
                self.parse_state = ParseState::Ground;
            }
            b'D' => {
                // Index (scroll up)
                self.linefeed();
                self.parse_state = ParseState::Ground;
            }
            b'E' => {
                // Next Line (NEL): CR + LF
                self.cursor_x = 0;
                self.linefeed();
                self.parse_state = ParseState::Ground;
            }
            b'M' => {
                // Reverse index (scroll down)
                if self.cursor_y == self.scroll_top {
                    self.scroll_down();
                } else {
                    self.cursor_y = self.cursor_y.saturating_sub(1);
                }
                self.parse_state = ParseState::Ground;
            }
            b'#' => {
                // ESC # — enter hash sub-state for DECALN etc.
                self.parse_state = ParseState::EscapeHash;
            }
            b'(' => self.parse_state = ParseState::EscapeCharset(0), // G0
            b')' => self.parse_state = ParseState::EscapeCharset(1), // G1
            b'*' => self.parse_state = ParseState::EscapeCharset(2), // G2
            b'+' => self.parse_state = ParseState::EscapeCharset(3), // G3
            b'N' => {
                // Single Shift 2 (SS2): next char from G2
                self.single_shift = Some(2);
                self.parse_state = ParseState::Ground;
            }
            b'O' => {
                // Single Shift 3 (SS3): next char from G3
                self.single_shift = Some(3);
                self.parse_state = ParseState::Ground;
            }
            b'n' => {
                // Locking Shift 2 (LS2): invoke G2 into GL
                self.active_charset = 2;
                self.parse_state = ParseState::Ground;
            }
            b'o' => {
                // Locking Shift 3 (LS3): invoke G3 into GL
                self.active_charset = 3;
                self.parse_state = ParseState::Ground;
            }
            b'c' => {
                // Full reset (RIS)
                self.reset();
                self.parse_state = ParseState::Ground;
            }
            _ => {
                // Unknown escape: return to ground
                self.parse_state = ParseState::Ground;
            }
        }
    }

    fn escape_hash(&mut self, byte: u8) {
        if byte == b'8' {
            // DECALN: fill entire screen with 'E', reset scroll region, cursor to origin.
            for cell in self.grid.iter_mut() {
                *cell = VCell {
                    ch: 'E',
                    style: CellStyle::default(),
                };
            }
            self.scroll_top = 0;
            self.scroll_bottom = self.height.saturating_sub(1);
            self.cursor_x = 0;
            self.cursor_y = 0;
        }
        self.parse_state = ParseState::Ground;
    }

    fn escape_charset(&mut self, slot: u8, byte: u8) {
        // The designator byte selects the charset: b'B' = ASCII, b'0' = DEC Special Graphics, etc.
        let idx = (slot as usize).min(3);
        self.charset_slots[idx] = byte;
        self.parse_state = ParseState::Ground;
    }

    fn csi(&mut self, byte: u8) {
        match byte {
            b'0'..=b'9' => {
                let digit = u16::from(byte - b'0');
                if let Some(last) = self.csi_params.last_mut() {
                    *last = last.saturating_mul(10).saturating_add(digit);
                } else {
                    self.csi_params.push(digit);
                }
            }
            b';' => {
                if self.csi_params.is_empty() {
                    self.csi_params.push(0);
                }
                self.csi_params.push(0);
            }
            b'?' | b'>' | b'!' | b' ' => {
                self.csi_intermediate.push(byte);
            }
            0x40..=0x7e => {
                // Final byte — dispatch
                self.dispatch_csi(byte);
                self.parse_state = ParseState::Ground;
            }
            _ => {
                // Invalid: abort CSI
                self.parse_state = ParseState::Ground;
            }
        }
    }

    fn osc(&mut self, byte: u8) {
        match byte {
            0x07 => {
                // BEL terminates OSC
                self.dispatch_osc();
                self.parse_state = ParseState::Ground;
            }
            0x1b => {
                // Could be ST (\x1b\\) — simplified: just end OSC
                self.dispatch_osc();
                self.parse_state = ParseState::Ground;
            }
            _ => {
                self.osc_data.push(byte);
            }
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8) {
        let params = &self.csi_params;
        let has_question = self.csi_intermediate.contains(&b'?');

        match final_byte {
            b'A' => {
                // Cursor Up
                let n = Self::param(params, 0, 1);
                self.cursor_y = self.cursor_y.saturating_sub(n);
            }
            b'B' => {
                // Cursor Down
                let n = Self::param(params, 0, 1);
                self.cursor_y = (self.cursor_y + n).min(self.height.saturating_sub(1));
            }
            b'C' => {
                // Cursor Forward
                let n = Self::param(params, 0, 1);
                self.cursor_x = (self.cursor_x + n).min(self.width.saturating_sub(1));
            }
            b'D' => {
                // Cursor Back
                let n = Self::param(params, 0, 1);
                self.cursor_x = self.cursor_x.saturating_sub(n);
            }
            b'E' => {
                // Cursor Next Line
                let n = Self::param(params, 0, 1);
                self.cursor_y = (self.cursor_y + n).min(self.height.saturating_sub(1));
                self.cursor_x = 0;
            }
            b'F' => {
                // Cursor Previous Line
                let n = Self::param(params, 0, 1);
                self.cursor_y = self.cursor_y.saturating_sub(n);
                self.cursor_x = 0;
            }
            b'G' => {
                // Cursor Horizontal Absolute (1-indexed)
                let col = Self::param(params, 0, 1).saturating_sub(1);
                self.cursor_x = col.min(self.width.saturating_sub(1));
            }
            b'H' | b'f' => {
                // Cursor Position (1-indexed)
                let row = Self::param(params, 0, 1).saturating_sub(1);
                let col = Self::param(params, 1, 1).saturating_sub(1);
                if self.origin_mode {
                    let abs_row = row.saturating_add(self.scroll_top);
                    self.cursor_y = abs_row.min(self.scroll_bottom);
                } else {
                    self.cursor_y = row.min(self.height.saturating_sub(1));
                }
                self.cursor_x = col.min(self.width.saturating_sub(1));
            }
            b'J' => {
                // Erase in Display
                let mode = Self::param(params, 0, 0);
                self.erase_display(mode);
            }
            b'K' => {
                // Erase in Line
                let mode = Self::param(params, 0, 0);
                self.erase_line(mode);
            }
            b'L' => {
                // Insert Lines (IL) — insert blank lines at cursor row, pushing down
                let n = Self::param(params, 0, 1);
                if self.cursor_y >= self.scroll_top && self.cursor_y <= self.scroll_bottom {
                    let blank = self.styled_blank();
                    for _ in 0..n {
                        // Shift lines down from cursor_y to scroll_bottom
                        for row in (self.cursor_y + 1..=self.scroll_bottom).rev() {
                            let src_start = self.idx(0, row - 1);
                            let dst_start = self.idx(0, row);
                            let w = usize::from(self.width);
                            if src_start < dst_start {
                                let (left, right) = self.grid.split_at_mut(dst_start);
                                right[..w].clone_from_slice(&left[src_start..src_start + w]);
                            }
                        }
                        // Clear the line at cursor_y
                        let row_start = self.idx(0, self.cursor_y);
                        for i in 0..usize::from(self.width) {
                            self.grid[row_start + i] = blank.clone();
                        }
                    }
                }
            }
            b'M' => {
                // Delete Lines (DL) — delete lines at cursor row, pulling up
                let n = Self::param(params, 0, 1);
                if self.cursor_y >= self.scroll_top && self.cursor_y <= self.scroll_bottom {
                    let blank = self.styled_blank();
                    for _ in 0..n {
                        // Shift lines up from cursor_y to scroll_bottom
                        for row in self.cursor_y..self.scroll_bottom {
                            let src_start = self.idx(0, row + 1);
                            let dst_start = self.idx(0, row);
                            let w = usize::from(self.width);
                            let (left, right) = self.grid.split_at_mut(src_start);
                            left[dst_start..dst_start + w].clone_from_slice(&right[..w]);
                        }
                        // Clear the bottom line of scroll region
                        let bottom_start = self.idx(0, self.scroll_bottom);
                        for i in 0..usize::from(self.width) {
                            self.grid[bottom_start + i] = blank.clone();
                        }
                    }
                }
            }
            b'S' => {
                // Scroll Up
                let n = Self::param(params, 0, 1);
                for _ in 0..n {
                    self.scroll_up();
                }
            }
            b'T' => {
                // Scroll Down
                let n = Self::param(params, 0, 1);
                for _ in 0..n {
                    self.scroll_down();
                }
            }
            b'd' => {
                // Vertical Position Absolute (1-indexed)
                let row = Self::param(params, 0, 1).saturating_sub(1);
                if self.origin_mode {
                    let abs_row = row.saturating_add(self.scroll_top);
                    self.cursor_y = abs_row.min(self.scroll_bottom);
                } else {
                    self.cursor_y = row.min(self.height.saturating_sub(1));
                }
            }
            b'm' => {
                // SGR
                self.dispatch_sgr();
            }
            b'n' => {
                // Device Status Report (we track but don't auto-respond)
                // Response generated via cpr_response()
            }
            b'r' => {
                // Set Scrolling Region (DECSTBM, 1-indexed)
                let top = Self::param(params, 0, 1).saturating_sub(1);
                let bottom = Self::param(params, 1, self.height).saturating_sub(1);
                if top < bottom && bottom < self.height {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                }
                self.cursor_x = 0;
                if self.origin_mode {
                    self.cursor_y = self.scroll_top;
                } else {
                    self.cursor_y = 0;
                }
            }
            b'@' => {
                // Insert Characters (ICH) — shift chars right at cursor, insert blanks
                let n = Self::param(params, 0, 1);
                let n = n.min(self.width.saturating_sub(self.cursor_x));
                // Wide char fixup: clean up at cursor position before shift
                self.fixup_wide_erase_row(self.cursor_y, self.cursor_x, n);
                let row_start = self.idx(0, self.cursor_y);
                let w = usize::from(self.width);
                let cx = usize::from(self.cursor_x);
                let count = usize::from(n);
                // Wide char fixup: if the shift pushes a wide lead's continuation off-screen,
                // blank the lead (it would end up at w-count-1 after shift if continuation was at w-count)
                if count < w {
                    let cutoff = w - count;
                    if cutoff > 0
                        && cutoff < w
                        && self.grid[row_start + cutoff].ch == WIDE_CONTINUATION
                    {
                        self.grid[row_start + cutoff - 1] = VCell::default();
                    }
                }
                // Shift characters right within the row
                let blank = self.styled_blank();
                let row = &mut self.grid[row_start..row_start + w];
                row[cx..].rotate_right(count.min(w - cx));
                // Clear the inserted positions
                for cell in row.iter_mut().skip(cx).take(count.min(w - cx)) {
                    *cell = blank.clone();
                }
                // Post-shift fixup: if the cell right after the inserted blanks is
                // an orphaned WIDE_CONTINUATION (shifted from cursor pos), blank it
                if cx + count < w && row[cx + count].ch == WIDE_CONTINUATION {
                    row[cx + count] = blank.clone();
                }
            }
            b'P' => {
                // Delete Characters (DCH) — shift chars left at cursor, fill blanks at end
                let n = Self::param(params, 0, 1);
                let n = n.min(self.width.saturating_sub(self.cursor_x));
                // Wide char fixup at delete boundaries
                self.fixup_wide_erase_row(self.cursor_y, self.cursor_x, n);
                let blank = self.styled_blank();
                let row_start = self.idx(0, self.cursor_y);
                let w = usize::from(self.width);
                let cx = usize::from(self.cursor_x);
                let count = usize::from(n);
                // Shift characters left within the row
                let row = &mut self.grid[row_start..row_start + w];
                row[cx..].rotate_left(count.min(w - cx));
                // Clear the vacated positions at end
                for cell in row.iter_mut().skip(w - count.min(w - cx)) {
                    *cell = blank.clone();
                }
            }
            b'X' => {
                // Erase Characters (ECH) — erase N chars from cursor without moving cursor
                let n = Self::param(params, 0, 1);
                let n = n.min(self.width.saturating_sub(self.cursor_x));
                self.fixup_wide_erase_row(self.cursor_y, self.cursor_x, n);
                let blank = self.styled_blank();
                let start = self.idx(self.cursor_x, self.cursor_y);
                for i in 0..usize::from(n) {
                    self.grid[start + i] = blank.clone();
                }
            }
            b'b' => {
                // Repeat Character (REP) — repeat last printed character N times
                let n = Self::param(params, 0, 1);
                if let Some(ch) = self.last_char {
                    for _ in 0..n {
                        self.put_char(ch);
                    }
                }
            }
            b'Z' => {
                // CBT: Cursor Backward Tabulation — move to previous tab stop
                let n = Self::param(params, 0, 1);
                for _ in 0..n {
                    if self.cursor_x == 0 {
                        break;
                    }
                    let mut col = self.cursor_x - 1;
                    loop {
                        if self.tab_stops[usize::from(col)] {
                            break;
                        }
                        if col == 0 {
                            break;
                        }
                        col -= 1;
                    }
                    self.cursor_x = col;
                }
            }
            b'g' => {
                // TBC: Tab Clear
                let mode = Self::param(params, 0, 0);
                match mode {
                    0 => {
                        // Clear tab stop at current column
                        let col = usize::from(self.cursor_x);
                        if col < self.tab_stops.len() {
                            self.tab_stops[col] = false;
                        }
                    }
                    3 | 5 => {
                        // Clear all tab stops
                        self.tab_stops.fill(false);
                    }
                    _ => {}
                }
            }
            b'p' if self.csi_intermediate.contains(&b'!') => {
                // Soft Reset (DECSTR) — CSI ! p
                self.current_style = CellStyle::default();
                self.cursor_visible = true;
                self.origin_mode = false;
                self.scroll_top = 0;
                self.scroll_bottom = self.height.saturating_sub(1);
                self.insert_mode = false;
                self.autowrap = true;
                self.charset_slots = [b'B'; 4];
                self.active_charset = 0;
                self.single_shift = None;
            }
            b'h' if has_question => {
                // DEC Private Mode Set
                let modes: Vec<u16> = self.csi_params.clone();
                for p in modes {
                    self.set_dec_mode(p, true);
                }
            }
            b'l' if has_question => {
                // DEC Private Mode Reset
                let modes: Vec<u16> = self.csi_params.clone();
                for p in modes {
                    self.set_dec_mode(p, false);
                }
            }
            b'h' if !has_question => {
                // ANSI Mode Set
                let modes: Vec<u16> = self.csi_params.clone();
                for p in modes {
                    self.set_ansi_mode(p, true);
                }
            }
            b'l' if !has_question => {
                // ANSI Mode Reset
                let modes: Vec<u16> = self.csi_params.clone();
                for p in modes {
                    self.set_ansi_mode(p, false);
                }
            }
            _ => {
                // Unknown CSI: ignored
            }
        }
    }

    fn dispatch_sgr(&mut self) {
        if self.csi_params.is_empty() {
            self.current_style.reset();
            return;
        }

        let params = self.csi_params.clone();
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => self.current_style.reset(),
                1 => self.current_style.bold = true,
                2 => self.current_style.dim = true,
                3 => self.current_style.italic = true,
                4 => self.current_style.underline = true,
                5 => self.current_style.blink = true,
                7 => self.current_style.reverse = true,
                8 => self.current_style.hidden = true,
                9 => self.current_style.strikethrough = true,
                22 => {
                    self.current_style.bold = false;
                    self.current_style.dim = false;
                }
                23 => self.current_style.italic = false,
                24 => self.current_style.underline = false,
                25 => self.current_style.blink = false,
                27 => self.current_style.reverse = false,
                28 => self.current_style.hidden = false,
                29 => self.current_style.strikethrough = false,
                // Standard foreground colors
                30..=37 => {
                    self.current_style.fg = Some(ansi_color(params[i] - 30));
                }
                38 => {
                    // Extended foreground
                    if let Some(color) = parse_extended_color(&params, &mut i) {
                        self.current_style.fg = Some(color);
                    }
                }
                39 => self.current_style.fg = None,
                // Standard background colors
                40..=47 => {
                    self.current_style.bg = Some(ansi_color(params[i] - 40));
                }
                48 => {
                    // Extended background
                    if let Some(color) = parse_extended_color(&params, &mut i) {
                        self.current_style.bg = Some(color);
                    }
                }
                49 => self.current_style.bg = None,
                // Bright foreground colors
                90..=97 => {
                    self.current_style.fg = Some(ansi_bright_color(params[i] - 90));
                }
                // Bright background colors
                100..=107 => {
                    self.current_style.bg = Some(ansi_bright_color(params[i] - 100));
                }
                _ => {} // Unknown SGR param: ignored
            }
            i += 1;
        }
    }

    fn dispatch_osc(&mut self) {
        let data = String::from_utf8_lossy(&self.osc_data).to_string();
        if let Some(rest) = data.strip_prefix("0;").or_else(|| data.strip_prefix("2;")) {
            self.title = rest.to_string();
        }
        // Other OSC codes (8 for hyperlinks, etc.) can be added later
    }

    fn set_dec_mode(&mut self, mode: u16, enable: bool) {
        match mode {
            6 => {
                // DECOM: origin mode — cursor addressing relative to scroll region.
                self.origin_mode = enable;
                // Enabling DECOM homes cursor to top of scroll region;
                // disabling homes to (0,0).
                if enable {
                    self.cursor_x = 0;
                    self.cursor_y = self.scroll_top;
                } else {
                    self.cursor_x = 0;
                    self.cursor_y = 0;
                }
            }
            7 => self.autowrap = enable,
            25 => self.cursor_visible = enable,
            1049 => {
                // Alternate screen buffer
                if self.quirks.windows_no_alt_screen {
                    return;
                }
                if enable && !self.alternate_screen {
                    self.alternate_grid = Some(std::mem::replace(
                        &mut self.grid,
                        vec![VCell::default(); usize::from(self.width) * usize::from(self.height)],
                    ));
                    self.alternate_cursor = Some((self.cursor_x, self.cursor_y));
                    self.cursor_x = 0;
                    self.cursor_y = 0;
                    self.alternate_screen = true;
                } else if !enable && self.alternate_screen {
                    if let Some(main_grid) = self.alternate_grid.take() {
                        self.grid = main_grid;
                    }
                    if let Some((x, y)) = self.alternate_cursor.take() {
                        self.cursor_x = x;
                        self.cursor_y = y;
                    }
                    self.alternate_screen = false;
                }
            }
            1047 => {
                // Alternate screen (without save/restore cursor)
                if self.quirks.windows_no_alt_screen {
                    return;
                }
                if enable && !self.alternate_screen {
                    self.alternate_grid = Some(std::mem::replace(
                        &mut self.grid,
                        vec![VCell::default(); usize::from(self.width) * usize::from(self.height)],
                    ));
                    self.alternate_screen = true;
                } else if !enable && self.alternate_screen {
                    if let Some(main_grid) = self.alternate_grid.take() {
                        self.grid = main_grid;
                    }
                    self.alternate_screen = false;
                }
            }
            _ => {
                // Other DEC modes: ignored (1000/1002/1006 mouse, 2004 paste, etc.)
            }
        }
    }

    fn set_ansi_mode(&mut self, mode: u16, enable: bool) {
        if mode == 4 {
            self.insert_mode = enable;
        }
    }

    fn put_char(&mut self, ch: char) {
        // Charset translation: resolve effective charset and translate
        let designator = if let Some(shift) = self.single_shift {
            let slot = (shift as usize).min(3);
            self.single_shift = None;
            self.charset_slots[slot]
        } else {
            self.charset_slots[(self.active_charset as usize) & 3]
        };
        let ch = translate_charset(ch, designator);

        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if char_width == 0 {
            return; // zero-width (combining marks, ZWJ): skip
        }

        // Auto-wrap: if cursor is past right margin and autowrap is on, wrap
        if self.cursor_x >= self.width {
            if self.autowrap {
                self.cursor_x = 0;
                self.linefeed();
            } else {
                // No auto-wrap: clamp to last column, overwrite in place
                self.cursor_x = self.width.saturating_sub(1);
            }
        }

        // Wide char at last column → wrap first (only if autowrap)
        if char_width == 2 && self.cursor_x + 1 >= self.width {
            if self.autowrap {
                let idx = self.idx(self.cursor_x, self.cursor_y);
                self.grid[idx] = VCell::default();
                self.cursor_x = 0;
                self.linefeed();
            } else {
                // No wrap: clamp to last column
                self.cursor_x = self.width.saturating_sub(1);
            }
        }

        let last_col = self.width.saturating_sub(1);
        let immediate_wrap = self.quirks.screen_immediate_wrap && self.cursor_x == last_col;
        let idx = self.idx(self.cursor_x, self.cursor_y);

        // IRM: insert mode — shift existing chars right before placing
        if self.insert_mode {
            let row_start = self.idx(0, self.cursor_y);
            let w = usize::from(self.width);
            let cx = usize::from(self.cursor_x);
            let shift = usize::from(u16::try_from(char_width).unwrap_or(1));
            let row = &mut self.grid[row_start..row_start + w];
            if cx + shift <= w {
                row[cx..].rotate_right(shift.min(w - cx));
            }
        }

        // Fixup: overwriting a continuation → blank the lead
        if self.grid[idx].ch == WIDE_CONTINUATION && self.cursor_x > 0 {
            let lead_idx = self.idx(self.cursor_x - 1, self.cursor_y);
            self.grid[lead_idx] = VCell::default();
        }
        // Fixup: narrow char overwrites a wide lead → blank its continuation
        if char_width == 1 && self.cursor_x + 1 < self.width {
            let next_idx = idx + 1;
            if self.grid[next_idx].ch == WIDE_CONTINUATION {
                self.grid[next_idx] = VCell::default();
            }
        }

        self.grid[idx] = VCell {
            ch,
            style: self.current_style.clone(),
        };

        // Wide char: place continuation in next cell
        if char_width == 2 && self.cursor_x + 1 < self.width {
            let cont_idx = idx + 1;
            self.grid[cont_idx] = VCell {
                ch: WIDE_CONTINUATION,
                style: self.current_style.clone(),
            };
        }

        self.last_char = Some(ch);
        let advance = u16::try_from(char_width).unwrap_or(1);
        if immediate_wrap {
            self.cursor_x = 0;
            self.linefeed();
        } else if self.autowrap {
            self.cursor_x += advance;
        } else {
            // No auto-wrap: clamp to last column
            self.cursor_x = (self.cursor_x + advance).min(self.width.saturating_sub(1));
        }
    }

    fn linefeed(&mut self) {
        if self.cursor_y == self.scroll_bottom {
            self.scroll_up();
        } else if self.cursor_y < self.height.saturating_sub(1) {
            self.cursor_y += 1;
        }
    }

    fn scroll_up(&mut self) {
        // Push the top line of the scroll region into scrollback
        let top_start = self.idx(0, self.scroll_top);
        let top_end = top_start + usize::from(self.width);
        let line: Vec<VCell> = self.grid[top_start..top_end].to_vec();
        self.scrollback.push_back(line);
        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        }

        // Shift lines up within scroll region
        for row in self.scroll_top..self.scroll_bottom {
            let src_start = self.idx(0, row + 1);
            let dst_start = self.idx(0, row);
            let w = usize::from(self.width);
            // Copy within the same vec using split_at_mut pattern
            let (left, right) = self.grid.split_at_mut(src_start);
            left[dst_start..dst_start + w].clone_from_slice(&right[..w]);
        }

        // Clear the bottom line of scroll region
        let blank = self.styled_blank();
        let bottom_start = self.idx(0, self.scroll_bottom);
        for i in 0..usize::from(self.width) {
            self.grid[bottom_start + i] = blank.clone();
        }
    }

    fn scroll_down(&mut self) {
        // Shift lines down within scroll region
        for row in (self.scroll_top + 1..=self.scroll_bottom).rev() {
            let src_start = self.idx(0, row - 1);
            let dst_start = self.idx(0, row);
            let w = usize::from(self.width);
            if src_start < dst_start {
                let (left, right) = self.grid.split_at_mut(dst_start);
                right[..w].clone_from_slice(&left[src_start..src_start + w]);
            }
        }

        // Clear the top line of scroll region
        let blank = self.styled_blank();
        let top_start = self.idx(0, self.scroll_top);
        for i in 0..usize::from(self.width) {
            self.grid[top_start + i] = blank.clone();
        }
    }

    /// A blank cell carrying the current SGR attributes (bg color, etc.).
    /// Per VT spec, erase/edit operations fill blanks with the current style.
    fn styled_blank(&self) -> VCell {
        VCell {
            ch: ' ',
            style: self.current_style.clone(),
        }
    }

    /// Clean up wide character boundaries before an erase/edit in a single row.
    /// Checks cells at the edges of the range `[start_col, start_col+count)` and
    /// blanks orphaned lead/continuation cells.
    fn fixup_wide_erase_row(&mut self, row_y: u16, start_col: u16, count: u16) {
        let w = self.width;
        let sc = start_col;
        let n = count;
        if n == 0 || sc >= w {
            return;
        }
        let row_start = self.idx(0, row_y);
        // If the first erased cell is a continuation, its lead is orphaned → blank lead
        if sc > 0 && self.grid[row_start + usize::from(sc)].ch == WIDE_CONTINUATION {
            self.grid[row_start + usize::from(sc - 1)] = VCell::default();
        }
        // If the cell just after the erased range is a continuation, it's orphaned → blank it
        let end_col = sc.saturating_add(n);
        if end_col < w && self.grid[row_start + usize::from(end_col)].ch == WIDE_CONTINUATION {
            self.grid[row_start + usize::from(end_col)] = VCell::default();
        }
    }

    fn erase_display(&mut self, mode: u16) {
        let blank = self.styled_blank();
        match mode {
            0 => {
                // Erase from cursor to end
                let count = self.width.saturating_sub(self.cursor_x);
                self.fixup_wide_erase_row(self.cursor_y, self.cursor_x, count);
                let start = self.idx(self.cursor_x, self.cursor_y);
                for cell in &mut self.grid[start..] {
                    *cell = blank.clone();
                }
            }
            1 => {
                // Erase from start to cursor (inclusive)
                let count = self.cursor_x + 1;
                self.fixup_wide_erase_row(self.cursor_y, 0, count);
                let end = self.idx(self.cursor_x, self.cursor_y) + 1;
                for cell in &mut self.grid[..end] {
                    *cell = blank.clone();
                }
            }
            2 | 3 => {
                // Erase entire display (3 also clears scrollback)
                for cell in &mut self.grid {
                    *cell = blank.clone();
                }
                if mode == 3 {
                    self.scrollback.clear();
                }
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let y = self.cursor_y;
        let blank = self.styled_blank();
        let row_start = self.idx(0, y);
        match mode {
            0 => {
                // Erase from cursor to end of line
                let count = self.width.saturating_sub(self.cursor_x);
                self.fixup_wide_erase_row(y, self.cursor_x, count);
                let start = row_start + usize::from(self.cursor_x);
                let end = row_start + usize::from(self.width);
                for cell in &mut self.grid[start..end] {
                    *cell = blank.clone();
                }
            }
            1 => {
                // Erase from start to cursor (inclusive)
                let count = self.cursor_x + 1;
                self.fixup_wide_erase_row(y, 0, count);
                let end = row_start + usize::from(count);
                for cell in &mut self.grid[row_start..end] {
                    *cell = blank.clone();
                }
            }
            2 => {
                // Erase entire line (no boundary fixup needed — whole row)
                let end = row_start + usize::from(self.width);
                for cell in &mut self.grid[row_start..end] {
                    *cell = blank.clone();
                }
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.grid = vec![VCell::default(); usize::from(self.width) * usize::from(self.height)];
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.cursor_visible = true;
        self.current_style = CellStyle::default();
        self.scrollback.clear();
        self.saved_cursor = None;
        self.scroll_top = 0;
        self.scroll_bottom = self.height.saturating_sub(1);
        self.title.clear();
        self.alternate_screen = false;
        self.alternate_grid = None;
        self.alternate_cursor = None;
        self.last_char = None;
        self.utf8_len = 0;
        self.utf8_expected = 0;
        self.tab_stops = Self::default_tab_stops(self.width);
        self.insert_mode = false;
        self.autowrap = true;
        self.charset_slots = [b'B'; 4];
        self.active_charset = 0;
        self.single_shift = None;
    }

    fn param(params: &[u16], idx: usize, default: u16) -> u16 {
        params
            .get(idx)
            .copied()
            .filter(|&v| v > 0)
            .unwrap_or(default)
    }
}

// ── Color helpers ───────────────────────────────────────────────────

fn ansi_color(idx: u16) -> Color {
    match idx {
        0 => Color::new(0, 0, 0),       // Black
        1 => Color::new(170, 0, 0),     // Red
        2 => Color::new(0, 170, 0),     // Green
        3 => Color::new(170, 170, 0),   // Yellow
        4 => Color::new(0, 0, 170),     // Blue
        5 => Color::new(170, 0, 170),   // Magenta
        6 => Color::new(0, 170, 170),   // Cyan
        7 => Color::new(170, 170, 170), // White
        _ => Color::default(),
    }
}

fn ansi_bright_color(idx: u16) -> Color {
    match idx {
        0 => Color::new(85, 85, 85),    // Bright Black
        1 => Color::new(255, 85, 85),   // Bright Red
        2 => Color::new(85, 255, 85),   // Bright Green
        3 => Color::new(255, 255, 85),  // Bright Yellow
        4 => Color::new(85, 85, 255),   // Bright Blue
        5 => Color::new(255, 85, 255),  // Bright Magenta
        6 => Color::new(85, 255, 255),  // Bright Cyan
        7 => Color::new(255, 255, 255), // Bright White
        _ => Color::default(),
    }
}

/// Parse extended color (38;2;r;g;b or 38;5;idx).
fn parse_extended_color(params: &[u16], i: &mut usize) -> Option<Color> {
    if *i + 1 >= params.len() {
        return None;
    }
    match params[*i + 1] {
        2 => {
            // Truecolor: 38;2;r;g;b
            if *i + 4 < params.len() {
                let r = params[*i + 2] as u8;
                let g = params[*i + 3] as u8;
                let b = params[*i + 4] as u8;
                *i += 4;
                Some(Color::new(r, g, b))
            } else {
                None
            }
        }
        5 => {
            // 256-color: 38;5;idx
            if *i + 2 < params.len() {
                let idx = params[*i + 2];
                *i += 2;
                Some(color_256(idx))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Convert 256-color index to RGB.
fn color_256(idx: u16) -> Color {
    match idx {
        0..=7 => ansi_color(idx),
        8..=15 => ansi_bright_color(idx - 8),
        16..=231 => {
            // 6x6x6 color cube
            let n = idx - 16;
            let b = (n % 6) as u8;
            let g = ((n / 6) % 6) as u8;
            let r = (n / 36) as u8;
            let to_rgb = |v: u8| if v == 0 { 0u8 } else { 55 + 40 * v };
            Color::new(to_rgb(r), to_rgb(g), to_rgb(b))
        }
        232..=255 => {
            // Grayscale ramp
            let v = (8 + 10 * (idx - 232)) as u8;
            Color::new(v, v, v)
        }
        _ => Color::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_invariants(vt: &VirtualTerminal) {
        // cursor_x == width is valid: it's the "pending wrap" state
        assert!(vt.cursor_x <= vt.width);
        assert!(vt.cursor_y < vt.height);
        assert_eq!(vt.grid.len(), vt.width as usize * vt.height as usize);
        assert!(vt.scroll_top <= vt.scroll_bottom);
        assert!(vt.scroll_bottom < vt.height);
        for line in &vt.scrollback {
            assert_eq!(line.len(), vt.width as usize);
        }
    }

    #[test]
    fn new_terminal_dimensions() {
        let vt = VirtualTerminal::new(80, 24);
        assert_eq!(vt.width(), 80);
        assert_eq!(vt.height(), 24);
        assert_eq!(vt.cursor(), (0, 0));
        assert!(vt.cursor_visible());
    }

    #[test]
    #[should_panic(expected = "dimensions must be > 0")]
    fn zero_width_panics() {
        let _ = VirtualTerminal::new(0, 24);
    }

    #[test]
    #[should_panic(expected = "dimensions must be > 0")]
    fn zero_height_panics() {
        let _ = VirtualTerminal::new(80, 0);
    }

    #[test]
    fn invariants_hold_for_varied_inputs() {
        let inputs: [&[u8]; 6] = [
            b"",
            b"Hello",
            b"ABCDE\r\nFGHIJ",
            b"\x1b[2J",
            b"\x1b[1;1H\x1b[2;2H",
            b"\x1b[?1049hAlt\x1b[?1049l",
        ];

        for width in 1..=6 {
            for height in 1..=4 {
                for input in inputs {
                    let mut vt = VirtualTerminal::new(width, height);
                    for chunk in input.chunks(3) {
                        vt.feed(chunk);
                        assert_invariants(&vt);
                    }
                }
            }
        }
    }

    #[test]
    fn plain_text_output() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"Hello, World!");
        assert_eq!(vt.char_at(0, 0), Some('H'));
        assert_eq!(vt.char_at(12, 0), Some('!'));
        assert_eq!(vt.cursor(), (13, 0));
        assert_eq!(vt.row_text(0), "Hello, World!");
    }

    #[test]
    fn newline_advances_cursor() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"Line 1\r\nLine 2");
        assert_eq!(vt.row_text(0), "Line 1");
        assert_eq!(vt.row_text(1), "Line 2");
        assert_eq!(vt.cursor(), (6, 1));
    }

    #[test]
    fn carriage_return() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"AAAA\rBB");
        assert_eq!(vt.row_text(0), "BBAA");
    }

    #[test]
    fn auto_wrap() {
        let mut vt = VirtualTerminal::new(5, 3);
        vt.feed(b"ABCDEFGH");
        assert_eq!(vt.row_text(0), "ABCDE");
        assert_eq!(vt.row_text(1), "FGH");
        assert_eq!(vt.cursor(), (3, 1));
    }

    #[test]
    fn screen_immediate_wrap_quirk_wraps_on_last_column() {
        let mut vt = VirtualTerminal::with_quirks(5, 3, QuirkSet::gnu_screen());
        vt.feed(b"ABCDE");
        assert_eq!(vt.row_text(0), "ABCDE");
        assert_eq!(vt.cursor(), (0, 1));

        vt.feed(b"F");
        assert_eq!(vt.row_text(1), "F");
        assert_eq!(vt.cursor(), (1, 1));
    }

    #[test]
    fn scroll_on_overflow() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"AAA\r\nBBB\r\nCCC\r\nDDD");
        // AAA scrolled into scrollback, screen shows BBB, CCC, DDD
        assert_eq!(vt.row_text(0), "BBB");
        assert_eq!(vt.row_text(1), "CCC");
        assert_eq!(vt.row_text(2), "DDD");
        assert_eq!(vt.scrollback_len(), 1);
        assert_eq!(vt.scrollback_line(0), Some("AAA".to_string()));
    }

    #[test]
    fn cursor_movement_csi() {
        let mut vt = VirtualTerminal::new(80, 24);
        // Move to (5, 3) — 1-indexed
        vt.feed(b"\x1b[4;6H");
        assert_eq!(vt.cursor(), (5, 3));
    }

    #[test]
    fn cursor_up_down_forward_back() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[10;10H"); // Move to (9, 9)
        vt.feed(b"\x1b[3A"); // Up 3
        assert_eq!(vt.cursor(), (9, 6));
        vt.feed(b"\x1b[2B"); // Down 2
        assert_eq!(vt.cursor(), (9, 8));
        vt.feed(b"\x1b[5C"); // Forward 5
        assert_eq!(vt.cursor(), (14, 8));
        vt.feed(b"\x1b[3D"); // Back 3
        assert_eq!(vt.cursor(), (11, 8));
    }

    #[test]
    fn cursor_clamps_to_bounds() {
        let mut vt = VirtualTerminal::new(10, 5);
        vt.feed(b"\x1b[100;100H");
        assert_eq!(vt.cursor(), (9, 4));
        vt.feed(b"\x1b[99A");
        assert_eq!(vt.cursor(), (9, 0));
    }

    #[test]
    fn erase_to_end_of_line() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"Hello World");
        vt.feed(b"\x1b[1;6H"); // Move to column 6 (0-indexed: 5)
        vt.feed(b"\x1b[K"); // Erase to end of line
        assert_eq!(vt.row_text(0), "Hello");
    }

    #[test]
    fn erase_entire_line() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"Hello World");
        vt.feed(b"\x1b[2K");
        assert_eq!(vt.row_text(0), "");
    }

    #[test]
    fn erase_display_from_cursor() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"AAAAAAAAAA");
        vt.feed(b"BBBBBBBBBB");
        vt.feed(b"CCCCCCCCCC");
        vt.feed(b"\x1b[2;5H"); // Row 2, Col 5 (1-indexed)
        vt.feed(b"\x1b[J"); // Erase from cursor to end
        assert_eq!(vt.row_text(0), "AAAAAAAAAA");
        assert_eq!(vt.row_text(1), "BBBB");
        assert_eq!(vt.row_text(2), "");
    }

    #[test]
    fn sgr_bold_and_color() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[1;31mHello\x1b[0m World");
        // "Hello" should be bold + red
        let style = vt.style_at(0, 0).unwrap();
        assert!(style.bold);
        assert_eq!(style.fg, Some(Color::new(170, 0, 0)));
        // " World" should be reset
        let style2 = vt.style_at(6, 0).unwrap();
        assert!(!style2.bold);
        assert_eq!(style2.fg, None);
    }

    #[test]
    fn sgr_truecolor() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[38;2;100;200;50mX");
        let style = vt.style_at(0, 0).unwrap();
        assert_eq!(style.fg, Some(Color::new(100, 200, 50)));
    }

    #[test]
    fn sgr_256_color() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[48;5;196mX"); // Bright red-ish in 256 palette
        let style = vt.style_at(0, 0).unwrap();
        assert!(style.bg.is_some());
    }

    #[test]
    fn dec_save_restore_cursor() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[5;10H"); // Move to (9, 4)
        vt.feed(b"\x1b7"); // Save
        vt.feed(b"\x1b[1;1H"); // Move to (0, 0)
        assert_eq!(vt.cursor(), (0, 0));
        vt.feed(b"\x1b8"); // Restore
        assert_eq!(vt.cursor(), (9, 4));
    }

    #[test]
    fn tmux_nested_cursor_quirk_ignores_save_restore_in_alt_screen() {
        let mut vt = VirtualTerminal::with_quirks(80, 24, QuirkSet::tmux_nested());
        vt.feed(b"\x1b[?1049h"); // Enter alt screen
        vt.feed(b"\x1b[5;10H"); // Move to (9, 4)
        vt.feed(b"\x1b7"); // Save (ignored)
        vt.feed(b"\x1b[1;1H"); // Move to (0, 0)
        vt.feed(b"\x1b8"); // Restore (ignored)
        assert_eq!(vt.cursor(), (0, 0));
    }

    #[test]
    fn combined_quirks_apply_independently() {
        let quirks = QuirkSet::empty()
            .with_screen_immediate_wrap(true)
            .with_tmux_nested_cursor(true);
        let mut vt = VirtualTerminal::with_quirks(5, 3, quirks);

        vt.feed(b"\x1b[?1049h");
        vt.feed(b"ABCDE");
        assert_eq!(vt.cursor(), (0, 1));

        vt.feed(b"\x1b[2;2H\x1b7\x1b[1;1H\x1b8");
        assert_eq!(vt.cursor(), (0, 0));
    }

    #[test]
    fn cursor_visibility() {
        let mut vt = VirtualTerminal::new(80, 24);
        assert!(vt.cursor_visible());
        vt.feed(b"\x1b[?25l"); // Hide cursor
        assert!(!vt.cursor_visible());
        vt.feed(b"\x1b[?25h"); // Show cursor
        assert!(vt.cursor_visible());
    }

    #[test]
    fn alternate_screen() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"Main");
        assert_eq!(vt.row_text(0), "Main");
        assert!(!vt.is_alternate_screen());

        vt.feed(b"\x1b[?1049h"); // Enter alt screen
        assert!(vt.is_alternate_screen());
        assert_eq!(vt.row_text(0), ""); // Alt screen is blank
        vt.feed(b"Alt");
        assert_eq!(vt.row_text(0), "Alt");

        vt.feed(b"\x1b[?1049l"); // Exit alt screen
        assert!(!vt.is_alternate_screen());
        assert_eq!(vt.row_text(0), "Main"); // Main screen restored
    }

    #[test]
    fn windows_no_alt_screen_quirk_ignores_alternate_buffer() {
        let mut vt = VirtualTerminal::with_quirks(10, 3, QuirkSet::windows_console());
        vt.feed(b"Main");
        vt.feed(b"\x1b[?1049h"); // Ignored
        vt.feed(b"Alt");
        vt.feed(b"\x1b[?1049l"); // Ignored
        assert!(!vt.is_alternate_screen());
        assert_eq!(vt.row_text(0), "MainAlt");
    }

    #[test]
    fn osc_title() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b]0;My Title\x07");
        assert_eq!(vt.title(), "My Title");
    }

    #[test]
    fn full_reset() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"Some text\x1b[1;31m");
        vt.feed(b"\x1bc"); // Full reset (RIS)
        assert_eq!(vt.cursor(), (0, 0));
        assert_eq!(vt.row_text(0), "");
        assert!(vt.cursor_visible());
    }

    #[test]
    fn cpr_response_format() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[5;10H");
        let response = vt.cpr_response();
        assert_eq!(response, b"\x1b[5;10R");
    }

    #[test]
    fn da1_response() {
        let vt = VirtualTerminal::new(80, 24);
        let response = vt.da1_response();
        assert_eq!(response, b"\x1b[?62;22c");
    }

    #[test]
    fn scroll_region() {
        let mut vt = VirtualTerminal::new(10, 5);
        // Set scroll region to rows 2-4 (1-indexed)
        vt.feed(b"\x1b[2;4r");
        // Fill screen
        vt.feed(b"\x1b[1;1HROW1");
        vt.feed(b"\x1b[2;1HROW2");
        vt.feed(b"\x1b[3;1HROW3");
        vt.feed(b"\x1b[4;1HROW4");
        vt.feed(b"\x1b[5;1HROW5");
        assert_eq!(vt.row_text(0), "ROW1");
        assert_eq!(vt.row_text(4), "ROW5");
    }

    #[test]
    fn tab_advances_to_stop() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"AB\tC");
        assert_eq!(vt.char_at(8, 0), Some('C'));
    }

    #[test]
    fn backspace() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"ABC\x08X");
        assert_eq!(vt.row_text(0), "ABX");
    }

    #[test]
    fn screen_text() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"AAA\r\nBBB\r\nCCC");
        let text = vt.screen_text();
        assert_eq!(text, "AAA\nBBB\nCCC");
    }

    #[test]
    fn scrollback_truncation() {
        let mut vt = VirtualTerminal::new(10, 2);
        vt.set_max_scrollback(3);
        // Push 5 lines, only last 3 remain in scrollback
        for i in 0..5 {
            vt.feed_str(&format!("Line{i}\n"));
        }
        assert!(vt.scrollback_len() <= 3);
    }

    #[test]
    fn out_of_bounds_cell_returns_none() {
        let vt = VirtualTerminal::new(10, 5);
        assert_eq!(vt.char_at(10, 0), None);
        assert_eq!(vt.char_at(0, 5), None);
        assert!(vt.style_at(99, 99).is_none());
    }

    #[test]
    fn reverse_index_at_scroll_top() {
        let mut vt = VirtualTerminal::new(10, 5);
        vt.feed(b"\x1b[2;4r"); // Scroll region 2-4
        vt.feed(b"\x1b[2;1H"); // Cursor at row 2
        vt.feed(b"\x1bM"); // Reverse index
        // Should scroll down within region
        assert_eq!(vt.cursor(), (0, 1));
    }

    #[test]
    fn cursor_horizontal_absolute() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[10G");
        assert_eq!(vt.cursor(), (9, 0));
    }

    #[test]
    fn vertical_position_absolute() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[5d");
        assert_eq!(vt.cursor(), (0, 4));
    }

    #[test]
    fn cursor_next_previous_line() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[5;10H"); // (9, 4)
        vt.feed(b"\x1b[2E"); // Next line x2
        assert_eq!(vt.cursor(), (0, 6));
        vt.feed(b"\x1b[1F"); // Previous line x1
        assert_eq!(vt.cursor(), (0, 5));
    }

    #[test]
    fn bright_colors() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[91mX"); // Bright red fg
        let style = vt.style_at(0, 0).unwrap();
        assert_eq!(style.fg, Some(Color::new(255, 85, 85)));
    }

    #[test]
    fn nel_next_line() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"ABCDE\x1bEX");
        // ESC E = CR + LF: cursor goes to col 0, next row
        assert_eq!(vt.row_text(0), "ABCDE");
        assert_eq!(vt.row_text(1), "X");
        assert_eq!(vt.cursor(), (1, 1));
    }

    #[test]
    fn nel_at_bottom_scrolls() {
        let mut vt = VirtualTerminal::new(5, 3);
        vt.feed(b"AAAAA\r\nBBBBB\r\nCCCCC");
        vt.feed(b"\x1b[3;3H\x1bE"); // CUP(3,3) → (2,2), then NEL at bottom
        assert_eq!(vt.row_text(0), "BBBBB");
        assert_eq!(vt.row_text(1), "CCCCC");
        assert_eq!(vt.row_text(2), "");
        assert_eq!(vt.cursor(), (0, 2));
    }

    #[test]
    fn decaln_fills_with_e() {
        let mut vt = VirtualTerminal::new(5, 3);
        vt.feed(b"ABC\x1b#8");
        assert_eq!(vt.row_text(0), "EEEEE");
        assert_eq!(vt.row_text(1), "EEEEE");
        assert_eq!(vt.row_text(2), "EEEEE");
        assert_eq!(vt.cursor(), (0, 0));
    }

    #[test]
    fn decaln_resets_scroll_region() {
        let mut vt = VirtualTerminal::new(5, 3);
        vt.feed(b"\x1b[2;3r"); // Set scroll region rows 2-3
        vt.feed(b"\x1b#8"); // DECALN resets margins
        // After DECALN, writing + scroll should affect full screen (region reset)
        vt.feed(b"\x1b[3;1HZZZZZ\n"); // Write at bottom, LF → scroll
        assert_eq!(vt.row_text(0), "EEEEE");
        assert_eq!(vt.row_text(1), "ZZZZZ");
        assert_eq!(vt.row_text(2), "");
    }

    #[test]
    fn utf8_basic_multibyte() {
        let mut vt = VirtualTerminal::new(10, 3);
        // "é" is 2 bytes: 0xc3 0xa9
        vt.feed("Aé B".as_bytes());
        assert_eq!(vt.row_text(0), "Aé B");
        assert_eq!(vt.cursor(), (4, 0));
    }

    #[test]
    fn wide_char_basic() {
        let mut vt = VirtualTerminal::new(10, 3);
        // "中" (U+4E2D) = 3 bytes, display width 2
        vt.feed("A中B".as_bytes());
        assert_eq!(vt.row_text(0), "A中B");
        assert_eq!(vt.cursor(), (4, 0)); // A(1) + 中(2) + B(1) = col 4
    }

    #[test]
    fn wide_char_wraps_at_last_column() {
        let mut vt = VirtualTerminal::new(5, 3);
        // 4 narrow chars + wide char: wide can't fit in col 4, wraps
        vt.feed("ABCD中".as_bytes());
        assert_eq!(vt.row_text(0), "ABCD");
        assert_eq!(vt.row_text(1), "中");
        assert_eq!(vt.cursor(), (2, 1));
    }

    #[test]
    fn narrow_overwrites_wide_lead() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed("中".as_bytes()); // col 0-1
        vt.feed(b"\x1b[1;1HX"); // CUP(1,1), write 'X' at col 0 (overwrites wide lead)
        // Lead becomes 'X', continuation should be blanked
        assert_eq!(vt.row_text(0), "X");
        assert_eq!(vt.cursor(), (1, 0));
    }

    #[test]
    fn narrow_overwrites_wide_continuation() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed("中".as_bytes()); // col 0-1
        vt.feed(b"\x1b[1;2HX"); // CUP(1,2), write 'X' at col 1 (overwrites continuation)
        // Lead blanked to space, col 1 becomes 'X'
        assert_eq!(vt.row_text(0), " X");
        assert_eq!(vt.cursor(), (2, 0));
    }

    // ── Tab stop tests ─────────────────────────────────────────────

    #[test]
    fn default_tab_stops_every_8() {
        let vt = VirtualTerminal::new(20, 3);
        // Default stops at 8, 16
        assert!(!vt.tab_stops[0]);
        assert!(vt.tab_stops[8]);
        assert!(vt.tab_stops[16]);
        assert!(!vt.tab_stops[1]);
        assert!(!vt.tab_stops[7]);
    }

    #[test]
    fn hts_sets_custom_tab_stop() {
        let mut vt = VirtualTerminal::new(20, 3);
        // Move to col 5, ESC H to set tab stop
        vt.feed(b"\x1b[1;6H\x1bH");
        assert!(vt.tab_stops[5]);
        // Tab from col 0 should now stop at 5
        vt.feed(b"\x1b[1;1H\t");
        assert_eq!(vt.cursor(), (5, 0));
    }

    #[test]
    fn tbc_clears_single_tab_stop() {
        let mut vt = VirtualTerminal::new(20, 3);
        // Clear tab stop at col 8
        vt.feed(b"\x1b[1;9H\x1b[0g");
        assert!(!vt.tab_stops[8]);
        // Tab from col 0 should now go to col 16
        vt.feed(b"\x1b[1;1H\t");
        assert_eq!(vt.cursor(), (16, 0));
    }

    #[test]
    fn tbc_clears_all_tab_stops() {
        let mut vt = VirtualTerminal::new(20, 3);
        // Clear all tab stops
        vt.feed(b"\x1b[3g");
        // Tab from col 0 → clamps to last col (no stops)
        vt.feed(b"\x1b[1;1H\t");
        assert_eq!(vt.cursor(), (19, 0));
    }

    #[test]
    fn cbt_moves_to_previous_tab_stop() {
        let mut vt = VirtualTerminal::new(20, 3);
        // Move to col 10, CBT → back to col 8
        vt.feed(b"\x1b[1;11H\x1b[Z");
        assert_eq!(vt.cursor(), (8, 0));
    }

    #[test]
    fn cbt_at_col_zero() {
        let mut vt = VirtualTerminal::new(20, 3);
        // CBT at col 0 stays at col 0
        vt.feed(b"\x1b[Z");
        assert_eq!(vt.cursor(), (0, 0));
    }

    #[test]
    fn reset_restores_default_tab_stops() {
        let mut vt = VirtualTerminal::new(20, 3);
        // Clear all, then reset
        vt.feed(b"\x1b[3g");
        assert!(!vt.tab_stops[8]);
        vt.feed(b"\x1bc"); // RIS (full reset)
        assert!(vt.tab_stops[8]);
    }

    // ── IRM (Insert/Replace Mode) tests ────────────────────────────

    #[test]
    fn irm_insert_mode_shifts_right() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"ABCDE");
        // Enable insert mode (CSI 4 h), move to col 2, type "XY"
        vt.feed(b"\x1b[4h\x1b[1;3HXY");
        // "AB" + inserted "XY" + shifted "CDE" → "ABXYCDE"
        assert_eq!(vt.row_text(0), "ABXYCDE");
    }

    #[test]
    fn irm_replace_mode_default() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"ABCDE");
        // Replace mode (default), move to col 2, type "XY"
        vt.feed(b"\x1b[1;3HXY");
        assert_eq!(vt.row_text(0), "ABXYE");
    }

    #[test]
    fn irm_disable_returns_to_replace() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"ABCDE");
        // Enable insert, then disable
        vt.feed(b"\x1b[4h\x1b[4l\x1b[1;3HXY");
        // Should overwrite, not insert
        assert_eq!(vt.row_text(0), "ABXYE");
    }

    #[test]
    fn irm_insert_pushes_off_right_edge() {
        let mut vt = VirtualTerminal::new(5, 3);
        vt.feed(b"ABCDE");
        // Insert mode, move to col 0, type "X"
        vt.feed(b"\x1b[4h\x1b[1;1HX");
        // "X" inserted at col 0, "ABCD" shifted right, "E" falls off
        assert_eq!(vt.row_text(0), "XABCD");
    }

    // ── DECAWM (Auto-Wrap Mode) tests ──────────────────────────────

    #[test]
    fn decawm_enabled_wraps_at_edge() {
        let mut vt = VirtualTerminal::new(5, 3);
        // Auto-wrap is on by default
        vt.feed(b"ABCDEF");
        assert_eq!(vt.row_text(0), "ABCDE");
        assert_eq!(vt.row_text(1), "F");
    }

    #[test]
    fn decawm_disabled_no_wrap() {
        let mut vt = VirtualTerminal::new(5, 3);
        // Disable auto-wrap
        vt.feed(b"\x1b[?7l");
        vt.feed(b"ABCDEFGH");
        // All chars overwrite at last column, no wrap
        assert_eq!(vt.row_text(0), "ABCDH");
        assert_eq!(vt.row_text(1), "");
        assert_eq!(vt.cursor(), (4, 0));
    }

    #[test]
    fn decawm_reenable_wraps_again() {
        let mut vt = VirtualTerminal::new(5, 3);
        // Disable, then re-enable
        vt.feed(b"\x1b[?7l\x1b[?7h");
        vt.feed(b"ABCDEF");
        assert_eq!(vt.row_text(0), "ABCDE");
        assert_eq!(vt.row_text(1), "F");
    }

    // ── Charset tests ───────────────────────────────────────────────

    #[test]
    fn dec_graphics_g0_designation() {
        let mut vt = VirtualTerminal::new(10, 3);
        // ESC ( 0 — designate G0 as DEC Special Graphics
        // 'q' = ─ (U+2500), 'x' = │ (U+2502)
        vt.feed(b"\x1b(0qqxx\x1b(B");
        assert_eq!(vt.char_at(0, 0).unwrap(), '\u{2500}'); // ─
        assert_eq!(vt.char_at(1, 0).unwrap(), '\u{2500}'); // ─
        assert_eq!(vt.char_at(2, 0).unwrap(), '\u{2502}'); // │
        assert_eq!(vt.char_at(3, 0).unwrap(), '\u{2502}'); // │
    }

    #[test]
    fn dec_graphics_g1_with_so_si() {
        let mut vt = VirtualTerminal::new(10, 3);
        // ESC ) 0 — designate G1 as DEC Special Graphics
        // SO (0x0e) — activate G1, print 'l' = ┌ (U+250C)
        // SI (0x0f) — back to G0 (ASCII), print 'l' = literal 'l'
        vt.feed(b"\x1b)0\x0el\x0fl");
        assert_eq!(vt.char_at(0, 0).unwrap(), '\u{250C}'); // ┌
        assert_eq!(vt.char_at(1, 0).unwrap(), 'l');
    }

    #[test]
    fn dec_graphics_box_chars() {
        let mut vt = VirtualTerminal::new(10, 3);
        // Test all four corners + crossing
        vt.feed(b"\x1b(0lkjmn\x1b(B");
        assert_eq!(vt.char_at(0, 0).unwrap(), '\u{250C}'); // l = ┌
        assert_eq!(vt.char_at(1, 0).unwrap(), '\u{2510}'); // k = ┐
        assert_eq!(vt.char_at(2, 0).unwrap(), '\u{2518}'); // j = ┘
        assert_eq!(vt.char_at(3, 0).unwrap(), '\u{2514}'); // m = └
        assert_eq!(vt.char_at(4, 0).unwrap(), '\u{253C}'); // n = ┼
    }

    #[test]
    fn charset_reset_restores_ascii() {
        let mut vt = VirtualTerminal::new(10, 3);
        // Designate G0 as DEC graphics, then full reset
        vt.feed(b"\x1b(0");
        vt.feed(b"\x1bc"); // RIS (full reset)
        vt.feed(b"q");
        assert_eq!(vt.char_at(0, 0).unwrap(), 'q'); // Should be literal 'q', not ─
    }

    #[test]
    fn charset_soft_reset_restores_ascii() {
        let mut vt = VirtualTerminal::new(10, 3);
        // Designate G0 as DEC graphics, then soft reset
        vt.feed(b"\x1b(0");
        vt.feed(b"\x1b[!p"); // DECSTR (soft reset)
        vt.feed(b"q");
        assert_eq!(vt.char_at(0, 0).unwrap(), 'q'); // Should be literal 'q', not ─
    }

    #[test]
    fn so_si_toggle_charset() {
        let mut vt = VirtualTerminal::new(10, 3);
        // G1 = DEC graphics; toggle SO/SI multiple times
        vt.feed(b"\x1b)0");
        vt.feed(b"A"); // G0 ASCII: 'A'
        vt.feed(b"\x0e"); // SO: switch to G1
        vt.feed(b"q"); // DEC graphics: ─
        vt.feed(b"\x0f"); // SI: back to G0
        vt.feed(b"B"); // ASCII: 'B'
        assert_eq!(vt.char_at(0, 0).unwrap(), 'A');
        assert_eq!(vt.char_at(1, 0).unwrap(), '\u{2500}'); // ─
        assert_eq!(vt.char_at(2, 0).unwrap(), 'B');
    }

    #[test]
    fn ascii_passthrough_in_dec_graphics() {
        let mut vt = VirtualTerminal::new(10, 3);
        // Characters outside 0x60-0x7e should pass through unchanged even in DEC graphics
        vt.feed(b"\x1b(0ABC\x1b(B");
        assert_eq!(vt.char_at(0, 0).unwrap(), 'A');
        assert_eq!(vt.char_at(1, 0).unwrap(), 'B');
        assert_eq!(vt.char_at(2, 0).unwrap(), 'C');
    }

    // ── ICH (Insert Characters, CSI @) tests ──────────────────────────

    #[test]
    fn ich_basic_insert() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"ABCDE");
        vt.feed(b"\x1b[1;3H"); // cursor at col 2
        vt.feed(b"\x1b[2@"); // insert 2 blanks
        assert_eq!(vt.row_text(0), "AB  CDE");
        assert_eq!(vt.cursor(), (2, 0));
    }

    #[test]
    fn ich_pushes_off_right_edge() {
        let mut vt = VirtualTerminal::new(5, 3);
        vt.feed(b"ABCDE");
        vt.feed(b"\x1b[1;2H"); // cursor at col 1
        vt.feed(b"\x1b[2@"); // insert 2
        assert_eq!(vt.row_text(0), "A  BC");
    }

    #[test]
    fn ich_at_wide_char_continuation() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed("A中B".as_bytes()); // A at 0, 中 at 1-2, B at 3
        vt.feed(b"\x1b[1;3H"); // cursor at col 2 (continuation of 中)
        vt.feed(b"\x1b[1@"); // insert 1 blank at continuation
        // Wide char lead at col 1 should be blanked (orphaned)
        assert_invariants(&vt);
    }

    // ── DCH (Delete Characters, CSI P) tests ──────────────────────────

    #[test]
    fn dch_basic_delete() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"ABCDE");
        vt.feed(b"\x1b[1;2H"); // cursor at col 1
        vt.feed(b"\x1b[2P"); // delete 2 chars
        assert_eq!(vt.row_text(0), "ADE");
        assert_eq!(vt.cursor(), (1, 0));
    }

    #[test]
    fn dch_fills_blanks_at_end() {
        let mut vt = VirtualTerminal::new(5, 3);
        vt.feed(b"ABCDE");
        vt.feed(b"\x1b[1;1H"); // cursor at col 0
        vt.feed(b"\x1b[3P"); // delete 3
        assert_eq!(vt.row_text(0), "DE");
    }

    #[test]
    fn dch_at_wide_char_boundary() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed("A中B".as_bytes()); // A at 0, 中 at 1-2, B at 3
        vt.feed(b"\x1b[1;2H"); // cursor at col 1 (lead of 中)
        vt.feed(b"\x1b[1P"); // delete 1 char at wide lead
        assert_invariants(&vt);
    }

    // ── ECH (Erase Characters, CSI X) tests ───────────────────────────

    #[test]
    fn ech_basic_erase() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"ABCDE");
        vt.feed(b"\x1b[1;2H"); // cursor at col 1
        vt.feed(b"\x1b[3X"); // erase 3 chars
        assert_eq!(vt.row_text(0), "A   E");
        assert_eq!(vt.cursor(), (1, 0)); // cursor doesn't move
    }

    #[test]
    fn ech_does_not_move_cursor() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"ABCDE");
        vt.feed(b"\x1b[1;3H"); // cursor at col 2
        vt.feed(b"\x1b[1X");
        assert_eq!(vt.cursor(), (2, 0));
        assert_eq!(vt.char_at(2, 0), Some(' '));
        assert_eq!(vt.char_at(3, 0), Some('D'));
    }

    #[test]
    fn ech_at_wide_char_continuation() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed("X中Y".as_bytes()); // X at 0, 中 at 1-2, Y at 3
        vt.feed(b"\x1b[1;3H"); // cursor at col 2 (continuation of 中)
        vt.feed(b"\x1b[1X"); // erase 1 at continuation
        // Lead at col 1 should be blanked (orphaned)
        assert_invariants(&vt);
        assert_eq!(vt.char_at(1, 0), Some(' ')); // orphaned lead blanked
    }

    #[test]
    fn ech_clamped_to_line_end() {
        let mut vt = VirtualTerminal::new(5, 3);
        vt.feed(b"ABCDE");
        vt.feed(b"\x1b[1;4H"); // cursor at col 3
        vt.feed(b"\x1b[99X"); // erase 99 (clamped to remaining 2)
        assert_eq!(vt.row_text(0), "ABC");
    }

    // ── IL (Insert Lines, CSI L) tests ────────────────────────────────

    #[test]
    fn il_basic_insert_line() {
        let mut vt = VirtualTerminal::new(5, 5);
        vt.feed(b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE");
        vt.feed(b"\x1b[2;1H"); // cursor at row 1
        vt.feed(b"\x1b[1L"); // insert 1 line
        assert_eq!(vt.row_text(0), "AAAAA");
        assert_eq!(vt.row_text(1), ""); // inserted blank
        assert_eq!(vt.row_text(2), "BBBBB");
        assert_eq!(vt.row_text(3), "CCCCC");
        assert_eq!(vt.row_text(4), "DDDDD");
        // EEEEE pushed off bottom
    }

    #[test]
    fn il_within_scroll_region() {
        let mut vt = VirtualTerminal::new(5, 5);
        vt.feed(b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE");
        vt.feed(b"\x1b[2;4r"); // scroll region rows 2-4 (0-indexed: 1-3)
        vt.feed(b"\x1b[2;1H"); // cursor at row 1 (within region)
        vt.feed(b"\x1b[1L"); // insert 1 line
        assert_eq!(vt.row_text(0), "AAAAA"); // outside region, untouched
        assert_eq!(vt.row_text(1), ""); // inserted blank
        assert_eq!(vt.row_text(2), "BBBBB"); // shifted down
        assert_eq!(vt.row_text(3), "CCCCC"); // shifted down (DDDDD pushed off bottom of region)
        assert_eq!(vt.row_text(4), "EEEEE"); // outside region, untouched
    }

    #[test]
    fn il_outside_scroll_region_ignored() {
        let mut vt = VirtualTerminal::new(5, 5);
        vt.feed(b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE");
        vt.feed(b"\x1b[2;4r"); // scroll region rows 2-4
        vt.feed(b"\x1b[1;1H"); // cursor at row 0 (outside region)
        vt.feed(b"\x1b[1L"); // insert: cursor outside region → no-op
        assert_eq!(vt.row_text(0), "AAAAA");
        assert_eq!(vt.row_text(1), "BBBBB");
    }

    // ── DL (Delete Lines, CSI M) tests ────────────────────────────────

    #[test]
    fn dl_basic_delete_line() {
        let mut vt = VirtualTerminal::new(5, 5);
        vt.feed(b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE");
        vt.feed(b"\x1b[2;1H"); // cursor at row 1
        vt.feed(b"\x1b[1M"); // delete 1 line
        assert_eq!(vt.row_text(0), "AAAAA");
        assert_eq!(vt.row_text(1), "CCCCC"); // pulled up
        assert_eq!(vt.row_text(2), "DDDDD");
        assert_eq!(vt.row_text(3), "EEEEE");
        assert_eq!(vt.row_text(4), ""); // blank at bottom
    }

    #[test]
    fn dl_within_scroll_region() {
        let mut vt = VirtualTerminal::new(5, 5);
        vt.feed(b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE");
        vt.feed(b"\x1b[2;4r"); // scroll region rows 2-4 (0-indexed: 1-3)
        vt.feed(b"\x1b[2;1H"); // cursor at row 1 (within region)
        vt.feed(b"\x1b[1M"); // delete 1 line
        assert_eq!(vt.row_text(0), "AAAAA"); // outside region
        assert_eq!(vt.row_text(1), "CCCCC"); // pulled up
        assert_eq!(vt.row_text(2), "DDDDD"); // pulled up
        assert_eq!(vt.row_text(3), ""); // blank at region bottom
        assert_eq!(vt.row_text(4), "EEEEE"); // outside region
    }

    #[test]
    fn dl_outside_scroll_region_ignored() {
        let mut vt = VirtualTerminal::new(5, 5);
        vt.feed(b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE");
        vt.feed(b"\x1b[2;4r"); // scroll region rows 2-4
        vt.feed(b"\x1b[5;1H"); // cursor at row 4 (outside region bottom)
        vt.feed(b"\x1b[1M"); // delete: cursor outside → no-op
        assert_eq!(vt.row_text(4), "EEEEE");
    }

    // ── SU/SD (Scroll Up/Down, CSI S/T) tests ────────────────────────

    #[test]
    fn su_scroll_up_within_region() {
        let mut vt = VirtualTerminal::new(5, 5);
        vt.feed(b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE");
        vt.feed(b"\x1b[2;4r"); // scroll region rows 2-4 (0-indexed: 1-3)
        vt.feed(b"\x1b[1S"); // scroll up 1 within region
        assert_eq!(vt.row_text(0), "AAAAA"); // outside region, untouched
        assert_eq!(vt.row_text(1), "CCCCC"); // shifted up from row 2
        assert_eq!(vt.row_text(2), "DDDDD"); // shifted up from row 3
        assert_eq!(vt.row_text(3), ""); // blank at region bottom
        assert_eq!(vt.row_text(4), "EEEEE"); // outside region, untouched
    }

    #[test]
    fn sd_scroll_down_within_region() {
        let mut vt = VirtualTerminal::new(5, 5);
        vt.feed(b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE");
        vt.feed(b"\x1b[2;4r"); // scroll region rows 2-4 (0-indexed: 1-3)
        vt.feed(b"\x1b[1T"); // scroll down 1 within region
        assert_eq!(vt.row_text(0), "AAAAA"); // outside region, untouched
        assert_eq!(vt.row_text(1), ""); // blank at region top
        assert_eq!(vt.row_text(2), "BBBBB"); // shifted down from row 1
        assert_eq!(vt.row_text(3), "CCCCC"); // shifted down from row 2 (DDDDD pushed off)
        assert_eq!(vt.row_text(4), "EEEEE"); // outside region, untouched
    }

    #[test]
    fn su_multiple_lines() {
        let mut vt = VirtualTerminal::new(5, 3);
        vt.feed(b"AAAAA\r\nBBBBB\r\nCCCCC");
        vt.feed(b"\x1b[2S"); // scroll up 2
        assert_eq!(vt.row_text(0), "CCCCC");
        assert_eq!(vt.row_text(1), "");
        assert_eq!(vt.row_text(2), "");
        assert_eq!(vt.scrollback_len(), 2);
    }

    // ── REP (Repeat Character, CSI b) tests ───────────────────────────

    #[test]
    fn rep_basic_repeat() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"X\x1b[3b"); // print X, then repeat 3 times
        assert_eq!(vt.row_text(0), "XXXX");
        assert_eq!(vt.cursor(), (4, 0));
    }

    #[test]
    fn rep_no_previous_char() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"\x1b[5b"); // repeat with no previous char → no-op
        assert_eq!(vt.row_text(0), "");
        assert_eq!(vt.cursor(), (0, 0));
    }

    #[test]
    fn rep_wraps_across_lines() {
        let mut vt = VirtualTerminal::new(5, 3);
        vt.feed(b"A\x1b[6b"); // A + repeat 6 → 7 A's total → wraps
        assert_eq!(vt.row_text(0), "AAAAA");
        assert_eq!(vt.row_text(1), "AA");
    }

    // ── DECOM (Origin Mode, DEC mode 6) tests ────────────────────────

    #[test]
    fn decom_cup_relative_to_scroll_region() {
        let mut vt = VirtualTerminal::new(10, 10);
        vt.feed(b"\x1b[3;7r"); // scroll region rows 3-7 (0-indexed: 2-6)
        vt.feed(b"\x1b[?6h"); // enable DECOM
        // CUP(1,1) should go to (0, scroll_top=2) in absolute coords
        vt.feed(b"\x1b[1;1H");
        assert_eq!(vt.cursor(), (0, 2));
        // CUP(3,5) should go to (4, scroll_top+2=4)
        vt.feed(b"\x1b[3;5H");
        assert_eq!(vt.cursor(), (4, 4));
    }

    #[test]
    fn decom_clamps_to_scroll_region() {
        let mut vt = VirtualTerminal::new(10, 10);
        vt.feed(b"\x1b[3;7r"); // scroll region rows 3-7
        vt.feed(b"\x1b[?6h"); // enable DECOM
        // CUP with row beyond region should clamp to scroll_bottom
        vt.feed(b"\x1b[99;1H");
        assert_eq!(vt.cursor(), (0, 6)); // clamped to scroll_bottom=6
    }

    #[test]
    fn decom_disable_homes_to_origin() {
        let mut vt = VirtualTerminal::new(10, 10);
        vt.feed(b"\x1b[3;7r");
        vt.feed(b"\x1b[?6h"); // enable → cursor to scroll_top
        assert_eq!(vt.cursor(), (0, 2));
        vt.feed(b"\x1b[5;5H"); // move somewhere
        vt.feed(b"\x1b[?6l"); // disable → cursor to (0,0)
        assert_eq!(vt.cursor(), (0, 0));
    }

    #[test]
    fn decom_vpa_relative_to_scroll_region() {
        let mut vt = VirtualTerminal::new(10, 10);
        vt.feed(b"\x1b[3;7r"); // scroll region rows 3-7
        vt.feed(b"\x1b[?6h"); // enable DECOM
        vt.feed(b"\x1b[2d"); // VPA row 2 → absolute row = scroll_top + 1 = 3
        assert_eq!(vt.cursor().1, 3);
    }

    #[test]
    fn decom_decstbm_homes_cursor() {
        let mut vt = VirtualTerminal::new(10, 10);
        vt.feed(b"\x1b[?6h"); // enable DECOM
        vt.feed(b"\x1b[5;5H"); // move somewhere
        vt.feed(b"\x1b[3;7r"); // set new scroll region → homes cursor
        assert_eq!(vt.cursor(), (0, 2)); // cursor at scroll_top
    }

    // ── Single-Shift SS2/SS3 tests ───────────────────────────────────

    #[test]
    fn ss2_translates_one_char_from_g2() {
        let mut vt = VirtualTerminal::new(10, 3);
        // Designate G2 as DEC Special Graphics
        vt.feed(b"\x1b*0");
        // SS2 (ESC N) → next char from G2, then revert to G0
        vt.feed(b"\x1bNq"); // 'q' via G2 (DEC graphics) → ─
        vt.feed(b"q"); // 'q' via G0 (ASCII) → 'q'
        assert_eq!(vt.char_at(0, 0).unwrap(), '\u{2500}'); // ─
        assert_eq!(vt.char_at(1, 0).unwrap(), 'q');
    }

    #[test]
    fn ss3_translates_one_char_from_g3() {
        let mut vt = VirtualTerminal::new(10, 3);
        // Designate G3 as DEC Special Graphics
        vt.feed(b"\x1b+0");
        // SS3 (ESC O) → next char from G3, then revert to G0
        vt.feed(b"\x1bOx"); // 'x' via G3 (DEC graphics) → │
        vt.feed(b"x"); // 'x' via G0 (ASCII) → 'x'
        assert_eq!(vt.char_at(0, 0).unwrap(), '\u{2502}'); // │
        assert_eq!(vt.char_at(1, 0).unwrap(), 'x');
    }

    #[test]
    fn ss2_only_affects_one_character() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"\x1b*0"); // G2 = DEC graphics
        vt.feed(b"\x1bNlk"); // SS2 + 'l' + 'k' (only 'l' via G2)
        assert_eq!(vt.char_at(0, 0).unwrap(), '\u{250C}'); // ┌ (l via DEC graphics)
        assert_eq!(vt.char_at(1, 0).unwrap(), 'k'); // literal k (back to G0)
    }

    // ── Locking Shift LS2/LS3 tests ─────────────────────────────────

    #[test]
    fn ls2_invokes_g2_into_gl() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"\x1b*0"); // G2 = DEC graphics
        vt.feed(b"\x1bn"); // LS2: invoke G2 into GL
        vt.feed(b"jm"); // should use DEC graphics
        assert_eq!(vt.char_at(0, 0).unwrap(), '\u{2518}'); // j = ┘
        assert_eq!(vt.char_at(1, 0).unwrap(), '\u{2514}'); // m = └
    }

    #[test]
    fn ls3_invokes_g3_into_gl() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"\x1b+0"); // G3 = DEC graphics
        vt.feed(b"\x1bo"); // LS3: invoke G3 into GL
        vt.feed(b"n"); // should use DEC graphics
        assert_eq!(vt.char_at(0, 0).unwrap(), '\u{253C}'); // n = ┼
    }

    #[test]
    fn ls2_persists_across_characters() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"\x1b*0"); // G2 = DEC graphics
        vt.feed(b"\x1bn"); // LS2
        vt.feed(b"tuvw"); // all via G2 DEC graphics
        assert_eq!(vt.char_at(0, 0).unwrap(), '\u{251C}'); // t = ├
        assert_eq!(vt.char_at(1, 0).unwrap(), '\u{2524}'); // u = ┤
        assert_eq!(vt.char_at(2, 0).unwrap(), '\u{2534}'); // v = ┴
        assert_eq!(vt.char_at(3, 0).unwrap(), '\u{252C}'); // w = ┬
    }

    // ── Alternate Screen Mode 1047 (without cursor save/restore) ─────

    #[test]
    fn alt_screen_1047_no_cursor_save() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"Main");
        vt.feed(b"\x1b[1;5H"); // cursor at col 4
        let (_cx, _cy) = vt.cursor();
        vt.feed(b"\x1b[?1047h"); // enter alt screen (no cursor save)
        assert!(vt.is_alternate_screen());
        assert_eq!(vt.row_text(0), ""); // blank alt screen

        vt.feed(b"Alt");
        vt.feed(b"\x1b[?1047l"); // exit alt screen (no cursor restore)
        assert!(!vt.is_alternate_screen());
        assert_eq!(vt.row_text(0), "Main"); // main grid restored
        // Cursor position is NOT restored (unlike mode 1049)
        // It keeps whatever position it had in alt screen
        let (_, _) = vt.cursor();
    }

    #[test]
    fn alt_screen_1047_double_enter_ignored() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"Main");
        vt.feed(b"\x1b[?1047h"); // enter alt screen (cursor stays at col 4)
        vt.feed(b"\x1b[1;1H"); // move to origin
        vt.feed(b"First");
        vt.feed(b"\x1b[?1047h"); // second enter → ignored
        assert!(vt.is_alternate_screen());
        assert_eq!(vt.row_text(0), "First"); // still in same alt screen
    }

    // ── Scroll region with DECSTBM edge cases ────────────────────────

    #[test]
    fn decstbm_invalid_range_ignored() {
        let mut vt = VirtualTerminal::new(10, 5);
        // top >= bottom → ignored
        vt.feed(b"\x1b[4;2r");
        assert_eq!(vt.scroll_top, 0);
        assert_eq!(vt.scroll_bottom, 4);
    }

    #[test]
    fn decstbm_bottom_at_screen_edge() {
        let mut vt = VirtualTerminal::new(10, 5);
        vt.feed(b"\x1b[2;5r"); // rows 2-5 (0-indexed: 1-4)
        assert_eq!(vt.scroll_top, 1);
        assert_eq!(vt.scroll_bottom, 4);
    }

    #[test]
    fn decstbm_homes_cursor_without_decom() {
        let mut vt = VirtualTerminal::new(10, 5);
        vt.feed(b"\x1b[3;3H"); // cursor at (2,2)
        vt.feed(b"\x1b[2;4r"); // set region → homes to (0,0)
        assert_eq!(vt.cursor(), (0, 0));
    }

    // ── Erase Display edge cases ─────────────────────────────────────

    #[test]
    fn erase_display_mode1_from_start_to_cursor() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"AAAAAAAAAA");
        vt.feed(b"BBBBBBBBBB");
        vt.feed(b"CCCCCCCCCC");
        vt.feed(b"\x1b[2;5H"); // row 1, col 4 (1-indexed)
        vt.feed(b"\x1b[1J"); // erase from start to cursor (inclusive)
        assert_eq!(vt.row_text(0), ""); // all of row 0 erased
        assert_eq!(vt.row_text(1), "     BBBBB"); // first 5 cols of row 1 erased
        assert_eq!(vt.row_text(2), "CCCCCCCCCC"); // untouched
    }

    #[test]
    fn erase_display_mode3_clears_scrollback() {
        let mut vt = VirtualTerminal::new(5, 2);
        vt.feed(b"AAAAA\r\nBBBBB\r\nCCCCC"); // scroll one line into scrollback
        assert!(vt.scrollback_len() > 0);
        vt.feed(b"\x1b[3J"); // mode 3: erase display + clear scrollback
        assert_eq!(vt.scrollback_len(), 0);
    }

    // ── Erase Line mode 1 (from start to cursor) ────────────────────

    #[test]
    fn erase_line_mode1() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed(b"ABCDEFGHIJ");
        vt.feed(b"\x1b[1;6H"); // cursor at col 5
        vt.feed(b"\x1b[1K"); // erase from start to cursor (inclusive)
        assert_eq!(vt.row_text(0), "      GHIJ");
    }

    // ── Soft reset (DECSTR) ─────────────────────────────────────────

    #[test]
    fn soft_reset_restores_defaults() {
        let mut vt = VirtualTerminal::new(10, 5);
        // Set various modes
        vt.feed(b"\x1b[?7l"); // disable autowrap
        vt.feed(b"\x1b[?25l"); // hide cursor
        vt.feed(b"\x1b[4h"); // enable insert mode
        vt.feed(b"\x1b[?6h"); // enable DECOM
        vt.feed(b"\x1b[2;4r"); // set scroll region
        // Soft reset
        vt.feed(b"\x1b[!p");
        assert!(vt.cursor_visible());
        assert_eq!(vt.scroll_top, 0);
        assert_eq!(vt.scroll_bottom, 4);
        // Verify autowrap restored by testing wrap behavior
        vt.feed(b"\x1b[1;1H");
        vt.feed(b"ABCDEFGHIJK");
        assert_eq!(vt.row_text(0), "ABCDEFGHIJ");
        assert_eq!(vt.row_text(1), "K"); // wraps → autowrap is on
    }

    // ── Wide char erase edge cases ──────────────────────────────────

    #[test]
    fn erase_line_splits_wide_char_at_boundary() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed("AB中DE".as_bytes()); // A(0) B(1) 中(2-3) D(4) E(5)
        vt.feed(b"\x1b[1;4H"); // cursor at col 3 (continuation of 中)
        vt.feed(b"\x1b[K"); // erase from cursor to end of line
        assert_invariants(&vt);
        // The lead of 中 at col 2 should be blanked (orphaned)
        assert_eq!(vt.char_at(2, 0), Some(' '));
    }

    #[test]
    fn dch_wide_char_continuation_at_boundary() {
        let mut vt = VirtualTerminal::new(10, 3);
        vt.feed("AB中DE".as_bytes());
        vt.feed(b"\x1b[1;3H"); // cursor at col 2 (lead of 中)
        vt.feed(b"\x1b[2P"); // delete 2 chars (both cells of 中)
        assert_eq!(vt.row_text(0), "ABDE");
        assert_invariants(&vt);
    }

    // ── Invariant checks on complex sequences ───────────────────────

    #[test]
    fn invariants_after_insert_delete_scroll_sequence() {
        let mut vt = VirtualTerminal::new(8, 4);
        // Complex sequence: fill, set region, IL, DL, SU, SD, ICH, DCH
        vt.feed(b"AABBCCDD");
        vt.feed(b"EEFFGGHH");
        vt.feed(b"IIJJKKLL");
        vt.feed(b"MMNNOOPP");
        vt.feed(b"\x1b[2;3r"); // scroll region rows 2-3
        vt.feed(b"\x1b[2;1H"); // cursor in region
        vt.feed(b"\x1b[1L"); // insert line
        vt.feed(b"\x1b[1M"); // delete line
        vt.feed(b"\x1b[1S"); // scroll up
        vt.feed(b"\x1b[1T"); // scroll down
        vt.feed(b"\x1b[2;4H\x1b[2@"); // ICH at col 3
        vt.feed(b"\x1b[1P"); // DCH
        assert_invariants(&vt);
    }

    #[test]
    fn invariants_after_wide_char_operations() {
        let mut vt = VirtualTerminal::new(6, 3);
        // Wide chars + editing operations
        vt.feed("中文字".as_bytes()); // 3 wide chars = 6 cols
        vt.feed(b"\x1b[1;1H\x1b[2@"); // ICH 2 at col 0
        assert_invariants(&vt);

        let mut vt2 = VirtualTerminal::new(6, 3);
        vt2.feed("中文字".as_bytes());
        vt2.feed(b"\x1b[1;1H\x1b[2P"); // DCH 2 at col 0
        assert_invariants(&vt2);

        let mut vt3 = VirtualTerminal::new(6, 3);
        vt3.feed("中文字".as_bytes());
        vt3.feed(b"\x1b[1;3H\x1b[2X"); // ECH 2 at col 2 (lead of 文)
        assert_invariants(&vt3);
    }
}

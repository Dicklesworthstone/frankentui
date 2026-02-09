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
    /// Last printed character for REP (CSI b) support.
    last_char: Option<char>,
    /// UTF-8 accumulator for multi-byte character decoding.
    utf8_buf: [u8; 4],
    /// Number of bytes accumulated in `utf8_buf`.
    utf8_len: u8,
    /// Expected total bytes for current UTF-8 sequence.
    utf8_expected: u8,
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
            last_char: None,
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
        }
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
                // Tab: advance to next 8-column stop
                let next_tab = (self.cursor_x / 8 + 1) * 8;
                self.cursor_x = next_tab.min(self.width.saturating_sub(1));
            }
            b'\x07' => {
                // Bell: ignored
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
                self.cursor_y = row.min(self.height.saturating_sub(1));
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
                            self.grid[row_start + i] = VCell::default();
                        }
                    }
                }
            }
            b'M' => {
                // Delete Lines (DL) — delete lines at cursor row, pulling up
                let n = Self::param(params, 0, 1);
                if self.cursor_y >= self.scroll_top && self.cursor_y <= self.scroll_bottom {
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
                            self.grid[bottom_start + i] = VCell::default();
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
                self.cursor_y = row.min(self.height.saturating_sub(1));
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
                self.cursor_y = 0;
            }
            b'@' => {
                // Insert Characters (ICH) — shift chars right at cursor, insert blanks
                let n = Self::param(params, 0, 1);
                let n = n.min(self.width.saturating_sub(self.cursor_x));
                let row_start = self.idx(0, self.cursor_y);
                let w = usize::from(self.width);
                let cx = usize::from(self.cursor_x);
                let count = usize::from(n);
                // Shift characters right within the row
                let row = &mut self.grid[row_start..row_start + w];
                row[cx..].rotate_right(count.min(w - cx));
                // Clear the inserted positions
                for cell in row.iter_mut().skip(cx).take(count.min(w - cx)) {
                    *cell = VCell::default();
                }
            }
            b'P' => {
                // Delete Characters (DCH) — shift chars left at cursor, fill blanks at end
                let n = Self::param(params, 0, 1);
                let n = n.min(self.width.saturating_sub(self.cursor_x));
                let row_start = self.idx(0, self.cursor_y);
                let w = usize::from(self.width);
                let cx = usize::from(self.cursor_x);
                let count = usize::from(n);
                // Shift characters left within the row
                let row = &mut self.grid[row_start..row_start + w];
                row[cx..].rotate_left(count.min(w - cx));
                // Clear the vacated positions at end
                for cell in row.iter_mut().skip(w - count.min(w - cx)) {
                    *cell = VCell::default();
                }
            }
            b'X' => {
                // Erase Characters (ECH) — erase N chars from cursor without moving cursor
                let n = Self::param(params, 0, 1);
                let n = n.min(self.width.saturating_sub(self.cursor_x));
                let start = self.idx(self.cursor_x, self.cursor_y);
                for i in 0..usize::from(n) {
                    self.grid[start + i] = VCell::default();
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
            b'p' if self.csi_intermediate.contains(&b'!') => {
                // Soft Reset (DECSTR) — CSI ! p
                self.current_style = CellStyle::default();
                self.cursor_visible = true;
                self.scroll_top = 0;
                self.scroll_bottom = self.height.saturating_sub(1);
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

    fn put_char(&mut self, ch: char) {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if char_width == 0 {
            return; // zero-width (combining marks, ZWJ): skip
        }

        if self.cursor_x >= self.width {
            self.cursor_x = 0;
            self.linefeed();
        }

        // Wide char at last column → wrap first
        if char_width == 2 && self.cursor_x + 1 >= self.width {
            let idx = self.idx(self.cursor_x, self.cursor_y);
            self.grid[idx] = VCell::default();
            self.cursor_x = 0;
            self.linefeed();
        }

        let last_col = self.width.saturating_sub(1);
        let immediate_wrap = self.quirks.screen_immediate_wrap && self.cursor_x == last_col;
        let idx = self.idx(self.cursor_x, self.cursor_y);

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
        } else {
            self.cursor_x += advance;
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
        let bottom_start = self.idx(0, self.scroll_bottom);
        for i in 0..usize::from(self.width) {
            self.grid[bottom_start + i] = VCell::default();
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
        let top_start = self.idx(0, self.scroll_top);
        for i in 0..usize::from(self.width) {
            self.grid[top_start + i] = VCell::default();
        }
    }

    fn erase_display(&mut self, mode: u16) {
        match mode {
            0 => {
                // Erase from cursor to end
                let start = self.idx(self.cursor_x, self.cursor_y);
                for cell in &mut self.grid[start..] {
                    *cell = VCell::default();
                }
            }
            1 => {
                // Erase from start to cursor
                let end = self.idx(self.cursor_x, self.cursor_y) + 1;
                for cell in &mut self.grid[..end] {
                    *cell = VCell::default();
                }
            }
            2 | 3 => {
                // Erase entire display (3 also clears scrollback)
                for cell in &mut self.grid {
                    *cell = VCell::default();
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
        let row_start = self.idx(0, y);
        match mode {
            0 => {
                // Erase from cursor to end of line
                let start = row_start + usize::from(self.cursor_x);
                let end = row_start + usize::from(self.width);
                for cell in &mut self.grid[start..end] {
                    *cell = VCell::default();
                }
            }
            1 => {
                // Erase from start to cursor
                let end = row_start + usize::from(self.cursor_x) + 1;
                for cell in &mut self.grid[row_start..end] {
                    *cell = VCell::default();
                }
            }
            2 => {
                // Erase entire line
                let end = row_start + usize::from(self.width);
                for cell in &mut self.grid[row_start..end] {
                    *cell = VCell::default();
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
}

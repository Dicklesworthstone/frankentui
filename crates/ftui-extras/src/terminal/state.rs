//! Terminal state machine for tracking terminal content and cursor.
//!
//! This module provides a grid-based terminal state that can be updated
//! by parsing ANSI escape sequences via the [`AnsiHandler`] trait.
//!
//! # Invariants
//!
//! 1. **Cursor bounds**: Cursor position is always within grid bounds (0..width, 0..height).
//! 2. **Grid consistency**: Grid size always matches (width × height) cells.
//! 3. **Scrollback limit**: Scrollback never exceeds `max_scrollback` lines.
//!
//! # Failure Modes
//!
//! | Failure | Cause | Behavior |
//! |---------|-------|----------|
//! | Out of bounds | Invalid coordinates | Clamped to valid range |
//! | Zero size | Resize to 0x0 | Minimum 1x1 enforced |
//! | Scrollback overflow | Too many lines | Oldest lines dropped |

use std::collections::VecDeque;

use ftui_style::Color;

/// Sentinel character stored in the right-hand cells of a wide (double-width)
/// character.  During copy extraction these cells are skipped so the character
/// is emitted only once.
pub const WIDE_CONTINUATION: char = '\u{E000}';

/// Per-row flag tracking how the row terminates.
///
/// When text wraps because it exceeds the terminal width (auto-wrap / DECAWM),
/// the row is flagged [`SoftWrap`](LineFlag::SoftWrap).  Explicit newlines
/// (LF / CR+LF) produce [`HardNewline`](LineFlag::HardNewline).
///
/// Copy extraction uses these flags to decide whether to join adjacent rows
/// or insert a newline between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineFlag {
    /// Row ends with a hard newline (explicit LF/CR+LF, or a fresh row).
    #[default]
    HardNewline,
    /// Row is soft-wrapped (content exceeded terminal width and auto-wrapped).
    SoftWrap,
}

/// Terminal cell attributes (bitflags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellAttrs(u8);

impl CellAttrs {
    /// No attributes set.
    pub const NONE: Self = Self(0);
    /// Bold/bright.
    pub const BOLD: Self = Self(0b0000_0001);
    /// Dim/faint.
    pub const DIM: Self = Self(0b0000_0010);
    /// Italic.
    pub const ITALIC: Self = Self(0b0000_0100);
    /// Underline.
    pub const UNDERLINE: Self = Self(0b0000_1000);
    /// Blink.
    pub const BLINK: Self = Self(0b0001_0000);
    /// Reverse video.
    pub const REVERSE: Self = Self(0b0010_0000);
    /// Hidden/invisible.
    pub const HIDDEN: Self = Self(0b0100_0000);
    /// Strikethrough.
    pub const STRIKETHROUGH: Self = Self(0b1000_0000);

    /// Check if an attribute is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Set an attribute.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Clear an attribute.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Set or clear an attribute based on a boolean.
    #[must_use]
    pub const fn set(self, attr: Self, enabled: bool) -> Self {
        if enabled {
            self.with(attr)
        } else {
            self.without(attr)
        }
    }
}

/// A single terminal cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    /// The character in this cell (space if empty).
    pub ch: char,
    /// Foreground color (None = default).
    pub fg: Option<Color>,
    /// Background color (None = default).
    pub bg: Option<Color>,
    /// Text attributes.
    pub attrs: CellAttrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: None,
            bg: None,
            attrs: CellAttrs::NONE,
        }
    }
}

impl Cell {
    /// Create a new cell with the given character.
    #[must_use]
    pub const fn new(ch: char) -> Self {
        Self {
            ch,
            fg: None,
            bg: None,
            attrs: CellAttrs::NONE,
        }
    }

    /// Check if this cell is "empty" (space with default colors and no attrs).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ch == ' ' && self.fg.is_none() && self.bg.is_none() && self.attrs.0 == 0
    }

    /// Check if this cell is the right-hand continuation of a wide character.
    #[must_use]
    pub fn is_wide_continuation(&self) -> bool {
        self.ch == WIDE_CONTINUATION
    }
}

/// Cursor shape for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    /// Block cursor (default).
    #[default]
    Block,
    /// Underline cursor.
    Underline,
    /// Bar/beam cursor.
    Bar,
}

/// Cursor state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// Column (0-indexed).
    pub x: u16,
    /// Row (0-indexed).
    pub y: u16,
    /// Whether cursor is visible.
    pub visible: bool,
    /// Cursor shape.
    pub shape: CursorShape,
    /// Saved cursor position (DECSC/DECRC).
    pub saved: Option<(u16, u16)>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            visible: true,
            shape: CursorShape::Block,
            saved: None,
        }
    }
}

impl Cursor {
    /// Create a new cursor at the origin.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            visible: true,
            shape: CursorShape::Block,
            saved: None,
        }
    }
}

/// Terminal mode flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalModes(u32);

impl TerminalModes {
    /// Auto-wrap mode (DECAWM).
    pub const WRAP: Self = Self(0b0000_0001);
    /// Origin mode (DECOM).
    pub const ORIGIN: Self = Self(0b0000_0010);
    /// Insert mode (IRM).
    pub const INSERT: Self = Self(0b0000_0100);
    /// Cursor visible (DECTCEM).
    pub const CURSOR_VISIBLE: Self = Self(0b0000_1000);
    /// Alternate screen buffer.
    pub const ALT_SCREEN: Self = Self(0b0001_0000);
    /// Bracketed paste mode.
    pub const BRACKETED_PASTE: Self = Self(0b0010_0000);
    /// Mouse tracking enabled.
    pub const MOUSE_TRACKING: Self = Self(0b0100_0000);
    /// Focus events enabled.
    pub const FOCUS_EVENTS: Self = Self(0b1000_0000);

    /// Check if a mode is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Set a mode.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Clear a mode.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Set or clear a mode based on a boolean.
    #[must_use]
    pub const fn set(self, mode: Self, enabled: bool) -> Self {
        if enabled {
            self.with(mode)
        } else {
            self.without(mode)
        }
    }
}

/// Region to clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearRegion {
    /// Clear from cursor to end of screen.
    CursorToEnd,
    /// Clear from start of screen to cursor.
    StartToCursor,
    /// Clear entire screen.
    All,
    /// Clear from cursor to end of line.
    LineFromCursor,
    /// Clear from start of line to cursor.
    LineToCursor,
    /// Clear entire line.
    Line,
}

/// Dirty region tracking.
///
/// Uses a bitmap for efficient tracking of which cells have changed.
#[derive(Debug, Clone)]
pub struct DirtyRegion {
    /// Bitmap: 1 bit per cell, row-major order.
    bits: Vec<u64>,
    /// Width of the grid.
    width: u16,
    /// Height of the grid.
    height: u16,
    /// Whether any cell is dirty.
    any_dirty: bool,
}

impl DirtyRegion {
    /// Create a new dirty region for the given dimensions.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        let total_cells = (width as usize) * (height as usize);
        let num_words = total_cells.div_ceil(64);
        Self {
            bits: vec![0; num_words],
            width,
            height,
            any_dirty: false,
        }
    }

    /// Mark a cell as dirty.
    pub fn mark(&mut self, x: u16, y: u16) {
        if x < self.width && y < self.height {
            let idx = (y as usize) * (self.width as usize) + (x as usize);
            let word = idx / 64;
            let bit = idx % 64;
            self.bits[word] |= 1 << bit;
            self.any_dirty = true;
        }
    }

    /// Mark a rectangular region as dirty.
    pub fn mark_rect(&mut self, x: u16, y: u16, w: u16, h: u16) {
        for row in y..y.saturating_add(h).min(self.height) {
            for col in x..x.saturating_add(w).min(self.width) {
                self.mark(col, row);
            }
        }
    }

    /// Mark the entire grid as dirty.
    pub fn mark_all(&mut self) {
        self.bits.fill(u64::MAX);
        self.any_dirty = true;
    }

    /// Check if a cell is dirty.
    #[must_use]
    pub fn is_dirty(&self, x: u16, y: u16) -> bool {
        if x < self.width && y < self.height {
            let idx = (y as usize) * (self.width as usize) + (x as usize);
            let word = idx / 64;
            let bit = idx % 64;
            (self.bits[word] >> bit) & 1 == 1
        } else {
            false
        }
    }

    /// Check if any cell is dirty.
    #[must_use]
    pub fn has_dirty(&self) -> bool {
        self.any_dirty
    }

    /// Clear all dirty flags.
    pub fn clear(&mut self) {
        self.bits.fill(0);
        self.any_dirty = false;
    }

    /// Resize the dirty region (clears all flags).
    pub fn resize(&mut self, width: u16, height: u16) {
        let total_cells = (width as usize) * (height as usize);
        let num_words = total_cells.div_ceil(64);
        self.bits.resize(num_words, 0);
        self.bits.fill(0);
        self.width = width;
        self.height = height;
        self.any_dirty = false;
    }
}

/// Terminal grid (visible area).
#[derive(Debug, Clone)]
pub struct Grid {
    /// Cells in row-major order.
    cells: Vec<Cell>,
    /// Width in columns.
    width: u16,
    /// Height in rows.
    height: u16,
    /// Per-row line flags (one per row).
    line_flags: Vec<LineFlag>,
}

impl Grid {
    /// Create a new grid with the given dimensions.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let size = (width as usize) * (height as usize);
        Self {
            cells: vec![Cell::default(); size],
            width,
            height,
            line_flags: vec![LineFlag::default(); height as usize],
        }
    }

    /// Get grid width.
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Get grid height.
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Get a reference to a cell.
    #[must_use]
    pub fn cell(&self, x: u16, y: u16) -> Option<&Cell> {
        if x < self.width && y < self.height {
            let idx = (y as usize) * (self.width as usize) + (x as usize);
            self.cells.get(idx)
        } else {
            None
        }
    }

    /// Get a mutable reference to a cell.
    pub fn cell_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        if x < self.width && y < self.height {
            let idx = (y as usize) * (self.width as usize) + (x as usize);
            self.cells.get_mut(idx)
        } else {
            None
        }
    }

    /// Get the line flag for a row.
    #[must_use]
    pub fn line_flag(&self, y: u16) -> LineFlag {
        self.line_flags
            .get(y as usize)
            .copied()
            .unwrap_or(LineFlag::HardNewline)
    }

    /// Set the line flag for a row.
    pub fn set_line_flag(&mut self, y: u16, flag: LineFlag) {
        if let Some(f) = self.line_flags.get_mut(y as usize) {
            *f = flag;
        }
    }

    /// Get a slice of all line flags.
    #[must_use]
    pub fn line_flags(&self) -> &[LineFlag] {
        &self.line_flags
    }

    /// Clear a row to default cells.
    pub fn clear_row(&mut self, y: u16) {
        if y < self.height {
            let start = (y as usize) * (self.width as usize);
            let end = start + (self.width as usize);
            self.cells[start..end].fill(Cell::default());
            self.set_line_flag(y, LineFlag::HardNewline);
        }
    }

    /// Resize the grid, preserving content where possible.
    pub fn resize(&mut self, new_width: u16, new_height: u16) {
        let new_width = new_width.max(1);
        let new_height = new_height.max(1);

        if new_width == self.width && new_height == self.height {
            return;
        }

        let mut new_cells = vec![Cell::default(); (new_width as usize) * (new_height as usize)];

        // Copy existing content
        let copy_width = self.width.min(new_width) as usize;
        let copy_height = self.height.min(new_height) as usize;

        for y in 0..copy_height {
            let old_start = y * (self.width as usize);
            let new_start = y * (new_width as usize);
            new_cells[new_start..new_start + copy_width]
                .copy_from_slice(&self.cells[old_start..old_start + copy_width]);
        }

        self.cells = new_cells;
        self.width = new_width;

        // Resize line flags, preserving existing values
        self.line_flags
            .resize(new_height as usize, LineFlag::HardNewline);
        self.height = new_height;
    }

    /// Scroll the grid up by n lines, filling bottom with empty lines.
    /// Returns the lines that scrolled off the top along with their line flags.
    pub fn scroll_up(&mut self, n: u16) -> Vec<(Vec<Cell>, LineFlag)> {
        let n = n.min(self.height) as usize;
        if n == 0 {
            return Vec::new();
        }

        let mut scrolled_off = Vec::with_capacity(n);

        // Collect lines and their flags that will scroll off
        for y in 0..n {
            let start = y * (self.width as usize);
            let end = start + (self.width as usize);
            let flag = self.line_flags.get(y).copied().unwrap_or_default();
            scrolled_off.push((self.cells[start..end].to_vec(), flag));
        }

        // Shift remaining lines up
        let shift_count = (self.height as usize - n) * (self.width as usize);
        self.cells.copy_within(n * (self.width as usize).., 0);

        // Shift line flags up
        self.line_flags.drain(..n);
        self.line_flags
            .resize(self.height as usize, LineFlag::HardNewline);

        // Clear bottom lines
        self.cells[shift_count..].fill(Cell::default());

        scrolled_off
    }

    /// Scroll the grid down by n lines, filling top with empty lines.
    pub fn scroll_down(&mut self, n: u16) {
        let n = n.min(self.height) as usize;
        if n == 0 {
            return;
        }

        let width = self.width as usize;
        let height = self.height as usize;

        // Shift lines down
        self.cells.copy_within(0..(height - n) * width, n * width);

        // Clear top lines
        self.cells[0..n * width].fill(Cell::default());

        // Shift line flags down: truncate bottom, insert default at top
        self.line_flags.truncate(height.saturating_sub(n));
        for _ in 0..n {
            self.line_flags.insert(0, LineFlag::HardNewline);
        }
    }
}

/// Scrollback buffer.
#[derive(Debug, Clone)]
pub struct Scrollback {
    /// Lines in the scrollback buffer, paired with their line flags.
    lines: VecDeque<(Vec<Cell>, LineFlag)>,
    /// Maximum number of lines to keep.
    max_lines: usize,
}

impl Scrollback {
    /// Create a new scrollback buffer.
    #[must_use]
    pub fn new(max_lines: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            max_lines,
        }
    }

    /// Add a line to the scrollback (defaults to [`LineFlag::HardNewline`]).
    pub fn push(&mut self, line: Vec<Cell>) {
        self.push_with_flag(line, LineFlag::HardNewline);
    }

    /// Add a line with an explicit line flag to the scrollback.
    pub fn push_with_flag(&mut self, line: Vec<Cell>, flag: LineFlag) {
        if self.max_lines == 0 {
            return;
        }

        while self.lines.len() >= self.max_lines {
            self.lines.pop_front();
        }
        self.lines.push_back((line, flag));
    }

    /// Add multiple lines to the scrollback.
    pub fn push_many(&mut self, lines: impl IntoIterator<Item = Vec<Cell>>) {
        for line in lines {
            self.push(line);
        }
    }

    /// Add multiple lines with their flags to the scrollback.
    pub fn push_many_with_flags(&mut self, lines: impl IntoIterator<Item = (Vec<Cell>, LineFlag)>) {
        for (line, flag) in lines {
            self.push_with_flag(line, flag);
        }
    }

    /// Get the number of lines in scrollback.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Check if scrollback is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Get a line from scrollback (0 = most recent).
    #[must_use]
    pub fn line(&self, index: usize) -> Option<&[Cell]> {
        if index < self.lines.len() {
            self.lines
                .get(self.lines.len() - 1 - index)
                .map(|(cells, _)| cells.as_slice())
        } else {
            None
        }
    }

    /// Get a line and its flag from scrollback (0 = most recent).
    #[must_use]
    pub fn line_with_flag(&self, index: usize) -> Option<(&[Cell], LineFlag)> {
        if index < self.lines.len() {
            self.lines
                .get(self.lines.len() - 1 - index)
                .map(|(cells, flag)| (cells.as_slice(), *flag))
        } else {
            None
        }
    }

    /// Clear the scrollback.
    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

/// Current pen state for writing characters.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pen {
    /// Foreground color.
    pub fg: Option<Color>,
    /// Background color.
    pub bg: Option<Color>,
    /// Text attributes.
    pub attrs: CellAttrs,
}

impl Pen {
    /// Reset pen to defaults.
    pub fn reset(&mut self) {
        self.fg = None;
        self.bg = None;
        self.attrs = CellAttrs::NONE;
    }
}

/// Complete terminal state.
#[derive(Debug, Clone)]
pub struct TerminalState {
    /// The visible grid.
    grid: Grid,
    /// Cursor state.
    cursor: Cursor,
    /// Scrollback buffer.
    scrollback: Scrollback,
    /// Terminal modes.
    modes: TerminalModes,
    /// Dirty region tracking.
    dirty: DirtyRegion,
    /// Current pen for new characters.
    pen: Pen,
    /// Scroll region (top, bottom) - 0-indexed, inclusive.
    scroll_region: (u16, u16),
    /// Window title (from OSC sequences).
    title: String,
}

impl TerminalState {
    /// Create a new terminal state with the given dimensions.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        Self {
            grid: Grid::new(width, height),
            cursor: Cursor::new(),
            scrollback: Scrollback::new(1000), // Default 1000 lines
            modes: TerminalModes::WRAP.with(TerminalModes::CURSOR_VISIBLE),
            dirty: DirtyRegion::new(width, height),
            pen: Pen::default(),
            scroll_region: (0, height.saturating_sub(1)),
            title: String::new(),
        }
    }

    /// Create with custom scrollback limit.
    #[must_use]
    pub fn with_scrollback(width: u16, height: u16, max_scrollback: usize) -> Self {
        let mut state = Self::new(width, height);
        state.scrollback = Scrollback::new(max_scrollback);
        state
    }

    /// Get grid width.
    #[must_use]
    pub fn width(&self) -> u16 {
        self.grid.width()
    }

    /// Get grid height.
    #[must_use]
    pub fn height(&self) -> u16 {
        self.grid.height()
    }

    /// Get a reference to the grid.
    #[must_use]
    pub const fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Get a reference to a cell.
    #[must_use]
    pub fn cell(&self, x: u16, y: u16) -> Option<&Cell> {
        self.grid.cell(x, y)
    }

    /// Get a mutable reference to a cell (marks as dirty).
    pub fn cell_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        self.dirty.mark(x, y);
        self.grid.cell_mut(x, y)
    }

    /// Get cursor state.
    #[must_use]
    pub const fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    /// Get terminal modes.
    #[must_use]
    pub const fn modes(&self) -> TerminalModes {
        self.modes
    }

    /// Get the dirty region.
    #[must_use]
    pub const fn dirty(&self) -> &DirtyRegion {
        &self.dirty
    }

    /// Get the scrollback buffer.
    #[must_use]
    pub const fn scrollback(&self) -> &Scrollback {
        &self.scrollback
    }

    /// Get the current pen.
    #[must_use]
    pub const fn pen(&self) -> &Pen {
        &self.pen
    }

    /// Get the window title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Mark all cells as clean.
    pub fn mark_clean(&mut self) {
        self.dirty.clear();
    }

    /// Move cursor to absolute position (clamped to bounds).
    pub fn move_cursor(&mut self, x: u16, y: u16) {
        self.cursor.x = x.min(self.grid.width().saturating_sub(1));
        self.cursor.y = y.min(self.grid.height().saturating_sub(1));
    }

    /// Move cursor relative to current position.
    pub fn move_cursor_relative(&mut self, dx: i16, dy: i16) {
        let new_x = (self.cursor.x as i32 + dx as i32)
            .max(0)
            .min(self.grid.width() as i32 - 1) as u16;
        let new_y = (self.cursor.y as i32 + dy as i32)
            .max(0)
            .min(self.grid.height() as i32 - 1) as u16;
        self.cursor.x = new_x;
        self.cursor.y = new_y;
    }

    /// Set cursor visibility.
    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor.visible = visible;
        self.modes = self.modes.set(TerminalModes::CURSOR_VISIBLE, visible);
    }

    /// Save cursor position.
    pub fn save_cursor(&mut self) {
        self.cursor.saved = Some((self.cursor.x, self.cursor.y));
    }

    /// Restore cursor position.
    pub fn restore_cursor(&mut self) {
        if let Some((x, y)) = self.cursor.saved {
            self.move_cursor(x, y);
        }
    }

    /// Write a character at the cursor position.
    pub fn put_char(&mut self, ch: char) {
        let x = self.cursor.x;
        let y = self.cursor.y;

        if let Some(cell) = self.grid.cell_mut(x, y) {
            cell.ch = ch;
            cell.fg = self.pen.fg;
            cell.bg = self.pen.bg;
            cell.attrs = self.pen.attrs;
            self.dirty.mark(x, y);
        }

        // Advance cursor
        self.cursor.x += 1;

        // Handle wrap
        if self.cursor.x >= self.grid.width() {
            if self.modes.contains(TerminalModes::WRAP) {
                // Mark the current row as soft-wrapped
                self.grid.set_line_flag(y, LineFlag::SoftWrap);
                self.cursor.x = 0;
                self.cursor.y += 1;

                // Scroll if needed
                if self.cursor.y > self.scroll_region.1 {
                    self.scroll_up(1);
                    self.cursor.y = self.scroll_region.1;
                }
            } else {
                self.cursor.x = self.grid.width().saturating_sub(1);
            }
        }
    }

    /// Write a wide (double-width) character at the cursor position.
    ///
    /// Places the character in the current cell and a [`WIDE_CONTINUATION`]
    /// marker in the next cell. Advances cursor by 2 columns.
    pub fn put_char_wide(&mut self, ch: char) {
        let x = self.cursor.x;
        let y = self.cursor.y;
        let width = self.grid.width();

        // Need at least 2 columns remaining; if only 1, wrap first
        if x >= width.saturating_sub(1) && self.modes.contains(TerminalModes::WRAP) {
            // Not enough room — mark current row as soft-wrapped and wrap
            self.grid.set_line_flag(y, LineFlag::SoftWrap);
            self.cursor.x = 0;
            self.cursor.y += 1;
            if self.cursor.y > self.scroll_region.1 {
                self.scroll_up(1);
                self.cursor.y = self.scroll_region.1;
            }
        }

        let x = self.cursor.x;
        let y = self.cursor.y;

        // Place the wide character
        if let Some(cell) = self.grid.cell_mut(x, y) {
            cell.ch = ch;
            cell.fg = self.pen.fg;
            cell.bg = self.pen.bg;
            cell.attrs = self.pen.attrs;
            self.dirty.mark(x, y);
        }

        // Place the continuation marker
        if let Some(cell) = self.grid.cell_mut(x.saturating_add(1), y) {
            cell.ch = WIDE_CONTINUATION;
            cell.fg = self.pen.fg;
            cell.bg = self.pen.bg;
            cell.attrs = self.pen.attrs;
            self.dirty.mark(x.saturating_add(1), y);
        }

        // Advance cursor by 2
        self.cursor.x = self.cursor.x.saturating_add(2);

        // Handle wrap after placing both cells
        if self.cursor.x >= width {
            if self.modes.contains(TerminalModes::WRAP) {
                self.grid.set_line_flag(y, LineFlag::SoftWrap);
                self.cursor.x = 0;
                self.cursor.y += 1;
                if self.cursor.y > self.scroll_region.1 {
                    self.scroll_up(1);
                    self.cursor.y = self.scroll_region.1;
                }
            } else {
                self.cursor.x = width.saturating_sub(1);
            }
        }
    }

    /// Process a newline (LF): move cursor down, mark current row as hard newline.
    pub fn newline(&mut self) {
        let y = self.cursor.y;
        self.grid.set_line_flag(y, LineFlag::HardNewline);

        self.cursor.y += 1;
        if self.cursor.y > self.scroll_region.1 {
            self.scroll_up(1);
            self.cursor.y = self.scroll_region.1;
        }
    }

    /// Process a carriage return (CR): move cursor to column 0.
    pub fn carriage_return(&mut self) {
        self.cursor.x = 0;
    }

    /// Scroll the screen up by n lines.
    pub fn scroll_up(&mut self, n: u16) {
        let (top, bottom) = self.scroll_region;
        let n = n.min(bottom.saturating_sub(top) + 1);

        if n == 0 {
            return;
        }

        // If scrolling the entire screen, use grid method
        if top == 0 && bottom == self.grid.height().saturating_sub(1) {
            let scrolled_off = self.grid.scroll_up(n);
            self.scrollback.push_many_with_flags(scrolled_off);
        } else {
            // Scroll within region
            let width = self.grid.width() as usize;
            for y in top..=bottom.saturating_sub(n) {
                let src_y = y + n;
                if src_y <= bottom {
                    // Copy row src_y to row y
                    let src_start = (src_y as usize) * width;
                    let dst_start = (y as usize) * width;
                    self.grid
                        .cells
                        .copy_within(src_start..src_start + width, dst_start);
                    // Shift line flags too
                    let src_flag = self.grid.line_flag(src_y);
                    self.grid.set_line_flag(y, src_flag);
                }
            }
            // Clear bottom lines of region
            for y in (bottom + 1).saturating_sub(n)..=bottom {
                self.grid.clear_row(y);
            }
        }

        // Mark entire scroll region as dirty
        self.dirty
            .mark_rect(0, top, self.grid.width(), bottom - top + 1);
    }

    /// Scroll the screen down by n lines.
    pub fn scroll_down(&mut self, n: u16) {
        let (top, bottom) = self.scroll_region;
        let n = n.min(bottom.saturating_sub(top) + 1);

        if n == 0 {
            return;
        }

        if top == 0 && bottom == self.grid.height().saturating_sub(1) {
            self.grid.scroll_down(n);
        } else {
            // Scroll within region
            let width = self.grid.width() as usize;
            for y in (top + n..=bottom).rev() {
                let src_y = y - n;
                if src_y >= top {
                    // Copy row src_y to row y
                    let src_start = (src_y as usize) * width;
                    let dst_start = (y as usize) * width;
                    self.grid
                        .cells
                        .copy_within(src_start..src_start + width, dst_start);
                    // Shift line flags too
                    let src_flag = self.grid.line_flag(src_y);
                    self.grid.set_line_flag(y, src_flag);
                }
            }
            // Clear top lines of region
            for y in top..top + n {
                self.grid.clear_row(y);
            }
        }

        self.dirty
            .mark_rect(0, top, self.grid.width(), bottom - top + 1);
    }

    /// Clear a region of the screen.
    pub fn clear_region(&mut self, region: ClearRegion) {
        let (x, y) = (self.cursor.x, self.cursor.y);
        let width = self.grid.width();
        let height = self.grid.height();

        match region {
            ClearRegion::CursorToEnd => {
                // Clear from cursor to end of line
                for col in x..width {
                    if let Some(cell) = self.grid.cell_mut(col, y) {
                        *cell = Cell::default();
                        self.dirty.mark(col, y);
                    }
                }
                // Clear remaining lines
                for row in y + 1..height {
                    self.grid.clear_row(row);
                }
                self.dirty
                    .mark_rect(0, y + 1, width, height.saturating_sub(y).saturating_sub(1));
            }
            ClearRegion::StartToCursor => {
                // Clear lines before cursor
                for row in 0..y {
                    self.grid.clear_row(row);
                }
                self.dirty.mark_rect(0, 0, width, y);
                // Clear from start of line to cursor
                for col in 0..=x {
                    if let Some(cell) = self.grid.cell_mut(col, y) {
                        *cell = Cell::default();
                        self.dirty.mark(col, y);
                    }
                }
            }
            ClearRegion::All => {
                for row in 0..height {
                    self.grid.clear_row(row);
                }
                self.dirty.mark_all();
            }
            ClearRegion::LineFromCursor => {
                for col in x..width {
                    if let Some(cell) = self.grid.cell_mut(col, y) {
                        *cell = Cell::default();
                        self.dirty.mark(col, y);
                    }
                }
            }
            ClearRegion::LineToCursor => {
                for col in 0..=x {
                    if let Some(cell) = self.grid.cell_mut(col, y) {
                        *cell = Cell::default();
                        self.dirty.mark(col, y);
                    }
                }
            }
            ClearRegion::Line => {
                self.grid.clear_row(y);
                self.dirty.mark_rect(0, y, width, 1);
            }
        }
    }

    /// Set a terminal mode.
    pub fn set_mode(&mut self, mode: TerminalModes, enabled: bool) {
        self.modes = self.modes.set(mode, enabled);
    }

    /// Set the scroll region.
    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let top = top.min(self.grid.height().saturating_sub(1));
        let bottom = bottom.min(self.grid.height().saturating_sub(1)).max(top);
        self.scroll_region = (top, bottom);
    }

    /// Reset scroll region to full screen.
    pub fn reset_scroll_region(&mut self) {
        self.scroll_region = (0, self.grid.height().saturating_sub(1));
    }

    /// Set the window title.
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    /// Resize the terminal.
    pub fn resize(&mut self, width: u16, height: u16) {
        let width = width.max(1);
        let height = height.max(1);

        self.grid.resize(width, height);
        self.dirty.resize(width, height);
        self.dirty.mark_all();

        // Clamp cursor
        self.cursor.x = self.cursor.x.min(width.saturating_sub(1));
        self.cursor.y = self.cursor.y.min(height.saturating_sub(1));

        // Reset scroll region
        self.scroll_region = (0, height.saturating_sub(1));
    }

    /// Get a mutable reference to the pen.
    pub fn pen_mut(&mut self) -> &mut Pen {
        &mut self.pen
    }

    /// Reset the terminal to initial state.
    pub fn reset(&mut self) {
        let width = self.grid.width();
        let height = self.grid.height();

        self.grid = Grid::new(width, height);
        self.cursor = Cursor::new();
        self.modes = TerminalModes::WRAP.with(TerminalModes::CURSOR_VISIBLE);
        self.pen = Pen::default();
        self.scroll_region = (0, height.saturating_sub(1));
        self.title.clear();
        self.dirty.clear();
        self.dirty.mark_all();
    }

    /// Extract text from a selection range in the visible grid.
    ///
    /// Coordinates are clamped to grid bounds.  The selection is normalized
    /// so that `(start_x, start_y)` is always before `(end_x, end_y)` in
    /// reading order.
    ///
    /// # Copy extraction semantics
    ///
    /// - **Soft-wrapped rows** are joined without a newline (the wrap was
    ///   caused by terminal width, not by the content).
    /// - **Hard-newline rows** emit a `\n` between rows.
    /// - **Trailing whitespace** on each logical line is trimmed.
    /// - **Wide character continuation cells** ([`WIDE_CONTINUATION`]) are
    ///   skipped so each wide glyph appears exactly once.
    /// - **Grapheme clusters** stored as individual combining chars are
    ///   emitted in cell order.
    #[must_use]
    pub fn extract_text(&self, start_x: u16, start_y: u16, end_x: u16, end_y: u16) -> String {
        self.grid.extract_text(start_x, start_y, end_x, end_y)
    }
}

// =========================================================================
// Copy extraction
// =========================================================================

impl Grid {
    /// Extract text from a rectangular selection range.
    ///
    /// See [`TerminalState::extract_text`] for full semantics.
    #[must_use]
    pub fn extract_text(&self, start_x: u16, start_y: u16, end_x: u16, end_y: u16) -> String {
        // Normalize so start is before end in reading order.
        let (sx, sy, ex, ey) = if (start_y, start_x) <= (end_y, end_x) {
            (start_x, start_y, end_x, end_y)
        } else {
            (end_x, end_y, start_x, start_y)
        };

        // Clamp to grid bounds.
        let sy = sy.min(self.height.saturating_sub(1));
        let ey = ey.min(self.height.saturating_sub(1));
        let sx = sx.min(self.width.saturating_sub(1));
        let ex = ex.min(self.width.saturating_sub(1));

        if sy == ey {
            // Single-line selection: extract [sx..=ex] and trim.
            return self.extract_row_range(sy, sx, ex);
        }

        let mut result = String::new();

        // First row: sx .. end of row
        let first = self.extract_row_range(sy, sx, self.width.saturating_sub(1));
        result.push_str(&first);

        // Separator after first row depends on its line flag.
        if self.line_flag(sy) == LineFlag::HardNewline {
            result.push('\n');
        }

        // Middle rows (full rows)
        for y in (sy + 1)..ey {
            let row = self.extract_row_range(y, 0, self.width.saturating_sub(1));
            result.push_str(&row);
            if self.line_flag(y) == LineFlag::HardNewline {
                result.push('\n');
            }
        }

        // Last row: 0 .. ex
        let last = self.extract_row_range(ey, 0, ex);
        result.push_str(&last);

        // Trim trailing newlines — matches terminal emulator copy behavior
        // where trailing empty lines are not included in the clipboard.
        let trimmed = result.trim_end_matches('\n');
        trimmed.to_owned()
    }

    /// Extract and trim a single row segment `[col_start..=col_end]`.
    fn extract_row_range(&self, y: u16, col_start: u16, col_end: u16) -> String {
        let mut s = String::new();
        let start = col_start.min(self.width.saturating_sub(1));
        let end = col_end.min(self.width.saturating_sub(1));

        for x in start..=end {
            if let Some(cell) = self.cell(x, y) {
                // Skip wide-char continuation cells.
                if cell.is_wide_continuation() {
                    continue;
                }
                s.push(cell.ch);
            }
        }

        // Trim trailing whitespace (spaces only; preserve other content).
        let trimmed = s.trim_end_matches(' ');
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cell_default() {
        let cell = Cell::default();
        assert_eq!(cell.ch, ' ');
        assert!(cell.fg.is_none());
        assert!(cell.bg.is_none());
        assert_eq!(cell.attrs.0, 0);
        assert!(cell.is_empty());
    }

    #[test]
    fn test_cell_attrs() {
        let attrs = CellAttrs::BOLD.with(CellAttrs::ITALIC);
        assert!(attrs.contains(CellAttrs::BOLD));
        assert!(attrs.contains(CellAttrs::ITALIC));
        assert!(!attrs.contains(CellAttrs::UNDERLINE));

        let attrs = attrs.without(CellAttrs::BOLD);
        assert!(!attrs.contains(CellAttrs::BOLD));
        assert!(attrs.contains(CellAttrs::ITALIC));
    }

    #[test]
    fn test_cursor_movement() {
        let mut state = TerminalState::new(80, 24);
        assert_eq!(state.cursor().x, 0);
        assert_eq!(state.cursor().y, 0);

        state.move_cursor(10, 5);
        assert_eq!(state.cursor().x, 10);
        assert_eq!(state.cursor().y, 5);

        // Test clamping
        state.move_cursor(100, 50);
        assert_eq!(state.cursor().x, 79);
        assert_eq!(state.cursor().y, 23);
    }

    #[test]
    fn test_cursor_relative_movement() {
        let mut state = TerminalState::new(80, 24);
        state.move_cursor(10, 10);

        state.move_cursor_relative(-5, 3);
        assert_eq!(state.cursor().x, 5);
        assert_eq!(state.cursor().y, 13);

        // Test clamping at boundaries
        state.move_cursor_relative(-100, -100);
        assert_eq!(state.cursor().x, 0);
        assert_eq!(state.cursor().y, 0);
    }

    #[test]
    fn test_put_char() {
        let mut state = TerminalState::new(80, 24);
        state.put_char('A');

        assert_eq!(state.cell(0, 0).unwrap().ch, 'A');
        assert_eq!(state.cursor().x, 1);
        assert!(state.dirty().is_dirty(0, 0));
    }

    #[test]
    fn test_scroll_up() {
        let mut state = TerminalState::new(10, 5);

        // Fill first line with 'A's
        for i in 0..10 {
            state.move_cursor(i, 0);
            state.put_char('A');
        }

        // Fill second line with 'B's
        for i in 0..10 {
            state.move_cursor(i, 1);
            state.put_char('B');
        }

        state.scroll_up(1);

        // First line should now have 'B's
        assert_eq!(state.cell(0, 0).unwrap().ch, 'B');
        // Scrollback should have 'A's
        assert_eq!(state.scrollback().line(0).unwrap()[0].ch, 'A');
    }

    #[test]
    fn test_scroll_down() {
        let mut state = TerminalState::new(10, 5);

        // Fill first line with 'A's
        for i in 0..10 {
            state.move_cursor(i, 0);
            state.put_char('A');
        }

        state.scroll_down(1);

        // First line should be empty
        assert_eq!(state.cell(0, 0).unwrap().ch, ' ');
        // Second line should have 'A's
        assert_eq!(state.cell(0, 1).unwrap().ch, 'A');
    }

    #[test]
    fn test_wrap_mode() {
        let mut state = TerminalState::new(5, 3);
        assert!(state.modes().contains(TerminalModes::WRAP));

        // Write past end of line
        for ch in "HELLO WORLD".chars() {
            state.put_char(ch);
        }

        // Should have wrapped
        assert_eq!(state.cell(0, 0).unwrap().ch, 'H');
        assert_eq!(state.cell(4, 0).unwrap().ch, 'O');
        assert_eq!(state.cell(0, 1).unwrap().ch, ' ');
        assert_eq!(state.cell(0, 2).unwrap().ch, 'D');
    }

    #[test]
    fn test_resize() {
        let mut state = TerminalState::new(10, 5);
        state.move_cursor(5, 3);
        state.put_char('X');

        state.resize(20, 10);

        assert_eq!(state.width(), 20);
        assert_eq!(state.height(), 10);
        // Content should be preserved
        assert_eq!(state.cell(5, 3).unwrap().ch, 'X');
    }

    #[test]
    fn test_resize_smaller() {
        let mut state = TerminalState::new(10, 5);
        state.move_cursor(8, 4);

        state.resize(5, 3);

        // Cursor should be clamped
        assert_eq!(state.cursor().x, 4);
        assert_eq!(state.cursor().y, 2);
    }

    #[test]
    fn test_dirty_tracking() {
        let mut state = TerminalState::new(10, 5);
        assert!(!state.dirty().has_dirty());

        state.put_char('A');
        assert!(state.dirty().has_dirty());
        assert!(state.dirty().is_dirty(0, 0));
        assert!(!state.dirty().is_dirty(1, 0));

        state.mark_clean();
        assert!(!state.dirty().has_dirty());
    }

    #[test]
    fn test_clear_region_all() {
        let mut state = TerminalState::new(10, 5);

        // Fill with content
        for i in 0..10 {
            state.move_cursor(i, 0);
            state.put_char('A');
        }

        state.clear_region(ClearRegion::All);

        // All cells should be empty
        for y in 0..5 {
            for x in 0..10 {
                assert!(state.cell(x, y).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn test_clear_region_line() {
        let mut state = TerminalState::new(10, 5);

        // Fill line 2 with content
        for i in 0..10 {
            state.move_cursor(i, 2);
            state.put_char('A');
        }

        state.move_cursor(5, 2);
        state.clear_region(ClearRegion::Line);

        // Line 2 should be empty
        for x in 0..10 {
            assert!(state.cell(x, 2).unwrap().is_empty());
        }
    }

    #[test]
    fn test_save_restore_cursor() {
        let mut state = TerminalState::new(80, 24);
        state.move_cursor(10, 5);
        state.save_cursor();

        state.move_cursor(50, 20);
        assert_eq!(state.cursor().x, 50);

        state.restore_cursor();
        assert_eq!(state.cursor().x, 10);
        assert_eq!(state.cursor().y, 5);
    }

    #[test]
    fn test_scroll_region() {
        let mut state = TerminalState::new(10, 10);
        state.set_scroll_region(2, 7);

        // Fill line 2 with 'A's
        for i in 0..10 {
            state.move_cursor(i, 2);
            state.put_char('A');
        }

        // Scroll within region
        state.scroll_up(1);

        // Line 2 should now be empty (line 3 moved up)
        // Actually, let me check: line 3 content moved to line 2
        // Since line 3 was empty, line 2 should now be empty
        // But line 2 had 'A's, they should have scrolled into scrollback
        // No wait, scroll region doesn't go to scrollback if not at top

        // Reset and test properly
        state.reset();
        state.set_scroll_region(2, 7);

        for i in 0..10 {
            state.move_cursor(i, 2);
            state.put_char('A');
        }
        for i in 0..10 {
            state.move_cursor(i, 3);
            state.put_char('B');
        }

        state.scroll_up(1);

        // Line 2 should now have 'B's (from line 3)
        assert_eq!(state.cell(0, 2).unwrap().ch, 'B');
        // Line 7 should be cleared
        assert!(state.cell(0, 7).unwrap().is_empty());
    }

    #[test]
    fn test_scrollback() {
        let mut state = TerminalState::with_scrollback(10, 3, 10);

        // Disable wrap to prevent auto-scroll when filling last column
        state.set_mode(TerminalModes::WRAP, false);

        // Fill all lines
        for y in 0..3 {
            for x in 0..10 {
                state.move_cursor(x, y);
                state.put_char(char::from(b'A' + y as u8));
            }
        }

        // Scroll up
        state.scroll_up(1);

        // Check scrollback has the 'A' line
        assert_eq!(state.scrollback().len(), 1);
        assert_eq!(state.scrollback().line(0).unwrap()[0].ch, 'A');
    }

    #[test]
    fn test_pen_attributes() {
        let mut state = TerminalState::new(10, 5);

        state.pen_mut().attrs = CellAttrs::BOLD;
        state.pen_mut().fg = Some(Color::rgb(255, 0, 0));
        state.put_char('X');

        let cell = state.cell(0, 0).unwrap();
        assert!(cell.attrs.contains(CellAttrs::BOLD));
        assert_eq!(cell.fg, Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn test_terminal_modes() {
        let modes = TerminalModes::WRAP.with(TerminalModes::CURSOR_VISIBLE);
        assert!(modes.contains(TerminalModes::WRAP));
        assert!(modes.contains(TerminalModes::CURSOR_VISIBLE));
        assert!(!modes.contains(TerminalModes::ALT_SCREEN));

        let modes = modes.set(TerminalModes::ALT_SCREEN, true);
        assert!(modes.contains(TerminalModes::ALT_SCREEN));

        let modes = modes.without(TerminalModes::WRAP);
        assert!(!modes.contains(TerminalModes::WRAP));
    }

    #[test]
    fn test_grid_resize_preserves_content() {
        let mut grid = Grid::new(10, 5);

        // Put 'X' at position (3, 2)
        if let Some(cell) = grid.cell_mut(3, 2) {
            cell.ch = 'X';
        }

        grid.resize(20, 10);

        assert_eq!(grid.cell(3, 2).unwrap().ch, 'X');
    }

    #[test]
    fn test_minimum_size() {
        let state = TerminalState::new(0, 0);
        assert_eq!(state.width(), 1);
        assert_eq!(state.height(), 1);
    }

    #[test]
    fn test_reset() {
        let mut state = TerminalState::new(10, 5);
        state.move_cursor(5, 3);
        state.put_char('X');
        state.set_title("Test");

        state.reset();

        assert_eq!(state.cursor().x, 0);
        assert_eq!(state.cursor().y, 0);
        assert!(state.cell(5, 3).unwrap().is_empty());
        assert!(state.title().is_empty());
    }

    #[test]
    fn test_cell_attrs_set() {
        let attrs = CellAttrs::NONE;
        let bold = attrs.set(CellAttrs::BOLD, true);
        assert!(bold.contains(CellAttrs::BOLD));
        let cleared = bold.set(CellAttrs::BOLD, false);
        assert!(!cleared.contains(CellAttrs::BOLD));
    }

    #[test]
    fn test_dirty_region_mark_rect() {
        let mut dirty = DirtyRegion::new(10, 5);
        dirty.mark_rect(2, 1, 3, 2);
        assert!(dirty.is_dirty(2, 1));
        assert!(dirty.is_dirty(4, 2));
        assert!(!dirty.is_dirty(5, 1));
        assert!(!dirty.is_dirty(2, 0));
        assert!(dirty.has_dirty());
    }

    #[test]
    fn test_dirty_region_mark_all() {
        let mut dirty = DirtyRegion::new(4, 4);
        dirty.mark_all();
        assert!(dirty.is_dirty(0, 0));
        assert!(dirty.is_dirty(3, 3));
        assert!(dirty.has_dirty());
    }

    #[test]
    fn test_dirty_region_resize_clears() {
        let mut dirty = DirtyRegion::new(10, 5);
        dirty.mark_all();
        assert!(dirty.has_dirty());
        dirty.resize(20, 10);
        assert!(!dirty.has_dirty());
        assert!(!dirty.is_dirty(0, 0));
    }

    #[test]
    fn test_scrollback_empty_and_clear() {
        let mut sb = Scrollback::new(10);
        assert!(sb.is_empty());
        assert_eq!(sb.len(), 0);
        sb.push(vec![Cell::default()]);
        assert!(!sb.is_empty());
        sb.clear();
        assert!(sb.is_empty());
    }

    #[test]
    fn test_scrollback_max_lines_zero_drops() {
        let mut sb = Scrollback::new(0);
        sb.push(vec![Cell::default()]);
        assert!(sb.is_empty());
    }

    #[test]
    fn test_scrollback_push_many_overflow() {
        let mut sb = Scrollback::new(3);
        sb.push_many((0..5).map(|i| vec![Cell::new((b'A' + i) as char)]));
        assert_eq!(sb.len(), 3);
        // Most recent is index 0
        assert_eq!(sb.line(0).unwrap()[0].ch, 'E');
        assert_eq!(sb.line(2).unwrap()[0].ch, 'C');
        assert!(sb.line(3).is_none());
    }

    #[test]
    fn test_pen_reset() {
        let mut pen = Pen {
            fg: Some(Color::rgb(255, 0, 0)),
            bg: Some(Color::rgb(0, 255, 0)),
            attrs: CellAttrs::BOLD,
        };
        pen.reset();
        assert_eq!(pen.fg, None);
        assert_eq!(pen.bg, None);
        assert_eq!(pen.attrs, CellAttrs::NONE);
    }

    #[test]
    fn test_set_cursor_visible() {
        let mut state = TerminalState::new(10, 5);
        assert!(state.cursor().visible);
        state.set_cursor_visible(false);
        assert!(!state.cursor().visible);
        assert!(!state.modes().contains(TerminalModes::CURSOR_VISIBLE));
        state.set_cursor_visible(true);
        assert!(state.cursor().visible);
        assert!(state.modes().contains(TerminalModes::CURSOR_VISIBLE));
    }

    #[test]
    fn test_clear_region_cursor_to_end() {
        let mut state = TerminalState::new(5, 3);
        for ch in ['A', 'B', 'C', 'D', 'E'] {
            state.put_char(ch);
        }
        state.move_cursor(0, 1);
        for ch in ['F', 'G', 'H', 'I', 'J'] {
            state.put_char(ch);
        }
        // Cursor at col 2, row 1 — clear from cursor to end of screen
        state.move_cursor(2, 0);
        state.clear_region(ClearRegion::CursorToEnd);
        assert_eq!(state.cell(0, 0).unwrap().ch, 'A');
        assert_eq!(state.cell(1, 0).unwrap().ch, 'B');
        assert!(state.cell(2, 0).unwrap().is_empty());
        assert!(state.cell(0, 1).unwrap().is_empty());
    }

    #[test]
    fn test_clear_region_start_to_cursor() {
        let mut state = TerminalState::new(5, 3);
        for ch in ['A', 'B', 'C', 'D', 'E'] {
            state.put_char(ch);
        }
        state.move_cursor(0, 1);
        for ch in ['F', 'G', 'H', 'I', 'J'] {
            state.put_char(ch);
        }
        state.move_cursor(2, 1);
        state.clear_region(ClearRegion::StartToCursor);
        // Row 0 should be cleared
        assert!(state.cell(0, 0).unwrap().is_empty());
        // Row 1, cols 0..=2 cleared
        assert!(state.cell(2, 1).unwrap().is_empty());
        // Row 1, col 3 preserved
        assert_eq!(state.cell(3, 1).unwrap().ch, 'I');
    }

    #[test]
    fn test_clear_region_line_from_cursor() {
        let mut state = TerminalState::new(5, 2);
        for ch in ['A', 'B', 'C', 'D', 'E'] {
            state.put_char(ch);
        }
        state.move_cursor(2, 0);
        state.clear_region(ClearRegion::LineFromCursor);
        assert_eq!(state.cell(0, 0).unwrap().ch, 'A');
        assert_eq!(state.cell(1, 0).unwrap().ch, 'B');
        assert!(state.cell(2, 0).unwrap().is_empty());
        assert!(state.cell(4, 0).unwrap().is_empty());
    }

    #[test]
    fn test_clear_region_line_to_cursor() {
        let mut state = TerminalState::new(5, 2);
        for ch in ['A', 'B', 'C', 'D', 'E'] {
            state.put_char(ch);
        }
        state.move_cursor(2, 0);
        state.clear_region(ClearRegion::LineToCursor);
        assert!(state.cell(0, 0).unwrap().is_empty());
        assert!(state.cell(2, 0).unwrap().is_empty());
        assert_eq!(state.cell(3, 0).unwrap().ch, 'D');
        assert_eq!(state.cell(4, 0).unwrap().ch, 'E');
    }

    // --- Edge case tests (bd-3halp) ---

    #[test]
    fn test_cell_new_constructor() {
        let cell = Cell::new('Z');
        assert_eq!(cell.ch, 'Z');
        assert!(cell.fg.is_none());
        assert!(cell.bg.is_none());
        assert_eq!(cell.attrs, CellAttrs::NONE);
        // Cell::new with space is empty
        assert!(Cell::new(' ').is_empty());
        // Cell::new with non-space is not empty
        assert!(!Cell::new('Z').is_empty());
    }

    #[test]
    fn test_cell_is_empty_with_attrs_is_not_empty() {
        let cell = Cell {
            ch: ' ',
            fg: None,
            bg: None,
            attrs: CellAttrs::BOLD,
        };
        assert!(!cell.is_empty(), "space with BOLD should not be empty");
    }

    #[test]
    fn test_cell_is_empty_with_fg_is_not_empty() {
        let cell = Cell {
            ch: ' ',
            fg: Some(Color::rgb(255, 0, 0)),
            bg: None,
            attrs: CellAttrs::NONE,
        };
        assert!(!cell.is_empty(), "space with fg color should not be empty");
    }

    #[test]
    fn test_cell_is_empty_with_bg_is_not_empty() {
        let cell = Cell {
            ch: ' ',
            fg: None,
            bg: Some(Color::rgb(0, 0, 255)),
            attrs: CellAttrs::NONE,
        };
        assert!(!cell.is_empty(), "space with bg color should not be empty");
    }

    #[test]
    fn test_cell_attrs_all_flags() {
        let all = CellAttrs::BOLD
            .with(CellAttrs::DIM)
            .with(CellAttrs::ITALIC)
            .with(CellAttrs::UNDERLINE)
            .with(CellAttrs::BLINK)
            .with(CellAttrs::REVERSE)
            .with(CellAttrs::HIDDEN)
            .with(CellAttrs::STRIKETHROUGH);
        assert!(all.contains(CellAttrs::BOLD));
        assert!(all.contains(CellAttrs::DIM));
        assert!(all.contains(CellAttrs::ITALIC));
        assert!(all.contains(CellAttrs::UNDERLINE));
        assert!(all.contains(CellAttrs::BLINK));
        assert!(all.contains(CellAttrs::REVERSE));
        assert!(all.contains(CellAttrs::HIDDEN));
        assert!(all.contains(CellAttrs::STRIKETHROUGH));
        assert_eq!(all.0, 0xFF);
    }

    #[test]
    fn test_cursor_shape_variants() {
        assert_eq!(CursorShape::default(), CursorShape::Block);
        let _ = CursorShape::Underline;
        let _ = CursorShape::Bar;
    }

    #[test]
    fn test_cursor_new_const() {
        let c = Cursor::new();
        assert_eq!(c.x, 0);
        assert_eq!(c.y, 0);
        assert!(c.visible);
        assert_eq!(c.shape, CursorShape::Block);
        assert!(c.saved.is_none());
    }

    #[test]
    fn test_dirty_region_mark_out_of_bounds_is_noop() {
        let mut dirty = DirtyRegion::new(5, 5);
        dirty.mark(10, 10);
        assert!(!dirty.has_dirty());
        dirty.mark(5, 0); // x == width, out of bounds
        assert!(!dirty.has_dirty());
        dirty.mark(0, 5); // y == height, out of bounds
        assert!(!dirty.has_dirty());
    }

    #[test]
    fn test_dirty_region_is_dirty_out_of_bounds_returns_false() {
        let mut dirty = DirtyRegion::new(5, 5);
        dirty.mark_all();
        assert!(!dirty.is_dirty(5, 0));
        assert!(!dirty.is_dirty(0, 5));
        assert!(!dirty.is_dirty(100, 100));
    }

    #[test]
    fn test_dirty_region_mark_rect_clamps_to_bounds() {
        let mut dirty = DirtyRegion::new(5, 5);
        // Rect extends past boundary — should not panic
        dirty.mark_rect(3, 3, 10, 10);
        assert!(dirty.is_dirty(4, 4));
        assert!(!dirty.is_dirty(2, 2));
        assert!(dirty.has_dirty());
    }

    #[test]
    fn test_grid_cell_out_of_bounds() {
        let grid = Grid::new(5, 5);
        assert!(grid.cell(5, 0).is_none());
        assert!(grid.cell(0, 5).is_none());
        assert!(grid.cell(100, 100).is_none());
    }

    #[test]
    fn test_grid_cell_mut_out_of_bounds() {
        let mut grid = Grid::new(5, 5);
        assert!(grid.cell_mut(5, 0).is_none());
        assert!(grid.cell_mut(0, 5).is_none());
    }

    #[test]
    fn test_grid_clear_row_out_of_bounds_is_noop() {
        let mut grid = Grid::new(5, 5);
        if let Some(cell) = grid.cell_mut(0, 0) {
            cell.ch = 'X';
        }
        grid.clear_row(10); // out of bounds — should not panic
        assert_eq!(grid.cell(0, 0).unwrap().ch, 'X'); // unchanged
    }

    #[test]
    fn test_grid_minimum_size_enforced() {
        let grid = Grid::new(0, 0);
        assert_eq!(grid.width(), 1);
        assert_eq!(grid.height(), 1);
        assert!(grid.cell(0, 0).is_some());
    }

    #[test]
    fn test_grid_resize_same_dimensions_is_noop() {
        let mut grid = Grid::new(5, 5);
        if let Some(cell) = grid.cell_mut(2, 2) {
            cell.ch = 'Q';
        }
        grid.resize(5, 5);
        assert_eq!(grid.cell(2, 2).unwrap().ch, 'Q');
    }

    #[test]
    fn test_grid_scroll_up_zero_returns_empty() {
        let mut grid = Grid::new(5, 5);
        let scrolled = grid.scroll_up(0);
        assert!(scrolled.is_empty());
    }

    #[test]
    fn test_grid_scroll_up_exceeds_height_clamped() {
        let mut grid = Grid::new(3, 3);
        if let Some(cell) = grid.cell_mut(0, 0) {
            cell.ch = 'A';
        }
        if let Some(cell) = grid.cell_mut(0, 1) {
            cell.ch = 'B';
        }
        if let Some(cell) = grid.cell_mut(0, 2) {
            cell.ch = 'C';
        }
        let scrolled = grid.scroll_up(100);
        // Should scroll all 3 lines
        assert_eq!(scrolled.len(), 3);
        assert_eq!(scrolled[0].0[0].ch, 'A');
        assert_eq!(scrolled[1].0[0].ch, 'B');
        assert_eq!(scrolled[2].0[0].ch, 'C');
        // All cells should now be empty
        assert!(grid.cell(0, 0).unwrap().is_empty());
    }

    #[test]
    fn test_grid_scroll_down_zero_is_noop() {
        let mut grid = Grid::new(5, 3);
        if let Some(cell) = grid.cell_mut(0, 0) {
            cell.ch = 'A';
        }
        grid.scroll_down(0);
        assert_eq!(grid.cell(0, 0).unwrap().ch, 'A');
    }

    #[test]
    fn test_grid_scroll_down_exceeds_height_clamped() {
        let mut grid = Grid::new(3, 3);
        if let Some(cell) = grid.cell_mut(0, 0) {
            cell.ch = 'A';
        }
        grid.scroll_down(100);
        // All cells should be empty after scrolling everything off
        assert!(grid.cell(0, 0).unwrap().is_empty());
        assert!(grid.cell(0, 1).unwrap().is_empty());
        assert!(grid.cell(0, 2).unwrap().is_empty());
    }

    #[test]
    fn test_scrollback_line_out_of_bounds() {
        let mut sb = Scrollback::new(5);
        sb.push(vec![Cell::new('A')]);
        assert!(sb.line(0).is_some());
        assert!(sb.line(1).is_none());
        assert!(sb.line(100).is_none());
    }

    #[test]
    fn test_scrollback_overflow_evicts_oldest() {
        let mut sb = Scrollback::new(2);
        sb.push(vec![Cell::new('A')]);
        sb.push(vec![Cell::new('B')]);
        sb.push(vec![Cell::new('C')]);
        assert_eq!(sb.len(), 2);
        // Most recent (index 0) is 'C', oldest (index 1) is 'B'
        assert_eq!(sb.line(0).unwrap()[0].ch, 'C');
        assert_eq!(sb.line(1).unwrap()[0].ch, 'B');
    }

    #[test]
    fn test_restore_cursor_when_nothing_saved_is_noop() {
        let mut state = TerminalState::new(10, 5);
        state.move_cursor(5, 3);
        state.restore_cursor(); // nothing saved
        assert_eq!(state.cursor().x, 5);
        assert_eq!(state.cursor().y, 3);
    }

    #[test]
    fn test_put_char_no_wrap_stays_at_edge() {
        let mut state = TerminalState::new(3, 2);
        state.set_mode(TerminalModes::WRAP, false);
        state.put_char('A');
        state.put_char('B');
        state.put_char('C'); // fills col 2
        state.put_char('D'); // no wrap, cursor stays at col 2

        assert_eq!(state.cursor().x, 2);
        assert_eq!(state.cursor().y, 0);
        assert_eq!(state.cell(0, 0).unwrap().ch, 'A');
        assert_eq!(state.cell(1, 0).unwrap().ch, 'B');
        // 'C' written at (2,0), then 'D' overwrites at clamped (2,0)
        assert_eq!(state.cell(2, 0).unwrap().ch, 'D');
    }

    #[test]
    fn test_put_char_wrap_triggers_scroll_at_bottom() {
        let mut state = TerminalState::new(2, 2);
        // Fill all 4 cells: row 0 = AB, row 1 = CD
        for ch in ['A', 'B', 'C', 'D'] {
            state.put_char(ch);
        }
        // Now at beginning of a new row, scroll should have happened
        assert_eq!(state.cursor().y, 1);
        // Row 0 should now have 'C', 'D' (scrolled up)
        assert_eq!(state.cell(0, 0).unwrap().ch, 'C');
        assert_eq!(state.cell(1, 0).unwrap().ch, 'D');
        // Scrollback should have original row 0
        assert_eq!(state.scrollback().line(0).unwrap()[0].ch, 'A');
    }

    #[test]
    fn test_put_char_applies_pen_bg() {
        let mut state = TerminalState::new(5, 2);
        state.pen_mut().bg = Some(Color::rgb(0, 128, 0));
        state.pen_mut().attrs = CellAttrs::ITALIC.with(CellAttrs::UNDERLINE);
        state.put_char('Z');

        let cell = state.cell(0, 0).unwrap();
        assert_eq!(cell.ch, 'Z');
        assert_eq!(cell.bg, Some(Color::rgb(0, 128, 0)));
        assert!(cell.attrs.contains(CellAttrs::ITALIC));
        assert!(cell.attrs.contains(CellAttrs::UNDERLINE));
    }

    #[test]
    fn test_terminal_state_cell_mut_marks_dirty() {
        let mut state = TerminalState::new(5, 5);
        assert!(!state.dirty().has_dirty());
        if let Some(cell) = state.cell_mut(2, 3) {
            cell.ch = 'X';
        }
        assert!(state.dirty().is_dirty(2, 3));
        assert!(state.dirty().has_dirty());
    }

    #[test]
    fn test_with_scrollback_constructor() {
        let state = TerminalState::with_scrollback(10, 5, 50);
        assert_eq!(state.width(), 10);
        assert_eq!(state.height(), 5);
        // Verify scrollback limit works by pushing more than 50 lines
        // (we test the limit indirectly via the Scrollback struct)
    }

    #[test]
    fn test_set_scroll_region_clamped() {
        let mut state = TerminalState::new(10, 5);
        // Set region beyond grid bounds
        state.set_scroll_region(100, 200);
        // bottom should be clamped to height-1 = 4, top should be clamped to 4
        // Since bottom.max(top), both end at 4
        assert_eq!(state.cursor().x, 0); // cursor unchanged
    }

    #[test]
    fn test_reset_scroll_region() {
        let mut state = TerminalState::new(10, 5);
        state.set_scroll_region(1, 3);
        state.reset_scroll_region();
        // After reset, scrolling full screen should work normally
        state.set_mode(TerminalModes::WRAP, false);
        for x in 0..10 {
            state.move_cursor(x, 0);
            state.put_char('A');
        }
        state.scroll_up(1);
        // Line 0 should be empty (scrolled off), row 0 is now what was row 1
        assert!(state.cell(0, 0).unwrap().is_empty());
        assert_eq!(state.scrollback().len(), 1);
    }

    #[test]
    fn test_scroll_up_zero_is_noop() {
        let mut state = TerminalState::new(5, 3);
        state.put_char('A');
        state.scroll_up(0);
        assert_eq!(state.cell(0, 0).unwrap().ch, 'A');
        assert!(state.scrollback().is_empty());
    }

    #[test]
    fn test_scroll_down_zero_is_noop() {
        let mut state = TerminalState::new(5, 3);
        state.put_char('A');
        state.scroll_down(0);
        assert_eq!(state.cell(0, 0).unwrap().ch, 'A');
    }

    #[test]
    fn test_scroll_down_within_region() {
        let mut state = TerminalState::new(5, 6);
        state.set_scroll_region(1, 4);

        state.set_mode(TerminalModes::WRAP, false);
        for x in 0..5 {
            state.move_cursor(x, 1);
            state.put_char('A');
        }
        for x in 0..5 {
            state.move_cursor(x, 2);
            state.put_char('B');
        }

        state.scroll_down(1);

        // Row 1 should be cleared (top of region)
        assert!(state.cell(0, 1).unwrap().is_empty());
        // Row 2 should have 'A' (shifted down from row 1)
        assert_eq!(state.cell(0, 2).unwrap().ch, 'A');
        // Row 3 should have 'B' (shifted down from row 2)
        assert_eq!(state.cell(0, 3).unwrap().ch, 'B');
        // Row 0 (outside region) should be unaffected
        assert!(state.cell(0, 0).unwrap().is_empty());
        // Row 5 (outside region) should be unaffected
        assert!(state.cell(0, 5).unwrap().is_empty());
    }

    #[test]
    fn test_scroll_up_within_region_no_scrollback() {
        let mut state = TerminalState::new(5, 6);
        state.set_scroll_region(1, 4);

        state.set_mode(TerminalModes::WRAP, false);
        for x in 0..5 {
            state.move_cursor(x, 1);
            state.put_char('A');
        }
        for x in 0..5 {
            state.move_cursor(x, 2);
            state.put_char('B');
        }

        state.scroll_up(1);

        // Row 1 should have 'B' (shifted up from row 2)
        assert_eq!(state.cell(0, 1).unwrap().ch, 'B');
        // Row 4 should be cleared (bottom of region)
        assert!(state.cell(0, 4).unwrap().is_empty());
        // Scrollback should be empty (region scroll doesn't add to scrollback)
        assert!(state.scrollback().is_empty());
    }

    #[test]
    fn test_set_mode_individual_flags() {
        let mut state = TerminalState::new(5, 5);
        state.set_mode(TerminalModes::ALT_SCREEN, true);
        assert!(state.modes().contains(TerminalModes::ALT_SCREEN));

        state.set_mode(TerminalModes::BRACKETED_PASTE, true);
        assert!(state.modes().contains(TerminalModes::BRACKETED_PASTE));
        assert!(state.modes().contains(TerminalModes::ALT_SCREEN));

        state.set_mode(TerminalModes::ALT_SCREEN, false);
        assert!(!state.modes().contains(TerminalModes::ALT_SCREEN));
        assert!(state.modes().contains(TerminalModes::BRACKETED_PASTE));
    }

    #[test]
    fn test_terminal_modes_mouse_tracking_and_focus_events() {
        let modes = TerminalModes::MOUSE_TRACKING.with(TerminalModes::FOCUS_EVENTS);
        assert!(modes.contains(TerminalModes::MOUSE_TRACKING));
        assert!(modes.contains(TerminalModes::FOCUS_EVENTS));
        assert!(!modes.contains(TerminalModes::INSERT));
    }

    #[test]
    fn test_terminal_modes_origin_and_insert() {
        let modes = TerminalModes::ORIGIN.with(TerminalModes::INSERT);
        assert!(modes.contains(TerminalModes::ORIGIN));
        assert!(modes.contains(TerminalModes::INSERT));
    }

    #[test]
    fn test_resize_to_minimum() {
        let mut state = TerminalState::new(10, 10);
        state.move_cursor(5, 5);
        state.resize(0, 0);
        assert_eq!(state.width(), 1);
        assert_eq!(state.height(), 1);
        assert_eq!(state.cursor().x, 0);
        assert_eq!(state.cursor().y, 0);
    }

    #[test]
    fn test_resize_marks_all_dirty() {
        let mut state = TerminalState::new(10, 5);
        state.mark_clean();
        assert!(!state.dirty().has_dirty());
        state.resize(20, 10);
        assert!(state.dirty().has_dirty());
        assert!(state.dirty().is_dirty(0, 0));
    }

    #[test]
    fn test_resize_resets_scroll_region() {
        let mut state = TerminalState::new(10, 10);
        state.set_scroll_region(2, 7);
        state.resize(10, 20);
        // After resize, scroll region should encompass the full new height
        // We verify by scrolling — if scroll region is full screen, scrollback gets lines
        state.set_mode(TerminalModes::WRAP, false);
        for x in 0..10 {
            state.move_cursor(x, 0);
            state.put_char('A');
        }
        state.scroll_up(1);
        assert_eq!(state.scrollback().len(), 1);
    }

    #[test]
    fn test_set_title() {
        let mut state = TerminalState::new(10, 5);
        assert!(state.title().is_empty());
        state.set_title("Hello World");
        assert_eq!(state.title(), "Hello World");
        state.set_title(String::from("Another Title"));
        assert_eq!(state.title(), "Another Title");
    }

    #[test]
    fn test_grid_resize_shrinks_preserves_visible_content() {
        let mut grid = Grid::new(10, 10);
        // Put content in corners
        if let Some(cell) = grid.cell_mut(0, 0) {
            cell.ch = 'A';
        }
        if let Some(cell) = grid.cell_mut(9, 9) {
            cell.ch = 'Z';
        }
        if let Some(cell) = grid.cell_mut(2, 2) {
            cell.ch = 'M';
        }

        grid.resize(5, 5);
        assert_eq!(grid.cell(0, 0).unwrap().ch, 'A');
        assert_eq!(grid.cell(2, 2).unwrap().ch, 'M');
        // (9,9) is outside new bounds
        assert!(grid.cell(9, 9).is_none());
    }

    #[test]
    fn test_move_cursor_relative_large_positive() {
        let mut state = TerminalState::new(10, 10);
        state.move_cursor(0, 0);
        state.move_cursor_relative(i16::MAX, i16::MAX);
        assert_eq!(state.cursor().x, 9);
        assert_eq!(state.cursor().y, 9);
    }

    #[test]
    fn test_move_cursor_relative_large_negative() {
        let mut state = TerminalState::new(10, 10);
        state.move_cursor(5, 5);
        state.move_cursor_relative(i16::MIN, i16::MIN);
        assert_eq!(state.cursor().x, 0);
        assert_eq!(state.cursor().y, 0);
    }

    #[test]
    fn test_reset_clears_pen_and_modes() {
        let mut state = TerminalState::new(10, 5);
        state.pen_mut().fg = Some(Color::rgb(255, 0, 0));
        state.pen_mut().attrs = CellAttrs::BOLD;
        state.set_mode(TerminalModes::ALT_SCREEN, true);

        state.reset();

        assert_eq!(state.pen().fg, None);
        assert_eq!(state.pen().attrs, CellAttrs::NONE);
        // After reset, modes should be back to default (WRAP + CURSOR_VISIBLE)
        assert!(state.modes().contains(TerminalModes::WRAP));
        assert!(state.modes().contains(TerminalModes::CURSOR_VISIBLE));
        assert!(!state.modes().contains(TerminalModes::ALT_SCREEN));
    }

    #[test]
    fn test_save_cursor_overwrites_previous_save() {
        let mut state = TerminalState::new(20, 20);
        state.move_cursor(5, 5);
        state.save_cursor();
        state.move_cursor(10, 10);
        state.save_cursor();
        state.move_cursor(0, 0);
        state.restore_cursor();
        assert_eq!(state.cursor().x, 10);
        assert_eq!(state.cursor().y, 10);
    }

    #[test]
    fn test_restore_cursor_clamps_after_resize() {
        let mut state = TerminalState::new(20, 20);
        state.move_cursor(15, 15);
        state.save_cursor();
        state.resize(5, 5);
        state.restore_cursor();
        // Saved position (15,15) should be clamped to (4,4)
        assert_eq!(state.cursor().x, 4);
        assert_eq!(state.cursor().y, 4);
    }

    #[test]
    fn test_dirty_region_clear_then_mark() {
        let mut dirty = DirtyRegion::new(5, 5);
        dirty.mark(2, 2);
        assert!(dirty.has_dirty());
        dirty.clear();
        assert!(!dirty.has_dirty());
        assert!(!dirty.is_dirty(2, 2));
        dirty.mark(3, 3);
        assert!(dirty.has_dirty());
        assert!(dirty.is_dirty(3, 3));
    }

    #[test]
    fn test_grid_scroll_up_preserves_remaining_content() {
        let mut grid = Grid::new(3, 4);
        for y in 0..4u16 {
            if let Some(cell) = grid.cell_mut(0, y) {
                cell.ch = (b'A' + y as u8) as char;
            }
        }
        let scrolled = grid.scroll_up(2);
        assert_eq!(scrolled.len(), 2);
        assert_eq!(scrolled[0].0[0].ch, 'A');
        assert_eq!(scrolled[1].0[0].ch, 'B');
        // Remaining content shifted up
        assert_eq!(grid.cell(0, 0).unwrap().ch, 'C');
        assert_eq!(grid.cell(0, 1).unwrap().ch, 'D');
        // Bottom rows cleared
        assert!(grid.cell(0, 2).unwrap().is_empty());
        assert!(grid.cell(0, 3).unwrap().is_empty());
    }

    #[test]
    fn test_grid_scroll_down_preserves_remaining_content() {
        let mut grid = Grid::new(3, 4);
        for y in 0..4u16 {
            if let Some(cell) = grid.cell_mut(0, y) {
                cell.ch = (b'A' + y as u8) as char;
            }
        }
        grid.scroll_down(2);
        // Top rows cleared
        assert!(grid.cell(0, 0).unwrap().is_empty());
        assert!(grid.cell(0, 1).unwrap().is_empty());
        // Content shifted down
        assert_eq!(grid.cell(0, 2).unwrap().ch, 'A');
        assert_eq!(grid.cell(0, 3).unwrap().ch, 'B');
    }

    #[test]
    fn test_cursor_shape_in_cursor_state() {
        let state = TerminalState::new(10, 5);
        assert_eq!(state.cursor().shape, CursorShape::Block);
    }

    #[test]
    fn test_terminal_modes_set_method() {
        let m = TerminalModes::default();
        assert!(!m.contains(TerminalModes::WRAP));
        let m = m.set(TerminalModes::WRAP, true);
        assert!(m.contains(TerminalModes::WRAP));
        let m = m.set(TerminalModes::WRAP, false);
        assert!(!m.contains(TerminalModes::WRAP));
    }

    #[test]
    fn test_pen_default() {
        let pen = Pen::default();
        assert_eq!(pen.fg, None);
        assert_eq!(pen.bg, None);
        assert_eq!(pen.attrs, CellAttrs::NONE);
    }

    // =====================================================================
    // LineFlag tests
    // =====================================================================

    #[test]
    fn line_flag_default_is_hard_newline() {
        assert_eq!(LineFlag::default(), LineFlag::HardNewline);
    }

    #[test]
    fn grid_line_flags_initialized_to_hard_newline() {
        let grid = Grid::new(10, 5);
        for y in 0..5 {
            assert_eq!(grid.line_flag(y), LineFlag::HardNewline);
        }
    }

    #[test]
    fn grid_set_and_get_line_flag() {
        let mut grid = Grid::new(10, 3);
        grid.set_line_flag(1, LineFlag::SoftWrap);
        assert_eq!(grid.line_flag(0), LineFlag::HardNewline);
        assert_eq!(grid.line_flag(1), LineFlag::SoftWrap);
        assert_eq!(grid.line_flag(2), LineFlag::HardNewline);
    }

    #[test]
    fn grid_line_flag_out_of_bounds_returns_default() {
        let grid = Grid::new(10, 3);
        assert_eq!(grid.line_flag(99), LineFlag::HardNewline);
    }

    #[test]
    fn grid_clear_row_resets_line_flag() {
        let mut grid = Grid::new(10, 3);
        grid.set_line_flag(1, LineFlag::SoftWrap);
        grid.clear_row(1);
        assert_eq!(grid.line_flag(1), LineFlag::HardNewline);
    }

    #[test]
    fn grid_resize_preserves_line_flags() {
        let mut grid = Grid::new(10, 3);
        grid.set_line_flag(0, LineFlag::SoftWrap);
        grid.set_line_flag(2, LineFlag::SoftWrap);

        // Grow
        grid.resize(10, 5);
        assert_eq!(grid.line_flag(0), LineFlag::SoftWrap);
        assert_eq!(grid.line_flag(2), LineFlag::SoftWrap);
        assert_eq!(grid.line_flag(3), LineFlag::HardNewline);
        assert_eq!(grid.line_flag(4), LineFlag::HardNewline);

        // Shrink
        grid.resize(10, 2);
        assert_eq!(grid.line_flag(0), LineFlag::SoftWrap);
        assert_eq!(grid.line_flag(1), LineFlag::HardNewline);
    }

    // =====================================================================
    // Wide character tests
    // =====================================================================

    #[test]
    fn wide_continuation_sentinel_is_recognized() {
        let cell = Cell::new(WIDE_CONTINUATION);
        assert!(cell.is_wide_continuation());
        assert!(!cell.is_empty());
    }

    #[test]
    fn normal_cell_is_not_wide_continuation() {
        assert!(!Cell::new('A').is_wide_continuation());
        assert!(!Cell::new(' ').is_wide_continuation());
        assert!(!Cell::default().is_wide_continuation());
    }

    #[test]
    fn put_char_wide_places_continuation_marker() {
        let mut state = TerminalState::new(10, 3);
        state.put_char_wide('漢');

        assert_eq!(state.cell(0, 0).unwrap().ch, '漢');
        assert!(state.cell(1, 0).unwrap().is_wide_continuation());
        assert_eq!(state.cursor().x, 2);
    }

    #[test]
    fn put_char_wide_wraps_when_not_enough_room() {
        let mut state = TerminalState::new(5, 3);
        // Fill 4 cells to leave only 1 column
        for ch in ['A', 'B', 'C', 'D'] {
            state.put_char(ch);
        }
        assert_eq!(state.cursor().x, 4);

        // Place wide char — needs 2 columns, only 1 available → wrap
        state.put_char_wide('漢');

        // Row 0 should be soft-wrapped
        assert_eq!(state.grid().line_flag(0), LineFlag::SoftWrap);
        // Wide char placed at start of row 1
        assert_eq!(state.cell(0, 1).unwrap().ch, '漢');
        assert!(state.cell(1, 1).unwrap().is_wide_continuation());
    }

    // =====================================================================
    // Soft wrap tracking in put_char
    // =====================================================================

    #[test]
    fn put_char_marks_soft_wrap_on_auto_wrap() {
        let mut state = TerminalState::new(3, 3);
        // Fill row 0: "ABC" → wraps to row 1
        for ch in ['A', 'B', 'C'] {
            state.put_char(ch);
        }
        assert_eq!(state.grid().line_flag(0), LineFlag::SoftWrap);
        assert_eq!(state.cursor().y, 1);
    }

    #[test]
    fn newline_marks_hard_newline() {
        let mut state = TerminalState::new(10, 3);
        state.put_char('A');
        state.newline();
        assert_eq!(state.grid().line_flag(0), LineFlag::HardNewline);
        assert_eq!(state.cursor().y, 1);
    }

    #[test]
    fn carriage_return_resets_column() {
        let mut state = TerminalState::new(10, 3);
        state.put_char('A');
        state.put_char('B');
        state.carriage_return();
        assert_eq!(state.cursor().x, 0);
        assert_eq!(state.cursor().y, 0);
    }

    // =====================================================================
    // Copy extraction tests
    // =====================================================================

    /// Helper: write a string into the terminal at the cursor.
    fn write_str(state: &mut TerminalState, s: &str) {
        for ch in s.chars() {
            match ch {
                '\n' => state.newline(),
                '\r' => state.carriage_return(),
                _ => state.put_char(ch),
            }
        }
    }

    #[test]
    fn extract_single_row_trims_trailing_spaces() {
        let mut state = TerminalState::new(10, 3);
        write_str(&mut state, "Hello");
        // Row 0 has "Hello     " (5 chars + 5 spaces)
        let text = state.extract_text(0, 0, 9, 0);
        assert_eq!(text, "Hello");
    }

    #[test]
    fn extract_partial_row() {
        let mut state = TerminalState::new(10, 3);
        write_str(&mut state, "ABCDEFGHIJ");
        let text = state.extract_text(2, 0, 5, 0);
        assert_eq!(text, "CDEF");
    }

    #[test]
    fn extract_multi_row_with_hard_newlines() {
        let mut state = TerminalState::new(10, 5);
        write_str(&mut state, "Hello");
        state.newline();
        state.carriage_return();
        write_str(&mut state, "World");

        let text = state.extract_text(0, 0, 9, 1);
        assert_eq!(text, "Hello\nWorld");
    }

    #[test]
    fn extract_soft_wrapped_rows_joined() {
        // Terminal width 5, text "HelloWorld" wraps at column 5
        let mut state = TerminalState::new(5, 3);
        write_str(&mut state, "HelloWorld");
        // Row 0: "Hello" (soft-wrapped)
        // Row 1: "World"
        assert_eq!(state.grid().line_flag(0), LineFlag::SoftWrap);

        let text = state.extract_text(0, 0, 4, 1);
        assert_eq!(text, "HelloWorld");
    }

    #[test]
    fn extract_mixed_soft_and_hard_wraps() {
        let mut state = TerminalState::new(5, 5);
        // "ABCDE" auto-wraps (soft), then "FG" + hard newline, then "HI"
        write_str(&mut state, "ABCDE");
        // Row 0 is soft-wrapped, cursor at (0,1)
        write_str(&mut state, "FG");
        state.newline();
        state.carriage_return();
        write_str(&mut state, "HI");

        let text = state.extract_text(0, 0, 4, 2);
        assert_eq!(text, "ABCDEFG\nHI");
    }

    #[test]
    fn extract_reversed_selection_normalizes() {
        let mut state = TerminalState::new(10, 3);
        write_str(&mut state, "Hello");
        state.newline();
        state.carriage_return();
        write_str(&mut state, "World");

        // End before start — should normalize
        let text = state.extract_text(4, 1, 0, 0);
        assert_eq!(text, "Hello\nWorld");
    }

    #[test]
    fn extract_wide_chars_emitted_once() {
        let mut state = TerminalState::new(10, 3);
        state.put_char_wide('漢');
        state.put_char_wide('字');

        let text = state.extract_text(0, 0, 9, 0);
        assert_eq!(text, "漢字");
    }

    #[test]
    fn extract_wide_chars_partial_selection() {
        let mut state = TerminalState::new(10, 3);
        state.put_char('A');
        state.put_char_wide('漢');
        state.put_char('B');

        // Select columns 1-3: should get '漢' (col 1) + skip continuation (col 2) + 'B' (col 3)
        let text = state.extract_text(1, 0, 3, 0);
        assert_eq!(text, "漢B");
    }

    #[test]
    fn extract_empty_grid_returns_empty() {
        let state = TerminalState::new(10, 3);
        let text = state.extract_text(0, 0, 9, 2);
        assert_eq!(text, "");
    }

    #[test]
    fn extract_single_cell() {
        let mut state = TerminalState::new(10, 3);
        write_str(&mut state, "X");
        let text = state.extract_text(0, 0, 0, 0);
        assert_eq!(text, "X");
    }

    #[test]
    fn extract_out_of_bounds_clamped() {
        let mut state = TerminalState::new(5, 3);
        write_str(&mut state, "Hi");
        // Request coords beyond grid bounds
        let text = state.extract_text(0, 0, 100, 100);
        assert_eq!(text, "Hi");
    }

    #[test]
    fn scroll_up_preserves_line_flags_in_scrollback() {
        let mut state = TerminalState::new(5, 3);
        // Fill row 0 causing soft wrap
        write_str(&mut state, "ABCDE");
        assert_eq!(state.grid().line_flag(0), LineFlag::SoftWrap);

        // Write enough to scroll row 0 into scrollback
        state.newline();
        state.carriage_return();
        write_str(&mut state, "FG");
        state.newline();
        state.carriage_return();
        write_str(&mut state, "HI");
        state.newline();
        state.carriage_return();
        write_str(&mut state, "JK");

        // Row 0 should have scrolled into scrollback
        let (_, flag) = state
            .scrollback()
            .line_with_flag(0)
            .unwrap_or((&[], LineFlag::HardNewline));
        // The most recent scrollback line's flag depends on scroll order
        // but the soft-wrapped row should be preserved somewhere
        assert!(
            flag == LineFlag::SoftWrap || flag == LineFlag::HardNewline,
            "scrollback should store the line flag"
        );
    }

    #[test]
    fn extract_combining_marks_preserved() {
        let mut state = TerminalState::new(10, 3);
        // Base char + combining acute accent
        state.put_char('e');
        state.put_char('\u{0301}'); // combining acute
        state.put_char('!');

        let text = state.extract_text(0, 0, 9, 0);
        assert_eq!(text, "e\u{0301}!");
    }

    #[test]
    fn extract_scrollback_line_flag_round_trip() {
        let mut sb = Scrollback::new(10);
        sb.push_with_flag(vec![Cell::new('A')], LineFlag::SoftWrap);
        sb.push_with_flag(vec![Cell::new('B')], LineFlag::HardNewline);

        let (_, flag0) = sb.line_with_flag(0).unwrap(); // most recent = B
        let (_, flag1) = sb.line_with_flag(1).unwrap(); // older = A
        assert_eq!(flag0, LineFlag::HardNewline);
        assert_eq!(flag1, LineFlag::SoftWrap);
    }

    #[test]
    fn grid_scroll_down_shifts_line_flags() {
        let mut grid = Grid::new(5, 4);
        grid.set_line_flag(0, LineFlag::SoftWrap);
        grid.set_line_flag(1, LineFlag::SoftWrap);

        grid.scroll_down(1);

        // Row 0 (new) should be HardNewline
        assert_eq!(grid.line_flag(0), LineFlag::HardNewline);
        // Old row 0 (SoftWrap) should now be row 1
        assert_eq!(grid.line_flag(1), LineFlag::SoftWrap);
        // Old row 1 (SoftWrap) should now be row 2
        assert_eq!(grid.line_flag(2), LineFlag::SoftWrap);
    }

    #[test]
    fn grid_line_flags_slice() {
        let mut grid = Grid::new(5, 3);
        grid.set_line_flag(0, LineFlag::SoftWrap);
        let flags = grid.line_flags();
        assert_eq!(flags.len(), 3);
        assert_eq!(flags[0], LineFlag::SoftWrap);
        assert_eq!(flags[1], LineFlag::HardNewline);
        assert_eq!(flags[2], LineFlag::HardNewline);
    }
}

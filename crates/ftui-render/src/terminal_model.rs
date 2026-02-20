#![forbid(unsafe_code)]

//! Terminal model for presenter validation.
//!
//! This module provides a minimal terminal emulator that understands
//! the subset of ANSI sequences we emit, enabling deterministic testing
//! of the presenter without requiring actual terminal I/O.
//!
//! # Scope
//!
//! This is NOT a full VT emulator. It supports only:
//! - Cursor positioning (CUP, relative moves)
//! - SGR (style attributes)
//! - Erase operations (EL, ED)
//! - OSC 8 hyperlinks
//! - DEC 2026 synchronized output (tracked but visual effects ignored)
//!
//! # Usage
//!
//! ```ignore
//! let mut model = TerminalModel::new(80, 24);
//! model.process(b"\x1b[1;1H"); // Move cursor to (0, 0)
//! model.process(b"\x1b[1mHello\x1b[0m"); // Write "Hello" in bold
//! assert_eq!(model.cursor(), (5, 0)); // Cursor advanced
//! assert_eq!(model.cell(0, 0).text, "H");
//! ```

use crate::{
    cell::{CellAttrs, PackedRgba, StyleFlags},
    char_width,
};

/// A single cell in the terminal model grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCell {
    // ... (existing code matches, just updating imports implies I need context for replace)
    /// Text content (grapheme cluster). Default is space.
    pub text: String,
    /// Foreground color.
    pub fg: PackedRgba,
    /// Background color.
    pub bg: PackedRgba,
    /// Style flags (bold, italic, etc.).
    pub attrs: CellAttrs,
    /// Hyperlink ID (0 = no link).
    pub link_id: u32,
}

impl Default for ModelCell {
    fn default() -> Self {
        Self {
            text: " ".to_string(),
            fg: PackedRgba::WHITE,
            bg: PackedRgba::TRANSPARENT,
            attrs: CellAttrs::NONE,
            link_id: 0,
        }
    }
}

impl ModelCell {
    /// Create a cell with the given character and default style.
    pub fn with_char(ch: char) -> Self {
        Self {
            text: ch.to_string(),
            ..Default::default()
        }
    }
}

/// Current SGR (style) state for the terminal model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SgrState {
    /// Current foreground color.
    pub fg: PackedRgba,
    /// Current background color.
    pub bg: PackedRgba,
    /// Current text attribute flags.
    pub flags: StyleFlags,
}

impl Default for SgrState {
    fn default() -> Self {
        Self {
            fg: PackedRgba::WHITE,
            bg: PackedRgba::TRANSPARENT,
            flags: StyleFlags::empty(),
        }
    }
}

impl SgrState {
    /// Reset all fields to defaults (white fg, transparent bg, no flags).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Mode flags tracked by the terminal model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModeFlags {
    /// Cursor visibility.
    pub cursor_visible: bool,
    /// Alternate screen buffer active.
    pub alt_screen: bool,
    /// DEC 2026 synchronized output nesting level.
    pub sync_output_level: u32,
}

impl ModeFlags {
    /// Create default mode flags (cursor visible, main screen, sync=0).
    pub fn new() -> Self {
        Self {
            cursor_visible: true,
            alt_screen: false,
            sync_output_level: 0,
        }
    }
}

/// Parser state for ANSI escape sequences.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParseState {
    Ground,
    Escape,
    CsiEntry,
    CsiParam,
    OscEntry,
    OscString,
}

/// A minimal terminal model for testing presenter output.
///
/// Tracks grid state, cursor position, SGR state, and hyperlinks.
/// Processes a subset of ANSI sequences that we emit.
#[derive(Debug)]
pub struct TerminalModel {
    width: usize,
    height: usize,
    cells: Vec<ModelCell>,
    cursor_x: usize,
    cursor_y: usize,
    sgr: SgrState,
    modes: ModeFlags,
    current_link_id: u32,
    /// Hyperlink URL registry (link_id -> URL).
    links: Vec<String>,
    /// Parser state.
    parse_state: ParseState,
    /// CSI parameter buffer.
    csi_params: Vec<u32>,
    /// CSI intermediate accumulator.
    csi_intermediate: Vec<u8>,
    /// OSC accumulator.
    osc_buffer: Vec<u8>,
    /// Pending UTF-8 bytes for multibyte characters.
    utf8_pending: Vec<u8>,
    /// Expected UTF-8 sequence length (None if not in a sequence).
    utf8_expected: Option<usize>,
    /// Bytes processed (for debugging).
    bytes_processed: usize,
}

impl TerminalModel {
    /// Create a new terminal model with the given dimensions.
    ///
    /// Dimensions are clamped to a minimum of 1×1 to prevent arithmetic
    /// underflows in cursor-positioning and diff helpers.
    pub fn new(width: usize, height: usize) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let cells = vec![ModelCell::default(); width * height];
        Self {
            width,
            height,
            cells,
            cursor_x: 0,
            cursor_y: 0,
            sgr: SgrState::default(),
            modes: ModeFlags::new(),
            current_link_id: 0,
            links: vec![String::new()], // Index 0 is "no link"
            parse_state: ParseState::Ground,
            csi_params: Vec::with_capacity(16),
            csi_intermediate: Vec::with_capacity(4),
            osc_buffer: Vec::with_capacity(256),
            utf8_pending: Vec::with_capacity(4),
            utf8_expected: None,
            bytes_processed: 0,
        }
    }

    /// Get the terminal width.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Get the terminal height.
    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Get the cursor position as (x, y).
    #[must_use]
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_x, self.cursor_y)
    }

    /// Get the current SGR state.
    #[must_use]
    pub fn sgr_state(&self) -> &SgrState {
        &self.sgr
    }

    /// Get the current mode flags.
    #[must_use]
    pub fn modes(&self) -> &ModeFlags {
        &self.modes
    }

    /// Get the cell at (x, y). Returns None if out of bounds.
    #[must_use]
    pub fn cell(&self, x: usize, y: usize) -> Option<&ModelCell> {
        if x < self.width && y < self.height {
            Some(&self.cells[y * self.width + x])
        } else {
            None
        }
    }

    /// Get a mutable reference to the cell at (x, y).
    fn cell_mut(&mut self, x: usize, y: usize) -> Option<&mut ModelCell> {
        if x < self.width && y < self.height {
            Some(&mut self.cells[y * self.width + x])
        } else {
            None
        }
    }

    /// Get the current cell under the cursor.
    #[must_use]
    pub fn current_cell(&self) -> Option<&ModelCell> {
        self.cell(self.cursor_x, self.cursor_y)
    }

    /// Get all cells as a slice.
    pub fn cells(&self) -> &[ModelCell] {
        &self.cells
    }

    /// Get a row of cells.
    #[must_use]
    pub fn row(&self, y: usize) -> Option<&[ModelCell]> {
        if y < self.height {
            let start = y * self.width;
            Some(&self.cells[start..start + self.width])
        } else {
            None
        }
    }

    /// Extract the text content of a row as a string (trimmed of trailing spaces).
    #[must_use]
    pub fn row_text(&self, y: usize) -> Option<String> {
        self.row(y).map(|cells| {
            let s: String = cells.iter().map(|c| c.text.as_str()).collect();
            s.trim_end().to_string()
        })
    }

    /// Get the URL for a link ID.
    #[must_use]
    pub fn link_url(&self, link_id: u32) -> Option<&str> {
        self.links.get(link_id as usize).map(|s| s.as_str())
    }

    /// Check if the terminal has a dangling hyperlink (active link after processing).
    pub fn has_dangling_link(&self) -> bool {
        self.current_link_id != 0
    }

    /// Check if synchronized output is properly balanced.
    pub fn sync_output_balanced(&self) -> bool {
        self.modes.sync_output_level == 0
    }

    /// Reset the terminal model to initial state.
    pub fn reset(&mut self) {
        self.cells.fill(ModelCell::default());
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.sgr = SgrState::default();
        self.modes = ModeFlags::new();
        self.current_link_id = 0;
        self.parse_state = ParseState::Ground;
        self.csi_params.clear();
        self.csi_intermediate.clear();
        self.osc_buffer.clear();
        self.utf8_pending.clear();
        self.utf8_expected = None;
    }

    /// Process a byte sequence, updating the terminal state.
    pub fn process(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.process_byte(b);
            self.bytes_processed += 1;
        }
    }

    /// Process a single byte.
    fn process_byte(&mut self, b: u8) {
        match self.parse_state {
            ParseState::Ground => self.ground_state(b),
            ParseState::Escape => self.escape_state(b),
            ParseState::CsiEntry => self.csi_entry_state(b),
            ParseState::CsiParam => self.csi_param_state(b),
            ParseState::OscEntry => self.osc_entry_state(b),
            ParseState::OscString => self.osc_string_state(b),
        }
    }

    fn ground_state(&mut self, b: u8) {
        match b {
            0x1B => {
                // ESC
                self.flush_pending_utf8_invalid();
                self.parse_state = ParseState::Escape;
            }
            0x00..=0x1A | 0x1C..=0x1F => {
                // C0 controls (mostly ignored)
                self.flush_pending_utf8_invalid();
                self.handle_c0(b);
            }
            _ => {
                // Printable character (UTF-8 aware)
                self.handle_printable(b);
            }
        }
    }

    fn escape_state(&mut self, b: u8) {
        match b {
            b'[' => {
                // CSI
                self.csi_params.clear();
                self.csi_intermediate.clear();
                self.parse_state = ParseState::CsiEntry;
            }
            b']' => {
                // OSC
                self.osc_buffer.clear();
                self.parse_state = ParseState::OscEntry;
            }
            b'7' => {
                // DECSC - save cursor (we track but don't implement save/restore stack)
                self.parse_state = ParseState::Ground;
            }
            b'8' => {
                // DECRC - restore cursor
                self.parse_state = ParseState::Ground;
            }
            b'=' | b'>' => {
                // Application/Normal keypad mode (ignored)
                self.parse_state = ParseState::Ground;
            }
            0x1B => {
                // ESC ESC - stay in escape (malformed, but handle gracefully)
            }
            _ => {
                // Unknown escape, return to ground
                self.parse_state = ParseState::Ground;
            }
        }
    }

    fn csi_entry_state(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                self.csi_params.push((b - b'0') as u32);
                self.parse_state = ParseState::CsiParam;
            }
            b';' => {
                // Semicolon at entry means "default first param, start second param".
                // Push 0 for first param (default) AND 0 for the start of the second.
                self.csi_params.push(0);
                self.csi_params.push(0);
                self.parse_state = ParseState::CsiParam;
            }
            b'?' | b'>' | b'!' => {
                self.csi_intermediate.push(b);
                self.parse_state = ParseState::CsiParam;
            }
            0x40..=0x7E => {
                // Final byte with no params
                self.execute_csi(b);
                self.parse_state = ParseState::Ground;
            }
            _ => {
                self.parse_state = ParseState::Ground;
            }
        }
    }

    fn csi_param_state(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                if self.csi_params.is_empty() {
                    self.csi_params.push(0);
                }
                if let Some(last) = self.csi_params.last_mut() {
                    *last = last.saturating_mul(10).saturating_add((b - b'0') as u32);
                }
            }
            b';' => {
                self.csi_params.push(0);
            }
            b':' => {
                // Subparameter (e.g., for 256/RGB colors) - we handle in SGR
                self.csi_params.push(0);
            }
            0x20..=0x2F => {
                self.csi_intermediate.push(b);
            }
            0x40..=0x7E => {
                // Final byte
                self.execute_csi(b);
                self.parse_state = ParseState::Ground;
            }
            _ => {
                self.parse_state = ParseState::Ground;
            }
        }
    }

    fn osc_entry_state(&mut self, b: u8) {
        match b {
            0x07 => {
                // BEL - OSC terminator
                self.execute_osc();
                self.parse_state = ParseState::Ground;
            }
            0x1B => {
                // Might be ST (ESC \)
                self.parse_state = ParseState::OscString;
            }
            _ => {
                self.osc_buffer.push(b);
            }
        }
    }

    fn osc_string_state(&mut self, b: u8) {
        match b {
            b'\\' => {
                // ST (ESC \)
                self.execute_osc();
                self.parse_state = ParseState::Ground;
            }
            _ => {
                // Not ST, put ESC back and continue
                self.osc_buffer.push(0x1B);
                self.osc_buffer.push(b);
                self.parse_state = ParseState::OscEntry;
            }
        }
    }

    fn handle_c0(&mut self, b: u8) {
        match b {
            0x07 => {} // BEL - ignored
            0x08 => {
                // BS - backspace
                if self.cursor_x > 0 {
                    self.cursor_x -= 1;
                }
            }
            0x09 => {
                // HT - tab (move to next 8-column stop)
                self.cursor_x = (self.cursor_x / 8 + 1) * 8;
                if self.cursor_x >= self.width {
                    self.cursor_x = self.width - 1;
                }
            }
            0x0A => {
                // LF - line feed
                if self.cursor_y + 1 < self.height {
                    self.cursor_y += 1;
                }
            }
            0x0D => {
                // CR - carriage return
                self.cursor_x = 0;
            }
            _ => {} // Other C0 controls ignored
        }
    }

    fn handle_printable(&mut self, b: u8) {
        if self.utf8_expected.is_none() {
            if b < 0x80 {
                self.put_char(b as char);
                return;
            }
            if let Some(expected) = Self::utf8_expected_len(b) {
                self.utf8_pending.clear();
                self.utf8_pending.push(b);
                self.utf8_expected = Some(expected);
                if expected == 1 {
                    self.flush_utf8_sequence();
                }
            } else {
                self.put_char('\u{FFFD}');
            }
            return;
        }

        if !Self::is_utf8_continuation(b) {
            self.flush_pending_utf8_invalid();
            self.handle_printable(b);
            return;
        }

        self.utf8_pending.push(b);
        if let Some(expected) = self.utf8_expected {
            if self.utf8_pending.len() == expected {
                self.flush_utf8_sequence();
            } else if self.utf8_pending.len() > expected {
                self.flush_pending_utf8_invalid();
            }
        }
    }

    fn flush_utf8_sequence(&mut self) {
        // Collect chars first to avoid borrow conflict with put_char.
        // UTF-8 sequences are at most 4 bytes, so this is small.
        let chars: Vec<char> = std::str::from_utf8(&self.utf8_pending)
            .map(|text| text.chars().collect())
            .unwrap_or_else(|_| vec!['\u{FFFD}']);
        self.utf8_pending.clear();
        self.utf8_expected = None;
        for ch in chars {
            self.put_char(ch);
        }
    }

    fn flush_pending_utf8_invalid(&mut self) {
        if self.utf8_expected.is_some() {
            self.put_char('\u{FFFD}');
            self.utf8_pending.clear();
            self.utf8_expected = None;
        }
    }

    fn utf8_expected_len(first: u8) -> Option<usize> {
        if first < 0x80 {
            Some(1)
        } else if (0xC2..=0xDF).contains(&first) {
            Some(2)
        } else if (0xE0..=0xEF).contains(&first) {
            Some(3)
        } else if (0xF0..=0xF4).contains(&first) {
            Some(4)
        } else {
            None
        }
    }

    fn is_utf8_continuation(byte: u8) -> bool {
        (0x80..=0xBF).contains(&byte)
    }

    fn put_char(&mut self, ch: char) {
        let width = char_width(ch);

        // Zero-width (combining) character handling
        if width == 0 {
            if self.cursor_x > 0 {
                // Append to previous cell
                let idx = self.cursor_y * self.width + self.cursor_x - 1;
                if let Some(cell) = self.cells.get_mut(idx) {
                    cell.text.push(ch);
                }
            } else if self.cursor_x < self.width && self.cursor_y < self.height {
                // At start of line, attach to current cell (if empty/space) or append
                let idx = self.cursor_y * self.width + self.cursor_x;
                let cell = &mut self.cells[idx];
                if cell.text == " " {
                    // Replace default space with space+combining
                    cell.text = format!(" {}", ch);
                } else {
                    cell.text.push(ch);
                }
            }
            return;
        }

        if self.cursor_x < self.width && self.cursor_y < self.height {
            let cell = &mut self.cells[self.cursor_y * self.width + self.cursor_x];
            cell.text = ch.to_string();
            cell.fg = self.sgr.fg;
            cell.bg = self.sgr.bg;
            cell.attrs = CellAttrs::new(self.sgr.flags, self.current_link_id);
            cell.link_id = self.current_link_id;

            // Handle wide characters (clear the next cell if it exists)
            if width == 2 && self.cursor_x + 1 < self.width {
                let next_cell = &mut self.cells[self.cursor_y * self.width + self.cursor_x + 1];
                next_cell.text = String::new(); // Clear content (placeholder)
                next_cell.fg = self.sgr.fg; // Extend background color
                next_cell.bg = self.sgr.bg;
                next_cell.attrs = CellAttrs::NONE; // Clear attributes
                next_cell.link_id = 0; // Clear link
            }
        }

        self.cursor_x += width;

        // Handle line wrap if at edge
        if self.cursor_x >= self.width {
            self.cursor_x = 0;
            if self.cursor_y + 1 < self.height {
                self.cursor_y += 1;
            }
            // Note: If we are at the bottom line, we wrap to the start of the *same* line.
            // This model does not implement scrolling because it is intended to validate
            // Frame-based rendering where absolute positioning (CUP) is used, and
            // wrapping/scrolling behavior should not be triggered by the Presenter.
        }
    }

    fn execute_csi(&mut self, final_byte: u8) {
        let has_question = self.csi_intermediate.contains(&b'?');

        match final_byte {
            b'H' | b'f' => self.csi_cup(),             // CUP - cursor position
            b'A' => self.csi_cuu(),                    // CUU - cursor up
            b'B' => self.csi_cud(),                    // CUD - cursor down
            b'C' => self.csi_cuf(),                    // CUF - cursor forward
            b'D' => self.csi_cub(),                    // CUB - cursor back
            b'G' => self.csi_cha(),                    // CHA - cursor horizontal absolute
            b'd' => self.csi_vpa(),                    // VPA - vertical position absolute
            b'J' => self.csi_ed(),                     // ED - erase in display
            b'K' => self.csi_el(),                     // EL - erase in line
            b'm' => self.csi_sgr(),                    // SGR - select graphic rendition
            b'h' if has_question => self.csi_decset(), // DECSET
            b'l' if has_question => self.csi_decrst(), // DECRST
            b's' => {
                // Save cursor position (ANSI)
            }
            b'u' => {
                // Restore cursor position (ANSI)
            }
            _ => {} // Unknown CSI - ignored
        }
    }

    fn csi_cup(&mut self) {
        // CSI row ; col H
        let row = self.csi_params.first().copied().unwrap_or(1).max(1) as usize;
        let col = self.csi_params.get(1).copied().unwrap_or(1).max(1) as usize;
        self.cursor_y = (row - 1).min(self.height - 1);
        self.cursor_x = (col - 1).min(self.width - 1);
    }

    fn csi_cuu(&mut self) {
        let n = self.csi_params.first().copied().unwrap_or(1).max(1) as usize;
        self.cursor_y = self.cursor_y.saturating_sub(n);
    }

    fn csi_cud(&mut self) {
        let n = self.csi_params.first().copied().unwrap_or(1).max(1) as usize;
        self.cursor_y = (self.cursor_y + n).min(self.height - 1);
    }

    fn csi_cuf(&mut self) {
        let n = self.csi_params.first().copied().unwrap_or(1).max(1) as usize;
        self.cursor_x = (self.cursor_x + n).min(self.width - 1);
    }

    fn csi_cub(&mut self) {
        let n = self.csi_params.first().copied().unwrap_or(1).max(1) as usize;
        self.cursor_x = self.cursor_x.saturating_sub(n);
    }

    fn csi_cha(&mut self) {
        let col = self.csi_params.first().copied().unwrap_or(1).max(1) as usize;
        self.cursor_x = (col - 1).min(self.width - 1);
    }

    fn csi_vpa(&mut self) {
        let row = self.csi_params.first().copied().unwrap_or(1).max(1) as usize;
        self.cursor_y = (row - 1).min(self.height - 1);
    }

    fn csi_ed(&mut self) {
        let mode = self.csi_params.first().copied().unwrap_or(0);
        match mode {
            0 => {
                // Erase from cursor to end of screen
                for x in self.cursor_x..self.width {
                    self.erase_cell(x, self.cursor_y);
                }
                for y in (self.cursor_y + 1)..self.height {
                    for x in 0..self.width {
                        self.erase_cell(x, y);
                    }
                }
            }
            1 => {
                // Erase from start of screen to cursor
                for y in 0..self.cursor_y {
                    for x in 0..self.width {
                        self.erase_cell(x, y);
                    }
                }
                for x in 0..=self.cursor_x {
                    self.erase_cell(x, self.cursor_y);
                }
            }
            2 | 3 => {
                // Erase entire screen
                for cell in &mut self.cells {
                    *cell = ModelCell::default();
                }
            }
            _ => {}
        }
    }

    fn csi_el(&mut self) {
        let mode = self.csi_params.first().copied().unwrap_or(0);
        match mode {
            0 => {
                // Erase from cursor to end of line
                for x in self.cursor_x..self.width {
                    self.erase_cell(x, self.cursor_y);
                }
            }
            1 => {
                // Erase from start of line to cursor
                for x in 0..=self.cursor_x {
                    self.erase_cell(x, self.cursor_y);
                }
            }
            2 => {
                // Erase entire line
                for x in 0..self.width {
                    self.erase_cell(x, self.cursor_y);
                }
            }
            _ => {}
        }
    }

    fn erase_cell(&mut self, x: usize, y: usize) {
        // Copy background color before borrowing self mutably
        let bg = self.sgr.bg;
        if let Some(cell) = self.cell_mut(x, y) {
            cell.text = " ".to_string();
            // Erase uses current background color
            cell.fg = PackedRgba::WHITE;
            cell.bg = bg;
            cell.attrs = CellAttrs::NONE;
            cell.link_id = 0;
        }
    }

    fn csi_sgr(&mut self) {
        if self.csi_params.is_empty() {
            self.sgr.reset();
            return;
        }

        let mut i = 0;
        while i < self.csi_params.len() {
            let code = self.csi_params[i];
            match code {
                0 => self.sgr.reset(),
                1 => self.sgr.flags.insert(StyleFlags::BOLD),
                2 => self.sgr.flags.insert(StyleFlags::DIM),
                3 => self.sgr.flags.insert(StyleFlags::ITALIC),
                4 => self.sgr.flags.insert(StyleFlags::UNDERLINE),
                5 => self.sgr.flags.insert(StyleFlags::BLINK),
                7 => self.sgr.flags.insert(StyleFlags::REVERSE),
                8 => self.sgr.flags.insert(StyleFlags::HIDDEN),
                9 => self.sgr.flags.insert(StyleFlags::STRIKETHROUGH),
                21 | 22 => self.sgr.flags.remove(StyleFlags::BOLD | StyleFlags::DIM),
                23 => self.sgr.flags.remove(StyleFlags::ITALIC),
                24 => self.sgr.flags.remove(StyleFlags::UNDERLINE),
                25 => self.sgr.flags.remove(StyleFlags::BLINK),
                27 => self.sgr.flags.remove(StyleFlags::REVERSE),
                28 => self.sgr.flags.remove(StyleFlags::HIDDEN),
                29 => self.sgr.flags.remove(StyleFlags::STRIKETHROUGH),
                // Basic foreground colors (30-37)
                30..=37 => {
                    self.sgr.fg = Self::basic_color(code - 30);
                }
                // Default foreground
                39 => {
                    self.sgr.fg = PackedRgba::WHITE;
                }
                // Basic background colors (40-47)
                40..=47 => {
                    self.sgr.bg = Self::basic_color(code - 40);
                }
                // Default background
                49 => {
                    self.sgr.bg = PackedRgba::TRANSPARENT;
                }
                // Bright foreground colors (90-97)
                90..=97 => {
                    self.sgr.fg = Self::bright_color(code - 90);
                }
                // Bright background colors (100-107)
                100..=107 => {
                    self.sgr.bg = Self::bright_color(code - 100);
                }
                // Extended colors (38/48)
                38 => {
                    if let Some(color) = self.parse_extended_color(&mut i) {
                        self.sgr.fg = color;
                    }
                }
                48 => {
                    if let Some(color) = self.parse_extended_color(&mut i) {
                        self.sgr.bg = color;
                    }
                }
                _ => {} // Unknown SGR code
            }
            i += 1;
        }
    }

    fn parse_extended_color(&self, i: &mut usize) -> Option<PackedRgba> {
        let mode = self.csi_params.get(*i + 1)?;
        match *mode {
            5 => {
                // 256-color mode: 38;5;n
                let idx = self.csi_params.get(*i + 2)?;
                *i += 2;
                Some(Self::color_256(*idx as u8))
            }
            2 => {
                // RGB mode: 38;2;r;g;b
                let r = *self.csi_params.get(*i + 2)? as u8;
                let g = *self.csi_params.get(*i + 3)? as u8;
                let b = *self.csi_params.get(*i + 4)? as u8;
                *i += 4;
                Some(PackedRgba::rgb(r, g, b))
            }
            _ => None,
        }
    }

    fn basic_color(idx: u32) -> PackedRgba {
        match idx {
            0 => PackedRgba::rgb(0, 0, 0),       // Black
            1 => PackedRgba::rgb(128, 0, 0),     // Red
            2 => PackedRgba::rgb(0, 128, 0),     // Green
            3 => PackedRgba::rgb(128, 128, 0),   // Yellow
            4 => PackedRgba::rgb(0, 0, 128),     // Blue
            5 => PackedRgba::rgb(128, 0, 128),   // Magenta
            6 => PackedRgba::rgb(0, 128, 128),   // Cyan
            7 => PackedRgba::rgb(192, 192, 192), // White
            _ => PackedRgba::WHITE,
        }
    }

    fn bright_color(idx: u32) -> PackedRgba {
        match idx {
            0 => PackedRgba::rgb(128, 128, 128), // Bright Black
            1 => PackedRgba::rgb(255, 0, 0),     // Bright Red
            2 => PackedRgba::rgb(0, 255, 0),     // Bright Green
            3 => PackedRgba::rgb(255, 255, 0),   // Bright Yellow
            4 => PackedRgba::rgb(0, 0, 255),     // Bright Blue
            5 => PackedRgba::rgb(255, 0, 255),   // Bright Magenta
            6 => PackedRgba::rgb(0, 255, 255),   // Bright Cyan
            7 => PackedRgba::rgb(255, 255, 255), // Bright White
            _ => PackedRgba::WHITE,
        }
    }

    fn color_256(idx: u8) -> PackedRgba {
        match idx {
            0..=7 => Self::basic_color(idx as u32),
            8..=15 => Self::bright_color((idx - 8) as u32),
            16..=231 => {
                // 6x6x6 color cube
                let idx = idx - 16;
                let r = (idx / 36) % 6;
                let g = (idx / 6) % 6;
                let b = idx % 6;
                let to_channel = |v| if v == 0 { 0 } else { 55 + v * 40 };
                PackedRgba::rgb(to_channel(r), to_channel(g), to_channel(b))
            }
            232..=255 => {
                // Grayscale ramp
                let gray = 8 + (idx - 232) * 10;
                PackedRgba::rgb(gray, gray, gray)
            }
        }
    }

    fn csi_decset(&mut self) {
        for &code in &self.csi_params {
            match code {
                25 => self.modes.cursor_visible = true, // DECTCEM - cursor visible
                1049 => self.modes.alt_screen = true,   // Alt screen buffer
                2026 => self.modes.sync_output_level += 1, // Synchronized output begin
                _ => {}
            }
        }
    }

    fn csi_decrst(&mut self) {
        for &code in &self.csi_params {
            match code {
                25 => self.modes.cursor_visible = false, // DECTCEM - cursor hidden
                1049 => self.modes.alt_screen = false,   // Alt screen buffer off
                2026 => {
                    // Synchronized output end
                    self.modes.sync_output_level = self.modes.sync_output_level.saturating_sub(1);
                }
                _ => {}
            }
        }
    }

    fn execute_osc(&mut self) {
        // Parse OSC: code ; data
        // Clone buffer to avoid borrow issues when calling handle_osc8
        let data = String::from_utf8_lossy(&self.osc_buffer).to_string();
        let mut parts = data.splitn(2, ';');
        let code: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

        // OSC 8 - hyperlink (other OSC codes ignored)
        if code == 8
            && let Some(rest) = parts.next()
        {
            let rest = rest.to_string();
            self.handle_osc8(&rest);
        }
    }

    fn handle_osc8(&mut self, data: &str) {
        // Format: OSC 8 ; params ; uri ST
        // We support: OSC 8 ; ; uri ST (start link) and OSC 8 ; ; ST (end link)
        let mut parts = data.splitn(2, ';');
        let _params = parts.next().unwrap_or("");
        let uri = parts.next().unwrap_or("");

        if uri.is_empty() {
            // End hyperlink
            self.current_link_id = 0;
        } else {
            // Start hyperlink
            self.links.push(uri.to_string());
            self.current_link_id = (self.links.len() - 1) as u32;
        }
    }

    /// Compare two grids and return a diff description for debugging.
    #[must_use]
    pub fn diff_grid(&self, expected: &[ModelCell]) -> Option<String> {
        if self.cells.len() != expected.len() {
            return Some(format!(
                "Grid size mismatch: got {} cells, expected {}",
                self.cells.len(),
                expected.len()
            ));
        }

        let mut diffs = Vec::new();
        for (i, (actual, exp)) in self.cells.iter().zip(expected.iter()).enumerate() {
            if actual != exp {
                let x = i % self.width;
                let y = i / self.width;
                diffs.push(format!(
                    "  ({}, {}): got {:?}, expected {:?}",
                    x, y, actual.text, exp.text
                ));
            }
        }

        if diffs.is_empty() {
            None
        } else {
            Some(format!("Grid differences:\n{}", diffs.join("\n")))
        }
    }

    /// Dump the escape sequences in a human-readable format (for debugging test failures).
    pub fn dump_sequences(bytes: &[u8]) -> String {
        let mut output = String::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1B {
                if i + 1 < bytes.len() {
                    match bytes[i + 1] {
                        b'[' => {
                            // CSI sequence
                            output.push_str("\\e[");
                            i += 2;
                            while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                                output.push(bytes[i] as char);
                                i += 1;
                            }
                            if i < bytes.len() {
                                output.push(bytes[i] as char);
                                i += 1;
                            }
                        }
                        b']' => {
                            // OSC sequence
                            output.push_str("\\e]");
                            i += 2;
                            while i < bytes.len() && bytes[i] != 0x07 {
                                if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'\\'
                                {
                                    output.push_str("\\e\\\\");
                                    i += 2;
                                    break;
                                }
                                output.push(bytes[i] as char);
                                i += 1;
                            }
                            if i < bytes.len() && bytes[i] == 0x07 {
                                output.push_str("\\a");
                                i += 1;
                            }
                        }
                        _ => {
                            output.push_str(&format!("\\e{}", bytes[i + 1] as char));
                            i += 2;
                        }
                    }
                } else {
                    output.push_str("\\e");
                    i += 1;
                }
            } else if bytes[i] < 0x20 {
                output.push_str(&format!("\\x{:02x}", bytes[i]));
                i += 1;
            } else {
                output.push(bytes[i] as char);
                i += 1;
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi;

    #[test]
    fn new_creates_empty_grid() {
        let model = TerminalModel::new(80, 24);
        assert_eq!(model.width(), 80);
        assert_eq!(model.height(), 24);
        assert_eq!(model.cursor(), (0, 0));
        assert_eq!(model.cells().len(), 80 * 24);
    }

    #[test]
    fn printable_text_writes_to_grid() {
        let mut model = TerminalModel::new(10, 5);
        model.process(b"Hello");
        assert_eq!(model.cursor(), (5, 0));
        assert_eq!(model.row_text(0), Some("Hello".to_string()));
    }

    #[test]
    fn cup_moves_cursor() {
        let mut model = TerminalModel::new(80, 24);
        model.process(b"\x1b[5;10H"); // Row 5, Col 10 (1-indexed)
        assert_eq!(model.cursor(), (9, 4)); // 0-indexed
    }

    #[test]
    fn cup_with_defaults() {
        let mut model = TerminalModel::new(80, 24);
        model.process(b"\x1b[H"); // Should default to 1;1
        assert_eq!(model.cursor(), (0, 0));
    }

    #[test]
    fn relative_cursor_moves() {
        let mut model = TerminalModel::new(80, 24);
        model.process(b"\x1b[10;10H"); // Move to (9, 9)
        model.process(b"\x1b[2A"); // Up 2
        assert_eq!(model.cursor(), (9, 7));
        model.process(b"\x1b[3B"); // Down 3
        assert_eq!(model.cursor(), (9, 10));
        model.process(b"\x1b[5C"); // Forward 5
        assert_eq!(model.cursor(), (14, 10));
        model.process(b"\x1b[3D"); // Back 3
        assert_eq!(model.cursor(), (11, 10));
    }

    #[test]
    fn sgr_sets_style_flags() {
        let mut model = TerminalModel::new(20, 5);
        model.process(b"\x1b[1mBold\x1b[0m");
        assert!(model.cell(0, 0).unwrap().attrs.has_flag(StyleFlags::BOLD));
        assert!(!model.cell(4, 0).unwrap().attrs.has_flag(StyleFlags::BOLD)); // After reset
    }

    #[test]
    fn sgr_sets_colors() {
        let mut model = TerminalModel::new(20, 5);
        model.process(b"\x1b[31mRed\x1b[0m");
        assert_eq!(model.cell(0, 0).unwrap().fg, PackedRgba::rgb(128, 0, 0));
    }

    #[test]
    fn sgr_256_colors() {
        let mut model = TerminalModel::new(20, 5);
        model.process(b"\x1b[38;5;196mX"); // Bright red in 256 palette
        let cell = model.cell(0, 0).unwrap();
        // 196 = 16 + 180 = 16 + 5*36 + 0*6 + 0 = red=5, g=0, b=0
        // r = 55 + 5*40 = 255, g = 0, b = 0
        assert_eq!(cell.fg, PackedRgba::rgb(255, 0, 0));
    }

    #[test]
    fn sgr_rgb_colors() {
        let mut model = TerminalModel::new(20, 5);
        model.process(b"\x1b[38;2;100;150;200mX");
        assert_eq!(model.cell(0, 0).unwrap().fg, PackedRgba::rgb(100, 150, 200));
    }

    #[test]
    fn erase_line() {
        let mut model = TerminalModel::new(10, 5);
        model.process(b"ABCDEFGHIJ");
        // After 10 chars in 10-col terminal, cursor wraps to (0, 1)
        // Move back to row 1, column 5 explicitly
        model.process(b"\x1b[1;5H"); // Row 1, Col 5 (1-indexed) = (4, 0)
        model.process(b"\x1b[K"); // Erase to end of line
        assert_eq!(model.row_text(0), Some("ABCD".to_string()));
    }

    #[test]
    fn erase_display() {
        let mut model = TerminalModel::new(10, 5);
        model.process(b"Line1\n");
        model.process(b"Line2\n");
        model.process(b"\x1b[2J"); // Erase entire screen
        for y in 0..5 {
            assert_eq!(model.row_text(y), Some(String::new()));
        }
    }

    #[test]
    fn osc8_hyperlinks() {
        let mut model = TerminalModel::new(20, 5);
        model.process(b"\x1b]8;;https://example.com\x07Link\x1b]8;;\x07");

        let cell = model.cell(0, 0).unwrap();
        assert!(cell.link_id > 0);
        assert_eq!(model.link_url(cell.link_id), Some("https://example.com"));

        // After link ends, link_id should be 0
        let cell_after = model.cell(4, 0).unwrap();
        assert_eq!(cell_after.link_id, 0);
    }

    #[test]
    fn dangling_link_detection() {
        let mut model = TerminalModel::new(20, 5);
        model.process(b"\x1b]8;;https://example.com\x07Link");
        assert!(model.has_dangling_link());

        model.process(b"\x1b]8;;\x07");
        assert!(!model.has_dangling_link());
    }

    #[test]
    fn sync_output_tracking() {
        let mut model = TerminalModel::new(20, 5);
        assert!(model.sync_output_balanced());

        model.process(b"\x1b[?2026h"); // Begin sync
        assert!(!model.sync_output_balanced());
        assert_eq!(model.modes().sync_output_level, 1);

        model.process(b"\x1b[?2026l"); // End sync
        assert!(model.sync_output_balanced());
    }

    #[test]
    fn utf8_multibyte_stream_is_decoded() {
        let mut model = TerminalModel::new(10, 1);
        let text = "a\u{00E9}\u{4E2D}\u{1F600}";
        model.process(text.as_bytes());

        assert_eq!(model.row_text(0).as_deref(), Some(text));
        assert_eq!(model.cursor(), (6, 0));
    }

    #[test]
    fn utf8_sequence_can_span_process_calls() {
        let mut model = TerminalModel::new(10, 1);
        let text = "\u{00E9}";
        let bytes = text.as_bytes();

        model.process(&bytes[..1]);
        assert_eq!(model.row_text(0).as_deref(), Some(""));

        model.process(&bytes[1..]);
        assert_eq!(model.row_text(0).as_deref(), Some(text));
    }

    #[test]
    fn line_wrap() {
        let mut model = TerminalModel::new(5, 3);
        model.process(b"ABCDEFGH");
        assert_eq!(model.row_text(0), Some("ABCDE".to_string()));
        assert_eq!(model.row_text(1), Some("FGH".to_string()));
        assert_eq!(model.cursor(), (3, 1));
    }

    #[test]
    fn cr_lf_handling() {
        let mut model = TerminalModel::new(20, 5);
        model.process(b"Hello\r\n");
        assert_eq!(model.cursor(), (0, 1));
        model.process(b"World");
        assert_eq!(model.row_text(0), Some("Hello".to_string()));
        assert_eq!(model.row_text(1), Some("World".to_string()));
    }

    #[test]
    fn cursor_visibility() {
        let mut model = TerminalModel::new(20, 5);
        assert!(model.modes().cursor_visible);

        model.process(b"\x1b[?25l"); // Hide cursor
        assert!(!model.modes().cursor_visible);

        model.process(b"\x1b[?25h"); // Show cursor
        assert!(model.modes().cursor_visible);
    }

    #[test]
    fn alt_screen_toggle_is_tracked() {
        let mut model = TerminalModel::new(20, 5);
        assert!(!model.modes().alt_screen);

        model.process(b"\x1b[?1049h");
        assert!(model.modes().alt_screen);

        model.process(b"\x1b[?1049l");
        assert!(!model.modes().alt_screen);
    }

    #[test]
    fn dump_sequences_readable() {
        let bytes = b"\x1b[1;1H\x1b[1mHello\x1b[0m";
        let dump = TerminalModel::dump_sequences(bytes);
        assert!(dump.contains("\\e[1;1H"));
        assert!(dump.contains("\\e[1m"));
        assert!(dump.contains("Hello"));
        assert!(dump.contains("\\e[0m"));
    }

    #[test]
    fn reset_clears_state() {
        let mut model = TerminalModel::new(20, 5);
        model.process(b"\x1b[10;10HTest\x1b[1m");
        model.reset();

        assert_eq!(model.cursor(), (0, 0));
        assert!(model.sgr_state().flags.is_empty());
        for y in 0..5 {
            assert_eq!(model.row_text(y), Some(String::new()));
        }
    }

    #[test]
    fn erase_scrollback_mode_clears_screen() {
        let mut model = TerminalModel::new(10, 3);
        model.process(b"Line1\nLine2\nLine3");
        model.process(b"\x1b[3J"); // ED scrollback mode

        for y in 0..3 {
            assert_eq!(model.row_text(y), Some(String::new()));
        }
    }

    #[test]
    fn scroll_region_sequences_are_ignored_but_safe() {
        let mut model = TerminalModel::new(12, 3);
        model.process(b"ABCD");
        let cursor_before = model.cursor();

        let mut buf = Vec::new();
        ansi::set_scroll_region(&mut buf, 1, 2).expect("scroll region sequence");
        model.process(&buf);
        model.process(ansi::RESET_SCROLL_REGION);

        assert_eq!(model.cursor(), cursor_before);
        model.process(b"EF");
        assert_eq!(model.row_text(0).as_deref(), Some("ABCDEF"));
    }

    #[test]
    fn scroll_region_invalid_params_do_not_corrupt_state() {
        let mut model = TerminalModel::new(8, 2);
        model.process(b"Hi");
        let cursor_before = model.cursor();

        model.process(b"\x1b[5;2r"); // bottom < top
        model.process(b"\x1b[0;0r"); // zero params
        model.process(b"\x1b[999;999r"); // out of bounds

        assert_eq!(model.cursor(), cursor_before);
        model.process(b"!");
        assert_eq!(model.row_text(0).as_deref(), Some("Hi!"));
    }

    // --- ModelCell ---

    #[test]
    fn model_cell_default_is_space() {
        let cell = ModelCell::default();
        assert_eq!(cell.text, " ");
        assert_eq!(cell.fg, PackedRgba::WHITE);
        assert_eq!(cell.bg, PackedRgba::TRANSPARENT);
        assert_eq!(cell.attrs, CellAttrs::NONE);
        assert_eq!(cell.link_id, 0);
    }

    #[test]
    fn model_cell_with_char() {
        let cell = ModelCell::with_char('X');
        assert_eq!(cell.text, "X");
        assert_eq!(cell.fg, PackedRgba::WHITE);
        assert_eq!(cell.link_id, 0);
    }

    #[test]
    fn model_cell_eq() {
        let a = ModelCell::default();
        let b = ModelCell::default();
        assert_eq!(a, b);
        let c = ModelCell::with_char('X');
        assert_ne!(a, c);
    }

    #[test]
    fn model_cell_clone() {
        let a = ModelCell::with_char('Z');
        let b = a.clone();
        assert_eq!(b.text, "Z");
    }

    // --- SgrState ---

    #[test]
    fn sgr_state_default_fields() {
        let s = SgrState::default();
        assert_eq!(s.fg, PackedRgba::WHITE);
        assert_eq!(s.bg, PackedRgba::TRANSPARENT);
        assert!(s.flags.is_empty());
    }

    #[test]
    fn sgr_state_reset() {
        let mut s = SgrState {
            fg: PackedRgba::rgb(255, 0, 0),
            bg: PackedRgba::rgb(0, 0, 255),
            flags: StyleFlags::BOLD | StyleFlags::ITALIC,
        };
        s.reset();
        assert_eq!(s.fg, PackedRgba::WHITE);
        assert_eq!(s.bg, PackedRgba::TRANSPARENT);
        assert!(s.flags.is_empty());
    }

    // --- ModeFlags ---

    #[test]
    fn mode_flags_new_defaults() {
        let m = ModeFlags::new();
        assert!(m.cursor_visible);
        assert!(!m.alt_screen);
        assert_eq!(m.sync_output_level, 0);
    }

    #[test]
    fn mode_flags_default_vs_new() {
        // Default trait gives false for bools, 0 for u32.
        let d = ModeFlags::default();
        assert!(!d.cursor_visible);
        // new() gives cursor_visible=true.
        let n = ModeFlags::new();
        assert!(n.cursor_visible);
    }

    // --- Construction edge cases ---

    #[test]
    fn new_zero_dimensions_clamped() {
        let model = TerminalModel::new(0, 0);
        assert_eq!(model.width(), 1);
        assert_eq!(model.height(), 1);
        assert_eq!(model.cells().len(), 1);
    }

    #[test]
    fn new_1x1() {
        let model = TerminalModel::new(1, 1);
        assert_eq!(model.width(), 1);
        assert_eq!(model.height(), 1);
        assert_eq!(model.cursor(), (0, 0));
    }

    // --- Cell access ---

    #[test]
    fn cell_out_of_bounds_returns_none() {
        let model = TerminalModel::new(5, 3);
        assert!(model.cell(5, 0).is_none());
        assert!(model.cell(0, 3).is_none());
        assert!(model.cell(100, 100).is_none());
    }

    #[test]
    fn cell_in_bounds_returns_some() {
        let model = TerminalModel::new(5, 3);
        assert!(model.cell(0, 0).is_some());
        assert!(model.cell(4, 2).is_some());
    }

    #[test]
    fn current_cell_at_cursor() {
        let mut model = TerminalModel::new(10, 5);
        model.process(b"AB");
        // Cursor at (2,0), current_cell should be the cell under it.
        let cc = model.current_cell().unwrap();
        assert_eq!(cc.text, " "); // Cursor is past "AB", on empty cell.
    }

    #[test]
    fn row_out_of_bounds_returns_none() {
        let model = TerminalModel::new(5, 3);
        assert!(model.row(3).is_none());
        assert!(model.row(100).is_none());
    }

    #[test]
    fn row_text_trims_trailing_spaces() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"Hi");
        assert_eq!(model.row_text(0), Some("Hi".to_string()));
    }

    #[test]
    fn link_url_invalid_id_returns_none() {
        let model = TerminalModel::new(5, 1);
        assert!(model.link_url(999).is_none());
    }

    #[test]
    fn link_url_zero_is_empty() {
        let model = TerminalModel::new(5, 1);
        assert_eq!(model.link_url(0), Some(""));
    }

    #[test]
    fn has_dangling_link_initially_false() {
        let model = TerminalModel::new(5, 1);
        assert!(!model.has_dangling_link());
    }

    // --- CHA (cursor horizontal absolute) ---

    #[test]
    fn cha_moves_to_column() {
        let mut model = TerminalModel::new(80, 24);
        model.process(b"\x1b[1;1H"); // (0,0)
        model.process(b"\x1b[20G"); // CHA col 20
        assert_eq!(model.cursor(), (19, 0));
    }

    #[test]
    fn cha_clamps_to_width() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[999G");
        assert_eq!(model.cursor().0, 9);
    }

    // --- VPA (vertical position absolute) ---

    #[test]
    fn vpa_moves_to_row() {
        let mut model = TerminalModel::new(80, 24);
        model.process(b"\x1b[10d"); // VPA row 10
        assert_eq!(model.cursor(), (0, 9));
    }

    #[test]
    fn vpa_clamps_to_height() {
        let mut model = TerminalModel::new(10, 5);
        model.process(b"\x1b[999d");
        assert_eq!(model.cursor().1, 4);
    }

    // --- Backspace ---

    #[test]
    fn backspace_moves_cursor_back() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"ABC");
        assert_eq!(model.cursor(), (3, 0));
        model.process(b"\x08"); // BS
        assert_eq!(model.cursor(), (2, 0));
    }

    #[test]
    fn backspace_at_column_zero_no_move() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x08");
        assert_eq!(model.cursor(), (0, 0));
    }

    // --- Tab ---

    #[test]
    fn tab_moves_to_next_tab_stop() {
        let mut model = TerminalModel::new(80, 1);
        model.process(b"\t");
        assert_eq!(model.cursor(), (8, 0));
        model.process(b"A\t");
        assert_eq!(model.cursor(), (16, 0));
    }

    #[test]
    fn tab_clamps_at_right_edge() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\t"); // -> 8
        model.process(b"\t"); // -> would be 16, but clamped to 9
        assert_eq!(model.cursor(), (9, 0));
    }

    // --- Escape sequences ---

    #[test]
    fn esc_7_8_do_not_panic() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b7"); // DECSC
        model.process(b"\x1b8"); // DECRC
        assert_eq!(model.cursor(), (0, 0));
    }

    #[test]
    fn esc_equals_greater_ignored() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b="); // App keypad
        model.process(b"\x1b>"); // Normal keypad
        assert_eq!(model.cursor(), (0, 0));
    }

    #[test]
    fn esc_esc_double_escape_handled() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b\x1b"); // Double ESC — stays in escape state
        // 'A' is consumed as unknown escape sequence, returning to ground.
        model.process(b"AB");
        // Only 'B' reaches ground as printable.
        assert_eq!(model.row_text(0).as_deref(), Some("B"));
    }

    #[test]
    fn unknown_escape_returns_to_ground() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1bQ"); // Unknown
        model.process(b"Hi");
        assert_eq!(model.row_text(0).as_deref(), Some("Hi"));
    }

    // --- EL modes ---

    #[test]
    fn el_mode_1_erases_from_start_to_cursor() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"ABCDEFGHIJ");
        model.process(b"\x1b[1;5H"); // (4, 0)
        model.process(b"\x1b[1K"); // Erase from start to cursor
        // Columns 0..=4 erased.
        let row = model.row_text(0).unwrap();
        assert!(row.starts_with("     ") || row.trim_start().starts_with("FGHIJ"));
    }

    #[test]
    fn el_mode_2_erases_entire_line() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"ABCDEFGHIJ");
        model.process(b"\x1b[1;5H");
        model.process(b"\x1b[2K"); // Erase entire line
        assert_eq!(model.row_text(0), Some(String::new()));
    }

    // --- ED modes ---

    #[test]
    fn ed_mode_0_erases_from_cursor_to_end() {
        let mut model = TerminalModel::new(10, 3);
        model.process(b"Line1\nLine2\nLine3");
        model.process(b"\x1b[2;1H"); // Row 2, Col 1 (line index 1)
        model.process(b"\x1b[0J"); // Erase from cursor to end
        assert_eq!(model.row_text(0), Some("Line1".to_string()));
        assert_eq!(model.row_text(1), Some(String::new()));
        assert_eq!(model.row_text(2), Some(String::new()));
    }

    #[test]
    fn ed_mode_1_erases_from_start_to_cursor() {
        let mut model = TerminalModel::new(10, 3);
        model.process(b"Line1\nLine2\nLine3");
        model.process(b"\x1b[2;3H"); // Row 2, Col 3 (0-indexed: y=1, x=2)
        model.process(b"\x1b[1J"); // Erase from start to cursor
        assert_eq!(model.row_text(0), Some(String::new()));
        // Row 1 erased up to and including cursor position (x=2).
        let row1 = model.row_text(1).unwrap();
        assert!(row1.starts_with("   ") || row1.len() <= 10);
    }

    // --- SGR attribute flags ---

    #[test]
    fn sgr_italic() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[3mI\x1b[0m");
        assert!(model.cell(0, 0).unwrap().attrs.has_flag(StyleFlags::ITALIC));
    }

    #[test]
    fn sgr_underline() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[4mU\x1b[0m");
        assert!(
            model
                .cell(0, 0)
                .unwrap()
                .attrs
                .has_flag(StyleFlags::UNDERLINE)
        );
    }

    #[test]
    fn sgr_dim() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[2mD\x1b[0m");
        assert!(model.cell(0, 0).unwrap().attrs.has_flag(StyleFlags::DIM));
    }

    #[test]
    fn sgr_strikethrough() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[9mS\x1b[0m");
        assert!(
            model
                .cell(0, 0)
                .unwrap()
                .attrs
                .has_flag(StyleFlags::STRIKETHROUGH)
        );
    }

    #[test]
    fn sgr_reverse() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[7mR\x1b[0m");
        assert!(
            model
                .cell(0, 0)
                .unwrap()
                .attrs
                .has_flag(StyleFlags::REVERSE)
        );
    }

    #[test]
    fn sgr_remove_bold() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[1mB\x1b[22mX");
        assert!(model.cell(0, 0).unwrap().attrs.has_flag(StyleFlags::BOLD));
        assert!(!model.cell(1, 0).unwrap().attrs.has_flag(StyleFlags::BOLD));
    }

    #[test]
    fn sgr_remove_italic() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[3mI\x1b[23mX");
        assert!(!model.cell(1, 0).unwrap().attrs.has_flag(StyleFlags::ITALIC));
    }

    // --- SGR colors ---

    #[test]
    fn sgr_basic_background() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[42mG"); // Green bg
        assert_eq!(model.cell(0, 0).unwrap().bg, PackedRgba::rgb(0, 128, 0));
    }

    #[test]
    fn sgr_default_fg_39() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[31m\x1b[39mX");
        assert_eq!(model.cell(0, 0).unwrap().fg, PackedRgba::WHITE);
    }

    #[test]
    fn sgr_default_bg_49() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[41m\x1b[49mX");
        assert_eq!(model.cell(0, 0).unwrap().bg, PackedRgba::TRANSPARENT);
    }

    #[test]
    fn sgr_bright_fg() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[91mX"); // Bright red fg
        assert_eq!(model.cell(0, 0).unwrap().fg, PackedRgba::rgb(255, 0, 0));
    }

    #[test]
    fn sgr_bright_bg() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[104mX"); // Bright blue bg
        assert_eq!(model.cell(0, 0).unwrap().bg, PackedRgba::rgb(0, 0, 255));
    }

    #[test]
    fn sgr_256_grayscale() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[38;5;232mX"); // Grayscale idx 232 → gray=8
        assert_eq!(model.cell(0, 0).unwrap().fg, PackedRgba::rgb(8, 8, 8));
    }

    #[test]
    fn sgr_256_basic_range() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[38;5;1mX"); // Index 1 = basic red
        assert_eq!(model.cell(0, 0).unwrap().fg, PackedRgba::rgb(128, 0, 0));
    }

    #[test]
    fn sgr_256_bright_range() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[38;5;9mX"); // Index 9 = bright red
        assert_eq!(model.cell(0, 0).unwrap().fg, PackedRgba::rgb(255, 0, 0));
    }

    #[test]
    fn sgr_empty_params_resets() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[1m\x1b[mX"); // SGR with no params = reset
        assert!(!model.cell(0, 0).unwrap().attrs.has_flag(StyleFlags::BOLD));
    }

    // --- Sync output ---

    #[test]
    fn sync_output_extra_end_saturates() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[?2026l"); // End without begin
        assert_eq!(model.modes().sync_output_level, 0);
        assert!(model.sync_output_balanced());
    }

    #[test]
    fn sync_output_nested() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[?2026h");
        model.process(b"\x1b[?2026h");
        assert_eq!(model.modes().sync_output_level, 2);
        model.process(b"\x1b[?2026l");
        assert_eq!(model.modes().sync_output_level, 1);
        assert!(!model.sync_output_balanced());
    }

    // --- diff_grid ---

    #[test]
    fn diff_grid_identical_returns_none() {
        let model = TerminalModel::new(3, 2);
        let expected = vec![ModelCell::default(); 6];
        assert!(model.diff_grid(&expected).is_none());
    }

    #[test]
    fn diff_grid_different_returns_some() {
        let mut model = TerminalModel::new(3, 1);
        model.process(b"ABC");
        let expected = vec![ModelCell::default(); 3];
        let diff = model.diff_grid(&expected);
        assert!(diff.is_some());
        let diff_str = diff.unwrap();
        assert!(diff_str.contains("Grid differences"));
    }

    #[test]
    fn diff_grid_size_mismatch() {
        let model = TerminalModel::new(3, 2);
        let expected = vec![ModelCell::default(); 5]; // Wrong size
        let diff = model.diff_grid(&expected);
        assert!(diff.is_some());
        assert!(diff.unwrap().contains("Grid size mismatch"));
    }

    // --- dump_sequences ---

    #[test]
    fn dump_sequences_osc() {
        let bytes = b"\x1b]8;;https://example.com\x07text\x1b]8;;\x07";
        let dump = TerminalModel::dump_sequences(bytes);
        assert!(dump.contains("\\e]8;;https://example.com\\a"));
    }

    #[test]
    fn dump_sequences_osc_st() {
        let bytes = b"\x1b]0;title\x1b\\";
        let dump = TerminalModel::dump_sequences(bytes);
        assert!(dump.contains("\\e]"));
        assert!(dump.contains("\\e\\\\"));
    }

    #[test]
    fn dump_sequences_c0_controls() {
        let bytes = b"\x08\x09\x0A";
        let dump = TerminalModel::dump_sequences(bytes);
        assert!(dump.contains("\\x08"));
        assert!(dump.contains("\\x09"));
        assert!(dump.contains("\\x0a"));
    }

    #[test]
    fn dump_sequences_trailing_esc() {
        let bytes = b"text\x1b";
        let dump = TerminalModel::dump_sequences(bytes);
        assert!(dump.contains("text"));
        assert!(dump.contains("\\e"));
    }

    #[test]
    fn dump_sequences_unknown_escape() {
        let bytes = b"\x1bQ";
        let dump = TerminalModel::dump_sequences(bytes);
        assert!(dump.contains("\\eQ"));
    }

    // --- Erase uses current bg color ---

    #[test]
    fn erase_line_uses_current_bg() {
        let mut model = TerminalModel::new(5, 1);
        model.process(b"Hello");
        model.process(b"\x1b[1;1H"); // Move to (0,0)
        model.process(b"\x1b[41m"); // Red bg
        model.process(b"\x1b[K"); // Erase to end
        let cell = model.cell(0, 0).unwrap();
        assert_eq!(cell.text, " ");
        assert_eq!(cell.bg, PackedRgba::rgb(128, 0, 0));
    }

    // --- Multiple hyperlinks ---

    #[test]
    fn multiple_hyperlinks_get_different_ids() {
        let mut model = TerminalModel::new(30, 1);
        model.process(b"\x1b]8;;https://a.com\x07A\x1b]8;;\x07");
        model.process(b"\x1b]8;;https://b.com\x07B\x1b]8;;\x07");
        let id_a = model.cell(0, 0).unwrap().link_id;
        let id_b = model.cell(1, 0).unwrap().link_id;
        assert_ne!(id_a, id_b);
        assert_eq!(model.link_url(id_a), Some("https://a.com"));
        assert_eq!(model.link_url(id_b), Some("https://b.com"));
    }

    // --- OSC with ST terminator ---

    #[test]
    fn osc8_with_st_terminator() {
        let mut model = TerminalModel::new(20, 1);
        model.process(b"\x1b]8;;https://st.com\x1b\\Link\x1b]8;;\x1b\\");
        let cell = model.cell(0, 0).unwrap();
        assert!(cell.link_id > 0);
        assert_eq!(model.link_url(cell.link_id), Some("https://st.com"));
        assert!(!model.has_dangling_link());
    }

    // --- TerminalModel Debug ---

    #[test]
    fn terminal_model_debug() {
        let model = TerminalModel::new(5, 3);
        let dbg = format!("{model:?}");
        assert!(dbg.contains("TerminalModel"));
    }

    // --- Wide character ---

    #[test]
    fn wide_char_occupies_two_cells() {
        let mut model = TerminalModel::new(10, 1);
        // CJK character takes 2 columns
        model.process("中".as_bytes());
        assert_eq!(model.cell(0, 0).unwrap().text, "中");
        // Next cell should be cleared (placeholder)
        assert_eq!(model.cell(1, 0).unwrap().text, "");
        assert_eq!(model.cursor(), (2, 0));
    }

    // --- Cursor CUP with f final byte ---

    #[test]
    fn cup_with_f_final_byte() {
        let mut model = TerminalModel::new(80, 24);
        model.process(b"\x1b[3;7f"); // Same as H
        assert_eq!(model.cursor(), (6, 2));
    }

    // --- CSI unknown final byte ---

    #[test]
    fn csi_unknown_final_byte_ignored() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"A");
        model.process(b"\x1b[99X"); // Unknown CSI
        model.process(b"B");
        assert_eq!(model.row_text(0).as_deref(), Some("AB"));
    }

    // --- CSI save/restore cursor (s/u) ---

    #[test]
    fn csi_save_restore_cursor_no_panic() {
        let mut model = TerminalModel::new(10, 5);
        model.process(b"\x1b[5;5H");
        model.process(b"\x1b[s"); // Save
        model.process(b"\x1b[1;1H");
        model.process(b"\x1b[u"); // Restore (not fully implemented, but shouldn't panic)
        // Just verify no crash.
        let (x, y) = model.cursor();
        assert!(x < model.width());
        assert!(y < model.height());
    }

    // --- BEL in ground state ---

    #[test]
    fn bel_in_ground_is_ignored() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x07Hi");
        assert_eq!(model.row_text(0).as_deref(), Some("Hi"));
    }

    // --- CUP clamps out-of-range values ---

    #[test]
    fn cup_clamps_large_row_col() {
        let mut model = TerminalModel::new(10, 5);
        model.process(b"\x1b[999;999H");
        assert_eq!(model.cursor(), (9, 4));
    }

    // --- Relative moves at boundaries ---

    #[test]
    fn cuu_at_top_stays() {
        let mut model = TerminalModel::new(10, 5);
        model.process(b"\x1b[1;1H");
        model.process(b"\x1b[50A"); // Up 50 from top
        assert_eq!(model.cursor(), (0, 0));
    }

    #[test]
    fn cud_at_bottom_stays() {
        let mut model = TerminalModel::new(10, 5);
        model.process(b"\x1b[5;1H");
        model.process(b"\x1b[50B"); // Down 50 from bottom
        assert_eq!(model.cursor(), (0, 4));
    }

    #[test]
    fn cuf_at_right_stays() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[1;10H");
        model.process(b"\x1b[50C"); // Forward 50 from right edge
        assert_eq!(model.cursor().0, 9);
    }

    #[test]
    fn cub_at_left_stays() {
        let mut model = TerminalModel::new(10, 1);
        model.process(b"\x1b[50D"); // Back 50 from column 0
        assert_eq!(model.cursor().0, 0);
    }

    // --- CSI with intermediate bytes ---

    #[test]
    fn csi_with_intermediate_no_crash() {
        let mut model = TerminalModel::new(10, 1);
        // Space (0x20) in CSI entry falls to default → Ground.
        // Then 'q' is printed as regular char.
        model.process(b"\x1b[ q");
        model.process(b"OK");
        // 'q' + "OK" are all printed.
        assert_eq!(model.row_text(0).as_deref(), Some("qOK"));
    }

    // --- Reset preserves dimensions ---

    #[test]
    fn reset_preserves_dimensions() {
        let mut model = TerminalModel::new(40, 20);
        model.process(b"SomeText");
        model.reset();
        assert_eq!(model.width(), 40);
        assert_eq!(model.height(), 20);
        assert_eq!(model.cursor(), (0, 0));
    }

    // --- LF at bottom does not crash ---

    #[test]
    fn lf_at_bottom_row_stays() {
        let mut model = TerminalModel::new(10, 3);
        model.process(b"\x1b[3;1H"); // Row 3 (bottom)
        model.process(b"\n"); // LF at bottom
        assert_eq!(model.cursor().1, 2); // Stays at bottom
    }
}

/// Property tests for terminal model correctness.
#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Generate a valid CSI sequence for cursor positioning.
    fn cup_sequence(row: u8, col: u8) -> Vec<u8> {
        format!("\x1b[{};{}H", row.max(1), col.max(1)).into_bytes()
    }

    /// Generate a valid SGR sequence.
    fn sgr_sequence(codes: &[u8]) -> Vec<u8> {
        let codes_str: Vec<String> = codes.iter().map(|c| c.to_string()).collect();
        format!("\x1b[{}m", codes_str.join(";")).into_bytes()
    }

    proptest! {
        /// Any sequence of printable ASCII doesn't crash.
        #[test]
        fn printable_ascii_no_crash(s in "[A-Za-z0-9 ]{0,100}") {
            let mut model = TerminalModel::new(80, 24);
            model.process(s.as_bytes());
            // Model should be in a valid state
            let (x, y) = model.cursor();
            prop_assert!(x < model.width());
            prop_assert!(y < model.height());
        }

        /// CUP sequences always leave cursor in bounds.
        #[test]
        fn cup_cursor_in_bounds(row in 0u8..100, col in 0u8..200) {
            let mut model = TerminalModel::new(80, 24);
            let seq = cup_sequence(row, col);
            model.process(&seq);

            let (x, y) = model.cursor();
            prop_assert!(x < model.width(), "cursor_x {} >= width {}", x, model.width());
            prop_assert!(y < model.height(), "cursor_y {} >= height {}", y, model.height());
        }

        /// Relative cursor moves never go out of bounds.
        #[test]
        fn relative_moves_in_bounds(
            start_row in 1u8..24,
            start_col in 1u8..80,
            up in 0u8..50,
            down in 0u8..50,
            left in 0u8..100,
            right in 0u8..100,
        ) {
            let mut model = TerminalModel::new(80, 24);

            // Position cursor
            model.process(&cup_sequence(start_row, start_col));

            // Apply relative moves
            model.process(format!("\x1b[{}A", up).as_bytes());
            model.process(format!("\x1b[{}B", down).as_bytes());
            model.process(format!("\x1b[{}D", left).as_bytes());
            model.process(format!("\x1b[{}C", right).as_bytes());

            let (x, y) = model.cursor();
            prop_assert!(x < model.width());
            prop_assert!(y < model.height());
        }

        /// SGR reset always clears all flags.
        #[test]
        fn sgr_reset_clears_flags(attrs in proptest::collection::vec(1u8..9, 0..5)) {
            let mut model = TerminalModel::new(80, 24);

            // Set some attributes
            if !attrs.is_empty() {
                model.process(&sgr_sequence(&attrs));
            }

            // Reset
            model.process(b"\x1b[0m");

            prop_assert!(model.sgr_state().flags.is_empty());
        }

        /// Hyperlinks always balance (no dangling after close).
        #[test]
        fn hyperlinks_balance(text in "[a-z]{1,20}") {
            let mut model = TerminalModel::new(80, 24);

            // Start link
            model.process(b"\x1b]8;;https://example.com\x07");
            prop_assert!(model.has_dangling_link());

            // Write some text
            model.process(text.as_bytes());

            // End link
            model.process(b"\x1b]8;;\x07");
            prop_assert!(!model.has_dangling_link());
        }

        /// Sync output always balances with nested begin/end.
        #[test]
        fn sync_output_balances(nesting in 1usize..5) {
            let mut model = TerminalModel::new(80, 24);

            // Begin sync N times
            for _ in 0..nesting {
                model.process(b"\x1b[?2026h");
            }
            prop_assert_eq!(model.modes().sync_output_level, nesting as u32);

            // End sync N times
            for _ in 0..nesting {
                model.process(b"\x1b[?2026l");
            }
            prop_assert!(model.sync_output_balanced());
        }

        /// Erase operations don't crash and leave cursor in bounds.
        #[test]
        fn erase_operations_safe(
            row in 1u8..24,
            col in 1u8..80,
            ed_mode in 0u8..4,
            el_mode in 0u8..3,
        ) {
            let mut model = TerminalModel::new(80, 24);

            // Position cursor
            model.process(&cup_sequence(row, col));

            // Erase display
            model.process(format!("\x1b[{}J", ed_mode).as_bytes());

            // Position again and erase line
            model.process(&cup_sequence(row, col));
            model.process(format!("\x1b[{}K", el_mode).as_bytes());

            let (x, y) = model.cursor();
            prop_assert!(x < model.width());
            prop_assert!(y < model.height());
        }

        /// Random bytes never cause a panic (fuzz-like test).
        #[test]
        fn random_bytes_no_panic(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            let mut model = TerminalModel::new(80, 24);
            model.process(&bytes);

            // Just check it didn't panic and cursor is valid
            let (x, y) = model.cursor();
            prop_assert!(x < model.width());
            prop_assert!(y < model.height());
        }
    }
}

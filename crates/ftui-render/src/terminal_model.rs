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
//! assert_eq!(model.cell(0, 0).char, 'H');
//! ```

use crate::cell::{CellAttrs, PackedRgba, StyleFlags};
use unicode_width::UnicodeWidthChar;

/// A single cell in the terminal model grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCell {
    // ... (existing code matches, just updating imports implies I need context for replace)
    /// Character content (default is space).
    pub ch: char,
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
            ch: ' ',
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
            ch,
            ..Default::default()
        }
    }
}

/// Current SGR (style) state for the terminal model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SgrState {
    pub fg: PackedRgba,
    pub bg: PackedRgba,
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
    /// Bytes processed (for debugging).
    bytes_processed: usize,
}

impl TerminalModel {
    /// Create a new terminal model with the given dimensions.
    pub fn new(width: usize, height: usize) -> Self {
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
            bytes_processed: 0,
        }
    }

    /// Get the terminal width.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Get the terminal height.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Get the cursor position as (x, y).
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_x, self.cursor_y)
    }

    /// Get the current SGR state.
    pub fn sgr_state(&self) -> &SgrState {
        &self.sgr
    }

    /// Get the current mode flags.
    pub fn modes(&self) -> &ModeFlags {
        &self.modes
    }

    /// Get the cell at (x, y). Returns None if out of bounds.
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
    pub fn current_cell(&self) -> Option<&ModelCell> {
        self.cell(self.cursor_x, self.cursor_y)
    }

    /// Get all cells as a slice.
    pub fn cells(&self) -> &[ModelCell] {
        &self.cells
    }

    /// Get a row of cells.
    pub fn row(&self, y: usize) -> Option<&[ModelCell]> {
        if y < self.height {
            let start = y * self.width;
            Some(&self.cells[start..start + self.width])
        } else {
            None
        }
    }

    /// Extract the text content of a row as a string (trimmed of trailing spaces).
    pub fn row_text(&self, y: usize) -> Option<String> {
        self.row(y).map(|cells| {
            let s: String = cells.iter().map(|c| c.ch).collect();
            s.trim_end().to_string()
        })
    }

    /// Get the URL for a link ID.
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
                self.parse_state = ParseState::Escape;
            }
            0x00..=0x1A | 0x1C..=0x1F => {
                // C0 controls (mostly ignored)
                self.handle_c0(b);
            }
            _ => {
                // Printable character
                self.put_char(b as char);
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

    fn put_char(&mut self, ch: char) {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);

        if self.cursor_x < self.width && self.cursor_y < self.height {
            let cell = &mut self.cells[self.cursor_y * self.width + self.cursor_x];
            cell.ch = ch;
            cell.fg = self.sgr.fg;
            cell.bg = self.sgr.bg;
            cell.attrs = CellAttrs::new(self.sgr.flags, self.current_link_id);
            cell.link_id = self.current_link_id;

            // Handle wide characters (clear the next cell if it exists)
            if width == 2 && self.cursor_x + 1 < self.width {
                let next_cell = &mut self.cells[self.cursor_y * self.width + self.cursor_x + 1];
                next_cell.ch = ' '; // Clear content
                next_cell.fg = self.sgr.fg; // Extend background color
                next_cell.bg = self.sgr.bg;
                next_cell.attrs = CellAttrs::NONE; // Clear attributes (or should they extend?)
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
            cell.ch = ' ';
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
                    x, y, actual.ch, exp.ch
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

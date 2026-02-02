#![forbid(unsafe_code)]

//! Simplified terminal model for testing presenter output.
//!
//! Parses ANSI escape sequences and updates an internal cell grid,
//! enabling verification that presenter output produces the expected
//! terminal state without needing a real terminal.
//!
//! # Supported Sequences
//! - SGR (Select Graphic Rendition): styles, colors (truecolor)
//! - CUP (Cursor Position): `CSI row ; col H`
//! - Cursor movement: `CSI n A/B/C/D`
//! - EL (Erase Line): `CSI n K`
//! - ED (Erase Display): `CSI n J`
//! - OSC 8 (Hyperlinks): open/close
//! - DEC synchronized output markers (ignored)
//!
//! # Example
//! ```
//! use ftui_harness::terminal_model::TerminalModel;
//!
//! let mut model = TerminalModel::new(80, 24);
//! model.feed(b"Hello\x1b[1;31m World\x1b[0m");
//! assert_eq!(model.char_at(0, 0), 'H');
//! assert_eq!(model.char_at(5, 0), ' ');
//! assert!(model.style_at(6, 0).bold);
//! ```

/// RGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Style state tracked by the terminal model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelStyle {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub reverse: bool,
    pub strikethrough: bool,
}

/// A single cell in the terminal model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCell {
    pub ch: char,
    pub style: ModelStyle,
    pub link: Option<String>,
}

impl Default for ModelCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: ModelStyle::default(),
            link: None,
        }
    }
}

/// Erase mode for EL/ED sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EraseMode {
    ToEnd,
    ToStart,
    All,
}

/// Internal parser state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParserState {
    Ground,
    Escape,
    Csi,
    Osc,
}

/// Simplified terminal model for testing.
pub struct TerminalModel {
    grid: Vec<Vec<ModelCell>>,
    cursor_x: u16,
    cursor_y: u16,
    current_style: ModelStyle,
    current_link: Option<String>,
    width: u16,
    height: u16,
    // Parser state
    state: ParserState,
    csi_params: Vec<u16>,
    csi_current: u16,
    osc_buffer: Vec<u8>,
}

impl TerminalModel {
    /// Create a new terminal model with the given dimensions.
    pub fn new(width: u16, height: u16) -> Self {
        let grid = (0..height)
            .map(|_| (0..width).map(|_| ModelCell::default()).collect())
            .collect();
        Self {
            grid,
            cursor_x: 0,
            cursor_y: 0,
            current_style: ModelStyle::default(),
            current_link: None,
            width,
            height,
            state: ParserState::Ground,
            csi_params: Vec::new(),
            csi_current: 0,
            osc_buffer: Vec::new(),
        }
    }

    /// Terminal width.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Terminal height.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Current cursor position.
    pub fn cursor(&self) -> (u16, u16) {
        (self.cursor_x, self.cursor_y)
    }

    /// Get the character at (x, y).
    pub fn char_at(&self, x: u16, y: u16) -> char {
        self.cell_at(x, y).map_or(' ', |c| c.ch)
    }

    /// Get the style at (x, y).
    pub fn style_at(&self, x: u16, y: u16) -> ModelStyle {
        self.cell_at(x, y)
            .map_or_else(ModelStyle::default, |c| c.style.clone())
    }

    /// Get the link at (x, y).
    pub fn link_at(&self, x: u16, y: u16) -> Option<String> {
        self.cell_at(x, y).and_then(|c| c.link.clone())
    }

    /// Get a cell reference.
    fn cell_at(&self, x: u16, y: u16) -> Option<&ModelCell> {
        self.grid
            .get(y as usize)
            .and_then(|row| row.get(x as usize))
    }

    /// Read a row as a string (trailing spaces trimmed).
    pub fn row_text(&self, y: u16) -> String {
        if let Some(row) = self.grid.get(y as usize) {
            let s: String = row.iter().map(|c| c.ch).collect();
            s.trim_end().to_string()
        } else {
            String::new()
        }
    }

    /// Read the entire screen as text.
    pub fn screen_text(&self) -> String {
        let mut lines: Vec<String> = (0..self.height).map(|y| self.row_text(y)).collect();
        // Trim trailing empty lines
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Dump model state for debugging.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for (y, row) in self.grid.iter().enumerate() {
            out.push_str(&format!("{y:3}| "));
            for cell in row {
                out.push(cell.ch);
            }
            out.push('\n');
        }
        out.push_str(&format!("Cursor: ({}, {})\n", self.cursor_x, self.cursor_y));
        out.push_str(&format!("Style: {:?}\n", self.current_style));
        out
    }

    /// Feed bytes to the terminal model.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.advance(byte);
        }
    }

    /// Feed a string to the terminal model.
    pub fn feed_str(&mut self, s: &str) {
        self.feed(s.as_bytes());
    }

    fn advance(&mut self, byte: u8) {
        match self.state {
            ParserState::Ground => self.ground(byte),
            ParserState::Escape => self.escape(byte),
            ParserState::Csi => self.csi(byte),
            ParserState::Osc => self.osc(byte),
        }
    }

    fn ground(&mut self, byte: u8) {
        match byte {
            0x1b => {
                self.state = ParserState::Escape;
            }
            0x0a => {
                // LF: move cursor down
                if self.cursor_y + 1 < self.height {
                    self.cursor_y += 1;
                }
            }
            0x0d => {
                // CR: move cursor to column 0
                self.cursor_x = 0;
            }
            0x08 => {
                // BS: move cursor left
                self.cursor_x = self.cursor_x.saturating_sub(1);
            }
            0x09 => {
                // TAB: advance to next 8-column tab stop
                self.cursor_x = ((self.cursor_x / 8) + 1) * 8;
                if self.cursor_x >= self.width {
                    self.cursor_x = self.width.saturating_sub(1);
                }
            }
            0x20..=0x7e => {
                self.put_char(byte as char);
            }
            0xc0..=0xff => {
                // Start of multi-byte UTF-8 - simplified: just treat as '?'
                self.put_char('?');
            }
            _ => {
                // Ignore other control chars
            }
        }
    }

    fn escape(&mut self, byte: u8) {
        match byte {
            b'[' => {
                self.state = ParserState::Csi;
                self.csi_params.clear();
                self.csi_current = 0;
            }
            b']' => {
                self.state = ParserState::Osc;
                self.osc_buffer.clear();
            }
            _ => {
                // Unknown escape, return to ground
                self.state = ParserState::Ground;
            }
        }
    }

    fn csi(&mut self, byte: u8) {
        match byte {
            b'0'..=b'9' => {
                self.csi_current = self.csi_current.saturating_mul(10) + (byte - b'0') as u16;
            }
            b';' => {
                self.csi_params.push(self.csi_current);
                self.csi_current = 0;
            }
            b'?' => {
                // Private mode prefix - ignore
            }
            b'A' => {
                self.csi_params.push(self.csi_current);
                let n = self.param(0, 1);
                self.cursor_y = self.cursor_y.saturating_sub(n);
                self.state = ParserState::Ground;
            }
            b'B' => {
                self.csi_params.push(self.csi_current);
                let n = self.param(0, 1);
                self.cursor_y = (self.cursor_y + n).min(self.height.saturating_sub(1));
                self.state = ParserState::Ground;
            }
            b'C' => {
                self.csi_params.push(self.csi_current);
                let n = self.param(0, 1);
                self.cursor_x = (self.cursor_x + n).min(self.width.saturating_sub(1));
                self.state = ParserState::Ground;
            }
            b'D' => {
                self.csi_params.push(self.csi_current);
                let n = self.param(0, 1);
                self.cursor_x = self.cursor_x.saturating_sub(n);
                self.state = ParserState::Ground;
            }
            b'H' | b'f' => {
                // CUP: Cursor Position
                self.csi_params.push(self.csi_current);
                let row = self.param(0, 1);
                let col = self.param(1, 1);
                self.cursor_y = row.saturating_sub(1).min(self.height.saturating_sub(1));
                self.cursor_x = col.saturating_sub(1).min(self.width.saturating_sub(1));
                self.state = ParserState::Ground;
            }
            b'J' => {
                // ED: Erase Display
                self.csi_params.push(self.csi_current);
                let mode = match self.param(0, 0) {
                    0 => EraseMode::ToEnd,
                    1 => EraseMode::ToStart,
                    _ => EraseMode::All,
                };
                self.erase_display(mode);
                self.state = ParserState::Ground;
            }
            b'K' => {
                // EL: Erase Line
                self.csi_params.push(self.csi_current);
                let mode = match self.param(0, 0) {
                    0 => EraseMode::ToEnd,
                    1 => EraseMode::ToStart,
                    _ => EraseMode::All,
                };
                self.erase_line(mode);
                self.state = ParserState::Ground;
            }
            b'm' => {
                // SGR: Select Graphic Rendition
                self.csi_params.push(self.csi_current);
                self.apply_sgr();
                self.state = ParserState::Ground;
            }
            b'h' | b'l' | b's' | b'u' => {
                // Mode set/reset, save/restore cursor - ignore
                self.state = ParserState::Ground;
            }
            _ => {
                // Unknown CSI final byte
                self.state = ParserState::Ground;
            }
        }
    }

    fn osc(&mut self, byte: u8) {
        match byte {
            0x07 => {
                // BEL terminates OSC
                self.process_osc();
                self.state = ParserState::Ground;
            }
            0x1b => {
                // ESC might be followed by \ to terminate OSC (ST)
                self.process_osc();
                self.state = ParserState::Escape;
            }
            _ => {
                self.osc_buffer.push(byte);
            }
        }
    }

    fn process_osc(&mut self) {
        let osc_str = String::from_utf8_lossy(&self.osc_buffer).to_string();
        // OSC 8 ; params ; url ST  - hyperlinks
        if let Some(rest) = osc_str.strip_prefix("8;") {
            // Find the URL after the params
            if let Some((_params, url)) = rest.split_once(';') {
                if url.is_empty() {
                    self.current_link = None;
                } else {
                    self.current_link = Some(url.to_string());
                }
            }
        }
    }

    fn param(&self, index: usize, default: u16) -> u16 {
        self.csi_params.get(index).copied().unwrap_or(default)
    }

    fn put_char(&mut self, ch: char) {
        let x = self.cursor_x as usize;
        let y = self.cursor_y as usize;
        if y < self.grid.len() && x < self.grid[y].len() {
            self.grid[y][x] = ModelCell {
                ch,
                style: self.current_style.clone(),
                link: self.current_link.clone(),
            };
        }
        self.cursor_x += 1;
        if self.cursor_x >= self.width {
            self.cursor_x = 0;
            if self.cursor_y + 1 < self.height {
                self.cursor_y += 1;
            }
        }
    }

    fn erase_line(&mut self, mode: EraseMode) {
        let y = self.cursor_y as usize;
        if y >= self.grid.len() {
            return;
        }
        let (start, end) = match mode {
            EraseMode::ToEnd => (self.cursor_x as usize, self.width as usize),
            EraseMode::ToStart => (0, self.cursor_x as usize + 1),
            EraseMode::All => (0, self.width as usize),
        };
        for x in start..end.min(self.grid[y].len()) {
            self.grid[y][x] = ModelCell::default();
        }
    }

    fn erase_display(&mut self, mode: EraseMode) {
        match mode {
            EraseMode::ToEnd => {
                // Erase from cursor to end of screen
                self.erase_line(EraseMode::ToEnd);
                for y in (self.cursor_y + 1) as usize..self.height as usize {
                    for cell in &mut self.grid[y] {
                        *cell = ModelCell::default();
                    }
                }
            }
            EraseMode::ToStart => {
                // Erase from start of screen to cursor
                for y in 0..self.cursor_y as usize {
                    for cell in &mut self.grid[y] {
                        *cell = ModelCell::default();
                    }
                }
                self.erase_line(EraseMode::ToStart);
            }
            EraseMode::All => {
                for row in &mut self.grid {
                    for cell in row {
                        *cell = ModelCell::default();
                    }
                }
            }
        }
    }

    fn apply_sgr(&mut self) {
        if self.csi_params.is_empty() || (self.csi_params.len() == 1 && self.csi_params[0] == 0) {
            self.current_style = ModelStyle::default();
            return;
        }

        let mut i = 0;
        while i < self.csi_params.len() {
            match self.csi_params[i] {
                0 => self.current_style = ModelStyle::default(),
                1 => self.current_style.bold = true,
                2 => self.current_style.dim = true,
                3 => self.current_style.italic = true,
                4 => self.current_style.underline = true,
                5 => self.current_style.blink = true,
                7 => self.current_style.reverse = true,
                9 => self.current_style.strikethrough = true,
                22 => {
                    self.current_style.bold = false;
                    self.current_style.dim = false;
                }
                23 => self.current_style.italic = false,
                24 => self.current_style.underline = false,
                25 => self.current_style.blink = false,
                27 => self.current_style.reverse = false,
                29 => self.current_style.strikethrough = false,
                38 => {
                    // Foreground: 38;2;r;g;b
                    if i + 4 < self.csi_params.len() && self.csi_params[i + 1] == 2 {
                        self.current_style.fg = Some(Rgb::new(
                            self.csi_params[i + 2] as u8,
                            self.csi_params[i + 3] as u8,
                            self.csi_params[i + 4] as u8,
                        ));
                        i += 4;
                    }
                }
                39 => self.current_style.fg = None,
                48 => {
                    // Background: 48;2;r;g;b
                    if i + 4 < self.csi_params.len() && self.csi_params[i + 1] == 2 {
                        self.current_style.bg = Some(Rgb::new(
                            self.csi_params[i + 2] as u8,
                            self.csi_params[i + 3] as u8,
                            self.csi_params[i + 4] as u8,
                        ));
                        i += 4;
                    }
                }
                49 => self.current_style.bg = None,
                _ => {}
            }
            i += 1;
        }
    }
}

/// Difference between expected and actual cell.
#[derive(Debug, Clone)]
pub struct CellDiff {
    pub x: u16,
    pub y: u16,
    pub expected: ModelCell,
    pub actual: ModelCell,
}

impl std::fmt::Display for CellDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "({}, {}): expected '{}' got '{}'",
            self.x, self.y, self.expected.ch, self.actual.ch
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_model_empty() {
        let m = TerminalModel::new(10, 5);
        assert_eq!(m.width(), 10);
        assert_eq!(m.height(), 5);
        assert_eq!(m.cursor(), (0, 0));
        assert_eq!(m.char_at(0, 0), ' ');
    }

    #[test]
    fn print_text() {
        let mut m = TerminalModel::new(20, 5);
        m.feed(b"Hello");
        assert_eq!(m.char_at(0, 0), 'H');
        assert_eq!(m.char_at(1, 0), 'e');
        assert_eq!(m.char_at(2, 0), 'l');
        assert_eq!(m.char_at(3, 0), 'l');
        assert_eq!(m.char_at(4, 0), 'o');
        assert_eq!(m.cursor(), (5, 0));
    }

    #[test]
    fn cursor_wraps_at_edge() {
        let mut m = TerminalModel::new(5, 3);
        m.feed(b"ABCDE");
        // After 5 chars in 5-wide terminal, cursor wraps
        assert_eq!(m.cursor(), (0, 1));
        assert_eq!(m.char_at(0, 0), 'A');
        assert_eq!(m.char_at(4, 0), 'E');
    }

    #[test]
    fn newline() {
        let mut m = TerminalModel::new(20, 5);
        m.feed(b"AB\nCD");
        assert_eq!(m.char_at(0, 0), 'A');
        assert_eq!(m.char_at(1, 0), 'B');
        // LF moves down, cursor_x stays
        assert_eq!(m.char_at(2, 1), 'C');
        assert_eq!(m.char_at(3, 1), 'D');
    }

    #[test]
    fn carriage_return() {
        let mut m = TerminalModel::new(20, 5);
        m.feed(b"Hello\r");
        assert_eq!(m.cursor(), (0, 0));
        m.feed(b"World");
        assert_eq!(m.row_text(0), "World");
    }

    #[test]
    fn crlf() {
        let mut m = TerminalModel::new(20, 5);
        m.feed(b"Line1\r\nLine2");
        assert_eq!(m.row_text(0), "Line1");
        assert_eq!(m.row_text(1), "Line2");
    }

    #[test]
    fn cursor_position_cup() {
        let mut m = TerminalModel::new(20, 10);
        m.feed(b"\x1b[5;10H");
        // CUP is 1-indexed
        assert_eq!(m.cursor(), (9, 4));
    }

    #[test]
    fn cursor_position_default() {
        let mut m = TerminalModel::new(20, 10);
        m.feed(b"\x1b[H");
        assert_eq!(m.cursor(), (0, 0));
    }

    #[test]
    fn cursor_movement() {
        let mut m = TerminalModel::new(20, 10);
        m.feed(b"\x1b[5;10H"); // row 5, col 10
        m.feed(b"\x1b[2A"); // up 2
        assert_eq!(m.cursor(), (9, 2));
        m.feed(b"\x1b[3B"); // down 3
        assert_eq!(m.cursor(), (9, 5));
        m.feed(b"\x1b[4C"); // right 4
        assert_eq!(m.cursor(), (13, 5));
        m.feed(b"\x1b[2D"); // left 2
        assert_eq!(m.cursor(), (11, 5));
    }

    #[test]
    fn cursor_movement_clamps() {
        let mut m = TerminalModel::new(10, 5);
        m.feed(b"\x1b[100A"); // up 100
        assert_eq!(m.cursor(), (0, 0));
        m.feed(b"\x1b[100B"); // down 100
        assert_eq!(m.cursor(), (0, 4));
        m.feed(b"\x1b[100D"); // left 100
        assert_eq!(m.cursor(), (0, 4));
        m.feed(b"\x1b[100C"); // right 100
        assert_eq!(m.cursor(), (9, 4));
    }

    #[test]
    fn sgr_bold() {
        let mut m = TerminalModel::new(20, 5);
        m.feed(b"\x1b[1mBold\x1b[0m");
        assert!(m.style_at(0, 0).bold);
        assert!(m.style_at(3, 0).bold);
    }

    #[test]
    fn sgr_reset() {
        let mut m = TerminalModel::new(20, 5);
        m.feed(b"\x1b[1;3mBI\x1b[0mN");
        assert!(m.style_at(0, 0).bold);
        assert!(m.style_at(0, 0).italic);
        assert!(!m.style_at(2, 0).bold);
        assert!(!m.style_at(2, 0).italic);
    }

    #[test]
    fn sgr_truecolor_fg() {
        let mut m = TerminalModel::new(20, 5);
        m.feed(b"\x1b[38;2;255;0;128mX\x1b[0m");
        let style = m.style_at(0, 0);
        assert_eq!(style.fg, Some(Rgb::new(255, 0, 128)));
    }

    #[test]
    fn sgr_truecolor_bg() {
        let mut m = TerminalModel::new(20, 5);
        m.feed(b"\x1b[48;2;10;20;30mX\x1b[0m");
        let style = m.style_at(0, 0);
        assert_eq!(style.bg, Some(Rgb::new(10, 20, 30)));
    }

    #[test]
    fn sgr_combined() {
        let mut m = TerminalModel::new(20, 5);
        m.feed(b"\x1b[1;3;4;38;2;255;128;0mX\x1b[0m");
        let style = m.style_at(0, 0);
        assert!(style.bold);
        assert!(style.italic);
        assert!(style.underline);
        assert_eq!(style.fg, Some(Rgb::new(255, 128, 0)));
    }

    #[test]
    fn sgr_selective_reset() {
        let mut m = TerminalModel::new(20, 5);
        m.feed(b"\x1b[1;3mX\x1b[23mY");
        let x_style = m.style_at(0, 0);
        assert!(x_style.bold);
        assert!(x_style.italic);
        let y_style = m.style_at(1, 0);
        assert!(y_style.bold);
        assert!(!y_style.italic);
    }

    #[test]
    fn erase_line_to_end() {
        let mut m = TerminalModel::new(10, 3);
        m.feed(b"ABCDEFGHIJ");
        m.feed(b"\x1b[1;4H"); // Position at col 4
        m.feed(b"\x1b[0K"); // Erase to end
        assert_eq!(m.row_text(0), "ABC");
    }

    #[test]
    fn erase_line_to_start() {
        let mut m = TerminalModel::new(10, 3);
        m.feed(b"ABCDEFGHIJ");
        m.feed(b"\x1b[1;4H"); // Position at col 4
        m.feed(b"\x1b[1K"); // Erase to start (including cursor)
        assert_eq!(m.char_at(0, 0), ' ');
        assert_eq!(m.char_at(1, 0), ' ');
        assert_eq!(m.char_at(2, 0), ' ');
        assert_eq!(m.char_at(3, 0), ' ');
        assert_eq!(m.char_at(4, 0), 'E');
    }

    #[test]
    fn erase_line_all() {
        let mut m = TerminalModel::new(10, 3);
        m.feed(b"ABCDEFGHIJ");
        m.feed(b"\x1b[1;4H");
        m.feed(b"\x1b[2K"); // Erase entire line
        assert_eq!(m.row_text(0), "");
    }

    #[test]
    fn erase_display_to_end() {
        let mut m = TerminalModel::new(10, 3);
        m.feed(b"Line1     ");
        m.feed(b"Line2     ");
        m.feed(b"Line3     ");
        m.feed(b"\x1b[2;1H"); // row 2, col 1
        m.feed(b"\x1b[0J"); // Erase from cursor to end of display
        assert_eq!(m.row_text(0), "Line1");
        assert_eq!(m.row_text(1), "");
        assert_eq!(m.row_text(2), "");
    }

    #[test]
    fn erase_display_all() {
        let mut m = TerminalModel::new(10, 3);
        m.feed(b"XXXXXXXXXX");
        m.feed(b"YYYYYYYYYY");
        m.feed(b"\x1b[2J");
        assert_eq!(m.screen_text(), "");
    }

    #[test]
    fn osc8_hyperlink() {
        let mut m = TerminalModel::new(30, 3);
        // OSC 8 ; ; url BEL  text  OSC 8 ; ; BEL
        m.feed(b"\x1b]8;;https://example.com\x07Link\x1b]8;;\x07");
        assert_eq!(m.char_at(0, 0), 'L');
        assert_eq!(m.link_at(0, 0), Some("https://example.com".to_string()));
        assert_eq!(m.link_at(3, 0), Some("https://example.com".to_string()));
        // After link close, no link
        assert_eq!(m.link_at(4, 0), None);
    }

    #[test]
    fn osc8_with_st_terminator() {
        let mut m = TerminalModel::new(30, 3);
        // OSC 8 ; ; url ESC \ text OSC 8 ; ; ESC \
        m.feed(b"\x1b]8;;http://test.com\x1b\\Link\x1b]8;;\x1b\\");
        assert_eq!(m.link_at(0, 0), Some("http://test.com".to_string()));
    }

    #[test]
    fn screen_text_trims() {
        let mut m = TerminalModel::new(10, 3);
        m.feed(b"Hello");
        let text = m.screen_text();
        assert_eq!(text, "Hello");
    }

    #[test]
    fn dump_format() {
        let mut m = TerminalModel::new(5, 2);
        m.feed(b"Hi");
        let dump = m.dump();
        assert!(dump.contains("Hi"));
        assert!(dump.contains("Cursor:"));
    }

    #[test]
    fn tab_stop() {
        let mut m = TerminalModel::new(20, 3);
        m.feed(b"A\tB");
        assert_eq!(m.char_at(0, 0), 'A');
        assert_eq!(m.char_at(8, 0), 'B');
    }

    #[test]
    fn backspace() {
        let mut m = TerminalModel::new(20, 3);
        m.feed(b"AB\x08C");
        // After AB, cursor at 2. BS moves to 1. C overwrites at 1.
        assert_eq!(m.char_at(0, 0), 'A');
        assert_eq!(m.char_at(1, 0), 'C');
    }

    #[test]
    fn feed_str_convenience() {
        let mut m = TerminalModel::new(20, 3);
        m.feed_str("Hello");
        assert_eq!(m.row_text(0), "Hello");
    }

    #[test]
    fn sgr_all_attributes() {
        let mut m = TerminalModel::new(20, 3);
        m.feed(b"\x1b[1;2;3;4;5;7;9mX\x1b[0m");
        let s = m.style_at(0, 0);
        assert!(s.bold);
        assert!(s.dim);
        assert!(s.italic);
        assert!(s.underline);
        assert!(s.blink);
        assert!(s.reverse);
        assert!(s.strikethrough);
    }

    #[test]
    fn sgr_reset_individual() {
        let mut m = TerminalModel::new(20, 3);
        m.feed(b"\x1b[1;3;4;9mX\x1b[22;23;24;29mY");
        let x = m.style_at(0, 0);
        assert!(x.bold);
        assert!(x.italic);
        assert!(x.underline);
        assert!(x.strikethrough);
        let y = m.style_at(1, 0);
        assert!(!y.bold);
        assert!(!y.italic);
        assert!(!y.underline);
        assert!(!y.strikethrough);
    }

    #[test]
    fn sgr_default_fg_bg() {
        let mut m = TerminalModel::new(20, 3);
        m.feed(b"\x1b[38;2;255;0;0mR\x1b[39mX");
        assert_eq!(m.style_at(0, 0).fg, Some(Rgb::new(255, 0, 0)));
        assert_eq!(m.style_at(1, 0).fg, None);

        m.feed(b"\x1b[48;2;0;255;0mG\x1b[49mX");
        assert_eq!(m.style_at(2, 0).bg, Some(Rgb::new(0, 255, 0)));
        assert_eq!(m.style_at(3, 0).bg, None);
    }

    #[test]
    fn multiple_lines_rendering() {
        let mut m = TerminalModel::new(20, 5);
        // Simulate presenter output: position and write each line
        m.feed(b"\x1b[1;1HLine 1");
        m.feed(b"\x1b[2;1HLine 2");
        m.feed(b"\x1b[3;1HLine 3");
        assert_eq!(m.row_text(0), "Line 1");
        assert_eq!(m.row_text(1), "Line 2");
        assert_eq!(m.row_text(2), "Line 3");
    }

    #[test]
    fn styled_text_rendering() {
        let mut m = TerminalModel::new(30, 3);
        // Red bold text followed by normal
        m.feed(b"\x1b[1;38;2;255;0;0mERROR\x1b[0m: something");
        assert!(m.style_at(0, 0).bold);
        assert_eq!(m.style_at(0, 0).fg, Some(Rgb::new(255, 0, 0)));
        assert!(!m.style_at(5, 0).bold);
        assert_eq!(m.style_at(5, 0).fg, None);
        assert_eq!(m.row_text(0), "ERROR: something");
    }
}

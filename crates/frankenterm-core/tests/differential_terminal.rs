use frankenterm_core::{
    Action, Cell, Color, Cursor, Grid, Modes, Parser, SavedCursor, Scrollback, SgrFlags,
    translate_charset,
};
use ftui_pty::virtual_terminal::VirtualTerminal;

const KNOWN_MISMATCHES_FIXTURE: &str =
    include_str!("../../../tests/fixtures/vt-conformance/differential/known_mismatches.tsv");

/// Normalized RGB color for comparison between core and reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgb(u8, u8, u8);

/// Normalized per-cell style for differential comparison.
///
/// Maps both core `SgrAttrs` and reference `CellStyle` to a common representation
/// so we can detect SGR attribute mismatches between the two implementations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct NormalizedStyle {
    fg: Option<Rgb>,
    bg: Option<Rgb>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
}

/// Standard ANSI color palette (indices 0-7) as RGB.
const ANSI_COLORS: [Rgb; 8] = [
    Rgb(0, 0, 0),       // Black
    Rgb(170, 0, 0),     // Red
    Rgb(0, 170, 0),     // Green
    Rgb(170, 170, 0),   // Yellow
    Rgb(0, 0, 170),     // Blue
    Rgb(170, 0, 170),   // Magenta
    Rgb(0, 170, 170),   // Cyan
    Rgb(170, 170, 170), // White
];

/// Bright ANSI color palette (indices 8-15) as RGB.
const BRIGHT_COLORS: [Rgb; 8] = [
    Rgb(85, 85, 85),    // Bright Black
    Rgb(255, 85, 85),   // Bright Red
    Rgb(85, 255, 85),   // Bright Green
    Rgb(255, 255, 85),  // Bright Yellow
    Rgb(85, 85, 255),   // Bright Blue
    Rgb(255, 85, 255),  // Bright Magenta
    Rgb(85, 255, 255),  // Bright Cyan
    Rgb(255, 255, 255), // Bright White
];

/// Resolve a 256-color index to RGB using the standard xterm palette.
fn color_256_to_rgb(idx: u8) -> Rgb {
    match idx {
        0..=7 => ANSI_COLORS[idx as usize],
        8..=15 => BRIGHT_COLORS[(idx - 8) as usize],
        16..=231 => {
            let n = idx - 16;
            let b = n % 6;
            let g = (n / 6) % 6;
            let r = n / 36;
            let to_rgb = |v: u8| if v == 0 { 0u8 } else { 55 + 40 * v };
            Rgb(to_rgb(r), to_rgb(g), to_rgb(b))
        }
        232..=255 => {
            let v = 8 + 10 * (idx - 232);
            Rgb(v, v, v)
        }
    }
}

/// Resolve a core `Color` enum to an `Option<Rgb>`.
fn resolve_core_color(color: &Color) -> Option<Rgb> {
    match color {
        Color::Default => None,
        Color::Named(idx) => {
            let i = *idx as usize;
            if i < 8 {
                Some(ANSI_COLORS[i])
            } else if i < 16 {
                Some(BRIGHT_COLORS[i - 8])
            } else {
                None
            }
        }
        Color::Indexed(idx) => Some(color_256_to_rgb(*idx)),
        Color::Rgb(r, g, b) => Some(Rgb(*r, *g, *b)),
    }
}

/// Build a `NormalizedStyle` from core `SgrAttrs`.
fn normalize_core_style(attrs: &frankenterm_core::SgrAttrs) -> NormalizedStyle {
    NormalizedStyle {
        fg: resolve_core_color(&attrs.fg),
        bg: resolve_core_color(&attrs.bg),
        bold: attrs.flags.contains(SgrFlags::BOLD),
        dim: attrs.flags.contains(SgrFlags::DIM),
        italic: attrs.flags.contains(SgrFlags::ITALIC),
        underline: attrs.flags.contains(SgrFlags::UNDERLINE),
        blink: attrs.flags.contains(SgrFlags::BLINK),
        inverse: attrs.flags.contains(SgrFlags::INVERSE),
        hidden: attrs.flags.contains(SgrFlags::HIDDEN),
        strikethrough: attrs.flags.contains(SgrFlags::STRIKETHROUGH),
    }
}

/// Build a `NormalizedStyle` from reference `CellStyle`.
fn normalize_ref_style(style: &ftui_pty::virtual_terminal::CellStyle) -> NormalizedStyle {
    NormalizedStyle {
        fg: style.fg.map(|c| Rgb(c.r, c.g, c.b)),
        bg: style.bg.map(|c| Rgb(c.r, c.g, c.b)),
        bold: style.bold,
        dim: style.dim,
        italic: style.italic,
        underline: style.underline,
        blink: style.blink,
        inverse: style.reverse,
        hidden: style.hidden,
        strikethrough: style.strikethrough,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalSnapshot {
    screen_text: String,
    cursor_row: u16,
    cursor_col: u16,
    /// Per-row, per-visible-column style (skipping wide continuations).
    cell_styles: Vec<Vec<NormalizedStyle>>,
}

#[derive(Debug)]
struct CoreTerminalHarness {
    parser: Parser,
    grid: Grid,
    cursor: Cursor,
    scrollback: Scrollback,
    modes: Modes,
    last_printed: Option<char>,
    saved_cursor: SavedCursor,
    cols: u16,
    rows: u16,
}

impl CoreTerminalHarness {
    fn new(cols: u16, rows: u16) -> Self {
        assert!(cols > 0, "cols must be > 0");
        assert!(rows > 0, "rows must be > 0");
        Self {
            parser: Parser::new(),
            grid: Grid::new(cols, rows),
            cursor: Cursor::new(cols, rows),
            scrollback: Scrollback::new(512),
            modes: Modes::new(),
            last_printed: None,
            saved_cursor: SavedCursor::default(),
            cols,
            rows,
        }
    }

    fn feed_bytes(&mut self, bytes: &[u8]) {
        for action in self.parser.feed(bytes) {
            self.apply_action(action);
        }
    }

    fn apply_action(&mut self, action: Action) {
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
                self.cursor
                    .move_to(row, self.cursor.col, self.rows, self.cols);
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
                self.cursor.move_to(0, 0, self.rows, self.cols);
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
            Action::EraseChars(count) => {
                self.grid.erase_chars(
                    self.cursor.row,
                    self.cursor.col,
                    count,
                    self.cursor.attrs.bg,
                );
                self.cursor.pending_wrap = false;
            }
            Action::CursorPosition { row, col } => {
                self.cursor.move_to(row, col, self.rows, self.cols);
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
                        // DECOM on: home cursor to top-left of scroll region.
                        self.cursor.row = self.cursor.scroll_top();
                        self.cursor.col = 0;
                        self.cursor.pending_wrap = false;
                    }
                }
            }
            Action::DecRst(params) => {
                for &p in &params {
                    self.modes.set_dec_mode(p, false);
                    if p == 6 {
                        // DECOM off: home cursor to absolute (0,0).
                        self.cursor.row = 0;
                        self.cursor.col = 0;
                        self.cursor.pending_wrap = false;
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
            Action::RestoreCursor => {
                self.saved_cursor.restore(&mut self.cursor);
            }
            Action::Index => {
                // ESC D: same as LF
                self.apply_newline();
            }
            Action::ReverseIndex => {
                if self.cursor.row <= self.cursor.scroll_top() {
                    self.grid.scroll_down(
                        self.cursor.scroll_top(),
                        self.cursor.scroll_bottom(),
                        1,
                        self.cursor.attrs.bg,
                    );
                } else {
                    self.cursor.move_up(1);
                }
            }
            Action::NextLine => {
                self.cursor.carriage_return();
                self.apply_newline();
            }
            Action::FullReset => {
                self.grid = Grid::new(self.cols, self.rows);
                self.cursor = Cursor::new(self.cols, self.rows);
                self.scrollback = Scrollback::new(512);
                self.modes = Modes::new();
                self.last_printed = None;
                self.saved_cursor = SavedCursor::default();
            }
            Action::SetTitle(_) | Action::HyperlinkStart(_) | Action::HyperlinkEnd => {}
            Action::SetTabStop => {
                self.cursor.set_tab_stop();
                self.cursor.pending_wrap = false;
            }
            Action::ClearTabStop(mode) => {
                match mode {
                    0 => self.cursor.clear_tab_stop(),
                    3 | 5 => self.cursor.clear_all_tab_stops(),
                    _ => {}
                }
                self.cursor.pending_wrap = false;
            }
            Action::BackTab(count) => {
                for _ in 0..count {
                    self.cursor.col = self.cursor.prev_tab_stop();
                }
                self.cursor.pending_wrap = false;
            }
            // Keypad mode toggles do not affect baseline grid snapshot output.
            Action::ApplicationKeypad | Action::NormalKeypad => {}
            Action::ScreenAlignment => {
                // DECALN: fill screen with 'E', reset margins, cursor to origin.
                self.grid.fill_all('E');
                self.cursor.reset_scroll_region(self.rows);
                self.cursor.move_to(0, 0, self.rows, self.cols);
            }
            Action::RepeatChar(count) => {
                // REP: repeat the last printed character `count` times.
                if let Some(ch) = self.last_printed {
                    for _ in 0..count {
                        self.apply_print(ch);
                    }
                }
            }
            Action::SetCursorShape(_) => {}
            Action::SoftReset => {
                // DECSTR: reset modes, attrs, charset, cursor visibility.
                self.modes.reset();
                self.cursor.attrs = frankenterm_core::SgrAttrs::default();
                self.cursor.reset_charset();
                self.cursor.visible = true;
                self.cursor.pending_wrap = false;
                self.cursor.reset_scroll_region(self.rows);
            }
            Action::EraseScrollback => {}
            Action::FocusIn | Action::FocusOut => {}
            Action::PasteStart | Action::PasteEnd => {}
            Action::DeviceAttributes
            | Action::DeviceAttributesSecondary
            | Action::DeviceStatusReport
            | Action::CursorPositionReport => {}
            Action::DesignateCharset { slot, charset } => {
                self.cursor.designate_charset(slot, charset);
            }
            Action::SingleShift2 => {
                self.cursor.single_shift = Some(2);
            }
            Action::SingleShift3 => {
                self.cursor.single_shift = Some(3);
            }
            Action::MouseEvent { .. } => {}
            Action::Escape(_) => {
                // Remaining escape actions are intentionally left unsupported in the
                // baseline harness and tracked via known-mismatch fixtures.
            }
        }
    }

    fn apply_print(&mut self, ch: char) {
        // Apply charset translation (DEC Graphics, etc.).
        let charset = self.cursor.effective_charset();
        let ch = translate_charset(ch, charset);
        self.cursor.consume_single_shift();
        self.last_printed = Some(ch);

        if self.cursor.pending_wrap {
            if self.modes.autowrap() {
                self.wrap_to_next_line();
            } else {
                // No autowrap: stay at last column
                self.cursor.col = self.cols.saturating_sub(1);
                self.cursor.pending_wrap = false;
            }
        }

        let width = Cell::display_width(ch);
        if width == 0 {
            return;
        }

        if width == 2 && self.cursor.col + 1 >= self.cols && self.modes.autowrap() {
            self.wrap_to_next_line();
        }

        // IRM: insert mode — shift existing chars right before writing
        if self.modes.insert_mode() {
            let shift = u16::from(width);
            self.grid.insert_chars(
                self.cursor.row,
                self.cursor.col,
                shift,
                self.cursor.attrs.bg,
            );
        }

        let written =
            self.grid
                .write_printable(self.cursor.row, self.cursor.col, ch, self.cursor.attrs);
        if written == 0 {
            return;
        }

        if self.cursor.col + u16::from(written) >= self.cols {
            if self.modes.autowrap() {
                self.cursor.pending_wrap = true;
            } else {
                // No autowrap: clamp to last column
                self.cursor.col = self.cols.saturating_sub(1);
                self.cursor.pending_wrap = false;
            }
        } else {
            self.cursor.col += u16::from(written);
            self.cursor.pending_wrap = false;
        }
    }

    fn apply_newline(&mut self) {
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

    fn wrap_to_next_line(&mut self) {
        self.cursor.col = 0;
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

    fn snapshot(&self) -> TerminalSnapshot {
        let mut cell_styles = Vec::with_capacity(self.rows as usize);
        for row in 0..self.rows {
            let mut row_styles = Vec::new();
            for col in 0..self.cols {
                if let Some(cell) = self.grid.cell(row, col) {
                    if cell.is_wide_continuation() {
                        continue;
                    }
                    row_styles.push(normalize_core_style(&cell.attrs));
                } else {
                    row_styles.push(NormalizedStyle::default());
                }
            }
            cell_styles.push(row_styles);
        }
        TerminalSnapshot {
            screen_text: self.screen_text(),
            cursor_row: self.cursor.row,
            cursor_col: self.cursor.col,
            cell_styles,
        }
    }

    fn screen_text(&self) -> String {
        (0..self.rows)
            .map(|row| {
                let mut line = String::with_capacity(self.cols as usize);
                for col in 0..self.cols {
                    if let Some(cell) = self.grid.cell(row, col) {
                        if cell.is_wide_continuation() {
                            continue; // skip continuation cells of wide chars
                        }
                        line.push(cell.content());
                    } else {
                        line.push(' ');
                    }
                }
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug)]
struct SupportedFixture {
    id: &'static str,
    cols: u16,
    rows: u16,
    bytes: &'static [u8],
}

#[derive(Debug)]
struct KnownMismatchFixture {
    id: String,
    cols: u16,
    rows: u16,
    bytes: Vec<u8>,
    root_cause: String,
}

fn run_core_snapshot(input: &[u8], cols: u16, rows: u16) -> TerminalSnapshot {
    let mut harness = CoreTerminalHarness::new(cols, rows);
    harness.feed_bytes(input);
    harness.snapshot()
}

fn run_reference_snapshot(input: &[u8], cols: u16, rows: u16) -> TerminalSnapshot {
    let mut vt = VirtualTerminal::new(cols, rows);
    vt.feed(input);
    let (cursor_col, cursor_row) = vt.cursor();
    let mut cell_styles = Vec::with_capacity(rows as usize);
    for row in 0..rows {
        let mut row_styles = Vec::new();
        for col in 0..cols {
            if let Some(cell) = vt.cell_at(col, row) {
                // Skip wide-char continuation cells (NUL char in reference)
                if cell.ch == '\0' {
                    continue;
                }
                row_styles.push(normalize_ref_style(&cell.style));
            } else {
                row_styles.push(NormalizedStyle::default());
            }
        }
        cell_styles.push(row_styles);
    }
    TerminalSnapshot {
        screen_text: vt.screen_text(),
        cursor_row,
        cursor_col,
        cell_styles,
    }
}

fn supported_fixtures() -> Vec<SupportedFixture> {
    vec![
        SupportedFixture {
            id: "plain_ascii",
            cols: 20,
            rows: 4,
            bytes: b"hello",
        },
        SupportedFixture {
            id: "newline_preserves_column",
            cols: 20,
            rows: 4,
            bytes: b"hi\nthere",
        },
        SupportedFixture {
            id: "carriage_return_overwrite",
            cols: 20,
            rows: 4,
            bytes: b"ABCDE\rZ",
        },
        SupportedFixture {
            id: "tab_to_default_stop",
            cols: 20,
            rows: 4,
            bytes: b"A\tB",
        },
        SupportedFixture {
            id: "backspace_overwrite",
            cols: 20,
            rows: 4,
            bytes: b"abc\x08d",
        },
        SupportedFixture {
            id: "csi_cup_reposition",
            cols: 10,
            rows: 3,
            bytes: b"Hello\x1b[2;3HX",
        },
        SupportedFixture {
            id: "csi_erase_line_right",
            cols: 10,
            rows: 3,
            bytes: b"ABCDE\x1b[1;4H\x1b[0K",
        },
        SupportedFixture {
            id: "csi_erase_display",
            cols: 10,
            rows: 3,
            bytes: b"AB\x1b[2JZ",
        },
        SupportedFixture {
            id: "csi_cub_left",
            cols: 10,
            rows: 3,
            bytes: b"abc\x1b[2DZ",
        },
        SupportedFixture {
            id: "csi_cursor_relative_moves",
            cols: 10,
            rows: 3,
            bytes: b"abc\x1b[1;1H\x1b[2C\x1b[1B\x1b[1D\x1b[1AX",
        },
        SupportedFixture {
            id: "csi_cha_column_absolute",
            cols: 10,
            rows: 3,
            bytes: b"ABCDE\x1b[1GZ",
        },
        SupportedFixture {
            id: "csi_cnl_next_line",
            cols: 10,
            rows: 3,
            bytes: b"abc\x1b[2EZ",
        },
        SupportedFixture {
            id: "csi_cpl_prev_line",
            cols: 10,
            rows: 3,
            bytes: b"\x1b[3;5Habc\x1b[2FZ",
        },
        SupportedFixture {
            id: "csi_vpa_row_absolute",
            cols: 10,
            rows: 4,
            bytes: b"ABCDE\x1b[3dZ",
        },
        SupportedFixture {
            id: "csi_scroll_up",
            cols: 10,
            rows: 3,
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[1S",
        },
        SupportedFixture {
            id: "csi_scroll_down",
            cols: 10,
            rows: 3,
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[1T",
        },
        SupportedFixture {
            id: "csi_scroll_region_and_scroll",
            cols: 10,
            rows: 5,
            bytes:
                b"\x1b[1;1HAAAA\x1b[2;1HBBBB\x1b[3;1HCCCC\x1b[4;1HDDDD\x1b[5;1HEEEE\x1b[2;4r\x1b[1S",
        },
        SupportedFixture {
            id: "csi_ich_insert_chars",
            cols: 10,
            rows: 3,
            bytes: b"ABCDE\x1b[1G\x1b[2@Z",
        },
        SupportedFixture {
            id: "csi_dch_delete_chars",
            cols: 10,
            rows: 3,
            bytes: b"ABCDE\x1b[2G\x1b[2P",
        },
        SupportedFixture {
            id: "csi_ech_erase_chars",
            cols: 10,
            rows: 3,
            bytes: b"ABCDE\x1b[2G\x1b[2X",
        },
        SupportedFixture {
            id: "csi_il_insert_lines",
            cols: 5,
            rows: 3,
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[2;1H\x1b[1L",
        },
        SupportedFixture {
            id: "csi_dl_delete_lines",
            cols: 5,
            rows: 3,
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[2;1H\x1b[1M",
        },
        SupportedFixture {
            id: "csi_rep_repeat_char",
            cols: 10,
            rows: 3,
            bytes: b"A\x1b[3b",
        },
        SupportedFixture {
            id: "csi_decstr_soft_reset",
            cols: 10,
            rows: 3,
            bytes: b"\x1b[1mABC\x1b[!pD",
        },
        // ── Save / Restore Cursor ────────────────────────────────────
        SupportedFixture {
            id: "save_restore_basic",
            cols: 10,
            rows: 3,
            // Write "AB", save cursor at (0,2), move to (1,0) write "CD", restore, write "E"
            bytes: b"AB\x1b7\x1b[2;1HCD\x1b8E",
        },
        SupportedFixture {
            id: "save_restore_after_scroll",
            cols: 5,
            rows: 3,
            // Fill 3 rows, save cursor at (2,0), scroll up 1, restore → cursor clamped
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[1;1H\x1b7\x1b[1SX\x1b8Y",
        },
        SupportedFixture {
            id: "save_restore_roundtrip_position",
            cols: 10,
            rows: 4,
            // Move to (2,5), save, move to (0,0), write "Z", restore, write "W"
            bytes: b"\x1b[3;6H\x1b7\x1b[1;1HZ\x1b8W",
        },
        // ── Scroll region content preservation ───────────────────────
        SupportedFixture {
            id: "scroll_region_su_preserves_above",
            cols: 5,
            rows: 5,
            // Fill 5 rows (A-E), set region 2-4, scroll up 1 → row 0 unchanged
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE\x1b[2;4r\x1b[1S",
        },
        SupportedFixture {
            id: "scroll_region_sd_preserves_below",
            cols: 5,
            rows: 5,
            // Fill 5 rows (A-E), set region 2-4, scroll down 1 → row 4 unchanged
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE\x1b[2;4r\x1b[1T",
        },
        SupportedFixture {
            id: "scroll_region_il_middle",
            cols: 5,
            rows: 5,
            // Fill 5 rows, set region 2-4, cursor to row 2, insert 1 line
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE\x1b[2;4r\x1b[3;1H\x1b[1L",
        },
        SupportedFixture {
            id: "scroll_region_dl_middle",
            cols: 5,
            rows: 5,
            // Fill 5 rows, set region 2-4, cursor to row 2, delete 1 line
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE\x1b[2;4r\x1b[3;1H\x1b[1M",
        },
        // ── Erase operations ─────────────────────────────────────────
        SupportedFixture {
            id: "ed_below_preserves_above",
            cols: 5,
            rows: 3,
            // Fill 3 rows, move to (1,2), ED 0 (erase below) → row 0 intact
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[2;3H\x1b[0J",
        },
        SupportedFixture {
            id: "ed_above_preserves_below",
            cols: 5,
            rows: 3,
            // Fill 3 rows, move to (1,2), ED 1 (erase above) → row 2 intact
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[2;3H\x1b[1J",
        },
        SupportedFixture {
            id: "el_modes",
            cols: 10,
            rows: 3,
            // Row 0: "ABCDE" + EL 0 at col 2 → "AB"
            // Row 1: "FGHIJ" + EL 1 at col 2 → "   IJ"
            // Row 2: "KLMNO" + EL 2 → blank
            bytes: b"ABCDE\r\nFGHIJ\r\nKLMNO\x1b[1;3H\x1b[0K\x1b[2;3H\x1b[1K\x1b[3;1H\x1b[2K",
        },
        // ── Insert/Delete/Erase chars edge cases ─────────────────────
        SupportedFixture {
            id: "ich_pushes_off_edge",
            cols: 5,
            rows: 3,
            // Write "ABCDE", CUP(1,1), insert 2 chars, write "XY"
            bytes: b"ABCDE\x1b[1;1H\x1b[2@XY",
        },
        SupportedFixture {
            id: "dch_pulls_from_right",
            cols: 10,
            rows: 3,
            // Write "ABCDEFGH", CUP(1,3), delete 2 → "ABEFGH" + 2 blanks
            bytes: b"ABCDEFGH\x1b[1;3H\x1b[2P",
        },
        SupportedFixture {
            id: "ech_no_cursor_move",
            cols: 10,
            rows: 3,
            // Write "ABCDE", CUP(1,2), erase 3, write "X" → cursor stayed at col 1
            bytes: b"ABCDE\x1b[1;2H\x1b[3XX",
        },
        // ── Auto-wrap + scroll ───────────────────────────────────────
        SupportedFixture {
            id: "autowrap_at_right_edge",
            cols: 5,
            rows: 3,
            // Write exactly 6 chars → first 5 on row 0, 6th wraps to row 1
            bytes: b"ABCDEF",
        },
        SupportedFixture {
            id: "autowrap_fills_and_scrolls",
            cols: 5,
            rows: 3,
            // Write 16 chars → fills 3 rows, then scrolls, last char on row 2
            bytes: b"AAAAABBBBBCCCCCX",
        },
        SupportedFixture {
            id: "rep_wraps_across_lines",
            cols: 5,
            rows: 3,
            // Write "A", move to col 3, write "B", REP 4 → wraps to next line
            bytes: b"\x1b[1;4HB\x1b[4b",
        },
        // ── Index / Reverse Index / Next Line ────────────────────────
        SupportedFixture {
            id: "index_at_bottom_scrolls",
            cols: 5,
            rows: 3,
            // Fill 3 rows, CUP to (2,0), ESC D → scroll up
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[3;1H\x1bD",
        },
        SupportedFixture {
            id: "reverse_index_at_top_scrolls",
            cols: 5,
            rows: 3,
            // Fill 3 rows, cursor to (0,0), ESC M → scroll down
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[1;1H\x1bM",
        },
        SupportedFixture {
            id: "newline_at_bottom_scrolls",
            cols: 5,
            rows: 3,
            // Fill 3 rows, CUP to (2,2), \r\n → CR + LF at bottom scrolls
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[3;3H\r\n",
        },
        // ── Boundary conditions ──────────────────────────────────────
        SupportedFixture {
            id: "cup_out_of_bounds_clamps",
            cols: 10,
            rows: 5,
            // CUP(99,99) → clamps to bottom-right, write "Z" then CUP to known pos
            bytes: b"\x1b[99;99HZ\x1b[1;1HX",
        },
        SupportedFixture {
            id: "cursor_move_clamps_at_edges",
            cols: 5,
            rows: 3,
            // CUU 999 from middle → row 0; CUB 999 → col 0
            bytes: b"\x1b[2;3H\x1b[999A\x1b[999DX",
        },
        // ── NEL (Next Line) ─────────────────────────────────────────
        SupportedFixture {
            id: "nel_basic",
            cols: 10,
            rows: 3,
            // Write "ABCDE", NEL → cursor goes to col 0, next row, write "X"
            bytes: b"ABCDE\x1bEX",
        },
        SupportedFixture {
            id: "nel_at_bottom_scrolls",
            cols: 5,
            rows: 3,
            // Fill 3 rows, CUP to (2,2), NEL at bottom scrolls
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[3;3H\x1bEX",
        },
        // ── DECALN (Screen Alignment) ────────────────────────────────
        SupportedFixture {
            id: "decaln_fills_screen",
            cols: 5,
            rows: 3,
            // Write some text, then DECALN fills with 'E'
            bytes: b"ABC\x1b#8",
        },
        SupportedFixture {
            id: "decaln_then_overwrite",
            cols: 5,
            rows: 3,
            // DECALN fills with 'E', then overwrite one cell
            bytes: b"\x1b#8\x1b[2;3HX",
        },
        // ── UTF-8 multi-byte characters ──────────────────────────────
        SupportedFixture {
            id: "utf8_two_byte",
            cols: 10,
            rows: 3,
            // "Aé B" — é is U+00E9 (2 bytes: 0xC3 0xA9)
            bytes: "A\u{00E9} B".as_bytes(),
        },
        SupportedFixture {
            id: "utf8_three_byte_cjk",
            cols: 10,
            rows: 3,
            // "A中B" — 中 is U+4E2D (3 bytes), display width 2
            bytes: "A\u{4E2D}B".as_bytes(),
        },
        // ── Wide character handling ───────────────────────────────────
        SupportedFixture {
            id: "wide_char_two_cells",
            cols: 10,
            rows: 3,
            // Wide char takes 2 cells, then narrow char follows
            bytes: "\u{4E2D}\u{6587}X".as_bytes(),
        },
        SupportedFixture {
            id: "wide_char_wrap_at_last_col",
            cols: 5,
            rows: 3,
            // 4 narrow chars + wide char wraps to next line
            bytes: "ABCD\u{4E2D}".as_bytes(),
        },
        // ── C0 controls: backspace ──────────────────────────────────
        SupportedFixture {
            id: "backspace_basic",
            cols: 10,
            rows: 3,
            // Write "ABC", BS, write "X" → overwrites 'C' → "ABX"
            bytes: b"ABC\x08X",
        },
        SupportedFixture {
            id: "backspace_at_col_zero",
            cols: 10,
            rows: 3,
            // BS at col 0 stays at col 0
            bytes: b"\x08X",
        },
        SupportedFixture {
            id: "backspace_multiple",
            cols: 10,
            rows: 3,
            // Write "ABCDE", BS twice, write "X" → overwrites 'C' → "ABXDE"
            bytes: b"ABCDE\x08\x08X",
        },
        // ── C0 controls: horizontal tab ─────────────────────────────
        SupportedFixture {
            id: "tab_default_stops",
            cols: 20,
            rows: 3,
            // Tab from col 0 → col 8, write "X"
            bytes: b"\tX",
        },
        SupportedFixture {
            id: "tab_from_mid_column",
            cols: 20,
            rows: 3,
            // Write "AB" (col 2), tab → col 8, write "X"
            bytes: b"AB\tX",
        },
        SupportedFixture {
            id: "tab_near_end_of_line",
            cols: 12,
            rows: 3,
            // Write 7 chars, tab → col 8, write "X"
            bytes: b"ABCDEFG\tX",
        },
        SupportedFixture {
            id: "tab_multiple",
            cols: 20,
            rows: 3,
            // Two tabs: col 0 → 8 → 16, write "X"
            bytes: b"\t\tX",
        },
        // ── C0 controls: bell ───────────────────────────────────────
        SupportedFixture {
            id: "bell_no_effect",
            cols: 10,
            rows: 3,
            // BEL should be ignored, cursor stays
            bytes: b"AB\x07CD",
        },
        // ── Cursor movement: CUF (Cursor Forward) ──────────────────
        SupportedFixture {
            id: "cuf_basic",
            cols: 10,
            rows: 3,
            // CUF 3 from col 0 → col 3, write "X"
            bytes: b"\x1b[3CX",
        },
        SupportedFixture {
            id: "cuf_clamps_at_edge",
            cols: 5,
            rows: 3,
            // CUF 999 from col 0 → clamps to col 4, CUP back, write
            bytes: b"\x1b[999C\x1b[1;1HX",
        },
        SupportedFixture {
            id: "cud_basic",
            cols: 10,
            rows: 5,
            // CUD 2 from row 0 → row 2, write "X"
            bytes: b"\x1b[2BX",
        },
        SupportedFixture {
            id: "cud_clamps_at_bottom",
            cols: 10,
            rows: 3,
            // CUD 999 from row 0 → clamps to row 2, write "X"
            bytes: b"\x1b[999BX",
        },
        // ── Cursor position via CHA and VPA ─────────────────────────
        SupportedFixture {
            id: "cha_sets_column",
            cols: 10,
            rows: 3,
            // Write "ABCDE", CHA 3 → col 2, write "X"
            bytes: b"ABCDE\x1b[3GX",
        },
        SupportedFixture {
            id: "vpa_sets_row",
            cols: 10,
            rows: 5,
            // Write "A" at (0,0), VPA 3 → row 2, write "X"
            bytes: b"A\x1b[3dX",
        },
        // ── Erase in Display: mode 2 (entire screen) ───────────────
        SupportedFixture {
            id: "ed_entire_screen",
            cols: 5,
            rows: 3,
            // Fill screen, ED 2 → entire screen blank, cursor stays
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\x1b[2;3H\x1b[2J",
        },
        // ── Erase in Display: selective positions ───────────────────
        SupportedFixture {
            id: "ed_below_from_middle_of_row",
            cols: 10,
            rows: 3,
            // Fill 3 rows, CUP(1,5), ED 0 → partial first row + full rows below
            bytes: b"AAAAABBBBB\r\nCCCCCDDDDD\r\nEEEEEFFFFF\x1b[1;6H\x1b[0J",
        },
        // ── Erase in Line: additional cases ─────────────────────────
        SupportedFixture {
            id: "el_from_start",
            cols: 10,
            rows: 3,
            // Write "ABCDEFGH", CUP(1,4), EL 1 → erase from start through col 3
            bytes: b"ABCDEFGH\x1b[1;4H\x1b[1K",
        },
        // ── Insert Lines edge cases ─────────────────────────────────
        SupportedFixture {
            id: "il_at_top_of_scroll_region",
            cols: 5,
            rows: 5,
            // Fill, set region 1-5 (full), cursor at row 0, IL 1
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE\x1b[1;5r\x1b[1;1H\x1b[1L",
        },
        SupportedFixture {
            id: "dl_at_bottom_of_scroll_region",
            cols: 5,
            rows: 5,
            // Fill, set region 1-5, cursor at row 4, DL 1
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\r\nEEEEE\x1b[1;5r\x1b[5;1H\x1b[1M",
        },
        // ── Multiple scroll operations ──────────────────────────────
        SupportedFixture {
            id: "scroll_up_multiple",
            cols: 8,
            rows: 4,
            // Fill 4 rows with short text + CUP, scroll up 2
            bytes: b"AAA\x1b[2;1HBBB\x1b[3;1HCCC\x1b[4;1HDDD\x1b[1;1H\x1b[2S",
        },
        SupportedFixture {
            id: "scroll_down_multiple",
            cols: 8,
            rows: 4,
            // Fill 4 rows with short text + CUP, scroll down 2
            bytes: b"AAA\x1b[2;1HBBB\x1b[3;1HCCC\x1b[4;1HDDD\x1b[1;1H\x1b[2T",
        },
        // ── ICH/DCH/ECH additional cases ────────────────────────────
        SupportedFixture {
            id: "ich_at_beginning_of_line",
            cols: 10,
            rows: 3,
            // Write "ABCDE", CUP(1,1), ICH 3 → shift right 3, write "XYZ"
            bytes: b"ABCDE\x1b[1;1H\x1b[3@XYZ",
        },
        SupportedFixture {
            id: "ech_at_end_of_line",
            cols: 10,
            rows: 3,
            // Write "ABCDEFGHIJ", CUP(1,8), ECH 5 → erase 3 (clamped to end)
            bytes: b"ABCDEFGHIJ\x1b[1;8H\x1b[5X",
        },
        // ── Soft reset ──────────────────────────────────────────────
        SupportedFixture {
            id: "soft_reset_clears_scroll_region",
            cols: 5,
            rows: 4,
            // Set scroll region 2-3, soft reset, scroll up → uses full screen
            bytes: b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\x1b[2;3r\x1b[!p\x1b[1S",
        },
        // ── CNL / CPL ───────────────────────────────────────────────
        SupportedFixture {
            id: "cnl_moves_down_and_to_col_zero",
            cols: 10,
            rows: 5,
            // Write "ABCDE", CNL 2 → down 2 rows, col 0, write "X"
            bytes: b"ABCDE\x1b[2EX",
        },
        SupportedFixture {
            id: "cpl_moves_up_and_to_col_zero",
            cols: 10,
            rows: 5,
            // CUP(3,5), CPL 2 → up 2 rows, col 0, write "X"
            bytes: b"\x1b[3;5H\x1b[2FX",
        },
        // ── Wrap behavior: overwrite at last column ─────────────────
        SupportedFixture {
            id: "overwrite_at_last_column",
            cols: 5,
            rows: 3,
            // Write 5 chars (pending wrap), CUP back to col 4, overwrite, CUP to col 1
            bytes: b"ABCDE\x1b[1;5HX\x1b[1;1H",
        },
        // ── CR + LF combined ────────────────────────────────────────
        SupportedFixture {
            id: "cr_lf_sequence",
            cols: 10,
            rows: 3,
            // Write "ABCDE", CR → col 0, LF → row 1, write "X"
            bytes: b"ABCDE\r\nX",
        },
        SupportedFixture {
            id: "lf_without_cr",
            cols: 10,
            rows: 3,
            // Write "ABCDE", LF → row 1 but col stays, write "X"
            bytes: b"ABCDE\nX",
        },
        // ── Custom tab stops (HTS, TBC, CBT) ────────────────────────
        SupportedFixture {
            id: "hts_set_custom_stop",
            cols: 20,
            rows: 3,
            // CUP(1,5), ESC H (set stop), CUP(1,1), tab → col 5, write "X"
            bytes: b"\x1b[1;6H\x1bH\x1b[1;1H\tX",
        },
        SupportedFixture {
            id: "tbc_clear_single_stop",
            cols: 20,
            rows: 3,
            // CUP(1,9) → col 8, CSI 0g (clear stop at 8), CUP(1,1), tab → col 16
            bytes: b"\x1b[1;9H\x1b[0g\x1b[1;1H\tX",
        },
        SupportedFixture {
            id: "tbc_clear_all_stops",
            cols: 20,
            rows: 3,
            // Clear all stops, tab from col 0 → last col, CUP back to write
            bytes: b"\x1b[3g\t\x1b[1;1HX",
        },
        SupportedFixture {
            id: "cbt_back_tab",
            cols: 20,
            rows: 3,
            // CUP(1,11) → col 10, CBT → col 8, write "X"
            bytes: b"\x1b[1;11H\x1b[ZX",
        },
        SupportedFixture {
            id: "cbt_multiple",
            cols: 20,
            rows: 3,
            // CUP(1,18) → col 17, CBT 2 → back to 8 then to 0, write "X"
            bytes: b"\x1b[1;18H\x1b[2ZX",
        },
        SupportedFixture {
            id: "hts_multiple_custom_stops",
            cols: 20,
            rows: 3,
            // Set stops at 3 and 6, clear all defaults, tab through custom stops
            bytes: b"\x1b[3g\x1b[1;4H\x1bH\x1b[1;7H\x1bH\x1b[1;1H\tA\tB",
        },
        // ── IRM (Insert/Replace Mode) ───────────────────────────────
        SupportedFixture {
            id: "irm_insert_shifts_right",
            cols: 10,
            rows: 3,
            // Write "ABCDE", enable insert mode, CUP(1,3), type "XY"
            bytes: b"ABCDE\x1b[4h\x1b[1;3HXY",
        },
        SupportedFixture {
            id: "irm_insert_at_beginning",
            cols: 10,
            rows: 3,
            // Write "ABCDE", insert mode, CUP(1,1), type "XY"
            bytes: b"ABCDE\x1b[4h\x1b[1;1HXY",
        },
        SupportedFixture {
            id: "irm_insert_pushes_off_edge",
            cols: 5,
            rows: 3,
            // Write "ABCDE", insert mode, CUP(1,1), type "X"
            bytes: b"ABCDE\x1b[4h\x1b[1;1HX",
        },
        SupportedFixture {
            id: "irm_disable_returns_to_replace",
            cols: 10,
            rows: 3,
            // Write "ABCDE", enable+disable insert, CUP(1,3), type "XY"
            bytes: b"ABCDE\x1b[4h\x1b[4l\x1b[1;3HXY",
        },
        // ── DECAWM (Auto-Wrap Mode) ─────────────────────────────────
        SupportedFixture {
            id: "decawm_off_no_wrap",
            cols: 5,
            rows: 3,
            // Disable autowrap, write 8 chars → last col overwritten repeatedly
            bytes: b"\x1b[?7lABCDEFGH",
        },
        SupportedFixture {
            id: "decawm_off_then_on_wraps",
            cols: 5,
            rows: 3,
            // Disable, write 3, re-enable, write 4 more → wraps after col 4
            bytes: b"\x1b[?7lABC\x1b[?7hDEFG",
        },
        SupportedFixture {
            id: "decawm_off_cursor_stays_at_edge",
            cols: 5,
            rows: 3,
            // Disable autowrap, CUP(1,5), write "XYZ" → col stays at 4
            bytes: b"\x1b[?7l\x1b[1;5HXYZ\x1b[1;1HA",
        },
        // ── Charset (DEC Special Graphics) ──────────────────────────
        SupportedFixture {
            id: "dec_graphics_box_top",
            cols: 10,
            rows: 3,
            // ESC ( 0 → DEC Graphics; l=┌ q=─ k=┐
            bytes: b"\x1b(0lqqqk",
        },
        SupportedFixture {
            id: "dec_graphics_box_bottom",
            cols: 10,
            rows: 3,
            // m=└ q=─ j=┘
            bytes: b"\x1b(0mqqqj",
        },
        SupportedFixture {
            id: "dec_graphics_tees",
            cols: 10,
            rows: 3,
            // t=├ u=┤ n=┼
            bytes: b"\x1b(0tun",
        },
        SupportedFixture {
            id: "charset_switch_back_to_ascii",
            cols: 10,
            rows: 3,
            // DEC Graphics l+q, switch back to ASCII, print l+q literally
            bytes: b"\x1b(0lq\x1b(Blq",
        },
        SupportedFixture {
            id: "dec_graphics_passthrough",
            cols: 10,
            rows: 3,
            // A-Z and 0-9 pass through unchanged in DEC Graphics
            bytes: b"\x1b(0ABC123",
        },
        SupportedFixture {
            id: "full_reset_clears_charset",
            cols: 10,
            rows: 3,
            // DEC Graphics 'l'=┌, then RIS clears charset, print 'l' literally
            bytes: b"\x1b(0l\x1bcl",
        },
        SupportedFixture {
            id: "soft_reset_clears_charset",
            cols: 10,
            rows: 3,
            // DEC Graphics 'l'=┌, then DECSTR clears charset, print 'l' literally
            bytes: b"\x1b(0l\x1b[!pl",
        },
        SupportedFixture {
            id: "dec_graphics_symbols",
            cols: 10,
            rows: 3,
            // `=◆ a=▒ f=° g=±
            bytes: b"\x1b(0`afg",
        },
        SupportedFixture {
            id: "dec_graphics_math",
            cols: 10,
            rows: 3,
            // y=≤ z=≥ {=π |=≠ }=£ ~=·
            bytes: b"\x1b(0yz{|}~",
        },
        SupportedFixture {
            id: "dec_graphics_with_attrs",
            cols: 10,
            rows: 3,
            // DEC Graphics + bold: x=│ q=─
            bytes: b"\x1b(0\x1b[1mxq",
        },
        SupportedFixture {
            id: "charset_dec_graphics_translation",
            cols: 10,
            rows: 3,
            // DEC Graphics: j=┘ k=┐ l=┌ m=└ n=┼ q=─ x=│
            bytes: b"\x1b(0jklmnqx",
        },
        SupportedFixture {
            id: "charset_g0_dec_graphics",
            cols: 10,
            rows: 3,
            // Print XY in ASCII, then designate G0=DEC Graphics, print Z
            bytes: b"XY\x1b(0Z",
        },
        SupportedFixture {
            id: "ss2_dec_graphics",
            cols: 10,
            rows: 3,
            // G2=DEC Graphics, print A, SS2 + q → ─, then B (back to G0 ASCII)
            bytes: b"\x1b*0A\x1bNqB",
        },
        SupportedFixture {
            id: "ss3_dec_graphics",
            cols: 10,
            rows: 3,
            // G3=DEC Graphics, print A, SS3 + l → ┌, then B (back to G0 ASCII)
            bytes: b"\x1b+0A\x1bOlB",
        },
        SupportedFixture {
            id: "ss2_only_one_char",
            cols: 10,
            rows: 3,
            // G2=DEC Graphics, SS2 + j → ┘ (one char only), then j → literal j
            bytes: b"\x1b*0\x1bNjj",
        },
        // ── SGR attributes + text interaction ───────────────────────
        SupportedFixture {
            id: "sgr_bold_text",
            cols: 10,
            rows: 3,
            // SGR 1 (bold), print AB, SGR 0 (reset), print CD
            bytes: b"\x1b[1mAB\x1b[0mCD",
        },
        SupportedFixture {
            id: "sgr_italic_underline_text",
            cols: 10,
            rows: 3,
            // SGR 3 (italic) + SGR 4 (underline) combined, print XY
            bytes: b"\x1b[3;4mXY",
        },
        SupportedFixture {
            id: "sgr_stacked_attributes",
            cols: 20,
            rows: 3,
            // Bold, dim, italic, underline, blink, inverse, hidden, strike — all at once
            bytes: b"\x1b[1;2;3;4;5;7;8;9mABC\x1b[0mDEF",
        },
        SupportedFixture {
            id: "sgr_selective_reset_bold_dim",
            cols: 15,
            rows: 3,
            // SGR 1 (bold) + SGR 2 (dim), print A, SGR 22 (unbold+undim), print B
            bytes: b"\x1b[1;2mA\x1b[22mB",
        },
        SupportedFixture {
            id: "sgr_selective_reset_italic",
            cols: 10,
            rows: 3,
            // SGR 3 (italic), print A, SGR 23 (no italic), print B
            bytes: b"\x1b[3mA\x1b[23mB",
        },
        SupportedFixture {
            id: "sgr_selective_reset_underline",
            cols: 10,
            rows: 3,
            // SGR 4 (underline), print A, SGR 24 (no underline), print B
            bytes: b"\x1b[4mA\x1b[24mB",
        },
        SupportedFixture {
            id: "sgr_selective_reset_blink",
            cols: 10,
            rows: 3,
            // SGR 5 (blink), print A, SGR 25 (no blink), print B
            bytes: b"\x1b[5mA\x1b[25mB",
        },
        SupportedFixture {
            id: "sgr_selective_reset_inverse",
            cols: 10,
            rows: 3,
            // SGR 7 (inverse), print A, SGR 27 (no inverse), print B
            bytes: b"\x1b[7mA\x1b[27mB",
        },
        SupportedFixture {
            id: "sgr_selective_reset_hidden",
            cols: 10,
            rows: 3,
            // SGR 8 (hidden), print A, SGR 28 (no hidden), print B
            bytes: b"\x1b[8mA\x1b[28mB",
        },
        SupportedFixture {
            id: "sgr_selective_reset_strikethrough",
            cols: 10,
            rows: 3,
            // SGR 9 (strikethrough), print A, SGR 29 (no strike), print B
            bytes: b"\x1b[9mA\x1b[29mB",
        },
        SupportedFixture {
            id: "sgr_named_fg_color",
            cols: 10,
            rows: 3,
            // SGR 31 (red fg), print AB, SGR 39 (default fg), print CD
            bytes: b"\x1b[31mAB\x1b[39mCD",
        },
        SupportedFixture {
            id: "sgr_named_bg_color",
            cols: 10,
            rows: 3,
            // SGR 42 (green bg), print AB, SGR 49 (default bg), print CD
            bytes: b"\x1b[42mAB\x1b[49mCD",
        },
        SupportedFixture {
            id: "sgr_bright_fg_color",
            cols: 10,
            rows: 3,
            // SGR 91 (bright red fg), print AB, SGR 39 (default fg), print CD
            bytes: b"\x1b[91mAB\x1b[39mCD",
        },
        SupportedFixture {
            id: "sgr_bright_bg_color",
            cols: 10,
            rows: 3,
            // SGR 102 (bright green bg), print AB, SGR 49 (default bg), print CD
            bytes: b"\x1b[102mAB\x1b[49mCD",
        },
        SupportedFixture {
            id: "sgr_256_fg_color",
            cols: 10,
            rows: 3,
            // SGR 38;5;196 (256-color red fg), print XY
            bytes: b"\x1b[38;5;196mXY",
        },
        SupportedFixture {
            id: "sgr_256_bg_color",
            cols: 10,
            rows: 3,
            // SGR 48;5;46 (256-color green bg), print XY
            bytes: b"\x1b[48;5;46mXY",
        },
        SupportedFixture {
            id: "sgr_truecolor_fg",
            cols: 10,
            rows: 3,
            // SGR 38;2;255;128;0 (truecolor orange fg), print AB
            bytes: b"\x1b[38;2;255;128;0mAB",
        },
        SupportedFixture {
            id: "sgr_truecolor_bg",
            cols: 10,
            rows: 3,
            // SGR 48;2;0;128;255 (truecolor blue bg), print AB
            bytes: b"\x1b[48;2;0;128;255mAB",
        },
        SupportedFixture {
            id: "sgr_full_reset_clears_all",
            cols: 15,
            rows: 3,
            // Stack bold+italic+fg+bg, print A, SGR 0, print B — text same, attrs differ
            bytes: b"\x1b[1;3;31;42mA\x1b[0mB",
        },
        SupportedFixture {
            id: "sgr_empty_param_is_reset",
            cols: 10,
            rows: 3,
            // SGR 1 (bold), print A, SGR with no params (= reset), print B
            bytes: b"\x1b[1mA\x1b[mB",
        },
        SupportedFixture {
            id: "sgr_multiple_sgr_sequences",
            cols: 15,
            rows: 3,
            // Two separate SGR sequences: bold then red fg, print ABC
            bytes: b"\x1b[1m\x1b[31mABC",
        },
        SupportedFixture {
            id: "sgr_color_with_bold_and_reset",
            cols: 20,
            rows: 3,
            // Bold+red, print AB, reset, green+italic, print CD, reset, print EF
            bytes: b"\x1b[1;31mAB\x1b[0m\x1b[32;3mCD\x1b[0mEF",
        },
        // ── SGR + cursor movement interaction ───────────────────────
        SupportedFixture {
            id: "sgr_persists_across_cup",
            cols: 10,
            rows: 5,
            // SGR bold, print A, CUP(3,1), print B — bold should persist
            bytes: b"\x1b[1mA\x1b[3;1HB",
        },
        SupportedFixture {
            id: "sgr_persists_across_newline",
            cols: 10,
            rows: 3,
            // SGR 31 (red), print A, newline, print B — red should persist
            bytes: b"\x1b[31mA\nB",
        },
        SupportedFixture {
            id: "sgr_persists_across_cr_lf",
            cols: 10,
            rows: 3,
            // SGR bold, print A, CR LF, print B
            bytes: b"\x1b[1mA\r\nB",
        },
        // ── Line editing + wide char interactions ───────────────────
        SupportedFixture {
            id: "il_with_wide_chars",
            cols: 10,
            rows: 4,
            // Row 1: 中文, Row 2: AB, CUP(1,1), IL 1 — insert blank line
            bytes: b"\xe4\xb8\xad\xe6\x96\x87\r\nAB\x1b[1;1H\x1b[1L",
        },
        SupportedFixture {
            id: "dl_with_wide_chars",
            cols: 10,
            rows: 4,
            // Row 1: 中文, Row 2: AB, CUP(1,1), DL 1 — delete first line
            bytes: b"\xe4\xb8\xad\xe6\x96\x87\r\nAB\x1b[1;1H\x1b[1M",
        },
        SupportedFixture {
            id: "overwrite_narrow_with_wide",
            cols: 10,
            rows: 3,
            // Print AB, CUP(1,1), print wide 中 → overwrites A and B
            bytes: b"AB\x1b[1;1H\xe4\xb8\xad",
        },
        // ── SGR bg color on erase/insert operations ────────────────
        // VirtualTerminal now uses styled_blank() (current_style) for blanks in
        // all erase/insert ops, matching core behavior per VT220 spec.
        SupportedFixture {
            id: "sgr_ech_with_bg",
            cols: 10,
            rows: 3,
            // Print ABCDE, set bg green (42), CUP(1,3), ECH 2 → cols 2-3 get green bg
            bytes: b"ABCDE\x1b[42m\x1b[1;3H\x1b[2X",
        },
        SupportedFixture {
            id: "sgr_el_right_with_bg",
            cols: 10,
            rows: 3,
            // Print ABCDE, set bg red (41), CUP(1,3), EL 0 (right) → cols 2+ get red bg
            bytes: b"ABCDE\x1b[41m\x1b[1;3H\x1b[0K",
        },
        SupportedFixture {
            id: "sgr_el_left_with_bg",
            cols: 10,
            rows: 3,
            // Print ABCDE, set bg blue (44), CUP(1,4), EL 1 (left) → cols 0-3 get blue bg
            bytes: b"ABCDE\x1b[44m\x1b[1;4H\x1b[1K",
        },
        SupportedFixture {
            id: "sgr_el_full_with_bg",
            cols: 10,
            rows: 3,
            // Print ABCDE, set bg magenta (45), CUP(1,3), EL 2 (full line)
            bytes: b"ABCDE\x1b[45m\x1b[1;3H\x1b[2K",
        },
        SupportedFixture {
            id: "sgr_ed_below_with_bg",
            cols: 10,
            rows: 3,
            // Row 0: ABCDE, Row 1: FGHIJ, set bg green (42), CUP(1,3), ED 0 (below)
            bytes: b"ABCDE\nFGHIJ\x1b[42m\x1b[1;3H\x1b[0J",
        },
        SupportedFixture {
            id: "sgr_ed_above_with_bg",
            cols: 10,
            rows: 3,
            // Row 0: ABCDE, Row 1: FGHIJ, set bg yellow (43), CUP(2,3), ED 1 (above)
            bytes: b"ABCDE\nFGHIJ\x1b[43m\x1b[2;3H\x1b[1J",
        },
        SupportedFixture {
            id: "sgr_ed_all_with_bg",
            cols: 10,
            rows: 3,
            // Print content on two rows, set bg cyan (46), ED 2 (all)
            bytes: b"ABCDE\nFGHIJ\x1b[46m\x1b[2J",
        },
        SupportedFixture {
            id: "sgr_ich_with_bg",
            cols: 10,
            rows: 3,
            // Print ABCDE, set bg green (42), CUP(1,3), ICH 2 → 2 blanks with green bg
            bytes: b"ABCDE\x1b[42m\x1b[1;3H\x1b[2@",
        },
        SupportedFixture {
            id: "sgr_dch_with_bg",
            cols: 10,
            rows: 3,
            // Print ABCDE, set bg red (41), CUP(1,2), DCH 2 → trailing blanks get red bg
            bytes: b"ABCDE\x1b[41m\x1b[1;2H\x1b[2P",
        },
        SupportedFixture {
            id: "sgr_il_with_bg",
            cols: 10,
            rows: 4,
            // Row 0: ABCDE, Row 1: FGHIJ, set bg blue (44), CUP(1,1), IL 1
            bytes: b"ABCDE\nFGHIJ\x1b[44m\x1b[1;1H\x1b[1L",
        },
        SupportedFixture {
            id: "sgr_dl_with_bg",
            cols: 10,
            rows: 4,
            // Row 0: ABCDE, Row 1: FGHIJ, set bg magenta (45), CUP(1,1), DL 1
            bytes: b"ABCDE\nFGHIJ\x1b[45m\x1b[1;1H\x1b[1M",
        },
        SupportedFixture {
            id: "sgr_su_with_bg",
            cols: 10,
            rows: 3,
            // Row 0: ABCDE, Row 1: FGHIJ, set bg yellow (43), SU 1 → bottom row gets yellow bg
            bytes: b"ABCDE\nFGHIJ\x1b[43m\x1b[1S",
        },
        SupportedFixture {
            id: "sgr_sd_with_bg",
            cols: 10,
            rows: 3,
            // Row 0: ABCDE, Row 1: FGHIJ, set bg cyan (46), SD 1 → top row gets cyan bg
            bytes: b"ABCDE\nFGHIJ\x1b[46m\x1b[1T",
        },
        // ── Mixed editing edge cases ────────────────────────────────
        SupportedFixture {
            id: "dch_at_right_edge",
            cols: 5,
            rows: 3,
            // Print ABCDE (fills row), CUP(1,5), DCH 1 — at last col
            bytes: b"ABCDE\x1b[1;5H\x1b[1P",
        },
        SupportedFixture {
            id: "ich_at_right_edge",
            cols: 5,
            rows: 3,
            // Print ABCDE, CUP(1,5), ICH 1 — insert at last col
            bytes: b"ABCDE\x1b[1;5H\x1b[1@",
        },
        SupportedFixture {
            id: "ech_beyond_line_width",
            cols: 5,
            rows: 3,
            // Print ABC, ECH 10 — erase more than remaining cols
            bytes: b"ABC\x1b[10X",
        },
        SupportedFixture {
            id: "dch_more_than_remaining",
            cols: 5,
            rows: 3,
            // Print ABCDE, CUP(1,3), DCH 10 — delete more than remaining
            bytes: b"ABCDE\x1b[1;3H\x1b[10P",
        },
        SupportedFixture {
            id: "ich_more_than_remaining",
            cols: 5,
            rows: 3,
            // Print ABCDE, CUP(1,3), ICH 10 — insert more than remaining
            bytes: b"ABCDE\x1b[1;3H\x1b[10@",
        },
        SupportedFixture {
            id: "ed_mode1_above_cursor",
            cols: 10,
            rows: 4,
            // Row 1: ABCD, Row 2: EFGH, CUP(2,3), ED 1 — erase above including cursor row
            bytes: b"ABCD\r\nEFGH\x1b[2;3H\x1b[1J",
        },
        SupportedFixture {
            id: "el_mode2_entire_line",
            cols: 10,
            rows: 3,
            // Print ABCDEFGH, CUP(1,4), EL 2 — erase entire line
            bytes: b"ABCDEFGH\x1b[1;4H\x1b[2K",
        },
        SupportedFixture {
            id: "dch_then_print_fills_gap",
            cols: 10,
            rows: 3,
            // Print ABCDE, CUP(1,2), DCH 2, print XY — fills the gap
            bytes: b"ABCDE\x1b[1;2H\x1b[2PXY",
        },
        SupportedFixture {
            id: "ich_then_print_in_gap",
            cols: 10,
            rows: 3,
            // Print ABCDE, CUP(1,2), ICH 2, print XY — fills inserted blanks
            bytes: b"ABCDE\x1b[1;2H\x1b[2@XY",
        },
        // ── Wide char multi-row layout ─────────────────────────────
        SupportedFixture {
            id: "wide_chars_across_rows",
            cols: 10,
            rows: 3,
            // Row 1: 中文 (4 cells), Row 2: AB — no wrap ambiguity
            bytes: b"\xe4\xb8\xad\xe6\x96\x87\r\nAB",
        },
        // ── Line edit: insert/delete lines ──────────────────────────
        SupportedFixture {
            id: "insert_line",
            cols: 5,
            rows: 3,
            // A at (0,0), CUP(2,1)+B, CUP(3,1)+C, CUP(1,1), IL(1)
            bytes: b"A\x1b[2HB\x00\x00\x1b[3HC\x00\x00\x1b[1;1H\x1b[1L",
        },
        SupportedFixture {
            id: "delete_line",
            cols: 5,
            rows: 3,
            // A at (0,0), CUP(2,1)+B, CUP(3,1)+C, CUP(1,1), DL(1)
            bytes: b"A\x1b[2HB\x1b[3HC\x1b[1;1H\x1b[1M",
        },
        SupportedFixture {
            id: "insert_line_mid_screen",
            cols: 6,
            rows: 4,
            // Fill 4 rows: LINE0-LINE3, CUP(2,1), IL(1)
            bytes: b"LINE0\n\rLINE1\n\rLINE2\n\rLINE3\x1b[2;1H\x1b[1L",
        },
        SupportedFixture {
            id: "delete_line_mid_screen",
            cols: 6,
            rows: 4,
            // Fill 4 rows: LINE0-LINE3, CUP(2,1), DL(1)
            bytes: b"LINE0\n\rLINE1\n\rLINE2\n\rLINE3\x1b[2;1H\x1b[1M",
        },
        // ── Line edit: insert/delete chars ──────────────────────────
        SupportedFixture {
            id: "insert_chars_basic",
            cols: 8,
            rows: 3,
            // ABCDE, CUP(1,2), ICH(2)+'X'
            bytes: b"ABCDE\x1b[1;2H\x1b[2@X",
        },
        SupportedFixture {
            id: "delete_chars_basic",
            cols: 8,
            rows: 3,
            // ABCDE, CUP(1,2), DCH(2)+'X'
            bytes: b"ABCDE\x1b[1;2H\x1b[2PX",
        },
        SupportedFixture {
            id: "insert_chars_at_margin",
            cols: 8,
            rows: 3,
            // ABCDEFGH, CUP(1,7), ICH(3)+'X'
            bytes: b"ABCDEFGH\x1b[1;7H\x1b[3@X",
        },
        SupportedFixture {
            id: "delete_chars_overflow",
            cols: 8,
            rows: 3,
            // ABCDE, CUP(1,3), DCH(10) — count > remaining
            bytes: b"ABCDE\x1b[1;3H\x1b[10P",
        },
        SupportedFixture {
            id: "insert_delete_chars",
            cols: 10,
            rows: 3,
            // ABCDEFG, CUP(1,3), ICH(2), CUP(1,6), DCH(1)+'X'
            bytes: b"ABCDEFG\x1b[1;3H\x1b[2@\x1b[1;6H\x1b[1PX",
        },
        SupportedFixture {
            id: "insert_chars_at_end",
            cols: 5,
            rows: 3,
            // ABCDE, CUP(1,5), ICH(2) — at end of row
            bytes: b"ABCDE\x1b[1;5H\x1b[2@",
        },
        SupportedFixture {
            id: "delete_chars_mid_row",
            cols: 5,
            rows: 3,
            // ABCDE, CUP(1,2), DCH(2) — delete from middle
            bytes: b"ABCDE\x1b[1;2H\x1b[2P",
        },
        // ── Line edit: IRM interactions ─────────────────────────────
        SupportedFixture {
            id: "irm_with_full_row",
            cols: 5,
            rows: 3,
            // IRM on, ABC, CUP home, XY → inserts XY before ABC
            bytes: b"\x1b[4hABC\x1b[HXY",
        },
        SupportedFixture {
            id: "delete_then_insert",
            cols: 10,
            rows: 3,
            // ABCDE, CUP home, DCH(2), IRM on, XY
            bytes: b"ABCDE\x1b[H\x1b[2P\x1b[4hXY",
        },
        SupportedFixture {
            id: "irm_insert_before_wide",
            cols: 8,
            rows: 3,
            // Wide char (世) + A, CUP(1,1), IRM on, print X
            bytes: b"\xe4\xb8\x96A\x1b[1;1H\x1b[4hX",
        },
        // SGR bg with ICH/DCH (restored — reference now uses styled_blank).
        SupportedFixture {
            id: "insert_chars_uses_current_bg",
            cols: 8,
            rows: 3,
            // Set red fg (31), print ABCDEF, set green bg (42), CUP(1,3), ICH 2
            bytes: b"\x1b[31mABCDEF\x1b[42m\x1b[1;3H\x1b[2@",
        },
        SupportedFixture {
            id: "delete_chars_uses_current_bg",
            cols: 8,
            rows: 3,
            // Set red fg (31), print ABCDEF, set green bg (42), CUP(1,3), DCH 2
            bytes: b"\x1b[31mABCDEF\x1b[42m\x1b[1;3H\x1b[2P",
        },
        // ── Wide char: basic and wrap ───────────────────────────────
        SupportedFixture {
            id: "wide_char_basic",
            cols: 10,
            rows: 3,
            bytes: b"A\xe4\xb8\xadB",
        },
        SupportedFixture {
            id: "wide_char_wrap",
            cols: 5,
            rows: 3,
            bytes: b"ABCD\xe4\xb8\xad",
        },
        SupportedFixture {
            id: "cjk_basic",
            cols: 10,
            rows: 3,
            bytes: b"A\xe4\xb8\x96\xe7\x95\x8cB",
        },
        SupportedFixture {
            id: "cjk_wrap_at_margin",
            cols: 6,
            rows: 3,
            bytes: b"ABCDE\xe4\xb8\x96",
        },
        SupportedFixture {
            id: "consecutive_wide_chars",
            cols: 10,
            rows: 3,
            bytes: b"\xe4\xb8\x96\xe7\x95\x8c",
        },
        // ── Wide char: overwrite interactions ───────────────────────
        SupportedFixture {
            id: "overwrite_continuation_with_narrow",
            cols: 10,
            rows: 3,
            bytes: b"\xe4\xb8\x96\x1b[1;2HY",
        },
        SupportedFixture {
            id: "overwrite_wide_first_half",
            cols: 8,
            rows: 3,
            bytes: b"\xe4\xb8\x96\x1b[1;1HX",
        },
        SupportedFixture {
            id: "overwrite_wide_second_half",
            cols: 8,
            rows: 3,
            bytes: b"\xe4\xb8\x96\x1b[1;2HY",
        },
        // ── Wide char: erase/edit interactions ──────────────────────
        SupportedFixture {
            id: "ech_splits_wide_char",
            cols: 8,
            rows: 1,
            bytes: b"A\xe4\xb8\xadB\x1b[1;3H\x1b[1X",
        },
        SupportedFixture {
            id: "ech_at_wide_head",
            cols: 8,
            rows: 1,
            bytes: b"A\xe4\xb8\xadB\x1b[1;2H\x1b[1X",
        },
        SupportedFixture {
            id: "ich_splits_wide_char",
            cols: 8,
            rows: 1,
            bytes: b"A\xe4\xb8\xadBC\x1b[1;3H\x1b[1@",
        },
        SupportedFixture {
            id: "dch_half_wide_char",
            cols: 8,
            rows: 1,
            bytes: b"A\xe4\xb8\xadBCD\x1b[1;2H\x1b[1P",
        },
        SupportedFixture {
            id: "dch_at_continuation",
            cols: 8,
            rows: 1,
            bytes: b"A\xe4\xb8\xadBC\x1b[1;3H\x1b[1P",
        },
        SupportedFixture {
            id: "el_right_from_wide_continuation",
            cols: 8,
            rows: 1,
            bytes: b"A\xe4\xb8\xadBCD\x1b[1;3H\x1b[0K",
        },
        SupportedFixture {
            id: "el_left_through_wide_head",
            cols: 8,
            rows: 1,
            bytes: b"A\xe4\xb8\xadBC\x1b[1;2H\x1b[1K",
        },
    ]
}

fn parse_known_mismatch_fixtures() -> Vec<KnownMismatchFixture> {
    let mut fixtures = Vec::new();
    for line in KNOWN_MISMATCHES_FIXTURE.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parsed = parse_known_mismatch_line(trimmed);
        assert!(
            parsed.is_ok(),
            "invalid known-mismatch fixture line: {trimmed}"
        );
        if let Ok(fixture) = parsed {
            fixtures.push(fixture);
        }
    }
    fixtures
}

fn parse_known_mismatch_line(line: &str) -> Result<KnownMismatchFixture, String> {
    let mut parts = line.splitn(5, '|');
    let id = parts.next().ok_or("fixture id missing")?.trim().to_string();
    let cols = parts
        .next()
        .ok_or("fixture cols missing")?
        .trim()
        .parse::<u16>()
        .map_err(|error| format!("fixture cols must be a u16: {error}"))?;
    let rows = parts
        .next()
        .ok_or("fixture rows missing")?
        .trim()
        .parse::<u16>()
        .map_err(|error| format!("fixture rows must be a u16: {error}"))?;
    let input_hex = parts.next().ok_or("fixture input hex missing")?.trim();
    let root_cause = parts
        .next()
        .ok_or("fixture root cause missing")?
        .trim()
        .to_string();
    Ok(KnownMismatchFixture {
        id,
        cols,
        rows,
        bytes: decode_hex(input_hex)?,
        root_cause,
    })
}

fn decode_hex(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err(format!("hex payload must have even length: {hex}"));
    }
    let bytes = hex.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = decode_nibble(pair[0])?;
        let lo = decode_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn decode_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(format!("invalid hex nibble: {byte}")),
    }
}

#[test]
fn differential_supported_subset_matches_virtual_terminal_reference() {
    for fixture in supported_fixtures() {
        let core = run_core_snapshot(fixture.bytes, fixture.cols, fixture.rows);
        let reference = run_reference_snapshot(fixture.bytes, fixture.cols, fixture.rows);
        assert_eq!(
            core, reference,
            "fixture {} diverged unexpectedly",
            fixture.id
        );
    }
}

#[test]
fn differential_known_mismatches_are_tracked_with_root_cause_notes() {
    let fixtures = parse_known_mismatch_fixtures();
    // Empty is allowed: means reference model parity is complete for tracked cases.

    for fixture in fixtures {
        let core = run_core_snapshot(&fixture.bytes, fixture.cols, fixture.rows);
        let reference = run_reference_snapshot(&fixture.bytes, fixture.cols, fixture.rows);
        assert_ne!(
            core, reference,
            "known mismatch fixture {} unexpectedly matched; review and move it to supported fixtures",
            fixture.id
        );
        assert!(
            !fixture.root_cause.is_empty(),
            "known mismatch fixture {} must carry a root-cause note",
            fixture.id
        );
    }
}

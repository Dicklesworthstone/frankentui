use frankenterm_core::{Action, Cell, Cursor, Grid, Modes, Parser, Scrollback, translate_charset};
use ftui_extras::terminal::{AnsiHandler, AnsiParser, ClearRegion, TerminalState};

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalSnapshot {
    screen_text: String,
    cursor_row: u16,
    cursor_col: u16,
}

#[derive(Debug)]
struct CoreTerminalHarness {
    parser: Parser,
    grid: Grid,
    cursor: Cursor,
    scrollback: Scrollback,
    modes: Modes,
    last_printed: Option<char>,
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
            Action::CursorColumn(col) => {
                self.cursor
                    .move_to(self.cursor.row, col, self.rows, self.cols);
            }
            Action::CursorRow(row) => {
                self.cursor
                    .move_to(row, self.cursor.col, self.rows, self.cols);
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
                self.grid.fill_all('E');
                self.cursor.reset_scroll_region(self.rows);
                self.cursor.move_to(0, 0, self.rows, self.cols);
            }
            Action::RepeatChar(count) => {
                if let Some(ch) = self.last_printed {
                    for _ in 0..count {
                        self.apply_print(ch);
                    }
                }
            }
            Action::SetCursorShape(_) => {}
            Action::SoftReset => {
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
            Action::Escape(_) => {}
            Action::DecSet(params) => {
                for &p in &params {
                    self.modes.set_dec_mode(p, true);
                }
            }
            Action::DecRst(params) => {
                for &p in &params {
                    self.modes.set_dec_mode(p, false);
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
            Action::SaveCursor | Action::RestoreCursor => {}
            Action::Index => {
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
            }
            Action::ReverseIndex => {
                if self.cursor.row == self.cursor.scroll_top() {
                    self.grid.scroll_down(
                        self.cursor.scroll_top(),
                        self.cursor.scroll_bottom(),
                        1,
                        self.cursor.attrs.bg,
                    );
                } else if self.cursor.row > 0 {
                    self.cursor.row -= 1;
                }
            }
            Action::NextLine => {
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
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
            }
            Action::FullReset => {
                self.grid = Grid::new(self.cols, self.rows);
                self.cursor = Cursor::new(self.cols, self.rows);
                self.scrollback = Scrollback::new(512);
                self.modes = Modes::new();
                self.last_printed = None;
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
            self.wrap_to_next_line();
        }

        let width = Cell::display_width(ch);
        if width == 0 {
            return;
        }

        if width == 2 && self.cursor.col + 1 >= self.cols {
            self.wrap_to_next_line();
        }

        let written =
            self.grid
                .write_printable(self.cursor.row, self.cursor.col, ch, self.cursor.attrs);
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
        TerminalSnapshot {
            screen_text: render_core_screen_text(&self.grid, self.cols, self.rows),
            cursor_row: self.cursor.row,
            cursor_col: self.cursor.col,
        }
    }
}

#[derive(Debug)]
struct ExtrasTerminalHarness {
    parser: AnsiParser,
    state: TerminalState,
}

impl ExtrasTerminalHarness {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            parser: AnsiParser::new(),
            state: TerminalState::new(cols, rows),
        }
    }

    fn feed_bytes(&mut self, bytes: &[u8]) {
        struct Handler<'a> {
            state: &'a mut TerminalState,
        }

        impl AnsiHandler for Handler<'_> {
            fn print(&mut self, ch: char) {
                self.state.put_char(ch);
            }

            fn execute(&mut self, byte: u8) {
                match byte {
                    0x08 => self.state.move_cursor_relative(-1, 0),
                    0x09 => {
                        let x = self.state.cursor().x;
                        let next = ((x / 8) + 1) * 8;
                        self.state.move_cursor(next, self.state.cursor().y);
                    }
                    0x0A..=0x0C => {
                        let cursor = self.state.cursor();
                        if cursor.y + 1 >= self.state.height() {
                            self.state.scroll_up(1);
                        } else {
                            self.state.move_cursor_relative(0, 1);
                        }
                    }
                    0x0D => self.state.move_cursor(0, self.state.cursor().y),
                    _ => {}
                }
            }

            fn csi_dispatch(&mut self, params: &[i64], intermediates: &[u8], action: char) {
                match (action, intermediates) {
                    ('H', []) | ('f', []) => {
                        let row = params.first().copied().unwrap_or(1).max(1) as u16;
                        let col = params.get(1).copied().unwrap_or(1).max(1) as u16;
                        self.state
                            .move_cursor(col.saturating_sub(1), row.saturating_sub(1));
                    }
                    ('A', []) => {
                        let n = params.first().copied().unwrap_or(1).max(1) as i16;
                        self.state.move_cursor_relative(0, -n);
                    }
                    ('B', []) => {
                        let n = params.first().copied().unwrap_or(1).max(1) as i16;
                        self.state.move_cursor_relative(0, n);
                    }
                    ('C', []) => {
                        let n = params.first().copied().unwrap_or(1).max(1) as i16;
                        self.state.move_cursor_relative(n, 0);
                    }
                    ('D', []) => {
                        let n = params.first().copied().unwrap_or(1).max(1) as i16;
                        self.state.move_cursor_relative(-n, 0);
                    }
                    ('J', []) => {
                        let mode = params.first().copied().unwrap_or(0);
                        match mode {
                            0 => self.state.clear_region(ClearRegion::CursorToEnd),
                            1 => self.state.clear_region(ClearRegion::StartToCursor),
                            2 | 3 => self.state.clear_region(ClearRegion::All),
                            _ => {}
                        }
                    }
                    ('K', []) => {
                        let mode = params.first().copied().unwrap_or(0);
                        match mode {
                            0 => self.state.clear_region(ClearRegion::LineFromCursor),
                            1 => self.state.clear_region(ClearRegion::LineToCursor),
                            2 => self.state.clear_region(ClearRegion::Line),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            fn osc_dispatch(&mut self, _params: &[&[u8]]) {}

            fn esc_dispatch(&mut self, _intermediates: &[u8], _c: char) {}
        }

        let mut handler = Handler {
            state: &mut self.state,
        };
        self.parser.parse(bytes, &mut handler);
    }

    fn snapshot(&self) -> TerminalSnapshot {
        TerminalSnapshot {
            screen_text: render_extras_screen_text(&self.state),
            cursor_row: self.state.cursor().y,
            cursor_col: self.state.cursor().x,
        }
    }
}

#[derive(Debug)]
struct Fixture {
    id: &'static str,
    cols: u16,
    rows: u16,
    bytes: &'static [u8],
}

fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            id: "plain_ascii",
            cols: 20,
            rows: 4,
            bytes: b"hello",
        },
        Fixture {
            id: "newline_preserves_column",
            cols: 20,
            rows: 4,
            bytes: b"hi\nthere",
        },
        Fixture {
            id: "carriage_return_overwrite",
            cols: 20,
            rows: 4,
            bytes: b"ABCDE\rZ",
        },
        Fixture {
            id: "tab_to_default_stop",
            cols: 20,
            rows: 4,
            bytes: b"A\tB",
        },
        Fixture {
            id: "backspace_overwrite",
            cols: 20,
            rows: 4,
            bytes: b"abc\x08d",
        },
        Fixture {
            id: "csi_cup_reposition",
            cols: 10,
            rows: 3,
            bytes: b"Hello\x1b[2;3HX",
        },
        Fixture {
            id: "csi_erase_line_right",
            cols: 10,
            rows: 3,
            bytes: b"ABCDE\x1b[1;4H\x1b[0K",
        },
        Fixture {
            id: "csi_erase_display",
            cols: 10,
            rows: 3,
            bytes: b"AB\x1b[2JZ",
        },
        Fixture {
            id: "csi_cursor_relative_moves",
            cols: 10,
            rows: 3,
            bytes: b"abc\x1b[1;1H\x1b[2C\x1b[1B\x1b[1D\x1b[1AX",
        },
    ]
}

fn run_core_snapshot(input: &[u8], cols: u16, rows: u16) -> TerminalSnapshot {
    let mut harness = CoreTerminalHarness::new(cols, rows);
    harness.feed_bytes(input);
    harness.snapshot()
}

fn run_extras_snapshot(input: &[u8], cols: u16, rows: u16) -> TerminalSnapshot {
    let mut harness = ExtrasTerminalHarness::new(cols, rows);
    harness.feed_bytes(input);
    harness.snapshot()
}

fn render_core_screen_text(grid: &Grid, cols: u16, rows: u16) -> String {
    (0..rows)
        .map(|row| {
            let mut line = String::with_capacity(cols as usize);
            for col in 0..cols {
                let ch = grid
                    .cell(row, col)
                    .map_or(' ', frankenterm_core::Cell::content);
                line.push(ch);
            }
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_extras_screen_text(state: &TerminalState) -> String {
    (0..state.height())
        .map(|row| {
            let mut line = String::with_capacity(state.width() as usize);
            for col in 0..state.width() {
                let ch = state.cell(col, row).map_or(' ', |cell| cell.ch);
                line.push(ch);
            }
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn differential_core_matches_extras_terminal_reference_for_supported_subset() {
    for fixture in fixtures() {
        let core = run_core_snapshot(fixture.bytes, fixture.cols, fixture.rows);
        let extras = run_extras_snapshot(fixture.bytes, fixture.cols, fixture.rows);
        assert_eq!(
            core, extras,
            "fixture {} diverged between frankenterm-core and ftui-extras reference",
            fixture.id
        );
    }
}

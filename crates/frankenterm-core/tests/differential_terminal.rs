use frankenterm_core::{Action, Cell, Cursor, Grid, Modes, Parser, Scrollback, translate_charset};
use ftui_pty::virtual_terminal::VirtualTerminal;

const KNOWN_MISMATCHES_FIXTURE: &str =
    include_str!("../../../tests/fixtures/vt-conformance/differential/known_mismatches.tsv");

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
            Action::SaveCursor | Action::RestoreCursor => {
                // Cursor save/restore not applied in the baseline harness.
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
            screen_text: self.screen_text(),
            cursor_row: self.cursor.row,
            cursor_col: self.cursor.col,
        }
    }

    fn screen_text(&self) -> String {
        (0..self.rows)
            .map(|row| {
                let mut line = String::with_capacity(self.cols as usize);
                for col in 0..self.cols {
                    let ch = self
                        .grid
                        .cell(row, col)
                        .map_or(' ', frankenterm_core::Cell::content);
                    line.push(ch);
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
    TerminalSnapshot {
        screen_text: vt.screen_text(),
        cursor_row,
        cursor_col,
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

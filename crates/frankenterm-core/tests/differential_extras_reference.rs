use frankenterm_core::{Grid, TerminalEngine};
use ftui_extras::terminal::{AnsiHandler, AnsiParser, ClearRegion, TerminalState};

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalSnapshot {
    screen_text: String,
    cursor_row: u16,
    cursor_col: u16,
}

#[derive(Debug)]
struct CoreTerminalHarness {
    engine: TerminalEngine,
}

impl CoreTerminalHarness {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            engine: TerminalEngine::new(cols, rows),
        }
    }

    fn feed_bytes(&mut self, bytes: &[u8]) {
        self.engine.feed_bytes(bytes);
    }

    fn snapshot(&self) -> TerminalSnapshot {
        TerminalSnapshot {
            screen_text: render_core_screen_text(
                self.engine.grid(),
                self.engine.cols(),
                self.engine.rows(),
            ),
            cursor_row: self.engine.cursor().row,
            cursor_col: self.engine.cursor().col,
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

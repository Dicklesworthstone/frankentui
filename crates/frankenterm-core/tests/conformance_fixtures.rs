use std::path::{Path, PathBuf};

use frankenterm_core::{Action, Cursor, Grid, Parser, SavedCursor, Scrollback, SgrFlags};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    #[allow(dead_code)]
    description: String,
    initial_size: [u16; 2],
    input_bytes_hex: String,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct Expected {
    cursor: CursorPos,
    cells: Vec<CellExpectation>,
}

#[derive(Debug, Deserialize)]
struct CursorPos {
    row: u16,
    col: u16,
}

#[derive(Debug, Deserialize)]
struct CellExpectation {
    row: u16,
    col: u16,
    #[serde(rename = "char")]
    ch: String,
    #[serde(default)]
    attrs: Option<AttrExpectation>,
}

#[derive(Debug, Deserialize, Default)]
struct AttrExpectation {
    #[serde(default)]
    bold: bool,
    #[serde(default)]
    dim: bool,
    #[serde(default)]
    italic: bool,
    #[serde(default)]
    underline: bool,
    #[serde(default)]
    blink: bool,
    #[serde(default)]
    inverse: bool,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    strikethrough: bool,
    #[serde(default)]
    overline: bool,
}

#[derive(Debug)]
struct CoreTerminalHarness {
    parser: Parser,
    grid: Grid,
    cursor: Cursor,
    saved_cursor: SavedCursor,
    scrollback: Scrollback,
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
            saved_cursor: SavedCursor::default(),
            scrollback: Scrollback::new(512),
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
            ),
            Action::ScrollDown(count) => {
                self.grid
                    .scroll_down(self.cursor.scroll_top(), self.cursor.scroll_bottom(), count)
            }
            Action::InsertLines(count) => {
                self.grid.insert_lines(
                    self.cursor.row,
                    count,
                    self.cursor.scroll_top(),
                    self.cursor.scroll_bottom(),
                );
                self.cursor.pending_wrap = false;
            }
            Action::DeleteLines(count) => {
                self.grid.delete_lines(
                    self.cursor.row,
                    count,
                    self.cursor.scroll_top(),
                    self.cursor.scroll_bottom(),
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
            // Mode toggles are currently not modeled in this conformance harness.
            Action::DecSet(_) | Action::DecRst(_) | Action::AnsiSet(_) | Action::AnsiRst(_) => {}
            Action::SaveCursor => self.saved_cursor = SavedCursor::save(&self.cursor, false),
            Action::RestoreCursor => self.saved_cursor.restore(&mut self.cursor),
            Action::Index => {
                if self.cursor.row + 1 >= self.cursor.scroll_bottom() {
                    self.grid.scroll_up_into(
                        self.cursor.scroll_top(),
                        self.cursor.scroll_bottom(),
                        1,
                        &mut self.scrollback,
                    );
                } else if self.cursor.row + 1 < self.rows {
                    self.cursor.row += 1;
                }
                self.cursor.pending_wrap = false;
            }
            Action::ReverseIndex => {
                if self.cursor.row == self.cursor.scroll_top() {
                    self.grid
                        .scroll_down(self.cursor.scroll_top(), self.cursor.scroll_bottom(), 1);
                } else {
                    self.cursor.move_up(1);
                }
                self.cursor.pending_wrap = false;
            }
            Action::NextLine => {
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
                self.apply_action(Action::Index);
            }
            Action::FullReset => {
                self.grid = Grid::new(self.cols, self.rows);
                self.cursor = Cursor::new(self.cols, self.rows);
                self.saved_cursor = SavedCursor::default();
                self.scrollback = Scrollback::new(512);
            }
            Action::SetTitle(_) | Action::HyperlinkStart(_) | Action::HyperlinkEnd => {}
            Action::Escape(_) => {}
        }
    }

    fn apply_print(&mut self, ch: char) {
        if self.cursor.pending_wrap {
            self.wrap_to_next_line();
        }
        if let Some(cell) = self.grid.cell_mut(self.cursor.row, self.cursor.col) {
            cell.set_content(ch, 1);
            cell.attrs = self.cursor.attrs;
        }
        if self.cursor.col + 1 >= self.cols {
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.col += 1;
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
            );
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
        self.cursor.pending_wrap = false;
    }
}

#[test]
fn vt_conformance_fixtures_replay() -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/vt-conformance");
    let mut paths = collect_fixture_paths(&root)?;
    paths.sort();
    if paths.is_empty() {
        return Err(format!(
            "no vt-conformance fixtures found under {}",
            root.display()
        ));
    }

    let mut failures = Vec::new();
    for path in paths {
        if let Err(err) = run_fixture(&path) {
            failures.push(format!("{}: {err}", path.display()));
        }
    }

    if !failures.is_empty() {
        return Err(format!(
            "vt-conformance fixtures failed:\n{}",
            failures.join("\n")
        ));
    }

    Ok(())
}

fn collect_fixture_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let rd = std::fs::read_dir(root)
        .map_err(|e| format!("failed to read fixture root {}: {e}", root.display()))?;
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let sub_rd = std::fs::read_dir(&path)
            .map_err(|e| format!("failed to read fixture dir {}: {e}", path.display()))?;
        for sub_entry in sub_rd.flatten() {
            let sub_path = sub_entry.path();
            if sub_path.extension().and_then(|s| s.to_str()) == Some("json") {
                out.push(sub_path);
            }
        }
    }
    Ok(out)
}

fn run_fixture(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let fixture: Fixture = serde_json::from_str(&text).map_err(|e| e.to_string())?;

    let cols = fixture.initial_size[0];
    let rows = fixture.initial_size[1];
    let bytes = decode_hex(&fixture.input_bytes_hex)?;

    let mut term = CoreTerminalHarness::new(cols, rows);
    term.feed_bytes(&bytes);

    if term.cursor.row != fixture.expected.cursor.row
        || term.cursor.col != fixture.expected.cursor.col
    {
        return Err(format!(
            "{}: cursor mismatch: got ({},{}), expected ({},{})",
            fixture.name,
            term.cursor.row,
            term.cursor.col,
            fixture.expected.cursor.row,
            fixture.expected.cursor.col
        ));
    }

    for exp in &fixture.expected.cells {
        let got = term.grid.cell(exp.row, exp.col).ok_or_else(|| {
            format!(
                "{}: cell out of bounds ({},{})",
                fixture.name, exp.row, exp.col
            )
        })?;
        let mut expected_chars = exp.ch.chars();
        let expected_ch = expected_chars
            .next()
            .ok_or_else(|| format!("{}: empty expected char string", fixture.name))?;
        if expected_chars.next().is_some() {
            return Err(format!(
                "{}: expected char string must be 1 char, got {:?}",
                fixture.name, exp.ch
            ));
        }
        if got.content() != expected_ch {
            return Err(format!(
                "{}: char mismatch at ({},{}): got {:?}, expected {:?}",
                fixture.name,
                exp.row,
                exp.col,
                got.content(),
                expected_ch
            ));
        }

        if let Some(attrs) = &exp.attrs {
            let flags = got.attrs.flags;
            assert_flag(
                fixture.name.as_str(),
                exp.row,
                exp.col,
                "bold",
                flags,
                SgrFlags::BOLD,
                attrs.bold,
            )?;
            assert_flag(
                fixture.name.as_str(),
                exp.row,
                exp.col,
                "dim",
                flags,
                SgrFlags::DIM,
                attrs.dim,
            )?;
            assert_flag(
                fixture.name.as_str(),
                exp.row,
                exp.col,
                "italic",
                flags,
                SgrFlags::ITALIC,
                attrs.italic,
            )?;
            assert_flag(
                fixture.name.as_str(),
                exp.row,
                exp.col,
                "underline",
                flags,
                SgrFlags::UNDERLINE,
                attrs.underline,
            )?;
            assert_flag(
                fixture.name.as_str(),
                exp.row,
                exp.col,
                "blink",
                flags,
                SgrFlags::BLINK,
                attrs.blink,
            )?;
            assert_flag(
                fixture.name.as_str(),
                exp.row,
                exp.col,
                "inverse",
                flags,
                SgrFlags::INVERSE,
                attrs.inverse,
            )?;
            assert_flag(
                fixture.name.as_str(),
                exp.row,
                exp.col,
                "hidden",
                flags,
                SgrFlags::HIDDEN,
                attrs.hidden,
            )?;
            assert_flag(
                fixture.name.as_str(),
                exp.row,
                exp.col,
                "strikethrough",
                flags,
                SgrFlags::STRIKETHROUGH,
                attrs.strikethrough,
            )?;
            assert_flag(
                fixture.name.as_str(),
                exp.row,
                exp.col,
                "overline",
                flags,
                SgrFlags::OVERLINE,
                attrs.overline,
            )?;
        }
    }

    Ok(())
}

fn assert_flag(
    fixture: &str,
    row: u16,
    col: u16,
    label: &str,
    flags: SgrFlags,
    flag: SgrFlags,
    expected: bool,
) -> Result<(), String> {
    let got = flags.contains(flag);
    if got == expected {
        return Ok(());
    }
    Err(format!(
        "{fixture}: attr mismatch at ({row},{col}) for {label}: got {got}, expected {expected}"
    ))
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !compact.len().is_multiple_of(2) {
        return Err("hex string must have even length".to_string());
    }
    let mut out = Vec::with_capacity(compact.len() / 2);
    let bytes = compact.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = (bytes[i] as char)
            .to_digit(16)
            .ok_or_else(|| "bad hex".to_string())?;
        let lo = (bytes[i + 1] as char)
            .to_digit(16)
            .ok_or_else(|| "bad hex".to_string())?;
        out.push(((hi << 4) | lo) as u8);
    }
    Ok(out)
}

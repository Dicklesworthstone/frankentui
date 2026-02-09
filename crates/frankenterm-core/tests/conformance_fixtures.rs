use std::path::{Path, PathBuf};

use frankenterm_core::{
    Action, Cell, Color, Cursor, Grid, Modes, Parser, SavedCursor, Scrollback, SgrFlags,
    translate_charset,
};
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
    #[serde(default)]
    fg_color: Option<ColorExpectation>,
    #[serde(default)]
    bg_color: Option<ColorExpectation>,
}

/// JSON-friendly representation of a terminal color for fixture expectations.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ColorExpectation {
    Default,
    Named(u8),
    Indexed(u8),
    Rgb([u8; 3]),
}

impl ColorExpectation {
    fn matches(&self, color: Color) -> bool {
        match (self, color) {
            (ColorExpectation::Default, Color::Default) => true,
            (ColorExpectation::Named(n), Color::Named(c)) => *n == c,
            (ColorExpectation::Indexed(n), Color::Indexed(c)) => *n == c,
            (ColorExpectation::Rgb([r, g, b]), Color::Rgb(cr, cg, cb)) => {
                *r == cr && *g == cg && *b == cb
            }
            _ => false,
        }
    }

    fn describe(&self) -> String {
        match self {
            ColorExpectation::Default => "default".to_string(),
            ColorExpectation::Named(n) => format!("named({n})"),
            ColorExpectation::Indexed(n) => format!("indexed({n})"),
            ColorExpectation::Rgb([r, g, b]) => format!("rgb({r},{g},{b})"),
        }
    }
}

#[derive(Debug)]
struct CoreTerminalHarness {
    parser: Parser,
    grid: Grid,
    cursor: Cursor,
    saved_cursor: SavedCursor,
    scrollback: Scrollback,
    modes: Modes,
    /// Last printed graphic character, used for REP (CSI b).
    last_char: Option<char>,
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
            modes: Modes::new(),
            last_char: None,
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
                if self.modes.origin_mode() {
                    let abs_row = row.saturating_add(self.cursor.scroll_top());
                    self.cursor.row = abs_row.min(self.cursor.scroll_bottom().saturating_sub(1));
                    self.cursor.pending_wrap = false;
                } else {
                    self.cursor
                        .move_to(row, self.cursor.col, self.rows, self.cols);
                }
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
                // DECOM: cursor homes to top of scroll region; otherwise (0,0).
                if self.modes.origin_mode() {
                    self.cursor.row = self.cursor.scroll_top();
                    self.cursor.col = 0;
                    self.cursor.pending_wrap = false;
                } else {
                    self.cursor.move_to(0, 0, self.rows, self.cols);
                }
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
            Action::CursorPosition { row, col } => {
                if self.modes.origin_mode() {
                    let abs_row = row.saturating_add(self.cursor.scroll_top());
                    self.cursor.row = abs_row.min(self.cursor.scroll_bottom().saturating_sub(1));
                    self.cursor.col = col.min(self.cols.saturating_sub(1));
                    self.cursor.pending_wrap = false;
                } else {
                    self.cursor.move_to(row, col, self.rows, self.cols);
                }
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
            Action::SaveCursor => {
                self.saved_cursor = SavedCursor::save(&self.cursor, self.modes.origin_mode());
            }
            Action::RestoreCursor => self.saved_cursor.restore(&mut self.cursor),
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
                self.cursor.pending_wrap = false;
            }
            Action::ReverseIndex => {
                if self.cursor.row == self.cursor.scroll_top() {
                    self.grid.scroll_down(
                        self.cursor.scroll_top(),
                        self.cursor.scroll_bottom(),
                        1,
                        self.cursor.attrs.bg,
                    );
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
                self.modes.reset();
            }
            Action::SetTitle(_) | Action::HyperlinkStart(_) | Action::HyperlinkEnd => {}
            Action::SetTabStop => {
                self.cursor.set_tab_stop();
            }
            Action::ClearTabStop(mode) => match mode {
                0 => self.cursor.clear_tab_stop(),
                3 | 5 => self.cursor.clear_all_tab_stops(),
                _ => {}
            },
            Action::BackTab(count) => {
                for _ in 0..count {
                    self.cursor.col = self.cursor.prev_tab_stop();
                }
                self.cursor.pending_wrap = false;
            }
            Action::EraseChars(count) => {
                self.grid.erase_chars(
                    self.cursor.row,
                    self.cursor.col,
                    count,
                    self.cursor.attrs.bg,
                );
            }
            Action::ScreenAlignment => {
                // DECALN: fill entire grid with 'E'
                for row in 0..self.rows {
                    for col in 0..self.cols {
                        if let Some(cell) = self.grid.cell_mut(row, col) {
                            cell.set_content('E', 1);
                        }
                    }
                }
                self.cursor.move_to(0, 0, self.rows, self.cols);
            }
            Action::RepeatChar(count) => {
                if let Some(ch) = self.last_char {
                    for _ in 0..count {
                        self.apply_print(ch);
                    }
                }
            }
            // Keypad mode changes tracked but not applied in conformance harness.
            Action::ApplicationKeypad | Action::NormalKeypad => {}
            // Cursor shape changes tracked but not applied in conformance harness.
            Action::SetCursorShape(_) => {}
            Action::SoftReset => {
                // DECSTR: reset modes, SGR, scroll region, cursor visibility, charset.
                // Unlike RIS, soft reset does NOT clear the screen or scrollback.
                self.modes = Modes::new();
                self.cursor.attrs = Default::default();
                self.cursor.set_scroll_region(0, self.rows, self.rows);
                self.cursor.pending_wrap = false;
                self.cursor.reset_charset();
            }
            // EraseScrollback clears scrollback buffer; no visible effect in grid.
            Action::EraseScrollback => {}
            // Focus/paste events are input-side; no grid effect.
            Action::FocusIn | Action::FocusOut => {}
            Action::PasteStart | Action::PasteEnd => {}
            // Device attribute queries produce reply bytes; no grid effect.
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
            // Mouse events are input-side; no grid effect.
            Action::MouseEvent { .. } => {}
            Action::Escape(_) => {}
        }
    }

    fn apply_print(&mut self, ch: char) {
        // Apply charset translation (DEC Graphics, etc.).
        let charset = self.cursor.effective_charset();
        let ch = translate_charset(ch, charset);
        self.cursor.consume_single_shift();
        self.last_char = Some(ch);
        if self.cursor.pending_wrap {
            if self.modes.autowrap() {
                self.wrap_to_next_line();
            } else {
                // DECAWM off: overwrite rightmost column, stay at margin
                self.cursor.pending_wrap = false;
            }
        }

        let width = Cell::display_width(ch);
        if width == 0 {
            // Fallback strategy for non-spacing scalars (combining marks/ZWJ/VS):
            // keep deterministic cursor state and leave the grid unchanged.
            return;
        }

        if width == 2 && self.cursor.col + 1 >= self.cols {
            if self.modes.autowrap() {
                self.wrap_to_next_line();
            } else {
                self.cursor.pending_wrap = false;
                return;
            }
        }

        // IRM: insert mode shifts chars right before writing
        if self.modes.insert_mode() {
            self.grid.insert_chars(
                self.cursor.row,
                self.cursor.col,
                u16::from(width),
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
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.col += u16::from(written);
            self.cursor.pending_wrap = false;
        }
    }

    fn apply_newline(&mut self) {
        // LNM (mode 20): if enabled, newline implies carriage return
        if self
            .modes
            .ansi
            .contains(frankenterm_core::AnsiModes::LINEFEED_NEWLINE)
        {
            self.cursor.col = 0;
        }
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

            if let Some(expected_fg) = &attrs.fg_color {
                let got_fg = got.attrs.fg;
                if !expected_fg.matches(got_fg) {
                    return Err(format!(
                        "{}: fg color mismatch at ({},{}): got {}, expected {}",
                        fixture.name,
                        exp.row,
                        exp.col,
                        describe_color(got_fg),
                        expected_fg.describe()
                    ));
                }
            }
            if let Some(expected_bg) = &attrs.bg_color {
                let got_bg = got.attrs.bg;
                if !expected_bg.matches(got_bg) {
                    return Err(format!(
                        "{}: bg color mismatch at ({},{}): got {}, expected {}",
                        fixture.name,
                        exp.row,
                        exp.col,
                        describe_color(got_bg),
                        expected_bg.describe()
                    ));
                }
            }
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

fn describe_color(color: Color) -> String {
    match color {
        Color::Default => "default".to_string(),
        Color::Named(n) => format!("named({n})"),
        Color::Indexed(n) => format!("indexed({n})"),
        Color::Rgb(r, g, b) => format!("rgb({r},{g},{b})"),
    }
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

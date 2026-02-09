//! JSON-driven VT conformance tests for scroll region behavior.
//!
//! Loads fixtures from `tests/fixtures/vt-conformance/scroll_region/` and
//! runs each through both the frankenterm-core engine and the VirtualTerminal
//! reference model, then validates against expected cursor/cell state.
//!
//! Fixture JSON schema:
//! ```json
//! {
//!   "name": "fixture_id",
//!   "description": "human-readable description",
//!   "initial_size": [cols, rows],
//!   "input_bytes_hex": "hex-encoded byte sequence",
//!   "expected": {
//!     "cursor": { "row": int, "col": int },
//!     "cells": [
//!       { "row": int, "col": int, "char": "c" },
//!       { "row": int, "col": int, "char": "c", "attrs": { "fg_color": { "named": int } } }
//!     ]
//!   }
//! }
//! ```
//!
//! # Evidence Ledger
//!
//! | Claim | Evidence |
//! |-------|----------|
//! | Fixtures test scroll region boundary behavior | 26 JSON fixtures cover DECSTBM, SU/SD, IL/DL, autowrap, DECSTR, save/restore |
//! | Both engines agree on expected output | Differential comparison between core and reference |
//! | Expected state is verified | Cursor position + specific cell content + optional SGR attrs |

use frankenterm_core::{
    Action, Cell, Cursor, Grid, Modes, Parser, SavedCursor, Scrollback, translate_charset,
};
use ftui_pty::virtual_terminal::VirtualTerminal;
use serde::Deserialize;
use std::path::PathBuf;

// ── Fixture schema ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    #[allow(dead_code)]
    description: String,
    initial_size: [u16; 2],
    input_bytes_hex: String,
    #[allow(dead_code)]
    comment: Option<String>,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct Expected {
    cursor: CursorExpect,
    cells: Vec<CellExpect>,
}

#[derive(Debug, Deserialize)]
struct CursorExpect {
    row: u16,
    col: u16,
}

#[derive(Debug, Deserialize)]
struct CellExpect {
    row: u16,
    col: u16,
    char: String,
    attrs: Option<AttrsExpect>,
}

#[derive(Debug, Deserialize)]
struct AttrsExpect {
    fg_color: Option<ColorExpect>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ColorExpect {
    Named { named: u8 },
}

// ── Hex decoding ────────────────────────────────────────────────────────────

fn decode_hex(hex: &str) -> Vec<u8> {
    // Strip whitespace for readability in fixtures.
    let hex: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        hex.len().is_multiple_of(2),
        "hex payload must have even length: {hex}"
    );
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let hi = decode_nibble(pair[0]);
            let lo = decode_nibble(pair[1]);
            (hi << 4) | lo
        })
        .collect()
}

fn decode_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => panic!("invalid hex nibble: {byte}"),
    }
}

// ── Core terminal harness (mirrors differential_terminal.rs) ────────────────

struct CoreHarness {
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

impl CoreHarness {
    fn new(cols: u16, rows: u16) -> Self {
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
            Action::RestoreCursor => {
                self.saved_cursor.restore(&mut self.cursor);
            }
            Action::Index => self.apply_newline(),
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
            Action::ScreenAlignment => {
                self.grid.fill_all('E');
                self.cursor.reset_scroll_region(self.rows);
                self.cursor.move_to(0, 0, self.rows, self.cols);
            }
            Action::SoftReset => {
                self.modes.reset();
                self.cursor.attrs = frankenterm_core::SgrAttrs::default();
                self.cursor.reset_charset();
                self.cursor.visible = true;
                self.cursor.pending_wrap = false;
                self.cursor.reset_scroll_region(self.rows);
            }
            Action::RepeatChar(count) => {
                if let Some(ch) = self.last_printed {
                    for _ in 0..count {
                        self.apply_print(ch);
                    }
                }
            }
            Action::DesignateCharset { slot, charset } => {
                self.cursor.designate_charset(slot, charset);
            }
            Action::SingleShift2 => {
                self.cursor.single_shift = Some(2);
            }
            Action::SingleShift3 => {
                self.cursor.single_shift = Some(3);
            }
            // Actions that don't affect grid/cursor state for testing purposes.
            Action::SetTitle(_)
            | Action::HyperlinkStart(_)
            | Action::HyperlinkEnd
            | Action::SetCursorShape(_)
            | Action::ApplicationKeypad
            | Action::NormalKeypad
            | Action::EraseScrollback
            | Action::FocusIn
            | Action::FocusOut
            | Action::PasteStart
            | Action::PasteEnd
            | Action::DeviceAttributes
            | Action::DeviceAttributesSecondary
            | Action::DeviceStatusReport
            | Action::CursorPositionReport
            | Action::MouseEvent { .. }
            | Action::Escape(_) => {}
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
        }
    }

    fn apply_print(&mut self, ch: char) {
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
}

// ── Fixture loading ─────────────────────────────────────────────────────────

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/vt-conformance/scroll_region")
}

fn load_fixtures() -> Vec<Fixture> {
    let dir = fixtures_dir();
    if !dir.exists() {
        return Vec::new();
    }

    let mut fixtures: Vec<Fixture> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read fixture directory {}: {e}", dir.display()))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                return None;
            }
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            let fixture: Fixture = serde_json::from_str(&content)
                .unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()));
            Some(fixture)
        })
        .collect();

    fixtures.sort_by(|a, b| a.name.cmp(&b.name));
    fixtures
}

// ── Validation helpers ──────────────────────────────────────────────────────

fn validate_core_against_expected(fixture: &Fixture, harness: &CoreHarness) -> Vec<String> {
    let mut failures = Vec::new();

    // Cursor position.
    if harness.cursor.row != fixture.expected.cursor.row {
        failures.push(format!(
            "cursor row: expected {}, got {}",
            fixture.expected.cursor.row, harness.cursor.row
        ));
    }
    if harness.cursor.col != fixture.expected.cursor.col {
        failures.push(format!(
            "cursor col: expected {}, got {}",
            fixture.expected.cursor.col, harness.cursor.col
        ));
    }

    // Cell content + attrs.
    for cell_expect in &fixture.expected.cells {
        let cell = harness.grid.cell(cell_expect.row, cell_expect.col);
        let expected_ch = cell_expect.char.chars().next().unwrap_or(' ');

        match cell {
            Some(cell) => {
                if cell.content() != expected_ch {
                    failures.push(format!(
                        "cell ({},{}) char: expected {:?}, got {:?}",
                        cell_expect.row,
                        cell_expect.col,
                        expected_ch,
                        cell.content()
                    ));
                }

                // Validate SGR attrs if specified.
                if let Some(attrs) = &cell_expect.attrs
                    && let Some(fg_expect) = &attrs.fg_color
                {
                    let ColorExpect::Named { named } = fg_expect;
                    let actual_fg = cell.attrs.fg;
                    let expected_fg = frankenterm_core::Color::Named(*named);
                    if actual_fg != expected_fg {
                        failures.push(format!(
                            "cell ({},{}) fg_color: expected Named({}), got {:?}",
                            cell_expect.row, cell_expect.col, named, actual_fg
                        ));
                    }
                }
            }
            None => {
                failures.push(format!(
                    "cell ({},{}) out of bounds (grid {}x{})",
                    cell_expect.row, cell_expect.col, harness.cols, harness.rows
                ));
            }
        }
    }

    failures
}

fn validate_reference_against_expected(fixture: &Fixture, vt: &VirtualTerminal) -> Vec<String> {
    let mut failures = Vec::new();
    let (cursor_col, cursor_row) = vt.cursor();

    if cursor_row != fixture.expected.cursor.row {
        failures.push(format!(
            "cursor row: expected {}, got {}",
            fixture.expected.cursor.row, cursor_row
        ));
    }
    if cursor_col != fixture.expected.cursor.col {
        failures.push(format!(
            "cursor col: expected {}, got {}",
            fixture.expected.cursor.col, cursor_col
        ));
    }

    for cell_expect in &fixture.expected.cells {
        let expected_ch = cell_expect.char.chars().next().unwrap_or(' ');

        // VirtualTerminal::cell_at takes (x=col, y=row).
        let vcell = vt.cell_at(cell_expect.col, cell_expect.row);
        match vcell {
            Some(vcell) => {
                if vcell.ch != expected_ch {
                    failures.push(format!(
                        "cell ({},{}) char: expected {:?}, got {:?}",
                        cell_expect.row, cell_expect.col, expected_ch, vcell.ch
                    ));
                }
            }
            None => {
                failures.push(format!(
                    "cell ({},{}) out of bounds",
                    cell_expect.row, cell_expect.col
                ));
            }
        }

        // Note: VirtualTerminal color checking is skipped since the reference
        // model uses a different color representation (ftui_pty::Color vs
        // frankenterm_core::Color). The core engine is the authoritative source
        // for SGR attribute validation.
    }

    failures
}

// ── Known failures ──────────────────────────────────────────────────────────

/// Fixtures that both engines fail because the underlying feature is not yet
/// implemented. These are tracked here (not silently skipped) so that when the
/// feature lands, the test will start passing and remind us to remove the entry.
const KNOWN_FAILURES: &[(&str, &str)] = &[];

fn is_known_failure(name: &str) -> bool {
    KNOWN_FAILURES.iter().any(|(n, _)| *n == name)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn scroll_region_fixtures_core_matches_expected() {
    let fixtures = load_fixtures();
    assert!(
        !fixtures.is_empty(),
        "no scroll region fixtures found in {}",
        fixtures_dir().display()
    );

    let mut total_failures = Vec::new();
    let mut known_failures_seen = Vec::new();

    for fixture in &fixtures {
        let [cols, rows] = fixture.initial_size;
        let bytes = decode_hex(&fixture.input_bytes_hex);

        let mut harness = CoreHarness::new(cols, rows);
        harness.feed_bytes(&bytes);

        let failures = validate_core_against_expected(fixture, &harness);
        if !failures.is_empty() {
            if is_known_failure(&fixture.name) {
                known_failures_seen.push(fixture.name.clone());
            } else {
                total_failures.push(format!(
                    "\n  fixture {:?} ({}):\n    {}",
                    fixture.name,
                    fixture.description,
                    failures.join("\n    ")
                ));
            }
        } else if is_known_failure(&fixture.name) {
            // Known failure started passing - remove from KNOWN_FAILURES.
            total_failures.push(format!(
                "\n  fixture {:?} is in KNOWN_FAILURES but now passes - remove it",
                fixture.name
            ));
        }
    }

    if !known_failures_seen.is_empty() {
        eprintln!(
            "  [known failures, core]: {}",
            known_failures_seen.join(", ")
        );
    }

    assert!(
        total_failures.is_empty(),
        "core engine failed {} fixture(s):{}",
        total_failures.len(),
        total_failures.join("")
    );
}

#[test]
fn scroll_region_fixtures_reference_matches_expected() {
    let fixtures = load_fixtures();
    assert!(
        !fixtures.is_empty(),
        "no scroll region fixtures found in {}",
        fixtures_dir().display()
    );

    let mut total_failures = Vec::new();
    let mut known_failures_seen = Vec::new();

    for fixture in &fixtures {
        let [cols, rows] = fixture.initial_size;
        let bytes = decode_hex(&fixture.input_bytes_hex);

        let mut vt = VirtualTerminal::new(cols, rows);
        vt.feed(&bytes);

        let failures = validate_reference_against_expected(fixture, &vt);
        if !failures.is_empty() {
            if is_known_failure(&fixture.name) {
                known_failures_seen.push(fixture.name.clone());
            } else {
                total_failures.push(format!(
                    "\n  fixture {:?} ({}):\n    {}",
                    fixture.name,
                    fixture.description,
                    failures.join("\n    ")
                ));
            }
        } else if is_known_failure(&fixture.name) {
            total_failures.push(format!(
                "\n  fixture {:?} is in KNOWN_FAILURES but now passes - remove it",
                fixture.name
            ));
        }
    }

    if !known_failures_seen.is_empty() {
        eprintln!(
            "  [known failures, ref]: {}",
            known_failures_seen.join(", ")
        );
    }

    assert!(
        total_failures.is_empty(),
        "reference engine failed {} fixture(s):{}",
        total_failures.len(),
        total_failures.join("")
    );
}

#[test]
fn scroll_region_fixtures_core_and_reference_agree() {
    let fixtures = load_fixtures();
    assert!(
        !fixtures.is_empty(),
        "no scroll region fixtures found in {}",
        fixtures_dir().display()
    );

    let mut divergences = Vec::new();

    for fixture in &fixtures {
        let [cols, rows] = fixture.initial_size;
        let bytes = decode_hex(&fixture.input_bytes_hex);

        // Run core engine.
        let mut harness = CoreHarness::new(cols, rows);
        harness.feed_bytes(&bytes);

        // Run reference model.
        let mut vt = VirtualTerminal::new(cols, rows);
        vt.feed(&bytes);

        // Compare cursor positions.
        let (ref_col, ref_row) = vt.cursor();
        if harness.cursor.row != ref_row || harness.cursor.col != ref_col {
            divergences.push(format!(
                "\n  fixture {:?}: cursor diverged - core ({},{}) vs ref ({},{})",
                fixture.name, harness.cursor.row, harness.cursor.col, ref_row, ref_col
            ));
            continue;
        }

        // Compare screen text (full grid comparison).
        let core_text = screen_text(&harness.grid, cols, rows);
        let ref_text = vt.screen_text();
        if core_text != ref_text {
            divergences.push(format!(
                "\n  fixture {:?}: screen text diverged\n    core: {:?}\n    ref:  {:?}",
                fixture.name, core_text, ref_text
            ));
        }
    }

    assert!(
        divergences.is_empty(),
        "core/reference diverged on {} fixture(s):{}",
        divergences.len(),
        divergences.join("")
    );
}

fn screen_text(grid: &Grid, cols: u16, rows: u16) -> String {
    (0..rows)
        .map(|row| {
            let mut line = String::with_capacity(cols as usize);
            for col in 0..cols {
                if let Some(cell) = grid.cell(row, col) {
                    if cell.is_wide_continuation() {
                        continue;
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

#[test]
fn fixture_count_is_at_least_twenty() {
    let fixtures = load_fixtures();
    assert!(
        fixtures.len() >= 20,
        "expected at least 20 scroll region fixtures, found {}",
        fixtures.len()
    );
}

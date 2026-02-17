use std::path::{Path, PathBuf};

use frankenterm_core::{Color, SgrFlags, TerminalEngine};
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

    fn cursor_row(&self) -> u16 {
        self.engine.cursor().row
    }

    fn cursor_col(&self) -> u16 {
        self.engine.cursor().col
    }

    fn cell(&self, row: u16, col: u16) -> Option<&frankenterm_core::Cell> {
        self.engine.grid().cell(row, col)
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

    if term.cursor_row() != fixture.expected.cursor.row
        || term.cursor_col() != fixture.expected.cursor.col
    {
        return Err(format!(
            "{}: cursor mismatch: got ({},{}), expected ({},{})",
            fixture.name,
            term.cursor_row(),
            term.cursor_col(),
            fixture.expected.cursor.row,
            fixture.expected.cursor.col
        ));
    }

    for exp in &fixture.expected.cells {
        let got = term.cell(exp.row, exp.col).ok_or_else(|| {
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

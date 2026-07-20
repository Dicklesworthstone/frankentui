#![cfg(unix)]

use std::collections::HashSet;
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ftui_pty::virtual_terminal::{CellStyle, Color, VirtualTerminal};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const FIXTURE_ROOT_REL: &str = "../../tests/fixtures/vt-conformance";
const KNOWN_MISMATCHES_REL: &str =
    "../../tests/fixtures/vt-conformance/differential/known_mismatches.tsv";
const JSONL_PATH_ENV: &str = "FTUI_VT_CONFORMANCE_JSONL";
const SUMMARY_PATH_ENV: &str = "FTUI_VT_CONFORMANCE_SUMMARY_JSON";
const RUN_ID_ENV: &str = "FTUI_VT_CONFORMANCE_RUN_ID";
const XTERM_DIFF_JSONL_PATH_ENV: &str = "FTUI_XTERM_DIFF_JSONL";
const XTERM_DIFF_SUMMARY_PATH_ENV: &str = "FTUI_XTERM_DIFF_SUMMARY_JSON";
const XTERM_DIFF_RUN_ID_ENV: &str = "FTUI_XTERM_DIFF_RUN_ID";

#[derive(Clone, Copy, Debug)]
struct SuiteConfig {
    suite: &'static str,
    reference_engine: &'static str,
    comparison_domain: &'static str,
    jsonl_env: &'static str,
    summary_env: &'static str,
    run_id_env: &'static str,
}

impl SuiteConfig {
    const fn vt_support_matrix() -> Self {
        Self {
            suite: "vt_support_matrix",
            reference_engine: "vt-conformance-fixtures",
            comparison_domain: "fixture_expected_grid_cursor_and_attrs",
            jsonl_env: JSONL_PATH_ENV,
            summary_env: SUMMARY_PATH_ENV,
            run_id_env: RUN_ID_ENV,
        }
    }

    const fn xterm_shared_fixture_differential() -> Self {
        Self {
            suite: "xterm_shared_fixture_differential",
            reference_engine: "xterm.js-shared-fixture-baseline",
            comparison_domain: "grid_cursor_attrs_known_mismatch_classification",
            jsonl_env: XTERM_DIFF_JSONL_PATH_ENV,
            summary_env: XTERM_DIFF_SUMMARY_PATH_ENV,
            run_id_env: XTERM_DIFF_RUN_ID_ENV,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct Fixture {
    name: String,
    initial_size: [u16; 2],
    input_bytes_hex: String,
    expected: FixtureExpected,
}

#[derive(Clone, Debug, Deserialize)]
struct FixtureExpected {
    cursor: ExpectedCursor,
    #[serde(default)]
    cells: Vec<ExpectedCell>,
}

#[derive(Clone, Debug, Deserialize)]
struct ExpectedCursor {
    row: u16,
    col: u16,
}

#[derive(Clone, Debug, Deserialize)]
struct ExpectedCell {
    row: u16,
    col: u16,
    #[serde(rename = "char")]
    ch: String,
    #[serde(default)]
    attrs: ExpectedAttrs,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ExpectedAttrs {
    #[serde(default)]
    bold: Option<bool>,
    #[serde(default)]
    dim: Option<bool>,
    #[serde(default)]
    italic: Option<bool>,
    #[serde(default)]
    underline: Option<bool>,
    #[serde(default)]
    blink: Option<bool>,
    #[serde(default)]
    inverse: Option<bool>,
    #[serde(default)]
    strikethrough: Option<bool>,
    #[serde(default)]
    hidden: Option<bool>,
    #[serde(default)]
    overline: Option<bool>,
    #[serde(default)]
    fg_color: Option<ColorExpectation>,
    #[serde(default)]
    bg_color: Option<ColorExpectation>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum ColorExpectation {
    Default(String),
    Named { named: u16 },
    Indexed { indexed: u16 },
    Rgb { rgb: [u8; 3] },
}

#[derive(Clone, Debug, Serialize)]
struct FixtureMismatch {
    field: String,
    row: Option<u16>,
    col: Option<u16>,
    expected: String,
    actual: String,
}

#[derive(Debug, Serialize)]
struct FixtureResult {
    fixture_id: String,
    fixture_path: String,
    correlation_id: String,
    status: String,
    duration_ms: u128,
    mismatch_count: usize,
    mismatches: Vec<FixtureMismatch>,
}

#[derive(Debug, Serialize)]
struct Summary {
    run_id: String,
    suite: String,
    reference_engine: String,
    comparison_domain: String,
    started_at: String,
    finished_at: String,
    total: usize,
    passed: usize,
    known_mismatch: usize,
    failed: usize,
    known_mismatch_catalog_size: usize,
}

#[test]
fn vt_support_matrix_runner_matches_fixtures() {
    let outcome = run_fixture_suite(SuiteConfig::vt_support_matrix());

    assert!(
        outcome.failures.is_empty(),
        "VT support-matrix failures ({}):\n{}",
        outcome.failures.len(),
        outcome.failures.join("\n")
    );
}

#[test]
fn xterm_shared_fixture_differential_matches_reference_fixtures() {
    let outcome = run_fixture_suite(SuiteConfig::xterm_shared_fixture_differential());

    assert!(
        outcome.summary.total >= 300,
        "xterm shared fixture corpus should remain broad, got {} fixtures",
        outcome.summary.total
    );
    assert!(
        outcome.summary.passed > 0,
        "xterm shared fixture differential must evaluate at least one passing fixture"
    );
    assert!(
        outcome.failures.is_empty(),
        "xterm shared fixture differential failures ({}):\n{}",
        outcome.failures.len(),
        outcome.failures.join("\n")
    );
}

#[derive(Debug)]
struct SuiteOutcome {
    summary: Summary,
    failures: Vec<String>,
}

fn run_fixture_suite(config: SuiteConfig) -> SuiteOutcome {
    let started_at = timestamp_utc();
    let run_id = env::var(config.run_id_env).unwrap_or_else(|_| {
        format!(
            "{}-{}",
            config.suite,
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        )
    });
    let fixture_root = fixture_root();
    let known_mismatch_ids = load_known_mismatch_ids(&known_mismatches_path());
    let fixture_paths = collect_fixture_paths(&fixture_root);
    assert!(
        !fixture_paths.is_empty(),
        "no fixtures discovered under {}",
        fixture_root.display()
    );

    let mut jsonl = open_optional_log(config.jsonl_env);
    write_jsonl_event(
        &mut jsonl,
        json!({
            "event": "run_start",
            "ts": timestamp_utc(),
            "run_id": run_id,
            "suite": config.suite,
            "reference_engine": config.reference_engine,
            "comparison_domain": config.comparison_domain,
            "fixture_root": fixture_root.display().to_string(),
            "fixture_count": fixture_paths.len(),
            "known_mismatch_catalog_size": known_mismatch_ids.len(),
        }),
    );

    let mut passed = 0usize;
    let mut known_mismatch = 0usize;
    let mut failed = 0usize;
    let mut failures = Vec::new();

    for fixture_path in fixture_paths {
        let fixture = load_fixture(&fixture_path);
        let result = evaluate_fixture(&fixture, &fixture_path, &known_mismatch_ids, &run_id, None);

        match result.status.as_str() {
            "pass" => passed += 1,
            "known_mismatch" => known_mismatch += 1,
            _ => {
                failed += 1;
                failures.push(result_to_failure_line(&result));
            }
        }

        write_jsonl_event(
            &mut jsonl,
            json!({
                "event": "fixture_result",
                "ts": timestamp_utc(),
                "run_id": run_id,
                "suite": config.suite,
                "reference_engine": config.reference_engine,
                "comparison_domain": config.comparison_domain,
                "correlation_id": result.correlation_id,
                "fixture": result.fixture_id,
                "fixture_path": result.fixture_path,
                "status": result.status,
                "duration_ms": result.duration_ms,
                "mismatch_count": result.mismatch_count,
                "mismatches": result.mismatches,
            }),
        );
    }

    let finished_at = timestamp_utc();
    let summary = Summary {
        run_id: run_id.clone(),
        suite: config.suite.to_string(),
        reference_engine: config.reference_engine.to_string(),
        comparison_domain: config.comparison_domain.to_string(),
        started_at,
        finished_at: finished_at.clone(),
        total: passed + known_mismatch + failed,
        passed,
        known_mismatch,
        failed,
        known_mismatch_catalog_size: known_mismatch_ids.len(),
    };

    write_jsonl_event(
        &mut jsonl,
        json!({
            "event": "run_summary",
            "ts": finished_at,
            "run_id": run_id,
            "suite": config.suite,
            "reference_engine": config.reference_engine,
            "comparison_domain": config.comparison_domain,
            "total": summary.total,
            "passed": summary.passed,
            "known_mismatch": summary.known_mismatch,
            "failed": summary.failed,
            "known_mismatch_catalog_size": summary.known_mismatch_catalog_size,
        }),
    );
    write_optional_summary(config.summary_env, &summary);

    SuiteOutcome { summary, failures }
}

#[test]
fn vt_support_matrix_failure_injection_emits_actionable_diagnostics() {
    let path = fixture_root().join("cursor/cup_basic.json");
    let fixture = load_fixture(&path);
    let result = evaluate_fixture(
        &fixture,
        &path,
        &HashSet::new(),
        "vt-support-matrix-injected",
        Some((0, 1)),
    );

    assert_eq!(result.status, "fail", "injected run should fail");
    let cursor_mismatch = result
        .mismatches
        .iter()
        .find(|m| m.field == "cursor")
        .expect("injected mismatch should include cursor context");
    assert!(
        cursor_mismatch.expected.contains("col="),
        "expected cursor mismatch should include expected col"
    );
    assert!(
        cursor_mismatch.actual.contains("col="),
        "expected cursor mismatch should include actual col"
    );
    assert!(
        result.correlation_id.contains("vt-support-matrix-injected"),
        "correlation id should include run id"
    );
}

fn evaluate_fixture(
    fixture: &Fixture,
    fixture_path: &Path,
    known_mismatch_ids: &HashSet<String>,
    run_id: &str,
    cursor_injection: Option<(i16, i16)>,
) -> FixtureResult {
    let start = Instant::now();
    let mut mismatches = Vec::new();

    let width = fixture.initial_size[0];
    let height = fixture.initial_size[1];
    let mut vt = VirtualTerminal::new(width, height);

    match decode_hex_stream(&fixture.input_bytes_hex) {
        Ok(input) => vt.feed(&input),
        Err(error) => mismatches.push(FixtureMismatch {
            field: "input_bytes_hex".to_string(),
            row: None,
            col: None,
            expected: "valid hex stream".to_string(),
            actual: error,
        }),
    }

    if mismatches.is_empty() {
        let expected_row = adjust_u16(
            fixture.expected.cursor.row,
            cursor_injection.map_or(0, |v| v.0),
        );
        let expected_col = adjust_u16(
            fixture.expected.cursor.col,
            cursor_injection.map_or(0, |v| v.1),
        );
        let (actual_col, actual_row) = vt.cursor();

        if expected_row != actual_row || expected_col != actual_col {
            mismatches.push(FixtureMismatch {
                field: "cursor".to_string(),
                row: Some(expected_row),
                col: Some(expected_col),
                expected: format!("row={expected_row}, col={expected_col}"),
                actual: format!("row={actual_row}, col={actual_col}"),
            });
        }

        for expected_cell in &fixture.expected.cells {
            match vt.cell_at(expected_cell.col, expected_cell.row) {
                None => mismatches.push(FixtureMismatch {
                    field: "cell_presence".to_string(),
                    row: Some(expected_cell.row),
                    col: Some(expected_cell.col),
                    expected: "cell in bounds".to_string(),
                    actual: "cell out of bounds".to_string(),
                }),
                Some(actual_cell) => {
                    compare_cell_char(expected_cell, actual_cell.ch, &mut mismatches);
                    compare_attrs(
                        &expected_cell.attrs,
                        &actual_cell.style,
                        expected_cell.row,
                        expected_cell.col,
                        &mut mismatches,
                    );
                }
            }
        }
    }

    let is_known_mismatch = known_mismatch_ids.contains(&fixture.name);
    let status = if mismatches.is_empty() {
        "pass"
    } else if is_known_mismatch {
        "known_mismatch"
    } else {
        "fail"
    };

    let fixture_path_str = fixture_path.display().to_string();
    let relative_path = fixture_path_str
        .split_once("/tests/fixtures/vt-conformance/")
        .map_or(fixture_path_str.clone(), |(_, rel)| rel.to_string());

    FixtureResult {
        fixture_id: fixture.name.clone(),
        fixture_path: relative_path,
        correlation_id: format!("{run_id}:{}", fixture.name),
        status: status.to_string(),
        duration_ms: start.elapsed().as_millis(),
        mismatch_count: mismatches.len(),
        mismatches,
    }
}

fn compare_cell_char(
    expected_cell: &ExpectedCell,
    actual: char,
    mismatches: &mut Vec<FixtureMismatch>,
) {
    let expected = expected_cell.ch.chars().next().unwrap_or(' ');
    if expected != actual {
        mismatches.push(FixtureMismatch {
            field: "char".to_string(),
            row: Some(expected_cell.row),
            col: Some(expected_cell.col),
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
}

fn compare_attrs(
    expected: &ExpectedAttrs,
    actual: &CellStyle,
    row: u16,
    col: u16,
    mismatches: &mut Vec<FixtureMismatch>,
) {
    compare_bool_attr("bold", expected.bold, actual.bold, row, col, mismatches);
    compare_bool_attr("dim", expected.dim, actual.dim, row, col, mismatches);
    compare_bool_attr(
        "italic",
        expected.italic,
        actual.italic,
        row,
        col,
        mismatches,
    );
    compare_bool_attr(
        "underline",
        expected.underline,
        actual.underline,
        row,
        col,
        mismatches,
    );
    compare_bool_attr("blink", expected.blink, actual.blink, row, col, mismatches);
    compare_bool_attr(
        "inverse",
        expected.inverse,
        actual.reverse,
        row,
        col,
        mismatches,
    );
    compare_bool_attr(
        "strikethrough",
        expected.strikethrough,
        actual.strikethrough,
        row,
        col,
        mismatches,
    );
    compare_bool_attr(
        "hidden",
        expected.hidden,
        actual.hidden,
        row,
        col,
        mismatches,
    );
    compare_bool_attr(
        "overline",
        expected.overline,
        actual.overline,
        row,
        col,
        mismatches,
    );
    compare_color_attr(
        "fg_color",
        expected.fg_color.as_ref(),
        actual.fg,
        row,
        col,
        mismatches,
    );
    compare_color_attr(
        "bg_color",
        expected.bg_color.as_ref(),
        actual.bg,
        row,
        col,
        mismatches,
    );
}

fn compare_bool_attr(
    field: &str,
    expected: Option<bool>,
    actual: bool,
    row: u16,
    col: u16,
    mismatches: &mut Vec<FixtureMismatch>,
) {
    if let Some(expect) = expected
        && expect != actual
    {
        mismatches.push(FixtureMismatch {
            field: field.to_string(),
            row: Some(row),
            col: Some(col),
            expected: expect.to_string(),
            actual: actual.to_string(),
        });
    }
}

fn compare_color_attr(
    field: &str,
    expected: Option<&ColorExpectation>,
    actual: Option<Color>,
    row: u16,
    col: u16,
    mismatches: &mut Vec<FixtureMismatch>,
) {
    let Some(expectation) = expected else {
        return;
    };

    match parse_expected_color(expectation) {
        Ok(expected_color) => {
            if expected_color != actual {
                mismatches.push(FixtureMismatch {
                    field: field.to_string(),
                    row: Some(row),
                    col: Some(col),
                    expected: color_to_string(expected_color),
                    actual: color_to_string(actual),
                });
            }
        }
        Err(error) => mismatches.push(FixtureMismatch {
            field: field.to_string(),
            row: Some(row),
            col: Some(col),
            expected: "valid color expectation".to_string(),
            actual: error,
        }),
    }
}

fn parse_expected_color(expectation: &ColorExpectation) -> Result<Option<Color>, String> {
    match expectation {
        ColorExpectation::Default(value) => {
            if value.eq_ignore_ascii_case("default") {
                Ok(None)
            } else {
                Err(format!("unsupported default token '{value}'"))
            }
        }
        ColorExpectation::Named { named } => named_to_color(*named)
            .ok_or_else(|| format!("unsupported named color index {named}"))
            .map(Some),
        ColorExpectation::Indexed { indexed } => {
            if *indexed > 255 {
                Err(format!("indexed color out of range: {indexed}"))
            } else {
                Ok(Some(color_256(*indexed as u8)))
            }
        }
        ColorExpectation::Rgb { rgb } => Ok(Some(Color::new(rgb[0], rgb[1], rgb[2]))),
    }
}

fn named_to_color(index: u16) -> Option<Color> {
    match index {
        0..=7 => Some(ansi_color(index as u8)),
        8..=15 => Some(ansi_bright_color((index - 8) as u8)),
        _ => None,
    }
}

fn ansi_color(idx: u8) -> Color {
    match idx {
        0 => Color::new(0, 0, 0),
        1 => Color::new(170, 0, 0),
        2 => Color::new(0, 170, 0),
        3 => Color::new(170, 170, 0),
        4 => Color::new(0, 0, 170),
        5 => Color::new(170, 0, 170),
        6 => Color::new(0, 170, 170),
        7 => Color::new(170, 170, 170),
        _ => Color::new(0, 0, 0),
    }
}

fn ansi_bright_color(idx: u8) -> Color {
    match idx {
        0 => Color::new(85, 85, 85),
        1 => Color::new(255, 85, 85),
        2 => Color::new(85, 255, 85),
        3 => Color::new(255, 255, 85),
        4 => Color::new(85, 85, 255),
        5 => Color::new(255, 85, 255),
        6 => Color::new(85, 255, 255),
        7 => Color::new(255, 255, 255),
        _ => Color::new(0, 0, 0),
    }
}

fn color_256(idx: u8) -> Color {
    match idx {
        0..=7 => ansi_color(idx),
        8..=15 => ansi_bright_color(idx - 8),
        16..=231 => {
            let value = idx - 16;
            let blue = value % 6;
            let green = (value / 6) % 6;
            let red = value / 36;
            let to_rgb = |component: u8| {
                if component == 0 {
                    0
                } else {
                    55 + component * 40
                }
            };
            Color::new(to_rgb(red), to_rgb(green), to_rgb(blue))
        }
        232..=255 => {
            let value = 8 + (idx - 232) * 10;
            Color::new(value, value, value)
        }
    }
}

fn decode_hex_stream(hex: &str) -> Result<Vec<u8>, String> {
    let compact: Vec<u8> = hex
        .as_bytes()
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();

    if !compact.len().is_multiple_of(2) {
        return Err(format!("odd hex length: {}", compact.len()));
    }

    let mut out = Vec::with_capacity(compact.len() / 2);
    for pair in compact.as_chunks::<2>().0 {
        let high = nibble(pair[0]).ok_or_else(|| format!("invalid hex byte: {}", pair[0]))?;
        let low = nibble(pair[1]).ok_or_else(|| format!("invalid hex byte: {}", pair[1]))?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(10 + (byte - b'a')),
        b'A'..=b'F' => Some(10 + (byte - b'A')),
        _ => None,
    }
}

fn color_to_string(color: Option<Color>) -> String {
    match color {
        Some(value) => format!("rgb({}, {}, {})", value.r, value.g, value.b),
        None => "default".to_string(),
    }
}

fn result_to_failure_line(result: &FixtureResult) -> String {
    let detail = result
        .mismatches
        .first()
        .map(|mismatch| {
            format!(
                "{} at row={:?}, col={:?}: expected={}, actual={}",
                mismatch.field, mismatch.row, mismatch.col, mismatch.expected, mismatch.actual
            )
        })
        .unwrap_or_else(|| "unknown mismatch".to_string());

    format!("{} [{}] {}", result.fixture_id, result.fixture_path, detail)
}

fn collect_fixture_paths(root: &Path) -> Vec<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|error| {
                    panic!("failed to read dir entry in {}: {error}", dir.display())
                })
                .path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "json") {
                files.push(path);
            }
        }
    }

    files.sort();
    files
}

fn load_fixture(path: &Path) -> Fixture {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_slice::<Fixture>(&bytes)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

fn load_known_mismatch_ids(path: &Path) -> HashSet<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    contents
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            trimmed.split('|').next().map(str::to_string)
        })
        .collect()
}

fn write_optional_summary(var_name: &str, summary: &Summary) {
    let Some(path) = env::var_os(var_name) else {
        return;
    };
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| {
            panic!(
                "failed to create summary directory {}: {error}",
                parent.display()
            )
        });
    }
    let file = File::create(&path)
        .unwrap_or_else(|error| panic!("failed to create summary {}: {error}", path.display()));
    serde_json::to_writer_pretty(file, summary)
        .unwrap_or_else(|error| panic!("failed to write summary {}: {error}", path.display()));
}

fn open_optional_log(var_name: &str) -> Option<File> {
    let path = env::var_os(var_name)?;
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| {
            panic!("failed to create log dir {}: {error}", parent.display())
        });
    }
    Some(
        File::create(&path)
            .unwrap_or_else(|error| panic!("failed to create log {}: {error}", path.display())),
    )
}

fn write_jsonl_event(file: &mut Option<File>, event: serde_json::Value) {
    let Some(writer) = file else {
        return;
    };
    serde_json::to_writer(&mut *writer, &event).expect("failed to serialize JSONL event");
    writer.write_all(b"\n").expect("failed to append newline");
    writer.flush().expect("failed to flush JSONL event");
}

fn timestamp_utc() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("Rfc3339 formatting should succeed")
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_ROOT_REL)
}

fn known_mismatches_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(KNOWN_MISMATCHES_REL)
}

fn adjust_u16(value: u16, delta: i16) -> u16 {
    if delta >= 0 {
        value.saturating_add(delta as u16)
    } else {
        value.saturating_sub(delta.unsigned_abs())
    }
}

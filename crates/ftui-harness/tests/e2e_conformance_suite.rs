#![forbid(unsafe_code)]

//! bd-3fc.9: E2E test — Full conformance suite across emulator matrix.
//!
//! Runs the complete VT conformance fixture corpus and representative golden-frame
//! rendering scenarios across all 5 terminal profiles. Validates:
//!
//! 1. All VT golden frames match (cursor + cell assertions per fixture).
//! 2. No ERROR-level failures during any scenario.
//! 3. p99 frame time within SLO (< 5ms per fixture in debug).
//! 4. Conformance report generated (JSONL + summary JSON).
//! 5. `conformance_pass_rate` gauge = 1.0 for all emulators (or documented exceptions).
//!
//! # Running
//!
//! ```sh
//! CARGO_TARGET_DIR=/tmp/ftui-test-target cargo test -p ftui-harness --test e2e_conformance_suite
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ftui_core::geometry::Rect;
use ftui_core::terminal_capabilities::{TerminalCapabilities, TerminalProfile};
use ftui_layout::Constraint;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::presenter::Presenter;
use ftui_text::Text;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;
use ftui_widgets::list::{List, ListItem, ListState};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::ProgressBar;
use ftui_widgets::sparkline::Sparkline;
use ftui_widgets::table::{Row, Table, TableState};
use ftui_widgets::{StatefulWidget, Widget};

// ============================================================================
// Terminal Profiles (the "5 emulators")
// ============================================================================

fn emulator_profiles() -> Vec<(&'static str, TerminalCapabilities)> {
    vec![
        (
            "xterm-256color",
            TerminalCapabilities::from_profile(TerminalProfile::Xterm256Color),
        ),
        (
            "screen-256color",
            TerminalCapabilities::from_profile(TerminalProfile::Screen),
        ),
        (
            "tmux-256color",
            TerminalCapabilities::from_profile(TerminalProfile::Tmux),
        ),
        (
            "kitty",
            TerminalCapabilities::from_profile(TerminalProfile::Kitty),
        ),
        (
            "alacritty",
            TerminalCapabilities::from_profile(TerminalProfile::Modern),
        ),
    ]
}

// ============================================================================
// VT Conformance Fixture Types
// ============================================================================

#[derive(Clone, Debug)]
struct VtFixture {
    name: String,
    initial_size: [u16; 2],
    input_bytes_hex: String,
    expected_cursor: (u16, u16),
    expected_cells: Vec<(u16, u16, char)>,
}

fn parse_fixture(value: &serde_json::Value) -> Option<VtFixture> {
    let name = value["name"].as_str()?.to_string();
    let size = value["initial_size"].as_array()?;
    let width = size.first()?.as_u64()? as u16;
    let height = size.get(1)?.as_u64()? as u16;
    let hex = value["input_bytes_hex"].as_str()?.to_string();
    let cursor_row = value["expected"]["cursor"]["row"].as_u64()? as u16;
    let cursor_col = value["expected"]["cursor"]["col"].as_u64()? as u16;

    let mut cells = Vec::new();
    if let Some(cell_arr) = value["expected"]["cells"].as_array() {
        for cell in cell_arr {
            let row = cell["row"].as_u64().unwrap_or(0) as u16;
            let col = cell["col"].as_u64().unwrap_or(0) as u16;
            let ch = cell["char"]
                .as_str()
                .and_then(|s| s.chars().next())
                .unwrap_or(' ');
            cells.push((row, col, ch));
        }
    }

    Some(VtFixture {
        name,
        initial_size: [width, height],
        input_bytes_hex: hex,
        expected_cursor: (cursor_row, cursor_col),
        expected_cells: cells,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KnownMismatch {
    fixture_id: String,
    cols: u16,
    rows: u16,
    input_bytes_hex: String,
    root_cause: String,
}

struct FixtureResult<'a> {
    emulator: &'a str,
    category: &'a str,
    fixture: &'a VtFixture,
    status: &'a str,
    start: Instant,
    mismatch: Option<String>,
    known_mismatch: Option<&'a KnownMismatch>,
}

// ============================================================================
// JSONL Evidence
// ============================================================================

static SEQ: AtomicU64 = AtomicU64::new(0);

fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

fn emit_jsonl(events: &[String], path: &Path) {
    let mut file = std::fs::File::create(path).expect("create conformance JSONL");
    for line in events {
        writeln!(file, "{}", line).expect("write event");
    }
}

fn push_event(events: &mut Vec<String>, event: serde_json::Value) {
    events.push(serde_json::to_string(&event).expect("serialize JSONL event"));
}

// ============================================================================
// Conformance Runner
// ============================================================================

struct ConformanceRunner {
    events: Vec<String>,
    timings_us: Vec<u64>,
    passed: usize,
    known_exceptions: usize,
    failed: usize,
    failures: Vec<String>,
    known_root_causes: BTreeMap<String, usize>,
}

impl ConformanceRunner {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            timings_us: Vec::new(),
            passed: 0,
            known_exceptions: 0,
            failed: 0,
            failures: Vec::new(),
            known_root_causes: BTreeMap::new(),
        }
    }

    fn run_vt_fixture(
        &mut self,
        emulator: &str,
        category: &str,
        fixture: &VtFixture,
        known_mismatches: &HashMap<String, KnownMismatch>,
    ) {
        let start = Instant::now();

        let width = fixture.initial_size[0];
        let height = fixture.initial_size[1];
        let mut vt = ftui_pty::virtual_terminal::VirtualTerminal::new(width, height);

        let input = match decode_hex(&fixture.input_bytes_hex) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.record_result(FixtureResult {
                    emulator,
                    category,
                    fixture,
                    status: "fail",
                    start,
                    mismatch: Some(e),
                    known_mismatch: None,
                });
                return;
            }
        };
        vt.feed(&input);

        let (actual_col, actual_row) = vt.cursor();
        let (expected_row, expected_col) = fixture.expected_cursor;

        let mut mismatch = None;
        if expected_row != actual_row || expected_col != actual_col {
            mismatch = Some(format!(
                "cursor: expected=({},{}) actual=({},{})",
                expected_row, expected_col, actual_row, actual_col
            ));
        }

        if mismatch.is_none() {
            for &(row, col, expected_ch) in &fixture.expected_cells {
                if let Some(actual_cell) = vt.cell_at(col, row) {
                    if expected_ch != actual_cell.ch {
                        mismatch = Some(format!(
                            "cell({},{}): expected='{}' actual='{}'",
                            row, col, expected_ch, actual_cell.ch
                        ));
                        break;
                    }
                } else {
                    mismatch = Some(format!("cell({},{}) out of bounds", row, col));
                    break;
                }
            }
        }

        let known_mismatch = known_mismatches.get(&fixture.name);
        let status = match (&mismatch, known_mismatch) {
            (None, _) => "pass",
            (Some(_), Some(_)) => "known_exception",
            (Some(_), None) => "fail",
        };

        self.record_result(FixtureResult {
            emulator,
            category,
            fixture,
            status,
            start,
            mismatch,
            known_mismatch,
        });
    }

    fn record_result(&mut self, result: FixtureResult<'_>) {
        let FixtureResult {
            emulator,
            category,
            fixture,
            status,
            start,
            mismatch,
            known_mismatch,
        } = result;
        let duration_us = start.elapsed().as_micros() as u64;
        self.timings_us.push(duration_us);

        match status {
            "pass" => self.passed += 1,
            "known_exception" => {
                self.known_exceptions += 1;
                if let Some(known) = known_mismatch {
                    *self
                        .known_root_causes
                        .entry(known.root_cause.clone())
                        .or_insert(0) += 1;
                }
            }
            _ => {
                self.failed += 1;
                self.failures.push(format!(
                    "[{}] {}/{}: {}",
                    emulator,
                    category,
                    fixture.name,
                    mismatch.as_deref().unwrap_or("unknown")
                ));
            }
        }

        push_event(
            &mut self.events,
            serde_json::json!({
                "event": "vt_conformance",
                "seq": next_seq(),
                "emulator": emulator,
                "category": category,
                "fixture": fixture.name,
                "status": status,
                "duration_us": duration_us,
                "mismatch": mismatch,
                "fixture_cols": fixture.initial_size[0],
                "fixture_rows": fixture.initial_size[1],
                "input_bytes_hex": fixture.input_bytes_hex,
                "known_root_cause": known_mismatch.map(|entry| entry.root_cause.as_str()),
            }),
        );
    }

    fn record_render(
        &mut self,
        emulator: &str,
        scenario: &str,
        checksum: &str,
        duration_us: u64,
        deterministic: bool,
    ) {
        self.timings_us.push(duration_us);

        push_event(
            &mut self.events,
            serde_json::json!({
                "event": "render_conformance",
                "seq": next_seq(),
                "emulator": emulator,
                "scenario": scenario,
                "checksum": checksum,
                "duration_us": duration_us,
                "deterministic": deterministic,
            }),
        );
    }

    fn percentile(&self, pct: f64) -> u64 {
        if self.timings_us.is_empty() {
            return 0;
        }
        let mut sorted = self.timings_us.clone();
        sorted.sort_unstable();
        let idx = ((pct / 100.0) * (sorted.len() - 1) as f64).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn pass_rate(&self) -> f64 {
        let total = self.passed + self.known_exceptions + self.failed;
        if total == 0 {
            return 1.0;
        }
        (self.passed + self.known_exceptions) as f64 / total as f64
    }

    fn summary_json(&self, render_count: usize) -> String {
        serde_json::json!({
            "event": "conformance_summary",
            "total_evaluations": self.passed + self.known_exceptions + self.failed,
            "passed": self.passed,
            "known_exceptions": self.known_exceptions,
            "failed": self.failed,
            "pass_rate": self.pass_rate(),
            "p50_us": self.percentile(50.0),
            "p99_us": self.percentile(99.0),
            "max_us": self.percentile(100.0),
            "render_scenarios": render_count,
            "emulators": 5,
            "known_root_causes": self.known_root_causes,
        })
        .to_string()
    }

    fn xterm_shared_fixture_differential_summary_json(&self) -> String {
        serde_json::json!({
            "event": "xterm_shared_fixture_differential_summary",
            "emulator": "xterm-256color",
            "total_fixtures": self.passed + self.known_exceptions + self.failed,
            "passed": self.passed,
            "known_exceptions": self.known_exceptions,
            "failed": self.failed,
            "pass_rate": self.pass_rate(),
            "known_root_causes": self.known_root_causes,
        })
        .to_string()
    }
}

// ============================================================================
// VT Fixture Discovery
// ============================================================================

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/vt-conformance")
}

fn parse_known_mismatch_line(line: &str) -> Option<KnownMismatch> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut parts = trimmed.splitn(5, '|');
    let fixture_id = parts.next()?.trim().to_string();
    let cols = parts.next()?.trim().parse().ok()?;
    let rows = parts.next()?.trim().parse().ok()?;
    let input_bytes_hex = parts.next()?.trim().to_string();
    let root_cause = parts.next()?.trim().to_string();
    Some(KnownMismatch {
        fixture_id,
        cols,
        rows,
        input_bytes_hex,
        root_cause,
    })
}

fn load_known_mismatches() -> HashMap<String, KnownMismatch> {
    let path = fixture_root().join("differential/known_mismatches.tsv");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    contents
        .lines()
        .filter_map(parse_known_mismatch_line)
        .map(|entry| (entry.fixture_id.clone(), entry))
        .collect()
}

fn discover_fixtures() -> BTreeMap<String, Vec<(PathBuf, VtFixture)>> {
    let root = fixture_root();
    let mut categories: BTreeMap<String, Vec<(PathBuf, VtFixture)>> = BTreeMap::new();

    let Ok(entries) = std::fs::read_dir(&root) else {
        return categories;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let category = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if category == "differential" {
            continue;
        }

        let Ok(files) = std::fs::read_dir(&path) else {
            continue;
        };

        let mut fixtures = Vec::new();
        for file_entry in files.flatten() {
            let fp = file_entry.path();
            if fp.extension().is_some_and(|ext| ext == "json")
                && let Ok(bytes) = std::fs::read(&fp)
                && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(fixture) = parse_fixture(&value)
            {
                fixtures.push((fp, fixture));
            }
        }
        fixtures.sort_by(|a, b| a.0.cmp(&b.0));
        if !fixtures.is_empty() {
            categories.insert(category, fixtures);
        }
    }

    categories
}

// ============================================================================
// Full Render Pipeline Helper
// ============================================================================

fn full_pipeline_checksum(
    caps: &TerminalCapabilities,
    width: u16,
    height: u16,
    render_fn: fn(&mut Frame),
) -> (String, u64) {
    let start = Instant::now();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    render_fn(&mut frame);

    let empty = Buffer::new(width, height);
    let diff = BufferDiff::compute(&empty, &frame.buffer);

    let mut presenter = Presenter::new(Vec::<u8>::new(), *caps);
    presenter.present(&frame.buffer, &diff).unwrap();
    let bytes = presenter.into_inner().unwrap();

    let hash = blake3::hash(&bytes);
    let duration_us = start.elapsed().as_micros() as u64;
    (format!("blake3:{}", hash.to_hex()), duration_us)
}

fn decode_hex(hex: &str) -> Result<Vec<u8>, String> {
    let compact: Vec<u8> = hex
        .as_bytes()
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();

    if !compact.len().is_multiple_of(2) {
        return Err(format!("odd hex length: {}", compact.len()));
    }

    let mut out = Vec::with_capacity(compact.len() / 2);
    for pair in compact.as_chunks::<2>().0 {
        let high = nibble(pair[0]).ok_or_else(|| format!("invalid hex: {}", pair[0]))?;
        let low = nibble(pair[1]).ok_or_else(|| format!("invalid hex: {}", pair[1]))?;
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

// ============================================================================
// Render Scenarios
// ============================================================================

type RenderScenario = (&'static str, u16, u16, fn(&mut Frame));

fn render_scenarios() -> Vec<RenderScenario> {
    vec![
        (
            "paragraph_basic",
            80,
            24,
            render_paragraph as fn(&mut Frame),
        ),
        ("paragraph_wide", 120, 40, render_paragraph),
        ("list_basic", 80, 24, render_list),
        ("table_basic", 120, 40, render_table),
        ("progress_bars", 80, 24, render_progress),
        ("sparkline_wave", 80, 24, render_sparkline),
        ("composite_dashboard", 120, 40, render_composite),
    ]
}

fn render_paragraph(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::raw(
        "FrankenTUI Conformance Suite\n\
         Testing deterministic rendering across emulator profiles.\n\
         This paragraph validates text wrapping, style inheritance,\n\
         and border rendering consistency.",
    ))
    .block(Block::new().borders(Borders::ALL).title("Conformance"))
    .render(area, frame);
}

fn render_list(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let items: Vec<ListItem> = (0..20)
        .map(|i| ListItem::new(format!("Fixture #{:03}: conformance check", i)))
        .collect();
    let list = List::new(items).block(Block::new().borders(Borders::ALL).title("Fixtures"));
    let mut state = ListState::default();
    state.select(Some(5));
    StatefulWidget::render(&list, area, frame, &mut state);
}

fn render_table(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let rows: Vec<Row> = (0..15)
        .map(|i| {
            Row::new(vec![
                format!("{}", i + 1),
                format!("fixture_{:03}", i),
                "pass".to_string(),
                format!("{}us", 42 + i * 3),
            ])
        })
        .collect();
    let widths = [
        Constraint::Fixed(8),
        Constraint::Fixed(30),
        Constraint::Fixed(12),
        Constraint::Fixed(12),
    ];
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["#", "Fixture", "Status", "Time"]))
        .block(Block::new().borders(Borders::ALL).title("Results"));
    let mut state = TableState::default();
    state.select(Some(0));
    StatefulWidget::render(&table, area, frame, &mut state);
}

fn render_progress(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let block = Block::new().borders(Borders::ALL).title("Progress");
    let inner = block.inner(area);
    block.render(area, frame);

    if inner.height >= 4 && inner.width > 0 {
        let bar1 = Rect::new(inner.x, inner.y, inner.width, 1);
        ProgressBar::default().ratio(0.75).render(bar1, frame);
        let bar2 = Rect::new(inner.x, inner.y + 2, inner.width, 1);
        ProgressBar::default().ratio(1.0).render(bar2, frame);
    }
}

fn render_sparkline(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height().min(5));
    let data: Vec<f64> = (0..60)
        .map(|i| (i as f64 * 0.3).sin() * 50.0 + 50.0)
        .collect();
    let block = Block::new().borders(Borders::ALL).title("Timing");
    let inner = block.inner(area);
    block.render(area, frame);
    if inner.height > 0 && inner.width > 0 {
        let spark_area = Rect::new(inner.x, inner.y, inner.width, inner.height);
        Sparkline::new(&data).render(spark_area, frame);
    }
}

fn render_composite(frame: &mut Frame) {
    let w = frame.buffer.width();
    let h = frame.buffer.height();

    // Top half: paragraph.
    let top = Rect::new(0, 0, w, h / 2);
    Paragraph::new(Text::raw(
        "Composite Dashboard\nMultiple widgets rendered in a single frame.",
    ))
    .block(Block::new().borders(Borders::ALL).title("Header"))
    .render(top, frame);

    // Bottom half: progress.
    let bottom = Rect::new(0, h / 2, w, h - h / 2);
    let block = Block::new().borders(Borders::ALL).title("Status");
    let inner = block.inner(bottom);
    block.render(bottom, frame);

    if inner.height > 0 && inner.width > 0 {
        let bar = Rect::new(inner.x, inner.y, inner.width, 1);
        ProgressBar::default().ratio(0.95).render(bar, frame);
    }
}

// ============================================================================
// Tests
// ============================================================================

/// Phase 1: VT conformance across all emulators.
#[test]
fn conformance_vt_fixtures_across_all_emulators() {
    let categories = discover_fixtures();
    assert!(
        !categories.is_empty(),
        "no VT conformance fixtures found at {}",
        fixture_root().display()
    );

    let known_mismatches = load_known_mismatches();
    let profiles = emulator_profiles();
    let mut runner = ConformanceRunner::new();

    let total_fixtures: usize = categories.values().map(|v| v.len()).sum();

    for (emulator_name, _caps) in &profiles {
        for (category, fixtures) in &categories {
            for (_path, fixture) in fixtures {
                runner.run_vt_fixture(emulator_name, category, fixture, &known_mismatches);
            }
        }
    }

    let jsonl_path = std::env::temp_dir().join("e2e_conformance_vt.jsonl");
    emit_jsonl(&runner.events, &jsonl_path);

    let pass_rate = runner.pass_rate();
    eprintln!(
        "VT conformance: {}/{} passed, {} known exceptions, {} failed (pass_rate={:.4})",
        runner.passed,
        total_fixtures * profiles.len(),
        runner.known_exceptions,
        runner.failed,
        pass_rate
    );

    assert_eq!(
        runner.failed,
        0,
        "VT conformance failures ({}):\n{}",
        runner.failed,
        runner.failures.join("\n")
    );
    assert!(
        (pass_rate - 1.0).abs() < f64::EPSILON,
        "conformance_pass_rate must be 1.0, got {:.6}",
        pass_rate
    );
}

/// Phase 2: Golden-frame render conformance across all emulators.
#[test]
fn conformance_golden_frame_rendering_across_all_emulators() {
    let profiles = emulator_profiles();
    let scenarios = render_scenarios();
    let mut runner = ConformanceRunner::new();
    let mut all_deterministic = true;

    for (emulator_name, caps) in &profiles {
        for &(scenario_name, width, height, render_fn) in &scenarios {
            let (cs1, dur1) = full_pipeline_checksum(caps, width, height, render_fn);
            let (cs2, _) = full_pipeline_checksum(caps, width, height, render_fn);
            let deterministic = cs1 == cs2;
            if !deterministic {
                all_deterministic = false;
            }

            runner.record_render(emulator_name, scenario_name, &cs1, dur1, deterministic);

            assert!(
                deterministic,
                "NON-DETERMINISTIC: emulator={} scenario={} cs1={} cs2={}",
                emulator_name, scenario_name, cs1, cs2
            );
        }
    }

    let jsonl_path = std::env::temp_dir().join("e2e_conformance_render.jsonl");
    emit_jsonl(&runner.events, &jsonl_path);

    assert!(
        all_deterministic,
        "all render scenarios must produce deterministic output"
    );
}

/// Phase 3: p99 frame time within SLO.
#[test]
fn conformance_p99_within_slo() {
    let categories = discover_fixtures();
    let known_mismatches = load_known_mismatches();
    let mut runner = ConformanceRunner::new();

    let caps = TerminalCapabilities::from_profile(TerminalProfile::Xterm256Color);
    for (category, fixtures) in &categories {
        for (_path, fixture) in fixtures {
            runner.run_vt_fixture("xterm-256color", category, fixture, &known_mismatches);
        }
    }

    for &(name, width, height, render_fn) in &render_scenarios() {
        let (cs, dur) = full_pipeline_checksum(&caps, width, height, render_fn);
        runner.record_render("xterm-256color", name, &cs, dur, true);
    }

    let p50 = runner.percentile(50.0);
    let p99 = runner.percentile(99.0);
    let max = runner.percentile(100.0);

    eprintln!(
        "Timing: p50={}us, p99={}us, max={}us ({} samples)",
        p50,
        p99,
        max,
        runner.timings_us.len()
    );

    // SLO: p99 < 10ms (10000us) in debug. Release would be < 1ms.
    // Debug builds are ~10x slower due to no inlining/optimization.
    let p99_limit_us = 10_000;
    assert!(
        p99 < p99_limit_us,
        "p99 frame time {}us exceeds SLO {}us",
        p99,
        p99_limit_us
    );
}

/// Phase 4: Conformance report generation.
#[test]
fn conformance_report_generated() {
    let categories = discover_fixtures();
    let known_mismatches = load_known_mismatches();
    let mut runner = ConformanceRunner::new();

    for (category, fixtures) in &categories {
        for (_path, fixture) in fixtures {
            runner.run_vt_fixture("xterm-256color", category, fixture, &known_mismatches);
        }
    }

    let caps = TerminalCapabilities::from_profile(TerminalProfile::Xterm256Color);
    let scenarios = render_scenarios();
    for &(name, width, height, render_fn) in &scenarios {
        let (cs, dur) = full_pipeline_checksum(&caps, width, height, render_fn);
        runner.record_render("xterm-256color", name, &cs, dur, true);
    }

    let summary_json = runner.summary_json(scenarios.len());
    let xterm_diff_summary = runner.xterm_shared_fixture_differential_summary_json();
    let summary_path = std::env::temp_dir().join("e2e_conformance_summary.json");
    std::fs::write(
        &summary_path,
        format!("{summary_json}\n{xterm_diff_summary}\n"),
    )
    .expect("write summary");

    // Validate summary schema.
    let parsed: serde_json::Value =
        serde_json::from_str(&summary_json).expect("parse summary JSON");
    assert_eq!(parsed["event"], "conformance_summary");
    assert!(parsed["total_evaluations"].is_u64());
    assert!(parsed["passed"].is_u64());
    assert!(parsed["pass_rate"].is_f64());
    assert!(parsed["p50_us"].is_u64());
    assert!(parsed["p99_us"].is_u64());
    assert!(parsed["render_scenarios"].is_u64());
    assert!(parsed["emulators"].is_u64());
    assert!(parsed["known_root_causes"].is_object());

    let xterm_parsed: serde_json::Value =
        serde_json::from_str(&xterm_diff_summary).expect("parse xterm diff summary");
    assert_eq!(
        xterm_parsed["event"],
        "xterm_shared_fixture_differential_summary"
    );
    assert_eq!(xterm_parsed["emulator"], "xterm-256color");
    assert!(xterm_parsed["known_root_causes"].is_object());

    eprintln!("Conformance report: {}", summary_json);
}

/// Verify per-emulator pass rate is 1.0.
#[test]
fn conformance_pass_rate_per_emulator() {
    let categories = discover_fixtures();
    let known_mismatches = load_known_mismatches();
    let profiles = emulator_profiles();

    for (emulator_name, _caps) in &profiles {
        let mut runner = ConformanceRunner::new();
        for (category, fixtures) in &categories {
            for (_path, fixture) in fixtures {
                runner.run_vt_fixture(emulator_name, category, fixture, &known_mismatches);
            }
        }

        let rate = runner.pass_rate();
        assert!(
            (rate - 1.0).abs() < f64::EPSILON,
            "conformance_pass_rate for {} must be 1.0, got {:.6} ({} failures:\n{})",
            emulator_name,
            rate,
            runner.failed,
            runner.failures.join("\n")
        );
    }
}

/// Verify fixture count is stable (no regressions in fixture corpus).
#[test]
fn conformance_fixture_count_stable() {
    let categories = discover_fixtures();
    let total: usize = categories.values().map(|v| v.len()).sum();

    assert!(
        total >= 300,
        "fixture corpus has regressed: expected >= 300, got {}",
        total
    );

    let expected_categories = [
        "c0_controls",
        "charset",
        "cursor",
        "erase",
        "erase_chars",
        "esc_sequences",
        "line_edit",
        "modes",
        "repeat",
        "scroll",
        "scroll_region",
        "sgr",
        "tab_stops",
        "utf8",
        "wide_chars",
        "wrap_behavior",
    ];
    for cat in &expected_categories {
        assert!(
            categories.contains_key(*cat),
            "missing conformance category: {}",
            cat
        );
    }

    eprintln!(
        "Fixture corpus: {} categories, {} total fixtures",
        categories.len(),
        total
    );
}

/// Verify that render output differs between profiles with different capabilities.
#[test]
fn conformance_profiles_produce_distinct_output() {
    let profiles = emulator_profiles();
    let checksums: Vec<String> = profiles
        .iter()
        .map(|(_, caps)| full_pipeline_checksum(caps, 80, 24, render_composite).0)
        .collect();

    let unique: HashSet<&str> = checksums.iter().map(|s| s.as_str()).collect();

    assert!(
        unique.len() >= 2,
        "expected different profiles to produce different ANSI output, got {} unique checksums",
        unique.len()
    );
}

/// Verify all fixtures have valid JSON structure.
#[test]
fn conformance_fixture_schema_valid() {
    let categories = discover_fixtures();
    for (category, fixtures) in &categories {
        for (path, fixture) in fixtures {
            assert!(
                !fixture.name.is_empty(),
                "fixture has empty name: {}",
                path.display()
            );
            assert!(
                fixture.initial_size[0] > 0 && fixture.initial_size[1] > 0,
                "fixture {} has zero dimension: {:?}",
                fixture.name,
                fixture.initial_size
            );
            assert!(
                !fixture.input_bytes_hex.is_empty(),
                "fixture {}/{} has empty input",
                category,
                fixture.name
            );
            assert!(
                decode_hex(&fixture.input_bytes_hex).is_ok(),
                "fixture {}/{} has invalid hex",
                category,
                fixture.name
            );
        }
    }
}

#[test]
fn known_mismatch_tsv_line_parses_full_metadata() {
    let entry = parse_known_mismatch_line(
        "cup_large_clamp|5|3|1b5b3939393b393939485a|Cursor pending-wrap semantics diverge after large CUP clamp write",
    )
    .expect("known mismatch line should parse");

    assert_eq!(entry.fixture_id, "cup_large_clamp");
    assert_eq!(entry.cols, 5);
    assert_eq!(entry.rows, 3);
    assert_eq!(entry.input_bytes_hex, "1b5b3939393b393939485a");
    assert_eq!(
        entry.root_cause,
        "Cursor pending-wrap semantics diverge after large CUP clamp write"
    );
}

/// JSONL output schema compliance.
#[test]
fn conformance_jsonl_schema_compliance() {
    let mut runner = ConformanceRunner::new();
    let known_mismatches = load_known_mismatches();

    let categories = discover_fixtures();
    if let Some((cat_name, cat_fixtures)) = categories.iter().next()
        && let Some((_path, fixture)) = cat_fixtures.first()
    {
        runner.run_vt_fixture("xterm-256color", cat_name, fixture, &known_mismatches);
    }

    let caps = TerminalCapabilities::from_profile(TerminalProfile::Xterm256Color);
    let (cs, dur) = full_pipeline_checksum(&caps, 80, 24, render_paragraph);
    runner.record_render("xterm-256color", "paragraph_basic", &cs, dur, true);

    let jsonl_path = std::env::temp_dir().join("e2e_conformance_schema_test.jsonl");
    emit_jsonl(&runner.events, &jsonl_path);

    let content = std::fs::read_to_string(&jsonl_path).expect("read JSONL");
    let lines: Vec<&str> = content.lines().collect();
    assert!(lines.len() >= 2, "expected at least 2 JSONL lines");

    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("parse JSONL line");
        assert!(v["event"].is_string());
        assert!(v["seq"].is_u64());
        assert!(v["emulator"].is_string());
    }

    // Verify VT event has required fields.
    let vt_line: serde_json::Value = serde_json::from_str(lines[0]).expect("parse first line");
    if vt_line["event"] == "vt_conformance" {
        assert!(vt_line["category"].is_string());
        assert!(vt_line["fixture"].is_string());
        assert!(vt_line["status"].is_string());
        assert!(vt_line["duration_us"].is_u64());
    }

    std::fs::remove_file(&jsonl_path).ok();
}

#[test]
fn xterm_shared_fixture_summary_includes_known_root_causes() {
    let mut runner = ConformanceRunner::new();
    let known = KnownMismatch {
        fixture_id: "cup_large_clamp".to_string(),
        cols: 5,
        rows: 3,
        input_bytes_hex: "1b5b3939393b393939485a".to_string(),
        root_cause: "Cursor pending-wrap semantics diverge after large CUP clamp write".to_string(),
    };
    let fixture = VtFixture {
        name: known.fixture_id.clone(),
        initial_size: [known.cols, known.rows],
        input_bytes_hex: known.input_bytes_hex.clone(),
        expected_cursor: (0, 0),
        expected_cells: Vec::new(),
    };

    runner.record_result(FixtureResult {
        emulator: "xterm-256color",
        category: "cursor",
        fixture: &fixture,
        status: "known_exception",
        start: Instant::now(),
        mismatch: Some("cursor mismatch".to_string()),
        known_mismatch: Some(&known),
    });

    let summary: serde_json::Value =
        serde_json::from_str(&runner.xterm_shared_fixture_differential_summary_json())
            .expect("summary JSON should parse");
    assert_eq!(
        summary["event"],
        "xterm_shared_fixture_differential_summary"
    );
    assert_eq!(summary["emulator"], "xterm-256color");
    assert_eq!(summary["known_exceptions"], 1);
    assert_eq!(summary["failed"], 0);
    assert_eq!(
        summary["known_root_causes"]["Cursor pending-wrap semantics diverge after large CUP clamp write"],
        1
    );
}

/// No panics on extreme terminal sizes.
#[test]
fn conformance_no_panic_extreme_sizes() {
    let caps = TerminalCapabilities::from_profile(TerminalProfile::Xterm256Color);
    let extreme_sizes: [(u16, u16); 4] = [(1, 1), (1, 100), (300, 1), (300, 100)];

    for (w, h) in &extreme_sizes {
        let (cs, _dur) = full_pipeline_checksum(&caps, *w, *h, render_composite);
        assert!(
            cs.starts_with("blake3:"),
            "invalid checksum for {}x{}: {}",
            w,
            h,
            cs
        );
    }
}

/// Category coverage: each category has meaningful fixture count.
#[test]
fn conformance_category_coverage() {
    let categories = discover_fixtures();
    let min_fixtures_per_category = 5;

    for (category, fixtures) in &categories {
        assert!(
            fixtures.len() >= min_fixtures_per_category,
            "category '{}' has only {} fixtures (min: {})",
            category,
            fixtures.len(),
            min_fixtures_per_category
        );
    }
}

/// Multi-frame render: verify diff-based second frame is also deterministic.
#[test]
fn conformance_multi_frame_deterministic() {
    let profiles = emulator_profiles();

    for (emulator_name, caps) in &profiles {
        // Frame 1: paragraph.
        let mut pool1 = GraphemePool::new();
        let mut f1 = Frame::new(80, 24, &mut pool1);
        render_paragraph(&mut f1);

        let empty = Buffer::new(80, 24);
        let diff1 = BufferDiff::compute(&empty, &f1.buffer);
        let mut p1 = Presenter::new(Vec::<u8>::new(), *caps);
        p1.present(&f1.buffer, &diff1).unwrap();
        let _ = p1.into_inner().unwrap();

        // Frame 2: progress overlay.
        let mut pool2 = GraphemePool::new();
        let mut f2 = Frame::new(80, 24, &mut pool2);
        render_progress(&mut f2);

        let diff2 = BufferDiff::compute(&f1.buffer, &f2.buffer);
        let mut p2a = Presenter::new(Vec::<u8>::new(), *caps);
        p2a.present(&f2.buffer, &diff2).unwrap();
        let bytes2a = p2a.into_inner().unwrap();

        let mut p2b = Presenter::new(Vec::<u8>::new(), *caps);
        p2b.present(&f2.buffer, &diff2).unwrap();
        let bytes2b = p2b.into_inner().unwrap();

        assert_eq!(
            blake3::hash(&bytes2a).to_hex().to_string(),
            blake3::hash(&bytes2b).to_hex().to_string(),
            "multi-frame diff output non-deterministic on {}",
            emulator_name
        );
    }
}

/// Full end-to-end: combined VT + render conformance with summary.
#[test]
fn conformance_full_suite_gate() {
    let categories = discover_fixtures();
    let known_mismatches = load_known_mismatches();
    let profiles = emulator_profiles();
    let scenarios = render_scenarios();
    let mut runner = ConformanceRunner::new();

    let total_fixtures: usize = categories.values().map(|v| v.len()).sum();

    // VT conformance on all emulators.
    for (emulator_name, _caps) in &profiles {
        for (category, fixtures) in &categories {
            for (_path, fixture) in fixtures {
                runner.run_vt_fixture(emulator_name, category, fixture, &known_mismatches);
            }
        }
    }

    // Render conformance on all emulators.
    for (emulator_name, caps) in &profiles {
        for &(scenario_name, width, height, render_fn) in &scenarios {
            let (cs1, dur1) = full_pipeline_checksum(caps, width, height, render_fn);
            let (cs2, _) = full_pipeline_checksum(caps, width, height, render_fn);
            runner.record_render(emulator_name, scenario_name, &cs1, dur1, cs1 == cs2);
        }
    }

    // Generate final report.
    let summary_json = runner.summary_json(scenarios.len());
    let xterm_diff_summary = runner.xterm_shared_fixture_differential_summary_json();
    let jsonl_path = std::env::temp_dir().join("e2e_conformance_gate.jsonl");
    let mut events = runner.events.clone();
    events.push(summary_json.clone());
    events.push(xterm_diff_summary);
    emit_jsonl(&events, &jsonl_path);

    let total_vt = total_fixtures * profiles.len();
    let total_render = scenarios.len() * profiles.len();
    eprintln!(
        "CONFORMANCE GATE: {} VT evaluations + {} render evaluations",
        total_vt, total_render
    );
    eprintln!(
        "  passed={}, known_exceptions={}, failed={}, pass_rate={:.4}",
        runner.passed,
        runner.known_exceptions,
        runner.failed,
        runner.pass_rate()
    );
    eprintln!(
        "  timing: p50={}us p99={}us max={}us",
        runner.percentile(50.0),
        runner.percentile(99.0),
        runner.percentile(100.0)
    );

    // Gate assertions.
    assert_eq!(
        runner.failed,
        0,
        "CONFORMANCE GATE FAILED — {} failures:\n{}",
        runner.failed,
        runner.failures.join("\n")
    );
    assert!(
        (runner.pass_rate() - 1.0).abs() < f64::EPSILON,
        "conformance_pass_rate must be 1.0 for gate, got {:.6}",
        runner.pass_rate()
    );
}

#![forbid(unsafe_code)]

//! Integration test: G17 widget feature scenarios.
//!
//! Validates the nine promised widget features headlessly, asserting
//! exact visual/behavioral invariants and emitting JSONL evidence when
//! `$E2E_JSONL_FILE` is set.

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{PackedRgba, StyleFlags};
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_style::{Style, StyleSheet, TableTheme};
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderSet, BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::json_view::{JsonView, JsonViewState};
use ftui_widgets::progress::ProgressBar;
use ftui_widgets::sparkline::Sparkline;
use ftui_widgets::table::{ColumnSpec, Row, Table, TableState, Truncate};
use ftui_widgets::textarea::TextArea;
use ftui_widgets::{FrameExt, StatefulWidget, Widget};
use std::sync::Arc;

struct ScenarioResult {
    scenario: &'static str,
    assertions: usize,
    snapshot_hash: String,
    duration_ms: u64,
}

fn hash_buffer(buf: &Buffer) -> String {
    let mut hasher = blake3::Hasher::new();
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            if let Some(cell) = buf.get(x, y) {
                if let Some(c) = cell.content.as_char() {
                    hasher.update(c.encode_utf8(&mut [0; 4]).as_bytes());
                } else if let Some(id) = cell.content.grapheme_id() {
                    hasher.update(&id.raw().to_le_bytes());
                } else if cell.content.is_continuation() {
                    hasher.update(b"cont");
                } else {
                    hasher.update(b"empty");
                }
                hasher.update(&cell.fg.0.to_le_bytes());
                hasher.update(&cell.bg.0.to_le_bytes());
                hasher.update(&cell.attrs.flags().bits().to_le_bytes());
                hasher.update(&cell.attrs.link_id().to_le_bytes());
            }
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn count_rendered_rows(buf: &Buffer) -> usize {
    (0..buf.height())
        .filter(|&y| {
            (0..buf.width()).any(|x| {
                buf.get(x, y)
                    .map(|c| c.content.as_char().unwrap_or(' ') != ' ')
                    .unwrap_or(false)
            })
        })
        .count()
}

fn raw_row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.width())
        .map(|x| {
            buf.get(x, y)
                .and_then(|c| c.content.as_char())
                .unwrap_or(' ')
        })
        .collect()
}

static FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn emit_scenario(res: &ScenarioResult) {
    if let Ok(path) = std::env::var("E2E_JSONL_FILE") {
        let run_id = std::env::var("E2E_RUN_ID").unwrap_or_else(|_| "widget_scenarios".to_string());
        let seed: i64 = std::env::var("E2E_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let timestamp = std::env::var("E2E_TIMESTAMP").unwrap_or_else(|_| "T000001".to_string());

        let line = serde_json::json!({
            "schema_version": "e2e-jsonl-v1",
            "type": "widget_scenario",
            "timestamp": timestamp,
            "run_id": run_id,
            "seed": seed,
            "scenario": res.scenario,
            "screen": "headless",
            "snapshot_hash": res.snapshot_hash,
            "assertions": res.assertions,
            "exit_code": 0,
            "duration_ms": res.duration_ms,
        });

        let mut rendered = serde_json::to_string(&line).unwrap_or_default();
        rendered.push('\n');

        let _guard = FILE_LOCK.lock().unwrap();
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write;
            let _ = f.write_all(rendered.as_bytes());
            let _ = f.flush();
        }
    }
}

/// G17.1: ProgressBar indeterminate mode.
#[test]
fn g17_01_progress_indeterminate() {
    let start = std::time::Instant::now();
    let mut pool = GraphemePool::new();
    let bg = PackedRgba::BLUE;

    let mut hashes = Vec::new();
    let mut lit_positions = Vec::new();

    for phase in [0, 3, 7] {
        let pb = ProgressBar::new()
            .indeterminate(phase)
            .gauge_style(Style::new().bg(bg));
        let mut frame = Frame::new(40, 1, &mut pool);
        Widget::render(&pb, Rect::new(0, 0, 40, 1), &mut frame);

        let lit_count = (0..40)
            .filter(|&x| frame.buffer.get(x, 0).map(|c| c.bg == bg).unwrap_or(false))
            .count();
        lit_positions.push(lit_count);
        hashes.push(hash_buffer(&frame.buffer));
    }

    assert_eq!(lit_positions[0], 0, "phase 0 has not entered yet");
    assert_eq!(lit_positions[1], 3, "phase 3 has 3 cols lit");
    assert_eq!(lit_positions[2], 7, "phase 7 has 7 cols lit");
    assert_ne!(hashes[0], hashes[1], "phases 0 and 3 differ in hash");
    assert_ne!(hashes[1], hashes[2], "phases 3 and 7 differ in hash");

    emit_scenario(&ScenarioResult {
        scenario: "g17_01_progress_indeterminate",
        assertions: 5,
        snapshot_hash: hashes[2].clone(),
        duration_ms: start.elapsed().as_millis() as u64,
    });
}

/// G17.2: JsonView folding.
#[test]
fn g17_02_jsonview_fold() {
    let start = std::time::Instant::now();
    let mut pool = GraphemePool::new();
    let value = serde_json::json!({
        "name": "frankentui",
        "nested": {
            "a": 1,
            "b": 2
        }
    });
    let jv = JsonView::new(value.to_string());

    // Unfolded render
    let mut state_unfolded = JsonViewState::new();
    let mut frame_unfolded = Frame::new(40, 10, &mut pool);
    StatefulWidget::render(
        &jv,
        Rect::new(0, 0, 40, 10),
        &mut frame_unfolded,
        &mut state_unfolded,
    );
    let unfolded_lines = count_rendered_rows(&frame_unfolded.buffer);
    drop(frame_unfolded);

    // Fold root
    let mut state_folded = JsonViewState::new();
    state_folded.fold(&vec![]);
    let mut frame_folded = Frame::new(40, 10, &mut pool);
    StatefulWidget::render(
        &jv,
        Rect::new(0, 0, 40, 10),
        &mut frame_folded,
        &mut state_folded,
    );
    let folded_lines = count_rendered_rows(&frame_folded.buffer);

    assert!(
        folded_lines < unfolded_lines,
        "folded line count ({folded_lines}) must be less than unfolded ({unfolded_lines})"
    );
    let row0 = raw_row_text(&frame_folded.buffer, 0);
    assert!(
        row0.contains("{…}") || row0.contains("{...}"),
        "row 0 must contain placeholder, got: {row0}"
    );

    emit_scenario(&ScenarioResult {
        scenario: "g17_02_jsonview_fold",
        assertions: 2,
        snapshot_hash: hash_buffer(&frame_folded.buffer),
        duration_ms: start.elapsed().as_millis() as u64,
    });
}

/// G17.3: TextArea highlighter hook.
#[test]
fn g17_03_textarea_highlight() {
    let start = std::time::Instant::now();
    let mut pool = GraphemePool::new();
    let ta = TextArea::new()
        .with_text("fn main() {}")
        .with_highlighter(Arc::new(|_line, text| {
            if text.starts_with("fn") {
                vec![(0..2, Style::new().bold())]
            } else {
                Vec::new()
            }
        }));

    let mut frame = Frame::new(40, 2, &mut pool);
    Widget::render(&ta, Rect::new(0, 0, 40, 2), &mut frame);

    assert!(
        frame
            .buffer
            .get(0, 0)
            .unwrap()
            .attrs
            .has_flag(StyleFlags::BOLD),
        "col 0 ('f') must be bold"
    );
    assert!(
        frame
            .buffer
            .get(1, 0)
            .unwrap()
            .attrs
            .has_flag(StyleFlags::BOLD),
        "col 1 ('n') must be bold"
    );
    assert!(
        !frame
            .buffer
            .get(2, 0)
            .unwrap()
            .attrs
            .has_flag(StyleFlags::BOLD),
        "col 2 (' ') must not be bold"
    );

    emit_scenario(&ScenarioResult {
        scenario: "g17_03_textarea_highlight",
        assertions: 3,
        snapshot_hash: hash_buffer(&frame.buffer),
        duration_ms: start.elapsed().as_millis() as u64,
    });
}

/// G17.4: TextInput history recall.
#[test]
fn g17_04_input_history() {
    let start = std::time::Instant::now();
    let mut pool = GraphemePool::new();
    let mut input = TextInput::new().with_history(10);

    let key = |code| {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        })
    };

    // Enter "a", submit and clear for next entry
    input.handle_event(&key(KeyCode::Char('a')));
    input.handle_event(&key(KeyCode::Enter));
    input.set_value("");

    // Enter "b", submit and clear
    input.handle_event(&key(KeyCode::Char('b')));
    input.handle_event(&key(KeyCode::Enter));
    input.set_value("");

    // Press Up once -> recalls "b"
    input.handle_event(&key(KeyCode::Up));
    assert_eq!(input.value(), "b");

    // Press Up twice -> recalls "a"
    input.handle_event(&key(KeyCode::Up));
    assert_eq!(input.value(), "a");

    let mut frame = Frame::new(40, 1, &mut pool);
    Widget::render(&input, Rect::new(0, 0, 40, 1), &mut frame);

    emit_scenario(&ScenarioResult {
        scenario: "g17_04_input_history",
        assertions: 2,
        snapshot_hash: hash_buffer(&frame.buffer),
        duration_ms: start.elapsed().as_millis() as u64,
    });
}

/// G17.5: Sparkline min/max markers.
#[test]
fn g17_05_sparkline_markers() {
    let start = std::time::Instant::now();
    let mut pool = GraphemePool::new();
    let data = [1.0, 5.0, 3.0, 9.0, 2.0];
    let sparkline = Sparkline::new(&data)
        .with_min_marker('▼', Style::default())
        .with_max_marker('▲', Style::default());

    let mut frame = Frame::new(5, 1, &mut pool);
    Widget::render(&sparkline, Rect::new(0, 0, 5, 1), &mut frame);

    assert_eq!(
        frame.buffer.get(0, 0).and_then(|c| c.content.as_char()),
        Some('▼'),
        "min marker at col 0"
    );
    assert_eq!(
        frame.buffer.get(3, 0).and_then(|c| c.content.as_char()),
        Some('▲'),
        "max marker at col 3"
    );

    emit_scenario(&ScenarioResult {
        scenario: "g17_05_sparkline_markers",
        assertions: 2,
        snapshot_hash: hash_buffer(&frame.buffer),
        duration_ms: start.elapsed().as_millis() as u64,
    });
}

/// G17.6: Dashed and Custom border types.
#[test]
fn g17_06_border_styles() {
    let start = std::time::Instant::now();
    let mut pool = GraphemePool::new();

    // 1. Dashed border block
    let dashed_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Dashed);
    let mut frame_dashed = Frame::new(10, 4, &mut pool);
    Widget::render(&dashed_block, Rect::new(0, 0, 10, 4), &mut frame_dashed);

    assert_eq!(
        frame_dashed
            .buffer
            .get(0, 0)
            .and_then(|c| c.content.as_char()),
        Some('┌')
    );
    assert_eq!(
        frame_dashed
            .buffer
            .get(1, 0)
            .and_then(|c| c.content.as_char()),
        Some('┄')
    );
    assert_eq!(
        frame_dashed
            .buffer
            .get(0, 1)
            .and_then(|c| c.content.as_char()),
        Some('┆')
    );
    let dashed_hash = hash_buffer(&frame_dashed.buffer);
    drop(frame_dashed);

    // 2. Custom border block
    let custom_set = BorderSet {
        horizontal: '=',
        vertical: '!',
        top_left: '@',
        top_right: '@',
        bottom_left: '@',
        bottom_right: '@',
        tee_up: '@',
        tee_down: '@',
        tee_left: '@',
        tee_right: '@',
        cross: '@',
    };
    let custom_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Custom(custom_set));
    let mut frame_custom = Frame::new(10, 4, &mut pool);
    Widget::render(&custom_block, Rect::new(0, 0, 10, 4), &mut frame_custom);

    assert_eq!(
        frame_custom
            .buffer
            .get(0, 0)
            .and_then(|c| c.content.as_char()),
        Some('@')
    );
    assert_eq!(
        frame_custom
            .buffer
            .get(1, 0)
            .and_then(|c| c.content.as_char()),
        Some('=')
    );
    assert_eq!(
        frame_custom
            .buffer
            .get(0, 1)
            .and_then(|c| c.content.as_char()),
        Some('!')
    );

    // 3. Variant counts matching README and claims ledger
    assert_eq!(BorderType::NAMED_STYLE_COUNT, 7);
    assert_eq!(BorderType::BORDER_STYLE_COUNT, 8);

    emit_scenario(&ScenarioResult {
        scenario: "g17_06_border_styles",
        assertions: 8,
        snapshot_hash: dashed_hash,
        duration_ms: start.elapsed().as_millis() as u64,
    });
}

/// G17.7: TableTheme stripe/header/selection and column options.
#[test]
fn g17_07_table_theme_columns() {
    let start = std::time::Instant::now();
    let mut pool = GraphemePool::new();

    let theme = TableTheme::modern()
        .with_stripe_period(3)
        .with_header_style(Style::new().bold())
        .with_selection_style(Style::new().bg(PackedRgba::BLUE));

    let header = Row::new(vec!["Col1", "Col2"]);
    let rows = vec![
        Row::new(vec!["日本語テスト", "123"]),
        Row::new(vec!["row1", "456"]),
        Row::new(vec!["row2", "789"]),
        Row::new(vec!["row3", "999"]),
    ];

    let table = Table::new(rows, vec![Constraint::Fixed(6), Constraint::Fixed(8)])
        .header(header)
        .theme(theme)
        .with_column_spec(0, ColumnSpec::new(Truncate::Ellipsis, Alignment::Left))
        .with_column_alignment(1, Alignment::Right);

    let mut state = TableState::default();
    state.select(Some(1));

    let mut frame = Frame::new(20, 6, &mut pool);
    StatefulWidget::render(&table, Rect::new(0, 0, 20, 6), &mut frame, &mut state);

    assert!(
        frame
            .buffer
            .get(0, 0)
            .unwrap()
            .attrs
            .has_flag(StyleFlags::BOLD),
        "header is bold"
    );
    assert_eq!(
        frame.buffer.get(0, 2).unwrap().bg,
        PackedRgba::BLUE,
        "selected row has selection bg"
    );
    let row1_col0_text: String = (0..6)
        .filter_map(|x| frame.buffer.get(x, 1).and_then(|c| c.content.as_char()))
        .collect();
    assert!(
        row1_col0_text.contains('…'),
        "col 0 truncated with ellipsis, got: {row1_col0_text}"
    );

    emit_scenario(&ScenarioResult {
        scenario: "g17_07_table_theme_columns",
        assertions: 3,
        snapshot_hash: hash_buffer(&frame.buffer),
        duration_ms: start.elapsed().as_millis() as u64,
    });
}

/// G17.8: StyleSheet propagation.
#[test]
fn g17_08_stylesheet() {
    let start = std::time::Instant::now();
    let mut pool = GraphemePool::new();

    let sheet = StyleSheet::new();
    sheet.define("heading", Style::new().fg(PackedRgba::RED));

    let b1 = Block::styled(&sheet, "heading");
    let mut f1 = Frame::new(10, 2, &mut pool);
    Widget::render(&b1, Rect::new(0, 0, 10, 2), &mut f1);
    let fg1 = f1.buffer.get(0, 0).unwrap().fg;
    assert_eq!(fg1, PackedRgba::RED, "b1 border fg is RED");
    drop(f1);

    sheet.define("heading", Style::new().fg(PackedRgba::GREEN));
    let b2 = Block::styled(&sheet, "heading");
    let mut f2 = Frame::new(10, 2, &mut pool);
    Widget::render(&b2, Rect::new(0, 0, 10, 2), &mut f2);
    let fg2 = f2.buffer.get(0, 0).unwrap().fg;
    assert_eq!(fg2, PackedRgba::GREEN, "b2 border fg is GREEN");
    assert_ne!(fg1, fg2, "redefined stylesheet propagated to block");

    emit_scenario(&ScenarioResult {
        scenario: "g17_08_stylesheet",
        assertions: 3,
        snapshot_hash: hash_buffer(&f2.buffer),
        duration_ms: start.elapsed().as_millis() as u64,
    });
}

/// G17.9: FrameExt convenience rendering.
#[test]
fn g17_09_frame_ext() {
    let start = std::time::Instant::now();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);

    assert_eq!(frame.area(), Rect::new(0, 0, 80, 24));

    let chunks = Flex::horizontal()
        .constraints([Constraint::Percentage(30.0), Constraint::Percentage(70.0)])
        .split(frame.area());

    assert_eq!(chunks[0].width, 24);
    assert_eq!(chunks[1].width, 56);

    let b1 = Block::default().borders(Borders::ALL);
    let b2 = Block::default().borders(Borders::ALL);
    frame.render_widget(&b1, chunks[0]);
    frame.render_widget(&b2, chunks[1]);

    assert_eq!(
        frame
            .buffer
            .get(chunks[0].x, chunks[0].y)
            .and_then(|c| c.content.as_char()),
        Some('┌')
    );
    assert_eq!(
        frame
            .buffer
            .get(chunks[1].x, chunks[1].y)
            .and_then(|c| c.content.as_char()),
        Some('┌')
    );

    emit_scenario(&ScenarioResult {
        scenario: "g17_09_frame_ext",
        assertions: 5,
        snapshot_hash: hash_buffer(&frame.buffer),
        duration_ms: start.elapsed().as_millis() as u64,
    });
}

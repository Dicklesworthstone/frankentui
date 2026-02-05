#![forbid(unsafe_code)]

//! Integration tests for Widget + Frame API.
//!
//! These tests validate that widgets can:
//! - Write to the frame buffer
//! - Register hit regions
//! - Set cursor position
//! - Respect degradation levels

use ftui_core::geometry::Rect;
use ftui_layout::Constraint;
use ftui_render::budget::DegradationLevel;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::{Frame, HitId};
use ftui_render::grapheme_pool::GraphemePool;
use ftui_style::{
    TableEffect, TableEffectRule, TableEffectTarget, TableTheme, TableThemeDiagnostics,
};
use ftui_widgets::StatefulWidget;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::BorderType;
use ftui_widgets::help::{Help, HelpEntry, HelpMode, HelpRenderState};
use ftui_widgets::input::TextInput;
use ftui_widgets::list::List;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::ProgressBar;
use ftui_widgets::rule::Rule;
use ftui_widgets::table::{Row, Table, TableState};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;
use tracing::{Level, info};

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(Level::INFO)
        .try_init();
}

fn jsonl_enabled() -> bool {
    std::env::var("E2E_JSONL").is_ok() || std::env::var("CI").is_ok()
}

fn jsonl_timestamp() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("T{n:06}")
}

fn log_jsonl(step: &str, fields: &[(&str, String)]) {
    let mut parts = Vec::with_capacity(fields.len() + 2);
    parts.push(format!("\"ts\":\"{}\"", jsonl_timestamp()));
    parts.push(format!("\"step\":\"{}\"", step));
    parts.extend(fields.iter().map(|(k, v)| format!("\"{}\":\"{}\"", k, v)));
    eprintln!("{{{}}}", parts.join(","));
}

fn buffer_checksum(frame: &Frame) -> u64 {
    let mut hasher = DefaultHasher::new();
    let width = frame.buffer.width();
    let height = frame.buffer.height();
    for y in 0..height {
        for x in 0..width {
            if let Some(cell) = frame.buffer.get(x, y) {
                cell.content.hash(&mut hasher);
                cell.fg.0.hash(&mut hasher);
                cell.bg.0.hash(&mut hasher);
                cell.attrs.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn perf_rows(rows: usize, cols: usize) -> Vec<Row> {
    (0..rows)
        .map(|r| {
            Row::new((0..cols).map(|c| format!("R{r:02}C{c:02}")))
                .height(1)
                .bottom_margin(0)
        })
        .collect()
}

fn perf_header(cols: usize) -> Row {
    Row::new((0..cols).map(|c| format!("Col {c}"))).height(1)
}

fn perf_widths(cols: usize) -> Vec<Constraint> {
    vec![Constraint::Fixed(12); cols]
}

fn theme_perf_variants() -> [(String, TableTheme, f32); 2] {
    let base = TableTheme::aurora();
    let highlight_fg = base.row_hover.fg.unwrap_or(PackedRgba::rgb(240, 245, 255));
    let highlight_bg = base.row_hover.bg.unwrap_or(PackedRgba::rgb(40, 70, 110));
    let effect = TableTheme::aurora().with_effect(
        TableEffectRule::new(
            TableEffectTarget::Row(0),
            TableEffect::BreathingGlow {
                fg: highlight_fg,
                bg: highlight_bg,
                intensity: 0.22,
                speed: 1.0,
                phase_offset: 0.25,
                asymmetry: 0.12,
            },
        )
        .priority(1),
    );

    [
        ("baseline".to_string(), base, 0.0),
        ("effect".to_string(), effect, 0.37),
    ]
}

fn theme_diag_fields(diag: &TableThemeDiagnostics) -> Vec<(&'static str, String)> {
    vec![
        ("preset_id", format!("{:?}", diag.preset_id)),
        ("style_hash", format!("{:016x}", diag.style_hash)),
        ("effects_hash", format!("{:016x}", diag.effects_hash)),
        ("effect_count", diag.effect_count.to_string()),
        ("padding", diag.padding.to_string()),
        ("column_gap", diag.column_gap.to_string()),
        ("row_height", diag.row_height.to_string()),
    ]
}

struct BufferWidget;

impl Widget for BufferWidget {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }
        frame.buffer.set(area.x, area.y, Cell::from_char('X'));
    }
}

struct HitWidget {
    id: HitId,
}

impl Widget for HitWidget {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }
        let rect = Rect::new(area.x, area.y, 1, 1);
        frame.register_hit_region(rect, self.id);
    }
}

struct CursorWidget;

impl Widget for CursorWidget {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }
        frame.set_cursor(Some((area.x, area.y)));
        frame.set_cursor_visible(true);
    }
}

struct DegradationWidget;

impl Widget for DegradationWidget {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }
        let ch = if frame.buffer.degradation == DegradationLevel::EssentialOnly {
            'E'
        } else {
            'F'
        };
        frame.buffer.set(area.x, area.y, Cell::from_char(ch));
    }
}

#[test]
fn frame_buffer_access_from_widget() {
    init_tracing();
    info!("frame buffer access via Widget::render");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(2, 1, &mut pool);
    let area = Rect::new(0, 0, 2, 1);

    BufferWidget.render(area, &mut frame);

    let cell = frame.buffer.get(0, 0).unwrap();
    assert_eq!(cell.content.as_char(), Some('X'));
}

#[test]
fn frame_hit_grid_registration_and_lookup() {
    init_tracing();
    info!("hit grid registration via Widget::render");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(2, 1, &mut pool);
    let area = Rect::new(0, 0, 2, 1);

    let id = HitId::new(42);
    HitWidget { id }.render(area, &mut frame);

    let hit = frame.hit_test(0, 0).expect("expected hit at (0,0)");
    assert_eq!(hit.0, id);
}

#[test]
fn frame_cursor_position_set_and_clear() {
    init_tracing();
    info!("cursor position set/clear");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(2, 1, &mut pool);
    let area = Rect::new(0, 0, 2, 1);

    CursorWidget.render(area, &mut frame);
    assert_eq!(frame.cursor_position, Some((0, 0)));

    frame.set_cursor(None);
    assert_eq!(frame.cursor_position, None);
}

#[test]
fn frame_degradation_propagates_to_buffer() {
    init_tracing();
    info!("degradation level propagates to buffer");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    frame.set_degradation(DegradationLevel::EssentialOnly);

    DegradationWidget.render(Rect::new(0, 0, 1, 1), &mut frame);

    let cell = frame.buffer.get(0, 0).unwrap();
    assert_eq!(cell.content.as_char(), Some('E'));
    assert_eq!(frame.buffer.degradation, DegradationLevel::EssentialOnly);
}

#[test]
fn block_renders_borders_in_frame() {
    init_tracing();
    info!("block renders borders in frame");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(3, 3, &mut pool);
    let block = Block::bordered().border_type(BorderType::Ascii);

    block.render(Rect::new(0, 0, 3, 3), &mut frame);

    let cell = frame.buffer.get(0, 0).unwrap();
    assert_eq!(cell.content.as_char(), Some('+'));
}

#[test]
fn paragraph_renders_text_in_frame() {
    init_tracing();
    info!("paragraph renders text in frame");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(5, 1, &mut pool);
    let paragraph = Paragraph::new("Hi");

    paragraph.render(Rect::new(0, 0, 5, 1), &mut frame);

    let cell = frame.buffer.get(0, 0).unwrap();
    assert_eq!(cell.content.as_char(), Some('H'));
}

#[test]
fn rule_renders_line_in_frame() {
    init_tracing();
    info!("rule renders line in frame");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(4, 1, &mut pool);
    let rule = Rule::new().border_type(BorderType::Ascii);

    rule.render(Rect::new(0, 0, 4, 1), &mut frame);

    let cell = frame.buffer.get(0, 0).unwrap();
    assert_eq!(cell.content.as_char(), Some('-'));
}

#[test]
fn list_registers_hit_regions_in_frame() {
    init_tracing();
    info!("list registers hit regions in frame");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(4, 2, &mut pool);
    let list = List::new(["a", "b"]).hit_id(HitId::new(7));

    Widget::render(&list, Rect::new(0, 0, 4, 2), &mut frame);

    let hit0 = frame.hit_test(0, 0).expect("expected hit at row 0");
    let hit1 = frame.hit_test(0, 1).expect("expected hit at row 1");
    assert_eq!(hit0.0, HitId::new(7));
    assert_eq!(hit1.0, HitId::new(7));
    assert_eq!(hit0.2, 0);
    assert_eq!(hit1.2, 1);
}

#[test]
fn text_input_sets_cursor_in_frame() {
    init_tracing();
    info!("text input sets cursor in frame");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(5, 1, &mut pool);
    let input = TextInput::new().with_value("hi").with_focused(true);

    input.render(Rect::new(0, 0, 5, 1), &mut frame);

    assert_eq!(frame.cursor_position, Some((2, 0)));
}

#[test]
fn progress_bar_essential_only_renders_percentage() {
    init_tracing();
    info!("progress bar renders percentage at EssentialOnly");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(4, 1, &mut pool);
    frame.set_degradation(DegradationLevel::EssentialOnly);

    let pb = ProgressBar::new().ratio(0.5);
    pb.render(Rect::new(0, 0, 4, 1), &mut frame);

    let c0 = frame.buffer.get(0, 0).unwrap().content.as_char();
    let c1 = frame.buffer.get(1, 0).unwrap().content.as_char();
    let c2 = frame.buffer.get(2, 0).unwrap().content.as_char();
    assert_eq!(c0, Some('5'));
    assert_eq!(c1, Some('0'));
    assert_eq!(c2, Some('%'));
}

#[test]
fn zero_area_widgets_do_not_panic() {
    init_tracing();
    info!("widgets handle zero-area renders without panic");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);

    Block::bordered().render(area, &mut frame);
    Paragraph::new("Hi").render(area, &mut frame);
    Rule::new().render(area, &mut frame);
}

#[test]
fn help_hints_focus_change_storm_e2e() {
    init_tracing();
    info!("help hints focus-change storm with cache/dirty logging");

    let mut entries = vec![
        HelpEntry::new("^T", "Theme"),
        HelpEntry::new("^C", "Open"),
        HelpEntry::new("?", "Help"),
        HelpEntry::new("F12", "Debug"),
    ];
    let mut help = Help::new()
        .with_mode(HelpMode::Short)
        .with_entries(entries.clone());

    let mut state = HelpRenderState::default();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 1, &mut pool);
    let area = Rect::new(0, 0, 120, 1);

    StatefulWidget::render(&help, area, &mut frame, &mut state);

    let iterations = 200usize;
    let run_id = format!("bd-a8wk-{}", std::process::id());
    let log_enabled = jsonl_enabled();

    if log_enabled {
        log_jsonl(
            "env",
            &[
                ("run_id", run_id.clone()),
                ("case", "help_hints_focus_storm".to_string()),
                ("mode", "short".to_string()),
                ("width", area.width.to_string()),
                ("height", area.height.to_string()),
                ("iterations", iterations.to_string()),
                ("term", std::env::var("TERM").unwrap_or_default()),
                ("colorterm", std::env::var("COLORTERM").unwrap_or_default()),
            ],
        );
    }

    let mut times_us = Vec::with_capacity(iterations);
    let mut dirty_cells = Vec::with_capacity(iterations);
    let mut dirty_counts = Vec::with_capacity(iterations);
    let mut total_hits = 0u64;
    let mut total_misses = 0u64;
    let mut total_dirty_updates = 0u64;
    let mut total_layout_rebuilds = 0u64;

    for i in 0..iterations {
        let label = if i % 2 == 0 { "Open" } else { "Edit" };
        entries[1].desc.clear();
        entries[1].desc.push_str(label);
        help = help.with_entries(entries.clone());

        let before = state.stats();
        let start = Instant::now();
        StatefulWidget::render(&help, area, &mut frame, &mut state);
        let render_us = start.elapsed().as_micros() as u64;
        let after = state.stats();

        let hits = after.hits.saturating_sub(before.hits);
        let misses = after.misses.saturating_sub(before.misses);
        let dirty_updates = after.dirty_updates.saturating_sub(before.dirty_updates);
        let layout_rebuilds = after.layout_rebuilds.saturating_sub(before.layout_rebuilds);

        let dirty = state.take_dirty_rects();
        let dirty_cell_count: u64 = dirty
            .iter()
            .map(|rect| rect.width as u64 * rect.height as u64)
            .sum();
        let checksum = buffer_checksum(&frame);

        times_us.push(render_us);
        dirty_cells.push(dirty_cell_count);
        dirty_counts.push(dirty.len() as u64);
        total_hits += hits;
        total_misses += misses;
        total_dirty_updates += dirty_updates;
        total_layout_rebuilds += layout_rebuilds;

        if log_enabled {
            log_jsonl(
                "frame",
                &[
                    ("run_id", run_id.clone()),
                    ("idx", i.to_string()),
                    ("render_us", render_us.to_string()),
                    ("dirty_rects", dirty.len().to_string()),
                    ("dirty_cells", dirty_cell_count.to_string()),
                    ("hits", hits.to_string()),
                    ("misses", misses.to_string()),
                    ("dirty_updates", dirty_updates.to_string()),
                    ("layout_rebuilds", layout_rebuilds.to_string()),
                    ("checksum", format!("{checksum:016x}")),
                ],
            );
        }
    }

    times_us.sort();
    dirty_cells.sort();
    dirty_counts.sort();
    let len = times_us.len();
    let p50 = times_us[len / 2];
    let p95 = times_us[((len as f64 * 0.95) as usize).min(len.saturating_sub(1))];
    let p99 = times_us[((len as f64 * 0.99) as usize).min(len.saturating_sub(1))];
    let dirty_len = dirty_cells.len();
    let dirty_p50 = dirty_cells[dirty_len / 2];
    let dirty_p95 =
        dirty_cells[((dirty_len as f64 * 0.95) as usize).min(dirty_len.saturating_sub(1))];
    let counts_len = dirty_counts.len();
    let dirty_rect_p50 = dirty_counts[counts_len / 2];
    let dirty_rect_p95 =
        dirty_counts[((counts_len as f64 * 0.95) as usize).min(counts_len.saturating_sub(1))];

    if log_enabled {
        log_jsonl(
            "summary",
            &[
                ("run_id", run_id),
                ("p50_us", p50.to_string()),
                ("p95_us", p95.to_string()),
                ("p99_us", p99.to_string()),
                ("dirty_cells_p50", dirty_p50.to_string()),
                ("dirty_cells_p95", dirty_p95.to_string()),
                ("dirty_rects_p50", dirty_rect_p50.to_string()),
                ("dirty_rects_p95", dirty_rect_p95.to_string()),
                ("hits_total", total_hits.to_string()),
                ("misses_total", total_misses.to_string()),
                ("dirty_updates_total", total_dirty_updates.to_string()),
                ("layout_rebuilds_total", total_layout_rebuilds.to_string()),
            ],
        );
    }

    assert_eq!(
        total_misses, 0,
        "focus-change updates should not trigger layout rebuilds"
    );
    assert_eq!(
        total_layout_rebuilds, 0,
        "layout rebuilds should be avoided for stable hint widths"
    );
    assert!(total_dirty_updates > 0, "dirty updates should be recorded");
}

#[test]
fn table_theme_perf_baseline_vs_effect_jsonl() {
    init_tracing();
    info!("table theme perf baseline vs effects");

    let log_enabled = jsonl_enabled();
    let iterations = if std::env::var("CI").is_ok() { 120 } else { 40 };
    let sizes: [(u16, u16); 2] = [(80, 24), (120, 40)];
    let run_id = format!("bd-2k018-17-{}", std::process::id());

    let rows = 12usize;
    let cols = 4usize;
    let rows_data = perf_rows(rows, cols);
    let header = perf_header(cols);
    let widths = perf_widths(cols);

    for (width, height) in sizes {
        let area = Rect::new(0, 0, width, height);
        for (label, theme, phase) in theme_perf_variants() {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(width, height, &mut pool);
            let mut state = TableState::default();
            let table = Table::new(rows_data.clone(), widths.clone())
                .header(header.clone())
                .theme(theme.clone())
                .theme_phase(phase)
                .column_spacing(theme.column_gap as u16);

            if log_enabled {
                let mut fields = vec![
                    ("run_id", run_id.clone()),
                    ("case", "table_theme_perf".to_string()),
                    ("mode", label.clone()),
                    ("width", width.to_string()),
                    ("height", height.to_string()),
                    ("iterations", iterations.to_string()),
                    ("rows", rows.to_string()),
                    ("cols", cols.to_string()),
                    ("phase", format!("{phase:.3}")),
                    ("alloc_tracking", "none".to_string()),
                ];
                let diag = theme.diagnostics();
                fields.extend(theme_diag_fields(&diag));
                log_jsonl("table_perf_env", &fields);
            }

            StatefulWidget::render(&table, area, &mut frame, &mut state);

            let start = Instant::now();
            for _ in 0..iterations {
                StatefulWidget::render(&table, area, &mut frame, &mut state);
            }
            let elapsed_us = start.elapsed().as_micros() as u64;
            let per_iter_us = elapsed_us as f64 / iterations as f64;
            let checksum = buffer_checksum(&frame);

            if log_enabled {
                let mut fields = vec![
                    ("run_id", run_id.clone()),
                    ("mode", label.clone()),
                    ("width", width.to_string()),
                    ("height", height.to_string()),
                    ("iterations", iterations.to_string()),
                    ("elapsed_us", elapsed_us.to_string()),
                    ("per_iter_us", format!("{per_iter_us:.3}")),
                    ("checksum", format!("{checksum:016x}")),
                ];
                let diag = theme.diagnostics();
                fields.extend(theme_diag_fields(&diag));
                log_jsonl("table_perf_summary", &fields);
            }

            assert_ne!(checksum, 0, "table render should populate buffer");
        }
    }
}

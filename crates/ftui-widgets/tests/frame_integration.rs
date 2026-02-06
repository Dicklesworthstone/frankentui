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
use ftui_render::frame::{Frame, HitId, HitRegion};
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
use ftui_widgets::modal::{Dialog, DialogResult, DialogState};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::ProgressBar;
use ftui_widgets::rule::Rule;
use ftui_widgets::scrollbar::{Scrollbar, ScrollbarOrientation, ScrollbarState};
use ftui_widgets::table::{Row, Table, TableState};
use ftui_widgets::tree::{Tree, TreeNode};
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

// -----------------------------------------------------------------------
// bd-iuvb.17.3: Widget hit region tests
// -----------------------------------------------------------------------

#[test]
fn scrollbar_registers_hit_regions_with_track_pos() {
    init_tracing();
    info!("scrollbar registers hit regions with track position data");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(1, 5, &mut pool);
    let area = Rect::new(0, 0, 1, 5);
    let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight).hit_id(HitId::new(5));
    let mut state = ScrollbarState::new(100, 0, 10);

    StatefulWidget::render(&sb, area, &mut frame, &mut state);

    // Encoding is (part << 56) | track_position (2-field format, no track_len).
    for y in 0..5u16 {
        let (id, region, data) = frame.hit_test(0, y).expect("expected hit");
        assert_eq!(id, HitId::new(5));
        assert_eq!(region, HitRegion::Scrollbar);
        let track_pos = (data & 0x00FF_FFFF_FFFF_FFFF) as u16;
        assert_eq!(track_pos, y, "track_pos at y={y} should equal y");
    }
}

#[test]
fn table_registers_hit_regions_in_frame() {
    init_tracing();
    info!("table registers hit regions in frame");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(20, 3, &mut pool);
    let rows = [
        Row::new(["a"]).height(1).bottom_margin(0),
        Row::new(["b"]).height(1).bottom_margin(0),
        Row::new(["c"]).height(1).bottom_margin(0),
    ];
    let table = Table::new(rows, [Constraint::Fixed(10)]).hit_id(HitId::new(99));

    Widget::render(&table, Rect::new(0, 0, 20, 3), &mut frame);

    let hit0 = frame.hit_test(0, 0);
    let hit1 = frame.hit_test(0, 1);
    let hit2 = frame.hit_test(0, 2);
    assert_eq!(hit0, Some((HitId::new(99), HitRegion::Content, 0)));
    assert_eq!(hit1, Some((HitId::new(99), HitRegion::Content, 1)));
    assert_eq!(hit2, Some((HitId::new(99), HitRegion::Content, 2)));
}

#[test]
fn list_hit_data_encodes_item_index() {
    init_tracing();
    info!("list hit data encodes item index");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(10, 5, &mut pool);
    let list = List::new(["zero", "one", "two", "three", "four"]).hit_id(HitId::new(42));

    Widget::render(&list, Rect::new(0, 0, 10, 5), &mut frame);

    for y in 0..5u16 {
        let (id, region, data) = frame.hit_test(0, y).expect("expected hit");
        assert_eq!(id, HitId::new(42));
        assert_eq!(region, HitRegion::Content);
        assert_eq!(data, y as u64, "item index at row {y}");
    }
}

#[test]
fn table_hit_data_encodes_row_index() {
    init_tracing();
    info!("table hit data encodes row index");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(20, 4, &mut pool);
    let rows = [
        Row::new(["r0"]).height(1).bottom_margin(0),
        Row::new(["r1"]).height(1).bottom_margin(0),
        Row::new(["r2"]).height(1).bottom_margin(0),
        Row::new(["r3"]).height(1).bottom_margin(0),
    ];
    let table = Table::new(rows, [Constraint::Fixed(10)]).hit_id(HitId::new(77));

    Widget::render(&table, Rect::new(0, 0, 20, 4), &mut frame);

    for y in 0..4u16 {
        let (id, _region, data) = frame.hit_test(0, y).expect("expected hit");
        assert_eq!(id, HitId::new(77));
        assert_eq!(data, y as u64, "row index at row {y}");
    }
}

#[test]
fn scrollbar_encodes_track_and_thumb_parts() {
    init_tracing();
    info!("scrollbar encodes distinct part values for thumb vs track");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(1, 10, &mut pool);
    let area = Rect::new(0, 0, 1, 10);
    let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight).hit_id(HitId::new(20));
    let mut state = ScrollbarState::new(100, 0, 10);

    StatefulWidget::render(&sb, area, &mut frame, &mut state);

    let (_, _, data0) = frame.hit_test(0, 0).expect("hit at y=0");
    let part0 = data0 >> 56;
    assert_eq!(part0, 1, "y=0 should be SCROLLBAR_PART_THUMB");

    let (_, _, data1) = frame.hit_test(0, 1).expect("hit at y=1");
    let part1 = data1 >> 56;
    assert_eq!(part1, 0, "y=1 should be SCROLLBAR_PART_TRACK");
}

#[test]
fn multiple_widgets_coexist_with_different_hit_ids() {
    init_tracing();
    info!("multiple widgets coexist with different hit ids");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(20, 4, &mut pool);

    let list = List::new(["a", "b"]).hit_id(HitId::new(10));
    Widget::render(&list, Rect::new(0, 0, 10, 2), &mut frame);

    let rows = [
        Row::new(["x"]).height(1).bottom_margin(0),
        Row::new(["y"]).height(1).bottom_margin(0),
    ];
    let table = Table::new(rows, [Constraint::Fixed(10)]).hit_id(HitId::new(20));
    Widget::render(&table, Rect::new(0, 2, 10, 2), &mut frame);

    let (id_list, _, _) = frame.hit_test(0, 0).expect("list hit");
    let (id_table, _, _) = frame.hit_test(0, 2).expect("table hit");
    assert_eq!(id_list, HitId::new(10));
    assert_eq!(id_table, HitId::new(20));
}

#[test]
fn no_hit_id_means_no_hit_regions() {
    init_tracing();
    info!("list without hit_id produces no hit regions");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(10, 2, &mut pool);
    let list = List::new(["a", "b"]);

    Widget::render(&list, Rect::new(0, 0, 10, 2), &mut frame);

    assert!(frame.hit_test(0, 0).is_none(), "no hit at (0,0)");
    assert!(frame.hit_test(0, 1).is_none(), "no hit at (0,1)");
}

#[test]
fn table_without_hit_id_has_no_hit_regions() {
    init_tracing();
    info!("table without hit_id produces no hit regions");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(20, 2, &mut pool);
    let rows = [
        Row::new(["a"]).height(1).bottom_margin(0),
        Row::new(["b"]).height(1).bottom_margin(0),
    ];
    let table = Table::new(rows, [Constraint::Fixed(10)]);

    Widget::render(&table, Rect::new(0, 0, 20, 2), &mut frame);

    assert!(frame.hit_test(0, 0).is_none(), "no hit at (0,0)");
    assert!(frame.hit_test(0, 1).is_none(), "no hit at (0,1)");
}

#[test]
fn scrollbar_without_hit_id_has_no_hit_regions() {
    init_tracing();
    info!("scrollbar without hit_id produces no hit regions");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(1, 5, &mut pool);
    let area = Rect::new(0, 0, 1, 5);
    let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    let mut state = ScrollbarState::new(100, 0, 10);

    StatefulWidget::render(&sb, area, &mut frame, &mut state);

    for y in 0..5u16 {
        assert!(frame.hit_test(0, y).is_none(), "no hit at y={y}");
    }
}

#[test]
fn hit_regions_are_deterministic_across_renders() {
    init_tracing();
    info!("hit regions are deterministic across renders");

    let make_frame = || {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(10, 3, &mut pool);
        let list = List::new(["a", "b", "c"]).hit_id(HitId::new(50));
        Widget::render(&list, Rect::new(0, 0, 10, 3), &mut frame);
        let mut hits = Vec::new();
        for y in 0..3u16 {
            hits.push(frame.hit_test(0, y));
        }
        hits
    };

    let hits1 = make_frame();
    let hits2 = make_frame();
    assert_eq!(hits1, hits2, "two renders produce identical hit results");
}

// -----------------------------------------------------------------------
// bd-iuvb.17.3: Dialog hit region tests
// -----------------------------------------------------------------------

#[test]
fn dialog_modal_backdrop_covers_full_area() {
    init_tracing();
    info!("dialog modal has backdrop, content, and button hit regions");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    let dialog = Dialog::confirm("Title", "Message").hit_id(HitId::new(100));
    let mut state = DialogState::new();

    StatefulWidget::render(&dialog, area, &mut frame, &mut state);

    // Scan all cells and categorize hit regions.
    let mut has_backdrop = false;
    let mut has_content = false;
    let mut has_button = false;
    for y in 0..24u16 {
        for x in 0..80u16 {
            if let Some((id, region, _data)) = frame.hit_test(x, y)
                && id == HitId::new(100)
            {
                match region {
                    HitRegion::Custom(1) => has_backdrop = true,
                    HitRegion::Custom(2) => has_content = true,
                    HitRegion::Button => has_button = true,
                    _ => {}
                }
            }
        }
    }
    assert!(has_backdrop, "Dialog should have backdrop hit regions");
    assert!(has_content, "Dialog should have content hit regions");
    assert!(
        has_button,
        "Dialog button hit regions should NOT be overwritten by backdrop/content"
    );
}

#[test]
fn dialog_without_hit_id_has_no_hit_regions() {
    init_tracing();
    info!("dialog without hit_id produces no hit regions");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    let dialog = Dialog::confirm("Title", "Message");
    let mut state = DialogState::new();

    dialog.render(area, &mut frame, &mut state);

    let mut any_hit = false;
    for y in 0..24u16 {
        for x in 0..80u16 {
            if frame.hit_test(x, y).is_some() {
                any_hit = true;
                break;
            }
        }
        if any_hit {
            break;
        }
    }
    assert!(!any_hit, "Dialog without hit_id should have no hit regions");
}

#[test]
fn dialog_closed_state_renders_nothing() {
    init_tracing();
    info!("closed dialog renders no hit regions");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    let dialog = Dialog::alert("Alert", "Warning!").hit_id(HitId::new(200));
    let mut state = DialogState::new();
    state.close(DialogResult::Cancel);

    StatefulWidget::render(&dialog, area, &mut frame, &mut state);

    let mut any_hit = false;
    for y in 0..24u16 {
        for x in 0..80u16 {
            if frame.hit_test(x, y).is_some() {
                any_hit = true;
                break;
            }
        }
        if any_hit {
            break;
        }
    }
    assert!(!any_hit, "Closed dialog should render nothing");
}

// -----------------------------------------------------------------------
// bd-iuvb.17.3: Tree hit region tests
// -----------------------------------------------------------------------

#[test]
fn tree_registers_hit_regions_in_frame() {
    init_tracing();
    info!("tree registers hit regions in frame");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(30, 5, &mut pool);

    let tree = Tree::new(
        TreeNode::new("root")
            .child(TreeNode::new("child1"))
            .child(TreeNode::new("child2")),
    )
    .hit_id(HitId::new(60));

    // show_root=true by default: root + child1 + child2 = 3 rows
    Widget::render(&tree, Rect::new(0, 0, 30, 5), &mut frame);

    let hit0 = frame.hit_test(0, 0).expect("expected hit at row 0 (root)");
    let hit1 = frame
        .hit_test(0, 1)
        .expect("expected hit at row 1 (child1)");
    let hit2 = frame
        .hit_test(0, 2)
        .expect("expected hit at row 2 (child2)");
    assert_eq!(hit0.0, HitId::new(60));
    assert_eq!(hit1.0, HitId::new(60));
    assert_eq!(hit2.0, HitId::new(60));
    // All regions should be Content
    assert_eq!(hit0.1, HitRegion::Content);
    assert_eq!(hit1.1, HitRegion::Content);
    assert_eq!(hit2.1, HitRegion::Content);
}

#[test]
fn tree_hit_data_encodes_visible_row_index() {
    init_tracing();
    info!("tree hit data encodes visible row index");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(40, 6, &mut pool);

    // root → child1 → grandchild, child2
    let tree = Tree::new(
        TreeNode::new("root")
            .child(TreeNode::new("child1").child(TreeNode::new("grandchild")))
            .child(TreeNode::new("child2")),
    )
    .hit_id(HitId::new(61));

    // Visible rows: 0=root, 1=child1, 2=grandchild, 3=child2
    Widget::render(&tree, Rect::new(0, 0, 40, 6), &mut frame);

    for expected_row in 0u16..4 {
        let msg = format!("expected hit at row {expected_row}");
        let (id, region, data) = frame.hit_test(0, expected_row).expect(&msg);
        assert_eq!(id, HitId::new(61));
        assert_eq!(region, HitRegion::Content);
        assert_eq!(
            data, expected_row as u64,
            "visible row index at row {expected_row}"
        );
    }
    // Row 4 has no tree content
    assert!(frame.hit_test(0, 4).is_none(), "no hit past tree content");
}

#[test]
fn tree_without_hit_id_has_no_hit_regions() {
    init_tracing();
    info!("tree without hit_id produces no hit regions");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(30, 5, &mut pool);

    let tree = Tree::new(
        TreeNode::new("root")
            .child(TreeNode::new("child1"))
            .child(TreeNode::new("child2")),
    );

    Widget::render(&tree, Rect::new(0, 0, 30, 5), &mut frame);

    for y in 0..3u16 {
        assert!(frame.hit_test(0, y).is_none(), "no hit at y={y}");
    }
}

#[test]
fn tree_collapsed_subtree_skips_hit_regions() {
    init_tracing();
    info!("collapsed subtree nodes do not register hit regions");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(40, 6, &mut pool);

    // child1 is collapsed, so grandchild is not rendered
    let tree = Tree::new(
        TreeNode::new("root")
            .child(
                TreeNode::new("child1")
                    .with_expanded(false)
                    .child(TreeNode::new("grandchild")),
            )
            .child(TreeNode::new("child2")),
    )
    .hit_id(HitId::new(62));

    // Visible rows: 0=root, 1=child1(collapsed), 2=child2
    Widget::render(&tree, Rect::new(0, 0, 40, 6), &mut frame);

    let (_, _, data0) = frame.hit_test(0, 0).expect("root");
    let (_, _, data1) = frame.hit_test(0, 1).expect("child1");
    let (_, _, data2) = frame.hit_test(0, 2).expect("child2");
    assert_eq!(data0, 0, "root at visible row 0");
    assert_eq!(data1, 1, "child1 at visible row 1");
    assert_eq!(data2, 2, "child2 at visible row 2");

    // Row 3 should have no hit (grandchild is hidden)
    assert!(frame.hit_test(0, 3).is_none(), "grandchild not rendered");
}

#[test]
fn tree_hidden_root_adjusts_hit_indices() {
    init_tracing();
    info!("tree with show_root=false starts hit indices at children");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(30, 5, &mut pool);

    let tree = Tree::new(
        TreeNode::new("root")
            .child(TreeNode::new("child1"))
            .child(TreeNode::new("child2")),
    )
    .with_show_root(false)
    .hit_id(HitId::new(63));

    // Visible rows: 0=child1, 1=child2 (root is hidden)
    Widget::render(&tree, Rect::new(0, 0, 30, 5), &mut frame);

    let (id0, _, data0) = frame.hit_test(0, 0).expect("child1");
    let (id1, _, data1) = frame.hit_test(0, 1).expect("child2");
    assert_eq!(id0, HitId::new(63));
    assert_eq!(id1, HitId::new(63));
    assert_eq!(data0, 0, "child1 at visible row 0");
    assert_eq!(data1, 1, "child2 at visible row 1");
    assert!(frame.hit_test(0, 2).is_none(), "no hit at row 2");
}

// -----------------------------------------------------------------------
// bd-iuvb.17.3: Dialog button hit data encoding
// -----------------------------------------------------------------------

#[test]
fn dialog_button_hit_data_encodes_button_index() {
    init_tracing();
    info!("dialog button hit data encodes sequential button indices");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    let dialog = Dialog::confirm("Title", "Message").hit_id(HitId::new(101));
    let mut state = DialogState::new();

    StatefulWidget::render(&dialog, area, &mut frame, &mut state);

    // Collect all Button hit regions and their data values.
    let mut button_data: Vec<u64> = Vec::new();
    for y in 0..24u16 {
        for x in 0..80u16 {
            if let Some((id, HitRegion::Button, data)) = frame.hit_test(x, y)
                && id == HitId::new(101)
                && !button_data.contains(&data)
            {
                button_data.push(data);
            }
        }
    }
    button_data.sort();

    // Dialog::confirm has 2 buttons: OK(0) and Cancel(1)
    assert_eq!(
        button_data.len(),
        2,
        "confirm dialog should have exactly 2 buttons, found: {button_data:?}"
    );
    assert_eq!(button_data[0], 0, "first button index");
    assert_eq!(button_data[1], 1, "second button index");
}

// -----------------------------------------------------------------------
// bd-iuvb.17.3: Cross-widget hit region determinism
// -----------------------------------------------------------------------

#[test]
fn cross_widget_hit_regions_deterministic() {
    init_tracing();
    info!("multiple widget types produce deterministic hit regions");

    let make_hits = || {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(40, 12, &mut pool);

        // List in top rows
        let list = List::new(["a", "b", "c"]).hit_id(HitId::new(10));
        Widget::render(&list, Rect::new(0, 0, 20, 3), &mut frame);

        // Table in middle rows
        let rows = [
            Row::new(["x"]).height(1).bottom_margin(0),
            Row::new(["y"]).height(1).bottom_margin(0),
        ];
        let table = Table::new(rows, [Constraint::Fixed(10)]).hit_id(HitId::new(20));
        Widget::render(&table, Rect::new(0, 3, 20, 2), &mut frame);

        // Tree below that
        let tree =
            Tree::new(TreeNode::new("root").child(TreeNode::new("leaf"))).hit_id(HitId::new(30));
        Widget::render(&tree, Rect::new(0, 5, 30, 3), &mut frame);

        // Scrollbar on the right
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight).hit_id(HitId::new(40));
        let mut sb_state = ScrollbarState::new(50, 0, 10);
        StatefulWidget::render(&sb, Rect::new(39, 0, 1, 12), &mut frame, &mut sb_state);

        // Collect all hits
        let mut hits = Vec::new();
        for y in 0..12u16 {
            for x in 0..40u16 {
                hits.push(frame.hit_test(x, y));
            }
        }
        hits
    };

    let hits1 = make_hits();
    let hits2 = make_hits();
    assert_eq!(
        hits1, hits2,
        "cross-widget hit regions must be deterministic"
    );
}

// -----------------------------------------------------------------------
// bd-iuvb.17.3: Horizontal scrollbar hit region encoding
// -----------------------------------------------------------------------

#[test]
fn horizontal_scrollbar_registers_hit_regions() {
    init_tracing();
    info!("horizontal scrollbar registers hit regions with track position data");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(10, 1, &mut pool);
    let area = Rect::new(0, 0, 10, 1);
    let sb = Scrollbar::new(ScrollbarOrientation::HorizontalBottom).hit_id(HitId::new(55));
    let mut state = ScrollbarState::new(100, 0, 10);

    StatefulWidget::render(&sb, area, &mut frame, &mut state);

    // All 10 cells in the row should have hits
    for x in 0..10u16 {
        let (id, region, _data) = frame
            .hit_test(x, 0)
            .unwrap_or_else(|| panic!("expected hit at x={x}"));
        assert_eq!(id, HitId::new(55));
        assert_eq!(region, HitRegion::Scrollbar);
    }

    // Verify thumb and track parts are distinct
    let (_, _, data_thumb) = frame.hit_test(0, 0).unwrap();
    let part_thumb = data_thumb >> 56;
    assert_eq!(
        part_thumb, 1,
        "x=0 should be SCROLLBAR_PART_THUMB (position=0)"
    );

    let (_, _, data_track) = frame.hit_test(5, 0).unwrap();
    let part_track = data_track >> 56;
    assert_eq!(part_track, 0, "x=5 should be SCROLLBAR_PART_TRACK");
}

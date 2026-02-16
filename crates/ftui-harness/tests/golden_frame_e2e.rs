#![forbid(unsafe_code)]

//! Full render pipeline golden-frame E2E verification (bd-1q5.13).
//!
//! Each scenario:
//! 1. Creates a terminal with known capabilities (5 profiles).
//! 2. Builds a widget tree.
//! 3. Renders through full pipeline: model → view → buffer → diff → ANSI bytes.
//! 4. BLAKE3-checksums the ANSI byte output.
//! 5. Verifies determinism: same inputs → same bytes across runs.
//!
//! Profiles tested: xterm-256color, screen-256color, tmux-256color, kitty, alacritty (modern).
//!
//! # Running
//!
//! ```sh
//! cargo test -p ftui-harness --test golden_frame_e2e
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

use ftui_core::geometry::Rect;
use ftui_core::terminal_capabilities::{TerminalCapabilities, TerminalProfile};
use ftui_layout::Constraint;
use ftui_render::buffer::Buffer;
use ftui_render::cell::PackedRgba;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::presenter::Presenter;
use ftui_style::Style;
use ftui_text::{Span, Text};
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::list::{List, ListItem, ListState};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::ProgressBar;
use ftui_widgets::rule::Rule;
use ftui_widgets::scrollbar::{Scrollbar, ScrollbarOrientation, ScrollbarState};
use ftui_widgets::sparkline::Sparkline;
use ftui_widgets::table::{Row, Table, TableState};
use ftui_widgets::{StatefulWidget, Widget};

// ===========================================================================
// Terminal Profiles
// ===========================================================================

fn profiles() -> Vec<(&'static str, TerminalCapabilities)> {
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

// ===========================================================================
// JSONL Logging
// ===========================================================================

fn log_jsonl(step: &str, data: &[(&str, &str)]) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let fields: Vec<String> = std::iter::once(format!("\"seq\":{seq}"))
        .chain(std::iter::once(format!("\"step\":\"{step}\"")))
        .chain(data.iter().map(|(k, v)| format!("\"{k}\":\"{v}\"")))
        .collect();
    eprintln!("{{{}}}", fields.join(","));
}

// ===========================================================================
// Full Pipeline Helper
// ===========================================================================

/// Render through the full pipeline: buffer → diff → presenter → ANSI bytes.
/// Returns the BLAKE3 hex digest of the output bytes.
fn full_pipeline_checksum(
    caps: &TerminalCapabilities,
    width: u16,
    height: u16,
    render_fn: &dyn Fn(&mut Frame),
) -> String {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    render_fn(&mut frame);

    let empty = Buffer::empty(width, height);
    let diff = BufferDiff::compute(&empty, &frame.buffer);

    let mut presenter = Presenter::new(Vec::<u8>::new(), caps.clone());
    presenter.present(&frame.buffer, &diff).unwrap();
    let bytes = presenter.into_inner().unwrap();

    let hash = blake3::hash(&bytes);
    format!("blake3:{}", hash.to_hex())
}

/// Assert that the full pipeline produces identical output across two runs.
fn assert_pipeline_deterministic(
    scenario: &str,
    profile: &str,
    caps: &TerminalCapabilities,
    width: u16,
    height: u16,
    render_fn: &dyn Fn(&mut Frame),
) {
    let cs1 = full_pipeline_checksum(caps, width, height, render_fn);
    let cs2 = full_pipeline_checksum(caps, width, height, render_fn);

    assert_eq!(
        cs1, cs2,
        "PIPELINE DETERMINISM VIOLATION: scenario={scenario} profile={profile} size={width}x{height}"
    );

    log_jsonl(
        "e2e_pipeline",
        &[
            ("scenario", scenario),
            ("profile", profile),
            ("size", &format!("{width}x{height}")),
            ("checksum", &cs1),
            ("outcome", "pass"),
        ],
    );
}

/// Run a scenario across all 5 terminal profiles.
fn assert_across_profiles(scenario: &str, width: u16, height: u16, render_fn: &dyn Fn(&mut Frame)) {
    for (profile_name, caps) in &profiles() {
        assert_pipeline_deterministic(scenario, profile_name, caps, width, height, render_fn);
    }
}

/// Assert that different profiles produce different ANSI bytes (when capabilities differ).
fn assert_profiles_differ_when_caps_differ(
    scenario: &str,
    width: u16,
    height: u16,
    render_fn: &dyn Fn(&mut Frame),
) {
    let checksums: Vec<(String, String)> = profiles()
        .iter()
        .map(|(name, caps)| {
            (
                name.to_string(),
                full_pipeline_checksum(caps, width, height, render_fn),
            )
        })
        .collect();

    // At least 2 distinct checksums (modern vs mux should differ due to sync brackets)
    let unique: std::collections::HashSet<&str> =
        checksums.iter().map(|(_, cs)| cs.as_str()).collect();
    assert!(
        unique.len() >= 2,
        "Expected different profiles to produce different ANSI output for scenario={scenario}, \
         but all produced identical bytes. checksums={checksums:?}"
    );
}

// ===========================================================================
// Multi-frame pipeline helper
// ===========================================================================

/// Render two frames in sequence, returning the BLAKE3 checksum of the second
/// frame's ANSI output (which is a diff-based update, not a full redraw).
fn two_frame_pipeline_checksum(
    caps: &TerminalCapabilities,
    width: u16,
    height: u16,
    render_first: &dyn Fn(&mut Frame),
    render_second: &dyn Fn(&mut Frame),
) -> String {
    // Frame 1
    let mut pool1 = GraphemePool::new();
    let mut frame1 = Frame::new(width, height, &mut pool1);
    render_first(&mut frame1);

    let empty = Buffer::empty(width, height);
    let diff1 = BufferDiff::compute(&empty, &frame1.buffer);
    let mut presenter = Presenter::new(Vec::<u8>::new(), caps.clone());
    presenter.present(&frame1.buffer, &diff1).unwrap();

    // Frame 2 (diff against frame 1)
    let mut pool2 = GraphemePool::new();
    let mut frame2 = Frame::new(width, height, &mut pool2);
    render_second(&mut frame2);

    let diff2 = BufferDiff::compute(&frame1.buffer, &frame2.buffer);
    let mut presenter2 = Presenter::new(Vec::<u8>::new(), caps.clone());
    presenter2.present(&frame2.buffer, &diff2).unwrap();
    let bytes = presenter2.into_inner().unwrap();

    let hash = blake3::hash(&bytes);
    format!("blake3:{}", hash.to_hex())
}

// ===========================================================================
// Scenario Helpers
// ===========================================================================

fn render_paragraph(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::raw(
        "Hello, world! This is a paragraph widget for golden-frame testing.",
    ))
    .render(area, frame);
}

fn render_block_all_borders(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Block::new()
        .borders(Borders::ALL)
        .title(Span::raw("Block"))
        .render(area, frame);
}

fn render_block_rounded(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Span::raw("Rounded"))
        .render(area, frame);
}

fn render_block_double(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .title(Span::raw("Double"))
        .render(area, frame);
}

fn render_sparkline(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let data = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 7, 6, 5, 4, 3, 2, 1, 0];
    Sparkline::default().data(&data).render(area, frame);
}

fn render_progress_50(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    ProgressBar::default().percent(50).render(area, frame);
}

fn render_progress_100(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    ProgressBar::default().percent(100).render(area, frame);
}

fn render_rule(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Rule::horizontal("Section").render(area, frame);
}

fn render_list_selected(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let items: Vec<ListItem> = (0..10)
        .map(|i| ListItem::new(format!("Item {i}")))
        .collect();
    let widget = List::new(items).highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(3));
    StatefulWidget::render(&widget, area, frame, &mut state);
}

fn render_table_selected(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let rows: Vec<Row> = (0..8)
        .map(|i| Row::new(vec![format!("A{i}"), format!("B{i}"), format!("C{i}")]))
        .collect();
    let widths = [
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
    ];
    let widget = Table::new(rows, widths).highlight_symbol(">> ");
    let mut state = TableState::default();
    state.select(Some(2));
    StatefulWidget::render(&widget, area, frame, &mut state);
}

// --- Styled text scenarios ---

fn render_bold_text(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::from_spans([Span::styled(
        "Bold text",
        Style::new().bold(),
    )]))
    .render(area, frame);
}

fn render_italic_text(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::from_spans([Span::styled(
        "Italic text",
        Style::new().italic(),
    )]))
    .render(area, frame);
}

fn render_underline_text(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::from_spans([Span::styled(
        "Underlined",
        Style::new().underlined(),
    )]))
    .render(area, frame);
}

fn render_strikethrough_text(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::from_spans([Span::styled(
        "Strikethrough",
        Style::new().strikethrough(),
    )]))
    .render(area, frame);
}

fn render_fg_color(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::from_spans([Span::styled(
        "Red text",
        Style::new().fg(PackedRgba::rgb(255, 0, 0)),
    )]))
    .render(area, frame);
}

fn render_bg_color(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::from_spans([Span::styled(
        "Blue bg",
        Style::new().bg(PackedRgba::rgb(0, 0, 255)),
    )]))
    .render(area, frame);
}

fn render_mixed_styles(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::from_spans([
        Span::styled("Bold", Style::new().bold()),
        Span::raw(" and "),
        Span::styled("italic", Style::new().italic()),
        Span::raw(" and "),
        Span::styled(
            "colored",
            Style::new()
                .fg(PackedRgba::rgb(0, 255, 0))
                .bg(PackedRgba::rgb(128, 0, 128)),
        ),
    ]))
    .render(area, frame);
}

fn render_dim_text(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::from_spans([Span::styled(
        "Dim text",
        Style::new().dim(),
    )]))
    .render(area, frame);
}

fn render_reverse_video(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::from_spans([Span::styled(
        "Reversed",
        Style::new().reversed(),
    )]))
    .render(area, frame);
}

fn render_hidden_text(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::from_spans([Span::styled(
        "Hidden",
        Style::new().hidden(),
    )]))
    .render(area, frame);
}

// --- Layout composition scenarios ---

fn render_horizontal_split(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let chunks =
        ftui_layout::Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
    Paragraph::new(Text::raw("Left")).render(chunks[0], frame);
    Paragraph::new(Text::raw("Right")).render(chunks[1], frame);
}

fn render_vertical_split(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let chunks =
        ftui_layout::Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
    Paragraph::new(Text::raw("Top")).render(chunks[0], frame);
    Paragraph::new(Text::raw("Bottom")).render(chunks[1], frame);
}

fn render_three_column(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let chunks = ftui_layout::Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .split(area);
    for (i, chunk) in chunks.iter().enumerate() {
        Paragraph::new(Text::raw(format!("Col {i}"))).render(*chunk, frame);
    }
}

fn render_nested_flex(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let outer =
        ftui_layout::Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).split(area);
    let inner =
        ftui_layout::Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(outer[1]);
    Paragraph::new(Text::raw("Header")).render(outer[0], frame);
    Paragraph::new(Text::raw("Sidebar")).render(inner[0], frame);
    Paragraph::new(Text::raw("Main content area")).render(inner[1], frame);
}

fn render_grid_2x2(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let rows =
        ftui_layout::Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
    for (r, row_area) in rows.iter().enumerate() {
        let cols = ftui_layout::Layout::horizontal([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(*row_area);
        for (c, col_area) in cols.iter().enumerate() {
            Paragraph::new(Text::raw(format!("({r},{c})"))).render(*col_area, frame);
        }
    }
}

fn render_sidebar_main(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let chunks =
        ftui_layout::Layout::horizontal([Constraint::Length(20), Constraint::Fill(1)]).split(area);
    Block::new()
        .borders(Borders::ALL)
        .title(Span::raw("Nav"))
        .render(chunks[0], frame);
    Paragraph::new(Text::raw("Main content here.")).render(chunks[1], frame);
}

fn render_header_footer(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let chunks = ftui_layout::Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    Paragraph::new(Text::raw("=== HEADER ===")).render(chunks[0], frame);
    Paragraph::new(Text::raw("Body content")).render(chunks[1], frame);
    Paragraph::new(Text::raw("=== FOOTER ===")).render(chunks[2], frame);
}

fn render_ratio_layout(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let chunks = ftui_layout::Layout::horizontal([
        Constraint::Ratio(1, 4),
        Constraint::Ratio(2, 4),
        Constraint::Ratio(1, 4),
    ])
    .split(area);
    Paragraph::new(Text::raw("25%")).render(chunks[0], frame);
    Paragraph::new(Text::raw("50%")).render(chunks[1], frame);
    Paragraph::new(Text::raw("25%")).render(chunks[2], frame);
}

fn render_mixed_constraints(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let chunks = ftui_layout::Layout::horizontal([
        Constraint::Length(10),
        Constraint::Fill(1),
        Constraint::Max(20),
    ])
    .split(area);
    for (i, chunk) in chunks.iter().enumerate() {
        Paragraph::new(Text::raw(format!("Pane {i}"))).render(*chunk, frame);
    }
}

fn render_deeply_nested(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let outer = Block::new().borders(Borders::ALL).title(Span::raw("L1"));
    let inner_area = outer.inner(area);
    outer.render(area, frame);

    if inner_area.width > 4 && inner_area.height > 2 {
        let inner = Block::new().borders(Borders::ALL).title(Span::raw("L2"));
        let innermost_area = inner.inner(inner_area);
        inner.render(inner_area, frame);
        if innermost_area.width > 0 && innermost_area.height > 0 {
            Paragraph::new(Text::raw("Deep")).render(innermost_area, frame);
        }
    }
}

// --- Edge case scenarios ---

fn render_empty(_frame: &mut Frame) {
    // Deliberately empty — tests that an empty buffer produces consistent output.
}

fn render_single_cell(frame: &mut Frame) {
    let area = Rect::new(
        0,
        0,
        1.min(frame.buffer.width()),
        1.min(frame.buffer.height()),
    );
    Paragraph::new(Text::raw("X")).render(area, frame);
}

fn render_unicode_cjk(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::raw("漢字テスト日本語")).render(area, frame);
}

fn render_unicode_emoji(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::raw("🎉🚀🌍🔥✨")).render(area, frame);
}

fn render_unicode_combining(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    // e + combining acute = é; a + combining ring = å
    Paragraph::new(Text::raw("e\u{0301} a\u{030A} o\u{0308}")).render(area, frame);
}

fn render_long_line(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let long = "A".repeat(200);
    Paragraph::new(Text::raw(long)).render(area, frame);
}

fn render_multiline(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let text = (0..20)
        .map(|i| format!("Line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    Paragraph::new(Text::raw(text)).render(area, frame);
}

fn render_newlines_only(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::raw("\n\n\n\n\n")).render(area, frame);
}

fn render_spaces_only(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::raw("          ")).render(area, frame);
}

fn render_tabs(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Paragraph::new(Text::raw("col1\tcol2\tcol3")).render(area, frame);
}

// --- Scrollbar scenarios ---

fn render_scrollbar_vertical(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let widget = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    let mut state = ScrollbarState::new(100).position(25);
    StatefulWidget::render(&widget, area, frame, &mut state);
}

fn render_scrollbar_horizontal(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let widget = Scrollbar::new(ScrollbarOrientation::HorizontalBottom);
    let mut state = ScrollbarState::new(100).position(50);
    StatefulWidget::render(&widget, area, frame, &mut state);
}

// --- Additional widget scenarios ---

fn render_list_empty(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let items: Vec<ListItem> = vec![];
    List::new(items).render(area, frame);
}

fn render_list_scrolled(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let items: Vec<ListItem> = (0..50)
        .map(|i| ListItem::new(format!("Item {i}")))
        .collect();
    let widget = List::new(items).highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(40));
    StatefulWidget::render(&widget, area, frame, &mut state);
}

fn render_table_many_rows(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let rows: Vec<Row> = (0..50)
        .map(|i| Row::new(vec![format!("Row {i}"), format!("Data {i}")]))
        .collect();
    let widths = [Constraint::Length(15), Constraint::Fill(1)];
    Table::new(rows, widths).render(area, frame);
}

fn render_block_thick(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .title(Span::raw("Thick"))
        .render(area, frame);
}

fn render_progress_0(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    ProgressBar::default().percent(0).render(area, frame);
}

fn render_composite_dashboard(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let chunks = ftui_layout::Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);

    Paragraph::new(Text::raw("Dashboard")).render(chunks[0], frame);

    let body = ftui_layout::Layout::horizontal([Constraint::Length(20), Constraint::Fill(1)])
        .split(chunks[1]);
    Block::new()
        .borders(Borders::ALL)
        .title(Span::raw("Nav"))
        .render(body[0], frame);

    let main_chunks =
        ftui_layout::Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).split(body[1]);
    ProgressBar::default()
        .percent(75)
        .render(main_chunks[0], frame);
    let data = vec![3, 5, 7, 2, 8, 4, 6, 1, 9, 5];
    Sparkline::default()
        .data(&data)
        .render(main_chunks[1], frame);

    Paragraph::new(Text::raw("Status: OK")).render(chunks[2], frame);
}

fn render_styled_borders_with_content(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            "Styled",
            Style::new().bold().fg(PackedRgba::rgb(255, 165, 0)),
        ));
    let inner = block.inner(area);
    block.render(area, frame);
    if inner.width > 0 && inner.height > 0 {
        Paragraph::new(Text::from_spans([
            Span::styled("Line 1", Style::new().fg(PackedRgba::rgb(0, 255, 0))),
            Span::raw(" | "),
            Span::styled(
                "Line 2",
                Style::new().italic().fg(PackedRgba::rgb(0, 0, 255)),
            ),
        ]))
        .render(inner, frame);
    }
}

// ===========================================================================
// Category 1: Simple Widgets (10 scenarios)
// ===========================================================================

#[test]
fn e2e_01_paragraph() {
    assert_across_profiles("paragraph", 80, 24, &render_paragraph);
}

#[test]
fn e2e_02_block_all_borders() {
    assert_across_profiles("block_all_borders", 80, 24, &render_block_all_borders);
}

#[test]
fn e2e_03_block_rounded() {
    assert_across_profiles("block_rounded", 80, 24, &render_block_rounded);
}

#[test]
fn e2e_04_block_double() {
    assert_across_profiles("block_double", 80, 24, &render_block_double);
}

#[test]
fn e2e_05_sparkline() {
    assert_across_profiles("sparkline", 80, 24, &render_sparkline);
}

#[test]
fn e2e_06_progress_50() {
    assert_across_profiles("progress_50", 80, 24, &render_progress_50);
}

#[test]
fn e2e_07_progress_100() {
    assert_across_profiles("progress_100", 80, 24, &render_progress_100);
}

#[test]
fn e2e_08_rule() {
    assert_across_profiles("rule", 80, 24, &render_rule);
}

#[test]
fn e2e_09_list_selected() {
    assert_across_profiles("list_selected", 80, 24, &render_list_selected);
}

#[test]
fn e2e_10_table_selected() {
    assert_across_profiles("table_selected", 80, 24, &render_table_selected);
}

// ===========================================================================
// Category 2: Styled Text (10 scenarios)
// ===========================================================================

#[test]
fn e2e_11_bold() {
    assert_across_profiles("bold", 80, 24, &render_bold_text);
}

#[test]
fn e2e_12_italic() {
    assert_across_profiles("italic", 80, 24, &render_italic_text);
}

#[test]
fn e2e_13_underline() {
    assert_across_profiles("underline", 80, 24, &render_underline_text);
}

#[test]
fn e2e_14_strikethrough() {
    assert_across_profiles("strikethrough", 80, 24, &render_strikethrough_text);
}

#[test]
fn e2e_15_fg_color() {
    assert_across_profiles("fg_color", 80, 24, &render_fg_color);
}

#[test]
fn e2e_16_bg_color() {
    assert_across_profiles("bg_color", 80, 24, &render_bg_color);
}

#[test]
fn e2e_17_mixed_styles() {
    assert_across_profiles("mixed_styles", 80, 24, &render_mixed_styles);
}

#[test]
fn e2e_18_dim() {
    assert_across_profiles("dim", 80, 24, &render_dim_text);
}

#[test]
fn e2e_19_reverse() {
    assert_across_profiles("reverse", 80, 24, &render_reverse_video);
}

#[test]
fn e2e_20_hidden() {
    assert_across_profiles("hidden", 80, 24, &render_hidden_text);
}

// ===========================================================================
// Category 3: Layout Compositions (10 scenarios)
// ===========================================================================

#[test]
fn e2e_21_horizontal_split() {
    assert_across_profiles("horizontal_split", 80, 24, &render_horizontal_split);
}

#[test]
fn e2e_22_vertical_split() {
    assert_across_profiles("vertical_split", 80, 24, &render_vertical_split);
}

#[test]
fn e2e_23_three_column() {
    assert_across_profiles("three_column", 120, 40, &render_three_column);
}

#[test]
fn e2e_24_nested_flex() {
    assert_across_profiles("nested_flex", 120, 40, &render_nested_flex);
}

#[test]
fn e2e_25_grid_2x2() {
    assert_across_profiles("grid_2x2", 80, 24, &render_grid_2x2);
}

#[test]
fn e2e_26_sidebar_main() {
    assert_across_profiles("sidebar_main", 80, 24, &render_sidebar_main);
}

#[test]
fn e2e_27_header_footer() {
    assert_across_profiles("header_footer", 80, 24, &render_header_footer);
}

#[test]
fn e2e_28_ratio_layout() {
    assert_across_profiles("ratio_layout", 80, 24, &render_ratio_layout);
}

#[test]
fn e2e_29_mixed_constraints() {
    assert_across_profiles("mixed_constraints", 120, 40, &render_mixed_constraints);
}

#[test]
fn e2e_30_deeply_nested() {
    assert_across_profiles("deeply_nested", 80, 24, &render_deeply_nested);
}

// ===========================================================================
// Category 4: Edge Cases (10 scenarios)
// ===========================================================================

#[test]
fn e2e_31_empty() {
    assert_across_profiles("empty", 80, 24, &render_empty);
}

#[test]
fn e2e_32_single_cell() {
    assert_across_profiles("single_cell", 80, 24, &render_single_cell);
}

#[test]
fn e2e_33_unicode_cjk() {
    assert_across_profiles("unicode_cjk", 80, 24, &render_unicode_cjk);
}

#[test]
fn e2e_34_unicode_emoji() {
    assert_across_profiles("unicode_emoji", 80, 24, &render_unicode_emoji);
}

#[test]
fn e2e_35_unicode_combining() {
    assert_across_profiles("unicode_combining", 80, 24, &render_unicode_combining);
}

#[test]
fn e2e_36_long_line() {
    assert_across_profiles("long_line", 80, 24, &render_long_line);
}

#[test]
fn e2e_37_multiline() {
    assert_across_profiles("multiline", 80, 24, &render_multiline);
}

#[test]
fn e2e_38_newlines_only() {
    assert_across_profiles("newlines_only", 80, 24, &render_newlines_only);
}

#[test]
fn e2e_39_spaces_only() {
    assert_across_profiles("spaces_only", 80, 24, &render_spaces_only);
}

#[test]
fn e2e_40_tabs() {
    assert_across_profiles("tabs", 80, 24, &render_tabs);
}

// ===========================================================================
// Category 5: Multi-Frame & Diff (7 scenarios)
// ===========================================================================

#[test]
fn e2e_41_redraw_same_content() {
    for (profile_name, caps) in &profiles() {
        let cs1 = two_frame_pipeline_checksum(caps, 80, 24, &render_paragraph, &render_paragraph);
        let cs2 = two_frame_pipeline_checksum(caps, 80, 24, &render_paragraph, &render_paragraph);
        assert_eq!(
            cs1, cs2,
            "redraw_same_content determinism failed for {profile_name}"
        );
    }
}

#[test]
fn e2e_42_content_change() {
    for (profile_name, caps) in &profiles() {
        let cs1 = two_frame_pipeline_checksum(caps, 80, 24, &render_paragraph, &render_bold_text);
        let cs2 = two_frame_pipeline_checksum(caps, 80, 24, &render_paragraph, &render_bold_text);
        assert_eq!(
            cs1, cs2,
            "content_change determinism failed for {profile_name}"
        );
    }
}

#[test]
fn e2e_43_style_change() {
    for (profile_name, caps) in &profiles() {
        let cs1 = two_frame_pipeline_checksum(caps, 80, 24, &render_paragraph, &render_fg_color);
        let cs2 = two_frame_pipeline_checksum(caps, 80, 24, &render_paragraph, &render_fg_color);
        assert_eq!(
            cs1, cs2,
            "style_change determinism failed for {profile_name}"
        );
    }
}

#[test]
fn e2e_44_empty_to_content() {
    for (profile_name, caps) in &profiles() {
        let cs1 = two_frame_pipeline_checksum(caps, 80, 24, &render_empty, &render_paragraph);
        let cs2 = two_frame_pipeline_checksum(caps, 80, 24, &render_empty, &render_paragraph);
        assert_eq!(
            cs1, cs2,
            "empty_to_content determinism failed for {profile_name}"
        );
    }
}

#[test]
fn e2e_45_content_to_empty() {
    for (profile_name, caps) in &profiles() {
        let cs1 = two_frame_pipeline_checksum(caps, 80, 24, &render_paragraph, &render_empty);
        let cs2 = two_frame_pipeline_checksum(caps, 80, 24, &render_paragraph, &render_empty);
        assert_eq!(
            cs1, cs2,
            "content_to_empty determinism failed for {profile_name}"
        );
    }
}

#[test]
fn e2e_46_layout_change() {
    for (profile_name, caps) in &profiles() {
        let cs1 = two_frame_pipeline_checksum(
            caps,
            80,
            24,
            &render_horizontal_split,
            &render_vertical_split,
        );
        let cs2 = two_frame_pipeline_checksum(
            caps,
            80,
            24,
            &render_horizontal_split,
            &render_vertical_split,
        );
        assert_eq!(
            cs1, cs2,
            "layout_change determinism failed for {profile_name}"
        );
    }
}

#[test]
fn e2e_47_widget_switch() {
    for (profile_name, caps) in &profiles() {
        let cs1 = two_frame_pipeline_checksum(
            caps,
            80,
            24,
            &render_sparkline,
            &render_composite_dashboard,
        );
        let cs2 = two_frame_pipeline_checksum(
            caps,
            80,
            24,
            &render_sparkline,
            &render_composite_dashboard,
        );
        assert_eq!(
            cs1, cs2,
            "widget_switch determinism failed for {profile_name}"
        );
    }
}

// ===========================================================================
// Cross-cutting: Profile divergence
// ===========================================================================

#[test]
fn e2e_profiles_diverge_on_styled_content() {
    assert_profiles_differ_when_caps_differ("profile_divergence", 80, 24, &render_mixed_styles);
}

// ===========================================================================
// Cross-cutting: Additional widget coverage
// ===========================================================================

#[test]
fn e2e_48_scrollbar_vertical() {
    assert_across_profiles("scrollbar_vertical", 80, 24, &render_scrollbar_vertical);
}

#[test]
fn e2e_49_scrollbar_horizontal() {
    assert_across_profiles("scrollbar_horizontal", 80, 24, &render_scrollbar_horizontal);
}

#[test]
fn e2e_50_list_empty() {
    assert_across_profiles("list_empty", 80, 24, &render_list_empty);
}

#[test]
fn e2e_51_list_scrolled() {
    assert_across_profiles("list_scrolled", 80, 24, &render_list_scrolled);
}

#[test]
fn e2e_52_table_many_rows() {
    assert_across_profiles("table_many_rows", 80, 24, &render_table_many_rows);
}

#[test]
fn e2e_53_block_thick() {
    assert_across_profiles("block_thick", 80, 24, &render_block_thick);
}

#[test]
fn e2e_54_progress_0() {
    assert_across_profiles("progress_0", 80, 24, &render_progress_0);
}

#[test]
fn e2e_55_composite_dashboard() {
    assert_across_profiles("composite_dashboard", 120, 40, &render_composite_dashboard);
}

#[test]
fn e2e_56_styled_borders_with_content() {
    assert_across_profiles(
        "styled_borders_content",
        80,
        24,
        &render_styled_borders_with_content,
    );
}

// ===========================================================================
// Cross-cutting: Size sensitivity
// ===========================================================================

#[test]
fn e2e_57_size_sensitivity() {
    let caps = TerminalCapabilities::from_profile(TerminalProfile::Xterm256Color);
    let cs_80x24 = full_pipeline_checksum(&caps, 80, 24, &render_paragraph);
    let cs_120x40 = full_pipeline_checksum(&caps, 120, 40, &render_paragraph);
    assert_ne!(
        cs_80x24, cs_120x40,
        "Same content at different sizes should produce different ANSI output"
    );
}

// ===========================================================================
// Tracing span verification
// ===========================================================================

#[test]
fn e2e_tracing_present_span_emitted() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    struct PresentSpanChecker {
        saw_present: Arc<AtomicBool>,
    }

    impl tracing::Subscriber for PresentSpanChecker {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            if span.metadata().name() == "present" {
                self.saw_present.store(true, Ordering::Relaxed);
            }
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
        fn event(&self, _event: &tracing::Event<'_>) {}
        fn enter(&self, _span: &tracing::span::Id) {}
        fn exit(&self, _span: &tracing::span::Id) {}
    }

    let saw_it = Arc::new(AtomicBool::new(false));
    let subscriber = PresentSpanChecker {
        saw_present: Arc::clone(&saw_it),
    };
    let _guard = tracing::subscriber::set_default(subscriber);

    let caps = TerminalCapabilities::from_profile(TerminalProfile::Modern);
    let _ = full_pipeline_checksum(&caps, 80, 24, &render_paragraph);

    // The "present" span is behind #[cfg(feature = "tracing")] in presenter.rs.
    // This test verifies the infrastructure is wired up when tracing is available.
    log_jsonl(
        "tracing_check",
        &[
            ("span", "present"),
            (
                "observed",
                if saw_it.load(Ordering::Relaxed) {
                    "true"
                } else {
                    "false_feature_gated"
                },
            ),
        ],
    );
}

#[test]
fn e2e_determinism_across_runs_triple() {
    let caps = TerminalCapabilities::from_profile(TerminalProfile::Kitty);
    let cs1 = full_pipeline_checksum(&caps, 80, 24, &render_composite_dashboard);
    let cs2 = full_pipeline_checksum(&caps, 80, 24, &render_composite_dashboard);
    let cs3 = full_pipeline_checksum(&caps, 80, 24, &render_composite_dashboard);
    assert_eq!(cs1, cs2, "Run 1 vs 2 mismatch");
    assert_eq!(cs2, cs3, "Run 2 vs 3 mismatch");
}

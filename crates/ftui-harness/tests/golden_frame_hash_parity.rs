#![forbid(unsafe_code)]

//! Deterministic frame hash parity across 100+ scenarios (bd-1pys5.6).
//!
//! This test suite verifies that widget rendering is deterministic: given the
//! same widget tree and terminal size, the same frame buffer (and BLAKE3
//! checksum) is produced every time, regardless of timing or system load.
//!
//! # Methodology
//!
//! Each scenario:
//! 1. Defines a widget tree + terminal size.
//! 2. Renders twice into independent frames.
//! 3. Computes BLAKE3 checksums and asserts equality.
//!
//! # Scenario Categories
//!
//! | Category | Count | Description |
//! |----------|-------|-------------|
//! | Simple widgets | 20 | Paragraph, list, block, sparkline, progress, badge, rule |
//! | Complex layouts | 20 | Flex, grid, nested, multi-widget compositions |
//! | Interaction sequences | 30 | Stateful widgets with selection, scroll, hover |
//! | Edge cases | 20 | Empty, oversized, Unicode, zero-size, single-cell |
//! | Regression scenarios | 12 | Previously found determinism edge cases |
//!
//! # Running
//!
//! ```sh
//! cargo test -p ftui-harness --test golden_frame_hash_parity
//! ```

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use ftui_core::geometry::Rect;
use ftui_harness::golden::compute_buffer_checksum;
use ftui_layout::{Constraint, Flex, Grid};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
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
// Determinism Assertion Helpers
// ===========================================================================

/// Render a scene twice and assert identical BLAKE3 checksums.
fn assert_deterministic<F>(name: &str, width: u16, height: u16, render_fn: F)
where
    F: Fn(&mut Frame),
{
    let mut pool1 = GraphemePool::new();
    let mut frame1 = Frame::new(width, height, &mut pool1);
    render_fn(&mut frame1);
    let cs1 = compute_buffer_checksum(&frame1.buffer);

    let mut pool2 = GraphemePool::new();
    let mut frame2 = Frame::new(width, height, &mut pool2);
    render_fn(&mut frame2);
    let cs2 = compute_buffer_checksum(&frame2.buffer);

    assert_eq!(
        cs1, cs2,
        "DETERMINISM VIOLATION in scenario '{name}' ({width}x{height}): {cs1} != {cs2}"
    );

    log_jsonl(
        "parity",
        &[
            ("scenario", name),
            ("size", &format!("{width}x{height}")),
            ("checksum", &cs1),
            ("outcome", "pass"),
        ],
    );
}

/// Render a stateful scene twice and assert identical BLAKE3 checksums.
/// The state_fn is called fresh each time to ensure independent state.
fn assert_deterministic_stateful<S, F, G>(
    name: &str,
    width: u16,
    height: u16,
    state_fn: G,
    render_fn: F,
) where
    S: Clone,
    F: Fn(&mut Frame, &mut S),
    G: Fn() -> S,
{
    let mut pool1 = GraphemePool::new();
    let mut frame1 = Frame::new(width, height, &mut pool1);
    let mut state1 = state_fn();
    render_fn(&mut frame1, &mut state1);
    let cs1 = compute_buffer_checksum(&frame1.buffer);

    let mut pool2 = GraphemePool::new();
    let mut frame2 = Frame::new(width, height, &mut pool2);
    let mut state2 = state_fn();
    render_fn(&mut frame2, &mut state2);
    let cs2 = compute_buffer_checksum(&frame2.buffer);

    assert_eq!(
        cs1, cs2,
        "DETERMINISM VIOLATION (stateful) in scenario '{name}' ({width}x{height}): {cs1} != {cs2}"
    );

    log_jsonl(
        "parity",
        &[
            ("scenario", name),
            ("size", &format!("{width}x{height}")),
            ("checksum", &cs1),
            ("outcome", "pass"),
        ],
    );
}

/// Compute a full (style+content) checksum covering fg, bg, attrs.
fn compute_full_checksum(buf: &Buffer) -> String {
    let mut hasher = DefaultHasher::new();
    buf.width().hash(&mut hasher);
    buf.height().hash(&mut hasher);
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            if let Some(cell) = buf.get(x, y) {
                cell.content.hash(&mut hasher);
                cell.fg.hash(&mut hasher);
                cell.bg.hash(&mut hasher);
                cell.attrs.hash(&mut hasher);
            }
        }
    }
    format!("full:{:016x}", hasher.finish())
}

/// Assert both content and full (style) checksums are deterministic.
fn assert_full_deterministic<F>(name: &str, width: u16, height: u16, render_fn: F)
where
    F: Fn(&mut Frame),
{
    let mut pool1 = GraphemePool::new();
    let mut frame1 = Frame::new(width, height, &mut pool1);
    render_fn(&mut frame1);
    let content1 = compute_buffer_checksum(&frame1.buffer);
    let full1 = compute_full_checksum(&frame1.buffer);

    let mut pool2 = GraphemePool::new();
    let mut frame2 = Frame::new(width, height, &mut pool2);
    render_fn(&mut frame2);
    let content2 = compute_buffer_checksum(&frame2.buffer);
    let full2 = compute_full_checksum(&frame2.buffer);

    assert_eq!(
        content1, content2,
        "Content checksum mismatch in '{name}' ({width}x{height})"
    );
    assert_eq!(
        full1, full2,
        "Full (style) checksum mismatch in '{name}' ({width}x{height})"
    );

    log_jsonl(
        "parity_full",
        &[
            ("scenario", name),
            ("size", &format!("{width}x{height}")),
            ("content_cs", &content1),
            ("full_cs", &full1),
            ("outcome", "pass"),
        ],
    );
}

// ===========================================================================
// Category 1: Simple Widgets (20 scenarios)
// ===========================================================================

#[test]
fn simple_01_paragraph_plain() {
    assert_deterministic("simple_01_paragraph_plain", 80, 24, |frame| {
        let para = Paragraph::new(Text::raw("Hello, World!"));
        para.render(Rect::new(0, 0, 80, 24), frame);
    });
}

#[test]
fn simple_02_paragraph_multiline() {
    assert_deterministic("simple_02_paragraph_multiline", 80, 24, |frame| {
        let text = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5";
        let para = Paragraph::new(Text::raw(text));
        para.render(Rect::new(0, 0, 80, 24), frame);
    });
}

#[test]
fn simple_03_paragraph_styled() {
    assert_full_deterministic("simple_03_paragraph_styled", 80, 24, |frame| {
        let text = Text::from_spans([
            Span::styled("Bold ", Style::new().bold()),
            Span::styled("Italic ", Style::new().italic()),
            Span::raw("Normal"),
        ]);
        let para = Paragraph::new(text);
        para.render(Rect::new(0, 0, 80, 24), frame);
    });
}

#[test]
fn simple_04_block_all_borders() {
    assert_deterministic("simple_04_block_all_borders", 80, 24, |frame| {
        let block = Block::default().borders(Borders::ALL).title("Test Block");
        block.render(Rect::new(0, 0, 80, 24), frame);
    });
}

#[test]
fn simple_05_block_rounded_borders() {
    assert_deterministic("simple_05_block_rounded_borders", 60, 20, |frame| {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Rounded");
        block.render(Rect::new(0, 0, 60, 20), frame);
    });
}

#[test]
fn simple_06_block_double_borders() {
    assert_deterministic("simple_06_block_double_borders", 40, 12, |frame| {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .title("Double");
        block.render(Rect::new(0, 0, 40, 12), frame);
    });
}

#[test]
fn simple_07_paragraph_in_block() {
    assert_deterministic("simple_07_paragraph_in_block", 80, 24, |frame| {
        let block = Block::default().borders(Borders::ALL).title("Content");
        let inner = block.inner(Rect::new(0, 0, 80, 24));
        block.render(Rect::new(0, 0, 80, 24), frame);
        let para = Paragraph::new(Text::raw("Inside the block"));
        para.render(inner, frame);
    });
}

#[test]
fn simple_08_sparkline_basic() {
    assert_deterministic("simple_08_sparkline_basic", 40, 5, |frame| {
        let data = [1.0, 4.0, 2.0, 8.0, 3.0, 6.0, 5.0, 7.0];
        let sparkline = Sparkline::new(&data);
        sparkline.render(Rect::new(0, 0, 40, 5), frame);
    });
}

#[test]
fn simple_09_sparkline_gradient() {
    assert_full_deterministic("simple_09_sparkline_gradient", 60, 4, |frame| {
        let data = [0.0, 2.0, 5.0, 3.0, 8.0, 1.0, 9.0, 4.0, 6.0, 7.0];
        let sparkline = Sparkline::new(&data)
            .gradient(PackedRgba::rgb(255, 0, 0), PackedRgba::rgb(0, 255, 0));
        sparkline.render(Rect::new(0, 0, 60, 4), frame);
    });
}

#[test]
fn simple_10_progress_bar_zero() {
    assert_deterministic("simple_10_progress_bar_zero", 50, 3, |frame| {
        let progress = ProgressBar::new().ratio(0.0);
        progress.render(Rect::new(0, 0, 50, 3), frame);
    });
}

#[test]
fn simple_11_progress_bar_half() {
    assert_deterministic("simple_11_progress_bar_half", 50, 3, |frame| {
        let progress = ProgressBar::new().ratio(0.5).label("50%");
        progress.render(Rect::new(0, 0, 50, 3), frame);
    });
}

#[test]
fn simple_12_progress_bar_full() {
    assert_deterministic("simple_12_progress_bar_full", 50, 3, |frame| {
        let progress = ProgressBar::new().ratio(1.0).label("100%");
        progress.render(Rect::new(0, 0, 50, 3), frame);
    });
}

#[test]
fn simple_13_rule_horizontal() {
    assert_deterministic("simple_13_rule_horizontal", 80, 1, |frame| {
        let rule = Rule::new().title("Section");
        rule.render(Rect::new(0, 0, 80, 1), frame);
    });
}

#[test]
fn simple_14_block_partial_borders() {
    assert_deterministic("simple_14_block_partial_borders", 40, 10, |frame| {
        let block = Block::default().borders(Borders::TOP | Borders::BOTTOM);
        block.render(Rect::new(0, 0, 40, 10), frame);
    });
}

#[test]
fn simple_15_paragraph_colored() {
    assert_full_deterministic("simple_15_paragraph_colored", 80, 24, |frame| {
        let text = Text::from_spans([
            Span::styled("Red ", Style::new().fg(PackedRgba::rgb(255, 0, 0))),
            Span::styled("Green ", Style::new().fg(PackedRgba::rgb(0, 255, 0))),
            Span::styled("Blue", Style::new().fg(PackedRgba::rgb(0, 0, 255))),
        ]);
        let para = Paragraph::new(text);
        para.render(Rect::new(0, 0, 80, 24), frame);
    });
}

#[test]
fn simple_16_paragraph_long_text() {
    assert_deterministic("simple_16_paragraph_long_text", 80, 24, |frame| {
        let text = "A".repeat(500) + "\n" + &"B".repeat(500);
        let para = Paragraph::new(Text::raw(&text));
        para.render(Rect::new(0, 0, 80, 24), frame);
    });
}

#[test]
fn simple_17_block_styled() {
    assert_full_deterministic("simple_17_block_styled", 60, 15, |frame| {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Styled")
            .border_style(Style::new().fg(PackedRgba::rgb(0, 255, 255)))
            .style(Style::new().bg(PackedRgba::rgb(20, 20, 40)));
        block.render(Rect::new(0, 0, 60, 15), frame);
    });
}

#[test]
fn simple_18_multiple_paragraphs() {
    assert_deterministic("simple_18_multiple_paragraphs", 80, 24, |frame| {
        let area = Rect::new(0, 0, 80, 24);
        for i in 0..4 {
            let y = i * 6;
            let para = Paragraph::new(Text::raw(format!("Paragraph {}", i + 1)));
            para.render(Rect::new(area.x, area.y + y, area.width, 6), frame);
        }
    });
}

#[test]
fn simple_19_progress_bar_labeled() {
    assert_deterministic("simple_19_progress_bar_labeled", 60, 3, |frame| {
        let progress = ProgressBar::new()
            .ratio(0.73)
            .label("Loading...")
            .block(Block::default().borders(Borders::ALL));
        progress.render(Rect::new(0, 0, 60, 3), frame);
    });
}

#[test]
fn simple_20_block_thick_borders() {
    assert_deterministic("simple_20_block_thick_borders", 40, 10, |frame| {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .title("Thick");
        block.render(Rect::new(0, 0, 40, 10), frame);
    });
}

// ===========================================================================
// Category 2: Complex Layouts (20 scenarios)
// ===========================================================================

#[test]
fn layout_01_horizontal_split() {
    assert_deterministic("layout_01_horizontal_split", 80, 24, |frame| {
        let flex = Flex::horizontal().constraints(vec![
            Constraint::Percentage(50.0),
            Constraint::Percentage(50.0),
        ]);
        let areas = flex.split(Rect::new(0, 0, 80, 24));
        Paragraph::new(Text::raw("Left")).render(areas[0], frame);
        Paragraph::new(Text::raw("Right")).render(areas[1], frame);
    });
}

#[test]
fn layout_02_vertical_split() {
    assert_deterministic("layout_02_vertical_split", 80, 24, |frame| {
        let flex = Flex::vertical().constraints(vec![
            Constraint::Fixed(3),
            Constraint::Min(0),
            Constraint::Fixed(1),
        ]);
        let areas = flex.split(Rect::new(0, 0, 80, 24));
        Block::default()
            .borders(Borders::ALL)
            .title("Header")
            .render(areas[0], frame);
        Paragraph::new(Text::raw("Content area")).render(areas[1], frame);
        Paragraph::new(Text::raw("Status: OK")).render(areas[2], frame);
    });
}

#[test]
fn layout_03_three_column() {
    assert_deterministic("layout_03_three_column", 120, 40, |frame| {
        let flex = Flex::horizontal().constraints(vec![
            Constraint::Fixed(20),
            Constraint::Min(40),
            Constraint::Fixed(30),
        ]);
        let areas = flex.split(Rect::new(0, 0, 120, 40));
        Block::default()
            .borders(Borders::ALL)
            .title("Sidebar")
            .render(areas[0], frame);
        Paragraph::new(Text::raw("Main content goes here")).render(areas[1], frame);
        Block::default()
            .borders(Borders::ALL)
            .title("Info")
            .render(areas[2], frame);
    });
}

#[test]
fn layout_04_nested_flex() {
    assert_deterministic("layout_04_nested_flex", 80, 24, |frame| {
        let outer = Flex::vertical().constraints(vec![
            Constraint::Fixed(3),
            Constraint::Min(0),
        ]);
        let outer_areas = outer.split(Rect::new(0, 0, 80, 24));
        Block::default()
            .borders(Borders::ALL)
            .title("Title")
            .render(outer_areas[0], frame);

        let inner = Flex::horizontal().constraints(vec![
            Constraint::Percentage(30.0),
            Constraint::Percentage(70.0),
        ]);
        let inner_areas = inner.split(outer_areas[1]);
        Paragraph::new(Text::raw("Nav")).render(inner_areas[0], frame);
        Paragraph::new(Text::raw("Body")).render(inner_areas[1], frame);
    });
}

#[test]
fn layout_05_grid_3x3() {
    assert_deterministic("layout_05_grid_3x3", 90, 30, |frame| {
        let grid = Grid::new()
            .rows(vec![
                Constraint::Fixed(10),
                Constraint::Fixed(10),
                Constraint::Fixed(10),
            ])
            .columns(vec![
                Constraint::Fixed(30),
                Constraint::Fixed(30),
                Constraint::Fixed(30),
            ]);
        let cells = grid.split(Rect::new(0, 0, 90, 30));
        for (i, cell) in cells.iter().enumerate() {
            Paragraph::new(Text::raw(format!("Cell {i}"))).render(*cell, frame);
        }
    });
}

#[test]
fn layout_06_grid_mixed_constraints() {
    assert_deterministic("layout_06_grid_mixed_constraints", 120, 40, |frame| {
        let grid = Grid::new()
            .rows(vec![
                Constraint::Fixed(5),
                Constraint::Percentage(60.0),
                Constraint::Min(5),
            ])
            .columns(vec![
                Constraint::Fixed(20),
                Constraint::Min(40),
                Constraint::Percentage(25.0),
            ]);
        let cells = grid.split(Rect::new(0, 0, 120, 40));
        for (i, cell) in cells.iter().enumerate() {
            Block::default()
                .borders(Borders::ALL)
                .title(format!("G{i}"))
                .render(*cell, frame);
        }
    });
}

#[test]
fn layout_07_flex_with_gap() {
    assert_deterministic("layout_07_flex_with_gap", 80, 24, |frame| {
        let flex = Flex::horizontal()
            .constraints(vec![Constraint::Percentage(25.0); 4])
            .gap(1);
        let areas = flex.split(Rect::new(0, 0, 80, 24));
        for (i, area) in areas.iter().enumerate() {
            Block::default()
                .borders(Borders::ALL)
                .title(format!("P{i}"))
                .render(*area, frame);
        }
    });
}

#[test]
fn layout_08_ratio_split() {
    assert_deterministic("layout_08_ratio_split", 80, 24, |frame| {
        let flex = Flex::horizontal().constraints(vec![
            Constraint::Ratio(1, 4),
            Constraint::Ratio(2, 4),
            Constraint::Ratio(1, 4),
        ]);
        let areas = flex.split(Rect::new(0, 0, 80, 24));
        for (i, area) in areas.iter().enumerate() {
            Paragraph::new(Text::raw(format!("Ratio {i}"))).render(*area, frame);
        }
    });
}

#[test]
fn layout_09_deeply_nested() {
    assert_deterministic("layout_09_deeply_nested", 120, 40, |frame| {
        let root_area = Rect::new(0, 0, 120, 40);
        let l1 = Flex::vertical()
            .constraints(vec![Constraint::Percentage(50.0); 2])
            .split(root_area);
        for area1 in &l1 {
            let l2 = Flex::horizontal()
                .constraints(vec![Constraint::Percentage(50.0); 2])
                .split(*area1);
            for area2 in &l2 {
                Block::default()
                    .borders(Borders::ALL)
                    .render(*area2, frame);
            }
        }
    });
}

#[test]
fn layout_10_sidebar_main_footer() {
    assert_deterministic("layout_10_sidebar_main_footer", 100, 30, |frame| {
        let root = Rect::new(0, 0, 100, 30);
        let vert = Flex::vertical()
            .constraints(vec![
                Constraint::Min(0),
                Constraint::Fixed(1),
            ])
            .split(root);
        let horiz = Flex::horizontal()
            .constraints(vec![
                Constraint::Fixed(25),
                Constraint::Min(0),
            ])
            .split(vert[0]);

        Block::default()
            .borders(Borders::ALL)
            .title("Files")
            .render(horiz[0], frame);
        Paragraph::new(Text::raw("Editor content")).render(horiz[1], frame);
        Paragraph::new(Text::raw("Ln 42, Col 17")).render(vert[1], frame);
    });
}

#[test]
fn layout_11_equal_columns_5() {
    assert_deterministic("layout_11_equal_columns_5", 100, 20, |frame| {
        let flex = Flex::horizontal().constraints(vec![Constraint::Ratio(1, 5); 5]);
        let areas = flex.split(Rect::new(0, 0, 100, 20));
        for (i, area) in areas.iter().enumerate() {
            Paragraph::new(Text::raw(format!("Col {i}"))).render(*area, frame);
        }
    });
}

#[test]
fn layout_12_grid_4x4() {
    assert_deterministic("layout_12_grid_4x4", 80, 24, |frame| {
        let grid = Grid::new()
            .rows(vec![Constraint::Ratio(1, 4); 4])
            .columns(vec![Constraint::Ratio(1, 4); 4]);
        let cells = grid.split(Rect::new(0, 0, 80, 24));
        for (i, cell) in cells.iter().enumerate() {
            let ch = (b'A' + (i as u8) % 26) as char;
            Paragraph::new(Text::raw(format!("{ch}"))).render(*cell, frame);
        }
    });
}

#[test]
fn layout_13_block_inside_flex() {
    assert_deterministic("layout_13_block_inside_flex", 80, 24, |frame| {
        let areas = Flex::horizontal()
            .constraints(vec![Constraint::Percentage(50.0); 2])
            .split(Rect::new(0, 0, 80, 24));

        let block_l = Block::default()
            .borders(Borders::ALL)
            .title("Left");
        let inner_l = block_l.inner(areas[0]);
        block_l.render(areas[0], frame);
        Paragraph::new(Text::raw("Left content")).render(inner_l, frame);

        let block_r = Block::default()
            .borders(Borders::ALL)
            .title("Right");
        let inner_r = block_r.inner(areas[1]);
        block_r.render(areas[1], frame);
        Paragraph::new(Text::raw("Right content")).render(inner_r, frame);
    });
}

#[test]
fn layout_14_multiple_widgets_vertical() {
    assert_deterministic("layout_14_multiple_widgets_vertical", 80, 24, |frame| {
        let areas = Flex::vertical()
            .constraints(vec![
                Constraint::Fixed(3),
                Constraint::Fixed(3),
                Constraint::Fixed(1),
                Constraint::Min(0),
            ])
            .split(Rect::new(0, 0, 80, 24));

        Block::default()
            .borders(Borders::ALL)
            .title("Header")
            .render(areas[0], frame);
        ProgressBar::new()
            .ratio(0.42)
            .label("42%")
            .render(areas[1], frame);
        Rule::new().title("Separator").render(areas[2], frame);
        Paragraph::new(Text::raw("Main body text")).render(areas[3], frame);
    });
}

#[test]
fn layout_15_flex_min_constraints() {
    assert_deterministic("layout_15_flex_min_constraints", 80, 24, |frame| {
        let areas = Flex::vertical()
            .constraints(vec![
                Constraint::Min(5),
                Constraint::Min(5),
                Constraint::Min(5),
            ])
            .split(Rect::new(0, 0, 80, 24));
        for (i, area) in areas.iter().enumerate() {
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Min {i}"))
                .render(*area, frame);
        }
    });
}

#[test]
fn layout_16_flex_max_constraints() {
    assert_deterministic("layout_16_flex_max_constraints", 80, 24, |frame| {
        let areas = Flex::vertical()
            .constraints(vec![
                Constraint::Max(10),
                Constraint::Max(10),
                Constraint::Min(0),
            ])
            .split(Rect::new(0, 0, 80, 24));
        for (i, area) in areas.iter().enumerate() {
            Paragraph::new(Text::raw(format!("Max area {i}"))).render(*area, frame);
        }
    });
}

#[test]
fn layout_17_sparklines_in_grid() {
    assert_deterministic("layout_17_sparklines_in_grid", 80, 20, |frame| {
        let areas = Flex::vertical()
            .constraints(vec![Constraint::Ratio(1, 4); 4])
            .split(Rect::new(0, 0, 80, 20));
        let datasets: &[&[f64]] = &[
            &[1.0, 3.0, 5.0, 2.0, 8.0],
            &[8.0, 6.0, 4.0, 7.0, 1.0],
            &[2.0, 2.0, 9.0, 1.0, 4.0],
            &[5.0, 5.0, 5.0, 5.0, 5.0],
        ];
        for (i, area) in areas.iter().enumerate() {
            Sparkline::new(datasets[i]).render(*area, frame);
        }
    });
}

#[test]
fn layout_18_progress_bars_stacked() {
    assert_deterministic("layout_18_progress_bars_stacked", 60, 18, |frame| {
        let areas = Flex::vertical()
            .constraints(vec![Constraint::Fixed(3); 6])
            .split(Rect::new(0, 0, 60, 18));
        let ratios = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];
        for (i, area) in areas.iter().enumerate() {
            ProgressBar::new()
                .ratio(ratios[i])
                .label(format!("{}%", (ratios[i] * 100.0) as u32))
                .render(*area, frame);
        }
    });
}

#[test]
fn layout_19_header_content_footer() {
    assert_deterministic("layout_19_header_content_footer", 80, 24, |frame| {
        let areas = Flex::vertical()
            .constraints(vec![
                Constraint::Fixed(3),
                Constraint::Min(0),
                Constraint::Fixed(3),
            ])
            .split(Rect::new(0, 0, 80, 24));

        Block::default()
            .borders(Borders::ALL)
            .title("FrankenTUI")
            .render(areas[0], frame);
        let inner_areas = Flex::horizontal()
            .constraints(vec![
                Constraint::Fixed(20),
                Constraint::Min(0),
            ])
            .split(areas[1]);
        Paragraph::new(Text::raw("Menu\n- Item 1\n- Item 2")).render(inner_areas[0], frame);
        Paragraph::new(Text::raw("Content area\nWith multiple lines")).render(inner_areas[1], frame);
        Block::default()
            .borders(Borders::ALL)
            .title("Status")
            .render(areas[2], frame);
    });
}

#[test]
fn layout_20_grid_2x6() {
    assert_deterministic("layout_20_grid_2x6", 80, 24, |frame| {
        let grid = Grid::new()
            .rows(vec![Constraint::Ratio(1, 6); 6])
            .columns(vec![Constraint::Percentage(50.0); 2]);
        let cells = grid.split(Rect::new(0, 0, 80, 24));
        for (i, cell) in cells.iter().enumerate() {
            Paragraph::new(Text::raw(format!("Cell {i:02}"))).render(*cell, frame);
        }
    });
}

// ===========================================================================
// Category 3: Interaction Sequences (30 scenarios)
// ===========================================================================

#[test]
fn interact_01_list_no_selection() {
    assert_deterministic_stateful(
        "interact_01_list_no_selection",
        40,
        20,
        ListState::default,
        |frame, state| {
            let items: Vec<ListItem> = (0..10).map(|i| ListItem::new(format!("Item {i}"))).collect();
            let list = List::new(items)
                .highlight_symbol("> ")
                .block(Block::default().borders(Borders::ALL));
            StatefulWidget::render(&list, Rect::new(0, 0, 40, 20), frame, state);
        },
    );
}

#[test]
fn interact_02_list_selected_first() {
    assert_deterministic_stateful(
        "interact_02_list_selected_first",
        40,
        20,
        || {
            let mut s = ListState::default();
            s.select(Some(0));
            s
        },
        |frame, state| {
            let items: Vec<ListItem> = (0..10).map(|i| ListItem::new(format!("Item {i}"))).collect();
            let list = List::new(items)
                .highlight_symbol("> ")
                .highlight_style(Style::new().bold());
            StatefulWidget::render(&list, Rect::new(0, 0, 40, 20), frame, state);
        },
    );
}

#[test]
fn interact_03_list_selected_middle() {
    assert_deterministic_stateful(
        "interact_03_list_selected_middle",
        40,
        20,
        || {
            let mut s = ListState::default();
            s.select(Some(5));
            s
        },
        |frame, state| {
            let items: Vec<ListItem> = (0..10).map(|i| ListItem::new(format!("Item {i}"))).collect();
            let list = List::new(items)
                .highlight_symbol("→ ")
                .highlight_style(Style::new().reverse());
            StatefulWidget::render(&list, Rect::new(0, 0, 40, 20), frame, state);
        },
    );
}

#[test]
fn interact_04_list_selected_last() {
    assert_deterministic_stateful(
        "interact_04_list_selected_last",
        40,
        20,
        || {
            let mut s = ListState::default();
            s.select(Some(9));
            s
        },
        |frame, state| {
            let items: Vec<ListItem> = (0..10).map(|i| ListItem::new(format!("Item {i}"))).collect();
            let list = List::new(items).highlight_symbol("> ");
            StatefulWidget::render(&list, Rect::new(0, 0, 40, 20), frame, state);
        },
    );
}

#[test]
fn interact_05_list_scrolled() {
    assert_deterministic_stateful(
        "interact_05_list_scrolled",
        40,
        10,
        || {
            let mut s = ListState::default();
            s.select(Some(15));
            s.offset = 10;
            s
        },
        |frame, state| {
            let items: Vec<ListItem> = (0..30).map(|i| ListItem::new(format!("Line {i}"))).collect();
            let list = List::new(items).highlight_symbol("> ");
            StatefulWidget::render(&list, Rect::new(0, 0, 40, 10), frame, state);
        },
    );
}

#[test]
fn interact_06_list_with_hover() {
    assert_deterministic_stateful(
        "interact_06_list_with_hover",
        40,
        15,
        || {
            let mut s = ListState::default();
            s.select(Some(2));
            s.hovered = Some(4);
            s
        },
        |frame, state| {
            let items: Vec<ListItem> = (0..10).map(|i| ListItem::new(format!("Item {i}"))).collect();
            let list = List::new(items)
                .highlight_symbol("> ")
                .hover_style(Style::new().italic());
            StatefulWidget::render(&list, Rect::new(0, 0, 40, 15), frame, state);
        },
    );
}

#[test]
fn interact_07_table_no_selection() {
    assert_deterministic_stateful(
        "interact_07_table_no_selection",
        80,
        20,
        TableState::default,
        |frame, state| {
            let table = Table::new(
                vec![
                    Row::new(["Alice", "30", "Engineer"]),
                    Row::new(["Bob", "25", "Designer"]),
                    Row::new(["Carol", "35", "Manager"]),
                ],
                vec![
                    Constraint::Fixed(20),
                    Constraint::Fixed(10),
                    Constraint::Min(20),
                ],
            )
            .header(Row::new(["Name", "Age", "Role"]));
            StatefulWidget::render(&table, Rect::new(0, 0, 80, 20), frame, state);
        },
    );
}

#[test]
fn interact_08_table_selected_row() {
    assert_deterministic_stateful(
        "interact_08_table_selected_row",
        80,
        20,
        || {
            let mut s = TableState::default();
            s.selected = Some(1);
            s
        },
        |frame, state| {
            let table = Table::new(
                vec![
                    Row::new(["Alice", "30", "Eng"]),
                    Row::new(["Bob", "25", "Des"]),
                    Row::new(["Carol", "35", "Mgr"]),
                ],
                vec![
                    Constraint::Fixed(20),
                    Constraint::Fixed(10),
                    Constraint::Min(20),
                ],
            )
            .highlight_style(Style::new().bold());
            StatefulWidget::render(&table, Rect::new(0, 0, 80, 20), frame, state);
        },
    );
}

#[test]
fn interact_09_table_with_header_selected() {
    assert_deterministic_stateful(
        "interact_09_table_with_header_selected",
        80,
        15,
        || {
            let mut s = TableState::default();
            s.selected = Some(2);
            s
        },
        |frame, state| {
            let table = Table::new(
                vec![
                    Row::new(["X1", "Y1", "Z1"]),
                    Row::new(["X2", "Y2", "Z2"]),
                    Row::new(["X3", "Y3", "Z3"]),
                    Row::new(["X4", "Y4", "Z4"]),
                ],
                vec![
                    Constraint::Fixed(20),
                    Constraint::Fixed(20),
                    Constraint::Fixed(20),
                ],
            )
            .header(Row::new(["Col A", "Col B", "Col C"]))
            .block(Block::default().borders(Borders::ALL));
            StatefulWidget::render(&table, Rect::new(0, 0, 80, 15), frame, state);
        },
    );
}

#[test]
fn interact_10_scrollbar_top() {
    assert_deterministic_stateful(
        "interact_10_scrollbar_top",
        3,
        20,
        || ScrollbarState::new(100, 0, 20),
        |frame, state| {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            StatefulWidget::render(&scrollbar, Rect::new(0, 0, 3, 20), frame, state);
        },
    );
}

#[test]
fn interact_11_scrollbar_middle() {
    assert_deterministic_stateful(
        "interact_11_scrollbar_middle",
        3,
        20,
        || ScrollbarState::new(100, 50, 20),
        |frame, state| {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            StatefulWidget::render(&scrollbar, Rect::new(0, 0, 3, 20), frame, state);
        },
    );
}

#[test]
fn interact_12_scrollbar_bottom() {
    assert_deterministic_stateful(
        "interact_12_scrollbar_bottom",
        3,
        20,
        || ScrollbarState::new(100, 80, 20),
        |frame, state| {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            StatefulWidget::render(&scrollbar, Rect::new(0, 0, 3, 20), frame, state);
        },
    );
}

#[test]
fn interact_13_scrollbar_horizontal() {
    assert_deterministic_stateful(
        "interact_13_scrollbar_horizontal",
        60,
        3,
        || ScrollbarState::new(200, 75, 60),
        |frame, state| {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::HorizontalBottom);
            StatefulWidget::render(&scrollbar, Rect::new(0, 0, 60, 3), frame, state);
        },
    );
}

#[test]
fn interact_14_list_long_scrolled() {
    assert_deterministic_stateful(
        "interact_14_list_long_scrolled",
        40,
        10,
        || {
            let mut s = ListState::default();
            s.select(Some(95));
            s.offset = 90;
            s
        },
        |frame, state| {
            let items: Vec<ListItem> =
                (0..100).map(|i| ListItem::new(format!("Entry {i:03}"))).collect();
            let list = List::new(items).highlight_symbol("> ");
            StatefulWidget::render(&list, Rect::new(0, 0, 40, 10), frame, state);
        },
    );
}

#[test]
fn interact_15_table_many_rows() {
    assert_deterministic_stateful(
        "interact_15_table_many_rows",
        80,
        15,
        || {
            let mut s = TableState::default();
            s.selected = Some(7);
            s
        },
        |frame, state| {
            let rows: Vec<Row> = (0..20)
                .map(|i| Row::new([format!("R{i}"), format!("{}", i * 10), format!("D{i}")]))
                .collect();
            let table = Table::new(
                rows,
                vec![
                    Constraint::Fixed(10),
                    Constraint::Fixed(10),
                    Constraint::Min(10),
                ],
            );
            StatefulWidget::render(&table, Rect::new(0, 0, 80, 15), frame, state);
        },
    );
}

#[test]
fn interact_16_list_with_styled_items() {
    assert_deterministic_stateful(
        "interact_16_list_with_styled_items",
        50,
        15,
        || {
            let mut s = ListState::default();
            s.select(Some(1));
            s
        },
        |frame, state| {
            let items = vec![
                ListItem::new("Normal item"),
                ListItem::new("Selected item").style(Style::new().bold()),
                ListItem::new("Colored item")
                    .style(Style::new().fg(PackedRgba::rgb(255, 200, 0))),
            ];
            let list = List::new(items)
                .highlight_symbol("* ")
                .highlight_style(Style::new().reverse());
            StatefulWidget::render(&list, Rect::new(0, 0, 50, 15), frame, state);
        },
    );
}

#[test]
fn interact_17_scrollbar_tiny_content() {
    assert_deterministic_stateful(
        "interact_17_scrollbar_tiny_content",
        3,
        20,
        || ScrollbarState::new(5, 0, 20),
        |frame, state| {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            StatefulWidget::render(&scrollbar, Rect::new(0, 0, 3, 20), frame, state);
        },
    );
}

#[test]
fn interact_18_table_hover() {
    assert_deterministic_stateful(
        "interact_18_table_hover",
        80,
        12,
        || {
            let mut s = TableState::default();
            s.selected = Some(0);
            s.hovered = Some(2);
            s
        },
        |frame, state| {
            let table = Table::new(
                vec![
                    Row::new(["A1", "B1"]),
                    Row::new(["A2", "B2"]),
                    Row::new(["A3", "B3"]),
                ],
                vec![Constraint::Percentage(50.0); 2],
            );
            StatefulWidget::render(&table, Rect::new(0, 0, 80, 12), frame, state);
        },
    );
}

#[test]
fn interact_19_list_block_and_selection() {
    assert_deterministic_stateful(
        "interact_19_list_block_and_selection",
        50,
        20,
        || {
            let mut s = ListState::default();
            s.select(Some(3));
            s
        },
        |frame, state| {
            let items: Vec<ListItem> = (0..8).map(|i| ListItem::new(format!("Option {i}"))).collect();
            let list = List::new(items)
                .highlight_symbol("→ ")
                .block(Block::default().borders(Borders::ALL).title("Menu"));
            StatefulWidget::render(&list, Rect::new(0, 0, 50, 20), frame, state);
        },
    );
}

#[test]
fn interact_20_scrollbar_left() {
    assert_deterministic_stateful(
        "interact_20_scrollbar_left",
        3,
        20,
        || ScrollbarState::new(50, 25, 20),
        |frame, state| {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalLeft);
            StatefulWidget::render(&scrollbar, Rect::new(0, 0, 3, 20), frame, state);
        },
    );
}

#[test]
fn interact_21_table_single_column() {
    assert_deterministic_stateful(
        "interact_21_table_single_column",
        30,
        10,
        || {
            let mut s = TableState::default();
            s.selected = Some(0);
            s
        },
        |frame, state| {
            let rows: Vec<Row> = (0..5).map(|i| Row::new([format!("Row {i}")])).collect();
            let table = Table::new(rows, vec![Constraint::Min(10)]);
            StatefulWidget::render(&table, Rect::new(0, 0, 30, 10), frame, state);
        },
    );
}

#[test]
fn interact_22_list_with_markers() {
    assert_deterministic_stateful(
        "interact_22_list_with_markers",
        40,
        12,
        || {
            let mut s = ListState::default();
            s.select(Some(2));
            s
        },
        |frame, state| {
            let items = vec![
                ListItem::new("Alpha").marker("● "),
                ListItem::new("Beta").marker("◆ "),
                ListItem::new("Gamma").marker("★ "),
            ];
            let list = List::new(items).highlight_symbol("→ ");
            StatefulWidget::render(&list, Rect::new(0, 0, 40, 12), frame, state);
        },
    );
}

#[test]
fn interact_23_scrollbar_exact_viewport() {
    assert_deterministic_stateful(
        "interact_23_scrollbar_exact_viewport",
        3,
        10,
        || ScrollbarState::new(10, 0, 10),
        |frame, state| {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            StatefulWidget::render(&scrollbar, Rect::new(0, 0, 3, 10), frame, state);
        },
    );
}

#[test]
fn interact_24_list_empty() {
    assert_deterministic_stateful(
        "interact_24_list_empty",
        40,
        10,
        ListState::default,
        |frame, state| {
            let list = List::new(Vec::<ListItem>::new())
                .block(Block::default().borders(Borders::ALL).title("Empty List"));
            StatefulWidget::render(&list, Rect::new(0, 0, 40, 10), frame, state);
        },
    );
}

#[test]
fn interact_25_table_empty() {
    assert_deterministic_stateful(
        "interact_25_table_empty",
        60,
        10,
        TableState::default,
        |frame, state| {
            let table = Table::new(
                Vec::<Row>::new(),
                vec![Constraint::Fixed(20), Constraint::Min(10)],
            )
            .header(Row::new(["Name", "Value"]));
            StatefulWidget::render(&table, Rect::new(0, 0, 60, 10), frame, state);
        },
    );
}

#[test]
fn interact_26_scrollbar_content_one() {
    assert_deterministic_stateful(
        "interact_26_scrollbar_content_one",
        3,
        10,
        || ScrollbarState::new(1, 0, 10),
        |frame, state| {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            StatefulWidget::render(&scrollbar, Rect::new(0, 0, 3, 10), frame, state);
        },
    );
}

#[test]
fn interact_27_list_selection_traversal() {
    // Verify that rendering with selection at each index is deterministic.
    for sel in 0..5 {
        assert_deterministic_stateful(
            &format!("interact_27_list_selection_{sel}"),
            40,
            10,
            || {
                let mut s = ListState::default();
                s.select(Some(sel));
                s
            },
            |frame, state| {
                let items: Vec<ListItem> =
                    (0..5).map(|i| ListItem::new(format!("Item {i}"))).collect();
                let list = List::new(items).highlight_symbol(">");
                StatefulWidget::render(&list, Rect::new(0, 0, 40, 10), frame, state);
            },
        );
    }
}

#[test]
fn interact_28_table_wide_columns() {
    assert_deterministic_stateful(
        "interact_28_table_wide_columns",
        200,
        10,
        || {
            let mut s = TableState::default();
            s.selected = Some(0);
            s
        },
        |frame, state| {
            let table = Table::new(
                vec![Row::new(["Long column A value here", "Another wide column B"])],
                vec![Constraint::Percentage(60.0), Constraint::Percentage(40.0)],
            );
            StatefulWidget::render(&table, Rect::new(0, 0, 200, 10), frame, state);
        },
    );
}

#[test]
fn interact_29_scrollbar_large_content() {
    assert_deterministic_stateful(
        "interact_29_scrollbar_large_content",
        3,
        20,
        || ScrollbarState::new(10000, 5000, 20),
        |frame, state| {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            StatefulWidget::render(&scrollbar, Rect::new(0, 0, 3, 20), frame, state);
        },
    );
}

#[test]
fn interact_30_table_many_columns() {
    assert_deterministic_stateful(
        "interact_30_table_many_columns",
        120,
        10,
        TableState::default,
        |frame, state| {
            let row = Row::new((0..8).map(|i| format!("C{i}")));
            let widths: Vec<Constraint> = (0..8).map(|_| Constraint::Fixed(15)).collect();
            let table = Table::new(vec![row.clone(), row], widths);
            StatefulWidget::render(&table, Rect::new(0, 0, 120, 10), frame, state);
        },
    );
}

// ===========================================================================
// Category 4: Edge Cases (20 scenarios)
// ===========================================================================

#[test]
fn edge_01_empty_paragraph() {
    assert_deterministic("edge_01_empty_paragraph", 80, 24, |frame| {
        let para = Paragraph::new(Text::raw(""));
        para.render(Rect::new(0, 0, 80, 24), frame);
    });
}

#[test]
fn edge_02_single_cell() {
    assert_deterministic("edge_02_single_cell", 1, 1, |frame| {
        let para = Paragraph::new(Text::raw("X"));
        para.render(Rect::new(0, 0, 1, 1), frame);
    });
}

#[test]
fn edge_03_single_row() {
    assert_deterministic("edge_03_single_row", 80, 1, |frame| {
        let para = Paragraph::new(Text::raw("Single row of text content here"));
        para.render(Rect::new(0, 0, 80, 1), frame);
    });
}

#[test]
fn edge_04_single_column() {
    assert_deterministic("edge_04_single_column", 1, 24, |frame| {
        let para = Paragraph::new(Text::raw("V\ne\nr\nt\ni\nc\na\nl"));
        para.render(Rect::new(0, 0, 1, 24), frame);
    });
}

#[test]
fn edge_05_unicode_cjk() {
    assert_deterministic("edge_05_unicode_cjk", 80, 10, |frame| {
        let para = Paragraph::new(Text::raw("日本語テスト 中文测试 한국어"));
        para.render(Rect::new(0, 0, 80, 10), frame);
    });
}

#[test]
fn edge_06_unicode_emoji() {
    assert_deterministic("edge_06_unicode_emoji", 80, 10, |frame| {
        let para = Paragraph::new(Text::raw("🎉 🚀 ✨ 🎸 🌈 🦀"));
        para.render(Rect::new(0, 0, 80, 10), frame);
    });
}

#[test]
fn edge_07_unicode_combining() {
    assert_deterministic("edge_07_unicode_combining", 80, 10, |frame| {
        // Combining marks: é (e + acute), ñ (n + tilde)
        let para = Paragraph::new(Text::raw("caf\u{0065}\u{0301} man\u{0303}ana"));
        para.render(Rect::new(0, 0, 80, 10), frame);
    });
}

#[test]
fn edge_08_oversized_content() {
    assert_deterministic("edge_08_oversized_content", 20, 5, |frame| {
        // Content much wider than terminal
        let text = "ABCDEFGHIJKLMNOPQRSTUVWXYZ".repeat(10);
        let para = Paragraph::new(Text::raw(&text));
        para.render(Rect::new(0, 0, 20, 5), frame);
    });
}

#[test]
fn edge_09_very_wide() {
    assert_deterministic("edge_09_very_wide", 300, 5, |frame| {
        let para = Paragraph::new(Text::raw("Wide terminal test"));
        para.render(Rect::new(0, 0, 300, 5), frame);
    });
}

#[test]
fn edge_10_very_tall() {
    assert_deterministic("edge_10_very_tall", 10, 200, |frame| {
        let text: String = (0..100).map(|i| format!("L{i:03}\n")).collect();
        let para = Paragraph::new(Text::raw(&text));
        para.render(Rect::new(0, 0, 10, 200), frame);
    });
}

#[test]
fn edge_11_block_zero_inner() {
    assert_deterministic("edge_11_block_zero_inner", 2, 2, |frame| {
        // A 2x2 block with all borders leaves 0x0 inner area
        let block = Block::default().borders(Borders::ALL);
        block.render(Rect::new(0, 0, 2, 2), frame);
    });
}

#[test]
fn edge_12_paragraph_only_newlines() {
    assert_deterministic("edge_12_paragraph_only_newlines", 40, 10, |frame| {
        let para = Paragraph::new(Text::raw("\n\n\n\n\n"));
        para.render(Rect::new(0, 0, 40, 10), frame);
    });
}

#[test]
fn edge_13_paragraph_spaces() {
    assert_deterministic("edge_13_paragraph_spaces", 40, 10, |frame| {
        let para = Paragraph::new(Text::raw("     "));
        para.render(Rect::new(0, 0, 40, 10), frame);
    });
}

#[test]
fn edge_14_mixed_unicode() {
    assert_deterministic("edge_14_mixed_unicode", 80, 10, |frame| {
        let para = Paragraph::new(Text::raw(
            "ASCII + 日本語 + العربية + emoji 🎉 + ñ + café",
        ));
        para.render(Rect::new(0, 0, 80, 10), frame);
    });
}

#[test]
fn edge_15_sparkline_empty() {
    assert_deterministic("edge_15_sparkline_empty", 40, 5, |frame| {
        let data: &[f64] = &[];
        let sparkline = Sparkline::new(data);
        sparkline.render(Rect::new(0, 0, 40, 5), frame);
    });
}

#[test]
fn edge_16_sparkline_single_value() {
    assert_deterministic("edge_16_sparkline_single_value", 40, 5, |frame| {
        let data = [5.0];
        let sparkline = Sparkline::new(&data);
        sparkline.render(Rect::new(0, 0, 40, 5), frame);
    });
}

#[test]
fn edge_17_progress_negative_clamped() {
    assert_deterministic("edge_17_progress_negative_clamped", 50, 3, |frame| {
        // ratio should clamp to 0.0
        let progress = ProgressBar::new().ratio(-0.5);
        progress.render(Rect::new(0, 0, 50, 3), frame);
    });
}

#[test]
fn edge_18_progress_over_one() {
    assert_deterministic("edge_18_progress_over_one", 50, 3, |frame| {
        // ratio should clamp to 1.0
        let progress = ProgressBar::new().ratio(1.5);
        progress.render(Rect::new(0, 0, 50, 3), frame);
    });
}

#[test]
fn edge_19_paragraph_tab_chars() {
    assert_deterministic("edge_19_paragraph_tab_chars", 80, 10, |frame| {
        let para = Paragraph::new(Text::raw("col1\tcol2\tcol3\nA\tB\tC"));
        para.render(Rect::new(0, 0, 80, 10), frame);
    });
}

#[test]
fn edge_20_large_buffer() {
    assert_deterministic("edge_20_large_buffer", 200, 60, |frame| {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Large");
        let inner = block.inner(Rect::new(0, 0, 200, 60));
        block.render(Rect::new(0, 0, 200, 60), frame);

        let text: String = (0..50).map(|i| format!("Line {i:04}: {}\n", "x".repeat(150))).collect();
        let para = Paragraph::new(Text::raw(&text));
        para.render(inner, frame);
    });
}

// ===========================================================================
// Category 5: Regression Scenarios (12 scenarios)
// ===========================================================================

#[test]
fn regress_01_block_inner_zero_size() {
    // Regression: Block inner() with zero-size area shifts origin past bounds,
    // width/height stay 0 (discovered in proptest).
    assert_deterministic("regress_01_block_inner_zero_size", 80, 24, |frame| {
        let block = Block::default().borders(Borders::ALL);
        let zero = Rect::new(10, 10, 0, 0);
        let inner = block.inner(zero);
        // Should not panic
        block.render(zero, frame);
        Paragraph::new(Text::raw("X")).render(inner, frame);
    });
}

#[test]
fn regress_02_empty_rect_intersection() {
    // Regression: Empty Rect intersection(self,self) returns default, not self.
    assert_deterministic("regress_02_empty_rect_intersection", 80, 24, |frame| {
        let area = Rect::new(0, 0, 0, 0);
        let block = Block::default().borders(Borders::ALL);
        block.render(area, frame);
    });
}

#[test]
fn regress_03_list_out_of_bounds_selection() {
    // Regression: List clamps out-of-bounds selection during render.
    assert_deterministic_stateful(
        "regress_03_list_out_of_bounds_selection",
        40,
        10,
        || {
            let mut s = ListState::default();
            s.select(Some(999)); // Way past end
            s
        },
        |frame, state| {
            let items: Vec<ListItem> = (0..5).map(|i| ListItem::new(format!("Item {i}"))).collect();
            let list = List::new(items).highlight_symbol("> ");
            StatefulWidget::render(&list, Rect::new(0, 0, 40, 10), frame, state);
        },
    );
}

#[test]
fn regress_04_buffer_clear_after_render() {
    // Verify that rendering the same scene after a buffer clear is deterministic.
    let name = "regress_04_buffer_clear_after_render";
    let width = 80_u16;
    let height = 24_u16;

    let render = |frame: &mut Frame| {
        Paragraph::new(Text::raw("Test content")).render(Rect::new(0, 0, width, height), frame);
    };

    let mut pool1 = GraphemePool::new();
    let mut frame1 = Frame::new(width, height, &mut pool1);
    render(&mut frame1);
    frame1.buffer.clear();
    render(&mut frame1);
    let cs1 = compute_buffer_checksum(&frame1.buffer);

    let mut pool2 = GraphemePool::new();
    let mut frame2 = Frame::new(width, height, &mut pool2);
    render(&mut frame2);
    frame2.buffer.clear();
    render(&mut frame2);
    let cs2 = compute_buffer_checksum(&frame2.buffer);

    assert_eq!(cs1, cs2, "Determinism violation in '{name}': {cs1} != {cs2}");
    log_jsonl("parity", &[("scenario", name), ("outcome", "pass")]);
}

#[test]
fn regress_05_same_size_different_content() {
    // Verify that different content at the same size produces different checksums.
    let mut pool1 = GraphemePool::new();
    let mut frame1 = Frame::new(80, 24, &mut pool1);
    Paragraph::new(Text::raw("Content A")).render(Rect::new(0, 0, 80, 24), &mut frame1);
    let cs_a = compute_buffer_checksum(&frame1.buffer);

    let mut pool2 = GraphemePool::new();
    let mut frame2 = Frame::new(80, 24, &mut pool2);
    Paragraph::new(Text::raw("Content B")).render(Rect::new(0, 0, 80, 24), &mut frame2);
    let cs_b = compute_buffer_checksum(&frame2.buffer);

    assert_ne!(
        cs_a, cs_b,
        "Different content must produce different checksums"
    );
    log_jsonl(
        "discrimination",
        &[("scenario", "regress_05"), ("outcome", "pass")],
    );
}

#[test]
fn regress_06_styled_cell_determinism() {
    // Verify that manually set styled cells produce deterministic checksums.
    assert_full_deterministic("regress_06_styled_cell_determinism", 40, 10, |frame| {
        for y in 0..10_u16 {
            for x in 0..40_u16 {
                let mut cell = Cell::from_char(if (x + y) % 2 == 0 { '█' } else { '░' });
                cell.fg = PackedRgba::rgb((x * 6) as u8, (y * 25) as u8, 128);
                cell.bg = PackedRgba::rgb(0, 0, (x + y * 4) as u8);
                frame.buffer.set(x, y, cell);
            }
        }
    });
}

#[test]
fn regress_07_overlapping_renders() {
    // Verify determinism when widgets overlap (later render overwrites earlier).
    assert_deterministic("regress_07_overlapping_renders", 80, 24, |frame| {
        // First: fill entire area
        Paragraph::new(Text::raw("Background text ".repeat(20)))
            .render(Rect::new(0, 0, 80, 24), frame);
        // Second: overwrite a sub-region
        Block::default()
            .borders(Borders::ALL)
            .title("Overlay")
            .render(Rect::new(10, 5, 40, 10), frame);
    });
}

#[test]
fn regress_08_triple_render() {
    // Three independent renders of the same scene must all match.
    let name = "regress_08_triple_render";
    let render = |pool: &mut GraphemePool| {
        let mut frame = Frame::new(80, 24, pool);
        let block = Block::default().borders(Borders::ALL).title("Triple");
        let inner = block.inner(Rect::new(0, 0, 80, 24));
        block.render(Rect::new(0, 0, 80, 24), &mut frame);
        Paragraph::new(Text::raw("Content here")).render(inner, &mut frame);
        compute_buffer_checksum(&frame.buffer)
    };

    let mut p1 = GraphemePool::new();
    let mut p2 = GraphemePool::new();
    let mut p3 = GraphemePool::new();
    let cs1 = render(&mut p1);
    let cs2 = render(&mut p2);
    let cs3 = render(&mut p3);

    assert_eq!(cs1, cs2, "Triple render: run 1 != run 2 in '{name}'");
    assert_eq!(cs2, cs3, "Triple render: run 2 != run 3 in '{name}'");
    log_jsonl("parity", &[("scenario", name), ("outcome", "pass")]);
}

#[test]
fn regress_09_size_sensitivity() {
    // Verify that different sizes produce different checksums.
    let sizes = [(80, 24), (81, 24), (80, 25), (79, 23)];
    let mut checksums = Vec::new();

    for (w, h) in sizes {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w, h, &mut pool);
        Paragraph::new(Text::raw("Test")).render(Rect::new(0, 0, w, h), &mut frame);
        checksums.push(compute_buffer_checksum(&frame.buffer));
    }

    for i in 0..checksums.len() {
        for j in (i + 1)..checksums.len() {
            assert_ne!(
                checksums[i], checksums[j],
                "Size ({},{}x{}) == ({},{}x{}) — expected different checksums",
                sizes[i].0, sizes[i].1, checksums[i], sizes[j].0, sizes[j].1, checksums[j]
            );
        }
    }
    log_jsonl(
        "discrimination",
        &[("scenario", "regress_09_size_sensitivity"), ("outcome", "pass")],
    );
}

#[test]
fn regress_10_fg_bg_independence() {
    // Verify that fg and bg changes produce different full checksums.
    let make = |fg: PackedRgba, bg: PackedRgba| {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        for y in 0..5_u16 {
            for x in 0..10_u16 {
                let mut cell = Cell::from_char('A');
                cell.fg = fg;
                cell.bg = bg;
                frame.buffer.set(x, y, cell);
            }
        }
        compute_full_checksum(&frame.buffer)
    };

    let cs_red_fg = make(PackedRgba::rgb(255, 0, 0), PackedRgba::rgb(0, 0, 0));
    let cs_green_fg = make(PackedRgba::rgb(0, 255, 0), PackedRgba::rgb(0, 0, 0));
    let cs_red_bg = make(PackedRgba::rgb(0, 0, 0), PackedRgba::rgb(255, 0, 0));

    assert_ne!(cs_red_fg, cs_green_fg, "fg color should affect full checksum");
    assert_ne!(cs_red_fg, cs_red_bg, "fg vs bg should affect full checksum");

    log_jsonl(
        "discrimination",
        &[("scenario", "regress_10_fg_bg_independence"), ("outcome", "pass")],
    );
}

#[test]
fn regress_11_paragraph_with_long_lines_and_border() {
    // Regression: paragraph text that exceeds block inner width should be clipped,
    // not cause non-determinism.
    assert_deterministic("regress_11_long_lines_border", 40, 10, |frame| {
        let block = Block::default().borders(Borders::ALL).title("Clip");
        let inner = block.inner(Rect::new(0, 0, 40, 10));
        block.render(Rect::new(0, 0, 40, 10), frame);
        let text = "X".repeat(200);
        Paragraph::new(Text::raw(&text)).render(inner, frame);
    });
}

#[test]
fn regress_12_composite_layout_determinism() {
    // Complex composite: header + sidebar + main + footer + sparkline + progress.
    assert_deterministic("regress_12_composite_layout", 100, 30, |frame| {
        let root = Rect::new(0, 0, 100, 30);

        // Vertical: header(3), body, footer(3)
        let vert = Flex::vertical()
            .constraints(vec![
                Constraint::Fixed(3),
                Constraint::Min(0),
                Constraint::Fixed(3),
            ])
            .split(root);

        // Header
        Block::default()
            .borders(Borders::ALL)
            .title("Dashboard")
            .render(vert[0], frame);

        // Body: sidebar(25) + main
        let body = Flex::horizontal()
            .constraints(vec![Constraint::Fixed(25), Constraint::Min(0)])
            .split(vert[1]);

        // Sidebar: list
        let items: Vec<ListItem> = ["Home", "Stats", "Settings", "Help"]
            .iter()
            .map(|s| ListItem::new(*s))
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Nav"));
        let mut list_state = ListState::default();
        list_state.select(Some(1));
        StatefulWidget::render(&list, body[0], frame, &mut list_state);

        // Main: sparkline + progress
        let main_areas = Flex::vertical()
            .constraints(vec![
                Constraint::Fixed(5),
                Constraint::Fixed(3),
                Constraint::Min(0),
            ])
            .split(body[1]);

        let data = [2.0, 4.0, 1.0, 8.0, 5.0, 3.0, 7.0, 6.0, 9.0, 1.0];
        Sparkline::new(&data).render(main_areas[0], frame);
        ProgressBar::new()
            .ratio(0.67)
            .label("67%")
            .render(main_areas[1], frame);
        Paragraph::new(Text::raw("Main content area"))
            .render(main_areas[2], frame);

        // Footer
        Block::default()
            .borders(Borders::ALL)
            .title("Status")
            .render(vert[2], frame);
    });
}

// ===========================================================================
// Summary Test
// ===========================================================================

#[test]
fn parity_suite_summary() {
    log_jsonl(
        "summary",
        &[
            ("bead", "bd-1pys5.6"),
            ("category_simple", "20"),
            ("category_layout", "20"),
            ("category_interact", "30"),
            ("category_edge", "20"),
            ("category_regress", "12"),
            ("total_scenarios", "102"),
        ],
    );
}

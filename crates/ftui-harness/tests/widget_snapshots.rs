#![forbid(unsafe_code)]

//! Integration tests: snapshot testing for core widgets.
//!
//! Run `BLESS=1 cargo test --package ftui-harness` to create/update snapshots.

use ftui_core::geometry::Rect;
use ftui_harness::assert_snapshot;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_text::Text;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::BorderType;
use ftui_widgets::borders::Borders;
use ftui_widgets::list::{List, ListItem, ListState};
use ftui_widgets::panel::Panel;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::scrollbar::{Scrollbar, ScrollbarOrientation, ScrollbarState};
use ftui_widgets::{StatefulWidget, Widget};

// ============================================================================
// Block
// ============================================================================

#[test]
fn snapshot_block_plain() {
    let block = Block::default().borders(Borders::ALL).title("Box");
    let area = Rect::new(0, 0, 12, 5);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 5, &mut pool);
    block.render(area, &mut frame);
    assert_snapshot!("block_plain", &frame.buffer);
}

#[test]
fn snapshot_block_no_borders() {
    let block = Block::default().title("Hello");
    let area = Rect::new(0, 0, 10, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 3, &mut pool);
    block.render(area, &mut frame);
    assert_snapshot!("block_no_borders", &frame.buffer);
}

// ============================================================================
// Paragraph
// ============================================================================

#[test]
fn snapshot_paragraph_simple() {
    let para = Paragraph::new(Text::raw("Hello, FrankenTUI!"));
    let area = Rect::new(0, 0, 20, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(20, 1, &mut pool);
    para.render(area, &mut frame);
    assert_snapshot!("paragraph_simple", &frame.buffer);
}

#[test]
fn snapshot_paragraph_multiline() {
    let para = Paragraph::new(Text::raw("Line 1\nLine 2\nLine 3"));
    let area = Rect::new(0, 0, 10, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 3, &mut pool);
    para.render(area, &mut frame);
    assert_snapshot!("paragraph_multiline", &frame.buffer);
}

#[test]
fn snapshot_paragraph_centered() {
    let para = Paragraph::new(Text::raw("Hi")).alignment(Alignment::Center);
    let area = Rect::new(0, 0, 10, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 1, &mut pool);
    para.render(area, &mut frame);
    assert_snapshot!("paragraph_centered", &frame.buffer);
}

#[test]
fn snapshot_paragraph_in_block() {
    let para = Paragraph::new(Text::raw("Inner"))
        .block(Block::default().borders(Borders::ALL).title("Frame"));
    let area = Rect::new(0, 0, 15, 5);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(15, 5, &mut pool);
    para.render(area, &mut frame);
    assert_snapshot!("paragraph_in_block", &frame.buffer);
}

// ============================================================================
// List
// ============================================================================

#[test]
fn snapshot_list_basic() {
    let items = vec![
        ListItem::new("Apple"),
        ListItem::new("Banana"),
        ListItem::new("Cherry"),
    ];
    let list = List::new(items);
    let area = Rect::new(0, 0, 12, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 3, &mut pool);
    let mut state = ListState::default();
    StatefulWidget::render(&list, area, &mut frame, &mut state);
    assert_snapshot!("list_basic", &frame.buffer);
}

#[test]
fn snapshot_list_with_selection() {
    let items = vec![
        ListItem::new("One"),
        ListItem::new("Two"),
        ListItem::new("Three"),
    ];
    let list = List::new(items).highlight_symbol(">");
    let area = Rect::new(0, 0, 12, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 3, &mut pool);
    let mut state = ListState::default();
    state.select(Some(1));
    StatefulWidget::render(&list, area, &mut frame, &mut state);
    assert_snapshot!("list_with_selection", &frame.buffer);
}

// ============================================================================
// Scrollbar
// ============================================================================

#[test]
fn snapshot_scrollbar_vertical() {
    let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    let area = Rect::new(0, 0, 1, 10);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 10, &mut pool);
    let mut state = ScrollbarState::new(100, 0, 10);
    StatefulWidget::render(&sb, area, &mut frame, &mut state);
    assert_snapshot!("scrollbar_vertical_top", &frame.buffer);
}

#[test]
fn snapshot_scrollbar_vertical_mid() {
    let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    let area = Rect::new(0, 0, 1, 10);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 10, &mut pool);
    let mut state = ScrollbarState::new(100, 45, 10);
    StatefulWidget::render(&sb, area, &mut frame, &mut state);
    assert_snapshot!("scrollbar_vertical_mid", &frame.buffer);
}

#[test]
fn snapshot_scrollbar_horizontal() {
    let sb = Scrollbar::new(ScrollbarOrientation::HorizontalBottom);
    let area = Rect::new(0, 0, 20, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(20, 1, &mut pool);
    let mut state = ScrollbarState::new(100, 0, 20);
    StatefulWidget::render(&sb, area, &mut frame, &mut state);
    assert_snapshot!("scrollbar_horizontal", &frame.buffer);
}

// ============================================================================
// Raw Buffer
// ============================================================================

#[test]
fn snapshot_raw_buffer_pattern() {
    let mut buf = Buffer::new(8, 4);
    // Checkerboard pattern
    for y in 0..4u16 {
        for x in 0..8u16 {
            if (x + y) % 2 == 0 {
                buf.set(x, y, Cell::from_char('#'));
            } else {
                buf.set(x, y, Cell::from_char('.'));
            }
        }
    }
    assert_snapshot!("raw_checkerboard", &buf);
}

// ============================================================================
// Panel
// ============================================================================

#[test]
fn snapshot_panel_square() {
    let child = Paragraph::new(Text::raw("Inner"));
    let panel = Panel::new(child)
        .title("Panel")
        .padding(ftui_core::geometry::Sides::all(1));
    let area = Rect::new(0, 0, 14, 7);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(14, 7, &mut pool);
    panel.render(area, &mut frame);
    assert_snapshot!("panel_square", &frame.buffer);
}

#[test]
fn snapshot_panel_rounded_with_subtitle() {
    let child = Paragraph::new(Text::raw("Hello"));
    let panel = Panel::new(child)
        .border_type(BorderType::Rounded)
        .title("Top")
        .subtitle("Bottom")
        .title_alignment(Alignment::Center)
        .subtitle_alignment(Alignment::Center)
        .padding(ftui_core::geometry::Sides::all(1));
    let area = Rect::new(0, 0, 16, 7);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(16, 7, &mut pool);
    panel.render(area, &mut frame);
    assert_snapshot!("panel_rounded_subtitle", &frame.buffer);
}

#[test]
fn snapshot_panel_ascii_borders() {
    let child = Paragraph::new(Text::raw("ASCII"));
    let panel = Panel::new(child)
        .border_type(BorderType::Ascii)
        .title("Box")
        .padding(ftui_core::geometry::Sides::all(1));
    let area = Rect::new(0, 0, 12, 5);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 5, &mut pool);
    panel.render(area, &mut frame);
    assert_snapshot!("panel_ascii", &frame.buffer);
}

#[test]
fn snapshot_panel_title_truncates_with_ellipsis() {
    let child = Paragraph::new(Text::raw("X"));
    let panel = Panel::new(child)
        .border_type(BorderType::Square)
        .title("VeryLongTitle")
        .padding(ftui_core::geometry::Sides::all(0));
    let area = Rect::new(0, 0, 10, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 3, &mut pool);
    panel.render(area, &mut frame);
    assert_snapshot!("panel_title_ellipsis", &frame.buffer);
}

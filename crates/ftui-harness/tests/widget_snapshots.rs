#![forbid(unsafe_code)]

//! Integration tests: snapshot testing for core widgets.
//!
//! Run `BLESS=1 cargo test --package ftui-harness` to create/update snapshots.

use ftui_core::geometry::Rect;
use ftui_harness::assert_snapshot;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_text::Text;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::Borders;
use ftui_widgets::list::{List, ListItem, ListState};
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
    let mut buf = Buffer::new(12, 5);
    block.render(area, &mut buf);
    assert_snapshot!("block_plain", &buf);
}

#[test]
fn snapshot_block_no_borders() {
    let block = Block::default().title("Hello");
    let area = Rect::new(0, 0, 10, 3);
    let mut buf = Buffer::new(10, 3);
    block.render(area, &mut buf);
    assert_snapshot!("block_no_borders", &buf);
}

// ============================================================================
// Paragraph
// ============================================================================

#[test]
fn snapshot_paragraph_simple() {
    let para = Paragraph::new(Text::raw("Hello, FrankenTUI!"));
    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::new(20, 1);
    para.render(area, &mut buf);
    assert_snapshot!("paragraph_simple", &buf);
}

#[test]
fn snapshot_paragraph_multiline() {
    let para = Paragraph::new(Text::raw("Line 1\nLine 2\nLine 3"));
    let area = Rect::new(0, 0, 10, 3);
    let mut buf = Buffer::new(10, 3);
    para.render(area, &mut buf);
    assert_snapshot!("paragraph_multiline", &buf);
}

#[test]
fn snapshot_paragraph_centered() {
    let para = Paragraph::new(Text::raw("Hi")).alignment(Alignment::Center);
    let area = Rect::new(0, 0, 10, 1);
    let mut buf = Buffer::new(10, 1);
    para.render(area, &mut buf);
    assert_snapshot!("paragraph_centered", &buf);
}

#[test]
fn snapshot_paragraph_in_block() {
    let para = Paragraph::new(Text::raw("Inner"))
        .block(Block::default().borders(Borders::ALL).title("Frame"));
    let area = Rect::new(0, 0, 15, 5);
    let mut buf = Buffer::new(15, 5);
    para.render(area, &mut buf);
    assert_snapshot!("paragraph_in_block", &buf);
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
    let mut buf = Buffer::new(12, 3);
    let mut state = ListState::default();
    StatefulWidget::render(&list, area, &mut buf, &mut state);
    assert_snapshot!("list_basic", &buf);
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
    let mut buf = Buffer::new(12, 3);
    let mut state = ListState::default();
    state.select(Some(1));
    StatefulWidget::render(&list, area, &mut buf, &mut state);
    assert_snapshot!("list_with_selection", &buf);
}

// ============================================================================
// Scrollbar
// ============================================================================

#[test]
fn snapshot_scrollbar_vertical() {
    let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    let area = Rect::new(0, 0, 1, 10);
    let mut buf = Buffer::new(1, 10);
    let mut state = ScrollbarState::new(100, 0, 10);
    StatefulWidget::render(&sb, area, &mut buf, &mut state);
    assert_snapshot!("scrollbar_vertical_top", &buf);
}

#[test]
fn snapshot_scrollbar_vertical_mid() {
    let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    let area = Rect::new(0, 0, 1, 10);
    let mut buf = Buffer::new(1, 10);
    let mut state = ScrollbarState::new(100, 45, 10);
    StatefulWidget::render(&sb, area, &mut buf, &mut state);
    assert_snapshot!("scrollbar_vertical_mid", &buf);
}

#[test]
fn snapshot_scrollbar_horizontal() {
    let sb = Scrollbar::new(ScrollbarOrientation::HorizontalBottom);
    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::new(20, 1);
    let mut state = ScrollbarState::new(100, 0, 20);
    StatefulWidget::render(&sb, area, &mut buf, &mut state);
    assert_snapshot!("scrollbar_horizontal", &buf);
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

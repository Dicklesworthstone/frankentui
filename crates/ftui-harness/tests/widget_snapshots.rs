#![forbid(unsafe_code)]

//! Integration tests: snapshot testing for core widgets.
//!
//! Run `BLESS=1 cargo test --package ftui-harness` to create/update snapshots.

use ftui_core::geometry::{Rect, Sides};
use ftui_harness::{assert_snapshot, assert_snapshot_ansi};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_render::grapheme_pool::GraphemePool;
use ftui_text::Text;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::BorderType;
use ftui_widgets::borders::Borders;
use ftui_widgets::columns::Columns;
use ftui_widgets::inspector::{InspectorMode, InspectorOverlay, InspectorState, WidgetInfo};
use ftui_widgets::list::{List, ListItem, ListState};
use ftui_widgets::modal::{BackdropConfig, Modal, ModalPosition, ModalSizeConstraints};
use ftui_widgets::padding::Padding;
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
// Columns
// ============================================================================

#[test]
fn snapshot_columns_equal() {
    let columns = Columns::new()
        .add(Paragraph::new(Text::raw("Left")))
        .add(Paragraph::new(Text::raw("Center")))
        .add(Paragraph::new(Text::raw("Right")))
        .gap(1);

    let area = Rect::new(0, 0, 20, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(20, 3, &mut pool);
    columns.render(area, &mut frame);
    assert_snapshot!("columns_equal", &frame.buffer);
}

#[test]
fn snapshot_columns_padding() {
    let columns = Columns::new()
        .add(Padding::new(
            Paragraph::new(Text::raw("Pad")),
            Sides::all(1),
        ))
        .add(Paragraph::new(Text::raw("Plain")))
        .gap(1);

    let area = Rect::new(0, 0, 17, 5);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(17, 5, &mut pool);
    columns.render(area, &mut frame);
    assert_snapshot!("columns_padding", &frame.buffer);
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

// ============================================================================
// Modal
// ============================================================================

#[test]
fn snapshot_modal_center_80x24() {
    let content = Paragraph::new(Text::raw("Modal Content"))
        .block(Block::default().borders(Borders::ALL).title("Dialog"));
    let modal = Modal::new(content).size(
        ModalSizeConstraints::new()
            .min_width(20)
            .max_width(20)
            .min_height(5)
            .max_height(5),
    );
    let area = Rect::new(0, 0, 80, 24);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    modal.render(area, &mut frame);
    assert_snapshot!("modal_center_80x24", &frame.buffer);
}

#[test]
fn snapshot_modal_offset_80x24() {
    let content = Paragraph::new(Text::raw("Offset Modal"))
        .block(Block::default().borders(Borders::ALL).title("Offset"));
    let modal = Modal::new(content)
        .size(
            ModalSizeConstraints::new()
                .min_width(16)
                .max_width(16)
                .min_height(4)
                .max_height(4),
        )
        .position(ModalPosition::CenterOffset { x: -10, y: -3 });
    let area = Rect::new(0, 0, 80, 24);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    modal.render(area, &mut frame);
    assert_snapshot!("modal_offset_80x24", &frame.buffer);
}

#[test]
fn snapshot_modal_constrained_120x40() {
    let content = Paragraph::new(Text::raw("Constrained\nWith max size"))
        .block(Block::default().borders(Borders::ALL).title("Constrained"));
    let modal = Modal::new(content).size(
        ModalSizeConstraints::new()
            .min_width(10)
            .max_width(30)
            .min_height(3)
            .max_height(8),
    );
    let area = Rect::new(0, 0, 120, 40);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    modal.render(area, &mut frame);
    assert_snapshot!("modal_constrained_120x40", &frame.buffer);
}

#[test]
fn snapshot_modal_backdrop_opacity() {
    // Fill background with pattern to show backdrop effect
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 12, &mut pool);
    for y in 0..12u16 {
        for x in 0..40u16 {
            frame.buffer.set(
                x,
                y,
                Cell::from_char(if (x + y) % 2 == 0 { '#' } else { '.' }),
            );
        }
    }

    let content =
        Paragraph::new(Text::raw("Content")).block(Block::default().borders(Borders::ALL));
    let modal = Modal::new(content)
        .size(
            ModalSizeConstraints::new()
                .min_width(12)
                .max_width(12)
                .min_height(4)
                .max_height(4),
        )
        .backdrop(BackdropConfig::new(
            ftui_render::cell::PackedRgba::rgb(0, 0, 0),
            0.8,
        ));
    let area = Rect::new(0, 0, 40, 12);
    modal.render(area, &mut frame);
    assert_snapshot!("modal_backdrop_opacity", &frame.buffer);
}

// ============================================================================
// UI Inspector
// ============================================================================

#[test]
fn snapshot_inspector_hit_regions_with_panel() {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(50, 16, &mut pool);
    let area = Rect::new(0, 0, 50, 16);

    let shell = Block::new().title(" Inspector Demo ").borders(Borders::ALL);
    let inner = shell.inner(area);
    shell.render(area, &mut frame);

    let content = Paragraph::new(Text::raw("Hit regions\n- Button\n- Content"))
        .alignment(Alignment::Left);
    content.render(inner, &mut frame);

    let button_hit = Rect::new(inner.x + 2, inner.y + 2, 12, 3);
    let list_hit = Rect::new(inner.x + 2, inner.y + 6, 18, 2);

    frame.register_hit(button_hit, HitId::new(1), HitRegion::Button, 7);
    frame.register_hit(list_hit, HitId::new(2), HitRegion::Content, 0);

    let mut state = InspectorState::new();
    state.mode = InspectorMode::HitRegions;
    state.show_detail_panel = true;
    state.selected = Some(HitId::new(1));
    state.hover_pos = Some((list_hit.x + 1, list_hit.y));

    InspectorOverlay::new(&state).render(area, &mut frame);

    assert_snapshot_ansi!("inspector_hit_regions_with_panel", &frame.buffer);
}

#[test]
fn snapshot_inspector_widget_bounds_tree() {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(60, 18, &mut pool);
    let area = Rect::new(0, 0, 60, 18);

    let shell = Block::new().title(" Layout ").borders(Borders::ALL);
    shell.render(area, &mut frame);

    let mut state = InspectorState::new();
    state.mode = InspectorMode::WidgetBounds;

    let mut root = WidgetInfo::new("Root", Rect::new(1, 1, 58, 16)).with_depth(0);
    let mut left = WidgetInfo::new("LeftPane", Rect::new(2, 2, 26, 12))
        .with_depth(1)
        .with_hit_id(HitId::new(10));
    let right = WidgetInfo::new("RightPane", Rect::new(31, 2, 26, 12))
        .with_depth(1)
        .with_hit_id(HitId::new(11));
    left.add_child(WidgetInfo::new("SearchBox", Rect::new(4, 4, 20, 3)).with_depth(2));
    left.add_child(WidgetInfo::new("Results", Rect::new(4, 8, 20, 5)).with_depth(2));

    root.add_child(left);
    root.add_child(right);
    state.register_widget(root);

    InspectorOverlay::new(&state).render(area, &mut frame);

    assert_snapshot_ansi!("inspector_widget_bounds_tree", &frame.buffer);
}

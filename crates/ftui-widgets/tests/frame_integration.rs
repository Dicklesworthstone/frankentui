#![forbid(unsafe_code)]

//! Integration tests for Widget + Frame API.
//!
//! These tests validate that widgets can:
//! - Write to the frame buffer
//! - Register hit regions
//! - Set cursor position
//! - Respect degradation levels

use ftui_core::geometry::Rect;
use ftui_render::budget::DegradationLevel;
use ftui_render::cell::Cell;
use ftui_render::frame::{Frame, HitId};
use ftui_render::grapheme_pool::GraphemePool;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::BorderType;
use ftui_widgets::input::TextInput;
use ftui_widgets::list::List;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::ProgressBar;
use ftui_widgets::rule::Rule;
use tracing::{Level, info};

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(Level::INFO)
        .try_init();
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

    list.render(Rect::new(0, 0, 4, 2), &mut frame);

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

use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_widgets::textarea::TextArea;
use ftui_widgets::Widget;

#[test]
fn test_cursor_at_wrap_boundary_mid_line() {
    let mut pool = GraphemePool::new();
    // 2x2 area
    let mut frame = Frame::new(2, 2, &mut pool);
    
    // Text: "Hi你" -> "Hi" (width 2) + "你" (width 2)
    // Wrapped at width 2:
    // Row 0: "Hi"
    // Row 1: "你"
    let mut ta = TextArea::new()
        .with_text("Hi你")
        .with_soft_wrap(true)
        .with_focus(true);
    
    // Move cursor to index 2 (before '你').
    // "Hi" is index 0,1. Cursor at 2 is after 'i'.
    // Visual col: 'H'(1) + 'i'(1) = 2.
    ta.move_to_document_start();
    ta.move_right(); // 'H'
    ta.move_right(); // 'i'
    // Now at index 2.
    
    let area = Rect::new(0, 0, 2, 2);
    Widget::render(&ta, area, &mut frame);

    // Expect cursor at (0, 1) -> Start of second line
    // Currently likely failing (None or (2,0))
    assert_eq!(frame.cursor_position, Some((0, 1)));
}

use ftui_widgets::textarea::{TextArea, TextAreaState};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_widgets::Widget;
use ftui_widgets::StatefulWidget;

#[test]
fn test_soft_wrap_cursor_movement_preserves_column() {
    let text = "01234567890123456789"; // 20 chars
    let mut ta = TextArea::new().with_text(text).with_soft_wrap(true);
    
    // Set cursor to pos 5 ('5')
    ta.move_to_document_start();
    for _ in 0..5 {
        ta.move_right();
    }
    assert_eq!(ta.cursor().visual_col, 5);

    // Mock render to establish viewport width = 10
    let mut state = TextAreaState::default();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 5, &mut pool);
    let area = Rect::new(0, 0, 10, 2);
    // Render establishes last_viewport_width = 10
    // TextArea handles margins internally (gutter width 0 here)
    ta.render(area, &mut frame, &mut state);

    // Move down (should move to next wrapped line)
    ta.move_down();

    // Expected: pos 15 (second '5'). 
    // visual_col should be 15.
    // If bug exists: visual_col will be 10.
    assert_eq!(ta.cursor().visual_col, 15, "Cursor should be at 15, but was {}", ta.cursor().visual_col);
}

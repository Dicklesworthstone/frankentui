use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_widgets::list::{List, ListItem, ListState};
use ftui_widgets::StatefulWidget;

#[test]
fn list_scroll_independent_of_selection() {
    let items: Vec<ListItem> = (0..20)
        .map(|i| ListItem::new(format!("Item {i}")))
        .collect();
    let list = List::new(items);
    let area = Rect::new(0, 0, 10, 5);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 5, &mut pool);
    let mut state = ListState::default();

    // 1. Select item 0
    state.select(Some(0));
    
    // 2. Initial render (should ensure item 0 is visible)
    StatefulWidget::render(&list, area, &mut frame, &mut state);
    assert_eq!(state.offset, 0, "Offset should be 0 initially");

    // 3. Scroll down manually (simulate mouse wheel)
    state.scroll_down(5, 20);
    assert_eq!(state.offset, 5, "Offset should be 5 after scroll_down");

    // 4. Render again
    // With the fix, render should respect the manual scroll and NOT force item 0 into view.
    StatefulWidget::render(&list, area, &mut frame, &mut state);
    
    assert_eq!(state.offset, 5, "Offset should remain 5, respecting manual scroll");
}

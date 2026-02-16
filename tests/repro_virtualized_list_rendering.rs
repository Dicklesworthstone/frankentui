#![forbid(unsafe_code)]

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_widgets::virtualized::{RenderItem, VirtualizedList, VirtualizedListState};
use ftui_widgets::StatefulWidget;
use ftui_render::cell::Cell;

struct TestItem(String);

impl RenderItem for TestItem {
    fn render(&self, area: Rect, frame: &mut Frame, _selected: bool) {
        let s = &self.0;
        for (i, ch) in s.chars().enumerate() {
            if i >= area.width as usize { break; }
            for y in area.y..area.bottom() {
                 frame.buffer.set(area.x + i as u16, y, Cell::from_char(ch));
            }
        }
    }
}

#[test]
fn test_virtualized_list_large_item_small_viewport_zero_overscan() {
    // Item height 10. Viewport height 5. Overscan 0.
    // Should render at least the visible part of the item.

    let items = vec![TestItem("ITEM_0".to_string()), TestItem("ITEM_1".to_string())];
    let list = VirtualizedList::new(&items)
        .fixed_height(10)
        .show_scrollbar(false);

    let mut state = VirtualizedListState::new().with_overscan(0);
    
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 5, &mut pool);
    let area = Rect::new(0, 0, 10, 5);

    StatefulWidget::render(&list, area, &mut frame, &mut state);

    // If items_per_viewport is 0 and overscan is 0, nothing renders.
    // We expect "I" at (0,0).
    let cell = frame.buffer.get(0, 0).unwrap();
    assert_eq!(cell.content.as_char(), Some('I'), "Should render item content even if larger than viewport");
}

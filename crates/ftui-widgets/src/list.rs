#![forbid(unsafe_code)]

//! List widget.
//!
//! A widget to display a list of items with selection support.

use crate::block::Block;
use crate::{StatefulWidget, Widget, draw_text_span, draw_text_span_with_link, set_style_area};
use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_style::Style;
use ftui_text::Text;

/// A single item in a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem<'a> {
    content: Text,
    style: Style,
    marker: &'a str,
}

impl<'a> ListItem<'a> {
    pub fn new(content: impl Into<Text>) -> Self {
        Self {
            content: content.into(),
            style: Style::default(),
            marker: "",
        }
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn marker(mut self, marker: &'a str) -> Self {
        self.marker = marker;
        self
    }
}

impl<'a> From<&'a str> for ListItem<'a> {
    fn from(s: &'a str) -> Self {
        Self::new(s)
    }
}

/// A widget to display a list of items.
#[derive(Debug, Clone, Default)]
pub struct List<'a> {
    block: Option<Block<'a>>,
    items: Vec<ListItem<'a>>,
    style: Style,
    highlight_style: Style,
    highlight_symbol: Option<&'a str>,
    /// Optional hit ID for mouse interaction.
    /// When set, each list item registers a hit region with the hit grid.
    hit_id: Option<HitId>,
}

impl<'a> List<'a> {
    pub fn new(items: impl IntoIterator<Item = impl Into<ListItem<'a>>>) -> Self {
        Self {
            block: None,
            items: items.into_iter().map(|i| i.into()).collect(),
            style: Style::default(),
            highlight_style: Style::default(),
            highlight_symbol: None,
            hit_id: None,
        }
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn highlight_style(mut self, style: Style) -> Self {
        self.highlight_style = style;
        self
    }

    pub fn highlight_symbol(mut self, symbol: &'a str) -> Self {
        self.highlight_symbol = Some(symbol);
        self
    }

    /// Set a hit ID for mouse interaction.
    ///
    /// When set, each list item will register a hit region with the frame's
    /// hit grid (if enabled). The hit data will be the item's index, allowing
    /// click handlers to determine which item was clicked.
    pub fn hit_id(mut self, id: HitId) -> Self {
        self.hit_id = Some(id);
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct ListState {
    pub selected: Option<usize>,
    pub offset: usize,
}

impl ListState {
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
        if index.is_none() {
            self.offset = 0;
        }
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }
}

impl<'a> StatefulWidget for List<'a> {
    type State = ListState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "List",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        let list_area = match &self.block {
            Some(b) => {
                b.render(area, frame);
                b.inner(area)
            }
            None => area,
        };

        if list_area.is_empty() {
            return;
        }

        // Apply base style
        set_style_area(&mut frame.buffer, list_area, self.style);

        if self.items.is_empty() {
            return;
        }

        let list_height = list_area.height as usize;

        // Ensure selection is within bounds
        if let Some(selected) = state.selected
            && selected >= self.items.len()
        {
            state.selected = Some(self.items.len() - 1);
        }

        // Ensure visible range includes selected item
        if let Some(selected) = state.selected {
            if selected >= state.offset + list_height {
                state.offset = selected - list_height + 1;
            } else if selected < state.offset {
                state.offset = selected;
            }
        }

        // Iterate over visible items
        for (i, item) in self
            .items
            .iter()
            .enumerate()
            .skip(state.offset)
            .take(list_height)
        {
            let y = list_area.y + (i - state.offset) as u16;
            let is_selected = state.selected == Some(i);

            // Determine style
            let item_style = if is_selected {
                self.highlight_style
            } else {
                item.style
            };

            // Apply item background style to the whole row
            let row_area = Rect::new(list_area.x, y, list_area.width, 1);
            set_style_area(&mut frame.buffer, row_area, item_style);

            // Determine symbol
            let symbol = if is_selected {
                self.highlight_symbol.unwrap_or(item.marker)
            } else {
                item.marker
            };

            let mut x = list_area.x;

            // Draw symbol if present
            if !symbol.is_empty() {
                x = draw_text_span(frame, x, y, symbol, item_style, list_area.right());
                // Add a space after symbol
                x = draw_text_span(frame, x, y, " ", item_style, list_area.right());
            }

            // Draw content
            // Note: List items are currently single-line for simplicity in v1
            if let Some(line) = item.content.lines().first() {
                for span in line.spans() {
                    let span_style = match span.style {
                        Some(s) => s.merge(&item_style),
                        None => item_style,
                    };
                    x = draw_text_span_with_link(
                        frame,
                        x,
                        y,
                        &span.content,
                        span_style,
                        list_area.right(),
                        span.link.as_deref(),
                    );
                    if x >= list_area.right() {
                        break;
                    }
                }
            }

            // Register hit region for this item (if hit testing enabled)
            if let Some(id) = self.hit_id {
                frame.register_hit(row_area, id, HitRegion::Content, i as u64);
            }
        }
    }
}

impl<'a> Widget for List<'a> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let mut state = ListState::default();
        StatefulWidget::render(self, area, frame, &mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn render_empty_list() {
        let list = List::new(Vec::<ListItem>::new());
        let area = Rect::new(0, 0, 10, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        Widget::render(&list, area, &mut frame);
    }

    #[test]
    fn render_simple_list() {
        let items = vec![
            ListItem::new("Item A"),
            ListItem::new("Item B"),
            ListItem::new("Item C"),
        ];
        let list = List::new(items);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('I'));
        assert_eq!(frame.buffer.get(5, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(5, 1).unwrap().content.as_char(), Some('B'));
        assert_eq!(frame.buffer.get(5, 2).unwrap().content.as_char(), Some('C'));
    }

    #[test]
    fn list_state_select() {
        let mut state = ListState::default();
        assert_eq!(state.selected(), None);

        state.select(Some(2));
        assert_eq!(state.selected(), Some(2));

        state.select(None);
        assert_eq!(state.selected(), None);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn list_scrolls_to_selected() {
        let items: Vec<ListItem> = (0..10)
            .map(|i| ListItem::new(format!("Item {i}")))
            .collect();
        let list = List::new(items);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        let mut state = ListState::default();
        state.select(Some(5));

        StatefulWidget::render(&list, area, &mut frame, &mut state);
        // offset should have been adjusted so item 5 is visible
        assert!(state.offset <= 5);
        assert!(state.offset + 3 > 5);
    }

    #[test]
    fn list_clamps_selection() {
        let items = vec![ListItem::new("A"), ListItem::new("B")];
        let list = List::new(items);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        let mut state = ListState::default();
        state.select(Some(10)); // out of bounds

        StatefulWidget::render(&list, area, &mut frame, &mut state);
        // should clamp to last item
        assert_eq!(state.selected(), Some(1));
    }

    #[test]
    fn render_list_with_highlight_symbol() {
        let items = vec![ListItem::new("A"), ListItem::new("B")];
        let list = List::new(items).highlight_symbol(">");
        let area = Rect::new(0, 0, 10, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 2, &mut pool);
        let mut state = ListState::default();
        state.select(Some(0));

        StatefulWidget::render(&list, area, &mut frame, &mut state);
        // First item should have ">" symbol
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('>'));
    }

    #[test]
    fn render_zero_area() {
        let list = List::new(vec![ListItem::new("A")]);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);
    }

    #[test]
    fn list_item_from_str() {
        let item: ListItem = "hello".into();
        assert_eq!(
            item.content.lines().first().unwrap().to_plain_text(),
            "hello"
        );
        assert_eq!(item.marker, "");
    }

    #[test]
    fn list_item_with_marker() {
        let items = vec![
            ListItem::new("A").marker("•"),
            ListItem::new("B").marker("•"),
        ];
        let list = List::new(items);
        let area = Rect::new(0, 0, 10, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 2, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        // Marker should be rendered at the start
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('•'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('•'));
    }

    #[test]
    fn list_state_deselect_resets_offset() {
        let mut state = ListState {
            offset: 5,
            ..Default::default()
        };
        state.select(Some(10));
        assert_eq!(state.offset, 5); // select doesn't reset offset

        state.select(None);
        assert_eq!(state.offset, 0); // deselect resets offset
    }

    #[test]
    fn list_scrolls_up_when_selection_above_viewport() {
        let items: Vec<ListItem> = (0..10)
            .map(|i| ListItem::new(format!("Item {i}")))
            .collect();
        let list = List::new(items);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        let mut state = ListState::default();

        // First scroll down
        state.select(Some(8));
        StatefulWidget::render(&list, area, &mut frame, &mut state);
        assert!(state.offset > 0);

        // Now select item 0 - should scroll back up
        state.select(Some(0));
        StatefulWidget::render(&list, area, &mut frame, &mut state);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn render_list_more_items_than_viewport() {
        let items: Vec<ListItem> = (0..20).map(|i| ListItem::new(format!("{i}"))).collect();
        let list = List::new(items);
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        // Only first 3 should render
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('0'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('1'));
        assert_eq!(frame.buffer.get(0, 2).unwrap().content.as_char(), Some('2'));
    }

    #[test]
    fn widget_render_uses_default_state() {
        let items = vec![ListItem::new("X")];
        let list = List::new(items);
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        // Using Widget trait (not StatefulWidget)
        Widget::render(&list, area, &mut frame);
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('X'));
    }

    #[test]
    fn list_registers_hit_regions() {
        let items = vec![ListItem::new("A"), ListItem::new("B"), ListItem::new("C")];
        let list = List::new(items).hit_id(HitId::new(42));
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(10, 3, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        // Each row should have a hit region with the item index as data
        let hit0 = frame.hit_test(5, 0);
        let hit1 = frame.hit_test(5, 1);
        let hit2 = frame.hit_test(5, 2);

        assert_eq!(hit0, Some((HitId::new(42), HitRegion::Content, 0)));
        assert_eq!(hit1, Some((HitId::new(42), HitRegion::Content, 1)));
        assert_eq!(hit2, Some((HitId::new(42), HitRegion::Content, 2)));
    }

    #[test]
    fn list_no_hit_without_hit_id() {
        let items = vec![ListItem::new("A")];
        let list = List::new(items); // No hit_id set
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(10, 1, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        // No hit region should be registered
        assert!(frame.hit_test(5, 0).is_none());
    }

    #[test]
    fn list_no_hit_without_hit_grid() {
        let items = vec![ListItem::new("A")];
        let list = List::new(items).hit_id(HitId::new(1));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool); // No hit grid
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        // hit_test returns None when no hit grid
        assert!(frame.hit_test(5, 0).is_none());
    }
}

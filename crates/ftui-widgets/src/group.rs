#![forbid(unsafe_code)]

//! Group container widget.
//!
//! A composition primitive that renders multiple child widgets into the same
//! area in deterministic order. Unlike layout containers (Flex, Grid), Group
//! does not reposition children — each child receives the full parent area
//! and is rendered in sequence, with later children drawn on top of earlier
//! ones.
//!
//! This is useful for layering decorations, overlays, or combining widgets
//! that partition the area themselves.

use crate::Widget;
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;

/// A composite container that renders multiple widgets in order.
///
/// Children are rendered in the order they were added. Each child receives
/// the same area, so later children may overwrite earlier ones.
///
/// # Example
///
/// ```ignore
/// use ftui_widgets::group::Group;
///
/// let group = Group::new()
///     .push(background_widget)
///     .push(foreground_widget);
/// group.render(area, &mut frame);
/// ```
pub struct Group<'a> {
    children: Vec<Box<dyn Widget + 'a>>,
}

impl<'a> Group<'a> {
    /// Create a new empty group.
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    /// Add a widget to the group.
    #[must_use]
    pub fn push<W: Widget + 'a>(mut self, widget: W) -> Self {
        self.children.push(Box::new(widget));
        self
    }

    /// Add a boxed widget to the group.
    #[must_use]
    pub fn push_boxed(mut self, widget: Box<dyn Widget + 'a>) -> Self {
        self.children.push(widget);
        self
    }

    /// Number of children in the group.
    #[inline]
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Whether the group has no children.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}

impl Default for Group<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for Group<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }

        for child in &self.children {
            child.render(area, frame);
        }
    }

    fn is_essential(&self) -> bool {
        self.children.iter().any(|c| c.is_essential())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::Cell;
    use ftui_render::grapheme_pool::GraphemePool;

    fn buf_to_lines(buf: &ftui_render::buffer::Buffer) -> Vec<String> {
        let mut lines = Vec::new();
        for y in 0..buf.height() {
            let mut row = String::with_capacity(buf.width() as usize);
            for x in 0..buf.width() {
                let ch = buf
                    .get(x, y)
                    .and_then(|c| c.content.as_char())
                    .unwrap_or(' ');
                row.push(ch);
            }
            lines.push(row);
        }
        lines
    }

    #[derive(Debug, Clone, Copy)]
    struct Fill(char);

    impl Widget for Fill {
        fn render(&self, area: Rect, frame: &mut Frame) {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    frame.buffer.set(x, y, Cell::from_char(self.0));
                }
            }
        }
    }

    /// Renders a single character at a fixed position within the area.
    #[derive(Debug, Clone, Copy)]
    struct Dot {
        ch: char,
        dx: u16,
        dy: u16,
    }

    impl Widget for Dot {
        fn render(&self, area: Rect, frame: &mut Frame) {
            let x = area.x.saturating_add(self.dx);
            let y = area.y.saturating_add(self.dy);
            if x < area.right() && y < area.bottom() {
                frame.buffer.set(x, y, Cell::from_char(self.ch));
            }
        }
    }

    #[test]
    fn empty_group_is_noop() {
        let group = Group::new();
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        group.render(area, &mut frame);

        for y in 0..3 {
            for x in 0..5u16 {
                assert!(frame.buffer.get(x, y).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn single_child_renders() {
        let group = Group::new().push(Fill('A'));
        let area = Rect::new(0, 0, 3, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 2, &mut pool);
        group.render(area, &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["AAA", "AAA"]);
    }

    #[test]
    fn later_children_overwrite_earlier() {
        let group = Group::new().push(Fill('A')).push(Dot {
            ch: 'X',
            dx: 1,
            dy: 0,
        });
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        group.render(area, &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["AXA"]);
    }

    #[test]
    fn deterministic_render_order() {
        // Fill with A, then overwrite entire area with B
        let group = Group::new().push(Fill('A')).push(Fill('B'));
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        group.render(area, &mut frame);

        // B should win everywhere
        assert_eq!(buf_to_lines(&frame.buffer), vec!["BBB"]);
    }

    #[test]
    fn multiple_dots_compose() {
        let group = Group::new()
            .push(Dot {
                ch: '1',
                dx: 0,
                dy: 0,
            })
            .push(Dot {
                ch: '2',
                dx: 2,
                dy: 0,
            })
            .push(Dot {
                ch: '3',
                dx: 1,
                dy: 1,
            });
        let area = Rect::new(0, 0, 3, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 2, &mut pool);
        group.render(area, &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["1 2", " 3 "]);
    }

    #[test]
    fn zero_area_is_noop() {
        let group = Group::new().push(Fill('X'));
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        group.render(area, &mut frame);

        for y in 0..5 {
            for x in 0..5u16 {
                assert!(frame.buffer.get(x, y).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn len_and_is_empty() {
        let g0 = Group::new();
        assert!(g0.is_empty());
        assert_eq!(g0.len(), 0);

        let g1 = Group::new().push(Fill('A'));
        assert!(!g1.is_empty());
        assert_eq!(g1.len(), 1);

        let g3 = Group::new().push(Fill('A')).push(Fill('B')).push(Fill('C'));
        assert_eq!(g3.len(), 3);
    }

    #[test]
    fn is_essential_any_child() {
        struct Essential;
        impl Widget for Essential {
            fn render(&self, _: Rect, _: &mut Frame) {}
            fn is_essential(&self) -> bool {
                true
            }
        }

        assert!(!Group::new().push(Fill('A')).is_essential());
        assert!(Group::new().push(Essential).is_essential());
        assert!(Group::new().push(Fill('A')).push(Essential).is_essential());
    }

    #[test]
    fn push_boxed_works() {
        let boxed: Box<dyn Widget> = Box::new(Fill('Z'));
        let group = Group::new().push_boxed(boxed);
        assert_eq!(group.len(), 1);

        let area = Rect::new(0, 0, 2, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 1, &mut pool);
        group.render(area, &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["ZZ"]);
    }

    #[test]
    fn nested_groups_compose() {
        let inner = Group::new().push(Fill('I'));
        let outer = Group::new().push(Fill('O')).push(inner);

        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        outer.render(area, &mut frame);

        // Inner group (last child) overwrites outer
        assert_eq!(buf_to_lines(&frame.buffer), vec!["III"]);
    }

    #[test]
    fn default_group_is_empty() {
        let g = Group::default();
        assert!(g.is_empty());
        assert_eq!(g.len(), 0);
    }

    #[test]
    fn large_group_all_render() {
        let n = 20;
        let group = (0..n).fold(Group::new(), |g, _| g.push(Fill('X')));
        assert_eq!(group.len(), n);

        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        group.render(area, &mut frame);
        assert_eq!(buf_to_lines(&frame.buffer), vec!["XXX"]);
    }

    #[test]
    fn group_with_offset_area() {
        let group = Group::new().push(Fill('X'));
        let area = Rect::new(2, 1, 3, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 4, &mut pool);
        group.render(area, &mut frame);

        // Only the specified area should be filled
        assert_eq!(
            buf_to_lines(&frame.buffer),
            vec!["      ", "  XXX ", "  XXX ", "      "]
        );
    }
}

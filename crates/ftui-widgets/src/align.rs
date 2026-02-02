#![forbid(unsafe_code)]

//! Alignment container widget.
//!
//! Positions a child widget within an available area according to horizontal
//! and/or vertical alignment rules. The child is rendered into a sub-rect
//! computed from the parent area and the child's known or fixed dimensions.

use crate::block::Alignment;
use crate::{StatefulWidget, Widget};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;

/// Vertical alignment method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerticalAlignment {
    /// Align content to the top (default).
    #[default]
    Top,
    /// Center content vertically.
    Middle,
    /// Align content to the bottom.
    Bottom,
}

/// A widget wrapper that aligns a child within the available area.
///
/// By default, uses the full width/height of the parent area. When explicit
/// `child_width` or `child_height` are set, the child is positioned according
/// to the chosen horizontal and vertical alignment.
///
/// # Example
///
/// ```ignore
/// use ftui_widgets::align::{Align, VerticalAlignment};
/// use ftui_widgets::block::Alignment;
///
/// let centered = Align::new(my_widget)
///     .horizontal(Alignment::Center)
///     .vertical(VerticalAlignment::Middle)
///     .child_width(20)
///     .child_height(5);
/// ```
#[derive(Debug, Clone)]
pub struct Align<W> {
    inner: W,
    horizontal: Alignment,
    vertical: VerticalAlignment,
    child_width: Option<u16>,
    child_height: Option<u16>,
}

impl<W> Align<W> {
    /// Wrap a child widget with default alignment (top-left, full area).
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            horizontal: Alignment::Left,
            vertical: VerticalAlignment::Top,
            child_width: None,
            child_height: None,
        }
    }

    /// Set horizontal alignment.
    pub fn horizontal(mut self, alignment: Alignment) -> Self {
        self.horizontal = alignment;
        self
    }

    /// Set vertical alignment.
    pub fn vertical(mut self, alignment: VerticalAlignment) -> Self {
        self.vertical = alignment;
        self
    }

    /// Set the child's width. If `None`, the child uses the full parent width.
    pub fn child_width(mut self, width: u16) -> Self {
        self.child_width = Some(width);
        self
    }

    /// Set the child's height. If `None`, the child uses the full parent height.
    pub fn child_height(mut self, height: u16) -> Self {
        self.child_height = Some(height);
        self
    }

    /// Compute the aligned child rect within the parent area.
    pub fn aligned_area(&self, area: Rect) -> Rect {
        let w = self.child_width.unwrap_or(area.width).min(area.width);
        let h = self.child_height.unwrap_or(area.height).min(area.height);

        let x = match self.horizontal {
            Alignment::Left => area.x,
            Alignment::Center => area.x + (area.width.saturating_sub(w)) / 2,
            Alignment::Right => area.x + area.width.saturating_sub(w),
        };

        let y = match self.vertical {
            VerticalAlignment::Top => area.y,
            VerticalAlignment::Middle => area.y + (area.height.saturating_sub(h)) / 2,
            VerticalAlignment::Bottom => area.y + area.height.saturating_sub(h),
        };

        Rect::new(x, y, w, h)
    }

    /// Get a shared reference to the inner widget.
    pub const fn inner(&self) -> &W {
        &self.inner
    }

    /// Get a mutable reference to the inner widget.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Consume and return the inner widget.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Widget> Widget for Align<W> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }

        let child_area = self.aligned_area(area);
        if child_area.is_empty() {
            return;
        }

        self.inner.render(child_area, frame);
    }

    fn is_essential(&self) -> bool {
        self.inner.is_essential()
    }
}

impl<W: StatefulWidget> StatefulWidget for Align<W> {
    type State = W::State;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        if area.is_empty() {
            return;
        }

        let child_area = self.aligned_area(area);
        if child_area.is_empty() {
            return;
        }

        self.inner.render(child_area, frame, state);
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

    /// A small test widget that fills its area with a character.
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

    #[test]
    fn default_alignment_uses_full_area() {
        let align = Align::new(Fill('X'));
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        align.render(area, &mut frame);

        for line in buf_to_lines(&frame.buffer) {
            assert_eq!(line, "XXXXX");
        }
    }

    #[test]
    fn center_horizontal() {
        let align = Align::new(Fill('X'))
            .horizontal(Alignment::Center)
            .child_width(3);
        let area = Rect::new(0, 0, 7, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(7, 1, &mut pool);
        align.render(area, &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["  XXX  "]);
    }

    #[test]
    fn right_horizontal() {
        let align = Align::new(Fill('X'))
            .horizontal(Alignment::Right)
            .child_width(3);
        let area = Rect::new(0, 0, 7, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(7, 1, &mut pool);
        align.render(area, &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["    XXX"]);
    }

    #[test]
    fn left_horizontal() {
        let align = Align::new(Fill('X'))
            .horizontal(Alignment::Left)
            .child_width(3);
        let area = Rect::new(0, 0, 7, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(7, 1, &mut pool);
        align.render(area, &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["XXX    "]);
    }

    #[test]
    fn center_vertical() {
        let align = Align::new(Fill('X'))
            .vertical(VerticalAlignment::Middle)
            .child_height(1);
        let area = Rect::new(0, 0, 3, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 5, &mut pool);
        align.render(area, &mut frame);

        assert_eq!(
            buf_to_lines(&frame.buffer),
            vec!["   ", "   ", "XXX", "   ", "   "]
        );
    }

    #[test]
    fn bottom_vertical() {
        let align = Align::new(Fill('X'))
            .vertical(VerticalAlignment::Bottom)
            .child_height(2);
        let area = Rect::new(0, 0, 3, 4);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 4, &mut pool);
        align.render(area, &mut frame);

        assert_eq!(
            buf_to_lines(&frame.buffer),
            vec!["   ", "   ", "XXX", "XXX"]
        );
    }

    #[test]
    fn center_both_axes() {
        let align = Align::new(Fill('O'))
            .horizontal(Alignment::Center)
            .vertical(VerticalAlignment::Middle)
            .child_width(1)
            .child_height(1);
        let area = Rect::new(0, 0, 5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        align.render(area, &mut frame);

        assert_eq!(
            buf_to_lines(&frame.buffer),
            vec!["     ", "     ", "  O  ", "     ", "     "]
        );
    }

    #[test]
    fn child_larger_than_area_is_clamped() {
        let align = Align::new(Fill('X'))
            .horizontal(Alignment::Center)
            .child_width(20)
            .child_height(10);
        let area = Rect::new(0, 0, 5, 3);

        let child_area = align.aligned_area(area);
        assert_eq!(child_area.width, 5);
        assert_eq!(child_area.height, 3);
    }

    #[test]
    fn zero_size_area_is_noop() {
        let align = Align::new(Fill('X'))
            .horizontal(Alignment::Center)
            .child_width(3);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        align.render(area, &mut frame);

        // Nothing should have been drawn
        for y in 0..5 {
            for x in 0..5u16 {
                assert!(frame.buffer.get(x, y).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn zero_child_size_is_noop() {
        let align = Align::new(Fill('X')).child_width(0).child_height(0);
        let area = Rect::new(0, 0, 5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        align.render(area, &mut frame);

        for y in 0..5 {
            for x in 0..5u16 {
                assert!(frame.buffer.get(x, y).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn area_with_offset() {
        let align = Align::new(Fill('X'))
            .horizontal(Alignment::Center)
            .child_width(2);
        let area = Rect::new(10, 5, 6, 1);

        let child = align.aligned_area(area);
        assert_eq!(child.x, 12);
        assert_eq!(child.y, 5);
        assert_eq!(child.width, 2);
    }

    #[test]
    fn aligned_area_right_bottom() {
        let align = Align::new(Fill('X'))
            .horizontal(Alignment::Right)
            .vertical(VerticalAlignment::Bottom)
            .child_width(2)
            .child_height(1);
        let area = Rect::new(0, 0, 10, 5);

        let child = align.aligned_area(area);
        assert_eq!(child.x, 8);
        assert_eq!(child.y, 4);
        assert_eq!(child.width, 2);
        assert_eq!(child.height, 1);
    }

    #[test]
    fn is_essential_delegates() {
        struct Essential;
        impl Widget for Essential {
            fn render(&self, _: Rect, _: &mut Frame) {}
            fn is_essential(&self) -> bool {
                true
            }
        }

        assert!(Align::new(Essential).is_essential());
        assert!(!Align::new(Fill('X')).is_essential());
    }
}

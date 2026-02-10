#![forbid(unsafe_code)]

//! Padding container widget.
//!
//! This is a small compositional building block: it shrinks the render area
//! passed to a child widget by applying [`Sides`] padding, and uses the
//! buffer's scissor stack to guarantee the child cannot write outside the
//! padded inner rectangle.

use crate::{StatefulWidget, Widget};
use ftui_core::geometry::{Rect, Sides};
use ftui_render::frame::Frame;

/// A widget wrapper that applies padding around an inner widget.
#[derive(Debug, Clone)]
pub struct Padding<W> {
    inner: W,
    padding: Sides,
}

impl<W> Padding<W> {
    /// Create a new padding wrapper.
    pub const fn new(inner: W, padding: Sides) -> Self {
        Self { inner, padding }
    }

    /// Set the padding (builder-style).
    #[must_use]
    pub const fn padding(mut self, padding: Sides) -> Self {
        self.padding = padding;
        self
    }

    /// Get the configured padding.
    pub const fn padding_sides(&self) -> Sides {
        self.padding
    }

    /// Compute the inner rect for a given outer area.
    #[inline]
    pub fn inner_area(&self, area: Rect) -> Rect {
        area.inner(self.padding)
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

struct ScissorGuard<'a, 'pool> {
    frame: &'a mut Frame<'pool>,
}

impl<'a, 'pool> ScissorGuard<'a, 'pool> {
    fn new(frame: &'a mut Frame<'pool>, rect: Rect) -> Self {
        frame.buffer.push_scissor(rect);
        Self { frame }
    }
}

impl Drop for ScissorGuard<'_, '_> {
    fn drop(&mut self) {
        self.frame.buffer.pop_scissor();
    }
}

impl<W: Widget> Widget for Padding<W> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Padding",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() {
            return;
        }

        let inner = self.inner_area(area);
        if inner.is_empty() {
            return;
        }

        let guard = ScissorGuard::new(frame, inner);
        self.inner.render(inner, guard.frame);
    }

    fn is_essential(&self) -> bool {
        self.inner.is_essential()
    }
}

impl<W: StatefulWidget> StatefulWidget for Padding<W> {
    type State = W::State;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "PaddingStateful",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() {
            return;
        }

        let inner = self.inner_area(area);
        if inner.is_empty() {
            return;
        }

        let guard = ScissorGuard::new(frame, inner);
        self.inner.render(inner, guard.frame, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::buffer::Buffer;
    use ftui_render::cell::Cell;
    use ftui_render::grapheme_pool::GraphemePool;

    fn buf_to_lines(buf: &Buffer) -> Vec<String> {
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

    #[derive(Debug, Clone, Copy)]
    struct Naughty;

    impl Widget for Naughty {
        fn render(&self, _area: Rect, frame: &mut Frame) {
            // Intentionally ignore the provided area and attempt to write outside.
            frame.buffer.set(0, 0, Cell::from_char('X'));
            frame.buffer.set(2, 2, Cell::from_char('Y'));
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct Boom;

    impl Widget for Boom {
        fn render(&self, _area: Rect, _frame: &mut Frame) {
            unreachable!("boom");
        }
    }

    #[test]
    fn inner_area_zero_padding_is_identity() {
        let pad = Padding::new(Fill('A'), Sides::all(0));
        let area = Rect::new(3, 4, 10, 7);
        assert_eq!(pad.inner_area(area), area);
    }

    #[test]
    fn inner_area_asymmetric_padding() {
        let pad = Padding::new(Fill('A'), Sides::new(1, 2, 1, 3));
        let area = Rect::new(0, 0, 10, 4);
        assert_eq!(pad.inner_area(area), Rect::new(3, 1, 5, 2));
    }

    #[test]
    fn inner_area_clamps_when_padding_exceeds_area() {
        let pad = Padding::new(Fill('A'), Sides::all(5));
        let inner = pad.inner_area(Rect::new(0, 0, 2, 2));
        assert_eq!(inner.width, 0);
        assert_eq!(inner.height, 0);
    }

    #[test]
    fn render_padding_shifts_child_and_leaves_gutter_blank() {
        let pad = Padding::new(Fill('A'), Sides::all(1));
        let area = Rect::from_size(5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        pad.render(area, &mut frame);

        assert_eq!(
            buf_to_lines(&frame.buffer),
            vec![
                "     ".to_string(),
                " AAA ".to_string(),
                " AAA ".to_string(),
                " AAA ".to_string(),
                "     ".to_string(),
            ]
        );
    }

    #[test]
    fn render_is_clipped_to_inner_rect_via_scissor() {
        let pad = Padding::new(Naughty, Sides::all(1));
        let area = Rect::from_size(5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        pad.render(area, &mut frame);

        // (0,0) is outside the inner rect, so it must not be written.
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
        // (2,2) is inside the inner rect and should be written.
        assert_eq!(frame.buffer.get(2, 2).unwrap().content.as_char(), Some('Y'));
    }

    #[test]
    fn scissor_stack_restores_on_panic() {
        let pad = Padding::new(Boom, Sides::all(1));
        let area = Rect::from_size(5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        assert_eq!(frame.buffer.scissor_depth(), 1);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pad.render(area, &mut frame);
        }));
        assert!(result.is_err());
        assert_eq!(frame.buffer.scissor_depth(), 1);
    }

    #[test]
    fn render_empty_area_is_noop() {
        let pad = Padding::new(Fill('X'), Sides::all(1));
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        pad.render(area, &mut frame);
        for y in 0..5 {
            for x in 0..5u16 {
                assert!(frame.buffer.get(x, y).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn padding_larger_than_area_renders_nothing() {
        let pad = Padding::new(Fill('X'), Sides::all(10));
        let area = Rect::from_size(5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        pad.render(area, &mut frame);
        // Inner area is empty, so nothing should be rendered
        for y in 0..5 {
            for x in 0..5u16 {
                assert!(frame.buffer.get(x, y).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn asymmetric_padding_top_left() {
        let pad = Padding::new(Fill('A'), Sides::new(2, 0, 0, 1));
        let area = Rect::from_size(5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        pad.render(area, &mut frame);

        let lines = buf_to_lines(&frame.buffer);
        // top=2, right=0, bottom=0, left=1
        assert_eq!(lines[0], "     "); // top padding row 0
        assert_eq!(lines[1], "     "); // top padding row 1
        assert_eq!(lines[2], " AAAA"); // content starts at x=1
        assert_eq!(lines[3], " AAAA");
        assert_eq!(lines[4], " AAAA");
    }

    #[test]
    fn padding_sides_accessor() {
        let pad = Padding::new(Fill('A'), Sides::new(1, 2, 3, 4));
        let s = pad.padding_sides();
        assert_eq!(s.top, 1);
        assert_eq!(s.right, 2);
        assert_eq!(s.bottom, 3);
        assert_eq!(s.left, 4);
    }

    #[test]
    fn inner_accessor() {
        let pad = Padding::new(Fill('A'), Sides::all(0));
        assert_eq!(pad.inner().0, 'A');
    }

    #[test]
    fn inner_mut_accessor() {
        let mut pad = Padding::new(Fill('A'), Sides::all(0));
        pad.inner_mut().0 = 'B';
        assert_eq!(pad.inner().0, 'B');
    }

    #[test]
    fn into_inner() {
        let pad = Padding::new(Fill('Z'), Sides::all(0));
        let inner = pad.into_inner();
        assert_eq!(inner.0, 'Z');
    }

    #[test]
    fn padding_builder() {
        let pad = Padding::new(Fill('A'), Sides::all(0)).padding(Sides::all(2));
        assert_eq!(pad.padding_sides(), Sides::all(2));
    }

    #[test]
    fn is_essential_delegates_to_inner() {
        #[derive(Debug, Clone, Copy)]
        struct Essential;
        impl Widget for Essential {
            fn render(&self, _: Rect, _: &mut Frame) {}
            fn is_essential(&self) -> bool {
                true
            }
        }

        let non_essential = Padding::new(Fill('A'), Sides::all(0));
        assert!(!non_essential.is_essential());

        let essential = Padding::new(Essential, Sides::all(0));
        assert!(essential.is_essential());
    }

    #[test]
    fn stateful_render_with_padding() {
        #[derive(Debug, Clone, Copy)]
        struct StateFill(char);

        impl StatefulWidget for StateFill {
            type State = usize;
            fn render(&self, area: Rect, frame: &mut Frame, state: &mut usize) {
                *state += 1;
                for y in area.y..area.bottom() {
                    for x in area.x..area.right() {
                        frame.buffer.set(x, y, Cell::from_char(self.0));
                    }
                }
            }
        }

        let pad = Padding::new(StateFill('S'), Sides::all(1));
        let area = Rect::from_size(5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        let mut state: usize = 0;
        StatefulWidget::render(&pad, area, &mut frame, &mut state);

        assert_eq!(state, 1);
        let lines = buf_to_lines(&frame.buffer);
        assert_eq!(lines[0], "     ");
        assert_eq!(lines[1], " SSS ");
        assert_eq!(lines[2], " SSS ");
    }

    #[test]
    fn large_padding_single_cell_inner() {
        let pad = Padding::new(Fill('X'), Sides::new(1, 1, 1, 1));
        let area = Rect::from_size(3, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 3, &mut pool);
        pad.render(area, &mut frame);

        let lines = buf_to_lines(&frame.buffer);
        assert_eq!(lines[0], "   ");
        assert_eq!(lines[1], " X ");
        assert_eq!(lines[2], "   ");
    }

    #[test]
    fn naughty_widget_with_asymmetric_padding() {
        // Test that scissor correctly clips even with non-uniform padding
        let pad = Padding::new(Naughty, Sides::new(0, 0, 0, 2));
        let area = Rect::from_size(5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        pad.render(area, &mut frame);

        // Naughty writes at (0,0) which is outside inner (x>=2), should be clipped
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
        // (2,2) is inside inner, should be written
        assert_eq!(frame.buffer.get(2, 2).unwrap().content.as_char(), Some('Y'));
    }
}

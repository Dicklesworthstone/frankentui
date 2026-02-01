#![forbid(unsafe_code)]

//! Padding container widget.
//!
//! This is a small compositional building block: it shrinks the render area
//! passed to a child widget by applying [`Sides`] padding, and uses the
//! [`Buffer`] scissor stack to guarantee the child cannot write outside the
//! padded inner rectangle.

use crate::{StatefulWidget, Widget};
use ftui_core::geometry::{Rect, Sides};
use ftui_render::buffer::Buffer;

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

struct ScissorGuard<'a> {
    buf: &'a mut Buffer,
}

impl<'a> ScissorGuard<'a> {
    fn new(buf: &'a mut Buffer, rect: Rect) -> Self {
        buf.push_scissor(rect);
        Self { buf }
    }
}

impl Drop for ScissorGuard<'_> {
    fn drop(&mut self) {
        self.buf.pop_scissor();
    }
}

impl<W: Widget> Widget for Padding<W> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
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

        let mut guard = ScissorGuard::new(buf, inner);
        self.inner.render(inner, &mut *guard.buf);
    }

    fn is_essential(&self) -> bool {
        self.inner.is_essential()
    }
}

impl<W: StatefulWidget> StatefulWidget for Padding<W> {
    type State = W::State;

    fn render(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
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

        let mut guard = ScissorGuard::new(buf, inner);
        self.inner.render(inner, &mut *guard.buf, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::Cell;

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
        fn render(&self, area: Rect, buf: &mut Buffer) {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    buf.set(x, y, Cell::from_char(self.0));
                }
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct Naughty;

    impl Widget for Naughty {
        fn render(&self, _area: Rect, buf: &mut Buffer) {
            // Intentionally ignore the provided area and attempt to write outside.
            buf.set(0, 0, Cell::from_char('X'));
            buf.set(2, 2, Cell::from_char('Y'));
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct Boom;

    impl Widget for Boom {
        fn render(&self, _area: Rect, _buf: &mut Buffer) {
            panic!("boom");
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
        let mut buf = Buffer::new(5, 5);
        pad.render(area, &mut buf);

        assert_eq!(
            buf_to_lines(&buf),
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
        let mut buf = Buffer::new(5, 5);
        pad.render(area, &mut buf);

        // (0,0) is outside the inner rect, so it must not be written.
        assert!(buf.get(0, 0).unwrap().is_empty());
        // (2,2) is inside the inner rect and should be written.
        assert_eq!(buf.get(2, 2).unwrap().content.as_char(), Some('Y'));
    }

    #[test]
    fn scissor_stack_restores_on_panic() {
        let pad = Padding::new(Boom, Sides::all(1));
        let area = Rect::from_size(5, 5);
        let mut buf = Buffer::new(5, 5);
        assert_eq!(buf.scissor_depth(), 1);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pad.render(area, &mut buf);
        }));
        assert!(result.is_err());
        assert_eq!(buf.scissor_depth(), 1);
    }
}


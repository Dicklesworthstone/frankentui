#![forbid(unsafe_code)]

//! Badge widget.
//!
//! A small, single-line label with background + foreground styling and
//! configurable left/right padding. Intended for "status", "priority", etc.
//!
//! Design goals:
//! - No per-render heap allocations (draws directly to the `Frame`)
//! - Deterministic output (stable padding + truncation)
//! - Tiny-area safe (0 width/height is a no-op)

use crate::{Widget, apply_style, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_style::Style;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A compact label with padding and style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Badge<'a> {
    label: &'a str,
    style: Style,
    pad_left: u16,
    pad_right: u16,
}

impl<'a> Badge<'a> {
    /// Create a new badge with 1 cell padding on each side.
    #[must_use]
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            style: Style::default(),
            pad_left: 1,
            pad_right: 1,
        }
    }

    /// Set the badge style (foreground/background/attrs).
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the left/right padding in cells.
    #[must_use]
    pub fn with_padding(mut self, left: u16, right: u16) -> Self {
        self.pad_left = left;
        self.pad_right = right;
        self
    }

    /// Display width in terminal cells (label width + padding).
    #[must_use]
    pub fn width(&self) -> u16 {
        let label_width: u16 = self
            .label
            .graphemes(true)
            .map(|g| UnicodeWidthStr::width(g) as u16)
            .sum();
        label_width
            .saturating_add(self.pad_left)
            .saturating_add(self.pad_right)
    }

    #[inline]
    fn render_spaces(
        frame: &mut Frame,
        mut x: u16,
        y: u16,
        n: u16,
        style: Style,
        max_x: u16,
    ) -> u16 {
        let mut cell = Cell::from_char(' ');
        apply_style(&mut cell, style);
        for _ in 0..n {
            if x >= max_x {
                break;
            }
            frame.buffer.set(x, y, cell);
            x = x.saturating_add(1);
        }
        x
    }
}

impl Widget for Badge<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }

        let y = area.y;
        let max_x = area.right();
        let mut x = area.x;

        x = Self::render_spaces(frame, x, y, self.pad_left, self.style, max_x);
        x = draw_text_span(frame, x, y, self.label, self.style, max_x);
        let _ = Self::render_spaces(frame, x, y, self.pad_right, self.style, max_x);
    }

    fn is_essential(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::PackedRgba;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn width_includes_padding() {
        let badge = Badge::new("OK");
        assert_eq!(badge.width(), 4);
        let badge = Badge::new("OK").with_padding(2, 3);
        assert_eq!(badge.width(), 7);
    }

    #[test]
    fn renders_padded_label_with_style() {
        let style = Style::new()
            .fg(PackedRgba::rgb(1, 2, 3))
            .bg(PackedRgba::rgb(4, 5, 6));
        let badge = Badge::new("OK").with_style(style);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        badge.render(Rect::new(0, 0, 10, 1), &mut frame);

        let expected = [' ', 'O', 'K', ' '];
        for (x, ch) in expected.into_iter().enumerate() {
            let cell = frame.buffer.get(x as u16, 0).unwrap();
            assert_eq!(cell.content.as_char(), Some(ch));
            assert_eq!(cell.fg, PackedRgba::rgb(1, 2, 3));
            assert_eq!(cell.bg, PackedRgba::rgb(4, 5, 6));
        }
    }

    #[test]
    fn truncates_in_small_area() {
        let style = Style::new()
            .fg(PackedRgba::rgb(1, 2, 3))
            .bg(PackedRgba::rgb(4, 5, 6));
        let badge = Badge::new("OK").with_style(style);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 1, &mut pool);
        badge.render(Rect::new(0, 0, 2, 1), &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some(' '));
        assert_eq!(frame.buffer.get(1, 0).unwrap().content.as_char(), Some('O'));
    }
}

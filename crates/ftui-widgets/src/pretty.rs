//! Pretty-printing widget for Rust values.
//!
//! Renders a value's [`Debug`] representation with optional wrapping and
//! configurable formatting into a [`Frame`].
//!
//! # Example
//!
//! ```
//! use ftui_widgets::pretty::Pretty;
//!
//! let data = vec![1, 2, 3];
//! let widget = Pretty::new(&data);
//! assert!(!widget.formatted_text().is_empty());
//! ```

use crate::{Widget, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;
use std::fmt::Debug;

/// Pretty-printing widget that renders a `Debug` representation.
///
/// Wraps any `Debug` value and renders it line-by-line into a frame,
/// using either compact (`{:?}`) or pretty (`{:#?}`) formatting.
pub struct Pretty<'a, T: Debug + ?Sized> {
    value: &'a T,
    compact: bool,
    style: Style,
}

impl<'a, T: Debug + ?Sized> Pretty<'a, T> {
    /// Create a new pretty widget for a value.
    #[must_use]
    pub fn new(value: &'a T) -> Self {
        Self {
            value,
            compact: false,
            style: Style::default(),
        }
    }

    /// Use compact formatting (`{:?}`) instead of pretty (`{:#?}`).
    #[must_use]
    pub fn with_compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// Set the text style.
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Get the formatted text as a string.
    #[must_use]
    pub fn formatted_text(&self) -> String {
        if self.compact {
            format!("{:?}", self.value)
        } else {
            format!("{:#?}", self.value)
        }
    }
}

impl<T: Debug + ?Sized> Widget for Pretty<'_, T> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let text = self.formatted_text();
        let max_x = area.right();

        for (row_idx, line) in text.lines().enumerate() {
            if row_idx as u16 >= area.height {
                break;
            }
            let y = area.y + row_idx as u16;
            draw_text_span(frame, area.x, y, line, self.style, max_x);
        }
    }

    fn is_essential(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn format_simple_value() {
        let widget = Pretty::new(&42i32);
        assert_eq!(widget.formatted_text(), "42");
    }

    #[test]
    fn format_vec() {
        let data = vec![1, 2, 3];
        let widget = Pretty::new(&data);
        let text = widget.formatted_text();
        assert!(text.contains("1"));
        assert!(text.contains("2"));
        assert!(text.contains("3"));
    }

    #[test]
    fn format_compact() {
        let data = vec![1, 2, 3];
        let compact = Pretty::new(&data).with_compact(true);
        let text = compact.formatted_text();
        // Compact is single-line
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn format_pretty() {
        let data = vec![1, 2, 3];
        let pretty = Pretty::new(&data).with_compact(false);
        let text = pretty.formatted_text();
        // Pretty is multi-line
        assert!(text.lines().count() > 1);
    }

    #[derive(Debug)]
    struct TestStruct {
        name: String,
        value: i32,
    }

    #[test]
    fn format_struct() {
        let s = TestStruct {
            name: "hello".to_string(),
            value: 42,
        };
        let widget = Pretty::new(&s);
        let text = widget.formatted_text();
        assert!(text.contains("name"));
        assert!(text.contains("hello"));
        assert!(text.contains("42"));
    }

    #[test]
    fn format_string() {
        let widget = Pretty::new("hello world");
        let text = widget.formatted_text();
        assert!(text.contains("hello world"));
    }

    #[test]
    fn render_basic() {
        let data = vec![1, 2, 3];
        let widget = Pretty::new(&data);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::new(0, 0, 40, 10);
        widget.render(area, &mut frame);

        // First line starts with '['
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('['));
    }

    #[test]
    fn render_zero_area() {
        let widget = Pretty::new(&42);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        widget.render(Rect::new(0, 0, 0, 0), &mut frame); // No panic
    }

    #[test]
    fn render_truncated_height() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let widget = Pretty::new(&data);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 3, &mut pool);
        let area = Rect::new(0, 0, 40, 3);
        widget.render(area, &mut frame); // Only 3 lines, no panic
    }

    #[test]
    fn is_not_essential() {
        let widget = Pretty::new(&42);
        assert!(!widget.is_essential());
    }

    #[test]
    fn format_empty_vec() {
        let data: Vec<i32> = vec![];
        let widget = Pretty::new(&data);
        assert_eq!(widget.formatted_text(), "[]");
    }

    #[test]
    fn format_nested() {
        let data = vec![vec![1, 2], vec![3, 4]];
        let widget = Pretty::new(&data);
        let text = widget.formatted_text();
        assert!(text.lines().count() > 1);
    }

    #[test]
    fn format_option() {
        let some: Option<i32> = Some(42);
        let none: Option<i32> = None;
        assert!(Pretty::new(&some).formatted_text().contains("42"));
        assert!(Pretty::new(&none).formatted_text().contains("None"));
    }
}

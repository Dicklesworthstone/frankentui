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
use ftui_a11y::node::{A11yNodeInfo, A11yRole};
use ftui_core::geometry::Rect;
use ftui_render::cell::Cell;
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

        let deg = frame.buffer.degradation;
        if !deg.render_content() {
            frame.buffer.fill(area, Cell::default());
            return;
        }

        let style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };

        let text = self.formatted_text();
        let max_x = area.right();
        frame.buffer.fill(area, Cell::default());

        for (row_idx, line) in text.lines().enumerate() {
            if row_idx >= area.height as usize {
                break;
            }
            let y = area.y.saturating_add(row_idx as u16);
            draw_text_span(frame, area.x, y, line, style, max_x);
        }

        if frame.a11y_enabled() {
            let bounds = frame.buffer.current_scissor().intersection(&area);
            if !bounds.is_empty() {
                let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let mut chars = collapsed.chars();
                let truncated: String = chars.by_ref().take(200).collect();
                let desc = if chars.next().is_some() {
                    format!("{truncated}...")
                } else {
                    truncated
                };
                let mut node = A11yNodeInfo::new(
                    A11yNodeInfo::stable_id_for(A11yRole::Label, area),
                    A11yRole::Label,
                    bounds,
                )
                .with_name("Pretty");
                if !desc.is_empty() {
                    node = node.with_description(desc);
                }
                frame.push_a11y(node);
            }
        }
    }

    fn is_essential(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::budget::DegradationLevel;
    use ftui_render::cell::PackedRgba;
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
    #[allow(dead_code)]
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
    fn render_no_styling_drops_configured_style() {
        let widget =
            Pretty::new(&42).with_style(Style::new().fg(PackedRgba::rgb(255, 0, 0)).bold());

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::NoStyling;
        let area = Rect::new(0, 0, 20, 1);
        widget.render(area, &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        let default_cell = ftui_render::cell::Cell::from_char('4');
        assert_eq!(cell.content.as_char(), Some('4'));
        assert_eq!(cell.fg, default_cell.fg);
        assert_eq!(cell.bg, default_cell.bg);
        assert_eq!(cell.attrs, default_cell.attrs);
    }

    #[test]
    fn render_skeleton_is_noop() {
        let widget = Pretty::new(&42);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        let area = Rect::new(0, 0, 20, 1);
        widget.render(area, &mut frame);

        frame.buffer.degradation = DegradationLevel::Skeleton;
        widget.render(area, &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        let default_cell = ftui_render::cell::Cell::default();
        assert_eq!(cell.content, default_cell.content);
        assert_eq!(cell.fg, default_cell.fg);
        assert_eq!(cell.bg, default_cell.bg);
        assert_eq!(cell.attrs, default_cell.attrs);
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
    fn render_shorter_line_clears_stale_suffix() {
        let long_value = vec![1000];
        let short_value = vec![1];
        let long = Pretty::new(&long_value).with_compact(true);
        let short = Pretty::new(&short_value).with_compact(true);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 2, &mut pool);
        let area = Rect::new(0, 0, 20, 2);

        long.render(area, &mut frame);
        short.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('['));
        assert_eq!(frame.buffer.get(1, 0).unwrap().content.as_char(), Some('1'));
        assert_eq!(frame.buffer.get(2, 0).unwrap().content.as_char(), Some(']'));
        assert!(frame.buffer.get(3, 0).unwrap().is_empty());
    }

    #[test]
    fn render_fewer_lines_clears_stale_rows() {
        let long_value = vec![1, 2, 3];
        let long = Pretty::new(&long_value);
        let short = Pretty::new(&42);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 6, &mut pool);
        let area = Rect::new(0, 0, 20, 6);

        long.render(area, &mut frame);
        short.render(area, &mut frame);

        for x in 0..20u16 {
            assert!(frame.buffer.get(x, 1).unwrap().is_empty());
        }
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

#![forbid(unsafe_code)]

//! Status line widget for agent harness UIs.
//!
//! Provides a horizontal status bar with left, center, and right regions
//! that can contain text, spinners, progress indicators, and key hints.
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::status_line::{StatusLine, StatusItem};
//!
//! let status = StatusLine::new()
//!     .left(StatusItem::text("[INSERT]"))
//!     .center(StatusItem::text("file.rs"))
//!     .right(StatusItem::key_hint("^C", "Quit"))
//!     .right(StatusItem::text("Ln 42, Col 10"));
//! ```

use crate::{Widget, apply_style, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_style::Style;
use unicode_width::UnicodeWidthStr;

/// An item that can be displayed in the status line.
#[derive(Debug, Clone)]
pub enum StatusItem<'a> {
    /// Plain text.
    Text(&'a str),
    /// A spinner showing activity (references spinner state by index).
    Spinner(usize),
    /// A progress indicator showing current/total.
    Progress { current: u64, total: u64 },
    /// A key hint showing a key and its action.
    KeyHint { key: &'a str, action: &'a str },
    /// A flexible spacer that expands to fill available space.
    Spacer,
}

impl<'a> StatusItem<'a> {
    /// Create a text item.
    pub const fn text(s: &'a str) -> Self {
        Self::Text(s)
    }

    /// Create a key hint item.
    pub const fn key_hint(key: &'a str, action: &'a str) -> Self {
        Self::KeyHint { key, action }
    }

    /// Create a progress item.
    pub const fn progress(current: u64, total: u64) -> Self {
        Self::Progress { current, total }
    }

    /// Create a spacer item.
    pub const fn spacer() -> Self {
        Self::Spacer
    }

    /// Calculate the display width of this item.
    fn width(&self) -> usize {
        match self {
            Self::Text(s) => UnicodeWidthStr::width(*s),
            Self::Spinner(_) => 1, // Single char spinner
            Self::Progress { current, total } => {
                // Format: "42/100" or "100%"
                let pct = if *total > 0 {
                    (*current * 100) / *total
                } else {
                    0
                };
                format!("{pct}%").len()
            }
            Self::KeyHint { key, action } => {
                // Format: "^C Quit"
                UnicodeWidthStr::width(*key) + 1 + UnicodeWidthStr::width(*action)
            }
            Self::Spacer => 0, // Spacer has no fixed width
        }
    }

    /// Render this item to a string.
    fn render_to_string(&self) -> String {
        match self {
            Self::Text(s) => (*s).to_string(),
            Self::Spinner(idx) => {
                // Simple spinner frames
                const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                FRAMES[*idx % FRAMES.len()].to_string()
            }
            Self::Progress { current, total } => {
                let pct = if *total > 0 {
                    (*current * 100) / *total
                } else {
                    0
                };
                format!("{pct}%")
            }
            Self::KeyHint { key, action } => {
                format!("{key} {action}")
            }
            Self::Spacer => String::new(),
        }
    }
}

/// A status line widget with left, center, and right regions.
#[derive(Debug, Clone, Default)]
pub struct StatusLine<'a> {
    left: Vec<StatusItem<'a>>,
    center: Vec<StatusItem<'a>>,
    right: Vec<StatusItem<'a>>,
    style: Style,
    separator: &'a str,
}

impl<'a> StatusLine<'a> {
    /// Create a new empty status line.
    pub fn new() -> Self {
        Self {
            left: Vec::new(),
            center: Vec::new(),
            right: Vec::new(),
            style: Style::default(),
            separator: " ",
        }
    }

    /// Add an item to the left region.
    pub fn left(mut self, item: StatusItem<'a>) -> Self {
        self.left.push(item);
        self
    }

    /// Add an item to the center region.
    pub fn center(mut self, item: StatusItem<'a>) -> Self {
        self.center.push(item);
        self
    }

    /// Add an item to the right region.
    pub fn right(mut self, item: StatusItem<'a>) -> Self {
        self.right.push(item);
        self
    }

    /// Set the overall style for the status line.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the separator between items (default: " ").
    pub fn separator(mut self, separator: &'a str) -> Self {
        self.separator = separator;
        self
    }

    /// Calculate total width needed for a list of items.
    fn items_width(&self, items: &[StatusItem]) -> usize {
        if items.is_empty() {
            return 0;
        }
        let sep_width = UnicodeWidthStr::width(self.separator);
        items
            .iter()
            .map(|item| item.width())
            .sum::<usize>()
            + sep_width * items.len().saturating_sub(1)
    }

    /// Render a list of items starting at x position.
    fn render_items(
        &self,
        frame: &mut Frame,
        items: &[StatusItem],
        mut x: u16,
        y: u16,
        max_x: u16,
        style: Style,
    ) -> u16 {
        let sep_width = UnicodeWidthStr::width(self.separator) as u16;

        for (i, item) in items.iter().enumerate() {
            if x >= max_x {
                break;
            }

            // Add separator between items
            if i > 0 && !self.separator.is_empty() {
                x = draw_text_span(frame, x, y, self.separator, style, max_x);
            }

            if x >= max_x {
                break;
            }

            // Skip spacers in rendering (they're only for layout calculation)
            if matches!(item, StatusItem::Spacer) {
                continue;
            }

            let text = item.render_to_string();
            x = draw_text_span(frame, x, y, &text, style, max_x);
        }

        x
    }
}

impl Widget for StatusLine<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "StatusLine",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() || area.height < 1 {
            return;
        }

        let deg = frame.buffer.degradation;

        // StatusLine is essential (user needs to see status)
        if !deg.render_content() {
            return;
        }

        let style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };

        // Fill the background
        for x in area.x..area.right() {
            let mut cell = Cell::from_char(' ');
            apply_style(&mut cell, style);
            frame.buffer.set(x, area.y, cell);
        }

        let width = area.width as usize;
        let left_width = self.items_width(&self.left);
        let center_width = self.items_width(&self.center);
        let right_width = self.items_width(&self.right);

        // Calculate positions
        let left_x = area.x;
        let right_x = area.right().saturating_sub(right_width as u16);
        let center_x = if center_width > 0 {
            // Center the center items in the available space
            let available_center = width.saturating_sub(left_width).saturating_sub(right_width);
            let center_start = left_width + available_center.saturating_sub(center_width) / 2;
            area.x.saturating_add(center_start as u16)
        } else {
            area.x
        };

        // Render left items
        if !self.left.is_empty() {
            self.render_items(frame, &self.left, left_x, area.y, area.right(), style);
        }

        // Render center items (if they fit)
        if !self.center.is_empty() && center_x + (center_width as u16) <= right_x {
            self.render_items(frame, &self.center, center_x, area.y, right_x, style);
        }

        // Render right items
        if !self.right.is_empty() && right_x >= area.x {
            self.render_items(frame, &self.right, right_x, area.y, area.right(), style);
        }
    }

    fn is_essential(&self) -> bool {
        true // Status line should always render
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::buffer::Buffer;
    use ftui_render::cell::PackedRgba;
    use ftui_render::grapheme_pool::GraphemePool;

    fn row_string(buf: &Buffer, y: u16, width: u16) -> String {
        (0..width)
            .map(|x| {
                buf.get(x, y)
                    .and_then(|c| c.content.as_char())
                    .unwrap_or(' ')
            })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn empty_status_line() {
        let status = StatusLine::new();
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        status.render(area, &mut frame);

        // Should just be spaces
        let s = row_string(&frame.buffer, 0, 20);
        assert!(s.is_empty() || s.chars().all(|c| c == ' '));
    }

    #[test]
    fn left_only() {
        let status = StatusLine::new().left(StatusItem::text("[INSERT]"));
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        status.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(s.starts_with("[INSERT]"), "Got: '{s}'");
    }

    #[test]
    fn right_only() {
        let status = StatusLine::new().right(StatusItem::text("Ln 42"));
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        status.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(s.ends_with("Ln 42"), "Got: '{s}'");
    }

    #[test]
    fn center_only() {
        let status = StatusLine::new().center(StatusItem::text("file.rs"));
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        status.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(s.contains("file.rs"), "Got: '{s}'");
        // Should be roughly centered
        let pos = s.find("file.rs").unwrap();
        assert!(pos > 2 && pos < 15, "Not centered, pos={pos}, got: '{s}'");
    }

    #[test]
    fn all_three_regions() {
        let status = StatusLine::new()
            .left(StatusItem::text("L"))
            .center(StatusItem::text("C"))
            .right(StatusItem::text("R"));
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        status.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(s.starts_with("L"), "Got: '{s}'");
        assert!(s.ends_with("R"), "Got: '{s}'");
        assert!(s.contains("C"), "Got: '{s}'");
    }

    #[test]
    fn key_hint() {
        let status = StatusLine::new().left(StatusItem::key_hint("^C", "Quit"));
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        status.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(s.contains("^C Quit"), "Got: '{s}'");
    }

    #[test]
    fn progress() {
        let status = StatusLine::new().left(StatusItem::progress(50, 100));
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        status.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(s.contains("50%"), "Got: '{s}'");
    }

    #[test]
    fn multiple_items_left() {
        let status = StatusLine::new()
            .left(StatusItem::text("A"))
            .left(StatusItem::text("B"))
            .left(StatusItem::text("C"));
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        status.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(s.starts_with("A B C"), "Got: '{s}'");
    }

    #[test]
    fn custom_separator() {
        let status = StatusLine::new()
            .separator(" | ")
            .left(StatusItem::text("A"))
            .left(StatusItem::text("B"));
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        status.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(s.contains("A | B"), "Got: '{s}'");
    }

    #[test]
    fn style_applied() {
        let fg = PackedRgba::rgb(255, 0, 0);
        let status = StatusLine::new()
            .style(Style::new().fg(fg))
            .left(StatusItem::text("X"));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        status.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().fg, fg);
    }

    #[test]
    fn is_essential() {
        let status = StatusLine::new();
        assert!(status.is_essential());
    }

    #[test]
    fn zero_area_no_panic() {
        let status = StatusLine::new().left(StatusItem::text("Test"));
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        status.render(area, &mut frame);
        // Should not panic
    }

    #[test]
    fn truncation_when_too_narrow() {
        let status = StatusLine::new()
            .left(StatusItem::text("VERYLONGTEXT"))
            .right(StatusItem::text("R"));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        status.render(area, &mut frame);

        // Should render what fits without panicking
        let s = row_string(&frame.buffer, 0, 10);
        assert!(!s.is_empty(), "Got empty string");
    }
}

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
use ftui_text::display_width;

/// An item that can be displayed in the status line.
#[derive(Debug, Clone)]
pub enum StatusItem<'a> {
    /// Plain text.
    Text(&'a str),
    /// A spinner showing activity (references spinner state by index).
    Spinner(usize),
    /// A progress indicator showing current/total.
    Progress {
        /// Current progress value.
        current: u64,
        /// Total progress value.
        total: u64,
    },
    /// A key hint showing a key and its action.
    KeyHint {
        /// Key binding label.
        key: &'a str,
        /// Description of the action.
        action: &'a str,
    },
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
            Self::Text(s) => display_width(s),
            Self::Spinner(_) => 1, // Single char spinner
            Self::Progress { current, total } => {
                // Format: "42/100" or "100%"
                let pct = current.saturating_mul(100).checked_div(*total).unwrap_or(0);
                format!("{pct}%").len()
            }
            Self::KeyHint { key, action } => {
                // Format: "^C Quit"
                display_width(key) + 1 + display_width(action)
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
                let pct = current.saturating_mul(100).checked_div(*total).unwrap_or(0);
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
    #[must_use]
    pub fn left(mut self, item: StatusItem<'a>) -> Self {
        self.left.push(item);
        self
    }

    /// Add an item to the center region.
    #[must_use]
    pub fn center(mut self, item: StatusItem<'a>) -> Self {
        self.center.push(item);
        self
    }

    /// Add an item to the right region.
    #[must_use]
    pub fn right(mut self, item: StatusItem<'a>) -> Self {
        self.right.push(item);
        self
    }

    /// Set the overall style for the status line.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the separator between items (default: " ").
    #[must_use]
    pub fn separator(mut self, separator: &'a str) -> Self {
        self.separator = separator;
        self
    }

    /// Calculate total fixed width (non-spacers, with separators between non-spacers).
    fn items_fixed_width(&self, items: &[StatusItem]) -> usize {
        let sep_width = display_width(self.separator);
        let mut width = 0usize;
        let mut prev_item = false;

        for item in items {
            if matches!(item, StatusItem::Spacer) {
                prev_item = false;
                continue;
            }

            if prev_item {
                width += sep_width;
            }
            width += item.width();
            prev_item = true;
        }

        width
    }

    /// Count flexible spacers in an item list.
    fn spacer_count(&self, items: &[StatusItem]) -> usize {
        items
            .iter()
            .filter(|item| matches!(item, StatusItem::Spacer))
            .count()
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
        let available = max_x.saturating_sub(x) as usize;
        let fixed_width = self.items_fixed_width(items);
        let spacers = self.spacer_count(items);
        let extra = available.saturating_sub(fixed_width);
        let per_spacer = extra.checked_div(spacers).unwrap_or(0);
        let mut remainder = extra.checked_rem(spacers).unwrap_or(0);
        let mut prev_item = false;

        for item in items {
            if x >= max_x {
                break;
            }

            if matches!(item, StatusItem::Spacer) {
                let mut space = per_spacer;
                if remainder > 0 {
                    space += 1;
                    remainder -= 1;
                }
                let advance = (space as u16).min(max_x.saturating_sub(x));
                x = x.saturating_add(advance);
                prev_item = false;
                continue;
            }

            // Add separator between non-spacer items
            if prev_item && !self.separator.is_empty() {
                x = draw_text_span(frame, x, y, self.separator, style, max_x);
                if x >= max_x {
                    break;
                }
            }

            let text = item.render_to_string();
            x = draw_text_span(frame, x, y, &text, style, max_x);
            prev_item = true;
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
            frame.buffer.set_fast(x, area.y, cell);
        }

        let width = area.width as usize;
        let left_width = self.items_fixed_width(&self.left);
        let center_width = self.items_fixed_width(&self.center);
        let right_width = self.items_fixed_width(&self.right);
        let center_spacers = self.spacer_count(&self.center);

        // Calculate positions
        let left_x = area.x;
        let right_x = area.right().saturating_sub(right_width as u16);
        let available_center = width.saturating_sub(left_width).saturating_sub(right_width);
        let center_target_width = if center_width > 0 && center_spacers > 0 {
            available_center
        } else {
            center_width
        };
        let center_x = if center_width > 0 || center_spacers > 0 {
            // Center the center items in the available space
            let center_start =
                left_width + available_center.saturating_sub(center_target_width) / 2;
            area.x.saturating_add(center_start as u16)
        } else {
            area.x
        };

        let center_can_render = (center_width > 0 || center_spacers > 0)
            && center_x + center_target_width as u16 <= right_x;
        let left_max_x = if center_can_render { center_x } else { right_x };

        // Render left items
        if !self.left.is_empty() {
            self.render_items(frame, &self.left, left_x, area.y, left_max_x, style);
        }

        // Render center items (if they fit)
        if center_can_render {
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

    fn row_full(buf: &Buffer, y: u16, width: u16) -> String {
        (0..width)
            .map(|x| {
                buf.get(x, y)
                    .and_then(|c| c.content.as_char())
                    .unwrap_or(' ')
            })
            .collect()
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
    fn spacer_expands_and_skips_separators() {
        let status = StatusLine::new()
            .separator(" | ")
            .left(StatusItem::text("L"))
            .left(StatusItem::spacer())
            .left(StatusItem::text("R"));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        status.render(area, &mut frame);

        let row = row_full(&frame.buffer, 0, 10);
        let chars: Vec<char> = row.chars().collect();
        assert_eq!(chars[0], 'L');
        assert_eq!(chars[9], 'R');
        assert!(
            !row.contains('|'),
            "Spacer should skip separators, got: '{row}'"
        );
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
    fn spinner_renders_braille_char() {
        let status = StatusLine::new().left(StatusItem::Spinner(0));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        status.render(area, &mut frame);

        let c = frame
            .buffer
            .get(0, 0)
            .and_then(|c| c.content.as_char())
            .unwrap();
        assert_eq!(c, '⠋');
    }

    #[test]
    fn spinner_cycles_through_frames() {
        // Frame index wraps modulo 10
        let item0 = StatusItem::Spinner(0);
        let item10 = StatusItem::Spinner(10);
        assert_eq!(item0.render_to_string(), item10.render_to_string());

        let item1 = StatusItem::Spinner(1);
        assert_ne!(item0.render_to_string(), item1.render_to_string());
    }

    #[test]
    fn spinner_width_is_one() {
        let item = StatusItem::Spinner(5);
        assert_eq!(item.width(), 1);
    }

    #[test]
    fn progress_zero_total_shows_zero_percent() {
        let item = StatusItem::progress(50, 0);
        assert_eq!(item.render_to_string(), "0%");
    }

    #[test]
    fn spacer_width_is_zero() {
        assert_eq!(StatusItem::spacer().width(), 0);
    }

    #[test]
    fn spacer_render_to_string_is_empty() {
        assert_eq!(StatusItem::spacer().render_to_string(), "");
    }

    #[test]
    fn status_line_default_is_empty() {
        let status = StatusLine::default();
        assert!(status.left.is_empty());
        assert!(status.center.is_empty());
        assert!(status.right.is_empty());
        assert_eq!(status.separator, "");
    }

    #[test]
    fn multiple_items_right() {
        let status = StatusLine::new()
            .right(StatusItem::text("X"))
            .right(StatusItem::text("Y"));
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        status.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(s.contains("X Y"), "Got: '{s}'");
    }

    #[test]
    fn key_hint_width() {
        let item = StatusItem::key_hint("^C", "Quit");
        // "^C" = 2 + " " = 1 + "Quit" = 4 = 7
        assert_eq!(item.width(), 7);
    }

    #[test]
    fn progress_full_hundred_percent() {
        let item = StatusItem::progress(100, 100);
        assert_eq!(item.render_to_string(), "100%");
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

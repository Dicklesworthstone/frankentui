#![forbid(unsafe_code)]

//! Horizontal rule (divider) widget.
//!
//! Draws a horizontal line across the available width, optionally with a
//! title that can be aligned left, center, or right.

use crate::block::Alignment;
use crate::borders::BorderType;
use crate::measurable::{MeasurableWidget, SizeConstraints};
use crate::{Widget, apply_style, draw_text_span};
use ftui_core::geometry::{Rect, Size};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::display_width;

/// A horizontal rule / divider.
///
/// Renders a single-row horizontal line using a border character, optionally
/// with a title inset at the given alignment.
///
/// # Examples
///
/// ```ignore
/// use ftui_widgets::rule::Rule;
/// use ftui_widgets::block::Alignment;
///
/// // Simple divider
/// let rule = Rule::new();
///
/// // Titled divider, centered
/// let rule = Rule::new()
///     .title("Section")
///     .title_alignment(Alignment::Center);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule<'a> {
    /// Optional title text.
    title: Option<&'a str>,
    /// Title alignment.
    title_alignment: Alignment,
    /// Style for the rule line characters.
    style: Style,
    /// Style for the title text (if different from rule style).
    title_style: Option<Style>,
    /// Border type determining the line character.
    border_type: BorderType,
}

impl<'a> Default for Rule<'a> {
    fn default() -> Self {
        Self {
            title: None,
            title_alignment: Alignment::Center,
            style: Style::default(),
            title_style: None,
            border_type: BorderType::Square,
        }
    }
}

impl<'a> Rule<'a> {
    /// Create a new rule with default settings (square horizontal line, no title).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the title text.
    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    /// Set the title alignment.
    pub fn title_alignment(mut self, alignment: Alignment) -> Self {
        self.title_alignment = alignment;
        self
    }

    /// Set the style for the rule line.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set a separate style for the title text.
    ///
    /// If not set, the rule's main style is used for the title.
    pub fn title_style(mut self, style: Style) -> Self {
        self.title_style = Some(style);
        self
    }

    /// Set the border type (determines the line character).
    pub fn border_type(mut self, border_type: BorderType) -> Self {
        self.border_type = border_type;
        self
    }

    /// Fill a range of cells with the rule character.
    fn fill_rule_char(&self, buf: &mut Buffer, y: u16, start: u16, end: u16) {
        let ch = if buf.degradation.use_unicode_borders() {
            self.border_type.to_border_set().horizontal
        } else {
            '-' // ASCII fallback
        };
        let style = if buf.degradation.apply_styling() {
            self.style
        } else {
            Style::default()
        };
        for x in start..end {
            let mut cell = Cell::from_char(ch);
            apply_style(&mut cell, style);
            buf.set_fast(x, y, cell);
        }
    }
}

impl Widget for Rule<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Rule",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() {
            return;
        }

        // Rule is decorative — skip at EssentialOnly+
        if !frame.buffer.degradation.render_decorative() {
            return;
        }

        let y = area.y;
        let width = area.width;

        match self.title {
            None => {
                // No title: fill the entire width with rule characters.
                self.fill_rule_char(&mut frame.buffer, y, area.x, area.right());
            }
            Some("") => self.fill_rule_char(&mut frame.buffer, y, area.x, area.right()),
            Some(title) => {
                let title_width = display_width(title) as u16;

                // Need at least 1 char of padding on each side of the title,
                // plus the title itself. If the area is too narrow, just draw
                // the rule without a title.
                let min_width_for_title = title_width.saturating_add(2);
                if width < min_width_for_title || width < 3 {
                    // Too narrow for title + padding; fall back to plain rule.
                    // If title fits exactly, truncate and show just the rule.
                    if title_width > width {
                        // Title doesn't even fit; just draw the rule line.
                        self.fill_rule_char(&mut frame.buffer, y, area.x, area.right());
                    } else {
                        // Title fits but no room for rule chars; show truncated title.
                        let ts = self.title_style.unwrap_or(self.style);
                        draw_text_span(frame, area.x, y, title, ts, area.right());
                        // Fill remaining with rule
                        let after = area.x.saturating_add(title_width);
                        self.fill_rule_char(&mut frame.buffer, y, after, area.right());
                    }
                    return;
                }

                // Truncate title if it won't fit with padding.
                let max_title_width = width.saturating_sub(2);
                let display_width = title_width.min(max_title_width);

                // Calculate where the title block starts (including 1-char pad on each side).
                let title_block_width = display_width + 2; // pad + title + pad
                let title_block_x = match self.title_alignment {
                    Alignment::Left => area.x,
                    Alignment::Center => area
                        .x
                        .saturating_add((width.saturating_sub(title_block_width)) / 2),
                    Alignment::Right => area.right().saturating_sub(title_block_width),
                };

                // Draw left rule section.
                self.fill_rule_char(&mut frame.buffer, y, area.x, title_block_x);

                // Draw left padding space.
                let pad_x = title_block_x;
                if let Some(cell) = frame.buffer.get_mut(pad_x, y) {
                    *cell = Cell::from_char(' ');
                    apply_style(cell, self.style);
                }

                // Draw title text.
                let ts = self.title_style.unwrap_or(self.style);
                let title_x = pad_x.saturating_add(1);
                let title_end = title_x.saturating_add(display_width);
                draw_text_span(frame, title_x, y, title, ts, title_end);

                // Draw right padding space.
                let right_pad_x = title_end;
                if right_pad_x < area.right()
                    && let Some(cell) = frame.buffer.get_mut(right_pad_x, y)
                {
                    *cell = Cell::from_char(' ');
                    apply_style(cell, self.style);
                }

                // Draw right rule section.
                let right_rule_start = right_pad_x.saturating_add(1);
                self.fill_rule_char(&mut frame.buffer, y, right_rule_start, area.right());
            }
        }
    }
}

impl MeasurableWidget for Rule<'_> {
    fn measure(&self, _available: Size) -> SizeConstraints {
        // Rule is always exactly 1 cell tall
        // Minimum width is 1 (single rule char), preferred depends on title
        let min_width = 1u16;

        let preferred_width = if let Some(title) = self.title {
            // Title + padding (1 space on each side) + at least 2 rule chars
            let title_width = display_width(title) as u16;
            title_width.saturating_add(4) // title + 2 spaces + 2 rule chars minimum
        } else {
            1 // Just a single rule char is fine
        };

        SizeConstraints {
            min: Size::new(min_width, 1),
            preferred: Size::new(preferred_width, 1),
            max: Some(Size::new(u16::MAX, 1)), // Fixed height of 1
        }
    }

    fn has_intrinsic_size(&self) -> bool {
        // Rule always has intrinsic height of 1
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    /// Helper: extract row content as chars from a buffer.
    fn row_chars(buf: &Buffer, y: u16, width: u16) -> Vec<char> {
        (0..width)
            .map(|x| {
                buf.get(x, y)
                    .and_then(|c| c.content.as_char())
                    .unwrap_or(' ')
            })
            .collect()
    }

    /// Helper: row content as a String (trimming trailing spaces).
    fn row_string(buf: &Buffer, y: u16, width: u16) -> String {
        let chars: String = row_chars(buf, y, width).into_iter().collect();
        chars.trim_end().to_string()
    }

    // --- No-title tests ---

    #[test]
    fn no_title_fills_width() {
        let rule = Rule::new();
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        rule.render(area, &mut frame);

        let row = row_chars(&frame.buffer, 0, 10);
        assert!(
            row.iter().all(|&c| c == '─'),
            "Expected all ─, got: {row:?}"
        );
    }

    #[test]
    fn no_title_heavy_border() {
        let rule = Rule::new().border_type(BorderType::Heavy);
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        rule.render(area, &mut frame);

        let row = row_chars(&frame.buffer, 0, 5);
        assert!(
            row.iter().all(|&c| c == '━'),
            "Expected all ━, got: {row:?}"
        );
    }

    #[test]
    fn no_title_double_border() {
        let rule = Rule::new().border_type(BorderType::Double);
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        rule.render(area, &mut frame);

        let row = row_chars(&frame.buffer, 0, 5);
        assert!(
            row.iter().all(|&c| c == '═'),
            "Expected all ═, got: {row:?}"
        );
    }

    #[test]
    fn no_title_ascii_border() {
        let rule = Rule::new().border_type(BorderType::Ascii);
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        rule.render(area, &mut frame);

        let row = row_chars(&frame.buffer, 0, 5);
        assert!(
            row.iter().all(|&c| c == '-'),
            "Expected all -, got: {row:?}"
        );
    }

    // --- Titled tests ---

    #[test]
    fn title_center_default() {
        let rule = Rule::new().title("Hi");
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        rule.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(
            s.contains(" Hi "),
            "Expected centered title with spaces, got: '{s}'"
        );
        assert!(s.contains('─'), "Expected rule chars, got: '{s}'");
    }

    #[test]
    fn title_left_aligned() {
        let rule = Rule::new().title("Hi").title_alignment(Alignment::Left);
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        rule.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(
            s.starts_with(" Hi "),
            "Left-aligned should start with ' Hi ', got: '{s}'"
        );
    }

    #[test]
    fn title_right_aligned() {
        let rule = Rule::new().title("Hi").title_alignment(Alignment::Right);
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        rule.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(
            s.ends_with(" Hi"),
            "Right-aligned should end with ' Hi', got: '{s}'"
        );
    }

    #[test]
    fn title_truncated_at_narrow_width() {
        // Title "Hello" is 5 chars, needs 7 with padding. Width is 7 exactly.
        let rule = Rule::new().title("Hello");
        let area = Rect::new(0, 0, 7, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(7, 1, &mut pool);
        rule.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 7);
        assert!(s.contains("Hello"), "Title should be present, got: '{s}'");
    }

    #[test]
    fn title_too_wide_falls_back_to_rule() {
        // Title "VeryLongTitle" is 13 chars, area is 5 wide. Can't fit.
        let rule = Rule::new().title("VeryLongTitle");
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        rule.render(area, &mut frame);

        let row = row_chars(&frame.buffer, 0, 5);
        // Should fall back to plain rule since title doesn't fit
        assert!(
            row.iter().all(|&c| c == '─'),
            "Expected fallback to rule, got: {row:?}"
        );
    }

    #[test]
    fn empty_title_same_as_no_title() {
        let rule = Rule::new().title("");
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        rule.render(area, &mut frame);

        let row = row_chars(&frame.buffer, 0, 10);
        assert!(
            row.iter().all(|&c| c == '─'),
            "Empty title should be plain rule, got: {row:?}"
        );
    }

    // --- Edge cases ---

    #[test]
    fn zero_width_no_panic() {
        let rule = Rule::new().title("Test");
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        rule.render(area, &mut frame);
        // Should not panic
    }

    #[test]
    fn width_one_no_title() {
        let rule = Rule::new();
        let area = Rect::new(0, 0, 1, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        rule.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('─'));
    }

    #[test]
    fn width_two_with_title() {
        // Width 2, title "X" (1 char). min_width_for_title = 3. Falls back.
        let rule = Rule::new().title("X");
        let area = Rect::new(0, 0, 2, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 1, &mut pool);
        rule.render(area, &mut frame);

        // Title "X" fits in 2 but no room for padding; should show "X" + rule or just rule
        let s = row_string(&frame.buffer, 0, 2);
        assert!(!s.is_empty(), "Should render something, got empty");
    }

    #[test]
    fn offset_area() {
        // Rule rendered at a non-zero origin.
        let rule = Rule::new();
        let area = Rect::new(5, 3, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 5, &mut pool);
        rule.render(area, &mut frame);

        // Cells before the area should be untouched (space/default)
        assert_ne!(frame.buffer.get(4, 3).unwrap().content.as_char(), Some('─'));
        // Cells in the area should be rule chars
        assert_eq!(frame.buffer.get(5, 3).unwrap().content.as_char(), Some('─'));
        assert_eq!(
            frame.buffer.get(14, 3).unwrap().content.as_char(),
            Some('─')
        );
        // Cell after the area should be untouched
        assert_ne!(
            frame.buffer.get(15, 3).unwrap().content.as_char(),
            Some('─')
        );
    }

    #[test]
    fn style_applied_to_rule_chars() {
        use ftui_render::cell::PackedRgba;

        let fg = PackedRgba::rgb(255, 0, 0);
        let rule = Rule::new().style(Style::new().fg(fg));
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        rule.render(area, &mut frame);

        for x in 0..5 {
            assert_eq!(frame.buffer.get(x, 0).unwrap().fg, fg);
        }
    }

    #[test]
    fn title_style_distinct_from_rule_style() {
        use ftui_render::cell::PackedRgba;

        let rule_fg = PackedRgba::rgb(255, 0, 0);
        let title_fg = PackedRgba::rgb(0, 255, 0);
        let rule = Rule::new()
            .title("AB")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(rule_fg))
            .title_style(Style::new().fg(title_fg));
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        rule.render(area, &mut frame);

        // Find the title characters and check their fg
        let mut found_title = false;
        for x in 0..20u16 {
            if let Some(cell) = frame.buffer.get(x, 0)
                && cell.content.as_char() == Some('A')
            {
                assert_eq!(cell.fg, title_fg, "Title char should have title_fg");
                found_title = true;
            }
        }
        assert!(found_title, "Should have found title character 'A'");

        // Check that rule chars have rule_fg
        let first = frame.buffer.get(0, 0).unwrap();
        assert_eq!(first.content.as_char(), Some('─'));
        assert_eq!(first.fg, rule_fg, "Rule char should have rule_fg");
    }

    // --- Unicode title ---

    #[test]
    fn unicode_title() {
        // Japanese characters (each 2 cells wide)
        let rule = Rule::new().title("日本");
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        rule.render(area, &mut frame);

        let s = row_string(&frame.buffer, 0, 20);
        assert!(s.contains('─'), "Should contain rule chars, got: '{s}'");
        // The unicode title should be rendered somewhere in the middle.
        // Wide characters are stored as grapheme IDs, so we check for
        // non-empty cells with width > 1 (indicating a wide character).
        let mut found_wide = false;
        for x in 0..20u16 {
            if let Some(cell) = frame.buffer.get(x, 0)
                && !cell.is_empty()
                && cell.content.width() > 1
            {
                found_wide = true;
                break;
            }
        }
        assert!(found_wide, "Should have rendered unicode title (wide char)");
    }

    // --- Degradation tests ---

    #[test]
    fn degradation_essential_only_skips_entirely() {
        use ftui_render::budget::DegradationLevel;

        let rule = Rule::new();
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::EssentialOnly;
        rule.render(area, &mut frame);

        // Rule is decorative, skipped at EssentialOnly
        for x in 0..10u16 {
            assert!(
                frame.buffer.get(x, 0).unwrap().is_empty(),
                "cell at x={x} should be empty at EssentialOnly"
            );
        }
    }

    #[test]
    fn degradation_skeleton_skips_entirely() {
        use ftui_render::budget::DegradationLevel;

        let rule = Rule::new();
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::Skeleton;
        rule.render(area, &mut frame);

        for x in 0..10u16 {
            assert!(
                frame.buffer.get(x, 0).unwrap().is_empty(),
                "cell at x={x} should be empty at Skeleton"
            );
        }
    }

    #[test]
    fn degradation_simple_borders_uses_ascii() {
        use ftui_render::budget::DegradationLevel;

        let rule = Rule::new().border_type(BorderType::Square);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::SimpleBorders;
        rule.render(area, &mut frame);

        // Should use ASCII '-' instead of Unicode '─'
        let row = row_chars(&frame.buffer, 0, 10);
        assert!(
            row.iter().all(|&c| c == '-'),
            "Expected all -, got: {row:?}"
        );
    }

    #[test]
    fn degradation_full_uses_unicode() {
        use ftui_render::budget::DegradationLevel;

        let rule = Rule::new().border_type(BorderType::Square);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::Full;
        rule.render(area, &mut frame);

        let row = row_chars(&frame.buffer, 0, 10);
        assert!(
            row.iter().all(|&c| c == '─'),
            "Expected all ─, got: {row:?}"
        );
    }

    // --- MeasurableWidget tests ---

    use crate::MeasurableWidget;
    use ftui_core::geometry::Size;

    #[test]
    fn measure_no_title() {
        let rule = Rule::new();
        let constraints = rule.measure(Size::MAX);

        // Min is 1x1, preferred is 1x1, max height is 1
        assert_eq!(constraints.min, Size::new(1, 1));
        assert_eq!(constraints.preferred, Size::new(1, 1));
        assert_eq!(constraints.max, Some(Size::new(u16::MAX, 1)));
    }

    #[test]
    fn measure_with_title() {
        let rule = Rule::new().title("Test");
        let constraints = rule.measure(Size::MAX);

        // "Test" is 4 chars, plus 2 spaces padding, plus 2 rule chars = 8
        assert_eq!(constraints.min, Size::new(1, 1));
        assert_eq!(constraints.preferred, Size::new(8, 1));
        assert_eq!(constraints.max.unwrap().height, 1);
    }

    #[test]
    fn measure_with_long_title() {
        let rule = Rule::new().title("Very Long Title");
        let constraints = rule.measure(Size::MAX);

        // "Very Long Title" is 15 chars, + 4 = 19
        assert_eq!(constraints.preferred, Size::new(19, 1));
    }

    #[test]
    fn measure_fixed_height() {
        let rule = Rule::new().title("Hi");
        let constraints = rule.measure(Size::MAX);

        // Height is always exactly 1
        assert_eq!(constraints.min.height, 1);
        assert_eq!(constraints.preferred.height, 1);
        assert_eq!(constraints.max.unwrap().height, 1);
    }

    #[test]
    fn rule_has_intrinsic_size() {
        let rule = Rule::new();
        assert!(rule.has_intrinsic_size());
    }

    #[test]
    fn rule_measure_is_pure() {
        let rule = Rule::new().title("Hello");
        let a = rule.measure(Size::new(100, 50));
        let b = rule.measure(Size::new(100, 50));
        assert_eq!(a, b);
    }
}

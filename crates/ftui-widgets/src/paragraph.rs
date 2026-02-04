#![forbid(unsafe_code)]

use crate::block::{Alignment, Block};
use crate::measurable::{MeasurableWidget, SizeConstraints};
use crate::{Widget, draw_text_span_scrolled, draw_text_span_with_link, set_style_area};
use ftui_core::geometry::{Rect, Size};
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::{Text, WrapMode, display_width};

/// A widget that renders multi-line styled text.
#[derive(Debug, Clone, Default)]
pub struct Paragraph<'a> {
    text: Text,
    block: Option<Block<'a>>,
    style: Style,
    wrap: Option<WrapMode>,
    alignment: Alignment,
    scroll: (u16, u16),
}

impl<'a> Paragraph<'a> {
    /// Create a new paragraph from the given text.
    pub fn new(text: impl Into<Text>) -> Self {
        Self {
            text: text.into(),
            block: None,
            style: Style::default(),
            wrap: None,
            alignment: Alignment::Left,
            scroll: (0, 0),
        }
    }

    /// Set the surrounding block.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Set the base text style.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the text wrapping mode.
    pub fn wrap(mut self, wrap: WrapMode) -> Self {
        self.wrap = Some(wrap);
        self
    }

    /// Set the text alignment.
    pub fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Set the scroll offset as (vertical, horizontal).
    pub fn scroll(mut self, offset: (u16, u16)) -> Self {
        self.scroll = offset;
        self
    }
}

impl Widget for Paragraph<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Paragraph",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        let deg = frame.buffer.degradation;

        // Skeleton+: nothing to render
        if !deg.render_content() {
            return;
        }

        if deg.apply_styling() {
            set_style_area(&mut frame.buffer, area, self.style);
        }

        let text_area = match self.block {
            Some(ref b) => {
                b.render(area, frame);
                b.inner(area)
            }
            None => area,
        };

        if text_area.is_empty() {
            return;
        }

        // At NoStyling, render text without per-span styles
        let style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };
        // Background is already applied for the whole area via `set_style_area()`. When drawing
        // text we avoid re-applying the same background, otherwise semi-transparent BG colors
        // get composited multiple times.
        let mut text_style = style;
        text_style.bg = None;

        let mut y = text_area.y;
        let mut current_visual_line = 0;
        let scroll_offset = self.scroll.0 as usize;

        let mut render_line = |line: &ftui_text::Line, y: u16| {
            // Render spans with proper Unicode widths
            let line_width: usize = line.width();

            let scroll_x = self.scroll.1;
            let start_x = align_x(text_area, line_width, self.alignment);

            // Let's iterate spans.
            // `span_visual_offset`: relative to line start.
            let mut span_visual_offset = 0;

            // Alignment offset relative to text_area.x
            let alignment_offset = start_x.saturating_sub(text_area.x);

            for span in line.spans() {
                let span_width = span.width();

                // Effective position of this span relative to text_area.x
                // pos = alignment_offset + span_visual_offset - scroll_x
                let line_rel_start = alignment_offset.saturating_add(span_visual_offset);

                // Check visibility
                if line_rel_start.saturating_add(span_width as u16) <= scroll_x {
                    // Fully scrolled out to the left
                    span_visual_offset = span_visual_offset.saturating_add(span_width as u16);
                    continue;
                }

                // Calculate actual draw position
                let draw_x;
                let local_scroll;

                if line_rel_start < scroll_x {
                    // Partially scrolled out left
                    draw_x = text_area.x;
                    local_scroll = scroll_x - line_rel_start;
                } else {
                    // Start is visible
                    draw_x = text_area.x.saturating_add(line_rel_start - scroll_x);
                    local_scroll = 0;
                }

                if draw_x >= text_area.right() {
                    // Fully clipped to the right
                    break;
                }

                // At NoStyling+, ignore span-level styles entirely
                let span_style = if deg.apply_styling() {
                    match span.style {
                        Some(s) => s.merge(&text_style),
                        None => text_style,
                    }
                } else {
                    text_style // Style::default() at NoStyling
                };

                if local_scroll > 0 {
                    draw_text_span_scrolled(
                        frame,
                        draw_x,
                        y,
                        span.content.as_ref(),
                        span_style,
                        text_area.right(),
                        local_scroll,
                        span.link.as_deref(),
                    );
                } else {
                    draw_text_span_with_link(
                        frame,
                        draw_x,
                        y,
                        span.content.as_ref(),
                        span_style,
                        text_area.right(),
                        span.link.as_deref(),
                    );
                }

                span_visual_offset = span_visual_offset.saturating_add(span_width as u16);
            }
        };

        for line in self.text.lines() {
            if y >= text_area.bottom() {
                break;
            }

            // If wrapping is enabled and line is wider than area, wrap it
            if let Some(wrap_mode) = self.wrap {
                let line_width = line.width();
                if line_width > text_area.width as usize {
                    let wrapped = line.wrap(text_area.width as usize, wrap_mode);
                    for wrapped_line in &wrapped {
                        if current_visual_line < scroll_offset {
                            current_visual_line += 1;
                            continue;
                        }

                        if y >= text_area.bottom() {
                            break;
                        }

                        render_line(wrapped_line, y);
                        y += 1;
                        current_visual_line += 1;
                    }
                    continue;
                }
            }

            // Non-wrapped line (or fits in width)
            if current_visual_line < scroll_offset {
                current_visual_line += 1;
                continue;
            }

            render_line(line, y);
            y = y.saturating_add(1);
            current_visual_line += 1;
        }
    }
}
impl MeasurableWidget for Paragraph<'_> {
    fn measure(&self, available: Size) -> SizeConstraints {
        // Calculate text measurements
        let text_width = self.text.width();
        let text_height = self.text.height();

        // Find the minimum width (longest word or longest non-breakable segment)
        // This requires iterating through the text to find word boundaries
        let min_width = self.calculate_min_width();

        // Get block chrome if present
        let (chrome_width, chrome_height) = self
            .block
            .as_ref()
            .map(|b| b.chrome_size())
            .unwrap_or((0, 0));

        // If wrapping is enabled, calculate wrapped height
        let (preferred_width, preferred_height) = if self.wrap.is_some() {
            // When wrapping, preferred width is either the text width or available width
            let wrap_width = if available.width > chrome_width {
                (available.width - chrome_width) as usize
            } else {
                1
            };

            // Estimate wrapped height by calculating how text would wrap
            let wrapped_height = self.estimate_wrapped_height(wrap_width);

            // Preferred width is min(text_width, available_width - chrome)
            let pref_w = text_width.min(wrap_width);
            (pref_w, wrapped_height)
        } else {
            // No wrapping: preferred is natural text dimensions
            (text_width, text_height)
        };

        // Convert to u16, saturating at MAX
        let min_w = (min_width as u16).saturating_add(chrome_width);
        // Only require 1 line minimum if there's actual content
        let min_h = if preferred_height > 0 {
            (1u16).saturating_add(chrome_height)
        } else {
            chrome_height
        };

        let pref_w = (preferred_width as u16).saturating_add(chrome_width);
        let pref_h = (preferred_height as u16).saturating_add(chrome_height);

        SizeConstraints {
            min: Size::new(min_w, min_h),
            preferred: Size::new(pref_w, pref_h),
            max: None, // Paragraph can use additional space for scrolling
        }
    }

    fn has_intrinsic_size(&self) -> bool {
        // Paragraph always has intrinsic size based on its text content
        true
    }
}

impl Paragraph<'_> {
    /// Calculate the minimum width needed (longest word).
    fn calculate_min_width(&self) -> usize {
        let mut max_word_width = 0;

        for line in self.text.lines() {
            let plain = line.to_plain_text();
            // Split on whitespace to find words
            for word in plain.split_whitespace() {
                let word_width = display_width(word);
                max_word_width = max_word_width.max(word_width);
            }
        }

        // If there are no words, use the full text width
        if max_word_width == 0 {
            return self.text.width();
        }

        max_word_width
    }

    /// Estimate the height when text is wrapped at the given width.
    fn estimate_wrapped_height(&self, wrap_width: usize) -> usize {
        if wrap_width == 0 {
            return self.text.height();
        }

        let wrap_mode = self.wrap.unwrap_or(WrapMode::Word);
        let mut total_lines = 0;

        for line in self.text.lines() {
            let line_width = line.width();
            if wrap_mode == WrapMode::None || line_width <= wrap_width {
                total_lines += 1;
                continue;
            }

            // Wrap this line and count resulting lines (style-preserving path).
            let wrapped = line.wrap(wrap_width, wrap_mode);
            total_lines += wrapped.len().max(1);
        }

        total_lines.max(1)
    }
}

/// Calculate the starting x position for a line given alignment.
fn align_x(area: Rect, line_width: usize, alignment: Alignment) -> u16 {
    let line_width_u16 = u16::try_from(line_width).unwrap_or(u16::MAX);
    match alignment {
        Alignment::Left => area.x,
        Alignment::Center => area
            .x
            .saturating_add(area.width.saturating_sub(line_width_u16) / 2),
        Alignment::Right => area
            .x
            .saturating_add(area.width.saturating_sub(line_width_u16)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn render_simple_text() {
        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        para.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(frame.buffer.get(4, 0).unwrap().content.as_char(), Some('o'));
    }

    #[test]
    fn render_multiline_text() {
        let para = Paragraph::new(Text::raw("AB\nCD"));
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        para.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(1, 0).unwrap().content.as_char(), Some('B'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('C'));
        assert_eq!(frame.buffer.get(1, 1).unwrap().content.as_char(), Some('D'));
    }

    #[test]
    fn render_centered_text() {
        let para = Paragraph::new(Text::raw("Hi")).alignment(Alignment::Center);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        para.render(area, &mut frame);

        // "Hi" is 2 wide, area is 10, so starts at (10-2)/2 = 4
        assert_eq!(frame.buffer.get(4, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(frame.buffer.get(5, 0).unwrap().content.as_char(), Some('i'));
    }

    #[test]
    fn render_with_scroll() {
        let para = Paragraph::new(Text::raw("Line1\nLine2\nLine3")).scroll((1, 0));
        let area = Rect::new(0, 0, 10, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 2, &mut pool);
        para.render(area, &mut frame);

        // Should skip Line1, show Line2 and Line3
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('L'));
        assert_eq!(frame.buffer.get(4, 0).unwrap().content.as_char(), Some('2'));
    }

    #[test]
    fn render_empty_area() {
        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        para.render(area, &mut frame);
    }

    #[test]
    fn render_right_aligned() {
        let para = Paragraph::new(Text::raw("Hi")).alignment(Alignment::Right);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        para.render(area, &mut frame);

        // "Hi" is 2 wide, area is 10, so starts at 10-2 = 8
        assert_eq!(frame.buffer.get(8, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(frame.buffer.get(9, 0).unwrap().content.as_char(), Some('i'));
    }

    #[test]
    fn render_with_word_wrap() {
        let para = Paragraph::new(Text::raw("hello world")).wrap(WrapMode::Word);
        let area = Rect::new(0, 0, 6, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 3, &mut pool);
        para.render(area, &mut frame);

        // "hello " fits in 6, " world" wraps to next line
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('h'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('w'));
    }

    #[test]
    fn render_with_char_wrap() {
        let para = Paragraph::new(Text::raw("abcdefgh")).wrap(WrapMode::Char);
        let area = Rect::new(0, 0, 4, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 3, &mut pool);
        para.render(area, &mut frame);

        // First line: abcd
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('a'));
        assert_eq!(frame.buffer.get(3, 0).unwrap().content.as_char(), Some('d'));
        // Second line: efgh
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('e'));
    }

    #[test]
    fn scroll_past_all_lines() {
        let para = Paragraph::new(Text::raw("AB")).scroll((5, 0));
        let area = Rect::new(0, 0, 5, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 2, &mut pool);
        para.render(area, &mut frame);

        // All lines skipped, buffer should remain empty
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn render_clipped_at_area_height() {
        let para = Paragraph::new(Text::raw("A\nB\nC\nD\nE"));
        let area = Rect::new(0, 0, 5, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 2, &mut pool);
        para.render(area, &mut frame);

        // Only first 2 lines should render
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('B'));
    }

    #[test]
    fn render_clipped_at_area_width() {
        let para = Paragraph::new(Text::raw("ABCDEF"));
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        para.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(2, 0).unwrap().content.as_char(), Some('C'));
    }

    #[test]
    fn align_x_left() {
        let area = Rect::new(5, 0, 20, 1);
        assert_eq!(align_x(area, 10, Alignment::Left), 5);
    }

    #[test]
    fn align_x_center() {
        let area = Rect::new(0, 0, 20, 1);
        // line_width=6, area=20, so (20-6)/2 = 7
        assert_eq!(align_x(area, 6, Alignment::Center), 7);
    }

    #[test]
    fn align_x_right() {
        let area = Rect::new(0, 0, 20, 1);
        // line_width=5, area=20, so 20-5 = 15
        assert_eq!(align_x(area, 5, Alignment::Right), 15);
    }

    #[test]
    fn align_x_wide_line_saturates() {
        let area = Rect::new(0, 0, 10, 1);
        // line wider than area: should saturate to area.x
        assert_eq!(align_x(area, 20, Alignment::Right), 0);
        assert_eq!(align_x(area, 20, Alignment::Center), 0);
    }

    #[test]
    fn builder_methods_chain() {
        let para = Paragraph::new(Text::raw("test"))
            .style(Style::default())
            .wrap(WrapMode::Word)
            .alignment(Alignment::Center)
            .scroll((1, 2));
        // Verify it builds without panic
        let area = Rect::new(0, 0, 10, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        para.render(area, &mut frame);
    }

    #[test]
    fn render_at_offset_area() {
        let para = Paragraph::new(Text::raw("X"));
        let area = Rect::new(3, 4, 5, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        para.render(area, &mut frame);

        assert_eq!(frame.buffer.get(3, 4).unwrap().content.as_char(), Some('X'));
        // Cell at (0,0) should be empty
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn wrap_clipped_at_area_bottom() {
        // Long wrapped text should stop at area height
        let para = Paragraph::new(Text::raw("abcdefghijklmnop")).wrap(WrapMode::Char);
        let area = Rect::new(0, 0, 4, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 2, &mut pool);
        para.render(area, &mut frame);

        // Only 2 rows of 4 chars each
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('a'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('e'));
    }

    // --- Degradation tests ---

    #[test]
    fn degradation_skeleton_skips_content() {
        use ftui_render::budget::DegradationLevel;

        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.set_degradation(DegradationLevel::Skeleton);
        para.render(area, &mut frame);

        // No text should be rendered at Skeleton level
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn degradation_full_renders_content() {
        use ftui_render::budget::DegradationLevel;

        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.set_degradation(DegradationLevel::Full);
        para.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('H'));
    }

    #[test]
    fn degradation_essential_only_still_renders_text() {
        use ftui_render::budget::DegradationLevel;

        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.set_degradation(DegradationLevel::EssentialOnly);
        para.render(area, &mut frame);

        // EssentialOnly still renders content (< Skeleton)
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('H'));
    }

    #[test]
    fn degradation_no_styling_ignores_span_styles() {
        use ftui_render::budget::DegradationLevel;
        use ftui_render::cell::PackedRgba;
        use ftui_text::{Line, Span};

        // Create text with a styled span
        let styled_span = Span::styled("Hello", Style::new().fg(PackedRgba::RED));
        let line = Line::from_spans([styled_span]);
        let text = Text::from(line);
        let para = Paragraph::new(text);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.set_degradation(DegradationLevel::NoStyling);
        para.render(area, &mut frame);

        // Text should render but span style should be ignored
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('H'));
        // Foreground color should NOT be red
        assert_ne!(
            frame.buffer.get(0, 0).unwrap().fg,
            PackedRgba::RED,
            "Span fg color should be ignored at NoStyling"
        );
    }

    // --- MeasurableWidget tests ---

    use crate::MeasurableWidget;
    use ftui_core::geometry::Size;

    #[test]
    fn measure_simple_text() {
        let para = Paragraph::new(Text::raw("Hello"));
        let constraints = para.measure(Size::MAX);

        // "Hello" is 5 chars wide, 1 line tall
        assert_eq!(constraints.preferred, Size::new(5, 1));
        assert_eq!(constraints.min.height, 1);
        // Min width is the longest word = "Hello" = 5
        assert_eq!(constraints.min.width, 5);
    }

    #[test]
    fn measure_multiline_text() {
        let para = Paragraph::new(Text::raw("Line1\nLine22\nL3"));
        let constraints = para.measure(Size::MAX);

        // Max width is "Line22" = 6, height = 3 lines
        assert_eq!(constraints.preferred, Size::new(6, 3));
        assert_eq!(constraints.min.height, 1);
        // Min width is longest word = "Line22" = 6
        assert_eq!(constraints.min.width, 6);
    }

    #[test]
    fn measure_with_block() {
        let block = crate::block::Block::bordered();
        let para = Paragraph::new(Text::raw("Hi")).block(block);
        let constraints = para.measure(Size::MAX);

        // "Hi" = 2 wide, 1 tall, plus 2 for borders on each axis
        assert_eq!(constraints.preferred, Size::new(4, 3));
        // Min width: "Hi" = 2 + 2 (borders) = 4
        assert_eq!(constraints.min.width, 4);
        // Min height: 1 + 2 (borders) = 3
        assert_eq!(constraints.min.height, 3);
    }

    #[test]
    fn measure_with_word_wrap() {
        let para = Paragraph::new(Text::raw("hello world")).wrap(WrapMode::Word);
        // Measure with narrow available width
        let constraints = para.measure(Size::new(6, 10));

        // With 6 chars available, "hello" fits, "world" wraps
        // Preferred width = 6 (available), height = 2 lines
        assert_eq!(constraints.preferred.height, 2);
        // Min width is longest word = "hello" = 5
        assert_eq!(constraints.min.width, 5);
    }

    #[test]
    fn measure_empty_text() {
        let para = Paragraph::new(Text::raw(""));
        let constraints = para.measure(Size::MAX);

        // Empty text: 0 width, 0 height (no lines)
        assert_eq!(constraints.preferred.width, 0);
        assert_eq!(constraints.preferred.height, 0);
        // Min height is 0 for empty text (no content to display)
        // This ensures min <= preferred invariant holds
        assert_eq!(constraints.min.height, 0);
    }

    #[test]
    fn calculate_min_width_single_long_word() {
        let para = Paragraph::new(Text::raw("supercalifragilistic"));
        assert_eq!(para.calculate_min_width(), 20);
    }

    #[test]
    fn calculate_min_width_multiple_words() {
        let para = Paragraph::new(Text::raw("the quick brown fox"));
        // Longest word is "quick" or "brown" = 5
        assert_eq!(para.calculate_min_width(), 5);
    }

    #[test]
    fn calculate_min_width_multiline() {
        let para = Paragraph::new(Text::raw("short\nlongword\na"));
        // Longest word is "longword" = 8
        assert_eq!(para.calculate_min_width(), 8);
    }

    #[test]
    fn estimate_wrapped_height_no_wrap_needed() {
        let para = Paragraph::new(Text::raw("short")).wrap(WrapMode::Word);
        // Width 10 is enough for "short" (5 chars)
        assert_eq!(para.estimate_wrapped_height(10), 1);
    }

    #[test]
    fn estimate_wrapped_height_needs_wrap() {
        let para = Paragraph::new(Text::raw("hello world")).wrap(WrapMode::Word);
        // Width 6: "hello " fits (6 chars), "world" (5 chars) wraps
        assert_eq!(para.estimate_wrapped_height(6), 2);
    }

    #[test]
    fn has_intrinsic_size() {
        let para = Paragraph::new(Text::raw("test"));
        assert!(para.has_intrinsic_size());
    }

    #[test]
    fn measure_is_pure() {
        let para = Paragraph::new(Text::raw("Hello World"));
        let a = para.measure(Size::new(100, 50));
        let b = para.measure(Size::new(100, 50));
        assert_eq!(a, b);
    }
}

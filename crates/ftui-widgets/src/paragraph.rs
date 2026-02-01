#![forbid(unsafe_code)]

use crate::block::{Alignment, Block};
use crate::{Widget, draw_text_span, set_style_area};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_style::Style;
use ftui_text::{Text, WrapMode, wrap_text};
use unicode_width::UnicodeWidthStr;

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

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn wrap(mut self, wrap: WrapMode) -> Self {
        self.wrap = Some(wrap);
        self
    }

    pub fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    pub fn scroll(mut self, offset: (u16, u16)) -> Self {
        self.scroll = offset;
        self
    }
}

impl Widget for Paragraph<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
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

        let deg = buf.degradation;

        // Skeleton+: nothing to render
        if !deg.render_content() {
            return;
        }

        if deg.apply_styling() {
            set_style_area(buf, area, self.style);
        }

        let text_area = match self.block {
            Some(ref b) => {
                b.render(area, buf);
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

        let mut y = text_area.y;
        let mut current_visual_line = 0;
        let scroll_offset = self.scroll.0 as usize;

        for line in self.text.lines() {
            if y >= text_area.bottom() {
                break;
            }

            // If wrapping is enabled and line is wider than area, wrap it
            if let Some(wrap_mode) = self.wrap {
                let plain = line.to_plain_text();
                let line_width = plain.width();

                if line_width > text_area.width as usize {
                    let wrapped = wrap_text(&plain, text_area.width as usize, wrap_mode);
                    for wrapped_line in &wrapped {
                        if current_visual_line < scroll_offset {
                            current_visual_line += 1;
                            continue;
                        }

                        if y >= text_area.bottom() {
                            break;
                        }
                        let w = wrapped_line.width();
                        let x = align_x(text_area, w, self.alignment);
                        draw_text_span(buf, x, y, wrapped_line, style, text_area.right());
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

            // Render spans with proper Unicode widths
            let line_width: usize = line.width();
            let mut x = align_x(text_area, line_width, self.alignment);

            for span in line.spans() {
                // At NoStyling+, ignore span-level styles entirely
                let span_style = if deg.apply_styling() {
                    match span.style {
                        Some(s) => s.merge(&style),
                        None => style,
                    }
                } else {
                    style // Style::default() at NoStyling
                };
                x = draw_text_span(
                    buf,
                    x,
                    y,
                    span.content.as_ref(),
                    span_style,
                    text_area.right(),
                );
                if x >= text_area.right() {
                    break;
                }
            }
            y += 1;
            current_visual_line += 1;
        }
    }
}

/// Calculate the starting x position for a line given alignment.
fn align_x(area: Rect, line_width: usize, alignment: Alignment) -> u16 {
    let line_width_u16 = u16::try_from(line_width).unwrap_or(u16::MAX);
    match alignment {
        Alignment::Left => area.x,
        Alignment::Center => area.x + area.width.saturating_sub(line_width_u16) / 2,
        Alignment::Right => area.x + area.width.saturating_sub(line_width_u16),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_simple_text() {
        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        para.render(area, &mut buf);

        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('o'));
    }

    #[test]
    fn render_multiline_text() {
        let para = Paragraph::new(Text::raw("AB\nCD"));
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::new(5, 3);
        para.render(area, &mut buf);

        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('B'));
        assert_eq!(buf.get(0, 1).unwrap().content.as_char(), Some('C'));
        assert_eq!(buf.get(1, 1).unwrap().content.as_char(), Some('D'));
    }

    #[test]
    fn render_centered_text() {
        let para = Paragraph::new(Text::raw("Hi")).alignment(Alignment::Center);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        para.render(area, &mut buf);

        // "Hi" is 2 wide, area is 10, so starts at (10-2)/2 = 4
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(buf.get(5, 0).unwrap().content.as_char(), Some('i'));
    }

    #[test]
    fn render_with_scroll() {
        let para = Paragraph::new(Text::raw("Line1\nLine2\nLine3")).scroll((1, 0));
        let area = Rect::new(0, 0, 10, 2);
        let mut buf = Buffer::new(10, 2);
        para.render(area, &mut buf);

        // Should skip Line1, show Line2 and Line3
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('L'));
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('2'));
    }

    #[test]
    fn render_empty_area() {
        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::new(1, 1);
        para.render(area, &mut buf);
    }

    #[test]
    fn render_right_aligned() {
        let para = Paragraph::new(Text::raw("Hi")).alignment(Alignment::Right);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        para.render(area, &mut buf);

        // "Hi" is 2 wide, area is 10, so starts at 10-2 = 8
        assert_eq!(buf.get(8, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(buf.get(9, 0).unwrap().content.as_char(), Some('i'));
    }

    #[test]
    fn render_with_word_wrap() {
        let para = Paragraph::new(Text::raw("hello world")).wrap(WrapMode::Word);
        let area = Rect::new(0, 0, 6, 3);
        let mut buf = Buffer::new(6, 3);
        para.render(area, &mut buf);

        // "hello " fits in 6, " world" wraps to next line
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('h'));
        assert_eq!(buf.get(0, 1).unwrap().content.as_char(), Some('w'));
    }

    #[test]
    fn render_with_char_wrap() {
        let para = Paragraph::new(Text::raw("abcdefgh")).wrap(WrapMode::Char);
        let area = Rect::new(0, 0, 4, 3);
        let mut buf = Buffer::new(4, 3);
        para.render(area, &mut buf);

        // First line: abcd
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('a'));
        assert_eq!(buf.get(3, 0).unwrap().content.as_char(), Some('d'));
        // Second line: efgh
        assert_eq!(buf.get(0, 1).unwrap().content.as_char(), Some('e'));
    }

    #[test]
    fn scroll_past_all_lines() {
        let para = Paragraph::new(Text::raw("AB")).scroll((5, 0));
        let area = Rect::new(0, 0, 5, 2);
        let mut buf = Buffer::new(5, 2);
        para.render(area, &mut buf);

        // All lines skipped, buffer should remain empty
        assert!(buf.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn render_clipped_at_area_height() {
        let para = Paragraph::new(Text::raw("A\nB\nC\nD\nE"));
        let area = Rect::new(0, 0, 5, 2);
        let mut buf = Buffer::new(5, 2);
        para.render(area, &mut buf);

        // Only first 2 lines should render
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(buf.get(0, 1).unwrap().content.as_char(), Some('B'));
    }

    #[test]
    fn render_clipped_at_area_width() {
        let para = Paragraph::new(Text::raw("ABCDEF"));
        let area = Rect::new(0, 0, 3, 1);
        let mut buf = Buffer::new(3, 1);
        para.render(area, &mut buf);

        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(buf.get(2, 0).unwrap().content.as_char(), Some('C'));
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
        let mut buf = Buffer::new(10, 5);
        para.render(area, &mut buf);
    }

    #[test]
    fn render_at_offset_area() {
        let para = Paragraph::new(Text::raw("X"));
        let area = Rect::new(3, 4, 5, 2);
        let mut buf = Buffer::new(10, 10);
        para.render(area, &mut buf);

        assert_eq!(buf.get(3, 4).unwrap().content.as_char(), Some('X'));
        // Cell at (0,0) should be empty
        assert!(buf.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn wrap_clipped_at_area_bottom() {
        // Long wrapped text should stop at area height
        let para = Paragraph::new(Text::raw("abcdefghijklmnop")).wrap(WrapMode::Char);
        let area = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::new(4, 2);
        para.render(area, &mut buf);

        // Only 2 rows of 4 chars each
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('a'));
        assert_eq!(buf.get(0, 1).unwrap().content.as_char(), Some('e'));
    }

    // --- Degradation tests ---

    #[test]
    fn degradation_skeleton_skips_content() {
        use ftui_render::budget::DegradationLevel;

        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        buf.degradation = DegradationLevel::Skeleton;
        para.render(area, &mut buf);

        // No text should be rendered at Skeleton level
        assert!(buf.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn degradation_full_renders_content() {
        use ftui_render::budget::DegradationLevel;

        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        buf.degradation = DegradationLevel::Full;
        para.render(area, &mut buf);

        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('H'));
    }

    #[test]
    fn degradation_essential_only_still_renders_text() {
        use ftui_render::budget::DegradationLevel;

        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        buf.degradation = DegradationLevel::EssentialOnly;
        para.render(area, &mut buf);

        // EssentialOnly still renders content (< Skeleton)
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('H'));
    }

    #[test]
    fn degradation_no_styling_ignores_span_styles() {
        use ftui_render::budget::DegradationLevel;
        use ftui_render::cell::PackedRgba;
        use ftui_text::{Line, Span};

        // Create text with a styled span
        let styled_span = Span::styled("Hello", Style::new().fg(PackedRgba::RED));
        let text = Text::from(vec![Line::from(vec![styled_span])]);
        let para = Paragraph::new(text);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        buf.degradation = DegradationLevel::NoStyling;
        para.render(area, &mut buf);

        // Text should render but span style should be ignored
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('H'));
        // Foreground color should NOT be red
        assert_ne!(
            buf.get(0, 0).unwrap().fg,
            PackedRgba::RED,
            "Span fg color should be ignored at NoStyling"
        );
    }
}

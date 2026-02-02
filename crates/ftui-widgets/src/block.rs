#![forbid(unsafe_code)]

use crate::Widget;
use crate::borders::{BorderType, Borders};
use crate::{apply_style, draw_text_span, set_style_area};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_style::Style;

/// A widget that draws a block with optional borders, title, and padding.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Block<'a> {
    borders: Borders,
    border_style: Style,
    border_type: BorderType,
    title: Option<&'a str>,
    title_alignment: Alignment,
    style: Style,
}

/// Text alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    #[default]
    Left,
    Center,
    Right,
}

impl<'a> Block<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a block with all borders enabled.
    pub fn bordered() -> Self {
        Self::default().borders(Borders::ALL)
    }

    pub fn borders(mut self, borders: Borders) -> Self {
        self.borders = borders;
        self
    }

    pub fn border_style(mut self, style: Style) -> Self {
        self.border_style = style;
        self
    }

    pub fn border_type(mut self, border_type: BorderType) -> Self {
        self.border_type = border_type;
        self
    }

    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    pub fn title_alignment(mut self, alignment: Alignment) -> Self {
        self.title_alignment = alignment;
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Compute the inner area inside the block's borders.
    pub fn inner(&self, area: Rect) -> Rect {
        let mut inner = area;

        if self.borders.contains(Borders::LEFT) {
            inner.x = inner.x.saturating_add(1);
            inner.width = inner.width.saturating_sub(1);
        }
        if self.borders.contains(Borders::TOP) {
            inner.y = inner.y.saturating_add(1);
            inner.height = inner.height.saturating_sub(1);
        }
        if self.borders.contains(Borders::RIGHT) {
            inner.width = inner.width.saturating_sub(1);
        }
        if self.borders.contains(Borders::BOTTOM) {
            inner.height = inner.height.saturating_sub(1);
        }

        inner
    }

    /// Create a styled border cell.
    fn border_cell(&self, c: char) -> Cell {
        let mut cell = Cell::from_char(c);
        apply_style(&mut cell, self.border_style);
        cell
    }

    fn render_borders(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let set = self.border_type.to_border_set();

        // Edges
        if self.borders.contains(Borders::LEFT) {
            for y in area.y..area.bottom() {
                buf.set(area.x, y, self.border_cell(set.vertical));
            }
        }
        if self.borders.contains(Borders::RIGHT) {
            let x = area.right() - 1;
            for y in area.y..area.bottom() {
                buf.set(x, y, self.border_cell(set.vertical));
            }
        }
        if self.borders.contains(Borders::TOP) {
            for x in area.x..area.right() {
                buf.set(x, area.y, self.border_cell(set.horizontal));
            }
        }
        if self.borders.contains(Borders::BOTTOM) {
            let y = area.bottom() - 1;
            for x in area.x..area.right() {
                buf.set(x, y, self.border_cell(set.horizontal));
            }
        }

        // Corners (drawn after edges to overwrite edge characters at corners)
        if self.borders.contains(Borders::LEFT | Borders::TOP) {
            buf.set(area.x, area.y, self.border_cell(set.top_left));
        }
        if self.borders.contains(Borders::RIGHT | Borders::TOP) {
            buf.set(area.right() - 1, area.y, self.border_cell(set.top_right));
        }
        if self.borders.contains(Borders::LEFT | Borders::BOTTOM) {
            buf.set(area.x, area.bottom() - 1, self.border_cell(set.bottom_left));
        }
        if self.borders.contains(Borders::RIGHT | Borders::BOTTOM) {
            buf.set(
                area.right() - 1,
                area.bottom() - 1,
                self.border_cell(set.bottom_right),
            );
        }
    }

    /// Render borders using ASCII characters regardless of configured border_type.
    fn render_borders_ascii(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let set = crate::borders::BorderSet::ASCII;

        if self.borders.contains(Borders::LEFT) {
            for y in area.y..area.bottom() {
                buf.set(area.x, y, self.border_cell(set.vertical));
            }
        }
        if self.borders.contains(Borders::RIGHT) {
            let x = area.right() - 1;
            for y in area.y..area.bottom() {
                buf.set(x, y, self.border_cell(set.vertical));
            }
        }
        if self.borders.contains(Borders::TOP) {
            for x in area.x..area.right() {
                buf.set(x, area.y, self.border_cell(set.horizontal));
            }
        }
        if self.borders.contains(Borders::BOTTOM) {
            let y = area.bottom() - 1;
            for x in area.x..area.right() {
                buf.set(x, y, self.border_cell(set.horizontal));
            }
        }

        if self.borders.contains(Borders::LEFT | Borders::TOP) {
            buf.set(area.x, area.y, self.border_cell(set.top_left));
        }
        if self.borders.contains(Borders::RIGHT | Borders::TOP) {
            buf.set(area.right() - 1, area.y, self.border_cell(set.top_right));
        }
        if self.borders.contains(Borders::LEFT | Borders::BOTTOM) {
            buf.set(area.x, area.bottom() - 1, self.border_cell(set.bottom_left));
        }
        if self.borders.contains(Borders::RIGHT | Borders::BOTTOM) {
            buf.set(
                area.right() - 1,
                area.bottom() - 1,
                self.border_cell(set.bottom_right),
            );
        }
    }

    /// Render title without styling.
    #[allow(dead_code, unused_variables)]
    fn render_title_plain(&self, area: Rect, _buf: &mut Buffer) {
        if let Some(title) = self.title {
            if !self.borders.contains(Borders::TOP) || area.width < 3 {
                return;
            }

            let available_width = area.width.saturating_sub(2) as usize;
            if available_width == 0 {
                return;
            }

            let title_width = unicode_width::UnicodeWidthStr::width(title);
            let display_width = title_width.min(available_width);

            let _x = match self.title_alignment {
                Alignment::Left => area.x + 1,
                Alignment::Center => {
                    area.x + 1 + ((available_width.saturating_sub(display_width)) / 2) as u16
                }
                Alignment::Right => area
                    .right()
                    .saturating_sub(1)
                    .saturating_sub(display_width as u16),
            };

            let _max_x = area.right().saturating_sub(1);
            // This still uses buffer directly because it's plain text (no interning needed for simple titles)
            // But we should really use draw_text_span with frame if possible.
            // For now, let's assume plain title rendering is safe on Buffer for ASCII.
            // But we changed draw_text_span signature to take Frame!
            // We need a Frame here.
            // But render_title_plain is called when we have a Buffer but maybe not a Frame?
            // Widget::render gives us a Frame.
            // So we can pass Frame to render_title_plain.
        }
    }

    fn render_title(&self, area: Rect, frame: &mut Frame) {
        if let Some(title) = self.title {
            if !self.borders.contains(Borders::TOP) || area.width < 3 {
                return;
            }

            let available_width = area.width.saturating_sub(2) as usize;
            if available_width == 0 {
                return;
            }

            let title_width = unicode_width::UnicodeWidthStr::width(title);
            let display_width = title_width.min(available_width);

            let x = match self.title_alignment {
                Alignment::Left => area.x + 1,
                Alignment::Center => {
                    area.x + 1 + ((available_width.saturating_sub(display_width)) / 2) as u16
                }
                Alignment::Right => area
                    .right()
                    .saturating_sub(1)
                    .saturating_sub(display_width as u16),
            };

            let max_x = area.right().saturating_sub(1);
            draw_text_span(frame, x, area.y, title, self.border_style, max_x);
        }
    }
}

impl Widget for Block<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Block",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() {
            return;
        }

        let deg = frame.degradation;

        // Skeleton+: skip everything, just clear area
        if !deg.render_content() {
            frame.buffer.fill(area, Cell::default());
            return;
        }

        // EssentialOnly: skip borders entirely, only apply bg style if styling enabled
        if !deg.render_decorative() {
            if deg.apply_styling() {
                set_style_area(&mut frame.buffer, area, self.style);
            }
            return;
        }

        // Apply background/style
        if deg.apply_styling() {
            set_style_area(&mut frame.buffer, area, self.style);
        }

        // Render borders (with possible ASCII downgrade)
        if deg.use_unicode_borders() {
            self.render_borders(area, &mut frame.buffer);
        } else {
            // Force ASCII borders regardless of configured border_type
            self.render_borders_ascii(area, &mut frame.buffer);
        }

        // Render title (skip at NoStyling to save time)
        if deg.apply_styling() {
            self.render_title(area, frame);
        } else if deg.render_decorative() {
            // Still show title but without styling
            // Pass frame to reuse draw_text_span
            if let Some(title) = self.title
                && self.borders.contains(Borders::TOP)
                && area.width >= 3
            {
                let available_width = area.width.saturating_sub(2) as usize;
                if available_width > 0 {
                    let title_width = unicode_width::UnicodeWidthStr::width(title);
                    let display_width = title_width.min(available_width);
                    let x = match self.title_alignment {
                        Alignment::Left => area.x + 1,
                        Alignment::Center => {
                            area.x
                                + 1
                                + ((available_width.saturating_sub(display_width)) / 2) as u16
                        }
                        Alignment::Right => area
                            .right()
                            .saturating_sub(1)
                            .saturating_sub(display_width as u16),
                    };
                    let max_x = area.right().saturating_sub(1);
                    draw_text_span(frame, x, area.y, title, Style::default(), max_x);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::PackedRgba;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn inner_with_all_borders() {
        let block = Block::new().borders(Borders::ALL);
        let area = Rect::new(0, 0, 10, 10);
        let inner = block.inner(area);
        assert_eq!(inner, Rect::new(1, 1, 8, 8));
    }

    #[test]
    fn inner_with_no_borders() {
        let block = Block::new();
        let area = Rect::new(0, 0, 10, 10);
        let inner = block.inner(area);
        assert_eq!(inner, area);
    }

    #[test]
    fn inner_with_partial_borders() {
        let block = Block::new().borders(Borders::TOP | Borders::LEFT);
        let area = Rect::new(0, 0, 10, 10);
        let inner = block.inner(area);
        assert_eq!(inner, Rect::new(1, 1, 9, 9));
    }

    #[test]
    fn render_empty_area() {
        let block = Block::new().borders(Borders::ALL);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        block.render(area, &mut frame);
    }

    #[test]
    fn render_block_with_square_borders() {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Square);
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        block.render(area, &mut frame);

        let buf = &frame.buffer;
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('┌'));
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('┐'));
        assert_eq!(buf.get(0, 2).unwrap().content.as_char(), Some('└'));
        assert_eq!(buf.get(4, 2).unwrap().content.as_char(), Some('┘'));
        assert_eq!(buf.get(2, 0).unwrap().content.as_char(), Some('─'));
        assert_eq!(buf.get(0, 1).unwrap().content.as_char(), Some('│'));
    }

    #[test]
    fn render_block_with_title() {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Square)
            .title("Hi");
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        block.render(area, &mut frame);

        let buf = &frame.buffer;
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(buf.get(2, 0).unwrap().content.as_char(), Some('i'));
    }

    #[test]
    fn render_block_with_background() {
        let block = Block::new().style(Style::new().bg(PackedRgba::rgb(10, 20, 30)));
        let area = Rect::new(0, 0, 3, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 2, &mut pool);
        block.render(area, &mut frame);

        let buf = &frame.buffer;
        assert_eq!(buf.get(0, 0).unwrap().bg, PackedRgba::rgb(10, 20, 30));
        assert_eq!(buf.get(2, 1).unwrap().bg, PackedRgba::rgb(10, 20, 30));
    }

    #[test]
    fn inner_with_only_bottom() {
        let block = Block::new().borders(Borders::BOTTOM);
        let area = Rect::new(0, 0, 10, 10);
        let inner = block.inner(area);
        assert_eq!(inner, Rect::new(0, 0, 10, 9));
    }

    #[test]
    fn inner_with_only_right() {
        let block = Block::new().borders(Borders::RIGHT);
        let area = Rect::new(0, 0, 10, 10);
        let inner = block.inner(area);
        assert_eq!(inner, Rect::new(0, 0, 9, 10));
    }

    #[test]
    fn inner_saturates_on_tiny_area() {
        let block = Block::new().borders(Borders::ALL);
        let area = Rect::new(0, 0, 1, 1);
        let inner = block.inner(area);
        // 1x1 with all borders: x+1=1, w-2=0, y+1=1, h-2=0
        assert_eq!(inner.width, 0);
    }

    #[test]
    fn bordered_constructor() {
        let block = Block::bordered();
        assert_eq!(block.borders, Borders::ALL);
    }

    #[test]
    fn default_has_no_borders() {
        let block = Block::new();
        assert_eq!(block.borders, Borders::empty());
        assert!(block.title.is_none());
    }

    #[test]
    fn render_rounded_borders() {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        block.render(area, &mut frame);

        let buf = &frame.buffer;
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('╭'));
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('╮'));
        assert_eq!(buf.get(0, 2).unwrap().content.as_char(), Some('╰'));
        assert_eq!(buf.get(4, 2).unwrap().content.as_char(), Some('╯'));
    }

    #[test]
    fn render_double_borders() {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Double);
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        block.render(area, &mut frame);

        let buf = &frame.buffer;
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('╔'));
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('╗'));
    }

    #[test]
    fn render_title_left_aligned() {
        let block = Block::new()
            .borders(Borders::ALL)
            .title("Test")
            .title_alignment(Alignment::Left);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        block.render(area, &mut frame);

        let buf = &frame.buffer;
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('T'));
        assert_eq!(buf.get(2, 0).unwrap().content.as_char(), Some('e'));
    }

    #[test]
    fn render_title_center_aligned() {
        let block = Block::new()
            .borders(Borders::ALL)
            .title("Hi")
            .title_alignment(Alignment::Center);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        block.render(area, &mut frame);

        // Title "Hi" (2 chars) in 8 available (10-2 borders), centered at offset 3
        let buf = &frame.buffer;
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(buf.get(5, 0).unwrap().content.as_char(), Some('i'));
    }

    #[test]
    fn render_title_right_aligned() {
        let block = Block::new()
            .borders(Borders::ALL)
            .title("Hi")
            .title_alignment(Alignment::Right);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        block.render(area, &mut frame);

        let buf = &frame.buffer;
        // "Hi" right-aligned: right()-1 - 2 = col 7
        assert_eq!(buf.get(7, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(buf.get(8, 0).unwrap().content.as_char(), Some('i'));
    }

    #[test]
    fn title_not_rendered_without_top_border() {
        let block = Block::new()
            .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
            .title("Hi");
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        block.render(area, &mut frame);

        let buf = &frame.buffer;
        // No title should appear on row 0
        assert_ne!(buf.get(1, 0).unwrap().content.as_char(), Some('H'));
    }

    #[test]
    fn border_style_applied() {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(PackedRgba::rgb(255, 0, 0)));
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        block.render(area, &mut frame);

        let buf = &frame.buffer;
        assert_eq!(buf.get(0, 0).unwrap().fg, PackedRgba::rgb(255, 0, 0));
    }

    #[test]
    fn only_horizontal_borders() {
        let block = Block::new()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_type(BorderType::Square);
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        block.render(area, &mut frame);

        let buf = &frame.buffer;
        // Top and bottom should have horizontal lines
        assert_eq!(buf.get(2, 0).unwrap().content.as_char(), Some('─'));
        assert_eq!(buf.get(2, 2).unwrap().content.as_char(), Some('─'));
        // Left edge should be empty (no vertical border)
        assert!(buf.get(0, 1).unwrap().is_empty() || buf.get(0, 1).unwrap().content.as_char() == Some(' '));
    }

    #[test]
    fn block_equality() {
        let a = Block::new().borders(Borders::ALL).title("Test");
        let b = Block::new().borders(Borders::ALL).title("Test");
        assert_eq!(a, b);
    }

    #[test]
    fn render_1x1_no_panic() {
        let block = Block::bordered();
        let area = Rect::new(0, 0, 1, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        block.render(area, &mut frame);
    }

    #[test]
    fn render_2x2_with_borders() {
        let block = Block::bordered().border_type(BorderType::Square);
        let area = Rect::new(0, 0, 2, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 2, &mut pool);
        block.render(area, &mut frame);

        let buf = &frame.buffer;
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('┌'));
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('┐'));
        assert_eq!(buf.get(0, 1).unwrap().content.as_char(), Some('└'));
        assert_eq!(buf.get(1, 1).unwrap().content.as_char(), Some('┘'));
    }

    #[test]
    fn title_too_narrow() {
        // Width 3 with all borders = 1 char available for title
        let block = Block::bordered().title("LongTitle");
        let area = Rect::new(0, 0, 4, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 3, &mut pool);
        block.render(area, &mut frame);
        // Should not panic, title gets truncated
    }

    #[test]
    fn alignment_default_is_left() {
        assert_eq!(Alignment::default(), Alignment::Left);
    }
}

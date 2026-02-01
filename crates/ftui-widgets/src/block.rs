#![forbid(unsafe_code)]

use crate::Widget;
use crate::borders::{BorderType, Borders};
use crate::{apply_style, draw_text_span, set_style_area};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
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
    fn render_title_plain(&self, area: Rect, buf: &mut Buffer) {
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
            draw_text_span(buf, x, area.y, title, Style::default(), max_x);
        }
    }

    fn render_title(&self, area: Rect, buf: &mut Buffer) {
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
            draw_text_span(buf, x, area.y, title, self.border_style, max_x);
        }
    }
}

impl Widget for Block<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
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

        let deg = buf.degradation;

        // Skeleton+: skip everything, just clear area
        if !deg.render_content() {
            buf.fill(area, Cell::default());
            return;
        }

        // EssentialOnly: skip borders entirely, only apply bg style if styling enabled
        if !deg.render_decorative() {
            if deg.apply_styling() {
                set_style_area(buf, area, self.style);
            }
            return;
        }

        // Apply background/style
        if deg.apply_styling() {
            set_style_area(buf, area, self.style);
        }

        // Render borders (with possible ASCII downgrade)
        if deg.use_unicode_borders() {
            self.render_borders(area, buf);
        } else {
            // Force ASCII borders regardless of configured border_type
            self.render_borders_ascii(area, buf);
        }

        // Render title (skip at NoStyling to save time)
        if deg.apply_styling() {
            self.render_title(area, buf);
        } else if deg.render_decorative() {
            // Still show title but without styling
            self.render_title_plain(area, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::PackedRgba;

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
        let mut buf = Buffer::new(1, 1);
        block.render(area, &mut buf);
    }

    #[test]
    fn render_block_with_square_borders() {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Square);
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::new(5, 3);
        block.render(area, &mut buf);

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
        let mut buf = Buffer::new(10, 3);
        block.render(area, &mut buf);

        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(buf.get(2, 0).unwrap().content.as_char(), Some('i'));
    }

    #[test]
    fn render_block_with_background() {
        let block = Block::new().style(Style::new().bg(PackedRgba::rgb(10, 20, 30)));
        let area = Rect::new(0, 0, 3, 2);
        let mut buf = Buffer::new(3, 2);
        block.render(area, &mut buf);

        assert_eq!(buf.get(0, 0).unwrap().bg, PackedRgba::rgb(10, 20, 30));
        assert_eq!(buf.get(2, 1).unwrap().bg, PackedRgba::rgb(10, 20, 30));
    }

    #[test]
    fn bordered_convenience() {
        let block = Block::bordered();
        let area = Rect::new(0, 0, 5, 3);
        let inner = block.inner(area);
        assert_eq!(inner, Rect::new(1, 1, 3, 1));
    }

    #[test]
    fn inner_single_cell_with_all_borders() {
        let block = Block::bordered();
        let inner = block.inner(Rect::new(0, 0, 2, 2));
        assert_eq!(inner.width, 0);
        assert_eq!(inner.height, 0);
    }

    #[test]
    fn inner_with_only_bottom_right() {
        let block = Block::new().borders(Borders::BOTTOM | Borders::RIGHT);
        let area = Rect::new(0, 0, 10, 10);
        let inner = block.inner(area);
        assert_eq!(inner, Rect::new(0, 0, 9, 9));
    }

    #[test]
    fn render_with_rounded_borders() {
        let block = Block::bordered().border_type(BorderType::Rounded);
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::new(5, 3);
        block.render(area, &mut buf);

        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('╭'));
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('╮'));
        assert_eq!(buf.get(0, 2).unwrap().content.as_char(), Some('╰'));
        assert_eq!(buf.get(4, 2).unwrap().content.as_char(), Some('╯'));
    }

    #[test]
    fn render_with_double_borders() {
        let block = Block::bordered().border_type(BorderType::Double);
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::new(5, 3);
        block.render(area, &mut buf);

        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('╔'));
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('╗'));
        assert_eq!(buf.get(0, 2).unwrap().content.as_char(), Some('╚'));
        assert_eq!(buf.get(4, 2).unwrap().content.as_char(), Some('╝'));
    }

    #[test]
    fn render_title_centered() {
        let block = Block::bordered()
            .title("AB")
            .title_alignment(Alignment::Center);
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::new(10, 3);
        block.render(area, &mut buf);

        // available_width = 10-2 = 8, title "AB" = 2, offset = (8-2)/2 = 3
        // title starts at area.x + 1 + 3 = 4
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(buf.get(5, 0).unwrap().content.as_char(), Some('B'));
    }

    #[test]
    fn render_title_right_aligned() {
        let block = Block::bordered()
            .title("XY")
            .title_alignment(Alignment::Right);
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::new(10, 3);
        block.render(area, &mut buf);

        // right = 10, minus 1 for border, minus 2 for title = 7
        assert_eq!(buf.get(7, 0).unwrap().content.as_char(), Some('X'));
        assert_eq!(buf.get(8, 0).unwrap().content.as_char(), Some('Y'));
    }

    #[test]
    fn title_skipped_without_top_border() {
        let block = Block::new()
            .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
            .title("Skip");
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::new(10, 3);
        block.render(area, &mut buf);

        assert_ne!(buf.get(1, 0).unwrap().content.as_char(), Some('S'));
    }

    #[test]
    fn render_at_nonzero_origin() {
        let block = Block::bordered();
        let area = Rect::new(3, 2, 5, 3);
        let mut buf = Buffer::new(10, 10);
        block.render(area, &mut buf);

        assert_eq!(buf.get(3, 2).unwrap().content.as_char(), Some('┌'));
        assert_eq!(buf.get(7, 2).unwrap().content.as_char(), Some('┐'));
        assert_eq!(buf.get(3, 4).unwrap().content.as_char(), Some('└'));
        assert!(buf.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn narrow_block_no_title_space() {
        let block = Block::bordered().title("Hello");
        let area = Rect::new(0, 0, 2, 3);
        let mut buf = Buffer::new(2, 3);
        block.render(area, &mut buf);
    }

    #[test]
    fn block_default_is_no_borders() {
        let block = Block::default();
        let area = Rect::new(0, 0, 5, 5);
        assert_eq!(block.inner(area), area);
    }

    // --- Degradation tests ---

    #[test]
    fn degradation_simple_borders_uses_ascii() {
        use ftui_render::budget::DegradationLevel;

        let block = Block::bordered().border_type(BorderType::Rounded);
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::new(5, 3);
        buf.degradation = DegradationLevel::SimpleBorders;
        block.render(area, &mut buf);

        // Should use ASCII '+' corners, not Unicode '╭'
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('+'));
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('+'));
        assert_eq!(buf.get(0, 2).unwrap().content.as_char(), Some('+'));
        assert_eq!(buf.get(4, 2).unwrap().content.as_char(), Some('+'));
        // Edges should be ASCII '-' and '|'
        assert_eq!(buf.get(2, 0).unwrap().content.as_char(), Some('-'));
        assert_eq!(buf.get(0, 1).unwrap().content.as_char(), Some('|'));
    }

    #[test]
    fn degradation_essential_only_skips_borders() {
        use ftui_render::budget::DegradationLevel;

        let block = Block::bordered();
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::new(5, 3);
        buf.degradation = DegradationLevel::EssentialOnly;
        block.render(area, &mut buf);

        // No border characters should be rendered
        assert_ne!(buf.get(0, 0).unwrap().content.as_char(), Some('┌'));
        assert_ne!(buf.get(0, 0).unwrap().content.as_char(), Some('+'));
    }

    #[test]
    fn degradation_skeleton_clears_area() {
        use ftui_render::budget::DegradationLevel;

        let block = Block::bordered();
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::new(5, 3);
        buf.degradation = DegradationLevel::Skeleton;
        block.render(area, &mut buf);

        // Area should be cleared (fill with default cells), no borders
        assert_ne!(buf.get(0, 0).unwrap().content.as_char(), Some('┌'));
        assert_ne!(buf.get(2, 0).unwrap().content.as_char(), Some('─'));
    }

    #[test]
    fn degradation_no_styling_skips_title_style() {
        use ftui_render::budget::DegradationLevel;

        let block = Block::bordered()
            .title("Test")
            .border_style(Style::new().fg(PackedRgba::RED));
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::new(10, 3);
        buf.degradation = DegradationLevel::NoStyling;
        block.render(area, &mut buf);

        // Borders should be ASCII, title present but unstyled
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('+'));
        // Title should still be rendered
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('T'));
    }

    #[test]
    fn degradation_full_uses_unicode() {
        use ftui_render::budget::DegradationLevel;

        let block = Block::bordered().border_type(BorderType::Rounded);
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::new(5, 3);
        buf.degradation = DegradationLevel::Full;
        block.render(area, &mut buf);

        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('╭'));
    }
}

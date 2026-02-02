#![forbid(unsafe_code)]

//! Panel widget: border + optional title/subtitle + inner padding + child content.

use crate::block::Alignment;
use crate::borders::{BorderSet, BorderType, Borders};
use crate::{Widget, apply_style, draw_text_span, set_style_area};
use ftui_core::geometry::{Rect, Sides};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_style::Style;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A bordered container that renders a child widget inside an inner padded area.
#[derive(Debug, Clone)]
pub struct Panel<'a, W> {
    child: W,
    borders: Borders,
    border_style: Style,
    border_type: BorderType,
    title: Option<&'a str>,
    title_alignment: Alignment,
    title_style: Style,
    subtitle: Option<&'a str>,
    subtitle_alignment: Alignment,
    subtitle_style: Style,
    style: Style,
    padding: Sides,
}

impl<'a, W> Panel<'a, W> {
    pub fn new(child: W) -> Self {
        Self {
            child,
            borders: Borders::ALL,
            border_style: Style::default(),
            border_type: BorderType::Square,
            title: None,
            title_alignment: Alignment::Left,
            title_style: Style::default(),
            subtitle: None,
            subtitle_alignment: Alignment::Left,
            subtitle_style: Style::default(),
            style: Style::default(),
            padding: Sides::default(),
        }
    }

    /// Set which borders to draw.
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

    pub fn title_style(mut self, style: Style) -> Self {
        self.title_style = style;
        self
    }

    pub fn subtitle(mut self, subtitle: &'a str) -> Self {
        self.subtitle = Some(subtitle);
        self
    }

    pub fn subtitle_alignment(mut self, alignment: Alignment) -> Self {
        self.subtitle_alignment = alignment;
        self
    }

    pub fn subtitle_style(mut self, style: Style) -> Self {
        self.subtitle_style = style;
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn padding(mut self, padding: impl Into<Sides>) -> Self {
        self.padding = padding.into();
        self
    }

    /// Compute the inner area inside the panel borders.
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

    fn border_cell(&self, c: char) -> Cell {
        let mut cell = Cell::from_char(c);
        apply_style(&mut cell, self.border_style);
        cell
    }

    fn pick_border_set(&self, buf: &Buffer) -> BorderSet {
        let deg = buf.degradation;
        if !deg.use_unicode_borders() {
            return BorderSet::ASCII;
        }
        self.border_type.to_border_set()
    }

    fn render_borders(&self, area: Rect, buf: &mut Buffer, set: BorderSet) {
        if area.is_empty() {
            return;
        }

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

        // Corners (drawn after edges)
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

    fn ellipsize<'s>(&self, s: &'s str, max_width: usize) -> std::borrow::Cow<'s, str> {
        let total = UnicodeWidthStr::width(s);
        if total <= max_width {
            return std::borrow::Cow::Borrowed(s);
        }
        if max_width == 0 {
            return std::borrow::Cow::Borrowed("");
        }

        // Use a single-cell ellipsis.
        if max_width == 1 {
            return std::borrow::Cow::Borrowed("…");
        }

        let mut out = String::new();
        let mut used = 0usize;
        let target = max_width - 1;

        for g in s.graphemes(true) {
            let w = UnicodeWidthStr::width(g);
            if w == 0 {
                continue;
            }
            if used + w > target {
                break;
            }
            out.push_str(g);
            used += w;
        }

        out.push('…');
        std::borrow::Cow::Owned(out)
    }

    fn render_top_text(
        &self,
        area: Rect,
        frame: &mut Frame,
        text: &str,
        alignment: Alignment,
        style: Style,
    ) {
        if area.width < 2 {
            return;
        }

        let available_width = area.width.saturating_sub(2) as usize;
        let text = self.ellipsize(text, available_width);
        let display_width = UnicodeWidthStr::width(text.as_ref()).min(available_width);

        let x = match alignment {
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
        draw_text_span(frame, x, area.y, text.as_ref(), style, max_x);
    }

    fn render_bottom_text(
        &self,
        area: Rect,
        frame: &mut Frame,
        text: &str,
        alignment: Alignment,
        style: Style,
    ) {
        if area.height < 1 || area.width < 2 {
            return;
        }

        let available_width = area.width.saturating_sub(2) as usize;
        let text = self.ellipsize(text, available_width);
        let display_width = UnicodeWidthStr::width(text.as_ref()).min(available_width);

        let x = match alignment {
            Alignment::Left => area.x + 1,
            Alignment::Center => {
                area.x + 1 + ((available_width.saturating_sub(display_width)) / 2) as u16
            }
            Alignment::Right => area
                .right()
                .saturating_sub(1)
                .saturating_sub(display_width as u16),
        };

        let y = area.bottom() - 1;
        let max_x = area.right().saturating_sub(1);
        draw_text_span(frame, x, y, text.as_ref(), style, max_x);
    }
}

struct ScissorGuard<'a, 'pool> {
    frame: &'a mut Frame<'pool>,
}

impl<'a, 'pool> ScissorGuard<'a, 'pool> {
    fn new(frame: &'a mut Frame<'pool>, rect: Rect) -> Self {
        frame.buffer.push_scissor(rect);
        Self { frame }
    }
}

impl Drop for ScissorGuard<'_, '_> {
    fn drop(&mut self) {
        self.frame.buffer.pop_scissor();
    }
}

impl<W: Widget> Widget for Panel<'_, W> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Panel",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() {
            return;
        }

        let deg = frame.buffer.degradation;

        // Skeleton+: skip everything, just clear area
        if !deg.render_content() {
            frame.buffer.fill(area, Cell::default());
            return;
        }

        // Background/style
        if deg.apply_styling() {
            set_style_area(&mut frame.buffer, area, self.style);
        }

        // Decorative layer: borders + title/subtitle
        if deg.render_decorative() {
            let set = self.pick_border_set(&frame.buffer);
            self.render_borders(area, &mut frame.buffer, set);

            if self.borders.contains(Borders::TOP)
                && let Some(title) = self.title
            {
                let title_style = if deg.apply_styling() {
                    self.title_style.merge(&self.border_style)
                } else {
                    Style::default()
                };
                self.render_top_text(area, frame, title, self.title_alignment, title_style);
            }

            if self.borders.contains(Borders::BOTTOM)
                && let Some(subtitle) = self.subtitle
            {
                let subtitle_style = if deg.apply_styling() {
                    self.subtitle_style.merge(&self.border_style)
                } else {
                    Style::default()
                };
                self.render_bottom_text(
                    area,
                    frame,
                    subtitle,
                    self.subtitle_alignment,
                    subtitle_style,
                );
            }
        }

        // Content
        let mut content_area = self.inner(area);
        content_area = content_area.inner(self.padding);
        if content_area.is_empty() {
            return;
        }

        let guard = ScissorGuard::new(frame, content_area);
        self.child.render(content_area, guard.frame);
    }

    fn is_essential(&self) -> bool {
        self.child.is_essential()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    fn panel_stub() -> Panel<'static, crate::block::Block<'static>> {
        Panel::new(crate::block::Block::default())
    }

    fn cell_char(frame: &Frame, x: u16, y: u16) -> Option<char> {
        frame.buffer.get(x, y).and_then(|c| c.content.as_char())
    }

    // --- ellipsize tests ---

    #[test]
    fn ellipsize_short_is_borrowed() {
        let panel = panel_stub();
        let out = panel.ellipsize("abc", 3);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(out, "abc");
    }

    #[test]
    fn ellipsize_truncates_with_ellipsis() {
        let panel = panel_stub();
        let out = panel.ellipsize("abcdef", 4);
        assert_eq!(out, "abc…");
    }

    #[test]
    fn ellipsize_zero_width_returns_empty() {
        let panel = panel_stub();
        let out = panel.ellipsize("abc", 0);
        assert_eq!(out, "");
    }

    #[test]
    fn ellipsize_width_one_returns_ellipsis() {
        let panel = panel_stub();
        let out = panel.ellipsize("abc", 1);
        assert_eq!(out, "…");
    }

    #[test]
    fn ellipsize_exact_fit_is_borrowed() {
        let panel = panel_stub();
        let out = panel.ellipsize("hello", 5);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(out, "hello");
    }

    #[test]
    fn ellipsize_one_over_truncates() {
        let panel = panel_stub();
        let out = panel.ellipsize("hello", 4);
        assert_eq!(out, "hel…");
    }

    // --- inner() calculation tests ---

    #[test]
    fn inner_all_borders() {
        let panel = panel_stub().borders(Borders::ALL);
        let area = Rect::new(0, 0, 10, 10);
        assert_eq!(panel.inner(area), Rect::new(1, 1, 8, 8));
    }

    #[test]
    fn inner_no_borders() {
        let panel = panel_stub().borders(Borders::NONE);
        let area = Rect::new(0, 0, 10, 10);
        assert_eq!(panel.inner(area), area);
    }

    #[test]
    fn inner_top_and_left_only() {
        let panel = panel_stub().borders(Borders::TOP | Borders::LEFT);
        let area = Rect::new(0, 0, 10, 10);
        assert_eq!(panel.inner(area), Rect::new(1, 1, 9, 9));
    }

    #[test]
    fn inner_right_and_bottom_only() {
        let panel = panel_stub().borders(Borders::RIGHT | Borders::BOTTOM);
        let area = Rect::new(0, 0, 10, 10);
        assert_eq!(panel.inner(area), Rect::new(0, 0, 9, 9));
    }

    #[test]
    fn inner_with_offset_area() {
        let panel = panel_stub().borders(Borders::ALL);
        let area = Rect::new(5, 3, 10, 8);
        assert_eq!(panel.inner(area), Rect::new(6, 4, 8, 6));
    }

    #[test]
    fn inner_zero_size_saturates() {
        let panel = panel_stub().borders(Borders::ALL);
        let area = Rect::new(0, 0, 1, 1);
        let inner = panel.inner(area);
        assert_eq!(inner.width, 0);
        assert_eq!(inner.height, 0);
    }

    // --- render border tests ---

    #[test]
    fn render_borders_square() {
        let child = crate::block::Block::default();
        let panel = Panel::new(child)
            .borders(Borders::ALL)
            .border_type(BorderType::Square);
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);

        panel.render(area, &mut frame);

        assert_eq!(cell_char(&frame, 0, 0), Some('┌'));
        assert_eq!(cell_char(&frame, 4, 0), Some('┐'));
        assert_eq!(cell_char(&frame, 0, 2), Some('└'));
        assert_eq!(cell_char(&frame, 4, 2), Some('┘'));
        assert_eq!(cell_char(&frame, 2, 0), Some('─'));
        assert_eq!(cell_char(&frame, 0, 1), Some('│'));
    }

    #[test]
    fn render_borders_rounded() {
        let child = crate::block::Block::default();
        let panel = Panel::new(child)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);

        panel.render(area, &mut frame);

        assert_eq!(cell_char(&frame, 0, 0), Some('╭'));
        assert_eq!(cell_char(&frame, 4, 0), Some('╮'));
        assert_eq!(cell_char(&frame, 0, 2), Some('╰'));
        assert_eq!(cell_char(&frame, 4, 2), Some('╯'));
    }

    #[test]
    fn render_empty_area_does_not_panic() {
        let panel = panel_stub().borders(Borders::ALL);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        panel.render(area, &mut frame);
    }

    // --- title rendering tests ---

    #[test]
    fn render_title_left_aligned() {
        let child = crate::block::Block::default();
        let panel = Panel::new(child)
            .borders(Borders::ALL)
            .border_type(BorderType::Square)
            .title("Hi")
            .title_alignment(Alignment::Left);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);

        panel.render(area, &mut frame);

        // Title starts at x=1 (after left border)
        assert_eq!(cell_char(&frame, 1, 0), Some('H'));
        assert_eq!(cell_char(&frame, 2, 0), Some('i'));
    }

    #[test]
    fn render_title_right_aligned() {
        let child = crate::block::Block::default();
        let panel = Panel::new(child)
            .borders(Borders::ALL)
            .border_type(BorderType::Square)
            .title("Hi")
            .title_alignment(Alignment::Right);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);

        panel.render(area, &mut frame);

        // "Hi" is 2 chars, right edge is 9, so title at 9-1-2=6..8
        // right() = 10, sub 1 = 9, sub 2 = 7
        assert_eq!(cell_char(&frame, 7, 0), Some('H'));
        assert_eq!(cell_char(&frame, 8, 0), Some('i'));
    }

    #[test]
    fn render_title_center_aligned() {
        let child = crate::block::Block::default();
        let panel = Panel::new(child)
            .borders(Borders::ALL)
            .border_type(BorderType::Square)
            .title("AB")
            .title_alignment(Alignment::Center);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);

        panel.render(area, &mut frame);

        // available_width = 10-2 = 8, display_width = 2
        // x = 0 + 1 + (8-2)/2 = 1 + 3 = 4
        assert_eq!(cell_char(&frame, 4, 0), Some('A'));
        assert_eq!(cell_char(&frame, 5, 0), Some('B'));
    }

    #[test]
    fn render_title_no_top_border_skips_title() {
        let child = crate::block::Block::default();
        let panel = Panel::new(child)
            .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
            .title("Hi");
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);

        panel.render(area, &mut frame);

        // Title should NOT appear on row 0 since no top border
        assert_ne!(cell_char(&frame, 1, 0), Some('H'));
    }

    #[test]
    fn render_title_truncated_with_ellipsis() {
        let child = crate::block::Block::default();
        let panel = Panel::new(child)
            .borders(Borders::ALL)
            .border_type(BorderType::Square)
            .title("LongTitle")
            .title_alignment(Alignment::Left);
        // Width 6: available = 6-2 = 4, "LongTitle" (9 chars) -> "Lon…"
        let area = Rect::new(0, 0, 6, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 3, &mut pool);

        panel.render(area, &mut frame);

        assert_eq!(cell_char(&frame, 1, 0), Some('L'));
        assert_eq!(cell_char(&frame, 2, 0), Some('o'));
        assert_eq!(cell_char(&frame, 3, 0), Some('n'));
        assert_eq!(cell_char(&frame, 4, 0), Some('…'));
    }

    // --- subtitle rendering tests ---

    #[test]
    fn render_subtitle_left_aligned() {
        let child = crate::block::Block::default();
        let panel = Panel::new(child)
            .borders(Borders::ALL)
            .border_type(BorderType::Square)
            .subtitle("Lo")
            .subtitle_alignment(Alignment::Left);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);

        panel.render(area, &mut frame);

        // Subtitle on bottom row (y=2), starting at x=1
        assert_eq!(cell_char(&frame, 1, 2), Some('L'));
        assert_eq!(cell_char(&frame, 2, 2), Some('o'));
    }

    #[test]
    fn render_subtitle_no_bottom_border_skips() {
        let child = crate::block::Block::default();
        let panel = Panel::new(child)
            .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
            .subtitle("Lo");
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);

        panel.render(area, &mut frame);

        // Subtitle should not appear since no bottom border
        assert_ne!(cell_char(&frame, 1, 2), Some('L'));
    }

    // --- padding tests ---

    #[test]
    fn inner_with_padding_reduces_area() {
        let panel = panel_stub().borders(Borders::ALL).padding(Sides::all(1));
        let area = Rect::new(0, 0, 10, 10);
        // inner from borders = (1,1,8,8), then padding of 1 on each side = (2,2,6,6)
        let inner_from_borders = panel.inner(area);
        let padded = inner_from_borders.inner(Sides::all(1));
        assert_eq!(padded, Rect::new(2, 2, 6, 6));
    }

    // --- child rendering tests ---

    /// A simple test widget that writes 'X' at (0,0) relative to its area.
    struct MarkerWidget;

    impl Widget for MarkerWidget {
        fn render(&self, area: Rect, frame: &mut Frame) {
            if !area.is_empty() {
                let mut cell = Cell::from_char('X');
                apply_style(&mut cell, Style::default());
                frame.buffer.set(area.x, area.y, cell);
            }
        }
    }

    #[test]
    fn child_rendered_inside_borders() {
        let panel = Panel::new(MarkerWidget).borders(Borders::ALL);
        let area = Rect::new(0, 0, 5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);

        panel.render(area, &mut frame);

        // Child area starts at (1,1) for ALL borders
        assert_eq!(cell_char(&frame, 1, 1), Some('X'));
    }

    #[test]
    fn child_rendered_with_padding_offset() {
        let panel = Panel::new(MarkerWidget)
            .borders(Borders::ALL)
            .padding(Sides::new(1, 1, 0, 1));
        let area = Rect::new(0, 0, 10, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);

        panel.render(area, &mut frame);

        // borders inner = (1,1,8,8), padding top=1 left=1 -> child at (2,2)
        assert_eq!(cell_char(&frame, 2, 2), Some('X'));
    }

    #[test]
    fn child_not_rendered_when_padding_consumes_all_space() {
        let panel = Panel::new(MarkerWidget)
            .borders(Borders::ALL)
            .padding(Sides::all(10));
        let area = Rect::new(0, 0, 5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);

        // Should not panic even though padding exceeds available space
        panel.render(area, &mut frame);
    }

    // --- builder chain test ---

    #[test]
    fn builder_chain_compiles() {
        let _panel = Panel::new(crate::block::Block::default())
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::new().bold())
            .title("Title")
            .title_alignment(Alignment::Center)
            .title_style(Style::new().italic())
            .subtitle("Sub")
            .subtitle_alignment(Alignment::Right)
            .subtitle_style(Style::new())
            .style(Style::new())
            .padding(Sides::all(1));
    }
}

#![forbid(unsafe_code)]

//! Panel widget: border + optional title/subtitle + inner padding + child content.

use crate::block::Alignment;
use crate::borders::{BorderSet, BorderType, Borders};
use crate::{Widget, apply_style, draw_text_span, set_style_area};
use ftui_core::geometry::{Rect, Sides};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
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

    fn render_top_text(&self, area: Rect, buf: &mut Buffer, text: &str, alignment: Alignment, style: Style) {
        if area.width < 2 {
            return;
        }

        let available_width = area.width.saturating_sub(2) as usize;
        let text = self.ellipsize(text, available_width);
        let display_width = UnicodeWidthStr::width(text.as_ref()).min(available_width);

        let x = match alignment {
            Alignment::Left => area.x + 1,
            Alignment::Center => area.x + 1 + ((available_width.saturating_sub(display_width)) / 2) as u16,
            Alignment::Right => area
                .right()
                .saturating_sub(1)
                .saturating_sub(display_width as u16),
        };

        let max_x = area.right().saturating_sub(1);
        draw_text_span(buf, x, area.y, text.as_ref(), style, max_x);
    }

    fn render_bottom_text(
        &self,
        area: Rect,
        buf: &mut Buffer,
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
            Alignment::Center => area.x + 1 + ((available_width.saturating_sub(display_width)) / 2) as u16,
            Alignment::Right => area
                .right()
                .saturating_sub(1)
                .saturating_sub(display_width as u16),
        };

        let y = area.bottom() - 1;
        let max_x = area.right().saturating_sub(1);
        draw_text_span(buf, x, y, text.as_ref(), style, max_x);
    }
}

struct ScissorGuard<'a> {
    buf: &'a mut Buffer,
}

impl<'a> ScissorGuard<'a> {
    fn new(buf: &'a mut Buffer, rect: Rect) -> Self {
        buf.push_scissor(rect);
        Self { buf }
    }
}

impl Drop for ScissorGuard<'_> {
    fn drop(&mut self) {
        self.buf.pop_scissor();
    }
}

impl<W: Widget> Widget for Panel<'_, W> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
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

        let deg = buf.degradation;

        // Skeleton+: skip everything, just clear area
        if !deg.render_content() {
            buf.fill(area, Cell::default());
            return;
        }

        // Background/style
        if deg.apply_styling() {
            set_style_area(buf, area, self.style);
        }

        // Decorative layer: borders + title/subtitle
        if deg.render_decorative() {
            let set = self.pick_border_set(buf);
            self.render_borders(area, buf, set);

            if self.borders.contains(Borders::TOP) {
                if let Some(title) = self.title {
                    let title_style = if deg.apply_styling() {
                        self.title_style.merge(&self.border_style)
                    } else {
                        Style::default()
                    };
                    self.render_top_text(area, buf, title, self.title_alignment, title_style);
                }
            }

            if self.borders.contains(Borders::BOTTOM) {
                if let Some(subtitle) = self.subtitle {
                    let subtitle_style = if deg.apply_styling() {
                        self.subtitle_style.merge(&self.border_style)
                    } else {
                        Style::default()
                    };
                    self.render_bottom_text(area, buf, subtitle, self.subtitle_alignment, subtitle_style);
                }
            }
        }

        // Content
        let mut content_area = self.inner(area);
        content_area = content_area.inner(self.padding);
        if content_area.is_empty() {
            return;
        }

        let guard = ScissorGuard::new(buf, content_area);
        self.child.render(content_area, &mut *guard.buf);
    }

    fn is_essential(&self) -> bool {
        self.child.is_essential()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipsize_short_is_borrowed() {
        let panel = Panel::new(crate::block::Block::default());
        let out = panel.ellipsize("abc", 3);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(out, "abc");
    }

    #[test]
    fn ellipsize_truncates_with_ellipsis() {
        let panel = Panel::new(crate::block::Block::default());
        let out = panel.ellipsize("abcdef", 4);
        assert_eq!(out, "abc…");
    }
}


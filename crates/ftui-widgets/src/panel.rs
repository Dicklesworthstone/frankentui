#![forbid(unsafe_code)]

//! Panel widget - bordered box with optional title and padding.
//!
//! A compositional widget that draws a border around content with:
//! - Configurable border style (square, rounded, double, heavy, ascii)
//! - Optional title with alignment (left, center, right)
//! - Optional subtitle
//! - Configurable padding between border and content
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::{Panel, Widget};
//! use ftui_render::drawing::BorderChars;
//! use ftui_text::text::Line;
//!
//! let panel = Panel::new(my_widget)
//!     .title(Line::raw("Settings"))
//!     .border_chars(BorderChars::ROUNDED);
//! panel.render(area, &mut buf);
//! ```

use crate::{StatefulWidget, Widget};
use ftui_core::geometry::{Rect, Sides};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::drawing::{BorderChars, Draw};
use ftui_text::text::Line;

/// Title alignment within the panel border.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TitleAlignment {
    /// Title aligned to the left edge (after corner).
    #[default]
    Left,
    /// Title centered in the top border.
    Center,
    /// Title aligned to the right edge (before corner).
    Right,
}

/// A panel widget that draws a border around content.
#[derive(Debug, Clone)]
pub struct Panel<W> {
    inner: W,
    title: Option<Line>,
    subtitle: Option<Line>,
    title_alignment: TitleAlignment,
    border_chars: BorderChars,
    padding: Sides,
    border_fg: Option<PackedRgba>,
    border_bg: Option<PackedRgba>,
    title_fg: Option<PackedRgba>,
    title_bg: Option<PackedRgba>,
}

impl<W> Panel<W> {
    /// Create a new panel wrapping the given widget.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            title: None,
            subtitle: None,
            title_alignment: TitleAlignment::Left,
            border_chars: BorderChars::SQUARE,
            padding: Sides::all(0),
            border_fg: None,
            border_bg: None,
            title_fg: None,
            title_bg: None,
        }
    }

    /// Set the panel title.
    #[must_use]
    pub fn title(mut self, title: Line) -> Self {
        self.title = Some(title);
        self
    }

    /// Set the panel subtitle (appears on bottom border).
    #[must_use]
    pub fn subtitle(mut self, subtitle: Line) -> Self {
        self.subtitle = Some(subtitle);
        self
    }

    /// Set title alignment.
    #[must_use]
    pub const fn title_alignment(mut self, alignment: TitleAlignment) -> Self {
        self.title_alignment = alignment;
        self
    }

    /// Set border characters.
    #[must_use]
    pub const fn border_chars(mut self, chars: BorderChars) -> Self {
        self.border_chars = chars;
        self
    }

    /// Set padding between border and content.
    #[must_use]
    pub const fn padding(mut self, padding: Sides) -> Self {
        self.padding = padding;
        self
    }

    /// Set border foreground color.
    #[must_use]
    pub const fn border_fg(mut self, color: PackedRgba) -> Self {
        self.border_fg = Some(color);
        self
    }

    /// Set border background color.
    #[must_use]
    pub const fn border_bg(mut self, color: PackedRgba) -> Self {
        self.border_bg = Some(color);
        self
    }

    /// Set title foreground color.
    #[must_use]
    pub const fn title_fg(mut self, color: PackedRgba) -> Self {
        self.title_fg = Some(color);
        self
    }

    /// Set title background color.
    #[must_use]
    pub const fn title_bg(mut self, color: PackedRgba) -> Self {
        self.title_bg = Some(color);
        self
    }

    /// Get the inner area (content area after border and padding).
    #[must_use]
    pub fn inner_area(&self, area: Rect) -> Rect {
        if area.width < 2 || area.height < 2 {
            return Rect::new(0, 0, 0, 0);
        }
        // Remove border (1 on each side)
        let after_border = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        // Remove padding
        after_border.inner(self.padding)
    }

    /// Get a reference to the inner widget.
    pub const fn inner(&self) -> &W {
        &self.inner
    }

    /// Get a mutable reference to the inner widget.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Consume and return the inner widget.
    pub fn into_inner(self) -> W {
        self.inner
    }

    fn render_border(&self, area: Rect, buf: &mut Buffer) {
        let mut border_cell = Cell::from_char(' ');
        if let Some(fg) = self.border_fg {
            border_cell = border_cell.with_fg(fg);
        }
        if let Some(bg) = self.border_bg {
            border_cell = border_cell.with_bg(bg);
        }

        buf.draw_border(area, self.border_chars, border_cell);
    }

    fn render_title(&self, area: Rect, buf: &mut Buffer, title: &Line, y: u16) {
        let title_width = title.width();
        if title_width == 0 {
            return;
        }

        // Available width for title (between corners, with 1 char padding each side)
        let available_width = area.width.saturating_sub(4) as usize;
        if available_width == 0 {
            return;
        }

        // Calculate x position based on alignment
        let x = match self.title_alignment {
            TitleAlignment::Left => area.x.saturating_add(2),
            TitleAlignment::Center => {
                let start = area.x.saturating_add(2);
                let title_w = title_width.min(available_width) as u16;
                let padding = (available_width as u16).saturating_sub(title_w) / 2;
                start.saturating_add(padding)
            }
            TitleAlignment::Right => {
                let title_w = title_width.min(available_width) as u16;
                area.right().saturating_sub(2).saturating_sub(title_w)
            }
        };

        // Set up title cell style
        let mut title_cell = Cell::from_char(' ');
        if let Some(fg) = self.title_fg.or(self.border_fg) {
            title_cell = title_cell.with_fg(fg);
        }
        if let Some(bg) = self.title_bg.or(self.border_bg) {
            title_cell = title_cell.with_bg(bg);
        }

        // Render title spans with truncation
        let max_x = area.right().saturating_sub(2);
        let mut current_x = x;
        for span in title.spans() {
            if current_x >= max_x {
                break;
            }
            let text = span.as_str();
            // Apply span style if present
            let mut cell = title_cell;
            if let Some(fg) = span.style.and_then(|s| s.fg) {
                cell = cell.with_fg(fg);
            }
            if let Some(bg) = span.style.and_then(|s| s.bg) {
                cell = cell.with_bg(bg);
            }
            current_x = buf.print_text_clipped(current_x, y, text, cell, max_x);
        }
    }
}

impl<W: Widget> Widget for Panel<W> {
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

        if area.is_empty() || area.width < 2 || area.height < 2 {
            return;
        }

        // Draw border
        self.render_border(area, buf);

        // Draw title on top border
        if let Some(title) = &self.title {
            self.render_title(area, buf, title, area.y);
        }

        // Draw subtitle on bottom border
        if let Some(subtitle) = &self.subtitle {
            self.render_title(area, buf, subtitle, area.bottom().saturating_sub(1));
        }

        // Render inner widget
        let inner_area = self.inner_area(area);
        if !inner_area.is_empty() {
            // Push scissor for inner content
            buf.push_scissor(inner_area);
            self.inner.render(inner_area, buf);
            buf.pop_scissor();
        }
    }

    fn is_essential(&self) -> bool {
        self.inner.is_essential()
    }
}

impl<W: StatefulWidget> StatefulWidget for Panel<W> {
    type State = W::State;

    fn render(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "PanelStateful",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() || area.width < 2 || area.height < 2 {
            return;
        }

        // Draw border
        self.render_border(area, buf);

        // Draw title on top border
        if let Some(title) = &self.title {
            self.render_title(area, buf, title, area.y);
        }

        // Draw subtitle on bottom border
        if let Some(subtitle) = &self.subtitle {
            self.render_title(area, buf, subtitle, area.bottom().saturating_sub(1));
        }

        // Render inner widget
        let inner_area = self.inner_area(area);
        if !inner_area.is_empty() {
            buf.push_scissor(inner_area);
            self.inner.render(inner_area, buf, state);
            buf.pop_scissor();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf_to_lines(buf: &Buffer) -> Vec<String> {
        let mut lines = Vec::new();
        for y in 0..buf.height() {
            let mut row = String::with_capacity(buf.width() as usize);
            for x in 0..buf.width() {
                let ch = buf
                    .get(x, y)
                    .and_then(|c| c.content.as_char())
                    .unwrap_or(' ');
                row.push(ch);
            }
            lines.push(row);
        }
        lines
    }

    #[derive(Debug, Clone, Copy)]
    struct Fill(char);

    impl Widget for Fill {
        fn render(&self, area: Rect, buf: &mut Buffer) {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    buf.set(x, y, Cell::from_char(self.0));
                }
            }
        }
    }

    #[test]
    fn panel_draws_square_border() {
        let panel = Panel::new(Fill('X'));
        let area = Rect::from_size(5, 4);
        let mut buf = Buffer::new(5, 4);
        panel.render(area, &mut buf);

        let lines = buf_to_lines(&buf);
        assert_eq!(lines[0], "┌───┐");
        assert_eq!(lines[1], "│XXX│");
        assert_eq!(lines[2], "│XXX│");
        assert_eq!(lines[3], "└───┘");
    }

    #[test]
    fn panel_draws_rounded_border() {
        let panel = Panel::new(Fill('X')).border_chars(BorderChars::ROUNDED);
        let area = Rect::from_size(5, 3);
        let mut buf = Buffer::new(5, 3);
        panel.render(area, &mut buf);

        let lines = buf_to_lines(&buf);
        assert_eq!(lines[0], "╭───╮");
        assert_eq!(lines[1], "│XXX│");
        assert_eq!(lines[2], "╰───╯");
    }

    #[test]
    fn panel_draws_ascii_border() {
        let panel = Panel::new(Fill('.')).border_chars(BorderChars::ASCII);
        let area = Rect::from_size(6, 4);
        let mut buf = Buffer::new(6, 4);
        panel.render(area, &mut buf);

        let lines = buf_to_lines(&buf);
        assert_eq!(lines[0], "+----+");
        assert_eq!(lines[1], "|....|");
        assert_eq!(lines[2], "|....|");
        assert_eq!(lines[3], "+----+");
    }

    #[test]
    fn panel_with_title_left() {
        let panel = Panel::new(Fill(' '))
            .title(Line::raw("Hi"))
            .title_alignment(TitleAlignment::Left);
        let area = Rect::from_size(10, 3);
        let mut buf = Buffer::new(10, 3);
        panel.render(area, &mut buf);

        let lines = buf_to_lines(&buf);
        // Title appears after corner + 1 space
        assert_eq!(lines[0], "┌─Hi─────┐");
    }

    #[test]
    fn panel_with_title_center() {
        let panel = Panel::new(Fill(' '))
            .title(Line::raw("Hi"))
            .title_alignment(TitleAlignment::Center);
        let area = Rect::from_size(12, 3);
        let mut buf = Buffer::new(12, 3);
        panel.render(area, &mut buf);

        let lines = buf_to_lines(&buf);
        // Title centered in available space
        assert_eq!(lines[0], "┌────Hi────┐");
    }

    #[test]
    fn panel_with_title_right() {
        let panel = Panel::new(Fill(' '))
            .title(Line::raw("Hi"))
            .title_alignment(TitleAlignment::Right);
        let area = Rect::from_size(10, 3);
        let mut buf = Buffer::new(10, 3);
        panel.render(area, &mut buf);

        let lines = buf_to_lines(&buf);
        // Title at right before corner
        assert_eq!(lines[0], "┌─────Hi─┐");
    }

    #[test]
    fn panel_with_subtitle() {
        let panel = Panel::new(Fill(' '))
            .title(Line::raw("Top"))
            .subtitle(Line::raw("Bot"));
        let area = Rect::from_size(10, 3);
        let mut buf = Buffer::new(10, 3);
        panel.render(area, &mut buf);

        let lines = buf_to_lines(&buf);
        assert_eq!(lines[0], "┌─Top────┐");
        assert_eq!(lines[2], "└─Bot────┘");
    }

    #[test]
    fn panel_truncates_long_title() {
        let panel = Panel::new(Fill(' ')).title(Line::raw("VeryLongTitle"));
        let area = Rect::from_size(8, 3);
        let mut buf = Buffer::new(8, 3);
        panel.render(area, &mut buf);

        let lines = buf_to_lines(&buf);
        // Title truncated to fit (4 chars available)
        assert_eq!(lines[0], "┌─Very─┐");
    }

    #[test]
    fn panel_inner_area_calculation() {
        let panel = Panel::new(Fill(' ')).padding(Sides::all(1));
        let area = Rect::from_size(10, 6);
        let inner = panel.inner_area(area);
        // Border takes 1 on each side, padding takes 1 more on each side
        assert_eq!(inner, Rect::new(2, 2, 6, 2));
    }

    #[test]
    fn panel_with_padding() {
        let panel = Panel::new(Fill('X')).padding(Sides::all(1));
        let area = Rect::from_size(7, 5);
        let mut buf = Buffer::new(7, 5);
        panel.render(area, &mut buf);

        let lines = buf_to_lines(&buf);
        assert_eq!(lines[0], "┌─────┐");
        assert_eq!(lines[1], "│     │");
        assert_eq!(lines[2], "│ XXX │");
        assert_eq!(lines[3], "│     │");
        assert_eq!(lines[4], "└─────┘");
    }

    #[test]
    fn panel_too_small_for_content() {
        let panel = Panel::new(Fill('X'));
        let area = Rect::from_size(2, 2);
        let mut buf = Buffer::new(2, 2);
        panel.render(area, &mut buf);

        // Just border, no content
        let lines = buf_to_lines(&buf);
        assert_eq!(lines[0], "┌┐");
        assert_eq!(lines[1], "└┘");
    }

    #[test]
    fn panel_empty_area_is_noop() {
        let panel = Panel::new(Fill('X'));
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::new(5, 5);
        panel.render(area, &mut buf);

        // Buffer unchanged
        assert!(buf.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn panel_border_fg_is_applied() {
        let panel = Panel::new(Fill(' ')).border_fg(PackedRgba::rgb(255, 0, 0));
        let area = Rect::from_size(4, 3);
        let mut buf = Buffer::new(4, 3);
        panel.render(area, &mut buf);

        let cell = buf.get(0, 0).unwrap();
        assert_eq!(cell.fg, PackedRgba::rgb(255, 0, 0));
    }

    #[test]
    fn panel_inner_area_zero_for_tiny_area() {
        let panel = Panel::new(Fill(' '));
        assert_eq!(panel.inner_area(Rect::from_size(1, 1)), Rect::new(0, 0, 0, 0));
        assert_eq!(panel.inner_area(Rect::from_size(0, 0)), Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn panel_accessors_work() {
        let panel = Panel::new(Fill('X'));
        assert_eq!(panel.inner().0, 'X');

        let mut panel = panel;
        panel.inner_mut().0 = 'Y';
        assert_eq!(panel.inner().0, 'Y');

        let fill = panel.into_inner();
        assert_eq!(fill.0, 'Y');
    }

    #[test]
    fn panel_is_essential_delegates() {
        #[derive(Debug)]
        struct Essential;
        impl Widget for Essential {
            fn render(&self, _: Rect, _: &mut Buffer) {}
            fn is_essential(&self) -> bool {
                true
            }
        }

        let panel = Panel::new(Essential);
        assert!(panel.is_essential());

        let panel2 = Panel::new(Fill(' '));
        assert!(!panel2.is_essential());
    }
}

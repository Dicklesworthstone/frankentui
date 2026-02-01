#![forbid(unsafe_code)]

//! Progress bar widget.

use crate::block::Block;
use crate::{Widget, set_style_area};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_style::Style;

/// A widget to display a progress bar.
#[derive(Debug, Clone, Default)]
pub struct ProgressBar<'a> {
    block: Option<Block<'a>>,
    ratio: f64,
    label: Option<&'a str>,
    style: Style,
    gauge_style: Style,
}

impl<'a> ProgressBar<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    pub fn ratio(mut self, ratio: f64) -> Self {
        self.ratio = ratio.clamp(0.0, 1.0);
        self
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn gauge_style(mut self, style: Style) -> Self {
        self.gauge_style = style;
        self
    }
}

impl<'a> Widget for ProgressBar<'a> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "ProgressBar",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        let deg = buf.degradation;

        // Skeleton+: skip entirely
        if !deg.render_content() {
            return;
        }

        // EssentialOnly: just show percentage text, no bar
        if !deg.render_decorative() {
            let pct = format!("{}%", (self.ratio * 100.0) as u8);
            crate::draw_text_span(buf, area.x, area.y, &pct, Style::default(), area.right());
            return;
        }

        let bar_area = match &self.block {
            Some(b) => {
                b.render(area, buf);
                b.inner(area)
            }
            None => area,
        };

        if bar_area.is_empty() {
            return;
        }

        if deg.apply_styling() {
            set_style_area(buf, bar_area, self.style);
        }

        let max_width = bar_area.width as f64;
        let filled_width = if self.ratio >= 1.0 {
            bar_area.width
        } else {
            (max_width * self.ratio).floor() as u16
        };

        // Draw filled part
        let gauge_style = if deg.apply_styling() {
            self.gauge_style
        } else {
            // At NoStyling, use '#' as fill char instead of background color
            Style::default()
        };
        let fill_char = if deg.apply_styling() { ' ' } else { '#' };

        for y in bar_area.top()..bar_area.bottom() {
            for x in 0..filled_width {
                let cell_x = bar_area.left() + x;
                if cell_x < bar_area.right() {
                    let mut cell = Cell::from_char(fill_char);
                    crate::apply_style(&mut cell, gauge_style);
                    buf.set(cell_x, y, cell);
                }
            }
        }

        // Draw label (centered)
        let label_style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };
        if let Some(label) = self.label {
            let label_width = unicode_width::UnicodeWidthStr::width(label);
            let label_x = bar_area.left()
                + ((bar_area.width as usize).saturating_sub(label_width) / 2) as u16;
            let label_y = bar_area.top() + (bar_area.height / 2);

            crate::draw_text_span(buf, label_x, label_y, label, label_style, bar_area.right());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::PackedRgba;

    // --- Builder tests ---

    #[test]
    fn default_progress_bar() {
        let pb = ProgressBar::new();
        assert_eq!(pb.ratio, 0.0);
        assert!(pb.label.is_none());
        assert!(pb.block.is_none());
    }

    #[test]
    fn ratio_clamped_above_one() {
        let pb = ProgressBar::new().ratio(1.5);
        assert_eq!(pb.ratio, 1.0);
    }

    #[test]
    fn ratio_clamped_below_zero() {
        let pb = ProgressBar::new().ratio(-0.5);
        assert_eq!(pb.ratio, 0.0);
    }

    #[test]
    fn ratio_normal_range() {
        let pb = ProgressBar::new().ratio(0.5);
        assert!((pb.ratio - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn builder_label() {
        let pb = ProgressBar::new().label("50%");
        assert_eq!(pb.label, Some("50%"));
    }

    // --- Rendering tests ---

    #[test]
    fn render_zero_area() {
        let pb = ProgressBar::new().ratio(0.5);
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::new(1, 1);
        Widget::render(&pb, area, &mut buf);
        // Should not panic
    }

    #[test]
    fn render_zero_ratio_no_fill() {
        let gauge_style = Style::new().bg(PackedRgba::RED);
        let pb = ProgressBar::new().ratio(0.0).gauge_style(gauge_style);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        Widget::render(&pb, area, &mut buf);

        // No cells should have the gauge style bg
        for x in 0..10 {
            let cell = buf.get(x, 0).unwrap();
            assert_ne!(
                cell.bg,
                PackedRgba::RED,
                "cell at x={x} should not have gauge bg"
            );
        }
    }

    #[test]
    fn render_full_ratio_fills_all() {
        let gauge_style = Style::new().bg(PackedRgba::GREEN);
        let pb = ProgressBar::new().ratio(1.0).gauge_style(gauge_style);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        Widget::render(&pb, area, &mut buf);

        // All cells should have gauge bg
        for x in 0..10 {
            let cell = buf.get(x, 0).unwrap();
            assert_eq!(
                cell.bg,
                PackedRgba::GREEN,
                "cell at x={x} should have gauge bg"
            );
        }
    }

    #[test]
    fn render_half_ratio() {
        let gauge_style = Style::new().bg(PackedRgba::BLUE);
        let pb = ProgressBar::new().ratio(0.5).gauge_style(gauge_style);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        Widget::render(&pb, area, &mut buf);

        // About 5 cells should be filled (10 * 0.5 = 5)
        let filled_count = (0..10)
            .filter(|&x| buf.get(x, 0).unwrap().bg == PackedRgba::BLUE)
            .count();
        assert_eq!(filled_count, 5);
    }

    #[test]
    fn render_multi_row_bar() {
        let gauge_style = Style::new().bg(PackedRgba::RED);
        let pb = ProgressBar::new().ratio(1.0).gauge_style(gauge_style);
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::new(5, 3);
        Widget::render(&pb, area, &mut buf);

        // All 3 rows should be filled
        for y in 0..3 {
            for x in 0..5 {
                let cell = buf.get(x, y).unwrap();
                assert_eq!(
                    cell.bg,
                    PackedRgba::RED,
                    "cell at ({x},{y}) should have gauge bg"
                );
            }
        }
    }

    #[test]
    fn render_with_label_centered() {
        let pb = ProgressBar::new().ratio(0.5).label("50%");
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        Widget::render(&pb, area, &mut buf);

        // Label "50%" is 3 chars wide, centered in 10 = starts at x=3
        // (10 - 3) / 2 = 3
        let c = buf.get(3, 0).and_then(|c| c.content.as_char());
        assert_eq!(c, Some('5'));
        let c = buf.get(4, 0).and_then(|c| c.content.as_char());
        assert_eq!(c, Some('0'));
        let c = buf.get(5, 0).and_then(|c| c.content.as_char());
        assert_eq!(c, Some('%'));
    }

    #[test]
    fn render_with_block() {
        let pb = ProgressBar::new()
            .ratio(1.0)
            .gauge_style(Style::new().bg(PackedRgba::GREEN))
            .block(Block::bordered());
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::new(10, 3);
        Widget::render(&pb, area, &mut buf);

        // Inner area is 8x1 (border takes 1 on each side)
        // All inner cells should have gauge bg
        for x in 1..9 {
            let cell = buf.get(x, 1).unwrap();
            assert_eq!(
                cell.bg,
                PackedRgba::GREEN,
                "inner cell at x={x} should have gauge bg"
            );
        }
    }

    // --- Degradation tests ---

    #[test]
    fn degradation_skeleton_skips_entirely() {
        use ftui_render::budget::DegradationLevel;

        let pb = ProgressBar::new()
            .ratio(0.5)
            .gauge_style(Style::new().bg(PackedRgba::GREEN));
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        buf.degradation = DegradationLevel::Skeleton;
        Widget::render(&pb, area, &mut buf);

        // Nothing should be rendered
        for x in 0..10 {
            assert!(
                buf.get(x, 0).unwrap().is_empty(),
                "cell at x={x} should be empty at Skeleton"
            );
        }
    }

    #[test]
    fn degradation_essential_only_shows_percentage() {
        use ftui_render::budget::DegradationLevel;

        let pb = ProgressBar::new()
            .ratio(0.5)
            .gauge_style(Style::new().bg(PackedRgba::GREEN));
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        buf.degradation = DegradationLevel::EssentialOnly;
        Widget::render(&pb, area, &mut buf);

        // Should show "50%" text, no gauge bar
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('5'));
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('0'));
        assert_eq!(buf.get(2, 0).unwrap().content.as_char(), Some('%'));
        // No gauge background color
        assert_ne!(buf.get(0, 0).unwrap().bg, PackedRgba::GREEN);
    }

    #[test]
    fn degradation_full_renders_bar() {
        use ftui_render::budget::DegradationLevel;

        let pb = ProgressBar::new()
            .ratio(1.0)
            .gauge_style(Style::new().bg(PackedRgba::BLUE));
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::new(10, 1);
        buf.degradation = DegradationLevel::Full;
        Widget::render(&pb, area, &mut buf);

        // All cells should have gauge bg
        for x in 0..10 {
            assert_eq!(
                buf.get(x, 0).unwrap().bg,
                PackedRgba::BLUE,
                "cell at x={x} should have gauge bg at Full"
            );
        }
    }
}

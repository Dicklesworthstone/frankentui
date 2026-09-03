#![forbid(unsafe_code)]

//! Progress bar widget.

use crate::block::Block;
use crate::{
    MeasurableWidget, SizeConstraints, Widget, apply_style, clear_text_area, clear_text_row,
};
use ftui_core::geometry::{Rect, Size};
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::display_width;

/// How a [`ProgressBar`] fills: a known fraction, or an animated marquee for
/// work of unknown duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProgressMode {
    /// Fill proportional to the bar's `ratio` (the default).
    #[default]
    Determinate,
    /// A fixed-width segment sweeps across the bar; `ratio` is ignored. The
    /// caller supplies the animation `phase` (a tick or frame counter), so the
    /// widget stays a stateless [`Widget`] and its output is deterministic.
    Indeterminate {
        /// Animation phase, typically a monotonically increasing tick count.
        phase: u64,
    },
}

/// A widget to display a progress bar.
#[derive(Debug, Clone, Default)]
pub struct ProgressBar<'a> {
    block: Option<Block<'a>>,
    ratio: f64,
    label: Option<&'a str>,
    style: Style,
    gauge_style: Style,
    mode: ProgressMode,
}

impl<'a> ProgressBar<'a> {
    /// Create a new progress bar with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the surrounding block.
    #[must_use]
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Set the progress ratio (clamped to 0.0..=1.0).
    #[must_use]
    pub fn ratio(mut self, ratio: f64) -> Self {
        self.ratio = if ratio.is_nan() {
            0.0
        } else {
            ratio.clamp(0.0, 1.0)
        };
        self
    }

    /// Set the centered label text.
    #[must_use]
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    /// Set the base style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the filled portion style.
    #[must_use]
    pub fn gauge_style(mut self, style: Style) -> Self {
        self.gauge_style = style;
        self
    }

    /// Switch to indeterminate mode with the given animation `phase`.
    ///
    /// In this mode a fixed-width segment sweeps across the bar and `ratio` is
    /// ignored; the label (if any) is still drawn. The caller supplies `phase`
    /// — usually a tick or frame counter driven from `Cmd::Tick`/`Every` — so
    /// the widget remains a stateless [`Widget`] (like [`Sparkline`], unlike the
    /// stateful `Spinner`) and renders deterministically for a given phase.
    ///
    /// [`Sparkline`]: crate::sparkline::Sparkline
    #[must_use]
    pub fn indeterminate(mut self, phase: u64) -> Self {
        self.mode = ProgressMode::Indeterminate { phase };
        self
    }

    /// Set the fill mode explicitly.
    #[must_use]
    pub fn mode(mut self, mode: ProgressMode) -> Self {
        self.mode = mode;
        self
    }

    /// Whether the bar is in indeterminate (marquee) mode.
    #[must_use]
    pub fn is_indeterminate(&self) -> bool {
        matches!(self.mode, ProgressMode::Indeterminate { .. })
    }

    /// The visible `[start, end)` columns of the marquee segment for a bar of
    /// `inner_width` cells at animation `phase`, or `None` when the width is 0.
    ///
    /// The segment is `max(3, inner_width / 5)` cells wide and its period is
    /// `inner_width + seg`, so it enters from the left, crosses, and leaves at
    /// the right before wrapping. The returned range is always clamped inside
    /// `[0, inner_width)` (it may be empty while the segment is off-screen).
    #[must_use]
    pub fn marquee_span(inner_width: u16, phase: u64) -> Option<(u16, u16)> {
        if inner_width == 0 {
            return None;
        }
        let width = u64::from(inner_width);
        let seg = (width / 5).max(3);
        let period = width + seg;
        let pos = (phase % period) as i64 - seg as i64;
        let start = pos.max(0);
        let end = (pos + seg as i64).clamp(0, width as i64);
        Some((start as u16, end as u16))
    }

    /// A short label of the current mode, for the render span.
    #[cfg(feature = "tracing")]
    fn mode_label(&self) -> &'static str {
        match self.mode {
            ProgressMode::Determinate => "determinate",
            ProgressMode::Indeterminate { .. } => "indeterminate",
        }
    }
}

impl<'a> Widget for ProgressBar<'a> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if frame.a11y_enabled() {
            frame.push_a11y_nodes(ftui_a11y::Accessible::accessibility_nodes(self, area));
        }
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "ProgressBar",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height,
            mode = self.mode_label()
        )
        .entered();

        let deg = frame.buffer.degradation;

        // Skeleton+: skip entirely
        if !deg.render_content() {
            return;
        }

        // EssentialOnly: just show text, no bar. Determinate shows a
        // percentage; indeterminate has none, so it shows the label or "...".
        if !deg.render_decorative() {
            clear_text_area(frame, area, Style::default());
            let text = match self.mode {
                ProgressMode::Determinate => format!("{}%", (self.ratio * 100.0) as u8),
                ProgressMode::Indeterminate { .. } => {
                    self.label.map_or_else(|| "...".to_string(), String::from)
                }
            };
            crate::draw_text_span(frame, area.x, area.y, &text, Style::default(), area.right());
            return;
        }

        let base_style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };

        clear_text_area(frame, area, base_style);

        let bar_area = match &self.block {
            Some(b) => {
                b.render(area, frame);
                b.inner(area)
            }
            None => area,
        };

        if bar_area.is_empty() {
            return;
        }

        // The gauge-styled columns: a left-anchored fill in determinate mode,
        // or the swept marquee segment in indeterminate mode. The determinate
        // range is byte-for-byte what it was before this branch existed.
        let (fill_start, fill_end) = match self.mode {
            ProgressMode::Determinate => {
                let max_width = bar_area.width as f64;
                let filled_width = if self.ratio >= 1.0 {
                    bar_area.width
                } else {
                    (max_width * self.ratio).floor() as u16
                };
                (0, filled_width)
            }
            ProgressMode::Indeterminate { phase } => {
                Self::marquee_span(bar_area.width, phase).unwrap_or((0, 0))
            }
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
            for x in fill_start..fill_end {
                let cell_x = bar_area.left().saturating_add(x);
                if cell_x < bar_area.right() {
                    let mut cell = Cell::from_char(fill_char);
                    crate::apply_style(&mut cell, gauge_style);
                    frame.buffer.set_fast(cell_x, y, cell);
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
            let label_width = display_width(label);
            let label_x = bar_area
                .left()
                .saturating_add(((bar_area.width as usize).saturating_sub(label_width) / 2) as u16);
            let label_y = bar_area.top().saturating_add(bar_area.height / 2);

            crate::draw_text_span(
                frame,
                label_x,
                label_y,
                label,
                label_style,
                bar_area.right(),
            );
        }
    }
}

impl MeasurableWidget for ProgressBar<'_> {
    fn measure(&self, _available: Size) -> SizeConstraints {
        // ProgressBar fills available width, has fixed height of 1 (or block inner height)
        let (block_width, block_height) = self
            .block
            .as_ref()
            .map(|b| {
                let inner = b.inner(Rect::new(0, 0, 100, 100));
                let w_overhead = 100u16.saturating_sub(inner.width);
                let h_overhead = 100u16.saturating_sub(inner.height);
                (w_overhead, h_overhead)
            })
            .unwrap_or((0, 0));

        // Minimum: 1 cell for bar + block overhead
        // Preferred: fills available width, 1 row + block overhead
        let min_width = 1u16.saturating_add(block_width);
        let min_height = 1u16.saturating_add(block_height);

        SizeConstraints {
            min: Size::new(min_width, min_height),
            preferred: Size::new(min_width, min_height), // Fills width, so preferred = min
            max: None,                                   // Can grow to fill available space
        }
    }

    fn has_intrinsic_size(&self) -> bool {
        // ProgressBar fills width, so it doesn't have true intrinsic width
        // but it does have intrinsic height
        true
    }
}

// ---------------------------------------------------------------------------
// Accessibility
// ---------------------------------------------------------------------------

impl ftui_a11y::Accessible for ProgressBar<'_> {
    fn accessibility_nodes(&self, area: Rect) -> Vec<ftui_a11y::node::A11yNodeInfo> {
        use ftui_a11y::node::{A11yNodeInfo, A11yRole, A11yState};

        let id = crate::a11y_node_id(area);
        // Indeterminate bars have no meaningful value: omit value_now (like an
        // ARIA indeterminate progressbar) and describe them as "in progress".
        let state = if self.is_indeterminate() {
            A11yState {
                value_text: Some("in progress".to_string()),
                ..A11yState::default()
            }
        } else {
            let pct = (self.ratio * 100.0).round() as u32;
            A11yState {
                value_now: Some(self.ratio),
                value_min: Some(0.0),
                value_max: Some(1.0),
                value_text: Some(format!("{pct}%")),
                ..A11yState::default()
            }
        };
        let name = self.label.map_or_else(
            || {
                state
                    .value_text
                    .clone()
                    .unwrap_or_else(|| "progress".to_string())
            },
            String::from,
        );

        vec![
            A11yNodeInfo::new(id, A11yRole::ProgressBar, area)
                .with_name(name)
                .with_state(state),
        ]
    }
}

// ---------------------------------------------------------------------------
// MiniBar
// ---------------------------------------------------------------------------

/// Color thresholds for [`MiniBar`].
#[derive(Debug, Clone, Copy)]
pub struct MiniBarColors {
    pub high: PackedRgba,
    pub mid: PackedRgba,
    pub low: PackedRgba,
    pub critical: PackedRgba,
}

impl MiniBarColors {
    pub fn new(high: PackedRgba, mid: PackedRgba, low: PackedRgba, critical: PackedRgba) -> Self {
        Self {
            high,
            mid,
            low,
            critical,
        }
    }
}

impl Default for MiniBarColors {
    fn default() -> Self {
        Self {
            high: PackedRgba::rgb(64, 200, 120),
            mid: PackedRgba::rgb(255, 180, 64),
            low: PackedRgba::rgb(80, 200, 240),
            critical: PackedRgba::rgb(160, 160, 160),
        }
    }
}

/// Thresholds for mapping values to colors.
#[derive(Debug, Clone, Copy)]
pub struct MiniBarThresholds {
    pub high: f64,
    pub mid: f64,
    pub low: f64,
}

impl Default for MiniBarThresholds {
    fn default() -> Self {
        Self {
            high: 0.75,
            mid: 0.50,
            low: 0.25,
        }
    }
}

/// Compact progress indicator for dashboard-style metrics.
#[derive(Debug, Clone)]
pub struct MiniBar {
    value: f64,
    width: u16,
    show_percent: bool,
    style: Style,
    filled_char: char,
    empty_char: char,
    colors: MiniBarColors,
    thresholds: MiniBarThresholds,
}

impl MiniBar {
    /// Create a new MiniBar with value in the 0.0..=1.0 range.
    pub fn new(value: f64, width: u16) -> Self {
        Self {
            value,
            width,
            show_percent: false,
            style: Style::new(),
            filled_char: '█',
            empty_char: '░',
            colors: MiniBarColors::default(),
            thresholds: MiniBarThresholds::default(),
        }
    }

    /// Override the value (clamped to 0.0..=1.0).
    #[must_use]
    pub fn value(mut self, value: f64) -> Self {
        self.value = value;
        self
    }

    /// Override the displayed width.
    #[must_use]
    pub fn width(mut self, width: u16) -> Self {
        self.width = width;
        self
    }

    /// Enable or disable percentage text.
    #[must_use]
    pub fn show_percent(mut self, show: bool) -> Self {
        self.show_percent = show;
        self
    }

    /// Set the base style for the bar.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Override the filled block character.
    #[must_use]
    pub fn filled_char(mut self, ch: char) -> Self {
        self.filled_char = ch;
        self
    }

    /// Override the empty block character.
    #[must_use]
    pub fn empty_char(mut self, ch: char) -> Self {
        self.empty_char = ch;
        self
    }

    /// Override the color thresholds.
    #[must_use]
    pub fn thresholds(mut self, thresholds: MiniBarThresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    /// Override the color palette.
    #[must_use]
    pub fn colors(mut self, colors: MiniBarColors) -> Self {
        self.colors = colors;
        self
    }

    /// Map a value to a color using default thresholds.
    pub fn color_for_value(value: f64) -> PackedRgba {
        let v = if value.is_finite() { value } else { 0.0 };
        let v = v.clamp(0.0, 1.0);
        let thresholds = MiniBarThresholds::default();
        let colors = MiniBarColors::default();
        if v > thresholds.high {
            colors.high
        } else if v > thresholds.mid {
            colors.mid
        } else if v > thresholds.low {
            colors.low
        } else {
            colors.critical
        }
    }

    /// Render the bar as a string (for testing/debugging).
    pub fn render_string(&self) -> String {
        let width = self.width as usize;
        if width == 0 {
            return String::new();
        }
        let filled = self.filled_cells(width);
        let empty = width.saturating_sub(filled);
        let mut out = String::with_capacity(width);
        out.extend(std::iter::repeat_n(self.filled_char, filled));
        out.extend(std::iter::repeat_n(self.empty_char, empty));
        out
    }

    fn normalized_value(&self) -> f64 {
        if self.value.is_finite() {
            self.value.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    fn filled_cells(&self, width: usize) -> usize {
        if width == 0 {
            return 0;
        }
        let v = self.normalized_value();
        let filled = (v * width as f64).round() as usize;
        filled.min(width)
    }

    fn color_for_value_with_palette(&self, value: f64) -> PackedRgba {
        let v = if value.is_finite() { value } else { 0.0 };
        let v = v.clamp(0.0, 1.0);
        if v > self.thresholds.high {
            self.colors.high
        } else if v > self.thresholds.mid {
            self.colors.mid
        } else if v > self.thresholds.low {
            self.colors.low
        } else {
            self.colors.critical
        }
    }
}

impl Widget for MiniBar {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "MiniBar",
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
        if !deg.render_content() {
            return;
        }

        let base_style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };
        clear_text_row(frame, area, base_style);

        let value = self.normalized_value();

        if !deg.render_decorative() {
            if self.show_percent {
                let pct = format!("{:3.0}%", value * 100.0);
                let pct_width = display_width(&pct) as u16;
                if area.width >= pct_width {
                    let text_x = area.right().saturating_sub(pct_width);
                    crate::draw_text_span(
                        frame,
                        text_x,
                        area.y,
                        &pct,
                        Style::default(),
                        area.right(),
                    );
                } else {
                    crate::draw_text_span(
                        frame,
                        area.x,
                        area.y,
                        pct.trim_start(),
                        Style::default(),
                        area.right(),
                    );
                }
            }
            return;
        }

        let mut bar_width = self.width.min(area.width) as usize;
        let mut render_percent = false;
        let mut percent_text = String::new();
        let mut percent_only_text = String::new();
        let percent_width = if self.show_percent {
            percent_text = format!(" {:3.0}%", value * 100.0);
            percent_only_text = percent_text.trim_start().to_owned();
            render_percent = true;
            display_width(&percent_text) as u16
        } else {
            0
        };

        if render_percent {
            if area.width <= percent_width {
                bar_width = 0;
            } else {
                let available = area.width - percent_width;
                bar_width = bar_width.min(available as usize);
            }
        }

        if bar_width == 0 {
            if render_percent {
                crate::draw_text_span(
                    frame,
                    area.x,
                    area.y,
                    &percent_only_text,
                    Style::default(),
                    area.right(),
                );
            }
            return;
        }

        let color = self.color_for_value_with_palette(value);
        let filled = self.filled_cells(bar_width);

        for i in 0..bar_width {
            let x = area.x + i as u16;
            if x >= area.right() {
                break;
            }
            let ch = if i < filled {
                self.filled_char
            } else {
                self.empty_char
            };
            let mut cell = Cell::from_char(ch);
            if deg.apply_styling() {
                apply_style(&mut cell, self.style);
                if i < filled {
                    cell.fg = color;
                }
            }
            frame.buffer.set_fast(x, area.y, cell);
        }

        if render_percent {
            let text_x = area.x + bar_width as u16;
            crate::draw_text_span(
                frame,
                text_x,
                area.y,
                &percent_text,
                Style::default(),
                area.right(),
            );
        }
    }
}

impl MeasurableWidget for MiniBar {
    fn measure(&self, _available: Size) -> SizeConstraints {
        // MiniBar has fixed dimensions
        let percent_width = if self.show_percent { 5 } else { 0 }; // " XXX%"
        let total_width = self.width.saturating_add(percent_width);

        SizeConstraints {
            min: Size::new(1, 1), // At least show something
            preferred: Size::new(total_width, 1),
            max: Some(Size::new(total_width, 1)), // Fixed size
        }
    }

    fn has_intrinsic_size(&self) -> bool {
        self.width > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::PackedRgba;
    use ftui_render::grapheme_pool::GraphemePool;

    fn cell_at(frame: &Frame, x: u16, y: u16) -> Cell {
        let cell = frame.buffer.get(x, y).copied();
        assert!(cell.is_some(), "test cell should exist at ({x},{y})");
        cell.unwrap()
    }

    fn raw_row_text(frame: &Frame, y: u16, width: u16) -> String {
        (0..width)
            .map(|x| {
                frame
                    .buffer
                    .get(x, y)
                    .and_then(|cell| cell.content.as_char())
                    .unwrap_or(' ')
            })
            .collect()
    }

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
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Widget::render(&pb, area, &mut frame);
        // Should not panic
    }

    #[test]
    fn render_zero_ratio_no_fill() {
        let gauge_style = Style::new().bg(PackedRgba::RED);
        let pb = ProgressBar::new().ratio(0.0).gauge_style(gauge_style);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        Widget::render(&pb, area, &mut frame);

        // No cells should have the gauge style bg
        for x in 0..10 {
            let cell = cell_at(&frame, x, 0);
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
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        Widget::render(&pb, area, &mut frame);

        // All cells should have gauge bg
        for x in 0..10 {
            let cell = cell_at(&frame, x, 0);
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
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        Widget::render(&pb, area, &mut frame);

        // About 5 cells should be filled (10 * 0.5 = 5)
        let filled_count = (0..10)
            .filter(|&x| cell_at(&frame, x, 0).bg == PackedRgba::BLUE)
            .count();
        assert_eq!(filled_count, 5);
    }

    #[test]
    fn render_multi_row_bar() {
        let gauge_style = Style::new().bg(PackedRgba::RED);
        let pb = ProgressBar::new().ratio(1.0).gauge_style(gauge_style);
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        Widget::render(&pb, area, &mut frame);

        // All 3 rows should be filled
        for y in 0..3 {
            for x in 0..5 {
                let cell = cell_at(&frame, x, y);
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
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        Widget::render(&pb, area, &mut frame);

        // Label "50%" is 3 chars wide, centered in 10 = starts at x=3
        // (10 - 3) / 2 = 3
        let c = frame.buffer.get(3, 0).and_then(|c| c.content.as_char());
        assert_eq!(c, Some('5'));
        let c = frame.buffer.get(4, 0).and_then(|c| c.content.as_char());
        assert_eq!(c, Some('0'));
        let c = frame.buffer.get(5, 0).and_then(|c| c.content.as_char());
        assert_eq!(c, Some('%'));
    }

    #[test]
    fn render_with_block() {
        let pb = ProgressBar::new()
            .ratio(1.0)
            .gauge_style(Style::new().bg(PackedRgba::GREEN))
            .block(Block::bordered());
        // Block::bordered() includes 1 cell padding on each side, so we need
        // at least 5 rows to have a 1-row inner content area.
        let area = Rect::new(0, 0, 10, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        Widget::render(&pb, area, &mut frame);

        // Inner area is 6x1 (borders + padding take 2 on each side)
        // All inner cells should have gauge bg
        for x in 2..8 {
            let cell = cell_at(&frame, x, 2);
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
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::Skeleton;
        Widget::render(&pb, area, &mut frame);

        // Nothing should be rendered
        for x in 0..10 {
            assert!(
                cell_at(&frame, x, 0).is_empty(),
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
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::EssentialOnly;
        Widget::render(&pb, area, &mut frame);

        // Should show "50%" text, no gauge bar
        assert_eq!(cell_at(&frame, 0, 0).content.as_char(), Some('5'));
        assert_eq!(cell_at(&frame, 1, 0).content.as_char(), Some('0'));
        assert_eq!(cell_at(&frame, 2, 0).content.as_char(), Some('%'));
        // No gauge background color
        assert_ne!(cell_at(&frame, 0, 0).bg, PackedRgba::GREEN);
    }

    #[test]
    fn degradation_full_renders_bar() {
        use ftui_render::budget::DegradationLevel;

        let pb = ProgressBar::new()
            .ratio(1.0)
            .gauge_style(Style::new().bg(PackedRgba::BLUE));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::Full;
        Widget::render(&pb, area, &mut frame);

        // All cells should have gauge bg
        for x in 0..10 {
            assert_eq!(
                cell_at(&frame, x, 0).bg,
                PackedRgba::BLUE,
                "cell at x={x} should have gauge bg at Full"
            );
        }
    }

    #[test]
    fn render_no_styling_ratio_shrink_clears_stale_fill() {
        use ftui_render::budget::DegradationLevel;

        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::NoStyling;

        Widget::render(&ProgressBar::new().ratio(0.8), area, &mut frame);
        Widget::render(&ProgressBar::new().ratio(0.2), area, &mut frame);

        assert_eq!(raw_row_text(&frame, 0, 10), "##        ");
    }

    #[test]
    fn degradation_essential_only_clears_previous_bar_content() {
        use ftui_render::budget::DegradationLevel;

        let pb = ProgressBar::new().ratio(0.5);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);

        Widget::render(&pb, area, &mut frame);
        frame.buffer.degradation = DegradationLevel::EssentialOnly;
        Widget::render(&pb, area, &mut frame);

        assert_eq!(raw_row_text(&frame, 0, 10), "50%       ");
    }

    // --- MiniBar tests ---

    #[test]
    fn minibar_zero_is_empty() {
        let bar = MiniBar::new(0.0, 10);
        let filled = bar.render_string().chars().filter(|c| *c == '█').count();
        assert_eq!(filled, 0);
    }

    #[test]
    fn minibar_full_is_complete() {
        let bar = MiniBar::new(1.0, 10);
        let filled = bar.render_string().chars().filter(|c| *c == '█').count();
        assert_eq!(filled, 10);
    }

    #[test]
    fn minibar_half_is_half() {
        let bar = MiniBar::new(0.5, 10);
        let filled = bar.render_string().chars().filter(|c| *c == '█').count();
        assert!((4..=6).contains(&filled));
    }

    #[test]
    fn minibar_color_thresholds() {
        let high = MiniBar::color_for_value(0.80);
        let mid = MiniBar::color_for_value(0.60);
        let low = MiniBar::color_for_value(0.30);
        let crit = MiniBar::color_for_value(0.10);
        assert_ne!(high, mid);
        assert_ne!(mid, low);
        assert_ne!(low, crit);
    }

    #[test]
    fn minibar_respects_width() {
        for width in [5, 10, 20] {
            let bar = MiniBar::new(0.5, width);
            assert_eq!(bar.render_string().chars().count(), width as usize);
        }
    }

    // --- MeasurableWidget tests ---

    #[test]
    fn progress_bar_measure_has_intrinsic_size() {
        let pb = ProgressBar::new();
        assert!(pb.has_intrinsic_size());
    }

    #[test]
    fn progress_bar_measure_min_size() {
        let pb = ProgressBar::new();
        let c = pb.measure(Size::MAX);

        assert_eq!(c.min.width, 1);
        assert_eq!(c.min.height, 1);
        assert!(c.max.is_none()); // Fills available width
    }

    #[test]
    fn progress_bar_measure_with_block() {
        let pb = ProgressBar::new().block(Block::bordered());
        let c = pb.measure(Size::MAX);

        // Block adds chrome (borders + padding) = 4 on each axis.
        assert_eq!(c.min.width, 5);
        assert_eq!(c.min.height, 5);
    }

    #[test]
    fn minibar_measure_fixed_width() {
        let bar = MiniBar::new(0.5, 10);
        let c = bar.measure(Size::MAX);

        assert_eq!(c.preferred.width, 10);
        assert_eq!(c.preferred.height, 1);
        assert_eq!(c.max, Some(Size::new(10, 1)));
    }

    #[test]
    fn minibar_measure_with_percent() {
        let bar = MiniBar::new(0.5, 10).show_percent(true);
        let c = bar.measure(Size::MAX);

        // Width = 10 + 5 (" XXX%") = 15
        assert_eq!(c.preferred.width, 15);
        assert_eq!(c.preferred.height, 1);
    }

    #[test]
    fn minibar_measure_has_intrinsic_size() {
        let bar = MiniBar::new(0.5, 10);
        assert!(bar.has_intrinsic_size());

        let zero_width = MiniBar::new(0.5, 0);
        assert!(!zero_width.has_intrinsic_size());
    }

    // ── Edge-case tests (bd-3b82x) ──────────────────────────

    #[test]
    fn ratio_nan_clamped_to_zero() {
        let pb = ProgressBar::new().ratio(f64::NAN);
        // We now safely handle NaN in the ratio setter to avoid clamping panic.
        // The render path uses floor() which handles NaN → 0
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let area = Rect::new(0, 0, 10, 1);
        Widget::render(&pb, area, &mut frame);
    }

    #[test]
    fn ratio_infinity_clamped() {
        let pb = ProgressBar::new().ratio(f64::INFINITY);
        assert_eq!(pb.ratio, 1.0);

        let pb_neg = ProgressBar::new().ratio(f64::NEG_INFINITY);
        assert_eq!(pb_neg.ratio, 0.0);
    }

    #[test]
    fn label_wider_than_area() {
        let pb = ProgressBar::new()
            .ratio(0.5)
            .label("This is a very long label text");
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let area = Rect::new(0, 0, 5, 1);
        Widget::render(&pb, area, &mut frame); // Should not panic, truncated
    }

    #[test]
    fn label_on_multi_row_bar_vertically_centered() {
        let pb = ProgressBar::new().ratio(0.5).label("X");
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        Widget::render(&pb, area, &mut frame);
        // label_y = top + height/2 = 0 + 2 = 2
        let c = frame.buffer.get(4, 2).and_then(|c| c.content.as_char());
        assert_eq!(c, Some('X'));
    }

    #[test]
    fn empty_label_renders_no_text() {
        let pb = ProgressBar::new().ratio(0.5).label("");
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let area = Rect::new(0, 0, 10, 1);
        Widget::render(&pb, area, &mut frame); // Should not panic
    }

    #[test]
    fn progress_bar_clone_and_debug() {
        let pb = ProgressBar::new().ratio(0.5).label("test");
        let cloned = pb.clone();
        assert!((cloned.ratio - 0.5).abs() < f64::EPSILON);
        assert_eq!(cloned.label, Some("test"));
        let dbg = format!("{:?}", pb);
        assert!(dbg.contains("ProgressBar"));
    }

    #[test]
    fn progress_bar_default_trait() {
        let pb = ProgressBar::default();
        assert_eq!(pb.ratio, 0.0);
        assert!(pb.label.is_none());
    }

    #[test]
    fn render_width_one() {
        let pb = ProgressBar::new()
            .ratio(1.0)
            .gauge_style(Style::new().bg(PackedRgba::RED));
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let area = Rect::new(0, 0, 1, 1);
        Widget::render(&pb, area, &mut frame);
        assert_eq!(cell_at(&frame, 0, 0).bg, PackedRgba::RED);
    }

    #[test]
    fn render_ratio_just_above_zero() {
        let pb = ProgressBar::new()
            .ratio(0.01)
            .gauge_style(Style::new().bg(PackedRgba::GREEN));
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(100, 1, &mut pool);
        let area = Rect::new(0, 0, 100, 1);
        Widget::render(&pb, area, &mut frame);
        // floor(100 * 0.01) = 1 cell filled
        assert_eq!(cell_at(&frame, 0, 0).bg, PackedRgba::GREEN);
        assert_ne!(cell_at(&frame, 1, 0).bg, PackedRgba::GREEN);
    }

    // --- MiniBar edge cases ---

    #[test]
    fn minibar_nan_value_treated_as_zero() {
        let bar = MiniBar::new(f64::NAN, 10);
        let filled = bar.render_string().chars().filter(|c| *c == '█').count();
        assert_eq!(filled, 0);
    }

    #[test]
    fn minibar_infinity_clamped_to_full() {
        let bar = MiniBar::new(f64::INFINITY, 10);
        let filled = bar.render_string().chars().filter(|c| *c == '█').count();
        assert_eq!(filled, 0); // NaN/Inf → normalized_value returns 0.0
    }

    #[test]
    fn minibar_negative_value() {
        let bar = MiniBar::new(-0.5, 10);
        let filled = bar.render_string().chars().filter(|c| *c == '█').count();
        assert_eq!(filled, 0);
    }

    #[test]
    fn minibar_value_above_one() {
        let bar = MiniBar::new(1.5, 10);
        let filled = bar.render_string().chars().filter(|c| *c == '█').count();
        assert_eq!(filled, 10); // clamped to 1.0
    }

    #[test]
    fn minibar_width_zero() {
        let bar = MiniBar::new(0.5, 0);
        assert_eq!(bar.render_string(), "");
    }

    #[test]
    fn minibar_width_one() {
        let bar = MiniBar::new(1.0, 1);
        let s = bar.render_string();
        assert_eq!(s.chars().count(), 1);
        assert_eq!(s.chars().next(), Some('█'));
    }

    #[test]
    fn minibar_custom_chars() {
        let bar = MiniBar::new(0.5, 4).filled_char('#').empty_char('-');
        let s = bar.render_string();
        assert!(s.contains('#'));
        assert!(s.contains('-'));
        assert_eq!(s.chars().count(), 4);
    }

    #[test]
    fn minibar_value_and_width_setters() {
        let bar = MiniBar::new(0.0, 5).value(1.0).width(3);
        assert_eq!(bar.render_string().chars().count(), 3);
        let filled = bar.render_string().chars().filter(|c| *c == '█').count();
        assert_eq!(filled, 3);
    }

    #[test]
    fn minibar_color_boundary_exactly_at_high() {
        // Default high threshold is 0.75; at exactly 0.75, value is NOT > 0.75
        let at_thresh = MiniBar::color_for_value(0.75);
        let above = MiniBar::color_for_value(0.76);
        let defaults = MiniBarColors::default();
        assert_eq!(above, defaults.high);
        assert_eq!(at_thresh, defaults.mid); // not above high threshold
    }

    #[test]
    fn minibar_color_boundary_exactly_at_mid() {
        let at_thresh = MiniBar::color_for_value(0.50);
        let defaults = MiniBarColors::default();
        assert_eq!(at_thresh, defaults.low); // not above mid threshold
    }

    #[test]
    fn minibar_color_boundary_exactly_at_low() {
        let at_thresh = MiniBar::color_for_value(0.25);
        let defaults = MiniBarColors::default();
        assert_eq!(at_thresh, defaults.critical); // not above low threshold
    }

    #[test]
    fn minibar_color_for_value_nan() {
        let c = MiniBar::color_for_value(f64::NAN);
        let defaults = MiniBarColors::default();
        assert_eq!(c, defaults.critical); // NaN → 0.0 → critical
    }

    #[test]
    fn minibar_colors_new() {
        let r = PackedRgba::rgb(255, 0, 0);
        let g = PackedRgba::rgb(0, 255, 0);
        let b = PackedRgba::rgb(0, 0, 255);
        let w = PackedRgba::rgb(255, 255, 255);
        let colors = MiniBarColors::new(r, g, b, w);
        assert_eq!(colors.high, r);
        assert_eq!(colors.mid, g);
        assert_eq!(colors.low, b);
        assert_eq!(colors.critical, w);
    }

    #[test]
    fn minibar_custom_thresholds_and_colors() {
        let colors = MiniBarColors::new(
            PackedRgba::rgb(1, 1, 1),
            PackedRgba::rgb(2, 2, 2),
            PackedRgba::rgb(3, 3, 3),
            PackedRgba::rgb(4, 4, 4),
        );
        let thresholds = MiniBarThresholds {
            high: 0.9,
            mid: 0.5,
            low: 0.1,
        };
        let bar = MiniBar::new(0.95, 10).colors(colors).thresholds(thresholds);
        let c = bar.color_for_value_with_palette(0.95);
        assert_eq!(c, PackedRgba::rgb(1, 1, 1));
    }

    #[test]
    fn minibar_clone_and_debug() {
        let bar = MiniBar::new(0.5, 10).show_percent(true);
        let cloned = bar.clone();
        assert_eq!(cloned.render_string(), bar.render_string());
        let dbg = format!("{:?}", bar);
        assert!(dbg.contains("MiniBar"));
    }

    #[test]
    fn minibar_render_zero_area() {
        let bar = MiniBar::new(0.5, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let area = Rect::new(0, 0, 0, 0);
        Widget::render(&bar, area, &mut frame); // Should not panic
    }

    #[test]
    fn minibar_render_with_percent_narrow() {
        let bar = MiniBar::new(0.5, 10).show_percent(true);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        // Area smaller than bar_width + percent_width
        let area = Rect::new(0, 0, 5, 1);
        Widget::render(&bar, area, &mut frame); // Should adapt or truncate
    }

    #[test]
    fn minibar_render_percent_only_no_bar_room() {
        let bar = MiniBar::new(0.5, 10).show_percent(true);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        // Area of width 5, percent takes 5 (" XXX%"), bar_width gets 0
        let area = Rect::new(0, 0, 5, 1);
        Widget::render(&bar, area, &mut frame);
        assert_eq!(cell_at(&frame, 0, 0).content.as_char(), Some('5'));
    }

    #[test]
    fn minibar_render_percent_only_starts_with_digits_in_tight_widths() {
        let bar = MiniBar::new(0.5, 10).show_percent(true);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 1, &mut pool);
        Widget::render(&bar, Rect::new(0, 0, 2, 1), &mut frame);

        assert_eq!(cell_at(&frame, 0, 0).content.as_char(), Some('5'));
        assert_eq!(cell_at(&frame, 1, 0).content.as_char(), Some('0'));
    }

    #[test]
    fn minibar_essential_only_percent_starts_with_digits_in_tight_widths() {
        use ftui_render::budget::DegradationLevel;

        let bar = MiniBar::new(0.5, 10).show_percent(true);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::EssentialOnly;
        Widget::render(&bar, Rect::new(0, 0, 2, 1), &mut frame);

        assert_eq!(cell_at(&frame, 0, 0).content.as_char(), Some('5'));
        assert_eq!(cell_at(&frame, 1, 0).content.as_char(), Some('0'));
    }

    #[test]
    fn minibar_essential_only_right_aligns_percent_when_width_allows() {
        use ftui_render::budget::DegradationLevel;

        let bar = MiniBar::new(0.5, 10).show_percent(true);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(7, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::EssentialOnly;
        Widget::render(&bar, Rect::new(0, 0, 7, 1), &mut frame);

        assert_eq!(cell_at(&frame, 0, 0).content.as_char(), Some(' '));
        assert_eq!(cell_at(&frame, 1, 0).content.as_char(), Some(' '));
        assert_eq!(cell_at(&frame, 2, 0).content.as_char(), Some(' '));
        assert_eq!(cell_at(&frame, 3, 0).content.as_char(), Some(' '));
        assert_eq!(cell_at(&frame, 4, 0).content.as_char(), Some('5'));
        assert_eq!(cell_at(&frame, 5, 0).content.as_char(), Some('0'));
        assert_eq!(cell_at(&frame, 6, 0).content.as_char(), Some('%'));
    }

    #[test]
    fn minibar_essential_only_clears_previous_bar_content() {
        use ftui_render::budget::DegradationLevel;

        let bar = MiniBar::new(0.5, 10).show_percent(true);
        let area = Rect::new(0, 0, 7, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(7, 1, &mut pool);

        Widget::render(&bar, area, &mut frame);
        frame.buffer.degradation = DegradationLevel::EssentialOnly;
        Widget::render(&bar, area, &mut frame);

        assert_eq!(raw_row_text(&frame, 0, 7), "    50%");
    }

    #[test]
    fn minibar_thresholds_default_values() {
        let t = MiniBarThresholds::default();
        assert!((t.high - 0.75).abs() < f64::EPSILON);
        assert!((t.mid - 0.50).abs() < f64::EPSILON);
        assert!((t.low - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn minibar_colors_default_not_all_same() {
        let c = MiniBarColors::default();
        assert_ne!(c.high, c.mid);
        assert_ne!(c.mid, c.low);
        assert_ne!(c.low, c.critical);
    }

    #[test]
    fn minibar_colors_copy() {
        let c = MiniBarColors::default();
        let c2 = c; // Copy
        assert_eq!(c.high, c2.high);
    }

    #[test]
    fn minibar_thresholds_copy() {
        let t = MiniBarThresholds::default();
        let t2 = t; // Copy
        assert!((t.high - t2.high).abs() < f64::EPSILON);
    }

    #[test]
    fn minibar_style_setter() {
        let bar = MiniBar::new(0.5, 10).style(Style::new().bold());
        let dbg = format!("{:?}", bar);
        assert!(dbg.contains("MiniBar"));
    }
}

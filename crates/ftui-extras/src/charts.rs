#![forbid(unsafe_code)]

//! Chart widgets for data visualization.
//!
//! Provides [`Sparkline`], [`BarChart`], and [`LineChart`] widgets for
//! rendering data in the terminal. Feature-gated behind `charts`.
//!
//! # Example
//!
//! ```ignore
//! use ftui_extras::charts::Sparkline;
//!
//! let data = [1.0, 4.0, 2.0, 8.0, 5.0, 7.0, 3.0, 6.0];
//! let sparkline = Sparkline::new(&data);
//! sparkline.render(area, &mut buf);
//! ```

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, CellContent, PackedRgba};
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_widgets::Widget;
use unicode_display_width::width as unicode_display_width;
use unicode_segmentation::UnicodeSegmentation;

use crate::canvas::{Mode, Painter};

// ===== Helpers =====

/// Bar characters for sparkline/vertical-bar rendering (9 levels: empty through full).
const BAR_CHARS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

#[inline]
fn ascii_display_width(text: &str) -> usize {
    let mut width = 0;
    for b in text.bytes() {
        match b {
            b'\t' | b'\n' | b'\r' => width += 1,
            0x20..=0x7E => width += 1,
            _ => {}
        }
    }
    width
}

#[inline]
fn is_zero_width_codepoint(c: char) -> bool {
    let u = c as u32;
    matches!(u, 0x0000..=0x001F | 0x007F..=0x009F)
        || matches!(u, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF)
        || matches!(u, 0xFE20..=0xFE2F)
        || matches!(u, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
        || matches!(
            u,
            0x00AD | 0x034F | 0x180E | 0x200B | 0x200C | 0x200D | 0x200E | 0x200F | 0x2060 | 0xFEFF
        )
        || matches!(u, 0x202A..=0x202E | 0x2066..=0x2069 | 0x206A..=0x206F)
}

#[inline]
fn grapheme_width(grapheme: &str) -> usize {
    if grapheme.is_ascii() {
        return ascii_display_width(grapheme);
    }
    if grapheme.chars().all(is_zero_width_codepoint) {
        return 0;
    }
    usize::try_from(unicode_display_width(grapheme))
        .expect("unicode display width should fit in usize")
}

#[inline]
fn display_width(text: &str) -> usize {
    if text.is_ascii() && text.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return text.len();
    }
    if text.is_ascii() {
        return ascii_display_width(text);
    }
    if !text.chars().any(is_zero_width_codepoint) {
        return usize::try_from(unicode_display_width(text))
            .expect("unicode display width should fit in usize");
    }
    text.graphemes(true).map(grapheme_width).sum()
}

/// Linearly interpolate between two colors.
fn lerp_color(a: PackedRgba, b: PackedRgba, t: f64) -> PackedRgba {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) } as f32;
    let inv = 1.0 - t;
    let r = (a.r() as f32 * inv + b.r() as f32 * t).round() as u8;
    let g = (a.g() as f32 * inv + b.g() as f32 * t).round() as u8;
    let bv = (a.b() as f32 * inv + b.b() as f32 * t).round() as u8;
    let av = (a.a() as f32 * inv + b.a() as f32 * t).round() as u8;
    PackedRgba::rgba(r, g, bv, av)
}

/// Heatmap gradient for normalized values (0.0 to 1.0).
///
/// Cold → Hot: Navy → Blue → Teal → Green → Gold → Orange → Red → Hot Pink.
pub fn heatmap_gradient(value: f64) -> PackedRgba {
    const STOPS: [(f64, PackedRgba); 8] = [
        (0.000, PackedRgba::rgb(30, 30, 80)),    // Navy
        (0.143, PackedRgba::rgb(50, 50, 180)),   // Blue
        (0.286, PackedRgba::rgb(50, 150, 150)),  // Teal
        (0.429, PackedRgba::rgb(80, 180, 80)),   // Green
        (0.571, PackedRgba::rgb(220, 180, 50)),  // Gold
        (0.714, PackedRgba::rgb(255, 140, 50)),  // Orange
        (0.857, PackedRgba::rgb(255, 80, 80)),   // Red
        (1.000, PackedRgba::rgb(255, 100, 180)), // Hot Pink
    ];

    let clamped = if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    };
    for window in STOPS.windows(2) {
        let (t0, c0) = window[0];
        let (t1, c1) = window[1];
        if clamped <= t1 {
            let t = if t1 > t0 {
                (clamped - t0) / (t1 - t0)
            } else {
                0.0
            };
            return lerp_color(c0, c1, t);
        }
    }

    STOPS[STOPS.len() - 1].1
}

/// Apply a Style's fg/bg to a Cell.
fn style_cell(cell: &mut Cell, style: Style) {
    if let Some(fg) = style.fg {
        cell.fg = fg;
    }
    if let Some(bg) = style.bg {
        cell.bg = bg;
    }
}

// ===== Sparkline =====

/// Compact single-line data visualization using block characters (`▁▂▃▄▅▆▇█`).
///
/// Each data value maps to one terminal column. Values are auto-scaled to
/// the data range unless explicit bounds are provided. An optional color
/// gradient interpolates between two colors based on normalized value.
#[derive(Debug, Clone)]
pub struct Sparkline<'a> {
    data: &'a [f64],
    style: Style,
    max: Option<f64>,
    min: Option<f64>,
    low_color: Option<PackedRgba>,
    high_color: Option<PackedRgba>,
}

impl<'a> Sparkline<'a> {
    pub fn new(data: &'a [f64]) -> Self {
        Self {
            data,
            style: Style::new(),
            max: None,
            min: None,
            low_color: None,
            high_color: None,
        }
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set explicit maximum value (otherwise auto-computed from data).
    pub fn max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }

    /// Set explicit minimum value (otherwise auto-computed from data).
    pub fn min(mut self, min: f64) -> Self {
        self.min = Some(min);
        self
    }

    /// Set a color gradient from low values to high values.
    pub fn gradient(mut self, low: PackedRgba, high: PackedRgba) -> Self {
        self.low_color = Some(low);
        self.high_color = Some(high);
        self
    }

    fn compute_bounds(&self) -> (f64, f64) {
        let data_min = self.data.iter().copied().fold(f64::INFINITY, f64::min);
        let data_max = self.data.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        (self.min.unwrap_or(data_min), self.max.unwrap_or(data_max))
    }
}

impl Widget for Sparkline<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() || self.data.is_empty() {
            return;
        }

        let (min, max) = self.compute_bounds();
        let range = max - min;

        // Render on the last row of the area.
        let y = area.bottom().saturating_sub(1);
        let width = area.width as usize;

        for (i, &value) in self.data.iter().enumerate().take(width) {
            let x = area.x.saturating_add(i as u16);

            let normalized = if range > 0.0 {
                let v = (value - min) / range;
                if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
            } else {
                1.0 // all values equal: full bar
            };

            // Map to bar character (0 = space, 8 = full block).
            let bar_idx = (normalized * 8.0).round().min(8.0) as usize;
            let ch = BAR_CHARS[bar_idx];
            if ch == ' ' {
                continue;
            }

            let mut cell = Cell::from_char(ch);
            style_cell(&mut cell, self.style);

            if let (Some(low), Some(high)) = (self.low_color, self.high_color) {
                cell.fg = lerp_color(low, high, normalized);
            }

            frame.buffer.set(x, y, cell);
        }
    }
}

// ===== BarChart =====

/// Bar orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BarDirection {
    #[default]
    Vertical,
    Horizontal,
}

/// Bar grouping mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BarMode {
    #[default]
    Grouped,
    Stacked,
}

/// A group of bars sharing a label.
#[derive(Debug, Clone)]
pub struct BarGroup<'a> {
    pub label: &'a str,
    pub values: Vec<f64>,
}

impl<'a> BarGroup<'a> {
    pub fn new(label: &'a str, values: Vec<f64>) -> Self {
        Self { label, values }
    }
}

/// Bar chart widget with grouped/stacked modes and horizontal/vertical orientation.
#[derive(Debug, Clone)]
pub struct BarChart<'a> {
    groups: Vec<BarGroup<'a>>,
    direction: BarDirection,
    mode: BarMode,
    bar_width: u16,
    bar_gap: u16,
    group_gap: u16,
    colors: Vec<PackedRgba>,
    style: Style,
    max: Option<f64>,
}

impl<'a> BarChart<'a> {
    pub fn new(groups: Vec<BarGroup<'a>>) -> Self {
        Self {
            groups,
            direction: BarDirection::default(),
            mode: BarMode::default(),
            bar_width: 1,
            bar_gap: 0,
            group_gap: 1,
            colors: vec![
                PackedRgba::rgb(0, 150, 255),
                PackedRgba::rgb(255, 100, 0),
                PackedRgba::rgb(0, 200, 100),
                PackedRgba::rgb(200, 50, 200),
            ],
            style: Style::new(),
            max: None,
        }
    }

    pub fn direction(mut self, direction: BarDirection) -> Self {
        self.direction = direction;
        self
    }

    pub fn mode(mut self, mode: BarMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn bar_width(mut self, width: u16) -> Self {
        self.bar_width = width.max(1);
        self
    }

    pub fn bar_gap(mut self, gap: u16) -> Self {
        self.bar_gap = gap;
        self
    }

    pub fn group_gap(mut self, gap: u16) -> Self {
        self.group_gap = gap;
        self
    }

    pub fn colors(mut self, colors: Vec<PackedRgba>) -> Self {
        self.colors = colors;
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }

    fn compute_max(&self) -> f64 {
        if let Some(m) = self.max {
            return m;
        }
        match self.mode {
            BarMode::Grouped => self
                .groups
                .iter()
                .flat_map(|g| g.values.iter())
                .copied()
                .fold(0.0_f64, f64::max),
            BarMode::Stacked => self
                .groups
                .iter()
                .map(|g| g.values.iter().sum::<f64>())
                .fold(0.0_f64, f64::max),
        }
    }

    fn get_color(&self, series_idx: usize) -> PackedRgba {
        if self.colors.is_empty() {
            PackedRgba::WHITE
        } else {
            self.colors[series_idx % self.colors.len()]
        }
    }
}

impl Widget for BarChart<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() || self.groups.is_empty() {
            return;
        }

        let max_val = self.compute_max();
        if max_val <= 0.0 {
            return;
        }

        match self.direction {
            BarDirection::Vertical => self.render_vertical(area, &mut frame.buffer, max_val),
            BarDirection::Horizontal => self.render_horizontal(area, &mut frame.buffer, max_val),
        }
    }
}

impl BarChart<'_> {
    fn render_vertical(&self, area: Rect, buf: &mut Buffer, max_val: f64) {
        // Reserve 1 row at bottom for labels.
        let chart_height = area.height.saturating_sub(1) as f64;
        if chart_height <= 0.0 {
            return;
        }

        let label_y = area.bottom().saturating_sub(1);
        let mut x_cursor = area.x;

        for (gi, group) in self.groups.iter().enumerate() {
            if gi > 0 {
                x_cursor += self.group_gap;
            }
            let group_start_x = x_cursor;

            match self.mode {
                BarMode::Grouped => {
                    // Baseline y: bottom row of the chart area (above label row).
                    let base_y = area.bottom().saturating_sub(2);
                    for (si, &val) in group.values.iter().enumerate() {
                        if si > 0 {
                            x_cursor += self.bar_gap;
                        }
                        let h = (val / max_val) * chart_height;
                        let h = if h.is_nan() { 0.0 } else { h };
                        let full = h.floor() as u16;
                        let frac_idx = ((h - h.floor()) * 8.0).round().min(8.0) as usize;
                        let color = self.get_color(si);

                        // Full rows from bottom up.
                        for row in 0..full {
                            let y = base_y.saturating_sub(row);
                            if y < area.y {
                                break;
                            }
                            for dx in 0..self.bar_width {
                                let x = x_cursor.saturating_add(dx);
                                if x < area.right() {
                                    let mut cell = Cell::from_char('█');
                                    cell.fg = color;
                                    buf.set_fast(x, y, cell);
                                }
                            }
                        }

                        // Fractional top row.
                        if frac_idx > 0 {
                            let y = base_y.saturating_sub(full);
                            if y >= area.y {
                                let ch = BAR_CHARS[frac_idx];
                                for dx in 0..self.bar_width {
                                    let x = x_cursor.saturating_add(dx);
                                    if x < area.right() {
                                        let mut cell = Cell::from_char(ch);
                                        cell.fg = color;
                                        buf.set_fast(x, y, cell);
                                    }
                                }
                            }
                        }

                        x_cursor += self.bar_width;
                    }
                }
                BarMode::Stacked => {
                    // Baseline y: bottom row of the chart area (above label row).
                    let base_y = area.bottom().saturating_sub(2);
                    // Use cumulative heights to avoid fractional gaps.
                    let mut cumulative = 0.0_f64;
                    for (si, &val) in group.values.iter().enumerate() {
                        let prev_rows = (cumulative / max_val * chart_height).round() as u16;
                        cumulative += val;
                        let curr_rows = (cumulative / max_val * chart_height).round() as u16;
                        let segment = curr_rows.saturating_sub(prev_rows);
                        let color = self.get_color(si);

                        for row in 0..segment {
                            let y = base_y.saturating_sub(prev_rows).saturating_sub(row);
                            if y < area.y {
                                break;
                            }
                            for dx in 0..self.bar_width {
                                let x = x_cursor.saturating_add(dx);
                                if x < area.right() {
                                    let mut cell = Cell::from_char('█');
                                    cell.fg = color;
                                    buf.set_fast(x, y, cell);
                                }
                            }
                        }
                    }
                    x_cursor += self.bar_width;
                }
            }

            // Group label (truncated to bar group width).
            let group_width = x_cursor.saturating_sub(group_start_x);
            let label_x = group_start_x.saturating_add(group_width.saturating_sub(1) / 2);
            if let Some(ch) = group.label.chars().next()
                && label_x < area.right()
                && label_y < area.bottom()
            {
                let mut cell = Cell::from_char(ch);
                style_cell(&mut cell, self.style);
                buf.set_fast(label_x, label_y, cell);
            }
        }
    }

    fn render_horizontal(&self, area: Rect, buf: &mut Buffer, max_val: f64) {
        // Reserve 2 columns at left for labels.
        let label_width = 2_u16;
        let chart_width = area.width.saturating_sub(label_width) as f64;
        if chart_width <= 0.0 {
            return;
        }

        let mut y_cursor = area.y;

        for (gi, group) in self.groups.iter().enumerate() {
            if gi > 0 {
                y_cursor += self.group_gap;
            }

            match self.mode {
                BarMode::Grouped => {
                    for (si, &val) in group.values.iter().enumerate() {
                        if si > 0 {
                            y_cursor += self.bar_gap;
                        }
                        let bar_len_f = (val / max_val) * chart_width;
                        let bar_len = if bar_len_f.is_nan() {
                            0
                        } else {
                            bar_len_f.round() as u16
                        };
                        let color = self.get_color(si);

                        for dy in 0..self.bar_width {
                            let y = y_cursor.saturating_add(dy);
                            if y >= area.bottom() {
                                break;
                            }
                            for dx in 0..bar_len {
                                let x = area.x.saturating_add(label_width).saturating_add(dx);
                                if x < area.right() {
                                    let mut cell = Cell::from_char('█');
                                    cell.fg = color;
                                    buf.set_fast(x, y, cell);
                                }
                            }
                        }

                        y_cursor += self.bar_width;
                    }
                }
                BarMode::Stacked => {
                    let mut left_col = 0_u16;
                    for (si, &val) in group.values.iter().enumerate() {
                        let bar_len_f = (val / max_val) * chart_width;
                        let bar_len = if bar_len_f.is_nan() {
                            0
                        } else {
                            bar_len_f.round() as u16
                        };
                        let color = self.get_color(si);

                        for dy in 0..self.bar_width {
                            let y = y_cursor.saturating_add(dy);
                            if y >= area.bottom() {
                                break;
                            }
                            for dx in 0..bar_len {
                                let x = area
                                    .x
                                    .saturating_add(label_width)
                                    .saturating_add(left_col)
                                    .saturating_add(dx);
                                if x < area.right() {
                                    let mut cell = Cell::from_char('█');
                                    cell.fg = color;
                                    buf.set_fast(x, y, cell);
                                }
                            }
                        }
                        left_col += bar_len;
                    }
                    y_cursor += self.bar_width;
                }
            }

            // Group label at left edge.
            if let Some(ch) = group.label.chars().next() {
                let ly = match self.mode {
                    BarMode::Grouped => y_cursor.saturating_sub(
                        (group.values.len() as u16) * self.bar_width
                            + group.values.len().saturating_sub(1) as u16 * self.bar_gap,
                    ),
                    BarMode::Stacked => y_cursor.saturating_sub(self.bar_width),
                };
                if ly < area.bottom() {
                    let mut cell = Cell::from_char(ch);
                    style_cell(&mut cell, self.style);
                    buf.set_fast(area.x, ly, cell);
                }
            }
        }
    }
}

// ===== LineChart =====

/// A data series for the line chart.
#[derive(Debug, Clone)]
pub struct Series<'a> {
    pub name: &'a str,
    pub data: &'a [(f64, f64)],
    pub color: PackedRgba,
    pub show_markers: bool,
}

impl<'a> Series<'a> {
    pub fn new(name: &'a str, data: &'a [(f64, f64)], color: PackedRgba) -> Self {
        Self {
            name,
            data,
            color,
            show_markers: false,
        }
    }

    pub fn markers(mut self, show: bool) -> Self {
        self.show_markers = show;
        self
    }
}

/// Line chart with multi-series support, axis rendering, and legend.
///
/// Uses [`Canvas`](crate::canvas::Canvas) internally with Braille mode for
/// sub-cell line resolution.
#[derive(Debug, Clone)]
pub struct LineChart<'a> {
    series: Vec<Series<'a>>,
    x_bounds: Option<(f64, f64)>,
    y_bounds: Option<(f64, f64)>,
    style: Style,
    x_labels: Vec<&'a str>,
    y_labels: Vec<&'a str>,
    show_legend: bool,
}

impl<'a> LineChart<'a> {
    pub fn new(series: Vec<Series<'a>>) -> Self {
        Self {
            series,
            x_bounds: None,
            y_bounds: None,
            style: Style::new(),
            x_labels: Vec::new(),
            y_labels: Vec::new(),
            show_legend: false,
        }
    }

    pub fn x_bounds(mut self, min: f64, max: f64) -> Self {
        self.x_bounds = Some((min, max));
        self
    }

    pub fn y_bounds(mut self, min: f64, max: f64) -> Self {
        self.y_bounds = Some((min, max));
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn x_labels(mut self, labels: Vec<&'a str>) -> Self {
        self.x_labels = labels;
        self
    }

    pub fn y_labels(mut self, labels: Vec<&'a str>) -> Self {
        self.y_labels = labels;
        self
    }

    pub fn legend(mut self, show: bool) -> Self {
        self.show_legend = show;
        self
    }

    fn auto_bounds(&self) -> ((f64, f64), (f64, f64)) {
        let mut x_min = f64::INFINITY;
        let mut x_max = f64::NEG_INFINITY;
        let mut y_min = f64::INFINITY;
        let mut y_max = f64::NEG_INFINITY;

        for series in &self.series {
            for &(x, y) in series.data {
                x_min = x_min.min(x);
                x_max = x_max.max(x);
                y_min = y_min.min(y);
                y_max = y_max.max(y);
            }
        }

        (
            self.x_bounds.unwrap_or((x_min, x_max)),
            self.y_bounds.unwrap_or((y_min, y_max)),
        )
    }
}

impl Widget for LineChart<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() || self.series.is_empty() {
            return;
        }

        // Reserve space for axes.
        let y_axis_width: u16 = if self.y_labels.is_empty() {
            1
        } else {
            self.y_labels
                .iter()
                .map(|l| display_width(l))
                .max()
                .unwrap_or(0) as u16
                + 1
        };
        let x_axis_height: u16 = if self.x_labels.is_empty() { 1 } else { 2 };

        let chart_area = Rect::new(
            area.x.saturating_add(y_axis_width),
            area.y,
            area.width.saturating_sub(y_axis_width),
            area.height.saturating_sub(x_axis_height),
        );

        if chart_area.width < 2 || chart_area.height < 2 {
            return;
        }

        let ((mut x_min, mut x_max), (mut y_min, mut y_max)) = self.auto_bounds();

        if (x_max - x_min).abs() < f64::EPSILON {
            x_min -= 1.0;
            x_max += 1.0;
        }
        if (y_max - y_min).abs() < f64::EPSILON {
            y_min -= 1.0;
            y_max += 1.0;
        }

        let x_range = x_max - x_min;
        let y_range = y_max - y_min;

        // Draw data using Canvas/Painter with Braille mode.
        let mut painter = Painter::for_area(chart_area, Mode::Braille);
        let px_w = (chart_area.width * Mode::Braille.cols_per_cell()) as f64;
        let px_h = (chart_area.height * Mode::Braille.rows_per_cell()) as f64;

        let to_px = |x: f64, y: f64| -> (i32, i32) {
            let px = (x - x_min) / x_range * (px_w - 1.0);
            let py = (y_max - y) / y_range * (px_h - 1.0);

            let px = if px.is_nan() { 0.0 } else { px };
            let py = if py.is_nan() { 0.0 } else { py };

            (px.round() as i32, py.round() as i32)
        };

        for series in &self.series {
            if series.data.is_empty() {
                continue;
            }

            // Draw lines between consecutive points.
            for window in series.data.windows(2) {
                let (x0, y0) = to_px(window[0].0, window[0].1);
                let (x1, y1) = to_px(window[1].0, window[1].1);
                painter.line_colored(x0, y0, x1, y1, Some(series.color));
            }

            // Single point: just a dot.
            if series.data.len() == 1 {
                let (px, py) = to_px(series.data[0].0, series.data[0].1);
                painter.point_colored(px, py, series.color);
            }

            // Optional markers.
            if series.show_markers {
                for &(x, y) in series.data {
                    let (px, py) = to_px(x, y);
                    for d in -1..=1 {
                        painter.point_colored(px + d, py, series.color);
                        painter.point_colored(px, py + d, series.color);
                    }
                }
            }
        }

        let canvas = crate::canvas::Canvas::from_painter(&painter).style(self.style);
        canvas.render(chart_area, frame);

        // Y axis line.
        let axis_x = chart_area.x.saturating_sub(1);
        if axis_x >= area.x {
            for y in chart_area.y..chart_area.bottom() {
                let mut cell = Cell::from_char('│');
                style_cell(&mut cell, self.style);
                frame.buffer.set(axis_x, y, cell);
            }
        }

        // X axis line.
        let axis_y = chart_area.bottom();
        if axis_y < area.bottom() {
            for x in chart_area.x..chart_area.right() {
                let mut cell = Cell::from_char('─');
                style_cell(&mut cell, self.style);
                frame.buffer.set(x, axis_y, cell);
            }
            // Corner.
            if axis_x >= area.x {
                let mut cell = Cell::from_char('└');
                style_cell(&mut cell, self.style);
                frame.buffer.set(axis_x, axis_y, cell);
            }
        }

        // Y labels (top-to-bottom order).
        if !self.y_labels.is_empty() {
            let n = self.y_labels.len();
            for (i, label) in self.y_labels.iter().enumerate() {
                let y = if n == 1 {
                    chart_area.y
                } else {
                    chart_area.y
                        + (i as u32 * chart_area.height.saturating_sub(1) as u32
                            / (n as u32 - 1).max(1)) as u16
                };
                let max_len = y_axis_width.saturating_sub(1) as usize;
                let label_width = display_width(label).min(max_len);
                let start_x = area.x.saturating_add(
                    (y_axis_width.saturating_sub(1)).saturating_sub(label_width as u16),
                );
                let mut col = 0u16;
                for grapheme in label.graphemes(true) {
                    let g_width = grapheme_width(grapheme);
                    if g_width == 0 {
                        continue;
                    }
                    if col as usize + g_width > max_len {
                        break;
                    }
                    let content = if g_width > 1 || grapheme.chars().count() > 1 {
                        let id = frame
                            .intern_with_width(grapheme, u8::try_from(g_width).unwrap_or(u8::MAX));
                        CellContent::from_grapheme(id)
                    } else if let Some(c) = grapheme.chars().next() {
                        CellContent::from_char(c)
                    } else {
                        continue;
                    };
                    let mut cell = Cell::new(content);
                    style_cell(&mut cell, self.style);
                    frame.buffer.set(start_x.saturating_add(col), y, cell);
                    col = col.saturating_add(u16::try_from(g_width).unwrap_or(u16::MAX));
                }
            }
        }

        // X labels.
        if !self.x_labels.is_empty() && axis_y.saturating_add(1) < area.bottom() {
            let text_y = axis_y.saturating_add(1);
            let n = self.x_labels.len();
            for (i, label) in self.x_labels.iter().enumerate() {
                let x = if n == 1 {
                    chart_area.x
                } else {
                    chart_area.x.saturating_add(
                        (i as u32 * chart_area.width.saturating_sub(1) as u32
                            / (n as u32 - 1).max(1)) as u16,
                    )
                };
                let mut col = 0u16;
                for grapheme in label.graphemes(true) {
                    let lx = x.saturating_add(col);
                    let g_width = grapheme_width(grapheme);
                    if g_width == 0 {
                        continue;
                    }
                    if lx as u32 + g_width as u32 > area.right() as u32 {
                        break;
                    }
                    let content = if g_width > 1 || grapheme.chars().count() > 1 {
                        let id = frame
                            .intern_with_width(grapheme, u8::try_from(g_width).unwrap_or(u8::MAX));
                        CellContent::from_grapheme(id)
                    } else if let Some(c) = grapheme.chars().next() {
                        CellContent::from_char(c)
                    } else {
                        continue;
                    };
                    let mut cell = Cell::new(content);
                    style_cell(&mut cell, self.style);
                    frame.buffer.set(lx, text_y, cell);
                    col = col.saturating_add(u16::try_from(g_width).unwrap_or(u16::MAX));
                }
            }
        }

        // Legend.
        if self.show_legend && !self.series.is_empty() {
            let max_name = self
                .series
                .iter()
                .map(|s| display_width(s.name))
                .max()
                .unwrap_or(0);
            let legend_width = (max_name as u16).saturating_add(3); // "■ name"
            let legend_x = chart_area.right().saturating_sub(legend_width);

            for (i, series) in self.series.iter().enumerate() {
                let y = chart_area.y.saturating_add(i as u16);
                if y >= chart_area.bottom() {
                    break;
                }
                let mut marker = Cell::from_char('■');
                marker.fg = series.color;
                frame.buffer.set(legend_x, y, marker);

                let mut col = 0u16;
                for grapheme in series.name.graphemes(true) {
                    let x = legend_x.saturating_add(2).saturating_add(col);
                    let g_width = grapheme_width(grapheme);
                    if g_width == 0 {
                        continue;
                    }
                    if x as u32 + g_width as u32 > area.right() as u32 {
                        break;
                    }
                    let content = if g_width > 1 || grapheme.chars().count() > 1 {
                        let id = frame
                            .intern_with_width(grapheme, u8::try_from(g_width).unwrap_or(u8::MAX));
                        CellContent::from_grapheme(id)
                    } else if let Some(c) = grapheme.chars().next() {
                        CellContent::from_char(c)
                    } else {
                        continue;
                    };
                    let mut cell = Cell::new(content);
                    style_cell(&mut cell, self.style);
                    frame.buffer.set(x, y, cell);
                    col = col.saturating_add(u16::try_from(g_width).unwrap_or(u16::MAX));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    // ===== Helper =====

    fn char_at(buf: &Buffer, x: u16, y: u16) -> Option<char> {
        buf.get(x, y).and_then(|c| c.content.as_char())
    }

    // ===== lerp_color =====

    #[test]
    fn lerp_color_at_zero() {
        let a = PackedRgba::rgb(0, 0, 0);
        let b = PackedRgba::rgb(255, 255, 255);
        assert_eq!(lerp_color(a, b, 0.0), a);
    }

    #[test]
    fn lerp_color_at_one() {
        let a = PackedRgba::rgb(0, 0, 0);
        let b = PackedRgba::rgb(255, 255, 255);
        assert_eq!(lerp_color(a, b, 1.0), b);
    }

    #[test]
    fn lerp_color_midpoint() {
        let a = PackedRgba::rgb(0, 0, 0);
        let b = PackedRgba::rgb(254, 254, 254);
        let mid = lerp_color(a, b, 0.5);
        assert_eq!(mid.r(), 127);
        assert_eq!(mid.g(), 127);
        assert_eq!(mid.b(), 127);
    }

    #[test]
    fn lerp_color_clamps() {
        let a = PackedRgba::rgb(100, 100, 100);
        let b = PackedRgba::rgb(200, 200, 200);
        assert_eq!(lerp_color(a, b, -1.0), a);
        assert_eq!(lerp_color(a, b, 2.0), b);
    }

    // ===== Heatmap Gradient =====

    #[test]
    fn heatmap_zero_is_cold() {
        let color = heatmap_gradient(0.0);
        assert!(color.b() > color.r());
    }

    #[test]
    fn heatmap_one_is_hot() {
        let color = heatmap_gradient(1.0);
        assert!(color.r() > color.b());
    }

    #[test]
    fn heatmap_mid_is_intermediate() {
        let color = heatmap_gradient(0.5);
        assert!(color.g() > 100);
    }

    #[test]
    fn heatmap_clamps_out_of_range() {
        assert_eq!(heatmap_gradient(-0.5), heatmap_gradient(0.0));
        assert_eq!(heatmap_gradient(1.5), heatmap_gradient(1.0));
    }

    #[test]
    fn heatmap_gradient_covers_range() {
        // Gradient should span from cool colors at 0.0 to warm colors at 1.0
        let cold = heatmap_gradient(0.0);
        let warm = heatmap_gradient(1.0);
        // Cold should be blue-ish (more blue than red)
        assert!(cold.b() >= cold.r(), "Cold end should be blue-ish");
        // Warm should be red/pink-ish (more red than blue at cool end)
        assert!(warm.r() > cold.r(), "Warm end should have more red");

        // Mid-range colors should transition smoothly (no NaN or panics)
        for i in 0..=100 {
            let value = i as f64 / 100.0;
            let _ = heatmap_gradient(value);
        }
    }

    // ===== Sparkline =====

    #[test]
    fn sparkline_empty_data_noop() {
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        Sparkline::new(&[]).render(area, &mut frame);
        // All cells should be empty.
        for x in 0..10 {
            assert!(frame.buffer.get(x, 0).unwrap().is_empty());
        }
    }

    #[test]
    fn sparkline_empty_area_noop() {
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Sparkline::new(&[1.0, 2.0]).render(area, &mut frame);
    }

    #[test]
    fn sparkline_all_same_values() {
        let data = [5.0, 5.0, 5.0];
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        Sparkline::new(&data).render(area, &mut frame);
        // All same -> normalized = 1.0 -> full block.
        for x in 0..3 {
            assert_eq!(char_at(&frame.buffer, x, 0), Some('█'));
        }
    }

    #[test]
    fn sparkline_auto_scaling() {
        // min=0, max=8 => each step = 1 bar level
        let data = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let area = Rect::new(0, 0, 9, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(9, 1, &mut pool);
        Sparkline::new(&data).render(area, &mut frame);

        // value 0 => bar_idx 0 => space (not rendered)
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
        // value 8 => bar_idx 8 => '█'
        assert_eq!(char_at(&frame.buffer, 8, 0), Some('█'));
        // value 4 => normalized=0.5 => bar_idx=4 => '▄'
        assert_eq!(char_at(&frame.buffer, 4, 0), Some('▄'));
    }

    #[test]
    fn sparkline_explicit_bounds() {
        let data = [5.0];
        let area = Rect::new(0, 0, 1, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Sparkline::new(&data)
            .min(0.0)
            .max(10.0)
            .render(area, &mut frame);
        // 5/10 = 0.5 => bar_idx = 4 => '▄'
        assert_eq!(char_at(&frame.buffer, 0, 0), Some('▄'));
    }

    #[test]
    fn sparkline_truncates_to_area_width() {
        let data = [8.0; 20]; // 20 values
        let area = Rect::new(0, 0, 5, 1); // only 5 columns
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        Sparkline::new(&data).render(area, &mut frame);
        // Should only render first 5 values.
        for x in 0..5 {
            assert_eq!(char_at(&frame.buffer, x, 0), Some('█'));
        }
    }

    #[test]
    fn sparkline_renders_on_last_row() {
        let data = [8.0, 8.0];
        let area = Rect::new(0, 0, 2, 3); // height=3
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 3, &mut pool);
        Sparkline::new(&data).render(area, &mut frame);
        // Top rows empty.
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
        assert!(frame.buffer.get(0, 1).unwrap().is_empty());
        // Last row has bars.
        assert_eq!(char_at(&frame.buffer, 0, 2), Some('█'));
    }

    #[test]
    fn sparkline_gradient_colors() {
        let low = PackedRgba::rgb(0, 0, 0);
        let high = PackedRgba::rgb(255, 255, 255);
        let data = [0.0, 10.0];
        let area = Rect::new(0, 0, 2, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 1, &mut pool);
        Sparkline::new(&data)
            .gradient(low, high)
            .render(area, &mut frame);

        // Second bar (value 10, max) should have high color.
        let cell = frame.buffer.get(1, 0).unwrap();
        assert_eq!(cell.fg, high);
    }

    // ===== BarChart =====

    #[test]
    fn barchart_empty_groups_noop() {
        let area = Rect::new(0, 0, 10, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        BarChart::new(vec![]).render(area, &mut frame);
        for x in 0..10 {
            for y in 0..5 {
                assert!(frame.buffer.get(x, y).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn barchart_vertical_single_bar() {
        let groups = vec![BarGroup::new("A", vec![10.0])];
        let area = Rect::new(0, 0, 3, 6); // 5 rows for chart + 1 for label
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 6, &mut pool);
        BarChart::new(groups).bar_width(1).render(area, &mut frame);

        // Full-height bar at x=0. Bar should fill rows 0..5, label at row 5.
        assert_eq!(char_at(&frame.buffer, 0, 0), Some('█'));
        assert_eq!(char_at(&frame.buffer, 0, 4), Some('█'));
        // Label row.
        assert_eq!(char_at(&frame.buffer, 0, 5), Some('A'));
    }

    #[test]
    fn barchart_vertical_grouped() {
        let groups = vec![BarGroup::new("G", vec![5.0, 10.0])];
        let area = Rect::new(0, 0, 4, 6);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 6, &mut pool);
        BarChart::new(groups)
            .bar_width(1)
            .bar_gap(0)
            .render(area, &mut frame);

        // Second bar (value=10, max=10) should be full height.
        assert_eq!(char_at(&frame.buffer, 1, 0), Some('█'));
        // First bar (value=5, half height) should have partial fill.
        // 5/10 * 5 rows = 2.5 rows. So 2 full rows + fractional.
        assert_eq!(char_at(&frame.buffer, 0, 4), Some('█'));
    }

    #[test]
    fn barchart_vertical_stacked() {
        let groups = vec![BarGroup::new("S", vec![5.0, 5.0])];
        let area = Rect::new(0, 0, 3, 6);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 6, &mut pool);
        BarChart::new(groups)
            .bar_width(1)
            .mode(BarMode::Stacked)
            .render(area, &mut frame);

        // Stacked: total=10, max=10. Full height.
        // Both segments should fill the chart area.
        assert_eq!(char_at(&frame.buffer, 0, 0), Some('█'));
        assert_eq!(char_at(&frame.buffer, 0, 4), Some('█'));
    }

    #[test]
    fn barchart_horizontal_single_bar() {
        let groups = vec![BarGroup::new("A", vec![10.0])];
        let area = Rect::new(0, 0, 12, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(12, 3, &mut pool);
        BarChart::new(groups)
            .direction(BarDirection::Horizontal)
            .bar_width(1)
            .render(area, &mut frame);

        // Label at x=0.
        assert_eq!(char_at(&frame.buffer, 0, 0), Some('A'));
        // Bar fills from x=2 rightward (label_width=2).
        assert_eq!(char_at(&frame.buffer, 2, 0), Some('█'));
        assert_eq!(char_at(&frame.buffer, 11, 0), Some('█'));
    }

    #[test]
    fn barchart_horizontal_stacked() {
        let groups = vec![BarGroup::new("X", vec![5.0, 5.0])];
        let area = Rect::new(0, 0, 12, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(12, 3, &mut pool);
        BarChart::new(groups)
            .direction(BarDirection::Horizontal)
            .mode(BarMode::Stacked)
            .bar_width(1)
            .render(area, &mut frame);

        // Both segments should fill the bar (total=10, max=10).
        assert_eq!(char_at(&frame.buffer, 2, 0), Some('█'));
    }

    #[test]
    fn barchart_zero_values_noop() {
        let groups = vec![BarGroup::new("Z", vec![0.0])];
        let area = Rect::new(0, 0, 5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        BarChart::new(groups).render(area, &mut frame);
        // max_val = 0 -> early return; no bars rendered.
        // Only the label might be rendered, but since we return early, nothing.
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn barchart_custom_colors() {
        let red = PackedRgba::rgb(255, 0, 0);
        let groups = vec![BarGroup::new("C", vec![10.0])];
        let area = Rect::new(0, 0, 3, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 3, &mut pool);
        BarChart::new(groups)
            .bar_width(1)
            .colors(vec![red])
            .render(area, &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.fg, red);
    }

    // ===== LineChart =====

    #[test]
    fn linechart_empty_series_noop() {
        let area = Rect::new(0, 0, 20, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        LineChart::new(vec![]).render(area, &mut frame);
    }

    #[test]
    fn linechart_small_area_noop() {
        let data: Vec<(f64, f64)> = vec![(0.0, 0.0), (1.0, 1.0)];
        let series = vec![Series::new("s", &data, PackedRgba::WHITE)];
        let area = Rect::new(0, 0, 2, 2); // too small after axis reservation
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 2, &mut pool);
        LineChart::new(series).render(area, &mut frame);
    }

    #[test]
    fn linechart_renders_axis() {
        let data: Vec<(f64, f64)> = vec![(0.0, 0.0), (10.0, 10.0)];
        let series = vec![Series::new("s", &data, PackedRgba::WHITE)];
        let area = Rect::new(0, 0, 20, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        LineChart::new(series).render(area, &mut frame);

        // Y axis line at x=0, rows 0..9.
        assert_eq!(char_at(&frame.buffer, 0, 0), Some('│'));
        // X axis line at y=9, x=1..20.
        assert_eq!(char_at(&frame.buffer, 1, 9), Some('─'));
        // Corner.
        assert_eq!(char_at(&frame.buffer, 0, 9), Some('└'));
    }

    #[test]
    fn linechart_with_labels() {
        let data: Vec<(f64, f64)> = vec![(0.0, 0.0), (10.0, 10.0)];
        let series = vec![Series::new("s", &data, PackedRgba::WHITE)];
        let area = Rect::new(0, 0, 30, 12);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 12, &mut pool);
        LineChart::new(series)
            .y_labels(vec!["10", "0"])
            .x_labels(vec!["0", "10"])
            .render(area, &mut frame);

        // Y labels: "10" at top, "0" at bottom of chart area.
        // y_axis_width = 3 (max_label_len=2 + 1).
        assert_eq!(char_at(&frame.buffer, 0, 0), Some('1'));
        assert_eq!(char_at(&frame.buffer, 1, 0), Some('0'));
        // X labels below axis.
        // x_axis at y=10 (area.height - x_axis_height = 12 - 2 = 10).
        // x_label at y=11.
        assert_eq!(char_at(&frame.buffer, 3, 11), Some('0'));
    }

    #[test]
    fn linechart_multi_series() {
        let data1: Vec<(f64, f64)> = vec![(0.0, 0.0), (10.0, 5.0)];
        let data2: Vec<(f64, f64)> = vec![(0.0, 5.0), (10.0, 10.0)];
        let red = PackedRgba::rgb(255, 0, 0);
        let blue = PackedRgba::rgb(0, 0, 255);
        let series = vec![
            Series::new("red", &data1, red),
            Series::new("blue", &data2, blue),
        ];
        let area = Rect::new(0, 0, 20, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        LineChart::new(series).render(area, &mut frame);

        // Both series should draw some braille characters in the chart area.
        let mut found_braille = false;
        for y in 0..9 {
            for x in 1..20 {
                if let Some(ch) = char_at(&frame.buffer, x, y)
                    && ('\u{2800}'..='\u{28FF}').contains(&ch)
                {
                    found_braille = true;
                }
            }
        }
        assert!(
            found_braille,
            "Should have rendered braille line characters"
        );
    }

    #[test]
    fn linechart_legend() {
        let data: Vec<(f64, f64)> = vec![(0.0, 0.0), (10.0, 10.0)];
        let red = PackedRgba::rgb(255, 0, 0);
        let series = vec![Series::new("test", &data, red)];
        let area = Rect::new(0, 0, 30, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 10, &mut pool);
        LineChart::new(series).legend(true).render(area, &mut frame);

        // Legend marker '■' should appear somewhere in top-right area.
        let mut found_legend = false;
        for x in 20..30 {
            if char_at(&frame.buffer, x, 0) == Some('■') {
                found_legend = true;
                break;
            }
        }
        assert!(found_legend, "Should have rendered legend marker");
    }

    #[test]
    fn linechart_single_point() {
        let data: Vec<(f64, f64)> = vec![(5.0, 5.0)];
        let series = vec![Series::new("pt", &data, PackedRgba::WHITE)];
        let area = Rect::new(0, 0, 20, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        // Single point with same min/max -> x_range=0, y_range=0 -> early return.
        LineChart::new(series).render(area, &mut frame);
        // Should not panic; renders axis lines but no data.
    }

    #[test]
    fn linechart_explicit_bounds() {
        let data: Vec<(f64, f64)> = vec![(2.0, 3.0), (8.0, 7.0)];
        let series = vec![Series::new("s", &data, PackedRgba::WHITE)];
        let area = Rect::new(0, 0, 20, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        LineChart::new(series)
            .x_bounds(0.0, 10.0)
            .y_bounds(0.0, 10.0)
            .render(area, &mut frame);

        // Should render without panic.
        assert_eq!(char_at(&frame.buffer, 0, 0), Some('│'));
    }

    #[test]
    fn linechart_markers() {
        let data: Vec<(f64, f64)> = vec![(0.0, 0.0), (5.0, 10.0), (10.0, 0.0)];
        let series = vec![Series::new("m", &data, PackedRgba::WHITE).markers(true)];
        let area = Rect::new(0, 0, 20, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        LineChart::new(series).render(area, &mut frame);

        // Should render braille chars with marker emphasis.
        let mut found_braille = false;
        for y in 0..9 {
            for x in 1..20 {
                if let Some(ch) = char_at(&frame.buffer, x, y)
                    && ('\u{2800}'..='\u{28FF}').contains(&ch)
                {
                    found_braille = true;
                }
            }
        }
        assert!(found_braille);
    }
}

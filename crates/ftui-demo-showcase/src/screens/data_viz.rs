#![forbid(unsafe_code)]

//! Data Visualization screen — charts, sparklines, and canvas drawing.
//!
//! Demonstrates:
//! - `Sparkline` with live animated data
//! - `BarChart` with grouped/stacked modes
//! - `LineChart` with multi-series and Braille rendering
//! - `Canvas` with programmatic drawing (Lissajous curve)

use std::cell::Cell as StdCell;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_extras::canvas::{Canvas, Mode, Painter};
use ftui_extras::charts::{
    BarChart, BarDirection, BarGroup, BarMode, LineChart, Series, Sparkline, heatmap_gradient,
};
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::display_width;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::{MiniBar, MiniBarColors};

use super::{HelpEntry, Screen};
use crate::data::ChartData;
use crate::theme;

/// Which chart panel is highlighted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChartPanel {
    Sparkline,
    BarChart,
    Spectrum,
    LineChart,
    Canvas,
    Heatmap,
}

impl ChartPanel {
    fn next(self) -> Self {
        match self {
            Self::Sparkline => Self::BarChart,
            Self::BarChart => Self::Spectrum,
            Self::Spectrum => Self::LineChart,
            Self::LineChart => Self::Canvas,
            Self::Canvas => Self::Heatmap,
            Self::Heatmap => Self::Sparkline,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Sparkline => Self::Heatmap,
            Self::BarChart => Self::Sparkline,
            Self::Spectrum => Self::BarChart,
            Self::LineChart => Self::Spectrum,
            Self::Canvas => Self::LineChart,
            Self::Heatmap => Self::Canvas,
        }
    }
}

pub struct DataViz {
    focus: ChartPanel,
    chart_data: ChartData,
    tick_count: u64,
    bar_horizontal: bool,
    paused: bool,
    layout_sparkline: StdCell<Rect>,
    layout_barchart: StdCell<Rect>,
    layout_spectrum: StdCell<Rect>,
    layout_linechart: StdCell<Rect>,
    layout_canvas: StdCell<Rect>,
    layout_heatmap: StdCell<Rect>,
}

impl Default for DataViz {
    fn default() -> Self {
        Self::new()
    }
}

impl DataViz {
    pub fn new() -> Self {
        let mut chart_data = ChartData::default();
        for t in 0..30 {
            chart_data.tick(t);
        }
        Self {
            focus: ChartPanel::Sparkline,
            chart_data,
            tick_count: 30,
            bar_horizontal: false,
            paused: false,
            layout_sparkline: StdCell::new(Rect::default()),
            layout_barchart: StdCell::new(Rect::default()),
            layout_spectrum: StdCell::new(Rect::default()),
            layout_linechart: StdCell::new(Rect::default()),
            layout_canvas: StdCell::new(Rect::default()),
            layout_heatmap: StdCell::new(Rect::default()),
        }
    }

    fn render_sparkline_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == ChartPanel::Sparkline,
            theme::screen_accent::DATA_VIZ,
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Sparklines")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
            ])
            .split(inner);

        let sine_data: Vec<f64> = self.chart_data.sine_series.iter().copied().collect();
        let cos_data: Vec<f64> = self.chart_data.cosine_series.iter().copied().collect();
        let rand_data: Vec<f64> = self.chart_data.random_series.iter().copied().collect();
        let colors = chart_palette();
        let gradients = sparkline_gradients();
        let sine_last = self.chart_data.sine_series.back().copied().unwrap_or(0.0);
        let cos_last = self.chart_data.cosine_series.back().copied().unwrap_or(0.0);
        let rand_last = self.chart_data.random_series.back().copied().unwrap_or(0.0);

        // Labels and sparklines in alternating rows
        if !rows[0].is_empty() {
            let label = format!("Sine wave: {:+.2}", sine_last);
            Paragraph::new(label)
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(rows[0], frame);
        }
        if !rows[1].is_empty() {
            Sparkline::new(&sine_data)
                .style(Style::new().fg(colors[0]))
                .gradient(gradients[0].0, gradients[0].1)
                .render(rows[1], frame);
        }
        if !rows[2].is_empty() {
            let label = format!("Cosine wave: {:+.2}", cos_last);
            Paragraph::new(label)
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(rows[2], frame);
        }
        if !rows[3].is_empty() {
            Sparkline::new(&cos_data)
                .style(Style::new().fg(colors[1]))
                .gradient(gradients[1].0, gradients[1].1)
                .render(rows[3], frame);
        }
        if !rows[4].is_empty() {
            let label = format!("Random noise: {:+.2}", rand_last);
            Paragraph::new(label)
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(rows[4], frame);
        }
        if !rows[5].is_empty() {
            Sparkline::new(&rand_data)
                .style(Style::new().fg(colors[2]))
                .gradient(gradients[2].0, gradients[2].1)
                .render(rows[5], frame);
        }
        if !rows[6].is_empty() {
            let mini_cols = Flex::horizontal()
                .gap(theme::spacing::XS)
                .constraints([
                    Constraint::Percentage(33.0),
                    Constraint::Percentage(33.0),
                    Constraint::Percentage(34.0),
                ])
                .split(rows[6]);

            let sine_value =
                normalize_series_value(self.chart_data.sine_series.back().copied().unwrap_or(0.0));
            let cos_value = normalize_series_value(
                self.chart_data.cosine_series.back().copied().unwrap_or(0.0),
            );
            let rand_value = normalize_series_value(
                self.chart_data.random_series.back().copied().unwrap_or(0.0),
            );

            render_mini_bar(frame, mini_cols[0], "SIN", sine_value, colors[0]);
            render_mini_bar(frame, mini_cols[1], "COS", cos_value, colors[1]);
            render_mini_bar(frame, mini_cols[2], "RND", rand_value, colors[2]);
        }
    }

    fn render_barchart_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == ChartPanel::BarChart,
            theme::screen_accent::DATA_VIZ,
        );

        let dir_label = if self.bar_horizontal {
            "Horizontal"
        } else {
            "Vertical"
        };
        let title = format!("Bar Chart ({dir_label})");
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(&title)
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let groups = vec![
            BarGroup::new("Q1", vec![42.0, 38.0, 55.0]),
            BarGroup::new("Q2", vec![58.0, 45.0, 62.0]),
            BarGroup::new("Q3", vec![35.0, 52.0, 48.0]),
            BarGroup::new("Q4", vec![70.0, 60.0, 75.0]),
        ];

        let direction = if self.bar_horizontal {
            BarDirection::Horizontal
        } else {
            BarDirection::Vertical
        };

        let chart = BarChart::new(groups)
            .direction(direction)
            .bar_width(2)
            .bar_gap(theme::spacing::XS)
            .group_gap(theme::spacing::SM)
            .colors(chart_palette().to_vec())
            .style(Style::new().fg(theme::fg::PRIMARY));

        chart.render(inner, frame);
    }

    fn render_spectrum_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == ChartPanel::Spectrum,
            theme::screen_accent::DATA_VIZ,
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Spectrum Bars")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let labels = ["32Hz", "64Hz", "125Hz", "250Hz", "500Hz", "1k", "2k", "4k"];
        let mut groups = Vec::with_capacity(labels.len());
        let phase = self.tick_count as f64 * 0.12;
        for (idx, label) in labels.iter().enumerate() {
            let t = phase + idx as f64 * 0.55;
            let a = (t.sin() * 0.5 + 0.5) * 70.0 + 10.0;
            let b = (t.cos() * 0.5 + 0.5) * 55.0 + 12.0;
            let c = ((t * 1.3).sin() * 0.5 + 0.5) * 40.0 + 8.0;
            groups.push(BarGroup::new(label, vec![a, b, c]));
        }

        let colors = vec![
            theme::accent::PRIMARY.into(),
            theme::accent::ACCENT_9.into(),
            theme::accent::ACCENT_10.into(),
        ];

        let chart = BarChart::new(groups)
            .direction(BarDirection::Vertical)
            .mode(BarMode::Stacked)
            .bar_width(1)
            .bar_gap(0)
            .group_gap(theme::spacing::XS)
            .colors(colors)
            .style(Style::new().fg(theme::fg::PRIMARY));

        chart.render(inner, frame);
    }

    fn render_linechart_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == ChartPanel::LineChart,
            theme::screen_accent::DATA_VIZ,
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Line Chart")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() || inner.width < 4 || inner.height < 4 {
            return;
        }

        let sine_points: Vec<(f64, f64)> = self
            .chart_data
            .sine_series
            .iter()
            .enumerate()
            .map(|(i, &v)| (i as f64, v))
            .collect();
        let cos_points: Vec<(f64, f64)> = self
            .chart_data
            .cosine_series
            .iter()
            .enumerate()
            .map(|(i, &v)| (i as f64, v))
            .collect();
        let noise_points: Vec<(f64, f64)> = self
            .chart_data
            .random_series
            .iter()
            .enumerate()
            .map(|(i, &v)| (i as f64, v))
            .collect();

        let line_colors = line_palette();
        let series = vec![
            Series::new("sin(t)", &sine_points, line_colors[0]),
            Series::new("cos(t)", &cos_points, line_colors[1]),
            Series::new("noise", &noise_points, line_colors[2]),
        ];

        let n = self.chart_data.sine_series.len() as f64;
        let chart = LineChart::new(series)
            .x_bounds(0.0, n.max(1.0))
            .y_bounds(-1.1, 1.1)
            .legend(true)
            .style(Style::new().fg(theme::fg::MUTED));

        chart.render(inner, frame);
    }

    fn render_canvas_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == ChartPanel::Canvas,
            theme::screen_accent::DATA_VIZ,
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Canvas + Heatmap")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() || inner.width < 2 || inner.height < 2 {
            return;
        }

        let (canvas_area, heatmap_area) = if inner.height >= 8 {
            let rows = Flex::vertical()
                .constraints([Constraint::Percentage(62.0), Constraint::Percentage(38.0)])
                .split(inner);
            (rows[0], Some(rows[1]))
        } else {
            (inner, None)
        };

        let mut painter = Painter::for_area(canvas_area, Mode::Braille);
        let (pw, ph) = painter.size();

        // Draw a Lissajous curve that evolves with tick count
        let a_freq = 3.0_f64;
        let b_freq = 2.0_f64;
        let phase = self.tick_count as f64 * 0.05;
        let cx = pw as f64 / 2.0;
        let cy = ph as f64 / 2.0;
        let rx = (pw as f64 / 2.0) - 2.0;
        let ry = (ph as f64 / 2.0) - 2.0;

        let steps = 500;
        for i in 0..steps {
            let t = (i as f64 / steps as f64) * std::f64::consts::TAU;
            let x = cx + rx * (a_freq * t + phase).sin();
            let y = cy + ry * (b_freq * t).sin();

            // Color gradient along the curve
            let color_t = i as f64 / steps as f64;
            let color = theme::accent_gradient(color_t + phase * 0.02);
            painter.point_colored(x as i32, y as i32, color);
        }

        // Draw border box
        painter.rect(0, 0, pw as i32 - 1, ph as i32 - 1);

        Canvas::from_painter(&painter)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(canvas_area, frame);

        if let Some(area) = heatmap_area {
            self.render_heatmap(frame, area);
        }
    }

    fn render_heatmap_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == ChartPanel::Heatmap,
            theme::screen_accent::DATA_VIZ,
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Signal Matrix")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() || inner.width < 4 || inner.height < 3 {
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(1), Constraint::Min(1)])
            .split(inner);

        if !rows[0].is_empty() {
            let legend_cols = Flex::horizontal()
                .constraints([Constraint::Fixed(14), Constraint::Min(1)])
                .split(rows[0]);
            Paragraph::new("Heatmap:")
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(legend_cols[0], frame);
            render_gradient_bar(frame, legend_cols[1]);
        }

        self.render_heatmap_grid(frame, rows[1]);
    }

    fn render_micro_panels(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let use_six = area.width >= 120;
        let cols = if use_six {
            Flex::horizontal()
                .gap(theme::spacing::XS)
                .constraints([
                    Constraint::Percentage(16.5),
                    Constraint::Percentage(16.5),
                    Constraint::Percentage(16.5),
                    Constraint::Percentage(16.5),
                    Constraint::Percentage(17.0),
                    Constraint::Percentage(17.0),
                ])
                .split(area)
        } else {
            Flex::horizontal()
                .gap(theme::spacing::XS)
                .constraints([
                    Constraint::Percentage(25.0),
                    Constraint::Percentage(25.0),
                    Constraint::Percentage(25.0),
                    Constraint::Percentage(25.0),
                ])
                .split(area)
        };

        let sine_data: Vec<f64> = self.chart_data.sine_series.iter().copied().collect();
        let cos_data: Vec<f64> = self.chart_data.cosine_series.iter().copied().collect();
        let rand_data: Vec<f64> = self.chart_data.random_series.iter().copied().collect();
        let mix_data: Vec<f64> = sine_data
            .iter()
            .zip(cos_data.iter())
            .map(|(a, b)| (a + b) * 0.5)
            .collect();
        let phase_data: Vec<f64> = sine_data
            .iter()
            .zip(cos_data.iter())
            .map(|(a, b)| a * b)
            .collect();
        let blend_data: Vec<f64> = sine_data
            .iter()
            .zip(rand_data.iter())
            .map(|(a, b)| (a + b) * 0.5)
            .collect();
        let colors = chart_palette();

        let panels = if use_six {
            vec![
                ("Sine", &sine_data, colors[0]),
                ("Cos", &cos_data, colors[1]),
                ("Noise", &rand_data, colors[2]),
                ("Mix", &mix_data, theme::accent::ACCENT_8.into()),
                ("Phase", &phase_data, theme::accent::ACCENT_6.into()),
                ("Blend", &blend_data, theme::accent::ACCENT_4.into()),
            ]
        } else {
            vec![
                ("Sine", &sine_data, colors[0]),
                ("Cos", &cos_data, colors[1]),
                ("Noise", &rand_data, colors[2]),
                ("Mix", &mix_data, theme::accent::ACCENT_8.into()),
            ]
        };

        for (area, (label, data, color)) in cols.iter().zip(panels.iter()) {
            let block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(label)
                .title_alignment(Alignment::Center)
                .style(theme::content_border());
            let inner = block.inner(*area);
            block.render(*area, frame);
            if !inner.is_empty() {
                let value = normalize_series_value(data.last().copied().unwrap_or(0.0));
                if inner.height >= 4 {
                    let rows = Flex::vertical()
                        .constraints([
                            Constraint::Fixed(1),
                            Constraint::Fixed(1),
                            Constraint::Min(1),
                        ])
                        .split(inner);
                    if !rows[0].is_empty() {
                        Sparkline::new(data)
                            .style(Style::new().fg(*color))
                            .render(rows[0], frame);
                    }
                    if !rows[1].is_empty() {
                        let colors = MiniBarColors::new(*color, *color, *color, *color);
                        MiniBar::new(value, rows[1].width)
                            .colors(colors)
                            .render(rows[1], frame);
                    }
                    if !rows[2].is_empty() {
                        let pct = format!("{:.0}%", value * 100.0);
                        Paragraph::new(pct)
                            .style(Style::new().fg(*color).bold())
                            .render(rows[2], frame);
                    }
                } else {
                    Sparkline::new(data)
                        .style(Style::new().fg(*color))
                        .render(inner, frame);
                }
            }
        }
    }

    fn render_heatmap(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.width < 4 || area.height < 3 {
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(1), Constraint::Min(1)])
            .split(area);

        if !rows[0].is_empty() {
            Paragraph::new("Heatmap")
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(rows[0], frame);
        }

        self.render_heatmap_grid(frame, rows[1]);
    }

    fn render_heatmap_grid(&self, frame: &mut Frame, grid: Rect) {
        if grid.is_empty() {
            return;
        }

        let w = grid.width.max(1) as f64;
        let h = grid.height.max(1) as f64;
        let phase = self.tick_count as f64 * 0.05;

        for dy in 0..grid.height {
            let ny = dy as f64 / (h - 1.0).max(1.0);
            for dx in 0..grid.width {
                let nx = dx as f64 / (w - 1.0).max(1.0);
                let wave_x = (nx * std::f64::consts::TAU * 1.2 + phase).sin() * 0.5 + 0.5;
                let wave_y = (ny * std::f64::consts::TAU * 0.9 - phase).cos() * 0.5 + 0.5;
                let value = (0.6 * wave_x + 0.4 * wave_y).clamp(0.0, 1.0);

                let color = heatmap_gradient(value);
                let mut cell = Cell::from_char(' ');
                cell.bg = color;
                if let Some(slot) = frame.buffer.get_mut(grid.x + dx, grid.y + dy) {
                    *slot = cell;
                }
            }
        }
    }
}

fn chart_palette() -> [PackedRgba; 3] {
    [
        theme::accent::PRIMARY.into(),
        theme::accent::SUCCESS.into(),
        theme::accent::WARNING.into(),
    ]
}

fn sparkline_gradients() -> [(PackedRgba, PackedRgba); 3] {
    [
        (
            theme::accent::PRIMARY.into(),
            theme::accent::ACCENT_7.into(),
        ),
        (
            theme::accent::SUCCESS.into(),
            theme::accent::ACCENT_9.into(),
        ),
        (
            theme::accent::WARNING.into(),
            theme::accent::ACCENT_10.into(),
        ),
    ]
}

fn line_palette() -> [PackedRgba; 3] {
    [
        theme::accent::PRIMARY.into(),
        theme::accent::SECONDARY.into(),
        theme::accent::WARNING.into(),
    ]
}

fn normalize_series_value(value: f64) -> f64 {
    if value.is_finite() {
        ((value + 1.0) * 0.5).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn render_mini_bar(frame: &mut Frame, area: Rect, label: &str, value: f64, color: PackedRgba) {
    if area.is_empty() {
        return;
    }

    let label_text = format!("{label} ");
    let label_width = display_width(&label_text) as u16;
    let cols = Flex::horizontal()
        .constraints([Constraint::Fixed(label_width), Constraint::Min(1)])
        .split(area);

    if !cols[0].is_empty() {
        Paragraph::new(label_text.as_str())
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(cols[0], frame);
    }

    if !cols[1].is_empty() {
        let colors = MiniBarColors::new(color, color, color, color);
        MiniBar::new(value, cols[1].width)
            .colors(colors)
            .style(Style::new().fg(theme::fg::MUTED))
            .render(cols[1], frame);
    }
}

fn render_gradient_bar(frame: &mut Frame, area: Rect) {
    if area.is_empty() {
        return;
    }

    let width = area.width.max(1) as f64;
    for dx in 0..area.width {
        let t = dx as f64 / (width - 1.0).max(1.0);
        let color = heatmap_gradient(t);
        let mut cell = Cell::from_char(' ');
        cell.bg = color;
        if let Some(slot) = frame.buffer.get_mut(area.x + dx, area.y) {
            *slot = cell;
        }
    }
}

impl Screen for DataViz {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            let spark = self.layout_sparkline.get();
            let bar = self.layout_barchart.get();
            let spectrum = self.layout_spectrum.get();
            let line = self.layout_linechart.get();
            let canvas = self.layout_canvas.get();
            let heatmap = self.layout_heatmap.get();
            self.focus = if spark.contains(mouse.x, mouse.y) {
                ChartPanel::Sparkline
            } else if bar.contains(mouse.x, mouse.y) {
                ChartPanel::BarChart
            } else if spectrum.contains(mouse.x, mouse.y) {
                ChartPanel::Spectrum
            } else if line.contains(mouse.x, mouse.y) {
                ChartPanel::LineChart
            } else if canvas.contains(mouse.x, mouse.y) {
                ChartPanel::Canvas
            } else if heatmap.contains(mouse.x, mouse.y) {
                ChartPanel::Heatmap
            } else {
                self.focus
            };
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::CTRL)
        {
            self.focus = self.focus.next();
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Right | KeyCode::Down,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && !modifiers.contains(Modifiers::CTRL)
        {
            self.focus = self.focus.next();
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Left,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::CTRL)
        {
            self.focus = self.focus.prev();
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Left | KeyCode::Up,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && !modifiers.contains(Modifiers::CTRL)
        {
            self.focus = self.focus.prev();
            return Cmd::None;
        }

        // Toggle bar direction with 'd'
        if let Event::Key(KeyEvent {
            code: KeyCode::Char('d'),
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            self.bar_horizontal = !self.bar_horizontal;
        }

        // Pause / resume data animation with Space
        if let Event::Key(KeyEvent {
            code: KeyCode::Char(' '),
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            self.paused = !self.paused;
        }

        // Reset chart data with 'r'
        if let Event::Key(KeyEvent {
            code: KeyCode::Char('r'),
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            let mut chart_data = ChartData::default();
            for t in 0..30 {
                chart_data.tick(t);
            }
            self.chart_data = chart_data;
            self.tick_count = 30;
        }

        Cmd::None
    }

    fn tick(&mut self, tick_count: u64) {
        if self.paused {
            return;
        }
        self.tick_count = tick_count;
        self.chart_data.tick(tick_count);
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let main = Flex::vertical()
            .constraints([Constraint::Min(1), Constraint::Fixed(1)])
            .split(area);

        // 3x3 grid of chart panels + micro strip
        let rows = Flex::vertical()
            .constraints([
                Constraint::Percentage(42.0),
                Constraint::Percentage(38.0),
                Constraint::Percentage(20.0),
            ])
            .split(main[0]);

        let top_cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(33.0),
                Constraint::Percentage(33.0),
                Constraint::Percentage(34.0),
            ])
            .split(rows[0]);

        let mid_cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(33.0),
                Constraint::Percentage(33.0),
                Constraint::Percentage(34.0),
            ])
            .split(rows[1]);

        self.layout_sparkline.set(top_cols[0]);
        self.layout_barchart.set(top_cols[1]);
        self.layout_spectrum.set(top_cols[2]);
        self.layout_linechart.set(mid_cols[0]);
        self.layout_canvas.set(mid_cols[1]);
        self.layout_heatmap.set(mid_cols[2]);

        self.render_sparkline_panel(frame, top_cols[0]);
        self.render_barchart_panel(frame, top_cols[1]);
        self.render_spectrum_panel(frame, top_cols[2]);
        self.render_linechart_panel(frame, mid_cols[0]);
        self.render_canvas_panel(frame, mid_cols[1]);
        self.render_heatmap_panel(frame, mid_cols[2]);
        self.render_micro_panels(frame, rows[2]);

        // Status bar
        let pause_label = if self.paused { " [PAUSED]" } else { "" };
        let status = format!(
            "Tick: {}{} | Arrows: panels | d: bar dir | Space: pause | r: reset",
            self.tick_count, pause_label
        );
        Paragraph::new(&*status)
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::alpha::SURFACE))
            .render(main[1], frame);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "\u{2190}/\u{2192}/\u{2191}/\u{2193}",
                action: "Switch panel",
            },
            HelpEntry {
                key: "d",
                action: "Toggle bar direction",
            },
            HelpEntry {
                key: "Space",
                action: if self.paused {
                    "Resume animation"
                } else {
                    "Pause animation"
                },
            },
            HelpEntry {
                key: "r",
                action: "Reset chart data",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Data Visualization"
    }

    fn tab_label(&self) -> &'static str {
        "Data Viz"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    fn ctrl_press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        })
    }

    #[test]
    fn initial_state() {
        let screen = DataViz::new();
        assert_eq!(screen.focus, ChartPanel::Sparkline);
        assert_eq!(screen.title(), "Data Visualization");
        assert_eq!(screen.tab_label(), "Data Viz");
    }

    #[test]
    fn panel_navigation() {
        let mut screen = DataViz::new();
        screen.update(&ctrl_press(KeyCode::Right));
        assert_eq!(screen.focus, ChartPanel::BarChart);
        screen.update(&ctrl_press(KeyCode::Right));
        assert_eq!(screen.focus, ChartPanel::Spectrum);
        screen.update(&ctrl_press(KeyCode::Left));
        assert_eq!(screen.focus, ChartPanel::BarChart);
    }

    #[test]
    fn tick_updates_data() {
        let mut screen = DataViz::new();
        let initial_len = screen.chart_data.sine_series.len();
        screen.tick(31);
        assert_eq!(screen.chart_data.sine_series.len(), initial_len + 1);
    }

    #[test]
    fn toggle_bar_direction() {
        let mut screen = DataViz::new();
        assert!(!screen.bar_horizontal);
        screen.update(&press(KeyCode::Char('d')));
        assert!(screen.bar_horizontal);
        screen.update(&press(KeyCode::Char('d')));
        assert!(!screen.bar_horizontal);
    }

    #[test]
    fn accent_gradient_is_nontransparent() {
        let c1 = theme::accent_gradient(0.0);
        let c2 = theme::accent_gradient(0.5);
        assert_ne!(c1, PackedRgba::TRANSPARENT);
        assert_ne!(c2, PackedRgba::TRANSPARENT);
    }

    #[test]
    fn space_pauses_animation() {
        let mut screen = DataViz::new();
        assert!(!screen.paused);
        screen.update(&press(KeyCode::Char(' ')));
        assert!(screen.paused);
        // Tick should be no-op when paused
        let tick_before = screen.tick_count;
        screen.tick(999);
        assert_eq!(screen.tick_count, tick_before);
        // Unpause
        screen.update(&press(KeyCode::Char(' ')));
        assert!(!screen.paused);
        screen.tick(999);
        assert_eq!(screen.tick_count, 999);
    }

    #[test]
    fn r_resets_chart_data() {
        let mut screen = DataViz::new();
        screen.tick(100);
        let tick_before_reset = screen.tick_count;
        assert!(tick_before_reset > 30);
        screen.update(&press(KeyCode::Char('r')));
        assert_eq!(
            screen.tick_count, 30,
            "Reset should restore initial tick count"
        );
    }

    #[test]
    fn keybindings_has_at_least_three() {
        let screen = DataViz::new();
        let bindings = screen.keybindings();
        assert!(
            bindings.len() >= 3,
            "DataViz should have at least 3 keybindings, got {}",
            bindings.len()
        );
    }
}

#![forbid(unsafe_code)]

//! Sparkline widget for compact trend visualization.
//!
//! Sparklines render data as a series of 8-level Unicode block characters
//! (▁▂▃▄▅▆▇█) for visualizing trends in minimal space.
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::sparkline::Sparkline;
//!
//! let data = vec![1.0, 4.0, 2.0, 8.0, 3.0, 6.0, 5.0];
//! let sparkline = Sparkline::new(&data)
//!     .style(Style::new().fg(PackedRgba::CYAN));
//! sparkline.render(area, frame);
//! ```

use crate::{MeasurableWidget, SizeConstraints, Widget};
use ftui_core::geometry::{Rect, Size};
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_style::Style;

/// Block characters for sparkline rendering (9 levels: empty + 8 bars).
const SPARK_CHARS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// A compact sparkline widget for trend visualization.
///
/// Sparklines display a series of values as a row of Unicode block characters,
/// with height proportional to value. Useful for showing trends in dashboards,
/// status bars, and data-dense UIs.
///
/// # Features
///
/// - Auto-scaling: Automatically determines min/max from data if not specified
/// - Manual bounds: Set explicit min/max for consistent scaling across multiple sparklines
/// - Color gradient: Optional start/end colors for value-based coloring
/// - Baseline: Optional baseline value (default 0.0) for distinguishing positive/negative
///
/// # Block Characters
///
/// Uses 9 levels of height: empty space plus 8 bar heights (▁▂▃▄▅▆▇█)
#[derive(Debug, Clone)]
pub struct Sparkline<'a> {
    /// Data values to display.
    data: &'a [f64],
    /// Optional minimum value (auto-detected if None).
    min: Option<f64>,
    /// Optional maximum value (auto-detected if None).
    max: Option<f64>,
    /// Base style for all characters.
    style: Style,
    /// Optional gradient: (low_color, high_color).
    gradient: Option<(PackedRgba, PackedRgba)>,
    /// Baseline value (default 0.0) - values at baseline show as empty.
    baseline: f64,
}

impl<'a> Sparkline<'a> {
    /// Create a new sparkline from data slice.
    #[must_use]
    pub fn new(data: &'a [f64]) -> Self {
        Self {
            data,
            min: None,
            max: None,
            style: Style::default(),
            gradient: None,
            baseline: 0.0,
        }
    }

    /// Set explicit minimum value for scaling.
    ///
    /// If not set, minimum is auto-detected from data.
    #[must_use]
    pub fn min(mut self, min: f64) -> Self {
        self.min = Some(min);
        self
    }

    /// Set explicit maximum value for scaling.
    ///
    /// If not set, maximum is auto-detected from data.
    #[must_use]
    pub fn max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }

    /// Set min and max bounds together.
    #[must_use]
    pub fn bounds(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    /// Set the base style (foreground color, etc.).
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set a color gradient from low to high values.
    ///
    /// Low values get `low_color`, high values get `high_color`,
    /// with linear interpolation between.
    #[must_use]
    pub fn gradient(mut self, low_color: PackedRgba, high_color: PackedRgba) -> Self {
        self.gradient = Some((low_color, high_color));
        self
    }

    /// Set the baseline value.
    ///
    /// Values at or below baseline show as empty space.
    /// Default is 0.0.
    #[must_use]
    pub fn baseline(mut self, baseline: f64) -> Self {
        self.baseline = baseline;
        self
    }

    /// Compute the min/max bounds from data or explicit settings.
    fn compute_bounds(&self) -> (f64, f64) {
        let data_min = self
            .min
            .unwrap_or_else(|| self.data.iter().copied().fold(f64::INFINITY, f64::min));
        let data_max = self
            .max
            .unwrap_or_else(|| self.data.iter().copied().fold(f64::NEG_INFINITY, f64::max));

        // Ensure min <= max; handle edge cases
        let min = if data_min.is_finite() { data_min } else { 0.0 };
        let max = if data_max.is_finite() { data_max } else { 1.0 };

        if min >= max {
            // All values are the same; create a range around the value
            (min - 0.5, max + 0.5)
        } else {
            (min, max)
        }
    }

    /// Map a value to a bar index (0-8).
    fn value_to_bar_index(&self, value: f64, min: f64, max: f64) -> usize {
        if !value.is_finite() {
            return 0;
        }

        if value <= self.baseline {
            return 0;
        }

        let range = max - min;
        if range <= 0.0 {
            return 4; // Middle bar for flat data
        }

        let normalized = (value - min) / range;
        let clamped = normalized.clamp(0.0, 1.0);
        // Map 0.0 -> 0, 1.0 -> 8
        (clamped * 8.0).round() as usize
    }

    /// Interpolate between two colors based on t (0.0 to 1.0).
    fn lerp_color(low: PackedRgba, high: PackedRgba, t: f64) -> PackedRgba {
        let t = t.clamp(0.0, 1.0) as f32;
        let r = (low.r() as f32 * (1.0 - t) + high.r() as f32 * t).round() as u8;
        let g = (low.g() as f32 * (1.0 - t) + high.g() as f32 * t).round() as u8;
        let b = (low.b() as f32 * (1.0 - t) + high.b() as f32 * t).round() as u8;
        let a = (low.a() as f32 * (1.0 - t) + high.a() as f32 * t).round() as u8;
        PackedRgba::rgba(r, g, b, a)
    }

    /// Render the sparkline as a string (for testing/debugging).
    pub fn render_to_string(&self) -> String {
        if self.data.is_empty() {
            return String::new();
        }

        let (min, max) = self.compute_bounds();
        self.data
            .iter()
            .map(|&v| {
                let idx = self.value_to_bar_index(v, min, max);
                SPARK_CHARS[idx]
            })
            .collect()
    }
}

impl Default for Sparkline<'_> {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl Widget for Sparkline<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Sparkline",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height,
            data_len = self.data.len()
        )
        .entered();

        if area.is_empty() || self.data.is_empty() {
            return;
        }

        let deg = frame.buffer.degradation;

        // Skeleton+: skip entirely
        if !deg.render_content() {
            return;
        }

        let (min, max) = self.compute_bounds();
        let range = max - min;

        // How many data points can we show?
        let display_count = (area.width as usize).min(self.data.len());

        for (i, &value) in self.data.iter().take(display_count).enumerate() {
            let x = area.x + i as u16;
            let y = area.y;

            if x >= area.right() {
                break;
            }

            let bar_idx = self.value_to_bar_index(value, min, max);
            let ch = SPARK_CHARS[bar_idx];

            let mut cell = Cell::from_char(ch);

            // Apply style
            if deg.apply_styling() {
                // Apply base style (fg, bg, attrs)
                crate::apply_style(&mut cell, self.style);

                // Override fg with gradient if configured
                if let Some((low_color, high_color)) = self.gradient {
                    let t = if range > 0.0 {
                        (value - min) / range
                    } else {
                        0.5
                    };
                    cell.fg = Self::lerp_color(low_color, high_color, t);
                } else if self.style.fg.is_none() {
                    // Default to white if no style fg and no gradient
                    cell.fg = PackedRgba::WHITE;
                }
            }

            frame.buffer.set_fast(x, y, cell);
        }
    }
}

impl MeasurableWidget for Sparkline<'_> {
    fn measure(&self, _available: Size) -> SizeConstraints {
        if self.data.is_empty() {
            return SizeConstraints::ZERO;
        }

        // Sparklines are always 1 row tall
        // Width is the number of data points
        let width = self.data.len() as u16;

        SizeConstraints {
            min: Size::new(1, 1), // At least 1 data point visible
            preferred: Size::new(width, 1),
            max: Some(Size::new(width, 1)), // Fixed content size
        }
    }

    fn has_intrinsic_size(&self) -> bool {
        !self.data.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    // --- Builder tests ---

    #[test]
    fn empty_data() {
        let sparkline = Sparkline::new(&[]);
        assert_eq!(sparkline.render_to_string(), "");
    }

    #[test]
    fn single_value() {
        let sparkline = Sparkline::new(&[5.0]);
        // Single value maps to middle bar
        let s = sparkline.render_to_string();
        assert_eq!(s.chars().count(), 1);
    }

    #[test]
    fn constant_values() {
        let data = vec![5.0, 5.0, 5.0, 5.0];
        let sparkline = Sparkline::new(&data);
        let s = sparkline.render_to_string();
        // All same height (middle bar)
        assert_eq!(s.chars().count(), 4);
        assert!(s.chars().all(|c| c == s.chars().next().unwrap()));
    }

    #[test]
    fn ascending_values() {
        let data: Vec<f64> = (0..9).map(|i| i as f64).collect();
        let sparkline = Sparkline::new(&data);
        let s = sparkline.render_to_string();
        let chars: Vec<char> = s.chars().collect();
        // First should be lowest, last should be highest
        assert_eq!(chars[0], ' ');
        assert_eq!(chars[8], '█');
    }

    #[test]
    fn descending_values() {
        let data: Vec<f64> = (0..9).rev().map(|i| i as f64).collect();
        let sparkline = Sparkline::new(&data);
        let s = sparkline.render_to_string();
        let chars: Vec<char> = s.chars().collect();
        // First should be highest, last should be lowest
        assert_eq!(chars[0], '█');
        assert_eq!(chars[8], ' ');
    }

    #[test]
    fn explicit_bounds() {
        let data = vec![5.0, 5.0, 5.0];
        let sparkline = Sparkline::new(&data).bounds(0.0, 10.0);
        let s = sparkline.render_to_string();
        // 5.0 is at 50%, should be middle bar (▄)
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars[0], '▄');
    }

    #[test]
    fn min_max_explicit() {
        let data = vec![0.0, 50.0, 100.0];
        let sparkline = Sparkline::new(&data).min(0.0).max(100.0);
        let s = sparkline.render_to_string();
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars[0], ' '); // 0%
        assert_eq!(chars[1], '▄'); // 50%
        assert_eq!(chars[2], '█'); // 100%
    }

    #[test]
    fn negative_values() {
        let data = vec![-10.0, 0.0, 10.0];
        let sparkline = Sparkline::new(&data);
        let s = sparkline.render_to_string();
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars[0], ' '); // Lowest
        assert_eq!(chars[2], '█'); // Highest
    }

    #[test]
    fn nan_values_handled() {
        let data = vec![1.0, f64::NAN, 3.0];
        let sparkline = Sparkline::new(&data);
        let s = sparkline.render_to_string();
        // NaN should render as empty (index 0)
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars[1], ' ');
    }

    #[test]
    fn infinity_values_handled() {
        let data = vec![f64::NEG_INFINITY, 0.0, f64::INFINITY];
        let sparkline = Sparkline::new(&data);
        let s = sparkline.render_to_string();
        // Infinities should be clamped
        assert_eq!(s.chars().count(), 3);
    }

    // --- Rendering tests ---

    #[test]
    fn render_empty_area() {
        let data = vec![1.0, 2.0, 3.0];
        let sparkline = Sparkline::new(&data);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Widget::render(&sparkline, area, &mut frame);
        // Should not panic
    }

    #[test]
    fn render_basic() {
        let data = vec![0.0, 0.5, 1.0];
        let sparkline = Sparkline::new(&data).bounds(0.0, 1.0);
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        Widget::render(&sparkline, area, &mut frame);

        let c0 = frame.buffer.get(0, 0).unwrap().content.as_char();
        let c1 = frame.buffer.get(1, 0).unwrap().content.as_char();
        let c2 = frame.buffer.get(2, 0).unwrap().content.as_char();

        assert_eq!(c0, Some(' ')); // 0%
        assert_eq!(c1, Some('▄')); // 50%
        assert_eq!(c2, Some('█')); // 100%
    }

    #[test]
    fn render_truncates_to_width() {
        let data: Vec<f64> = (0..100).map(|i| i as f64).collect();
        let sparkline = Sparkline::new(&data);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        Widget::render(&sparkline, area, &mut frame);

        // Should only render first 10 values
        for x in 0..10 {
            let cell = frame.buffer.get(x, 0).unwrap();
            assert!(cell.content.as_char().is_some());
        }
    }

    #[test]
    fn render_with_style() {
        let data = vec![1.0];
        let sparkline = Sparkline::new(&data).style(Style::new().fg(PackedRgba::GREEN));
        let area = Rect::new(0, 0, 1, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Widget::render(&sparkline, area, &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.fg, PackedRgba::GREEN);
    }

    #[test]
    fn render_with_gradient() {
        let data = vec![0.0, 0.5, 1.0];
        let sparkline = Sparkline::new(&data)
            .bounds(0.0, 1.0)
            .gradient(PackedRgba::BLUE, PackedRgba::RED);
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        Widget::render(&sparkline, area, &mut frame);

        let c0 = frame.buffer.get(0, 0).unwrap();
        let c2 = frame.buffer.get(2, 0).unwrap();

        // Low value should be blue-ish
        assert_eq!(c0.fg, PackedRgba::BLUE);
        // High value should be red-ish
        assert_eq!(c2.fg, PackedRgba::RED);
    }

    // --- Degradation tests ---

    #[test]
    fn degradation_skeleton_skips() {
        use ftui_render::budget::DegradationLevel;

        let data = vec![1.0, 2.0, 3.0];
        let sparkline = Sparkline::new(&data).style(Style::new().fg(PackedRgba::GREEN));
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::Skeleton;
        Widget::render(&sparkline, area, &mut frame);

        // All cells should be empty
        for x in 0..3 {
            assert!(
                frame.buffer.get(x, 0).unwrap().is_empty(),
                "cell at x={x} should be empty at Skeleton"
            );
        }
    }

    #[test]
    fn degradation_no_styling_renders_without_color() {
        use ftui_render::budget::DegradationLevel;

        let data = vec![0.5];
        let sparkline = Sparkline::new(&data)
            .bounds(0.0, 1.0)
            .style(Style::new().fg(PackedRgba::GREEN));
        let area = Rect::new(0, 0, 1, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::NoStyling;
        Widget::render(&sparkline, area, &mut frame);

        // Character should be rendered but without custom color
        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(cell.content.as_char().is_some());
        // fg should NOT be green since styling is disabled
        assert_ne!(cell.fg, PackedRgba::GREEN);
    }

    // --- Color interpolation tests ---

    #[test]
    fn lerp_color_endpoints() {
        let low = PackedRgba::rgb(0, 0, 0);
        let high = PackedRgba::rgb(255, 255, 255);

        assert_eq!(Sparkline::lerp_color(low, high, 0.0), low);
        assert_eq!(Sparkline::lerp_color(low, high, 1.0), high);
    }

    #[test]
    fn lerp_color_midpoint() {
        let low = PackedRgba::rgb(0, 0, 0);
        let high = PackedRgba::rgb(255, 255, 255);
        let mid = Sparkline::lerp_color(low, high, 0.5);

        assert_eq!(mid.r(), 128);
        assert_eq!(mid.g(), 128);
        assert_eq!(mid.b(), 128);
    }

    #[test]
    fn lerp_color_interpolates_alpha() {
        let low = PackedRgba::rgba(0, 0, 0, 0);
        let high = PackedRgba::rgba(255, 255, 255, 255);
        let mid = Sparkline::lerp_color(low, high, 0.5);

        assert_eq!(mid.r(), 128);
        assert_eq!(mid.g(), 128);
        assert_eq!(mid.b(), 128);
        assert_eq!(mid.a(), 128);
    }

    // --- MeasurableWidget tests ---

    #[test]
    fn measure_empty_sparkline() {
        let sparkline = Sparkline::new(&[]);
        let c = sparkline.measure(Size::MAX);
        assert_eq!(c, SizeConstraints::ZERO);
        assert!(!sparkline.has_intrinsic_size());
    }

    #[test]
    fn measure_single_value() {
        let data = [5.0];
        let sparkline = Sparkline::new(&data);
        let c = sparkline.measure(Size::MAX);

        assert_eq!(c.preferred.width, 1);
        assert_eq!(c.preferred.height, 1);
        assert!(sparkline.has_intrinsic_size());
    }

    #[test]
    fn measure_multiple_values() {
        let data: Vec<f64> = (0..50).map(|i| i as f64).collect();
        let sparkline = Sparkline::new(&data);
        let c = sparkline.measure(Size::MAX);

        assert_eq!(c.preferred.width, 50);
        assert_eq!(c.preferred.height, 1);
        assert_eq!(c.min.width, 1);
        assert_eq!(c.min.height, 1);
    }

    #[test]
    fn measure_max_equals_preferred() {
        let data = [1.0, 2.0, 3.0];
        let sparkline = Sparkline::new(&data);
        let c = sparkline.measure(Size::MAX);

        assert_eq!(c.max, Some(Size::new(3, 1)));
    }
}

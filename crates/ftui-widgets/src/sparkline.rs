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

use crate::{MeasurableWidget, SizeConstraints, Widget, clear_text_row};
use ftui_core::geometry::{Rect, Size};
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_style::Style;

/// Block characters for sparkline rendering (9 levels: empty + 8 bars).
const SPARK_CHARS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Optional glyph + style overrides that mark the extreme samples of a
/// [`Sparkline`].
///
/// Each marker replaces the bar glyph at the column of the first minimum or
/// first maximum sample (ties resolved to the earliest index) and merges its
/// style over that column's computed style (base + gradient). Markers refer to
/// the data extremes even when explicit scaling [`bounds`](Sparkline::bounds)
/// are set. When a single sample is both the min and the max, the max marker
/// wins. `NaN` samples are never extremes.
#[derive(Debug, Clone, Copy, Default)]
pub struct SparklineMarkers {
    /// Glyph and style for the first minimum sample, if enabled.
    pub min: Option<(char, Style)>,
    /// Glyph and style for the first maximum sample, if enabled.
    pub max: Option<(char, Style)>,
}

impl SparklineMarkers {
    /// The conventional maximum marker glyph (down-pointing triangle).
    pub const DEFAULT_MAX_GLYPH: char = '▾';
    /// The conventional minimum marker glyph (up-pointing triangle).
    pub const DEFAULT_MIN_GLYPH: char = '▴';
}

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
    /// Optional min/max marker glyphs and styles.
    markers: SparklineMarkers,
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
            markers: SparklineMarkers::default(),
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

    /// Mark the first minimum sample with `ch`, merging `style` over the
    /// column's computed style.
    ///
    /// The marker tracks the data minimum, independent of any explicit
    /// [`bounds`](Self::bounds). Ties resolve to the earliest sample; `NaN`
    /// samples are never extremes. Under an ASCII-only degradation level a
    /// non-ASCII glyph falls back to `^`. See [`SparklineMarkers`].
    #[must_use]
    pub fn with_min_marker(mut self, ch: char, style: Style) -> Self {
        self.markers.min = Some((ch, style));
        self
    }

    /// Mark the first maximum sample with `ch`, merging `style` over the
    /// column's computed style.
    ///
    /// The marker tracks the data maximum, independent of any explicit
    /// [`bounds`](Self::bounds). Ties resolve to the earliest sample; `NaN`
    /// samples are never extremes; and a single sample (both min and max) shows
    /// the max marker. Under an ASCII-only degradation level a non-ASCII glyph
    /// falls back to `v`. See [`SparklineMarkers`].
    #[must_use]
    pub fn with_max_marker(mut self, ch: char, style: Style) -> Self {
        self.markers.max = Some((ch, style));
        self
    }

    /// Set both markers at once.
    #[must_use]
    pub fn with_markers(mut self, markers: SparklineMarkers) -> Self {
        self.markers = markers;
        self
    }

    /// The configured markers.
    #[must_use]
    pub fn markers(&self) -> SparklineMarkers {
        self.markers
    }

    /// The indices of the first minimum and first maximum samples, ignoring
    /// `NaN`. Returns `None` when there is no finite sample (empty or all-NaN).
    /// Ties resolve to the earliest index.
    fn extreme_indices(data: &[f64]) -> Option<(usize, usize)> {
        let mut min_idx = None;
        let mut max_idx = None;
        let mut min_val = f64::INFINITY;
        let mut max_val = f64::NEG_INFINITY;
        for (i, &v) in data.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            if v < min_val {
                min_val = v;
                min_idx = Some(i);
            }
            if v > max_val {
                max_val = v;
                max_idx = Some(i);
            }
        }
        match (min_idx, max_idx) {
            (Some(min), Some(max)) => Some((min, max)),
            _ => None,
        }
    }

    /// The marker glyph, style, and whether it is the max marker for column
    /// `index`, or `None` when this column carries no marker. Max wins when a
    /// single sample is both extremes.
    fn column_marker(
        &self,
        index: usize,
        extremes: Option<(usize, usize)>,
    ) -> Option<(char, Style, bool)> {
        let (min_i, max_i) = extremes?;
        if index == max_i
            && let Some((ch, style)) = self.markers.max
        {
            Some((ch, style, true))
        } else if index == min_i
            && let Some((ch, style)) = self.markers.min
        {
            Some((ch, style, false))
        } else {
            None
        }
    }

    /// Apply the crate's ASCII degradation to a marker glyph: a non-ASCII glyph
    /// becomes `v` (max) or `^` (min) when Unicode borders are disabled.
    fn degrade_marker(ch: char, is_max: bool, unicode: bool) -> char {
        if unicode || ch.is_ascii() {
            ch
        } else if is_max {
            'v'
        } else {
            '^'
        }
    }

    /// A short label of which markers are enabled, for the render span.
    #[cfg(feature = "tracing")]
    fn markers_label(&self) -> &'static str {
        match (self.markers.min.is_some(), self.markers.max.is_some()) {
            (true, true) => "both",
            (true, false) => "min",
            (false, true) => "max",
            (false, false) => "none",
        }
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
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) } as f32;
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
        let extremes = Self::extreme_indices(self.data);
        self.data
            .iter()
            .enumerate()
            .map(|(i, &v)| match self.column_marker(i, extremes) {
                // Text mode renders the glyph as chosen (no ASCII degradation).
                Some((ch, _style, _is_max)) => ch,
                None => SPARK_CHARS[self.value_to_bar_index(v, min, max)],
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
            data_len = self.data.len(),
            markers = self.markers_label()
        )
        .entered();

        if area.is_empty() {
            return;
        }

        let deg = frame.buffer.degradation;

        // Skeleton+: skip entirely
        if !deg.render_content() {
            return;
        }

        let base_style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };
        clear_text_row(frame, area, base_style);

        if self.data.is_empty() {
            return;
        }

        let (min, max) = self.compute_bounds();
        let range = max - min;

        // Extreme columns are marked only when they fall inside the visible
        // window (the leftmost `display_count` samples).
        let extremes = Self::extreme_indices(self.data);
        let unicode = deg.use_unicode_borders();

        // How many data points can we show?
        let display_count = (area.width as usize).min(self.data.len());

        for (i, &value) in self.data.iter().take(display_count).enumerate() {
            let x = area.x + i as u16;
            let y = area.y;

            if x >= area.right() {
                break;
            }

            let bar_idx = self.value_to_bar_index(value, min, max);
            let (ch, marker_style) = match self.column_marker(i, extremes) {
                Some((glyph, style, is_max)) => {
                    (Self::degrade_marker(glyph, is_max, unicode), Some(style))
                }
                None => (SPARK_CHARS[bar_idx], None),
            };

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

                // Merge the marker style over the computed column style, so a
                // marker recolours the extreme without dropping the gradient.
                if let Some(style) = marker_style {
                    crate::apply_style(&mut cell, style);
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

    #[test]
    fn render_shorter_data_clears_stale_suffix() {
        let long = Sparkline::new(&[0.0, 0.5, 1.0, 0.75]).bounds(0.0, 1.0);
        let short = Sparkline::new(&[1.0]);
        let area = Rect::new(0, 0, 4, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 1, &mut pool);

        Widget::render(&long, area, &mut frame);
        Widget::render(&short, area, &mut frame);

        let row: String = (0..4)
            .map(|x| {
                frame
                    .buffer
                    .get(x, 0)
                    .and_then(|cell| cell.content.as_char())
                    .unwrap_or(' ')
            })
            .collect();
        assert_eq!(row, "▄   ");
    }

    #[test]
    fn render_empty_data_clears_stale_sparkline() {
        let long = Sparkline::new(&[0.0, 0.5, 1.0]).bounds(0.0, 1.0);
        let empty = Sparkline::new(&[]);
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);

        Widget::render(&long, area, &mut frame);
        Widget::render(&empty, area, &mut frame);

        for x in 0..3 {
            assert_eq!(
                frame
                    .buffer
                    .get(x, 0)
                    .and_then(|cell| cell.content.as_char()),
                Some(' ')
            );
        }
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

    mod marker_tests {
        use super::*;
        use proptest::prelude::*;

        fn chars_of(s: &str) -> Vec<char> {
            s.chars().collect()
        }

        #[test]
        fn min_marker_placed_at_first_min() {
            // First minimum (1.0) is at index 1; the later 1.0 stays a bar.
            let data = [3.0, 1.0, 2.0, 1.0];
            let s = Sparkline::new(&data)
                .with_min_marker('L', Style::default())
                .render_to_string();
            let chars = chars_of(&s);
            assert_eq!(chars[1], 'L');
            assert_eq!(chars.iter().filter(|&&c| c == 'L').count(), 1);
        }

        #[test]
        fn max_marker_placed_at_first_max() {
            // First maximum (3.0) is at index 1; the later 3.0 stays a bar.
            let data = [1.0, 3.0, 2.0, 3.0];
            let s = Sparkline::new(&data)
                .with_max_marker('H', Style::default())
                .render_to_string();
            let chars = chars_of(&s);
            assert_eq!(chars[1], 'H');
            assert_eq!(chars.iter().filter(|&&c| c == 'H').count(), 1);
        }

        #[test]
        fn markers_respect_explicit_bounds() {
            // Bounds only scale the bars; markers still track the data extremes.
            let data = [2.0, 8.0, 5.0];
            let s = Sparkline::new(&data)
                .bounds(0.0, 100.0)
                .with_min_marker('L', Style::default())
                .with_max_marker('H', Style::default())
                .render_to_string();
            let chars = chars_of(&s);
            assert_eq!(chars[0], 'L', "data min, not the bound min");
            assert_eq!(chars[1], 'H', "data max, not the bound max");
        }

        #[test]
        fn single_sample_gets_max_marker() {
            let data = [5.0];
            let both = Sparkline::new(&data)
                .with_min_marker('L', Style::default())
                .with_max_marker('H', Style::default())
                .render_to_string();
            assert_eq!(both, "H", "a single sample is both extremes; max wins");
            // Only a min marker still shows on the single sample.
            let min_only = Sparkline::new(&data)
                .with_min_marker('L', Style::default())
                .render_to_string();
            assert_eq!(min_only, "L");
        }

        #[test]
        fn render_to_string_includes_markers() {
            let data = [1.0, 5.0, 3.0];
            let s = Sparkline::new(&data)
                .with_min_marker('v', Style::default())
                .with_max_marker('^', Style::default())
                .render_to_string();
            assert!(s.contains('v') && s.contains('^'), "{s}");
        }

        #[test]
        fn empty_data_no_panic() {
            let sparkline = Sparkline::new(&[])
                .with_min_marker('L', Style::default())
                .with_max_marker('H', Style::default());
            assert_eq!(sparkline.render_to_string(), "");
            // Rendering empty data to a frame must not panic.
            let area = Rect::new(0, 0, 3, 1);
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(3, 1, &mut pool);
            Widget::render(&sparkline, area, &mut frame);
        }

        #[test]
        fn nan_samples_are_not_extremes() {
            let data = [f64::NAN, 5.0, 1.0, f64::NAN];
            // min at index 2 (1.0), max at index 1 (5.0); NaNs ignored.
            assert_eq!(Sparkline::extreme_indices(&data), Some((2, 1)));
            // All-NaN has no extremes.
            assert_eq!(Sparkline::extreme_indices(&[f64::NAN, f64::NAN]), None);
            assert_eq!(Sparkline::extreme_indices(&[]), None);
        }

        #[test]
        fn marker_outside_window_not_drawn() {
            // Ascending data: min at 0 (inside a width-5 window), max at 19
            // (outside it). The window is the leftmost `area.width` samples.
            let data: Vec<f64> = (0..20).map(|i| i as f64).collect();
            let sparkline = Sparkline::new(&data)
                .with_min_marker('L', Style::default())
                .with_max_marker('H', Style::default());
            let area = Rect::new(0, 0, 5, 1);
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(5, 1, &mut pool);
            Widget::render(&sparkline, area, &mut frame);

            assert_eq!(
                frame.buffer.get(0, 0).unwrap().content.as_char(),
                Some('L'),
                "the min is inside the window"
            );
            for x in 0..5 {
                assert_ne!(
                    frame.buffer.get(x, 0).unwrap().content.as_char(),
                    Some('H'),
                    "the max column is outside the window"
                );
            }
        }

        #[test]
        fn marker_style_merges_over_gradient() {
            // The max marker sets only a background; the gradient foreground at
            // that column must survive the merge.
            let data = [0.0, 10.0];
            let sparkline = Sparkline::new(&data)
                .bounds(0.0, 10.0)
                .gradient(PackedRgba::BLUE, PackedRgba::GREEN)
                .with_max_marker('H', Style::new().bg(PackedRgba::WHITE));
            let area = Rect::new(0, 0, 2, 1);
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(2, 1, &mut pool);
            Widget::render(&sparkline, area, &mut frame);

            let max_cell = frame.buffer.get(1, 0).unwrap();
            assert_eq!(max_cell.content.as_char(), Some('H'));
            assert_eq!(max_cell.fg, PackedRgba::GREEN, "gradient fg preserved");
            assert_eq!(max_cell.bg, PackedRgba::WHITE, "marker bg applied");
        }

        #[test]
        fn ascii_degradation_falls_back_to_carets() {
            use ftui_render::budget::DegradationLevel;

            let data = [1.0, 9.0, 3.0];
            let sparkline = Sparkline::new(&data)
                .with_min_marker(SparklineMarkers::DEFAULT_MIN_GLYPH, Style::default())
                .with_max_marker(SparklineMarkers::DEFAULT_MAX_GLYPH, Style::default());
            let area = Rect::new(0, 0, 3, 1);
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(3, 1, &mut pool);
            frame.buffer.degradation = DegradationLevel::SimpleBorders;
            Widget::render(&sparkline, area, &mut frame);

            assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('^'));
            assert_eq!(frame.buffer.get(1, 0).unwrap().content.as_char(), Some('v'));
        }

        #[test]
        fn no_markers_matches_plain_render() {
            // With no markers set, render_to_string is byte-identical to the
            // pre-feature output.
            let data = [0.0, 4.0, 2.0, 8.0, 3.0];
            let plain = Sparkline::new(&data).render_to_string();
            let still_plain = Sparkline::new(&data)
                .with_markers(SparklineMarkers::default())
                .render_to_string();
            assert_eq!(plain, still_plain);
        }

        #[test]
        fn sparkline_markers_20x1() {
            // A 20-wide render with a unique min at column 3 and a unique max at
            // column 12; both markers land on exactly those columns.
            let mut data = [5.0f64; 20];
            data[3] = 0.0;
            data[12] = 9.0;
            let sparkline = Sparkline::new(&data)
                .with_min_marker(SparklineMarkers::DEFAULT_MIN_GLYPH, Style::default())
                .with_max_marker(SparklineMarkers::DEFAULT_MAX_GLYPH, Style::default());
            let area = Rect::new(0, 0, 20, 1);
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(20, 1, &mut pool);
            Widget::render(&sparkline, area, &mut frame);

            let row: String = (0..20)
                .map(|x| {
                    frame
                        .buffer
                        .get(x, 0)
                        .and_then(|c| c.content.as_char())
                        .unwrap_or(' ')
                })
                .collect();
            let chars: Vec<char> = row.chars().collect();
            assert_eq!(chars[3], SparklineMarkers::DEFAULT_MIN_GLYPH);
            assert_eq!(chars[12], SparklineMarkers::DEFAULT_MAX_GLYPH);
            assert_eq!(
                chars
                    .iter()
                    .filter(|&&c| c == SparklineMarkers::DEFAULT_MIN_GLYPH)
                    .count(),
                1
            );
            assert_eq!(
                chars
                    .iter()
                    .filter(|&&c| c == SparklineMarkers::DEFAULT_MAX_GLYPH)
                    .count(),
                1
            );
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            /// `extreme_indices` returns the earliest indices whose values equal
            /// the min and max of the (NaN-free) data.
            #[test]
            fn extreme_indices_finds_earliest_extremes(
                data in prop::collection::vec(-100.0f64..100.0, 1..30),
            ) {
                let (min_i, max_i) = Sparkline::extreme_indices(&data)
                    .expect("finite data has extremes");
                let dmin = data.iter().copied().fold(f64::INFINITY, f64::min);
                let dmax = data.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                prop_assert_eq!(data[min_i], dmin);
                prop_assert_eq!(data[max_i], dmax);
                prop_assert!(data[..min_i].iter().all(|&v| v > dmin), "earliest min");
                prop_assert!(data[..max_i].iter().all(|&v| v < dmax), "earliest max");
            }
        }
    }
}

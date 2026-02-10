#![forbid(unsafe_code)]

//! Canvas widget for arbitrary pixel/shape drawing using Braille, block,
//! or half-block characters.
//!
//! ## Metaball Rendering
//!
//! The [`Painter::render_metaball_field`] method provides sub-pixel metaball
//! rendering using the shared sampling API from [`crate::visual_fx::effects::sampling`].
//!
//! Each terminal cell maps to a grid of sub-pixels whose resolution depends
//! on the chosen [`Mode`]:
//!
//! | Mode        | Sub-pixels per cell | Chars used       |
//! |-------------|--------------------:|------------------|
//! | `Braille`   | 2 × 4 = 8          | U+2800..U+28FF   |
//! | `Block`     | 2 × 2 = 4          | Quarter blocks   |
//! | `HalfBlock` | 1 × 2 = 2          | Upper/lower half |
//!
//! # Example
//!
//! ```ignore
//! use ftui_extras::canvas::{Canvas, Mode, Painter};
//!
//! let mut painter = Painter::new(40, 20, Mode::Braille);
//! painter.line(0, 0, 39, 19);
//! painter.rect(5, 3, 15, 10);
//! painter.circle(20, 10, 8);
//!
//! let canvas = Canvas::from_painter(&painter);
//! canvas.render(area, &mut buf);
//! ```

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_widgets::Widget;

/// Resolution mode for the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// 2×4 dots per cell using Unicode Braille patterns (U+2800..U+28FF).
    #[default]
    Braille,
    /// 2×2 quadrants per cell using Unicode quarter-block characters.
    Block,
    /// 1×2 vertical halves per cell using half-block characters.
    HalfBlock,
}

impl Mode {
    /// Sub-pixel columns per terminal cell.
    #[inline]
    pub const fn cols_per_cell(self) -> u16 {
        match self {
            Mode::Braille => 2,
            Mode::Block => 2,
            Mode::HalfBlock => 1,
        }
    }

    /// Sub-pixel rows per terminal cell.
    #[inline]
    pub const fn rows_per_cell(self) -> u16 {
        match self {
            Mode::Braille => 4,
            Mode::Block => 2,
            Mode::HalfBlock => 2,
        }
    }
}

/// A painter that accumulates pixel-level drawing operations on a virtual grid.
///
/// The grid dimensions are in sub-pixels. After drawing, convert to a
/// [`Canvas`] widget for rendering.
#[derive(Debug, Clone)]
pub struct Painter {
    /// Width in sub-pixels.
    width: u16,
    /// Height in sub-pixels.
    height: u16,
    /// Cached width as i32 for bounds checks.
    width_i32: i32,
    /// Cached height as i32 for bounds checks.
    height_i32: i32,
    /// Cached width as u32 for unsigned bounds checks.
    width_u32: u32,
    /// Cached height as u32 for unsigned bounds checks.
    height_u32: u32,
    /// Cached width as usize for index math.
    width_usize: usize,
    /// Resolution mode.
    mode: Mode,
    /// Pixel generation marks (row-major). A pixel is on when `pixels[idx] == generation`.
    pixels: Vec<u32>,
    /// Current clear generation for O(1) clears.
    generation: u32,
    /// Generation marker for full-canvas coverage.
    ///
    /// When this equals `generation`, all in-bounds pixels are treated as "on"
    /// without consulting per-pixel marks.
    full_coverage_generation: u32,
    /// Color per pixel (only stored when set; default = foreground).
    colors: Vec<Option<PackedRgba>>,
}

impl Painter {
    /// Create a new painter with the given sub-pixel dimensions and mode.
    pub fn new(width: u16, height: u16, mode: Mode) -> Self {
        let len = width as usize * height as usize;
        Self {
            width,
            height,
            width_i32: width as i32,
            height_i32: height as i32,
            width_u32: width as u32,
            height_u32: height as u32,
            width_usize: width as usize,
            mode,
            pixels: vec![0; len],
            generation: 1,
            full_coverage_generation: 0,
            colors: vec![None; len],
        }
    }

    /// Create a painter sized to fill a terminal area.
    pub fn for_area(area: Rect, mode: Mode) -> Self {
        let width = area.width.saturating_mul(mode.cols_per_cell());
        let height = area.height.saturating_mul(mode.rows_per_cell());
        Self::new(width, height, mode)
    }

    /// Ensure the painter has at least the given sub-pixel dimensions.
    ///
    /// This is a grow-only operation; buffers never shrink.
    pub fn ensure_size(&mut self, width: u16, height: u16, mode: Mode) {
        self.mode = mode;
        self.width = width;
        self.height = height;
        self.width_i32 = width as i32;
        self.height_i32 = height as i32;
        self.width_u32 = width as u32;
        self.height_u32 = height as u32;
        self.width_usize = width as usize;
        let len = width as usize * height as usize;
        if len > self.pixels.len() {
            self.pixels.resize(len, 0);
            self.colors.resize(len, None);
        }
    }

    /// Ensure the painter can cover a terminal area at the given mode.
    pub fn ensure_for_area(&mut self, area: Rect, mode: Mode) {
        let width = area.width.saturating_mul(mode.cols_per_cell());
        let height = area.height.saturating_mul(mode.rows_per_cell());
        self.ensure_size(width, height, mode);
    }

    /// Clear all pixels.
    pub fn clear(&mut self) {
        if self.generation == u32::MAX {
            // Rare wraparound path: reset marks to zero and restart generations.
            self.pixels.fill(0);
            self.generation = 1;
            self.full_coverage_generation = 0;
        } else {
            self.generation += 1;
        }
    }

    /// Mark the current frame as fully covered (every in-bounds pixel is "on").
    ///
    /// This is a rendering optimization for dense effects (for example plasma)
    /// that write every sub-pixel each frame.
    #[inline]
    pub fn mark_full_coverage(&mut self) {
        self.full_coverage_generation = self.generation;
    }

    /// Set a single pixel.
    #[inline]
    pub fn point(&mut self, x: i32, y: i32) {
        let xu = x as u32;
        let yu = y as u32;
        if xu >= self.width_u32 || yu >= self.height_u32 {
            return;
        }
        let idx = yu as usize * self.width_usize + xu as usize;
        if !self.is_full_coverage_current() {
            self.pixels[idx] = self.generation;
        }
        // Uncolored points must not inherit stale color from older generations.
        self.colors[idx] = None;
    }

    /// Set a single pixel with color.
    #[inline]
    pub fn point_colored(&mut self, x: i32, y: i32, color: PackedRgba) {
        let xu = x as u32;
        let yu = y as u32;
        if xu >= self.width_u32 || yu >= self.height_u32 {
            return;
        }
        let idx = yu as usize * self.width_usize + xu as usize;
        if !self.is_full_coverage_current() {
            self.pixels[idx] = self.generation;
        }
        self.colors[idx] = Some(color);
    }

    /// Set a single pixel with color when coordinates are already bounds-checked.
    ///
    /// This avoids repeated `i32` conversion and bounds checks in tight inner loops.
    #[inline]
    pub fn point_colored_in_bounds(&mut self, x: usize, y: usize, color: PackedRgba) {
        debug_assert!(x < self.width_usize);
        debug_assert!(y < self.height as usize);
        let idx = y * self.width_usize + x;
        self.point_colored_at_index_in_bounds(idx, color);
    }

    /// Set a single pixel with color by precomputed in-bounds index.
    ///
    /// This avoids repeated coordinate-to-index math in hot loops.
    #[inline]
    pub fn point_colored_at_index_in_bounds(&mut self, idx: usize, color: PackedRgba) {
        debug_assert!(idx < self.pixels.len());
        if !self.is_full_coverage_current() {
            self.pixels[idx] = self.generation;
        }
        self.colors[idx] = Some(color);
    }

    /// Set color by precomputed in-bounds index for full-coverage frames.
    ///
    /// Callers must ensure full coverage is active for the current generation.
    #[inline]
    pub fn set_color_at_index_in_bounds(&mut self, idx: usize, color: PackedRgba) {
        debug_assert!(idx < self.colors.len());
        debug_assert!(self.is_full_coverage_current());
        self.colors[idx] = Some(color);
    }

    /// Draw a line from (x0, y0) to (x1, y1) using Bresenham's algorithm.
    #[inline]
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.line_colored(x0, y0, x1, y1, None);
    }

    /// Draw a colored line.
    #[inline]
    pub fn line_colored(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Option<PackedRgba>) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx: i32 = if x0 < x1 { 1 } else { -1 };
        let sy: i32 = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let mut cx = x0;
        let mut cy = y0;

        loop {
            if let Some(c) = color {
                self.point_colored(cx, cy, c);
            } else {
                self.point(cx, cy);
            }

            if cx == x1 && cy == y1 {
                break;
            }

            let e2 = 2 * err;
            if e2 >= dy {
                if cx == x1 {
                    break;
                }
                err += dy;
                cx += sx;
            }
            if e2 <= dx {
                if cy == y1 {
                    break;
                }
                err += dx;
                cy += sy;
            }
        }
    }

    /// Draw an axis-aligned rectangle outline.
    #[inline]
    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if w <= 0 || h <= 0 {
            return;
        }
        self.line(x, y, x + w - 1, y);
        self.line(x + w - 1, y, x + w - 1, y + h - 1);
        self.line(x + w - 1, y + h - 1, x, y + h - 1);
        self.line(x, y + h - 1, x, y);
    }

    /// Draw a filled rectangle.
    #[inline]
    pub fn rect_filled(&mut self, x: i32, y: i32, w: i32, h: i32) {
        for dy in 0..h {
            for dx in 0..w {
                self.point(x + dx, y + dy);
            }
        }
    }

    /// Draw a filled convex polygon.
    #[inline]
    pub fn polygon_filled(&mut self, points: &[(i32, i32)]) {
        if points.len() < 3 {
            return;
        }
        let (mut min_x, mut max_x) = (points[0].0, points[0].0);
        let (mut min_y, mut max_y) = (points[0].1, points[0].1);
        for &(x, y) in points.iter().skip(1) {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                if point_in_convex_polygon(x, y, points) {
                    self.point(x, y);
                }
            }
        }
    }

    /// Draw a circle outline using the midpoint algorithm.
    #[inline]
    pub fn circle(&mut self, cx: i32, cy: i32, radius: i32) {
        if radius <= 0 {
            self.point(cx, cy);
            return;
        }

        let mut x = radius;
        let mut y = 0;
        let mut d = 1 - radius;

        while x >= y {
            self.plot_circle_octants(cx, cy, x, y);
            y += 1;
            if d < 0 {
                d += 2 * y + 1;
            } else {
                x -= 1;
                d += 2 * (y - x) + 1;
            }
        }
    }

    #[inline]
    fn plot_circle_octants(&mut self, cx: i32, cy: i32, x: i32, y: i32) {
        self.point(cx + x, cy + y);
        self.point(cx - x, cy + y);
        self.point(cx + x, cy - y);
        self.point(cx - x, cy - y);
        self.point(cx + y, cy + x);
        self.point(cx - y, cy + x);
        self.point(cx + y, cy - x);
        self.point(cx - y, cy - x);
    }

    // -----------------------------------------------------------------------
    // Metaball Field Rendering (requires visual-fx feature)
    // -----------------------------------------------------------------------

    /// Render a metaball field to the painter at sub-pixel resolution.
    ///
    /// This is a low-level method that accepts a custom color function. For a
    /// higher-level API with theme integration and animation support, see
    /// [`crate::visual_fx::effects::canvas_adapters::MetaballsCanvasAdapter`].
    ///
    /// This method uses the shared sampling API from [`crate::visual_fx::effects::sampling`]
    /// to render metaballs with sub-pixel precision using the canvas's resolution mode.
    ///
    /// # Arguments
    ///
    /// - `sampler`: A [`MetaballFieldSampler`] containing ball positions and radii
    /// - `threshold`: Field intensity for full color (typically 1.0)
    /// - `glow_threshold`: Field intensity where glow begins (typically 0.6)
    /// - `quality`: Quality level affecting how many balls contribute
    /// - `color_fn`: Function mapping (hue, intensity) to a color
    ///
    /// # Coordinate Mapping
    ///
    /// Sub-pixel coordinates are mapped to normalized `[0.0, 1.0]` space using
    /// [`cell_to_normalized`](crate::visual_fx::effects::sampling::cell_to_normalized),
    /// ensuring consistent sampling regardless of canvas resolution.
    ///
    /// # Performance
    ///
    /// - Allocation-free in steady state (after initial Painter setup)
    /// - O(width × height × balls) complexity
    /// - Quality parameter allows graceful degradation
    ///
    /// # Example
    ///
    /// ```ignore
    /// use ftui_extras::canvas::{Painter, Mode};
    /// use ftui_extras::visual_fx::effects::sampling::{MetaballFieldSampler, BallState};
    /// use ftui_extras::visual_fx::FxQuality;
    /// use ftui_render::cell::PackedRgba;
    ///
    /// let balls = vec![
    ///     BallState { x: 0.3, y: 0.5, r2: 0.04, hue: 0.0 },
    ///     BallState { x: 0.7, y: 0.5, r2: 0.04, hue: 0.5 },
    /// ];
    /// let sampler = MetaballFieldSampler::new(balls);
    ///
    /// let mut painter = Painter::for_area(area, Mode::Braille);
    /// painter.render_metaball_field(
    ///     &sampler,
    ///     1.0,  // threshold
    ///     0.6,  // glow_threshold
    ///     FxQuality::Full,
    ///     |hue, intensity| {
    ///         let r = (hue * 255.0) as u8;
    ///         let a = (intensity * 255.0) as u8;
    ///         PackedRgba::rgba(r, 100, 200, a)
    ///     },
    /// );
    /// ```
    #[cfg(feature = "visual-fx")]
    pub fn render_metaball_field<F>(
        &mut self,
        sampler: &crate::visual_fx::effects::sampling::MetaballFieldSampler,
        threshold: f64,
        glow_threshold: f64,
        quality: crate::visual_fx::FxQuality,
        color_fn: F,
    ) where
        F: Fn(f64, f64) -> PackedRgba,
    {
        use crate::visual_fx::effects::sampling::cell_to_normalized;

        if !quality.is_enabled() || self.width == 0 || self.height == 0 {
            return;
        }

        let threshold = threshold.max(glow_threshold + 0.0001);

        for py in 0..self.height {
            let ny = cell_to_normalized(py, self.height);
            for px in 0..self.width {
                let nx = cell_to_normalized(px, self.width);

                let (field_sum, avg_hue) = sampler.sample_field(nx, ny, quality);

                if field_sum > glow_threshold {
                    let intensity = if field_sum >= threshold {
                        1.0
                    } else {
                        (field_sum - glow_threshold) / (threshold - glow_threshold)
                    };

                    let color = color_fn(avg_hue, intensity);
                    self.point_colored_in_bounds(px as usize, py as usize, color);
                }
            }
        }
    }

    /// Get the sub-pixel dimensions.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// Current backing buffer length (pixels).
    pub fn buffer_len(&self) -> usize {
        self.pixels.len()
    }

    /// Get the terminal cell dimensions needed to display this painter.
    pub fn cell_size(&self) -> (u16, u16) {
        let cols = self.mode.cols_per_cell();
        let rows = self.mode.rows_per_cell();
        (self.width.div_ceil(cols), self.height.div_ceil(rows))
    }

    /// Check if a pixel is set.
    pub fn get(&self, x: i32, y: i32) -> bool {
        let xu = x as u32;
        let yu = y as u32;
        if xu >= self.width_u32 || yu >= self.height_u32 {
            return false;
        }
        if self.is_full_coverage_current() {
            return true;
        }
        let idx = yu as usize * self.width_usize + xu as usize;
        self.pixels[idx] == self.generation
    }

    #[inline]
    fn is_full_coverage_current(&self) -> bool {
        self.full_coverage_generation == self.generation
    }

    #[inline]
    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let xu = x as u32;
        let yu = y as u32;
        if xu >= self.width_u32 || yu >= self.height_u32 {
            return None;
        }
        Some(yu as usize * self.width_usize + xu as usize)
    }

    /// Render this painter's pixels into a cell grid.
    pub(crate) fn render_to_buffer(&self, area: Rect, buf: &mut Buffer, style: Style) {
        let cols = self.mode.cols_per_cell() as i32;
        let rows = self.mode.rows_per_cell() as i32;

        let cell_cols = area
            .width
            .min(self.width.div_ceil(self.mode.cols_per_cell()));
        let cell_rows = area
            .height
            .min(self.height.div_ceil(self.mode.rows_per_cell()));

        for cy in 0..cell_rows as i32 {
            for cx in 0..cell_cols as i32 {
                let px_x = cx * cols;
                let px_y = cy * rows;

                let (ch, fg_color, bg_color) = match self.mode {
                    Mode::Braille => self.braille_cell(px_x, px_y),
                    Mode::Block => self.block_cell(px_x, px_y),
                    Mode::HalfBlock => self.halfblock_cell(px_x, px_y),
                };

                if ch == ' ' {
                    continue;
                }

                let mut cell = Cell::from_char(ch);
                if let Some(fg) = style.fg {
                    cell.fg = fg;
                }
                if let Some(bg) = style.bg {
                    cell.bg = bg;
                }
                // Per-pixel color overrides base style
                if let Some(c) = fg_color {
                    cell.fg = c;
                }
                if let Some(c) = bg_color {
                    cell.bg = c;
                }

                buf.set_fast(
                    area.x.saturating_add(cx as u16),
                    area.y.saturating_add(cy as u16),
                    cell,
                );
            }
        }
    }

    /// Compute the Braille character for a 2×4 sub-pixel block.
    fn braille_cell(&self, px_x: i32, px_y: i32) -> (char, Option<PackedRgba>, Option<PackedRgba>) {
        // Braille dot numbering to bit mapping:
        // dot 1 (0,0) = bit 0    dot 4 (1,0) = bit 3
        // dot 2 (0,1) = bit 1    dot 5 (1,1) = bit 4
        // dot 3 (0,2) = bit 2    dot 6 (1,2) = bit 5
        // dot 7 (0,3) = bit 6    dot 8 (1,3) = bit 7
        const DOT_BITS: [[u8; 4]; 2] = [
            [0, 1, 2, 6], // column 0: dots 1,2,3,7
            [3, 4, 5, 7], // column 1: dots 4,5,6,8
        ];

        let mut bits: u8 = 0;
        let mut first_color: Option<PackedRgba> = None;
        let full_coverage = self.is_full_coverage_current();

        if full_coverage
            && px_x >= 0
            && px_y >= 0
            && px_x + 1 < self.width_i32
            && px_y + 3 < self.height_i32
        {
            let width = self.width_usize;
            let base = px_y as usize * width + px_x as usize;
            'scan_colors: for col in 0..2 {
                for row in 0..4 {
                    let idx = base + row * width + col;
                    if let Some(c) = self.colors[idx] {
                        first_color = Some(c);
                        break 'scan_colors;
                    }
                }
            }
            return ('\u{28FF}', first_color, None);
        }

        if full_coverage {
            // Full-coverage edge path: any in-bounds subpixel is on.
            for (col, col_bits) in DOT_BITS.iter().enumerate() {
                for (row, bit) in col_bits.iter().enumerate() {
                    let x = px_x + col as i32;
                    let y = px_y + row as i32;
                    if let Some(idx) = self.index(x, y) {
                        bits |= 1 << *bit;
                        if first_color.is_none() {
                            first_color = self.colors[idx];
                        }
                    }
                }
            }
        } else {
            // Fast path: avoid per-subpixel bounds checks when the full 2x4 block is in-bounds.
            // This matters for dense canvases (e.g., VFX plasma) where we sample every subpixel.
            if px_x >= 0 && px_y >= 0 && px_x + 1 < self.width_i32 && px_y + 3 < self.height_i32 {
                let width = self.width_usize;
                let base = px_y as usize * width + px_x as usize;
                for (col, col_bits) in DOT_BITS.iter().enumerate() {
                    for (row, bit) in col_bits.iter().enumerate() {
                        let idx = base + row * width + col;
                        if self.pixels[idx] == self.generation {
                            bits |= 1 << *bit;
                            if first_color.is_none() {
                                first_color = self.colors[idx];
                            }
                        }
                    }
                }
            } else {
                // Slow path: partial cells at edges and any out-of-bounds blocks.
                for (col, col_bits) in DOT_BITS.iter().enumerate() {
                    for (row, bit) in col_bits.iter().enumerate() {
                        let x = px_x + col as i32;
                        let y = px_y + row as i32;
                        if let Some(idx) = self.index(x, y)
                            && self.pixels[idx] == self.generation
                        {
                            bits |= 1 << *bit;
                            if first_color.is_none() {
                                first_color = self.colors[idx];
                            }
                        }
                    }
                }
            }
        }

        if bits == 0 {
            (' ', None, None)
        } else {
            // Braille patterns start at U+2800
            let ch = char::from_u32(0x2800 + bits as u32).unwrap_or(' ');
            (ch, first_color, None)
        }
    }

    /// Compute the block character for a 2×2 sub-pixel block.
    fn block_cell(&self, px_x: i32, px_y: i32) -> (char, Option<PackedRgba>, Option<PackedRgba>) {
        let tl = self.get(px_x, px_y);
        let tr = self.get(px_x + 1, px_y);
        let bl = self.get(px_x, px_y + 1);
        let br = self.get(px_x + 1, px_y + 1);

        let first_color = self.first_set_color(&[
            (px_x, px_y),
            (px_x + 1, px_y),
            (px_x, px_y + 1),
            (px_x + 1, px_y + 1),
        ]);

        let ch = match (tl, tr, bl, br) {
            (false, false, false, false) => ' ',
            (true, false, false, false) => '▘',
            (false, true, false, false) => '▝',
            (true, true, false, false) => '▀',
            (false, false, true, false) => '▖',
            (true, false, true, false) => '▌',
            (false, true, true, false) => '▞',
            (true, true, true, false) => '▛',
            (false, false, false, true) => '▗',
            (true, false, false, true) => '▚',
            (false, true, false, true) => '▐',
            (true, true, false, true) => '▜',
            (false, false, true, true) => '▄',
            (true, false, true, true) => '▙',
            (false, true, true, true) => '▟',
            (true, true, true, true) => '█',
        };

        (ch, first_color, None)
    }

    /// Compute the half-block character for a 1×2 sub-pixel block.
    fn halfblock_cell(
        &self,
        px_x: i32,
        px_y: i32,
    ) -> (char, Option<PackedRgba>, Option<PackedRgba>) {
        let top = self.get(px_x, px_y);
        let bot = self.get(px_x, px_y + 1);
        let top_color = self.color_at(px_x, px_y);
        let bot_color = self.color_at(px_x, px_y + 1);

        match (top, bot) {
            (false, false) => (' ', None, None),
            (true, false) => ('▀', top_color, None),
            (false, true) => ('▄', bot_color, None),
            (true, true) => ('▀', top_color, bot_color),
        }
    }

    fn first_set_color(&self, coords: &[(i32, i32)]) -> Option<PackedRgba> {
        for &(x, y) in coords {
            if self.get(x, y)
                && let Some(idx) = self.index(x, y)
                && let Some(c) = self.colors[idx]
            {
                return Some(c);
            }
        }
        None
    }

    fn color_at(&self, x: i32, y: i32) -> Option<PackedRgba> {
        if self.get(x, y)
            && let Some(idx) = self.index(x, y)
        {
            return self.colors[idx];
        }
        None
    }
}

fn point_in_convex_polygon(x: i32, y: i32, points: &[(i32, i32)]) -> bool {
    let mut sign: i32 = 0;
    let len = points.len();
    for i in 0..len {
        let (x0, y0) = points[i];
        let (x1, y1) = points[(i + 1) % len];
        let cross = (x - x0) * (y1 - y0) - (y - y0) * (x1 - x0);
        if cross == 0 {
            continue;
        }
        let s = cross.signum();
        if sign == 0 {
            sign = s;
        } else if sign != s {
            return false;
        }
    }
    true
}

/// Canvas widget that renders a [`Painter`]'s pixel buffer.
#[derive(Debug, Clone)]
pub struct Canvas {
    painter: Painter,
    style: Style,
}

/// Canvas widget that renders a borrowed [`Painter`] without cloning.
#[derive(Debug, Clone)]
pub struct CanvasRef<'a> {
    painter: &'a Painter,
    style: Style,
}

impl Canvas {
    /// Create a canvas from a painter.
    pub fn from_painter(painter: &Painter) -> Self {
        Self {
            painter: painter.clone(),
            style: Style::new(),
        }
    }

    /// Create a canvas that borrows a painter (no allocations).
    pub fn from_painter_ref(painter: &Painter) -> CanvasRef<'_> {
        CanvasRef {
            painter,
            style: Style::new(),
        }
    }

    /// Set the base style (foreground color for lit pixels, background for unlit).
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
}

impl<'a> CanvasRef<'a> {
    /// Create a canvas reference from a painter.
    pub fn from_painter(painter: &'a Painter) -> Self {
        Self {
            painter,
            style: Style::new(),
        }
    }

    /// Set the base style (foreground color for lit pixels, background for unlit).
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
}

impl Widget for Canvas {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }
        self.painter
            .render_to_buffer(area, &mut frame.buffer, self.style);
    }
}

impl Widget for CanvasRef<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }
        self.painter
            .render_to_buffer(area, &mut frame.buffer, self.style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn mode_dimensions() {
        assert_eq!(Mode::Braille.cols_per_cell(), 2);
        assert_eq!(Mode::Braille.rows_per_cell(), 4);
        assert_eq!(Mode::Block.cols_per_cell(), 2);
        assert_eq!(Mode::Block.rows_per_cell(), 2);
        assert_eq!(Mode::HalfBlock.cols_per_cell(), 1);
        assert_eq!(Mode::HalfBlock.rows_per_cell(), 2);
    }

    #[test]
    fn painter_point_and_get() {
        let mut p = Painter::new(10, 10, Mode::Braille);
        assert!(!p.get(5, 5));
        p.point(5, 5);
        assert!(p.get(5, 5));
    }

    #[test]
    fn painter_out_of_bounds() {
        let mut p = Painter::new(10, 10, Mode::Braille);
        p.point(-1, 0);
        p.point(0, -1);
        p.point(10, 0);
        p.point(0, 10);
        assert!(!p.get(-1, 0));
        assert!(!p.get(10, 0));
    }

    #[test]
    fn bresenham_horizontal() {
        let mut p = Painter::new(10, 5, Mode::Braille);
        p.line(0, 2, 9, 2);
        for x in 0..10 {
            assert!(p.get(x, 2), "pixel ({x}, 2) should be set");
        }
    }

    #[test]
    fn bresenham_vertical() {
        let mut p = Painter::new(5, 10, Mode::Braille);
        p.line(2, 0, 2, 9);
        for y in 0..10 {
            assert!(p.get(2, y), "pixel (2, {y}) should be set");
        }
    }

    #[test]
    fn bresenham_diagonal() {
        let mut p = Painter::new(10, 10, Mode::Braille);
        p.line(0, 0, 9, 9);
        for i in 0..10 {
            assert!(p.get(i, i), "pixel ({i}, {i}) should be set");
        }
    }

    #[test]
    fn bresenham_reversed() {
        let mut p = Painter::new(10, 10, Mode::Braille);
        p.line(9, 9, 0, 0);
        for i in 0..10 {
            assert!(p.get(i, i), "pixel ({i}, {i}) should be set");
        }
    }

    #[test]
    fn bresenham_single_point() {
        let mut p = Painter::new(10, 10, Mode::Braille);
        p.line(5, 5, 5, 5);
        assert!(p.get(5, 5));
    }

    #[test]
    fn rect_outline() {
        let mut p = Painter::new(10, 10, Mode::Braille);
        p.rect(2, 2, 4, 3);
        // Top edge
        for x in 2..6 {
            assert!(p.get(x, 2), "top ({x}, 2)");
        }
        // Bottom edge
        for x in 2..6 {
            assert!(p.get(x, 4), "bottom ({x}, 4)");
        }
        // Left edge
        for y in 2..5 {
            assert!(p.get(2, y), "left (2, {y})");
        }
        // Right edge
        for y in 2..5 {
            assert!(p.get(5, y), "right (5, {y})");
        }
        // Interior should be empty
        assert!(!p.get(3, 3));
        assert!(!p.get(4, 3));
    }

    #[test]
    fn rect_filled() {
        let mut p = Painter::new(10, 10, Mode::Braille);
        p.rect_filled(1, 1, 3, 3);
        for y in 1..4 {
            for x in 1..4 {
                assert!(p.get(x, y), "({x}, {y}) should be filled");
            }
        }
        assert!(!p.get(0, 0));
        assert!(!p.get(4, 4));
    }

    #[test]
    fn circle_basic() {
        let mut p = Painter::new(20, 20, Mode::Braille);
        p.circle(10, 10, 5);
        assert!(p.get(15, 10)); // rightmost
        assert!(p.get(5, 10)); // leftmost
        assert!(p.get(10, 5)); // topmost
        assert!(p.get(10, 15)); // bottommost
        assert!(!p.get(10, 10)); // center empty (outline only)
    }

    #[test]
    fn circle_zero_radius() {
        let mut p = Painter::new(10, 10, Mode::Braille);
        p.circle(5, 5, 0);
        assert!(p.get(5, 5));
    }

    #[test]
    fn braille_empty_cell() {
        let p = Painter::new(2, 4, Mode::Braille);
        let (ch, _, _) = p.braille_cell(0, 0);
        assert_eq!(ch, ' ');
    }

    #[test]
    fn braille_single_dot() {
        let mut p = Painter::new(2, 4, Mode::Braille);
        p.point(0, 0); // dot 1 = bit 0
        let (ch, _, _) = p.braille_cell(0, 0);
        assert_eq!(ch, '\u{2801}');
    }

    #[test]
    fn braille_all_dots() {
        let mut p = Painter::new(2, 4, Mode::Braille);
        for y in 0..4 {
            for x in 0..2 {
                p.point(x, y);
            }
        }
        let (ch, _, _) = p.braille_cell(0, 0);
        assert_eq!(ch, '\u{28FF}');
    }

    #[test]
    fn full_coverage_marks_pixels_on() {
        let mut p = Painter::new(2, 4, Mode::Braille);
        p.mark_full_coverage();

        let (ch, _, _) = p.braille_cell(0, 0);
        assert_eq!(ch, '\u{28FF}');
        assert!(p.get(0, 0));
        assert!(p.get(1, 3));
        assert!(!p.get(2, 0));
    }

    #[test]
    fn full_coverage_partial_braille_cell_respects_bounds() {
        let mut p = Painter::new(1, 1, Mode::Braille);
        p.mark_full_coverage();

        let (ch, _, _) = p.braille_cell(0, 0);
        // Only (0,0) exists in-bounds, so only dot 1 should be set.
        assert_eq!(ch, '\u{2801}');
    }

    #[test]
    fn block_quadrants() {
        let mut p = Painter::new(2, 2, Mode::Block);
        p.point(0, 0);
        assert_eq!(p.block_cell(0, 0).0, '▘');

        p.point(1, 0);
        assert_eq!(p.block_cell(0, 0).0, '▀');

        p.point(0, 1);
        p.point(1, 1);
        assert_eq!(p.block_cell(0, 0).0, '█');
    }

    #[test]
    fn halfblock_combinations() {
        let mut p = Painter::new(1, 2, Mode::HalfBlock);
        assert_eq!(p.halfblock_cell(0, 0).0, ' ');

        p.point(0, 0);
        assert_eq!(p.halfblock_cell(0, 0).0, '▀');

        p.clear();
        p.point(0, 1);
        assert_eq!(p.halfblock_cell(0, 0).0, '▄');

        p.point(0, 0);
        assert_eq!(p.halfblock_cell(0, 0).0, '▀');
    }

    #[test]
    fn canvas_renders_to_buffer() {
        let mut painter = Painter::new(4, 8, Mode::Braille);
        for y in 0..4 {
            for x in 0..2 {
                painter.point(x, y);
            }
        }

        let canvas = Canvas::from_painter(&painter);
        let area = Rect::new(0, 0, 2, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 2, &mut pool);
        canvas.render(area, &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('\u{28FF}'));
    }

    #[test]
    fn canvas_empty_area_noop() {
        let painter = Painter::new(4, 8, Mode::Braille);
        let canvas = Canvas::from_painter(&painter);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        canvas.render(area, &mut frame);
    }

    #[test]
    fn painter_for_area() {
        let area = Rect::new(0, 0, 10, 5);
        let p = Painter::for_area(area, Mode::Braille);
        assert_eq!(p.size(), (20, 20));
        assert_eq!(p.cell_size(), (10, 5));
    }

    #[test]
    fn colored_point() {
        let mut p = Painter::new(2, 4, Mode::Braille);
        let red = PackedRgba::rgb(255, 0, 0);
        p.point_colored(0, 0, red);
        assert!(p.get(0, 0));
        let (_, color, _) = p.braille_cell(0, 0);
        assert_eq!(color, Some(red));
    }

    #[test]
    fn colored_point_in_bounds() {
        let mut p = Painter::new(2, 4, Mode::Braille);
        let blue = PackedRgba::rgb(0, 0, 255);
        p.point_colored_in_bounds(1, 3, blue);
        assert!(p.get(1, 3));
        let idx = p.index(1, 3).expect("index should exist");
        assert_eq!(p.colors[idx], Some(blue));
    }

    #[test]
    #[should_panic(expected = "assertion failed")]
    fn colored_point_in_bounds_panics_on_out_of_bounds() {
        let mut p = Painter::new(2, 4, Mode::Braille);
        let blue = PackedRgba::rgb(0, 0, 255);

        // This helper assumes callers already validated bounds.
        p.point_colored_in_bounds(2, 0, blue);
    }

    #[test]
    fn colored_line() {
        let mut p = Painter::new(10, 1, Mode::Braille);
        let blue = PackedRgba::rgb(0, 0, 255);
        p.line_colored(0, 0, 4, 0, Some(blue));
        assert!(p.get(0, 0));
        assert!(p.get(4, 0));
        if let Some(idx) = p.index(2, 0) {
            assert_eq!(p.colors[idx], Some(blue));
        }
    }

    #[test]
    fn clear_resets_all() {
        let mut p = Painter::new(10, 10, Mode::Braille);
        p.point_colored(5, 5, PackedRgba::rgb(255, 0, 0));
        p.line(0, 0, 9, 9);
        p.clear();
        for y in 0..10 {
            for x in 0..10 {
                assert!(!p.get(x, y));
            }
        }
    }

    #[test]
    fn clear_resets_full_coverage() {
        let mut p = Painter::new(2, 4, Mode::Braille);
        p.mark_full_coverage();
        assert!(p.get(1, 3));

        p.clear();

        assert!(!p.get(0, 0));
        let (ch, _, _) = p.braille_cell(0, 0);
        assert_eq!(ch, ' ');
    }

    #[test]
    fn cell_size_rounds_up() {
        let p = Painter::new(3, 5, Mode::Braille);
        assert_eq!(p.cell_size(), (2, 2));
    }

    // -----------------------------------------------------------------------
    // Metaball Canvas Adapter Tests (visual-fx feature)
    // -----------------------------------------------------------------------

    #[cfg(feature = "visual-fx")]
    mod metaball_adapter_tests {
        use super::*;
        use crate::visual_fx::FxQuality;
        use crate::visual_fx::effects::sampling::{BallState, MetaballFieldSampler};

        #[test]
        fn metaball_field_renders_pixels() {
            let balls = vec![BallState {
                x: 0.5,
                y: 0.5,
                r2: 0.04, // radius^2 = 0.04 -> radius = 0.2
                hue: 0.5,
            }];
            let sampler = MetaballFieldSampler::new(balls);

            let mut painter = Painter::new(20, 20, Mode::Braille);
            painter.render_metaball_field(
                &sampler,
                1.0,
                0.1,
                FxQuality::Full,
                |_hue, intensity| {
                    let c = (intensity * 255.0) as u8;
                    PackedRgba::rgb(c, c, c)
                },
            );

            // Center pixel should be set (ball is at center)
            assert!(painter.get(10, 10), "center pixel should be set");

            // Corners should be unset (too far from ball)
            assert!(!painter.get(0, 0), "corner should be unset");
            assert!(!painter.get(19, 19), "opposite corner should be unset");
        }

        #[test]
        fn metaball_field_respects_quality_off() {
            let balls = vec![BallState {
                x: 0.5,
                y: 0.5,
                r2: 0.25,
                hue: 0.0,
            }];
            let sampler = MetaballFieldSampler::new(balls);

            let mut painter = Painter::new(10, 10, Mode::Braille);
            painter
                .render_metaball_field(&sampler, 1.0, 0.0, FxQuality::Off, |_, _| PackedRgba::RED);

            // Nothing should be set when quality is Off
            for y in 0..10 {
                for x in 0..10 {
                    assert!(!painter.get(x, y), "no pixels should be set when off");
                }
            }
        }

        #[test]
        fn metaball_field_empty_painter_is_safe() {
            let sampler = MetaballFieldSampler::new(vec![]);

            let mut painter = Painter::new(0, 0, Mode::Braille);
            painter
                .render_metaball_field(&sampler, 1.0, 0.5, FxQuality::Full, |_, _| PackedRgba::RED);
            // Should not panic
        }

        #[test]
        fn metaball_field_colors_are_applied() {
            let balls = vec![BallState {
                x: 0.5,
                y: 0.5,
                r2: 0.25, // Large ball covering most of canvas
                hue: 0.3,
            }];
            let sampler = MetaballFieldSampler::new(balls);

            let expected_color = PackedRgba::rgb(255, 0, 0);
            let mut painter = Painter::new(4, 4, Mode::Braille);
            painter.render_metaball_field(
                &sampler,
                0.1, // Low threshold - most pixels should be set
                0.0,
                FxQuality::Full,
                |_, _| expected_color,
            );

            // At least the center should have color
            if let Some(idx) = painter.index(2, 2) {
                assert!(
                    painter.pixels[idx] == painter.generation,
                    "center should be set"
                );
                assert_eq!(
                    painter.colors[idx],
                    Some(expected_color),
                    "color should match"
                );
            }
        }

        #[test]
        fn metaball_field_multiple_balls() {
            let balls = vec![
                BallState {
                    x: 0.25,
                    y: 0.5,
                    r2: 0.02,
                    hue: 0.0,
                },
                BallState {
                    x: 0.75,
                    y: 0.5,
                    r2: 0.02,
                    hue: 0.5,
                },
            ];
            let sampler = MetaballFieldSampler::new(balls);

            let mut painter = Painter::new(20, 10, Mode::Braille);
            painter.render_metaball_field(&sampler, 0.5, 0.1, FxQuality::Full, |hue, _| {
                // Use hue to distinguish balls
                if hue < 0.25 {
                    PackedRgba::RED
                } else {
                    PackedRgba::BLUE
                }
            });

            // Left side should have pixels (near first ball)
            assert!(painter.get(5, 5), "left side should have pixels");
            // Right side should have pixels (near second ball)
            assert!(painter.get(15, 5), "right side should have pixels");
        }

        #[test]
        fn metaball_field_threshold_controls_visibility() {
            let balls = vec![BallState {
                x: 0.5,
                y: 0.5,
                r2: 0.01, // Small ball
                hue: 0.0,
            }];
            let sampler = MetaballFieldSampler::new(balls);

            // High threshold - fewer pixels
            let mut high_thresh = Painter::new(20, 20, Mode::Braille);
            high_thresh.render_metaball_field(
                &sampler,
                10.0, // Very high threshold
                5.0,
                FxQuality::Full,
                |_, _| PackedRgba::RED,
            );

            // Low threshold - more pixels
            let mut low_thresh = Painter::new(20, 20, Mode::Braille);
            low_thresh.render_metaball_field(
                &sampler,
                0.1, // Low threshold
                0.0,
                FxQuality::Full,
                |_, _| PackedRgba::RED,
            );

            let high_count: usize = (0..20)
                .flat_map(|y| (0..20).map(move |x| (x, y)))
                .filter(|&(x, y)| high_thresh.get(x, y))
                .count();

            let low_count: usize = (0..20)
                .flat_map(|y| (0..20).map(move |x| (x, y)))
                .filter(|&(x, y)| low_thresh.get(x, y))
                .count();

            assert!(
                low_count >= high_count,
                "lower threshold should render more pixels: low={low_count} high={high_count}"
            );
        }
    }
}

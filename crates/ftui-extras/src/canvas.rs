#![forbid(unsafe_code)]

//! Canvas widget for arbitrary pixel/shape drawing using Braille, block,
//! or half-block characters.
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
    /// Resolution mode.
    mode: Mode,
    /// Pixel buffer (row-major, `true` = on).
    pixels: Vec<bool>,
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
            mode,
            pixels: vec![false; len],
            colors: vec![None; len],
        }
    }

    /// Create a painter sized to fill a terminal area.
    pub fn for_area(area: Rect, mode: Mode) -> Self {
        let width = area.width * mode.cols_per_cell();
        let height = area.height * mode.rows_per_cell();
        Self::new(width, height, mode)
    }

    /// Clear all pixels.
    pub fn clear(&mut self) {
        self.pixels.fill(false);
        self.colors.fill(None);
    }

    /// Set a single pixel.
    pub fn point(&mut self, x: i32, y: i32) {
        if let Some(idx) = self.index(x, y) {
            self.pixels[idx] = true;
        }
    }

    /// Set a single pixel with color.
    pub fn point_colored(&mut self, x: i32, y: i32, color: PackedRgba) {
        if let Some(idx) = self.index(x, y) {
            self.pixels[idx] = true;
            self.colors[idx] = Some(color);
        }
    }

    /// Draw a line from (x0, y0) to (x1, y1) using Bresenham's algorithm.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.line_colored(x0, y0, x1, y1, None);
    }

    /// Draw a colored line.
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
    pub fn rect_filled(&mut self, x: i32, y: i32, w: i32, h: i32) {
        for dy in 0..h {
            for dx in 0..w {
                self.point(x + dx, y + dy);
            }
        }
    }

    /// Draw a circle outline using the midpoint algorithm.
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

    /// Get the sub-pixel dimensions.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// Get the terminal cell dimensions needed to display this painter.
    pub fn cell_size(&self) -> (u16, u16) {
        let cols = self.mode.cols_per_cell();
        let rows = self.mode.rows_per_cell();
        (self.width.div_ceil(cols), self.height.div_ceil(rows))
    }

    /// Check if a pixel is set.
    pub fn get(&self, x: i32, y: i32) -> bool {
        self.index(x, y).map(|i| self.pixels[i]).unwrap_or(false)
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        Some(y as usize * self.width as usize + x as usize)
    }

    /// Render this painter's pixels into a cell grid.
    fn render_to_buffer(&self, area: Rect, buf: &mut Buffer, style: Style) {
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

                let (ch, color) = match self.mode {
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
                if let Some(c) = color {
                    cell.fg = c;
                }

                buf.set(area.x + cx as u16, area.y + cy as u16, cell);
            }
        }
    }

    /// Compute the Braille character for a 2×4 sub-pixel block.
    fn braille_cell(&self, px_x: i32, px_y: i32) -> (char, Option<PackedRgba>) {
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

        for col in 0..2 {
            for row in 0..4 {
                let x = px_x + col;
                let y = px_y + row;
                if self.get(x, y) {
                    bits |= 1 << DOT_BITS[col as usize][row as usize];
                    if first_color.is_none()
                        && let Some(idx) = self.index(x, y)
                    {
                        first_color = self.colors[idx];
                    }
                }
            }
        }

        if bits == 0 {
            (' ', None)
        } else {
            // Braille patterns start at U+2800
            let ch = char::from_u32(0x2800 + bits as u32).unwrap_or(' ');
            (ch, first_color)
        }
    }

    /// Compute the block character for a 2×2 sub-pixel block.
    fn block_cell(&self, px_x: i32, px_y: i32) -> (char, Option<PackedRgba>) {
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

        (ch, first_color)
    }

    /// Compute the half-block character for a 1×2 sub-pixel block.
    fn halfblock_cell(&self, px_x: i32, px_y: i32) -> (char, Option<PackedRgba>) {
        let top = self.get(px_x, px_y);
        let bot = self.get(px_x, px_y + 1);

        let first_color = self.first_set_color(&[(px_x, px_y), (px_x, px_y + 1)]);

        let ch = match (top, bot) {
            (false, false) => ' ',
            (true, false) => '▀',
            (false, true) => '▄',
            (true, true) => '█',
        };

        (ch, first_color)
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
}

/// Canvas widget that renders a [`Painter`]'s pixel buffer.
#[derive(Debug, Clone)]
pub struct Canvas {
    painter: Painter,
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
        let (ch, _) = p.braille_cell(0, 0);
        assert_eq!(ch, ' ');
    }

    #[test]
    fn braille_single_dot() {
        let mut p = Painter::new(2, 4, Mode::Braille);
        p.point(0, 0); // dot 1 = bit 0
        let (ch, _) = p.braille_cell(0, 0);
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
        let (ch, _) = p.braille_cell(0, 0);
        assert_eq!(ch, '\u{28FF}');
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
        assert_eq!(p.halfblock_cell(0, 0).0, '█');
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
        let (_, color) = p.braille_cell(0, 0);
        assert_eq!(color, Some(red));
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
    fn cell_size_rounds_up() {
        let p = Painter::new(3, 5, Mode::Braille);
        assert_eq!(p.cell_size(), (2, 2));
    }
}

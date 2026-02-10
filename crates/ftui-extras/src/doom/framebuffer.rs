//! Internal RGBA framebuffer for the Doom renderer.
//!
//! Renders to a fixed-size pixel buffer, then blits to a Painter for terminal output.

use ftui_render::cell::PackedRgba;

use crate::canvas::Painter;

/// RGBA framebuffer for intermediate rendering.
#[derive(Debug, Clone)]
pub struct DoomFramebuffer {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA pixels.
    pub pixels: Vec<PackedRgba>,
}

impl DoomFramebuffer {
    /// Create a new framebuffer with the given dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        let size = (width * height) as usize;
        Self {
            width,
            height,
            pixels: vec![PackedRgba::BLACK; size],
        }
    }

    /// Clear the framebuffer to black.
    pub fn clear(&mut self) {
        self.pixels.fill(PackedRgba::BLACK);
    }

    /// Set a pixel at (x, y) to the given color.
    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, color: PackedRgba) {
        if x < self.width && y < self.height {
            self.pixels[(y * self.width + x) as usize] = color;
        }
    }

    /// Get a pixel at (x, y).
    #[inline]
    pub fn get_pixel(&self, x: u32, y: u32) -> PackedRgba {
        if x < self.width && y < self.height {
            self.pixels[(y * self.width + x) as usize]
        } else {
            PackedRgba::BLACK
        }
    }

    /// Draw a vertical column of a single color from y_top to y_bottom.
    #[inline]
    pub fn draw_column(&mut self, x: u32, y_top: u32, y_bottom: u32, color: PackedRgba) {
        if x >= self.width {
            return;
        }
        let top = y_top.min(self.height);
        let bottom = y_bottom.min(self.height);
        let stride = self.width as usize;
        let mut idx = top as usize * stride + x as usize;
        for _ in top..bottom {
            self.pixels[idx] = color;
            idx += stride;
        }
    }

    /// Draw a vertical column with per-row color variation (for lighting gradient).
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn draw_column_shaded(
        &mut self,
        x: u32,
        y_top: u32,
        y_bottom: u32,
        base_r: u8,
        base_g: u8,
        base_b: u8,
        light_top: f32,
        light_bottom: f32,
    ) {
        if x >= self.width {
            return;
        }
        let top = y_top.min(self.height);
        let bottom = y_bottom.min(self.height);
        let height = bottom.saturating_sub(top);
        if height == 0 {
            return;
        }
        let inv_height = 1.0 / height as f32;
        let light_delta = light_bottom - light_top;
        let base_r_f = base_r as f32;
        let base_g_f = base_g as f32;
        let base_b_f = base_b as f32;
        let stride = self.width as usize;
        let mut idx = top as usize * stride + x as usize;
        for y in top..bottom {
            let light = light_top + light_delta * ((y - top) as f32 * inv_height);
            let r = (base_r_f * light).min(255.0) as u8;
            let g = (base_g_f * light).min(255.0) as u8;
            let b = (base_b_f * light).min(255.0) as u8;
            self.pixels[idx] = PackedRgba::rgb(r, g, b);
            idx += stride;
        }
    }

    /// Blit the framebuffer to a Painter, scaling to fit the painter's dimensions.
    pub fn blit_to_painter(&self, painter: &mut Painter, stride: usize) {
        let (pw, ph) = painter.size();
        let pw = pw as u32;
        let ph = ph as u32;

        if pw == 0 || ph == 0 || self.width == 0 || self.height == 0 {
            return;
        }

        let stride = stride.max(1) as u32;
        let pw_usize = pw as usize;
        let fb_width = self.width as usize;

        if stride == 1 {
            // Every pixel will be written — skip per-pixel generation stamps.
            painter.mark_full_coverage();
            for py in 0..ph {
                let fb_y = (py * self.height) / ph;
                let fb_row_start = fb_y as usize * fb_width;
                let painter_row_start = py as usize * pw_usize;
                for px in 0..pw {
                    let fb_x = ((px * self.width) / pw) as usize;
                    let color = self.pixels[fb_row_start + fb_x];
                    let painter_idx = painter_row_start + px as usize;
                    painter.set_color_at_index_in_bounds(painter_idx, color);
                }
            }
        } else {
            for py in (0..ph).step_by(stride as usize) {
                let fb_y = (py * self.height) / ph;
                let fb_row_start = fb_y as usize * fb_width;
                let painter_row_start = py as usize * pw_usize;
                for px in (0..pw).step_by(stride as usize) {
                    let fb_x = ((px * self.width) / pw) as usize;
                    let color = self.pixels[fb_row_start + fb_x];
                    let painter_idx = painter_row_start + px as usize;
                    painter.point_colored_at_index_in_bounds(painter_idx, color);
                }
            }
        }
    }

    /// Resize the framebuffer, clearing contents.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels
            .resize((width * height) as usize, PackedRgba::BLACK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Mode;
    use ftui_core::geometry::Rect;
    use ftui_render::buffer::Buffer;
    use ftui_style::Style;

    #[test]
    fn new_framebuffer_is_black() {
        let fb = DoomFramebuffer::new(10, 10);
        assert_eq!(fb.pixels.len(), 100);
        for p in &fb.pixels {
            assert_eq!(*p, PackedRgba::BLACK);
        }
    }

    #[test]
    fn set_get_pixel() {
        let mut fb = DoomFramebuffer::new(10, 10);
        fb.set_pixel(5, 5, PackedRgba::RED);
        assert_eq!(fb.get_pixel(5, 5), PackedRgba::RED);
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::BLACK);
    }

    #[test]
    fn out_of_bounds_is_safe() {
        let mut fb = DoomFramebuffer::new(10, 10);
        fb.set_pixel(100, 100, PackedRgba::RED); // Should not panic
        assert_eq!(fb.get_pixel(100, 100), PackedRgba::BLACK);
    }

    #[test]
    fn draw_column() {
        let mut fb = DoomFramebuffer::new(10, 10);
        fb.draw_column(5, 2, 8, PackedRgba::GREEN);
        assert_eq!(fb.get_pixel(5, 0), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(5, 2), PackedRgba::GREEN);
        assert_eq!(fb.get_pixel(5, 7), PackedRgba::GREEN);
        assert_eq!(fb.get_pixel(5, 8), PackedRgba::BLACK);
    }

    #[test]
    fn draw_column_out_of_bounds_x_is_safe() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column(10, 0, 5, PackedRgba::RED);
        // Should not panic
    }

    #[test]
    fn draw_column_shaded_gradient() {
        let mut fb = DoomFramebuffer::new(10, 10);
        fb.draw_column_shaded(0, 0, 4, 100, 100, 100, 1.0, 0.0);
        // Top pixel should be brighter than bottom pixel
        let top = fb.get_pixel(0, 0);
        let bot = fb.get_pixel(0, 3);
        assert!(top.r() >= bot.r(), "top should be brighter than bottom");
    }

    #[test]
    fn draw_column_shaded_zero_height_is_safe() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column_shaded(0, 3, 3, 100, 100, 100, 1.0, 1.0);
        // Should not panic with zero-height column
    }

    #[test]
    fn draw_column_shaded_clamps_overflow() {
        let mut fb = DoomFramebuffer::new(5, 5);
        // light_top = 2.0 with base_r = 200 would produce 400.0, must clamp to 255
        fb.draw_column_shaded(0, 0, 1, 200, 200, 200, 2.0, 2.0);
        let pixel = fb.get_pixel(0, 0);
        assert_eq!(pixel.r(), 255);
        assert_eq!(pixel.g(), 255);
        assert_eq!(pixel.b(), 255);
    }

    #[test]
    fn draw_column_shaded_out_of_bounds_x_is_safe() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column_shaded(10, 0, 5, 100, 100, 100, 1.0, 0.5);
        // Should not panic
    }

    #[test]
    fn clear_resets_to_black() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.set_pixel(0, 0, PackedRgba::RED);
        fb.set_pixel(4, 4, PackedRgba::GREEN);
        fb.clear();
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(4, 4), PackedRgba::BLACK);
    }

    #[test]
    fn resize_changes_dimensions() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.set_pixel(2, 2, PackedRgba::RED);
        fb.resize(10, 10);
        assert_eq!(fb.width, 10);
        assert_eq!(fb.height, 10);
        assert_eq!(fb.pixels.len(), 100);
    }

    #[test]
    fn draw_column_shaded_uniform_light() {
        let mut fb = DoomFramebuffer::new(10, 10);
        // With uniform light, all pixels in column should be identical
        fb.draw_column_shaded(3, 1, 5, 100, 150, 200, 0.5, 0.5);
        let expected = PackedRgba::rgb(50, 75, 100);
        for y in 1..5 {
            assert_eq!(fb.get_pixel(3, y), expected, "uniform light at y={y}");
        }
        // Pixels outside range should be black
        assert_eq!(fb.get_pixel(3, 0), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(3, 5), PackedRgba::BLACK);
    }

    // --- new() edge cases ---

    #[test]
    fn new_zero_width() {
        let fb = DoomFramebuffer::new(0, 10);
        assert_eq!(fb.pixels.len(), 0);
        assert_eq!(fb.width, 0);
        assert_eq!(fb.height, 10);
    }

    #[test]
    fn new_zero_height() {
        let fb = DoomFramebuffer::new(10, 0);
        assert_eq!(fb.pixels.len(), 0);
    }

    #[test]
    fn new_1x1() {
        let fb = DoomFramebuffer::new(1, 1);
        assert_eq!(fb.pixels.len(), 1);
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::BLACK);
    }

    // --- set_pixel/get_pixel boundary cases ---

    #[test]
    fn set_get_pixel_corners() {
        let mut fb = DoomFramebuffer::new(8, 6);
        let colors = [
            PackedRgba::RED,
            PackedRgba::GREEN,
            PackedRgba::BLUE,
            PackedRgba::WHITE,
        ];
        let corners = [(0, 0), (7, 0), (0, 5), (7, 5)];
        for (i, &(x, y)) in corners.iter().enumerate() {
            fb.set_pixel(x, y, colors[i]);
        }
        for (i, &(x, y)) in corners.iter().enumerate() {
            assert_eq!(fb.get_pixel(x, y), colors[i], "corner ({x},{y})");
        }
    }

    #[test]
    fn set_pixel_overwrites_previous() {
        let mut fb = DoomFramebuffer::new(4, 4);
        fb.set_pixel(1, 1, PackedRgba::RED);
        fb.set_pixel(1, 1, PackedRgba::GREEN);
        assert_eq!(fb.get_pixel(1, 1), PackedRgba::GREEN);
    }

    #[test]
    fn get_pixel_oob_returns_black_various() {
        let fb = DoomFramebuffer::new(5, 5);
        // Just past each edge
        assert_eq!(fb.get_pixel(5, 0), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(0, 5), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(5, 5), PackedRgba::BLACK);
        // Far OOB
        assert_eq!(fb.get_pixel(u32::MAX, u32::MAX), PackedRgba::BLACK);
    }

    // --- draw_column edge cases ---

    #[test]
    fn draw_column_inverted_range_draws_nothing() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column(2, 4, 1, PackedRgba::RED);
        // y_top > y_bottom → empty range, all should be black
        for y in 0..5 {
            assert_eq!(fb.get_pixel(2, y), PackedRgba::BLACK, "y={y}");
        }
    }

    #[test]
    fn draw_column_clamps_y_to_height() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column(0, 3, 100, PackedRgba::RED);
        // Should draw rows 3..5 (clamped to height)
        assert_eq!(fb.get_pixel(0, 2), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(0, 3), PackedRgba::RED);
        assert_eq!(fb.get_pixel(0, 4), PackedRgba::RED);
    }

    #[test]
    fn draw_column_top_at_height_draws_nothing() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column(2, 5, 9, PackedRgba::RED);
        for y in 0..5 {
            assert_eq!(fb.get_pixel(2, y), PackedRgba::BLACK, "y={y}");
        }
    }

    #[test]
    fn draw_column_full_height() {
        let mut fb = DoomFramebuffer::new(3, 4);
        fb.draw_column(1, 0, 4, PackedRgba::GREEN);
        for y in 0..4 {
            assert_eq!(fb.get_pixel(1, y), PackedRgba::GREEN, "y={y}");
        }
        // Adjacent columns untouched
        for y in 0..4 {
            assert_eq!(fb.get_pixel(0, y), PackedRgba::BLACK, "left col y={y}");
            assert_eq!(fb.get_pixel(2, y), PackedRgba::BLACK, "right col y={y}");
        }
    }

    #[test]
    fn draw_column_at_last_x() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column(4, 0, 3, PackedRgba::BLUE);
        assert_eq!(fb.get_pixel(4, 0), PackedRgba::BLUE);
        assert_eq!(fb.get_pixel(4, 2), PackedRgba::BLUE);
        assert_eq!(fb.get_pixel(4, 3), PackedRgba::BLACK);
    }

    // --- draw_column_shaded edge cases ---

    #[test]
    fn draw_column_shaded_inverted_gradient() {
        let mut fb = DoomFramebuffer::new(10, 10);
        // light goes from 0.0 (dark at top) to 1.0 (bright at bottom)
        fb.draw_column_shaded(0, 0, 4, 200, 200, 200, 0.0, 1.0);
        let top = fb.get_pixel(0, 0);
        let bot = fb.get_pixel(0, 3);
        assert!(
            top.r() <= bot.r(),
            "bottom should be brighter, top.r={} bot.r={}",
            top.r(),
            bot.r()
        );
    }

    #[test]
    fn draw_column_shaded_zero_light() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column_shaded(0, 0, 3, 255, 128, 64, 0.0, 0.0);
        for y in 0..3 {
            let p = fb.get_pixel(0, y);
            assert_eq!(p.r(), 0, "zero light should produce black r at y={y}");
            assert_eq!(p.g(), 0, "zero light should produce black g at y={y}");
            assert_eq!(p.b(), 0, "zero light should produce black b at y={y}");
        }
    }

    #[test]
    fn draw_column_shaded_single_pixel() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column_shaded(2, 2, 3, 100, 100, 100, 0.8, 0.8);
        let p = fb.get_pixel(2, 2);
        assert_eq!(p.r(), 80);
        assert_eq!(p.g(), 80);
        assert_eq!(p.b(), 80);
        // Adjacent rows untouched
        assert_eq!(fb.get_pixel(2, 1), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(2, 3), PackedRgba::BLACK);
    }

    #[test]
    fn draw_column_shaded_y_clamped_to_height() {
        let mut fb = DoomFramebuffer::new(5, 5);
        // y_bottom far exceeds height — should clamp to 5
        fb.draw_column_shaded(0, 3, 100, 200, 200, 200, 1.0, 1.0);
        assert_eq!(fb.get_pixel(0, 3), PackedRgba::rgb(200, 200, 200));
        assert_eq!(fb.get_pixel(0, 4), PackedRgba::rgb(200, 200, 200));
        // No panic from exceeding bounds
    }

    #[test]
    fn draw_column_shaded_inverted_range_draws_nothing() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column_shaded(1, 4, 2, 255, 255, 255, 1.0, 0.0);
        for y in 0..5 {
            assert_eq!(fb.get_pixel(1, y), PackedRgba::BLACK, "y={y}");
        }
    }

    #[test]
    fn draw_column_shaded_negative_light_clamps_to_zero() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column_shaded(0, 0, 2, 200, 150, 100, -1.0, -0.5);
        for y in 0..2 {
            let pixel = fb.get_pixel(0, y);
            assert_eq!(pixel.r(), 0, "r at y={y}");
            assert_eq!(pixel.g(), 0, "g at y={y}");
            assert_eq!(pixel.b(), 0, "b at y={y}");
        }
    }

    // --- resize edge cases ---

    #[test]
    fn resize_to_smaller() {
        let mut fb = DoomFramebuffer::new(10, 10);
        fb.set_pixel(9, 9, PackedRgba::RED);
        fb.resize(3, 3);
        assert_eq!(fb.width, 3);
        assert_eq!(fb.height, 3);
        assert_eq!(fb.pixels.len(), 9);
    }

    #[test]
    fn resize_to_1x1() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.resize(1, 1);
        assert_eq!(fb.pixels.len(), 1);
    }

    #[test]
    fn resize_to_zero() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.resize(0, 0);
        assert_eq!(fb.pixels.len(), 0);
        assert_eq!(fb.width, 0);
        assert_eq!(fb.height, 0);
    }

    #[test]
    fn resize_grow_fills_black() {
        let mut fb = DoomFramebuffer::new(2, 2);
        fb.set_pixel(0, 0, PackedRgba::RED);
        fb.resize(4, 4);
        // New pixels should be black
        assert_eq!(fb.get_pixel(3, 3), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(2, 2), PackedRgba::BLACK);
    }

    // --- clear after operations ---

    #[test]
    fn clear_after_draw_column() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column(0, 0, 5, PackedRgba::RED);
        fb.draw_column_shaded(1, 0, 5, 255, 255, 255, 1.0, 1.0);
        fb.clear();
        for y in 0..5 {
            for x in 0..5 {
                assert_eq!(
                    fb.get_pixel(x, y),
                    PackedRgba::BLACK,
                    "({x},{y}) should be black after clear"
                );
            }
        }
    }

    // --- draw_column_shaded color channel independence ---

    #[test]
    fn draw_column_shaded_independent_channels() {
        let mut fb = DoomFramebuffer::new(5, 5);
        // Only red channel has non-zero base
        fb.draw_column_shaded(0, 0, 1, 200, 0, 0, 1.0, 1.0);
        let p = fb.get_pixel(0, 0);
        assert_eq!(p.r(), 200);
        assert_eq!(p.g(), 0);
        assert_eq!(p.b(), 0);

        // Only green channel
        fb.draw_column_shaded(1, 0, 1, 0, 150, 0, 1.0, 1.0);
        let p = fb.get_pixel(1, 0);
        assert_eq!(p.r(), 0);
        assert_eq!(p.g(), 150);
        assert_eq!(p.b(), 0);

        // Only blue channel
        fb.draw_column_shaded(2, 0, 1, 0, 0, 100, 1.0, 1.0);
        let p = fb.get_pixel(2, 0);
        assert_eq!(p.r(), 0);
        assert_eq!(p.g(), 0);
        assert_eq!(p.b(), 100);
    }

    // --- blit_to_painter ---

    #[test]
    fn blit_to_painter_halfblock_maps_fg_and_bg() {
        let mut fb = DoomFramebuffer::new(1, 2);
        let top = PackedRgba::RED;
        let bottom = PackedRgba::BLUE;
        fb.set_pixel(0, 0, top);
        fb.set_pixel(0, 1, bottom);

        let mut painter = Painter::new(1, 2, Mode::HalfBlock);
        fb.blit_to_painter(&mut painter, 1);

        let mut buf = Buffer::new(1, 1);
        painter.render_to_buffer(Rect::new(0, 0, 1, 1), &mut buf, Style::default());

        let cell = buf.get(0, 0).expect("rendered cell");
        assert_eq!(cell.content.as_char(), Some('▀'));
        assert_eq!(cell.fg, top);
        assert_eq!(cell.bg, bottom);
    }

    #[test]
    fn blit_to_painter_respects_stride() {
        let mut fb = DoomFramebuffer::new(2, 2);
        fb.set_pixel(0, 0, PackedRgba::RED);
        fb.set_pixel(1, 0, PackedRgba::GREEN);
        fb.set_pixel(0, 1, PackedRgba::BLUE);
        fb.set_pixel(1, 1, PackedRgba::WHITE);

        let mut painter = Painter::new(2, 2, Mode::HalfBlock);
        // stride=2 on a 2x2 painter should only sample the top-left pixel.
        fb.blit_to_painter(&mut painter, 2);

        assert!(painter.get(0, 0));
        assert!(!painter.get(1, 0));
        assert!(!painter.get(0, 1));
        assert!(!painter.get(1, 1));
    }

    #[test]
    fn blit_to_painter_scales_x_nearest_neighbor() {
        let mut fb = DoomFramebuffer::new(2, 2);
        fb.set_pixel(0, 0, PackedRgba::RED);
        fb.set_pixel(1, 0, PackedRgba::GREEN);
        fb.set_pixel(0, 1, PackedRgba::BLUE);
        fb.set_pixel(1, 1, PackedRgba::WHITE);

        // Upscale from 2->4 columns; left half should map to fb_x=0, right half to fb_x=1.
        let mut painter = Painter::new(4, 2, Mode::HalfBlock);
        fb.blit_to_painter(&mut painter, 1);

        let mut buf = Buffer::new(4, 1);
        painter.render_to_buffer(Rect::new(0, 0, 4, 1), &mut buf, Style::default());

        for x in 0..4u16 {
            let cell = buf.get(x, 0).expect("rendered cell");
            assert_eq!(cell.content.as_char(), Some('▀'), "x={x}");
            if x < 2 {
                assert_eq!(cell.fg, PackedRgba::RED, "x={x} fg");
                assert_eq!(cell.bg, PackedRgba::BLUE, "x={x} bg");
            } else {
                assert_eq!(cell.fg, PackedRgba::GREEN, "x={x} fg");
                assert_eq!(cell.bg, PackedRgba::WHITE, "x={x} bg");
            }
        }
    }

    #[test]
    fn blit_to_painter_stride_zero_matches_stride_one() {
        let mut fb = DoomFramebuffer::new(3, 2);
        let top = [PackedRgba::RED, PackedRgba::GREEN, PackedRgba::BLUE];
        let bottom = [
            PackedRgba::WHITE,
            PackedRgba::rgb(10, 20, 30),
            PackedRgba::rgb(200, 100, 50),
        ];

        for x in 0..3u32 {
            fb.set_pixel(x, 0, top[x as usize]);
            fb.set_pixel(x, 1, bottom[x as usize]);
        }

        let mut painter_stride_zero = Painter::new(3, 2, Mode::HalfBlock);
        let mut painter_stride_one = Painter::new(3, 2, Mode::HalfBlock);
        fb.blit_to_painter(&mut painter_stride_zero, 0);
        fb.blit_to_painter(&mut painter_stride_one, 1);

        let mut buf_stride_zero = Buffer::new(3, 1);
        let mut buf_stride_one = Buffer::new(3, 1);
        painter_stride_zero.render_to_buffer(
            Rect::new(0, 0, 3, 1),
            &mut buf_stride_zero,
            Style::default(),
        );
        painter_stride_one.render_to_buffer(
            Rect::new(0, 0, 3, 1),
            &mut buf_stride_one,
            Style::default(),
        );

        for x in 0..3u16 {
            let left = buf_stride_zero.get(x, 0).expect("stride=0 cell");
            let right = buf_stride_one.get(x, 0).expect("stride=1 cell");
            assert_eq!(
                left.content.as_char(),
                right.content.as_char(),
                "x={x} char"
            );
            assert_eq!(left.fg, right.fg, "x={x} fg");
            assert_eq!(left.bg, right.bg, "x={x} bg");
        }
    }

    // ================================================================
    // Edge-case tests (bd-2kidr)
    // ================================================================

    #[test]
    fn debug_formatting() {
        let fb = DoomFramebuffer::new(2, 2);
        let dbg = format!("{:?}", fb);
        assert!(dbg.contains("DoomFramebuffer"));
        assert!(dbg.contains("width: 2"));
        assert!(dbg.contains("height: 2"));
    }

    #[test]
    fn clone_independence() {
        let mut original = DoomFramebuffer::new(3, 3);
        original.set_pixel(1, 1, PackedRgba::RED);
        let mut cloned = original.clone();
        cloned.set_pixel(1, 1, PackedRgba::GREEN);
        // Original unaffected
        assert_eq!(original.get_pixel(1, 1), PackedRgba::RED);
        assert_eq!(cloned.get_pixel(1, 1), PackedRgba::GREEN);
    }

    #[test]
    fn blit_to_painter_zero_framebuffer() {
        let fb = DoomFramebuffer::new(0, 0);
        let mut painter = Painter::new(5, 4, Mode::HalfBlock);
        fb.blit_to_painter(&mut painter, 1);
        // Should not panic
    }

    #[test]
    fn blit_to_painter_zero_painter() {
        let fb = DoomFramebuffer::new(5, 5);
        let mut painter = Painter::new(0, 0, Mode::HalfBlock);
        fb.blit_to_painter(&mut painter, 1);
        // Should not panic
    }

    #[test]
    fn set_pixel_on_zero_sized_fb() {
        let mut fb = DoomFramebuffer::new(0, 0);
        fb.set_pixel(0, 0, PackedRgba::RED); // Out of bounds, should not panic
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::BLACK);
    }

    #[test]
    fn clear_on_zero_sized_fb() {
        let mut fb = DoomFramebuffer::new(0, 0);
        fb.clear(); // No panic on empty pixels vec
        assert_eq!(fb.pixels.len(), 0);
    }

    #[test]
    fn resize_from_zero_to_nonzero() {
        let mut fb = DoomFramebuffer::new(0, 0);
        fb.resize(3, 3);
        assert_eq!(fb.pixels.len(), 9);
        fb.set_pixel(2, 2, PackedRgba::RED);
        assert_eq!(fb.get_pixel(2, 2), PackedRgba::RED);
    }

    #[test]
    fn resize_then_draw_column() {
        let mut fb = DoomFramebuffer::new(2, 2);
        fb.resize(5, 5);
        fb.draw_column(3, 0, 5, PackedRgba::GREEN);
        for y in 0..5 {
            assert_eq!(fb.get_pixel(3, y), PackedRgba::GREEN, "y={y}");
        }
    }

    #[test]
    fn draw_column_zero_zero_range() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.draw_column(0, 0, 0, PackedRgba::RED);
        // Empty range, nothing drawn
        for y in 0..5 {
            assert_eq!(fb.get_pixel(0, y), PackedRgba::BLACK, "y={y}");
        }
    }

    #[test]
    fn draw_column_shaded_y_both_beyond_height() {
        let mut fb = DoomFramebuffer::new(5, 5);
        // Both y_top and y_bottom beyond framebuffer height
        fb.draw_column_shaded(0, 10, 20, 200, 200, 200, 1.0, 1.0);
        // Nothing should be drawn, no panic
        for y in 0..5 {
            assert_eq!(fb.get_pixel(0, y), PackedRgba::BLACK, "y={y}");
        }
    }

    #[test]
    fn sequential_operations() {
        let mut fb = DoomFramebuffer::new(4, 4);
        // Set, clear, resize, set again
        fb.set_pixel(0, 0, PackedRgba::RED);
        fb.clear();
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::BLACK);
        fb.resize(6, 6);
        fb.set_pixel(5, 5, PackedRgba::GREEN);
        assert_eq!(fb.get_pixel(5, 5), PackedRgba::GREEN);
        fb.draw_column(3, 0, 6, PackedRgba::BLUE);
        for y in 0..6 {
            assert_eq!(fb.get_pixel(3, y), PackedRgba::BLUE);
        }
    }

    #[test]
    fn resize_same_dimensions() {
        let mut fb = DoomFramebuffer::new(5, 5);
        fb.set_pixel(2, 2, PackedRgba::RED);
        fb.resize(5, 5);
        // Dimensions unchanged, but Vec::resize keeps existing data
        assert_eq!(fb.width, 5);
        assert_eq!(fb.height, 5);
        assert_eq!(fb.pixels.len(), 25);
        assert_eq!(fb.get_pixel(2, 2), PackedRgba::RED);
    }

    #[test]
    fn blit_to_painter_stride_larger_than_dimensions_samples_once() {
        let mut fb = DoomFramebuffer::new(4, 4);
        fb.set_pixel(0, 0, PackedRgba::RED);
        let mut painter = Painter::new(4, 4, Mode::HalfBlock);
        fb.blit_to_painter(&mut painter, 99);

        for y in 0..4u16 {
            for x in 0..4u16 {
                let expected = x == 0 && y == 0;
                assert_eq!(
                    painter.get(i32::from(x), i32::from(y)),
                    expected,
                    "({x},{y})"
                );
            }
        }
    }

    #[test]
    fn blit_to_painter_scales_x_non_even_ratio() {
        let mut fb = DoomFramebuffer::new(3, 2);
        let top = [PackedRgba::RED, PackedRgba::GREEN, PackedRgba::BLUE];
        let bottom = [
            PackedRgba::WHITE,
            PackedRgba::rgb(10, 20, 30),
            PackedRgba::rgb(200, 100, 50),
        ];
        for x in 0..3u32 {
            fb.set_pixel(x, 0, top[x as usize]);
            fb.set_pixel(x, 1, bottom[x as usize]);
        }

        let mut painter = Painter::new(5, 2, Mode::HalfBlock);
        fb.blit_to_painter(&mut painter, 1);

        let mut buf = Buffer::new(5, 1);
        painter.render_to_buffer(Rect::new(0, 0, 5, 1), &mut buf, Style::default());

        // floor(px * 3 / 5) => [0, 0, 1, 1, 2]
        let expected_source_column = [0usize, 0, 1, 1, 2];
        for (x, source_col) in expected_source_column.iter().enumerate() {
            let cell = buf.get(x as u16, 0).expect("rendered cell");
            assert_eq!(cell.content.as_char(), Some('▀'), "x={x}");
            assert_eq!(cell.fg, top[*source_col], "x={x} fg");
            assert_eq!(cell.bg, bottom[*source_col], "x={x} bg");
        }
    }
}

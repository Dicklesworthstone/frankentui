//! RGBA framebuffer for the Quake renderer.
//!
//! Renders to a fixed-size pixel buffer, then blits to a Painter for terminal output.
//! Pattern mirrors doom/framebuffer.rs for consistency.

use ftui_render::cell::PackedRgba;

use crate::canvas::Painter;

/// RGBA framebuffer with depth buffer for 3D rendering.
#[derive(Debug, Clone)]
pub struct QuakeFramebuffer {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA pixels.
    pub pixels: Vec<PackedRgba>,
    /// Per-pixel depth buffer (z values, larger = farther).
    pub depth: Vec<f32>,
}

impl QuakeFramebuffer {
    /// Create a new framebuffer with the given dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        let size = (width * height) as usize;
        Self {
            width,
            height,
            pixels: vec![PackedRgba::BLACK; size],
            depth: vec![f32::MAX; size],
        }
    }

    /// Clear the framebuffer to black and reset depth buffer.
    pub fn clear(&mut self) {
        self.pixels.fill(PackedRgba::BLACK);
        self.depth.fill(f32::MAX);
    }

    /// Reset only the depth buffer (pixels left as-is).
    #[inline]
    pub fn clear_depth(&mut self) {
        self.depth.fill(f32::MAX);
    }

    /// Set a pixel at (x, y) with depth test.
    #[inline]
    pub fn set_pixel_depth(&mut self, x: u32, y: u32, z: f32, color: PackedRgba) {
        if x < self.width && y < self.height {
            let idx = (y * self.width + x) as usize;
            if z < self.depth[idx] {
                self.pixels[idx] = color;
                self.depth[idx] = z;
            }
        }
    }

    /// Set a pixel at (x, y) unconditionally.
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

    /// Draw a vertical column of a single color.
    #[inline]
    pub fn draw_column(&mut self, x: u32, y_top: u32, y_bottom: u32, color: PackedRgba) {
        if x >= self.width {
            return;
        }
        let top = y_top.min(self.height);
        let bottom = y_bottom.min(self.height);
        for y in top..bottom {
            self.pixels[(y * self.width + x) as usize] = color;
        }
    }

    /// Draw a vertical column with distance-based shading.
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
        for y in top..bottom {
            let light = light_top + light_delta * ((y - top) as f32 * inv_height);
            let r = (base_r_f * light).min(255.0) as u8;
            let g = (base_g_f * light).min(255.0) as u8;
            let b = (base_b_f * light).min(255.0) as u8;
            self.pixels[(y * self.width + x) as usize] = PackedRgba::rgb(r, g, b);
        }
    }

    /// Blit the framebuffer to a Painter, scaling to fit.
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
        let size = (width * height) as usize;
        self.pixels.resize(size, PackedRgba::BLACK);
        self.depth.resize(size, f32::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_framebuffer_is_black() {
        let fb = QuakeFramebuffer::new(10, 10);
        assert_eq!(fb.pixels.len(), 100);
        for p in &fb.pixels {
            assert_eq!(*p, PackedRgba::BLACK);
        }
    }

    #[test]
    fn depth_test_closer_wins() {
        let mut fb = QuakeFramebuffer::new(10, 10);
        fb.set_pixel_depth(5, 5, 100.0, PackedRgba::RED);
        fb.set_pixel_depth(5, 5, 50.0, PackedRgba::GREEN);
        assert_eq!(fb.get_pixel(5, 5), PackedRgba::GREEN);
        // Farther pixel should not overwrite
        fb.set_pixel_depth(5, 5, 200.0, PackedRgba::BLUE);
        assert_eq!(fb.get_pixel(5, 5), PackedRgba::GREEN);
    }

    #[test]
    fn out_of_bounds_is_safe() {
        let mut fb = QuakeFramebuffer::new(10, 10);
        fb.set_pixel(100, 100, PackedRgba::RED);
        assert_eq!(fb.get_pixel(100, 100), PackedRgba::BLACK);
    }

    #[test]
    fn set_pixel_overwrites_unconditionally() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.set_pixel(2, 3, PackedRgba::RED);
        assert_eq!(fb.get_pixel(2, 3), PackedRgba::RED);
        fb.set_pixel(2, 3, PackedRgba::GREEN);
        assert_eq!(fb.get_pixel(2, 3), PackedRgba::GREEN);
    }

    #[test]
    fn clear_resets_pixels_and_depth() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.set_pixel(0, 0, PackedRgba::RED);
        fb.set_pixel_depth(1, 1, 10.0, PackedRgba::rgb(0, 255, 0));
        fb.clear();
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(1, 1), PackedRgba::BLACK);
        // Depth should be reset - a normal value should now win against f32::MAX
        let color = PackedRgba::rgb(0, 0, 255);
        fb.set_pixel_depth(1, 1, 100.0, color);
        assert_eq!(fb.get_pixel(1, 1), color);
    }

    #[test]
    fn draw_column_fills_vertical_strip() {
        let mut fb = QuakeFramebuffer::new(10, 10);
        fb.draw_column(3, 2, 6, PackedRgba::RED);
        assert_eq!(fb.get_pixel(3, 1), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(3, 2), PackedRgba::RED);
        assert_eq!(fb.get_pixel(3, 5), PackedRgba::RED);
        assert_eq!(fb.get_pixel(3, 6), PackedRgba::BLACK);
    }

    #[test]
    fn draw_column_out_of_bounds_x_is_safe() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.draw_column(10, 0, 5, PackedRgba::RED);
        // Should not panic
    }

    #[test]
    fn draw_column_shaded_gradient() {
        let mut fb = QuakeFramebuffer::new(10, 10);
        fb.draw_column_shaded(0, 0, 4, 100, 100, 100, 1.0, 0.0);
        // Top pixel should be brighter than bottom pixel
        let top = fb.get_pixel(0, 0);
        let bot = fb.get_pixel(0, 3);
        assert!(top.r() >= bot.r(), "top should be brighter than bottom");
    }

    #[test]
    fn draw_column_shaded_zero_height_is_safe() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.draw_column_shaded(0, 3, 3, 100, 100, 100, 1.0, 1.0);
        // Should not panic with zero-height column
    }

    #[test]
    fn resize_changes_dimensions() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.set_pixel(2, 2, PackedRgba::RED);
        fb.resize(10, 10);
        assert_eq!(fb.width, 10);
        assert_eq!(fb.height, 10);
        assert_eq!(fb.pixels.len(), 100);
        assert_eq!(fb.depth.len(), 100);
    }

    #[test]
    fn set_pixel_depth_out_of_bounds_is_safe() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.set_pixel_depth(10, 10, 1.0, PackedRgba::RED);
        // Should not panic
    }

    // --- new() ---

    #[test]
    fn new_depth_buffer_is_max() {
        let fb = QuakeFramebuffer::new(4, 3);
        assert_eq!(fb.depth.len(), 12);
        for &d in &fb.depth {
            assert_eq!(d, f32::MAX);
        }
    }

    #[test]
    fn new_zero_dimensions() {
        let fb = QuakeFramebuffer::new(0, 0);
        assert_eq!(fb.pixels.len(), 0);
        assert_eq!(fb.depth.len(), 0);
        assert_eq!(fb.width, 0);
        assert_eq!(fb.height, 0);
    }

    #[test]
    fn new_single_pixel() {
        let fb = QuakeFramebuffer::new(1, 1);
        assert_eq!(fb.pixels.len(), 1);
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::BLACK);
    }

    // --- get_pixel boundary ---

    #[test]
    fn get_pixel_at_max_valid_coords() {
        let mut fb = QuakeFramebuffer::new(3, 3);
        fb.set_pixel(2, 2, PackedRgba::RED);
        assert_eq!(fb.get_pixel(2, 2), PackedRgba::RED);
    }

    #[test]
    fn get_pixel_out_of_bounds_returns_black() {
        let fb = QuakeFramebuffer::new(5, 5);
        assert_eq!(fb.get_pixel(5, 0), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(0, 5), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(u32::MAX, u32::MAX), PackedRgba::BLACK);
    }

    // --- set_pixel boundary ---

    #[test]
    fn set_pixel_at_origin() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.set_pixel(0, 0, PackedRgba::GREEN);
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::GREEN);
    }

    #[test]
    fn set_pixel_oob_does_not_modify() {
        let mut fb = QuakeFramebuffer::new(3, 3);
        fb.set_pixel(3, 0, PackedRgba::RED);
        fb.set_pixel(0, 3, PackedRgba::RED);
        // All pixels should still be black
        for y in 0..3 {
            for x in 0..3 {
                assert_eq!(fb.get_pixel(x, y), PackedRgba::BLACK);
            }
        }
    }

    // --- set_pixel_depth ---

    #[test]
    fn set_pixel_depth_equal_z_does_not_overwrite() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.set_pixel_depth(0, 0, 10.0, PackedRgba::RED);
        fb.set_pixel_depth(0, 0, 10.0, PackedRgba::GREEN);
        // Equal z is NOT less than current, so RED stays
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::RED);
    }

    #[test]
    fn set_pixel_depth_negative_z() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.set_pixel_depth(0, 0, 1.0, PackedRgba::RED);
        fb.set_pixel_depth(0, 0, -1.0, PackedRgba::GREEN);
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::GREEN);
    }

    #[test]
    fn set_pixel_depth_updates_depth_buffer() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.set_pixel_depth(1, 1, 50.0, PackedRgba::RED);
        assert_eq!(fb.depth[6], 50.0); // row 1, col 1 in 5-wide buffer
    }

    // --- draw_column ---

    #[test]
    fn draw_column_full_height() {
        let mut fb = QuakeFramebuffer::new(3, 4);
        fb.draw_column(1, 0, 4, PackedRgba::BLUE);
        for y in 0..4 {
            assert_eq!(fb.get_pixel(1, y), PackedRgba::BLUE);
        }
        // Other columns untouched
        for y in 0..4 {
            assert_eq!(fb.get_pixel(0, y), PackedRgba::BLACK);
            assert_eq!(fb.get_pixel(2, y), PackedRgba::BLACK);
        }
    }

    #[test]
    fn draw_column_y_bottom_exceeds_height() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.draw_column(0, 3, 100, PackedRgba::RED);
        assert_eq!(fb.get_pixel(0, 2), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(0, 3), PackedRgba::RED);
        assert_eq!(fb.get_pixel(0, 4), PackedRgba::RED);
    }

    #[test]
    fn draw_column_inverted_range_no_effect() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.draw_column(0, 4, 2, PackedRgba::RED);
        // y_top > y_bottom means empty range
        for y in 0..5 {
            assert_eq!(fb.get_pixel(0, y), PackedRgba::BLACK);
        }
    }

    // --- draw_column_shaded ---

    #[test]
    fn draw_column_shaded_uniform_light() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.draw_column_shaded(2, 0, 3, 200, 100, 50, 1.0, 1.0);
        // Uniform light=1.0 means all pixels should be (200, 100, 50)
        for y in 0..3 {
            let p = fb.get_pixel(2, y);
            assert_eq!(p.r(), 200);
            assert_eq!(p.g(), 100);
            assert_eq!(p.b(), 50);
        }
    }

    #[test]
    fn draw_column_shaded_oob_x_is_safe() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.draw_column_shaded(10, 0, 5, 255, 255, 255, 1.0, 0.0);
        // Should not panic
    }

    #[test]
    fn draw_column_shaded_light_clamped_to_255() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        // Light=2.0 on base=200 → 400, should clamp to 255
        fb.draw_column_shaded(0, 0, 1, 200, 200, 200, 2.0, 2.0);
        let p = fb.get_pixel(0, 0);
        assert_eq!(p.r(), 255);
        assert_eq!(p.g(), 255);
        assert_eq!(p.b(), 255);
    }

    #[test]
    fn draw_column_shaded_zero_light() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        fb.draw_column_shaded(0, 0, 1, 255, 255, 255, 0.0, 0.0);
        let p = fb.get_pixel(0, 0);
        assert_eq!(p.r(), 0);
        assert_eq!(p.g(), 0);
        assert_eq!(p.b(), 0);
    }

    // --- resize ---

    #[test]
    fn resize_to_zero() {
        let mut fb = QuakeFramebuffer::new(10, 10);
        fb.resize(0, 0);
        assert_eq!(fb.width, 0);
        assert_eq!(fb.height, 0);
        assert_eq!(fb.pixels.len(), 0);
    }

    #[test]
    fn resize_grows_with_black_pixels() {
        let mut fb = QuakeFramebuffer::new(2, 2);
        fb.set_pixel(0, 0, PackedRgba::RED);
        fb.resize(4, 4);
        // New pixels should be black (Vec::resize fills with default)
        assert_eq!(fb.pixels.len(), 16);
        assert_eq!(fb.depth.len(), 16);
    }

    #[test]
    fn resize_shrinks() {
        let mut fb = QuakeFramebuffer::new(10, 10);
        fb.resize(3, 3);
        assert_eq!(fb.pixels.len(), 9);
        assert_eq!(fb.depth.len(), 9);
    }

    // --- blit_to_painter ---

    #[test]
    fn blit_to_painter_zero_painter_is_safe() {
        let fb = QuakeFramebuffer::new(10, 10);
        let mut painter = Painter::new(0, 0, crate::canvas::Mode::Braille);
        fb.blit_to_painter(&mut painter, 1);
        // Should not panic
    }

    #[test]
    fn blit_to_painter_zero_framebuffer_is_safe() {
        let fb = QuakeFramebuffer::new(0, 0);
        let mut painter = Painter::new(10, 10, crate::canvas::Mode::Braille);
        fb.blit_to_painter(&mut painter, 1);
        // Should not panic
    }

    #[test]
    fn blit_to_painter_stride_zero_treated_as_one() {
        let mut fb = QuakeFramebuffer::new(4, 4);
        fb.set_pixel(0, 0, PackedRgba::RED);
        let mut painter = Painter::new(4, 4, crate::canvas::Mode::Braille);
        fb.blit_to_painter(&mut painter, 0);
        // stride=0 → clamped to 1, should not panic
    }

    #[test]
    fn blit_to_painter_populates_colors() {
        let mut fb = QuakeFramebuffer::new(2, 2);
        fb.set_pixel(0, 0, PackedRgba::RED);
        fb.set_pixel(1, 0, PackedRgba::GREEN);
        fb.set_pixel(0, 1, PackedRgba::BLUE);
        fb.set_pixel(1, 1, PackedRgba::WHITE);

        // Use a painter same size as fb
        let mut painter = Painter::new(2, 2, crate::canvas::Mode::Braille);
        fb.blit_to_painter(&mut painter, 1);

        // Verify colors were written (painter.colors has Some values)
        let (pw, ph) = painter.size();
        assert_eq!(pw, 2);
        assert_eq!(ph, 2);
    }

    // --- clear after writes ---

    #[test]
    fn clear_after_depth_writes_allows_rewrite() {
        let mut fb = QuakeFramebuffer::new(3, 3);
        fb.set_pixel_depth(1, 1, 5.0, PackedRgba::RED);
        fb.clear();
        // After clear, depth is MAX again, so any z should write
        fb.set_pixel_depth(1, 1, 1000.0, PackedRgba::GREEN);
        assert_eq!(fb.get_pixel(1, 1), PackedRgba::GREEN);
    }

    // --- draw_column_shaded: gradient interpolation ---

    #[test]
    fn draw_column_shaded_gradient_interpolates() {
        let mut fb = QuakeFramebuffer::new(1, 10);
        fb.draw_column_shaded(0, 0, 10, 100, 100, 100, 1.0, 0.5);
        // Top should be brighter (light=1.0), bottom should be dimmer (light~0.5)
        let top = fb.get_pixel(0, 0);
        let bottom = fb.get_pixel(0, 9);
        assert!(top.r() > bottom.r());
    }

    // --- clear_depth ---

    #[test]
    fn clear_depth_resets_z_buffer_only() {
        let mut fb = QuakeFramebuffer::new(3, 3);
        let color = PackedRgba::rgb(200, 100, 50);
        fb.set_pixel_depth(1, 1, 5.0, color);
        assert_eq!(fb.get_pixel(1, 1), color);

        fb.clear_depth();
        // Pixel color should still be intact after clear_depth
        assert_eq!(fb.get_pixel(1, 1), color);
        // Depth should be reset to MAX, allowing any z to pass
        fb.set_pixel_depth(1, 1, 999.0, PackedRgba::GREEN);
        assert_eq!(fb.get_pixel(1, 1), PackedRgba::GREEN);
    }

    #[test]
    fn clear_depth_allows_closer_write_after_far_write() {
        let mut fb = QuakeFramebuffer::new(2, 2);
        fb.set_pixel_depth(0, 0, 1.0, PackedRgba::RED);
        // Far write blocked by closer existing depth
        fb.set_pixel_depth(0, 0, 10.0, PackedRgba::BLUE);
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::RED);

        // After clear_depth, any z should succeed
        fb.clear_depth();
        fb.set_pixel_depth(0, 0, 10.0, PackedRgba::BLUE);
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::BLUE);
    }

    #[test]
    fn clear_depth_on_empty_framebuffer_is_noop() {
        let mut fb = QuakeFramebuffer::new(0, 0);
        fb.clear_depth(); // Should not panic
    }

    #[test]
    fn clear_depth_does_not_affect_full_clear_behavior() {
        let mut fb = QuakeFramebuffer::new(2, 2);
        fb.set_pixel(0, 0, PackedRgba::RED);
        fb.clear_depth();
        // Pixel still has color
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::RED);
        // Full clear resets both
        fb.clear();
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::BLACK);
    }
}

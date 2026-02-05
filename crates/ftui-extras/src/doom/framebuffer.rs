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
        for y in top..bottom {
            self.pixels[(y * self.width + x) as usize] = color;
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
        for y in top..bottom {
            let t = (y - top) as f32 / height as f32;
            let light = light_top + (light_bottom - light_top) * t;
            let r = (base_r as f32 * light) as u8;
            let g = (base_g as f32 * light) as u8;
            let b = (base_b as f32 * light) as u8;
            self.pixels[(y * self.width + x) as usize] = PackedRgba::rgb(r, g, b);
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

        for py in (0..ph).step_by(stride as usize) {
            // Map painter y to framebuffer y
            let fb_y = (py * self.height) / ph;
            for px in (0..pw).step_by(stride as usize) {
                // Map painter x to framebuffer x
                let fb_x = (px * self.width) / pw;
                let color = self.get_pixel(fb_x, fb_y);
                // Only draw non-black pixels (skip transparent/background)
                if color.0 != 0 {
                    painter.point_colored(px as i32, py as i32, color);
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
}

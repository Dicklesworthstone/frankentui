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
        for y in top..bottom {
            let t = (y - top) as f32 / height as f32;
            let light = light_top + (light_bottom - light_top) * t;
            let r = (base_r as f32 * light).min(255.0) as u8;
            let g = (base_g as f32 * light).min(255.0) as u8;
            let b = (base_b as f32 * light).min(255.0) as u8;
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

        for py in (0..ph).step_by(stride as usize) {
            let fb_y = (py * self.height) / ph;
            for px in (0..pw).step_by(stride as usize) {
                let fb_x = (px * self.width) / pw;
                let color = self.get_pixel(fb_x, fb_y);
                painter.point_colored(px as i32, py as i32, color);
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
}

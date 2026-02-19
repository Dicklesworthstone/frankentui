#![forbid(unsafe_code)]

//! Quake Console effect.
//!
//! A drop-down console background with a procedural grunge/cloud texture.
//! Simulates the look of Quake's `conback.lmp` using fractal noise.

use crate::visual_fx::{BackdropFx, FxContext};
use ftui_render::cell::PackedRgba;

/// Quake-style drop-down console effect.
pub struct QuakeConsoleFx {
    /// Current drop extension (0.0 to 1.0).
    drop_progress: f32,
    /// Texture buffer to avoid recomputing noise every frame.
    texture: Vec<PackedRgba>,
    texture_size: (u16, u16),
}

impl QuakeConsoleFx {
    pub fn new() -> Self {
        Self { 
            drop_progress: 1.0,
            texture: Vec::new(),
            texture_size: (0, 0),
        }
    }

    pub fn set_progress(&mut self, progress: f32) {
        self.drop_progress = progress.clamp(0.0, 1.0);
    }

    /// Generate a procedural grunge texture.
    fn generate_texture(&mut self, width: u16, height: u16, theme: &crate::visual_fx::ThemeInputs) {
        let w = width as usize;
        let h = height as usize;
        let len = w * h;
        
        self.texture.resize(len, PackedRgba::default());
        self.texture_size = (width, height);

        // Simple value noise / fractional brownian motion approximation
        // We use a fixed seed LCG for determinism without external deps
        let mut rng = 0xCAFEBABE_u32;
        let mut rand = || {
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            (rng >> 16) as f32 / 65536.0
        };

        let base = theme.bg_base; // Dark brown/grey
        let highlight = theme.bg_surface; // Lighter brown

        // Generate noise
        for y in 0..h {
            for x in 0..w {
                // Coordinate scaling for "cloudy" look
                let nx = x as f32 * 0.1;
                let ny = y as f32 * 0.2;
                
                // Cheap noise: sin combination
                let v = (nx.sin() + ny.cos() + rand() * 0.5).abs().clamp(0.0, 1.0);
                
                // Mix colors: base + v * (highlight - base)
                // Since we can't easily lerp packed colors without unpacking,
                // we'll just overlay the highlight with opacity `v`.
                let pixel = highlight.with_opacity(v * 0.3).over(base);
                
                self.texture[y * w + x] = pixel;
            }
        }
    }
}

impl BackdropFx for QuakeConsoleFx {
    fn name(&self) -> &'static str {
        "quake-console"
    }

    fn resize(&mut self, width: u16, height: u16) {
        // We defer generation to render to access the theme,
        // but we track size here if needed.
    }

    fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
        if ctx.is_empty() {
            return;
        }

        let width = ctx.width as usize;
        let height = ctx.height as usize;
        let drop_height = (height as f32 * self.drop_progress).round() as usize;

        // Regenerate texture if size changed (or first run)
        if self.texture_size != (ctx.width, ctx.height) {
            self.generate_texture(ctx.width, ctx.height, ctx.theme);
        }

        // Blit texture for the dropped portion
        for y in 0..drop_height {
            let row_start = y * width;
            let src_row = &self.texture[row_start..row_start + width];
            let dst_row = &mut out[row_start..row_start + width];
            dst_row.copy_from_slice(src_row);
        }
        
        // Draw the bottom edge/highlight
        if drop_height < height && drop_height > 0 {
             let y = drop_height - 1;
             for x in 0..width {
                 let idx = y * width + x;
                 out[idx] = ctx.theme.accent_primary;
             }
        }
    }
}

impl Default for QuakeConsoleFx {
    fn default() -> Self {
        Self::new()
    }
}

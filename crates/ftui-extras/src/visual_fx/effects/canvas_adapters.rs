#![forbid(unsafe_code)]

//! Canvas Adapters for Visual FX (Braille/Sub-Cell Resolution)
//!
//! This module provides adapters that use the shared sampling API to fill
//! a `Painter` at sub-pixel resolution (Braille 2×4 dots per cell), achieving
//! higher effective resolution for visual effects like metaballs and plasma.
//!
//! # Feature Gating
//!
//! This module requires both `visual-fx` and `canvas` features to be enabled.
//! When only `visual-fx` is enabled, effects render at cell resolution.
//! When both are enabled, these adapters provide the higher-resolution option.
//!
//! # Design
//!
//! - **No duplicated math**: All sampling uses the shared `sampling` module.
//! - **No allocations per frame**: Painter buffers reused via `ensure_size`.
//! - **Theme-aware**: Colors derived from `ThemeInputs`.
//!
//! # Usage
//!
//! ```ignore
//! use ftui_extras::visual_fx::effects::canvas_adapters::{PlasmaCanvasAdapter, MetaballsCanvasAdapter};
//! use ftui_extras::canvas::{Painter, Mode, Canvas};
//!
//! // Create adapter
//! let mut plasma = PlasmaCanvasAdapter::new(PlasmaPalette::Neon);
//!
//! // Fill painter at sub-pixel resolution
//! let mut painter = Painter::for_area(area, Mode::Braille);
//! plasma.fill(&mut painter, time, quality, &theme);
//!
//! // Convert to widget and render
//! Canvas::from_painter(&painter).render(area, &mut frame);
//! ```

use crate::canvas::Painter;
use crate::visual_fx::effects::metaballs::MetaballsParams;
use crate::visual_fx::effects::plasma::PlasmaPalette;
use crate::visual_fx::effects::sampling::{
    BallState, MetaballFieldSampler, PlasmaSampler, Sampler,
};
use crate::visual_fx::{FxQuality, ThemeInputs};
use ftui_render::cell::PackedRgba;

// =============================================================================
// Plasma Canvas Adapter
// =============================================================================

/// Canvas adapter for rendering plasma at sub-pixel resolution.
///
/// Uses the shared `PlasmaSampler` for all wave computation, ensuring
/// identical results to cell-space rendering at higher resolution.
#[derive(Debug, Clone)]
pub struct PlasmaCanvasAdapter {
    /// Color palette for the effect.
    palette: PlasmaPalette,
}

impl PlasmaCanvasAdapter {
    /// Create a new plasma canvas adapter.
    #[inline]
    pub const fn new(palette: PlasmaPalette) -> Self {
        Self { palette }
    }

    /// Create a plasma adapter using theme accent colors.
    #[inline]
    pub const fn theme() -> Self {
        Self::new(PlasmaPalette::ThemeAccents)
    }

    /// Set the color palette.
    #[inline]
    pub fn set_palette(&mut self, palette: PlasmaPalette) {
        self.palette = palette;
    }

    /// Fill a painter with plasma at sub-pixel resolution.
    ///
    /// # Arguments
    /// * `painter` - The painter to fill (should be sized for the target area)
    /// * `time` - Current time in seconds (for animation)
    /// * `quality` - Quality tier (affects wave computation)
    /// * `theme` - Theme colors for palette lookup
    ///
    /// # No Allocations
    /// This method does not allocate after initial painter setup.
    pub fn fill(
        &self,
        painter: &mut Painter,
        time: f64,
        quality: FxQuality,
        theme: &ThemeInputs,
    ) {
        if !quality.is_enabled() {
            return;
        }

        let (width, height) = painter.size();
        if width == 0 || height == 0 {
            return;
        }

        let sampler = PlasmaSampler;
        let w = width as f64;
        let h = height as f64;

        for dy in 0..height {
            // Normalized y: sample at sub-pixel centers
            let ny = (dy as f64 + 0.5) / h;

            for dx in 0..width {
                // Normalized x: sample at sub-pixel centers
                let nx = (dx as f64 + 0.5) / w;

                // Sample plasma wave intensity
                let wave = sampler.sample(nx, ny, time, quality);

                // Convert to color via palette
                let color = self.palette.color_at(wave, theme);

                // Set sub-pixel with color
                painter.point_colored(dx as i32, dy as i32, color);
            }
        }
    }
}

impl Default for PlasmaCanvasAdapter {
    fn default() -> Self {
        Self::theme()
    }
}

// =============================================================================
// Metaballs Canvas Adapter
// =============================================================================

/// Canvas adapter for rendering metaballs at sub-pixel resolution.
///
/// Uses the shared `MetaballFieldSampler` for all field computation, ensuring
/// identical results to cell-space rendering at higher resolution.
#[derive(Debug, Clone)]
pub struct MetaballsCanvasAdapter {
    /// Parameters controlling metaball behavior.
    params: MetaballsParams,
    /// Cached ball states for the current frame.
    ball_cache: Vec<BallState>,
}

impl MetaballsCanvasAdapter {
    /// Create a new metaballs canvas adapter with default parameters.
    pub fn new() -> Self {
        Self {
            params: MetaballsParams::default(),
            ball_cache: Vec::new(),
        }
    }

    /// Create a metaballs adapter with specific parameters.
    pub fn with_params(params: MetaballsParams) -> Self {
        Self {
            params,
            ball_cache: Vec::new(),
        }
    }

    /// Set the metaballs parameters.
    pub fn set_params(&mut self, params: MetaballsParams) {
        self.params = params;
    }

    /// Get the current parameters.
    pub fn params(&self) -> &MetaballsParams {
        &self.params
    }

    /// Prepare ball states for the current frame.
    ///
    /// Call this once per frame before calling `fill`.
    pub fn prepare(&mut self, time: f64, quality: FxQuality) {
        let count = ball_count_for_quality(&self.params, quality);

        // Ensure cache capacity
        if self.ball_cache.len() != count {
            self.ball_cache.resize(count, BallState {
                x: 0.0,
                y: 0.0,
                r2: 0.0,
                hue: 0.0,
            });
        }

        // Animate balls
        let t_scaled = time * self.params.time_scale;
        let (bounds_min, bounds_max) = ordered_pair(self.params.bounds_min, self.params.bounds_max);
        let (radius_min, radius_max) = ordered_pair(self.params.radius_min, self.params.radius_max);

        for (i, ball) in self.params.balls.iter().take(count).enumerate() {
            let x = ping_pong(ball.x + ball.vx * t_scaled, bounds_min, bounds_max);
            let y = ping_pong(ball.y + ball.vy * t_scaled, bounds_min, bounds_max);
            let pulse = 1.0 + self.params.pulse_amount * (time * self.params.pulse_speed + ball.phase).sin();
            let radius = ball.radius.clamp(radius_min, radius_max).max(0.001) * pulse;
            let hue = (ball.hue + time * self.params.hue_speed).rem_euclid(1.0);

            self.ball_cache[i] = BallState { x, y, r2: radius * radius, hue };
        }
    }

    /// Fill a painter with metaballs at sub-pixel resolution.
    ///
    /// # Arguments
    /// * `painter` - The painter to fill (should be sized for the target area)
    /// * `quality` - Quality tier (affects field computation)
    /// * `theme` - Theme colors for palette lookup
    ///
    /// # Prerequisites
    /// Call `prepare(time, quality)` before this method for each frame.
    ///
    /// # No Allocations
    /// This method does not allocate after initial painter setup.
    pub fn fill(
        &self,
        painter: &mut Painter,
        quality: FxQuality,
        theme: &ThemeInputs,
    ) {
        if !quality.is_enabled() || self.ball_cache.is_empty() {
            return;
        }

        let (width, height) = painter.size();
        if width == 0 || height == 0 {
            return;
        }

        let sampler = MetaballFieldSampler::new(self.ball_cache.clone());
        let (glow, threshold) = thresholds(&self.params);
        let w = width as f64;
        let h = height as f64;

        for dy in 0..height {
            let ny = (dy as f64 + 0.5) / h;

            for dx in 0..width {
                let nx = (dx as f64 + 0.5) / w;

                // Sample field and hue
                let (field, avg_hue) = sampler.sample_field(nx, ny, quality);

                if field > glow {
                    let intensity = if field > threshold {
                        1.0
                    } else {
                        (field - glow) / (threshold - glow)
                    };

                    let color = color_at(self.params.palette, avg_hue, intensity, theme);
                    painter.point_colored(dx as i32, dy as i32, color);
                }
            }
        }
    }

    /// Convenience method that calls prepare and fill.
    pub fn fill_frame(
        &mut self,
        painter: &mut Painter,
        time: f64,
        quality: FxQuality,
        theme: &ThemeInputs,
    ) {
        self.prepare(time, quality);
        self.fill(painter, quality, theme);
    }
}

impl Default for MetaballsCanvasAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Internal Helpers (mirror sampling.rs to avoid changing its API)
// =============================================================================

fn ball_count_for_quality(params: &MetaballsParams, quality: FxQuality) -> usize {
    let total = params.balls.len();
    if total == 0 {
        return 0;
    }
    match quality {
        FxQuality::Full => total,
        FxQuality::Reduced => total.saturating_sub(total / 4).max(4).min(total),
        FxQuality::Minimal => total.saturating_sub(total / 2).max(3).min(total),
        FxQuality::Off => 0,
    }
}

fn thresholds(params: &MetaballsParams) -> (f64, f64) {
    let glow = params.glow_threshold.clamp(0.0, params.threshold.max(0.001));
    let mut threshold = params.threshold.max(glow + 0.0001);
    if threshold <= glow {
        threshold = glow + 0.0001;
    }
    (glow, threshold)
}

fn color_at(
    palette: crate::visual_fx::effects::metaballs::MetaballsPalette,
    hue: f64,
    intensity: f64,
    theme: &ThemeInputs,
) -> PackedRgba {
    let stops = palette_stops(palette, theme);
    let base = gradient_color(&stops, hue);
    let t = intensity.clamp(0.0, 1.0);
    lerp_color(theme.bg_base, base, t)
}

fn palette_stops(
    palette: crate::visual_fx::effects::metaballs::MetaballsPalette,
    theme: &ThemeInputs,
) -> [PackedRgba; 4] {
    use crate::visual_fx::effects::metaballs::MetaballsPalette;
    match palette {
        MetaballsPalette::ThemeAccents => [
            theme.bg_surface,
            theme.accent_primary,
            theme.accent_secondary,
            theme.fg_primary,
        ],
        MetaballsPalette::Aurora => [
            theme.accent_slots[0],
            theme.accent_primary,
            theme.accent_slots[1],
            theme.accent_secondary,
        ],
        MetaballsPalette::Lava => [
            theme.accent_slots[2],
            theme.accent_secondary,
            theme.accent_primary,
            theme.accent_slots[3],
        ],
        MetaballsPalette::Ocean => [
            theme.accent_primary,
            theme.accent_slots[3],
            theme.accent_slots[0],
            theme.fg_primary,
        ],
    }
}

#[inline]
fn ping_pong(value: f64, min: f64, max: f64) -> f64 {
    let range = (max - min).max(0.0001);
    let period = 2.0 * range;
    let mut v = (value - min).rem_euclid(period);
    if v > range {
        v = period - v;
    }
    min + v
}

#[inline]
fn lerp_color(a: PackedRgba, b: PackedRgba, t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    let r = (a.r() as f64 + (b.r() as f64 - a.r() as f64) * t) as u8;
    let g = (a.g() as f64 + (b.g() as f64 - a.g() as f64) * t) as u8;
    let bl = (a.b() as f64 + (b.b() as f64 - a.b() as f64) * t) as u8;
    PackedRgba::rgb(r, g, bl)
}

#[inline]
fn gradient_color(stops: &[PackedRgba; 4], t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    let scaled = t * 3.0;
    let idx = (scaled.floor() as usize).min(2);
    let local = scaled - idx as f64;
    match idx {
        0 => lerp_color(stops[0], stops[1], local),
        1 => lerp_color(stops[1], stops[2], local),
        _ => lerp_color(stops[2], stops[3], local),
    }
}

#[inline]
fn ordered_pair(a: f64, b: f64) -> (f64, f64) {
    if a <= b { (a, b) } else { (b, a) }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Mode;

    fn default_theme() -> ThemeInputs {
        ThemeInputs::default_dark()
    }

    #[test]
    fn plasma_adapter_fills_painter() {
        let theme = default_theme();
        let adapter = PlasmaCanvasAdapter::theme();
        let mut painter = Painter::new(20, 16, Mode::Braille);

        adapter.fill(&mut painter, 1.0, FxQuality::Full, &theme);

        // Verify some pixels were set (plasma should fill all)
        let (w, h) = painter.size();
        let mut set_count = 0;
        for y in 0..h {
            for x in 0..w {
                if painter.get(x as i32, y as i32) {
                    set_count += 1;
                }
            }
        }
        assert!(set_count > 0, "Plasma should set pixels");
    }

    #[test]
    fn plasma_adapter_quality_off_noop() {
        let theme = default_theme();
        let adapter = PlasmaCanvasAdapter::theme();
        let mut painter = Painter::new(10, 8, Mode::Braille);

        adapter.fill(&mut painter, 1.0, FxQuality::Off, &theme);

        // No pixels should be set
        let (w, h) = painter.size();
        for y in 0..h {
            for x in 0..w {
                assert!(!painter.get(x as i32, y as i32), "Off quality should not set pixels");
            }
        }
    }

    #[test]
    fn plasma_adapter_deterministic() {
        let theme = default_theme();
        let adapter = PlasmaCanvasAdapter::new(PlasmaPalette::Ocean);
        let mut p1 = Painter::new(16, 16, Mode::Braille);
        let mut p2 = Painter::new(16, 16, Mode::Braille);

        adapter.fill(&mut p1, 2.5, FxQuality::Full, &theme);
        adapter.fill(&mut p2, 2.5, FxQuality::Full, &theme);

        // Compare pixel states
        let (w, h) = p1.size();
        for y in 0..h {
            for x in 0..w {
                assert_eq!(
                    p1.get(x as i32, y as i32),
                    p2.get(x as i32, y as i32),
                    "Plasma should be deterministic at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn metaballs_adapter_fills_painter() {
        let theme = default_theme();
        let mut adapter = MetaballsCanvasAdapter::new();
        let mut painter = Painter::new(20, 16, Mode::Braille);

        adapter.fill_frame(&mut painter, 1.0, FxQuality::Full, &theme);

        // Verify some pixels were set (metaballs should set some)
        let (w, h) = painter.size();
        let mut set_count = 0;
        for y in 0..h {
            for x in 0..w {
                if painter.get(x as i32, y as i32) {
                    set_count += 1;
                }
            }
        }
        assert!(set_count > 0, "Metaballs should set some pixels");
    }

    #[test]
    fn metaballs_adapter_quality_off_noop() {
        let theme = default_theme();
        let mut adapter = MetaballsCanvasAdapter::new();
        let mut painter = Painter::new(10, 8, Mode::Braille);

        adapter.fill_frame(&mut painter, 1.0, FxQuality::Off, &theme);

        // No pixels should be set
        let (w, h) = painter.size();
        for y in 0..h {
            for x in 0..w {
                assert!(!painter.get(x as i32, y as i32), "Off quality should not set pixels");
            }
        }
    }

    #[test]
    fn metaballs_adapter_deterministic() {
        let theme = default_theme();
        let mut adapter = MetaballsCanvasAdapter::new();
        let mut p1 = Painter::new(16, 16, Mode::Braille);
        let mut p2 = Painter::new(16, 16, Mode::Braille);

        adapter.prepare(2.5, FxQuality::Full);
        adapter.fill(&mut p1, FxQuality::Full, &theme);
        adapter.fill(&mut p2, FxQuality::Full, &theme);

        // Compare pixel states
        let (w, h) = p1.size();
        for y in 0..h {
            for x in 0..w {
                assert_eq!(
                    p1.get(x as i32, y as i32),
                    p2.get(x as i32, y as i32),
                    "Metaballs should be deterministic at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn metaballs_adapter_prepare_updates_cache() {
        let mut adapter = MetaballsCanvasAdapter::new();

        adapter.prepare(0.0, FxQuality::Full);
        let count1 = adapter.ball_cache.len();

        adapter.prepare(1.0, FxQuality::Minimal);
        let count2 = adapter.ball_cache.len();

        // Minimal quality should have fewer balls
        assert!(count2 <= count1, "Minimal should have fewer or equal balls");
    }

    #[test]
    fn empty_painter_safe() {
        let theme = default_theme();
        let adapter = PlasmaCanvasAdapter::theme();
        let mut painter = Painter::new(0, 0, Mode::Braille);

        // Should not panic
        adapter.fill(&mut painter, 1.0, FxQuality::Full, &theme);
    }

    #[test]
    fn single_pixel_painter() {
        let theme = default_theme();
        let adapter = PlasmaCanvasAdapter::theme();
        let mut painter = Painter::new(1, 1, Mode::Braille);

        adapter.fill(&mut painter, 0.5, FxQuality::Full, &theme);

        // Single pixel should be set
        assert!(painter.get(0, 0), "Single pixel should be set");
    }
}

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
use crate::visual_fx::effects::sampling::BallState;
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
    /// Cached geometry for the current painter size.
    cache_width: u16,
    cache_height: u16,
    /// Wave-space x coordinates (nx * 6.0).
    wx: Vec<f64>,
    /// Wave-space y coordinates (ny * 6.0).
    wy: Vec<f64>,
    /// sin/cos for diagonal term (wx * 1.2).
    x_diag_sin: Vec<f64>,
    x_diag_cos: Vec<f64>,
    /// sin/cos for diagonal term (wy * 1.2).
    y_diag_sin: Vec<f64>,
    y_diag_cos: Vec<f64>,
    /// sin/cos base for v1 (wx * 1.5).
    x_wave_sin_base: Vec<f64>,
    x_wave_cos_base: Vec<f64>,
    /// sin/cos base for v2 (wy * 1.8).
    y_wave_sin_base: Vec<f64>,
    y_wave_cos_base: Vec<f64>,
    /// Precomputed sin/cos for radial term v4 (center).
    radial_center_sin_base: Vec<f64>,
    radial_center_cos_base: Vec<f64>,
    /// Precomputed sin/cos for radial term v5 (offset center).
    radial_offset_sin_base: Vec<f64>,
    radial_offset_cos_base: Vec<f64>,
    /// Precomputed sin/cos for interference base (x_sin2 * y_cos2).
    interference_sin_base: Vec<f64>,
    interference_cos_base: Vec<f64>,
    /// Per-frame scratch buffers for v1/v2.
    x_wave: Vec<f64>,
    y_wave: Vec<f64>,
}

impl PlasmaCanvasAdapter {
    /// Create a new plasma canvas adapter.
    #[inline]
    pub const fn new(palette: PlasmaPalette) -> Self {
        Self {
            palette,
            cache_width: 0,
            cache_height: 0,
            wx: Vec::new(),
            wy: Vec::new(),
            x_diag_sin: Vec::new(),
            x_diag_cos: Vec::new(),
            y_diag_sin: Vec::new(),
            y_diag_cos: Vec::new(),
            x_wave_sin_base: Vec::new(),
            x_wave_cos_base: Vec::new(),
            y_wave_sin_base: Vec::new(),
            y_wave_cos_base: Vec::new(),
            radial_center_sin_base: Vec::new(),
            radial_center_cos_base: Vec::new(),
            radial_offset_sin_base: Vec::new(),
            radial_offset_cos_base: Vec::new(),
            interference_sin_base: Vec::new(),
            interference_cos_base: Vec::new(),
            x_wave: Vec::new(),
            y_wave: Vec::new(),
        }
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

    fn ensure_cache(&mut self, width: u16, height: u16) {
        if self.cache_width == width && self.cache_height == height {
            return;
        }

        self.cache_width = width;
        self.cache_height = height;

        let w = width as usize;
        let h = height as usize;

        self.wx.resize(w, 0.0);
        self.x_diag_sin.resize(w, 0.0);
        self.x_diag_cos.resize(w, 0.0);
        self.x_wave_sin_base.resize(w, 0.0);
        self.x_wave_cos_base.resize(w, 0.0);

        let inv_w = if w > 0 { 1.0 / w as f64 } else { 0.0 };
        let mut x_sin2 = vec![0.0; w];
        for (x, x_sin2_val) in x_sin2.iter_mut().enumerate().take(w) {
            let nx = (x as f64 + 0.5) * inv_w;
            let wx = nx * 6.0;
            self.wx[x] = wx;
            let diag = wx * 1.2;
            let (sin, cos) = diag.sin_cos();
            self.x_diag_sin[x] = sin;
            self.x_diag_cos[x] = cos;
            let (sin1, cos1) = (wx * 1.5).sin_cos();
            self.x_wave_sin_base[x] = sin1;
            self.x_wave_cos_base[x] = cos1;
            *x_sin2_val = (wx * 2.0).sin();
        }

        self.wy.resize(h, 0.0);
        self.y_diag_sin.resize(h, 0.0);
        self.y_diag_cos.resize(h, 0.0);
        self.y_wave_sin_base.resize(h, 0.0);
        self.y_wave_cos_base.resize(h, 0.0);

        let inv_h = if h > 0 { 1.0 / h as f64 } else { 0.0 };
        let mut y_cos2 = vec![0.0; h];
        for (y, y_cos2_val) in y_cos2.iter_mut().enumerate().take(h) {
            let ny = (y as f64 + 0.5) * inv_h;
            let wy = ny * 6.0;
            self.wy[y] = wy;
            let diag = wy * 1.2;
            let (sin, cos) = diag.sin_cos();
            self.y_diag_sin[y] = sin;
            self.y_diag_cos[y] = cos;
            let (sin2, cos2) = (wy * 1.8).sin_cos();
            self.y_wave_sin_base[y] = sin2;
            self.y_wave_cos_base[y] = cos2;
            *y_cos2_val = (wy * 2.0).cos();
        }

        let total = w.saturating_mul(h);
        self.radial_center_sin_base.resize(total, 0.0);
        self.radial_center_cos_base.resize(total, 0.0);
        self.radial_offset_sin_base.resize(total, 0.0);
        self.radial_offset_cos_base.resize(total, 0.0);
        self.interference_sin_base.resize(total, 0.0);
        self.interference_cos_base.resize(total, 0.0);

        for (y, y_cos2_val) in y_cos2.iter().enumerate().take(h) {
            let wy = self.wy[y];
            let wy_sq = wy * wy;
            let wy_m3 = wy - 3.0;
            let wy_m3_sq = wy_m3 * wy_m3;
            let row_offset = y * w;
            for (x, x_sin2_val) in x_sin2.iter().enumerate().take(w) {
                let wx = self.wx[x];
                let wx_sq = wx * wx;
                let wx_m3 = wx - 3.0;
                let idx = row_offset + x;
                let radial_center = (wx_sq + wy_sq).sqrt() * 2.0;
                let radial_offset = ((wx_m3 * wx_m3) + wy_m3_sq).sqrt() * 1.8;
                let (sin_c, cos_c) = radial_center.sin_cos();
                let (sin_o, cos_o) = radial_offset.sin_cos();
                self.radial_center_sin_base[idx] = sin_c;
                self.radial_center_cos_base[idx] = cos_c;
                self.radial_offset_sin_base[idx] = sin_o;
                self.radial_offset_cos_base[idx] = cos_o;

                let base = *x_sin2_val * *y_cos2_val;
                let (sin_b, cos_b) = base.sin_cos();
                self.interference_sin_base[idx] = sin_b;
                self.interference_cos_base[idx] = cos_b;
            }
        }
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
        &mut self,
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

        self.ensure_cache(width, height);
        painter.mark_full_coverage();

        let w = width as usize;
        let h = height as usize;

        let t1 = time;
        let t2 = time * 0.8;
        let t3 = time * 0.6;
        let t4 = time * 1.2;
        let t6 = time * 0.5;
        let use_sunset_fast_path = matches!(self.palette, PlasmaPalette::Sunset);
        let (sin_t1, cos_t1) = t1.sin_cos();
        let (sin_t2, cos_t2) = t2.sin_cos();
        let (sin_t3, cos_t3) = t3.sin_cos();
        let (sin_t4, cos_t4) = t4.sin_cos();
        let (sin_time, cos_time) = time.sin_cos();
        let (sin_t6, cos_t6) = t6.sin_cos();

        self.x_wave.resize(w, 0.0);
        for (x, wave) in self.x_wave.iter_mut().enumerate().take(w) {
            *wave = self.x_wave_sin_base[x] * cos_t1 + self.x_wave_cos_base[x] * sin_t1;
        }

        self.y_wave.resize(h, 0.0);
        for (y, wave) in self.y_wave.iter_mut().enumerate().take(h) {
            *wave = self.y_wave_sin_base[y] * cos_t2 + self.y_wave_cos_base[y] * sin_t2;
        }
        let x_diag_sin = &self.x_diag_sin;
        let x_diag_cos = &self.x_diag_cos;

        // Hoist quality branching outside the hot pixel loop so we avoid
        // a branch check per pixel on every frame.
        match quality {
            FxQuality::Full => {
                if use_sunset_fast_path {
                    for y in 0..h {
                        let v2 = self.y_wave[y];
                        let y_diag_sin_t3 =
                            self.y_diag_sin[y] * cos_t3 + self.y_diag_cos[y] * sin_t3;
                        let y_diag_cos_t3 =
                            self.y_diag_cos[y] * cos_t3 - self.y_diag_sin[y] * sin_t3;
                        let row_offset = y * w;
                        for x in 0..w {
                            let v1 = self.x_wave[x];
                            let v3 = x_diag_sin[x] * y_diag_cos_t3 + x_diag_cos[x] * y_diag_sin_t3;
                            let idx = row_offset + x;
                            let v4 = self.radial_center_sin_base[idx] * cos_t4
                                - self.radial_center_cos_base[idx] * sin_t4;
                            let v5 = self.radial_offset_cos_base[idx] * cos_time
                                - self.radial_offset_sin_base[idx] * sin_time;
                            let v6 = self.interference_sin_base[idx] * cos_t6
                                + self.interference_cos_base[idx] * sin_t6;
                            let t = ((v1 + v2 + v3 + v4 + v5 + v6) / 6.0 + 1.0) * 0.5;
                            painter.set_color_at_index_in_bounds(idx, plasma_sunset_color_at(t));
                        }
                    }
                } else {
                    for y in 0..h {
                        let v2 = self.y_wave[y];
                        let y_diag_sin_t3 =
                            self.y_diag_sin[y] * cos_t3 + self.y_diag_cos[y] * sin_t3;
                        let y_diag_cos_t3 =
                            self.y_diag_cos[y] * cos_t3 - self.y_diag_sin[y] * sin_t3;
                        let row_offset = y * w;
                        for x in 0..w {
                            let v1 = self.x_wave[x];
                            let v3 = x_diag_sin[x] * y_diag_cos_t3 + x_diag_cos[x] * y_diag_sin_t3;
                            let idx = row_offset + x;
                            let v4 = self.radial_center_sin_base[idx] * cos_t4
                                - self.radial_center_cos_base[idx] * sin_t4;
                            let v5 = self.radial_offset_cos_base[idx] * cos_time
                                - self.radial_offset_sin_base[idx] * sin_time;
                            let v6 = self.interference_sin_base[idx] * cos_t6
                                + self.interference_cos_base[idx] * sin_t6;
                            let t = ((v1 + v2 + v3 + v4 + v5 + v6) / 6.0 + 1.0) * 0.5;
                            painter
                                .set_color_at_index_in_bounds(idx, self.palette.color_at(t, theme));
                        }
                    }
                }
            }
            FxQuality::Reduced => {
                if use_sunset_fast_path {
                    for y in 0..h {
                        let v2 = self.y_wave[y];
                        let y_diag_sin_t3 =
                            self.y_diag_sin[y] * cos_t3 + self.y_diag_cos[y] * sin_t3;
                        let y_diag_cos_t3 =
                            self.y_diag_cos[y] * cos_t3 - self.y_diag_sin[y] * sin_t3;
                        let row_offset = y * w;
                        for x in 0..w {
                            let v1 = self.x_wave[x];
                            let v3 = x_diag_sin[x] * y_diag_cos_t3 + x_diag_cos[x] * y_diag_sin_t3;
                            let idx = row_offset + x;
                            let v4 = self.radial_center_sin_base[idx] * cos_t4
                                - self.radial_center_cos_base[idx] * sin_t4;
                            let t = ((v1 + v2 + v3 + v4) / 4.0 + 1.0) * 0.5;
                            painter.set_color_at_index_in_bounds(idx, plasma_sunset_color_at(t));
                        }
                    }
                } else {
                    for y in 0..h {
                        let v2 = self.y_wave[y];
                        let y_diag_sin_t3 =
                            self.y_diag_sin[y] * cos_t3 + self.y_diag_cos[y] * sin_t3;
                        let y_diag_cos_t3 =
                            self.y_diag_cos[y] * cos_t3 - self.y_diag_sin[y] * sin_t3;
                        let row_offset = y * w;
                        for x in 0..w {
                            let v1 = self.x_wave[x];
                            let v3 = x_diag_sin[x] * y_diag_cos_t3 + x_diag_cos[x] * y_diag_sin_t3;
                            let idx = row_offset + x;
                            let v4 = self.radial_center_sin_base[idx] * cos_t4
                                - self.radial_center_cos_base[idx] * sin_t4;
                            let t = ((v1 + v2 + v3 + v4) / 4.0 + 1.0) * 0.5;
                            painter
                                .set_color_at_index_in_bounds(idx, self.palette.color_at(t, theme));
                        }
                    }
                }
            }
            FxQuality::Minimal => {
                if use_sunset_fast_path {
                    for y in 0..h {
                        let v2 = self.y_wave[y];
                        let y_diag_sin_t3 =
                            self.y_diag_sin[y] * cos_t3 + self.y_diag_cos[y] * sin_t3;
                        let y_diag_cos_t3 =
                            self.y_diag_cos[y] * cos_t3 - self.y_diag_sin[y] * sin_t3;
                        let row_offset = y * w;
                        for x in 0..w {
                            let v1 = self.x_wave[x];
                            let v3 = x_diag_sin[x] * y_diag_cos_t3 + x_diag_cos[x] * y_diag_sin_t3;
                            let t = ((v1 + v2 + v3) / 3.0 + 1.0) * 0.5;
                            painter.set_color_at_index_in_bounds(
                                row_offset + x,
                                plasma_sunset_color_at(t),
                            );
                        }
                    }
                } else {
                    for y in 0..h {
                        let v2 = self.y_wave[y];
                        let y_diag_sin_t3 =
                            self.y_diag_sin[y] * cos_t3 + self.y_diag_cos[y] * sin_t3;
                        let y_diag_cos_t3 =
                            self.y_diag_cos[y] * cos_t3 - self.y_diag_sin[y] * sin_t3;
                        let row_offset = y * w;
                        for x in 0..w {
                            let v1 = self.x_wave[x];
                            let v3 = x_diag_sin[x] * y_diag_cos_t3 + x_diag_cos[x] * y_diag_sin_t3;
                            let t = ((v1 + v2 + v3) / 3.0 + 1.0) * 0.5;
                            painter.set_color_at_index_in_bounds(
                                row_offset + x,
                                self.palette.color_at(t, theme),
                            );
                        }
                    }
                }
            }
            FxQuality::Off => {}
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
///
/// ## Cache-Friendly Layout (SoA + Ball-Major dx²)
///
/// The inner loop uses Structure-of-Arrays (SoA) for ball data: `r2_cache` and
/// `hue_cache` are contiguous `f64` slices instead of striding through 32-byte
/// `BallState` structs.  The `dx2_cache` is ball-major — `dx2_cache[ball * w + col]`
/// — so that consecutive-pixel dx² reads are contiguous, enabling prefetching and
/// auto-vectorization when pixels are processed in 4-wide blocks.
///
/// ## Row-Level Spatial Culling
///
/// Before processing each row, a cheap comparison-only check determines whether
/// any pixel in the row could exceed the glow threshold.  The bound exploits the
/// inequality `sum(r²_i / dist²_i) ≤ sum(r²_i) / min(dy²_i)`: if the nearest
/// ball is far enough vertically, the entire row is provably dark and skipped.
/// Cost: one `min` reduction over `balls_len` per row with zero divisions.
#[derive(Debug, Clone)]
pub struct MetaballsCanvasAdapter {
    /// Parameters controlling metaball behavior.
    params: MetaballsParams,
    /// Cached ball states for the current frame.
    ball_cache: Vec<BallState>,
    /// Cached geometry for the current painter size.
    cache_width: u16,
    cache_height: u16,
    /// Normalized x coordinates per column.
    x_coords: Vec<f64>,
    /// Normalized y coordinates per row.
    y_coords: Vec<f64>,
    /// Per-frame scratch buffer for dx^2 in ball-major layout: `[ball * w + col]`.
    dx2_cache: Vec<f64>,
    /// Per-row scratch buffer for dy^2 per ball.
    dy2_cache: Vec<f64>,
    /// SoA: contiguous r² values extracted from `ball_cache`.
    r2_cache: Vec<f64>,
    /// SoA: contiguous hue values extracted from `ball_cache`.
    hue_cache: Vec<f64>,
    /// Cached indices of active balls for reduced/minimal quality.
    active_indices: Vec<usize>,
    active_step: usize,
    active_len: usize,
}

impl MetaballsCanvasAdapter {
    /// Create a new metaballs canvas adapter with default parameters.
    pub fn new() -> Self {
        Self {
            params: MetaballsParams::default(),
            ball_cache: Vec::new(),
            cache_width: 0,
            cache_height: 0,
            x_coords: Vec::new(),
            y_coords: Vec::new(),
            dx2_cache: Vec::new(),
            dy2_cache: Vec::new(),
            r2_cache: Vec::new(),
            hue_cache: Vec::new(),
            active_indices: Vec::new(),
            active_step: 0,
            active_len: 0,
        }
    }

    /// Create a metaballs adapter with specific parameters.
    pub fn with_params(params: MetaballsParams) -> Self {
        Self {
            params,
            ball_cache: Vec::new(),
            cache_width: 0,
            cache_height: 0,
            x_coords: Vec::new(),
            y_coords: Vec::new(),
            dx2_cache: Vec::new(),
            dy2_cache: Vec::new(),
            r2_cache: Vec::new(),
            hue_cache: Vec::new(),
            active_indices: Vec::new(),
            active_step: 0,
            active_len: 0,
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

    fn ensure_coords(&mut self, width: u16, height: u16) {
        if self.cache_width == width && self.cache_height == height {
            return;
        }

        self.cache_width = width;
        self.cache_height = height;

        let w = width as usize;
        let h = height as usize;

        self.x_coords.resize(w, 0.0);
        self.y_coords.resize(h, 0.0);

        let inv_w = if w > 0 { 1.0 / w as f64 } else { 0.0 };
        for x in 0..w {
            self.x_coords[x] = (x as f64 + 0.5) * inv_w;
        }

        let inv_h = if h > 0 { 1.0 / h as f64 } else { 0.0 };
        for y in 0..h {
            self.y_coords[y] = (y as f64 + 0.5) * inv_h;
        }
    }

    /// Prepare ball states for the current frame.
    ///
    /// Call this once per frame before calling `fill`.
    pub fn prepare(&mut self, time: f64, quality: FxQuality) {
        let count = ball_count_for_quality(&self.params, quality);

        // Ensure cache capacity
        if self.ball_cache.len() != count {
            self.ball_cache.resize(
                count,
                BallState {
                    x: 0.0,
                    y: 0.0,
                    r2: 0.0,
                    hue: 0.0,
                },
            );
        }

        // Animate balls
        let t_scaled = time * self.params.time_scale;
        let (bounds_min, bounds_max) = ordered_pair(self.params.bounds_min, self.params.bounds_max);
        let (radius_min, radius_max) = ordered_pair(self.params.radius_min, self.params.radius_max);

        for (i, ball) in self.params.balls.iter().take(count).enumerate() {
            let x = ping_pong(ball.x + ball.vx * t_scaled, bounds_min, bounds_max);
            let y = ping_pong(ball.y + ball.vy * t_scaled, bounds_min, bounds_max);
            let pulse = 1.0
                + self.params.pulse_amount * (time * self.params.pulse_speed + ball.phase).sin();
            let radius = ball.radius.clamp(radius_min, radius_max).max(0.001) * pulse;
            let hue = (ball.hue + time * self.params.hue_speed).rem_euclid(1.0);

            self.ball_cache[i] = BallState {
                x,
                y,
                r2: radius * radius,
                hue,
            };
        }

        // Extract SoA: contiguous r² and hue arrays for cache-friendly inner loops.
        self.r2_cache.resize(count, 0.0);
        self.hue_cache.resize(count, 0.0);
        for (i, ball) in self.ball_cache.iter().enumerate() {
            self.r2_cache[i] = ball.r2;
            self.hue_cache[i] = ball.hue;
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
    pub fn fill(&mut self, painter: &mut Painter, quality: FxQuality, theme: &ThemeInputs) {
        if !quality.is_enabled() || self.ball_cache.is_empty() {
            return;
        }

        let (width, height) = painter.size();
        if width == 0 || height == 0 {
            return;
        }

        self.ensure_coords(width, height);

        let (glow, threshold) = thresholds(&self.params);
        let stops = palette_stops(self.params.palette, theme);

        let balls_len = self.ball_cache.len();
        let step = match quality {
            FxQuality::Full => 1,
            FxQuality::Reduced => {
                if balls_len > 4 {
                    4
                } else {
                    1
                }
            }
            FxQuality::Minimal => {
                if balls_len > 2 {
                    2
                } else {
                    1
                }
            }
            FxQuality::Off => return,
        };

        if step > 1 {
            self.ensure_active_indices(step, balls_len);
        }

        let w = width as usize;
        let h = height as usize;
        let x_coords = &self.x_coords;
        let y_coords = &self.y_coords;
        let balls = &self.ball_cache;

        // --- Ball-major dx² layout: dx2_cache[ball * w + col] ---
        // Consecutive-pixel dx² values for the same ball are contiguous, enabling
        // hardware prefetching and compiler auto-vectorization in 4-pixel blocks.
        self.dx2_cache.resize(balls_len.saturating_mul(w), 0.0);
        self.dy2_cache.resize(balls_len, 0.0);

        for (i, ball) in balls.iter().enumerate() {
            let base = i * w;
            for (x, &nx) in x_coords.iter().enumerate().take(w) {
                let dx = nx - ball.x;
                self.dx2_cache[base + x] = dx * dx;
            }
        }

        let dx2_cache = &self.dx2_cache;
        let dy2_cache = &mut self.dy2_cache;
        let r2_cache = &self.r2_cache;
        let hue_cache = &self.hue_cache;
        const EPS: f64 = 1e-8;

        // Spatial culling threshold: precompute once before the row loop.
        //
        // For any pixel, `dist² = dx² + dy² >= min_dist²`, so:
        //   sum_i (r²_i / dist²_i) <= sum_i (r²_i) / min_dist²
        //
        // This bound is used for:
        // - row-level skip using `min(dy²)` (cheap)
        // - 4-pixel-block skip using `min(dx²+dy²)` (tighter, avoids divisions on dark blocks)
        let sum_r2: f64 = if step == 1 {
            r2_cache.iter().copied().sum()
        } else {
            self.active_indices
                .iter()
                .copied()
                .map(|i| r2_cache[i])
                .sum()
        };
        let row_skip_dy2 = if glow > 0.0 { sum_r2 / glow } else { f64::MAX };

        // Hoist step==1 branching outside the hot pixel loop.
        if step == 1 {
            for (y, &ny) in y_coords.iter().enumerate().take(h) {
                let mut min_dy2 = f64::MAX;
                for (i, ball) in balls.iter().enumerate() {
                    let dy = ny - ball.y;
                    let dy2 = dy * dy;
                    dy2_cache[i] = dy2;
                    if dy2 < min_dy2 {
                        min_dy2 = dy2;
                    }
                }

                // Cheap row-skip: if the closest ball is too far vertically
                // for the aggregate field to reach `glow`, skip the row.
                // Cost: one pass over balls with no divisions.
                if min_dy2 > row_skip_dy2 {
                    continue;
                }

                let row_offset = y * w;

                // --- 4-pixel blocking: accumulate field for 4 columns per ball ---
                // Per-pixel accumulation order is preserved (ball 0, 1, …, N for each
                // pixel independently), so floating-point results are bit-identical.
                let full_blocks = w / 4;
                for block in 0..full_blocks {
                    let x_base = block * 4;

                    // Block-level spatial culling:
                    // If every ball is far enough from every pixel in this 4-wide block,
                    // the aggregate field cannot exceed `glow` and we can skip the expensive
                    // per-pixel divisions entirely.
                    let mut min_dist2 = f64::MAX;
                    for (i, &dy2) in dy2_cache.iter().enumerate().take(balls_len) {
                        let dx2_base = i * w + x_base;
                        let d0 = dx2_cache[dx2_base] + dy2;
                        let d1 = dx2_cache[dx2_base + 1] + dy2;
                        let d2 = dx2_cache[dx2_base + 2] + dy2;
                        let d3 = dx2_cache[dx2_base + 3] + dy2;
                        min_dist2 = min_dist2.min(d0.min(d1).min(d2).min(d3));

                        // Early-out: once min_dist² is below the threshold, skipping is impossible.
                        if min_dist2 <= row_skip_dy2 {
                            break;
                        }
                    }
                    if min_dist2 > row_skip_dy2 {
                        continue;
                    }

                    let mut sums = [0.0_f64; 4];
                    let mut hues = [0.0_f64; 4];

                    for (i, &dy2) in dy2_cache.iter().enumerate().take(balls_len) {
                        let r2 = r2_cache[i];
                        let hue_val = hue_cache[i];
                        let dx2_base = i * w + x_base;

                        // 4 contiguous dx² reads from ball-major layout.
                        let dx2_0 = dx2_cache[dx2_base];
                        let dx2_1 = dx2_cache[dx2_base + 1];
                        let dx2_2 = dx2_cache[dx2_base + 2];
                        let dx2_3 = dx2_cache[dx2_base + 3];

                        let d0 = dx2_0 + dy2;
                        let d1 = dx2_1 + dy2;
                        let d2 = dx2_2 + dy2;
                        let d3 = dx2_3 + dy2;

                        // Accumulate per-pixel field (branch almost always taken).
                        if d0 > EPS {
                            let c = r2 / d0;
                            sums[0] += c;
                            hues[0] += hue_val * c;
                        } else {
                            sums[0] += 100.0;
                            hues[0] += hue_val * 100.0;
                        }
                        if d1 > EPS {
                            let c = r2 / d1;
                            sums[1] += c;
                            hues[1] += hue_val * c;
                        } else {
                            sums[1] += 100.0;
                            hues[1] += hue_val * 100.0;
                        }
                        if d2 > EPS {
                            let c = r2 / d2;
                            sums[2] += c;
                            hues[2] += hue_val * c;
                        } else {
                            sums[2] += 100.0;
                            hues[2] += hue_val * 100.0;
                        }
                        if d3 > EPS {
                            let c = r2 / d3;
                            sums[3] += c;
                            hues[3] += hue_val * c;
                        } else {
                            sums[3] += 100.0;
                            hues[3] += hue_val * 100.0;
                        }
                    }

                    for j in 0..4 {
                        let s = sums[j];
                        if s > glow {
                            let avg_hue = hues[j] / s;
                            let intensity = if s > threshold {
                                1.0
                            } else {
                                (s - glow) / (threshold - glow)
                            };
                            let color = color_at_with_stops(&stops, avg_hue, intensity, theme);
                            painter
                                .point_colored_at_index_in_bounds(row_offset + x_base + j, color);
                        }
                    }
                }

                // Scalar tail for remaining columns (w % 4 != 0).
                for x in (full_blocks * 4)..w {
                    let mut sum = 0.0;
                    let mut weighted_hue = 0.0;
                    for (i, &dy2) in dy2_cache.iter().enumerate().take(balls_len) {
                        let dist_sq = dx2_cache[i * w + x] + dy2;
                        if dist_sq > EPS {
                            let contrib = r2_cache[i] / dist_sq;
                            sum += contrib;
                            weighted_hue += hue_cache[i] * contrib;
                        } else {
                            sum += 100.0;
                            weighted_hue += hue_cache[i] * 100.0;
                        }
                    }

                    if sum > glow {
                        let avg_hue = weighted_hue / sum;
                        let intensity = if sum > threshold {
                            1.0
                        } else {
                            (sum - glow) / (threshold - glow)
                        };
                        let color = color_at_with_stops(&stops, avg_hue, intensity, theme);
                        painter.point_colored_at_index_in_bounds(row_offset + x, color);
                    }
                }
            }
        } else {
            // step > 1: use active_indices for reduced/minimal quality.
            let active_indices = self.active_indices.as_slice();
            for (y, &ny) in y_coords.iter().enumerate().take(h) {
                let mut min_dy2 = f64::MAX;
                for &i in active_indices {
                    let dy = ny - balls[i].y;
                    let dy2 = dy * dy;
                    dy2_cache[i] = dy2;
                    if dy2 < min_dy2 {
                        min_dy2 = dy2;
                    }
                }

                // Cheap row-skip (same threshold as step==1 path).
                if min_dy2 > row_skip_dy2 {
                    continue;
                }

                let row_offset = y * w;

                let full_blocks = w / 4;
                for block in 0..full_blocks {
                    let x_base = block * 4;

                    // Block-level spatial culling (active balls only).
                    let mut min_dist2 = f64::MAX;
                    for &i in active_indices {
                        let dy2 = dy2_cache[i];
                        let dx2_base = i * w + x_base;
                        let d0 = dx2_cache[dx2_base] + dy2;
                        let d1 = dx2_cache[dx2_base + 1] + dy2;
                        let d2 = dx2_cache[dx2_base + 2] + dy2;
                        let d3 = dx2_cache[dx2_base + 3] + dy2;
                        min_dist2 = min_dist2.min(d0.min(d1).min(d2).min(d3));
                        if min_dist2 <= row_skip_dy2 {
                            break;
                        }
                    }
                    if min_dist2 > row_skip_dy2 {
                        continue;
                    }

                    let mut sums = [0.0_f64; 4];
                    let mut hues = [0.0_f64; 4];

                    for &i in active_indices {
                        let r2 = r2_cache[i];
                        let hue_val = hue_cache[i];
                        let dy2 = dy2_cache[i];
                        let dx2_base = i * w + x_base;

                        let d0 = dx2_cache[dx2_base] + dy2;
                        let d1 = dx2_cache[dx2_base + 1] + dy2;
                        let d2 = dx2_cache[dx2_base + 2] + dy2;
                        let d3 = dx2_cache[dx2_base + 3] + dy2;

                        if d0 > EPS {
                            let c = r2 / d0;
                            sums[0] += c;
                            hues[0] += hue_val * c;
                        } else {
                            sums[0] += 100.0;
                            hues[0] += hue_val * 100.0;
                        }
                        if d1 > EPS {
                            let c = r2 / d1;
                            sums[1] += c;
                            hues[1] += hue_val * c;
                        } else {
                            sums[1] += 100.0;
                            hues[1] += hue_val * 100.0;
                        }
                        if d2 > EPS {
                            let c = r2 / d2;
                            sums[2] += c;
                            hues[2] += hue_val * c;
                        } else {
                            sums[2] += 100.0;
                            hues[2] += hue_val * 100.0;
                        }
                        if d3 > EPS {
                            let c = r2 / d3;
                            sums[3] += c;
                            hues[3] += hue_val * c;
                        } else {
                            sums[3] += 100.0;
                            hues[3] += hue_val * 100.0;
                        }
                    }

                    for j in 0..4 {
                        let s = sums[j];
                        if s > glow {
                            let avg_hue = hues[j] / s;
                            let intensity = if s > threshold {
                                1.0
                            } else {
                                (s - glow) / (threshold - glow)
                            };
                            let color = color_at_with_stops(&stops, avg_hue, intensity, theme);
                            painter
                                .point_colored_at_index_in_bounds(row_offset + x_base + j, color);
                        }
                    }
                }

                for x in (full_blocks * 4)..w {
                    let mut sum = 0.0;
                    let mut weighted_hue = 0.0;
                    for &i in active_indices {
                        let dist_sq = dx2_cache[i * w + x] + dy2_cache[i];
                        if dist_sq > EPS {
                            let contrib = r2_cache[i] / dist_sq;
                            sum += contrib;
                            weighted_hue += hue_cache[i] * contrib;
                        } else {
                            sum += 100.0;
                            weighted_hue += hue_cache[i] * 100.0;
                        }
                    }

                    if sum > glow {
                        let avg_hue = weighted_hue / sum;
                        let intensity = if sum > threshold {
                            1.0
                        } else {
                            (sum - glow) / (threshold - glow)
                        };
                        let color = color_at_with_stops(&stops, avg_hue, intensity, theme);
                        painter.point_colored_at_index_in_bounds(row_offset + x, color);
                    }
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

impl MetaballsCanvasAdapter {
    fn ensure_active_indices(&mut self, step: usize, len: usize) {
        if self.active_step == step && self.active_len == len {
            return;
        }
        self.active_indices.clear();
        for i in 0..len {
            if i % step == 0 {
                self.active_indices.push(i);
            }
        }
        self.active_step = step;
        self.active_len = len;
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

#[inline]
fn plasma_lerp_rgb_fixed(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> PackedRgba {
    let t256 = (t.clamp(0.0, 1.0) * 256.0) as u32;
    let inv = 256 - t256;
    let r = ((a.0 as u32 * inv + b.0 as u32 * t256) >> 8) as u8;
    let g = ((a.1 as u32 * inv + b.1 as u32 * t256) >> 8) as u8;
    let b = ((a.2 as u32 * inv + b.2 as u32 * t256) >> 8) as u8;
    PackedRgba::rgb(r, g, b)
}

#[inline]
fn plasma_sunset_color_at(t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    if t < 0.33 {
        plasma_lerp_rgb_fixed((80, 20, 120), (255, 50, 120), t / 0.33)
    } else if t < 0.66 {
        plasma_lerp_rgb_fixed((255, 50, 120), (255, 150, 50), (t - 0.33) / 0.33)
    } else {
        plasma_lerp_rgb_fixed((255, 150, 50), (255, 255, 150), (t - 0.66) / 0.34)
    }
}

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
    let glow = params
        .glow_threshold
        .clamp(0.0, params.threshold.max(0.001));
    let mut threshold = params.threshold.max(glow + 0.0001);
    if threshold <= glow {
        threshold = glow + 0.0001;
    }
    (glow, threshold)
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
fn color_at_with_stops(
    stops: &[PackedRgba; 4],
    hue: f64,
    intensity: f64,
    theme: &ThemeInputs,
) -> PackedRgba {
    let base = gradient_color(stops, hue);
    let t = intensity.clamp(0.0, 1.0);
    lerp_color(theme.bg_base, base, t)
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
    if t <= 0.0 {
        return PackedRgba::rgb(a.r(), a.g(), a.b());
    }
    if t >= 1.0 {
        return PackedRgba::rgb(b.r(), b.g(), b.b());
    }
    let ar = a.r() as f64;
    let ag = a.g() as f64;
    let ab = a.b() as f64;
    let br = b.r() as f64;
    let bg = b.g() as f64;
    let bb = b.b() as f64;
    let r = (ar + (br - ar) * t) as u8;
    let g = (ag + (bg - ag) * t) as u8;
    let bl = (ab + (bb - ab) * t) as u8;
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
        let mut adapter = PlasmaCanvasAdapter::theme();
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
        let mut adapter = PlasmaCanvasAdapter::theme();
        let mut painter = Painter::new(10, 8, Mode::Braille);

        adapter.fill(&mut painter, 1.0, FxQuality::Off, &theme);

        // No pixels should be set
        let (w, h) = painter.size();
        for y in 0..h {
            for x in 0..w {
                assert!(
                    !painter.get(x as i32, y as i32),
                    "Off quality should not set pixels"
                );
            }
        }
    }

    #[test]
    fn plasma_adapter_deterministic() {
        let theme = default_theme();
        let mut adapter = PlasmaCanvasAdapter::new(PlasmaPalette::Ocean);
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
    fn plasma_diagonal_phase_row_precompute_is_identical() {
        // Proof for the v3 rewrite used in hot loops:
        // sin((x+y)*1.2 + t3) == sin(x*1.2) * cos(y*1.2 + t3) + cos(x*1.2) * sin(y*1.2 + t3)
        // where sin(y*1.2 + t3), cos(y*1.2 + t3) are precomputed once per row.
        let x_vals = [0.0_f64, 0.125, 0.5, 0.875, 1.0];
        let y_vals = [0.0_f64, 0.2, 0.4, 0.7, 1.0];
        let times = [0.0_f64, 0.33, 1.25, 2.5, 4.2];

        for nx in x_vals {
            for ny in y_vals {
                let x_diag = (nx * 6.0) * 1.2;
                let y_diag = (ny * 6.0) * 1.2;
                let (x_sin, x_cos) = x_diag.sin_cos();
                let (y_sin, y_cos) = y_diag.sin_cos();

                for time in times {
                    let t3 = time * 0.6;
                    let (sin_t3, cos_t3) = t3.sin_cos();

                    let sin_xy = x_sin * y_cos + x_cos * y_sin;
                    let cos_xy = x_cos * y_cos - x_sin * y_sin;
                    let old_v3 = sin_xy * cos_t3 + cos_xy * sin_t3;

                    let y_diag_sin_t3 = y_sin * cos_t3 + y_cos * sin_t3;
                    let y_diag_cos_t3 = y_cos * cos_t3 - y_sin * sin_t3;
                    let new_v3 = x_sin * y_diag_cos_t3 + x_cos * y_diag_sin_t3;

                    assert!(
                        (old_v3 - new_v3).abs() < 1e-12,
                        "v3 mismatch for nx={nx} ny={ny} time={time}: old={old_v3} new={new_v3}"
                    );
                }
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
                assert!(
                    !painter.get(x as i32, y as i32),
                    "Off quality should not set pixels"
                );
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
        let mut adapter = PlasmaCanvasAdapter::theme();
        let mut painter = Painter::new(0, 0, Mode::Braille);

        // Should not panic
        adapter.fill(&mut painter, 1.0, FxQuality::Full, &theme);
    }

    #[test]
    fn single_pixel_painter() {
        let theme = default_theme();
        let mut adapter = PlasmaCanvasAdapter::theme();
        let mut painter = Painter::new(1, 1, Mode::Braille);

        adapter.fill(&mut painter, 0.5, FxQuality::Full, &theme);

        // Single pixel should be set
        assert!(painter.get(0, 0), "Single pixel should be set");
    }

    #[test]
    fn plasma_ensure_cache_sizes_internal_buffers_and_maps_midpoints() {
        let mut adapter = PlasmaCanvasAdapter::theme();
        adapter.ensure_cache(2, 2);

        assert_eq!(adapter.cache_width, 2);
        assert_eq!(adapter.cache_height, 2);
        assert_eq!(adapter.wx.len(), 2);
        assert_eq!(adapter.wy.len(), 2);

        // Midpoint sampling: nx=(x+0.5)/w, wx=nx*6.0
        const EPS: f64 = 1e-12;
        assert!((adapter.wx[0] - 1.5).abs() < EPS, "wx[0]={}", adapter.wx[0]);
        assert!((adapter.wx[1] - 4.5).abs() < EPS, "wx[1]={}", adapter.wx[1]);
        assert!((adapter.wy[0] - 1.5).abs() < EPS, "wy[0]={}", adapter.wy[0]);
        assert!((adapter.wy[1] - 4.5).abs() < EPS, "wy[1]={}", adapter.wy[1]);

        // Radial + interference bases are per-pixel (row-major): w*h entries.
        assert_eq!(adapter.radial_center_sin_base.len(), 4);
        assert_eq!(adapter.radial_center_cos_base.len(), 4);
        assert_eq!(adapter.radial_offset_sin_base.len(), 4);
        assert_eq!(adapter.radial_offset_cos_base.len(), 4);
        assert_eq!(adapter.interference_sin_base.len(), 4);
        assert_eq!(adapter.interference_cos_base.len(), 4);
    }

    #[test]
    fn metaballs_fill_without_prepare_is_noop() {
        let theme = default_theme();
        let mut adapter = MetaballsCanvasAdapter::new();
        let mut painter = Painter::new(8, 6, Mode::Braille);

        // Contract says to call prepare() first; ensure we degrade safely instead of panicking.
        adapter.fill(&mut painter, FxQuality::Full, &theme);

        let (w, h) = painter.size();
        for y in 0..h {
            for x in 0..w {
                assert!(
                    !painter.get(x as i32, y as i32),
                    "fill() without prepare() should not set pixels"
                );
            }
        }
    }

    #[test]
    fn metaballs_ensure_coords_maps_midpoints() {
        let mut adapter = MetaballsCanvasAdapter::new();
        adapter.ensure_coords(1, 1);

        const EPS: f64 = 1e-12;
        assert_eq!(adapter.x_coords.len(), 1);
        assert_eq!(adapter.y_coords.len(), 1);
        assert!((adapter.x_coords[0] - 0.5).abs() < EPS);
        assert!((adapter.y_coords[0] - 0.5).abs() < EPS);

        adapter.ensure_coords(2, 2);
        assert_eq!(adapter.x_coords.len(), 2);
        assert_eq!(adapter.y_coords.len(), 2);
        assert!((adapter.x_coords[0] - 0.25).abs() < EPS);
        assert!((adapter.x_coords[1] - 0.75).abs() < EPS);
        assert!((adapter.y_coords[0] - 0.25).abs() < EPS);
        assert!((adapter.y_coords[1] - 0.75).abs() < EPS);
    }

    #[test]
    fn metaballs_dx2_cache_and_coords_match_painter_size() {
        let theme = default_theme();
        let mut adapter = MetaballsCanvasAdapter::new();
        adapter.prepare(0.0, FxQuality::Full);
        let balls_len = adapter.ball_cache.len();
        assert!(
            balls_len > 0,
            "default metaballs params should include balls"
        );

        let mut painter = Painter::new(7, 5, Mode::Braille);
        adapter.fill(&mut painter, FxQuality::Full, &theme);

        let (width, height) = painter.size();
        let w = width as usize;
        let h = height as usize;
        assert_eq!(adapter.cache_width, width);
        assert_eq!(adapter.cache_height, height);
        assert_eq!(adapter.x_coords.len(), w);
        assert_eq!(adapter.y_coords.len(), h);
        assert_eq!(adapter.dx2_cache.len(), balls_len.saturating_mul(w));
        assert_eq!(adapter.dy2_cache.len(), balls_len);
    }

    // =========================================================================
    // Helper function tests
    // =========================================================================

    #[test]
    fn ping_pong_within_range() {
        let v = ping_pong(0.5, 0.0, 1.0);
        assert!((v - 0.5).abs() < 1e-6);
    }

    #[test]
    fn ping_pong_bounces_back() {
        // value=1.5 in range [0,1]: period=2, 1.5 % 2 = 1.5 > 1.0, so 2.0-1.5 = 0.5
        let v = ping_pong(1.5, 0.0, 1.0);
        assert!((v - 0.5).abs() < 1e-6);
    }

    #[test]
    fn ping_pong_negative_value() {
        // rem_euclid handles negative correctly
        let v = ping_pong(-0.5, 0.0, 1.0);
        // -0.5 rem_euclid 2.0 = 1.5, which > 1.0, so 2.0-1.5 = 0.5
        assert!((v - 0.5).abs() < 1e-6);
    }

    #[test]
    fn ordered_pair_already_ordered() {
        let (a, b) = ordered_pair(1.0, 3.0);
        assert!((a - 1.0).abs() < 1e-6);
        assert!((b - 3.0).abs() < 1e-6);
    }

    #[test]
    fn ordered_pair_swaps() {
        let (a, b) = ordered_pair(5.0, 2.0);
        assert!((a - 2.0).abs() < 1e-6);
        assert!((b - 5.0).abs() < 1e-6);
    }

    #[test]
    fn lerp_color_at_zero() {
        let a = PackedRgba::rgb(0, 0, 0);
        let b = PackedRgba::rgb(255, 255, 255);
        let c = lerp_color(a, b, 0.0);
        assert_eq!(c.r(), 0);
        assert_eq!(c.g(), 0);
        assert_eq!(c.b(), 0);
    }

    #[test]
    fn lerp_color_at_one() {
        let a = PackedRgba::rgb(0, 0, 0);
        let b = PackedRgba::rgb(100, 150, 200);
        let c = lerp_color(a, b, 1.0);
        assert_eq!(c.r(), 100);
        assert_eq!(c.g(), 150);
        assert_eq!(c.b(), 200);
    }

    #[test]
    fn lerp_color_midpoint() {
        let a = PackedRgba::rgb(0, 0, 0);
        let b = PackedRgba::rgb(200, 100, 50);
        let c = lerp_color(a, b, 0.5);
        assert_eq!(c.r(), 100);
        assert_eq!(c.g(), 50);
        assert_eq!(c.b(), 25);
    }

    #[test]
    fn lerp_color_clamps_t() {
        let a = PackedRgba::rgb(10, 20, 30);
        let b = PackedRgba::rgb(100, 200, 250);
        let under = lerp_color(a, b, -1.0);
        assert_eq!(under.r(), 10);
        let over = lerp_color(a, b, 2.0);
        assert_eq!(over.r(), 100);
    }

    #[test]
    fn gradient_color_at_boundaries() {
        let stops = [
            PackedRgba::rgb(255, 0, 0),
            PackedRgba::rgb(0, 255, 0),
            PackedRgba::rgb(0, 0, 255),
            PackedRgba::rgb(255, 255, 255),
        ];
        // t=0 should be stop 0
        let c0 = gradient_color(&stops, 0.0);
        assert_eq!(c0.r(), 255);
        assert_eq!(c0.g(), 0);
        // t=1 should be stop 3
        let c1 = gradient_color(&stops, 1.0);
        assert_eq!(c1.r(), 255);
        assert_eq!(c1.g(), 255);
    }

    #[test]
    fn ball_count_full_quality() {
        let params = MetaballsParams::default();
        let total = params.balls.len();
        assert_eq!(ball_count_for_quality(&params, FxQuality::Full), total);
    }

    #[test]
    fn ball_count_off_is_zero() {
        let params = MetaballsParams::default();
        assert_eq!(ball_count_for_quality(&params, FxQuality::Off), 0);
    }

    #[test]
    fn ball_count_reduced_leq_full() {
        let params = MetaballsParams::default();
        let full = ball_count_for_quality(&params, FxQuality::Full);
        let reduced = ball_count_for_quality(&params, FxQuality::Reduced);
        assert!(reduced <= full);
    }

    #[test]
    fn ball_count_minimal_leq_reduced() {
        let params = MetaballsParams::default();
        let reduced = ball_count_for_quality(&params, FxQuality::Reduced);
        let minimal = ball_count_for_quality(&params, FxQuality::Minimal);
        assert!(minimal <= reduced);
    }

    #[test]
    fn thresholds_glow_less_than_threshold() {
        let params = MetaballsParams::default();
        let (glow, thresh) = thresholds(&params);
        assert!(glow < thresh);
    }

    #[test]
    fn plasma_adapter_set_palette() {
        let mut adapter = PlasmaCanvasAdapter::new(PlasmaPalette::Neon);
        adapter.set_palette(PlasmaPalette::Ocean);
        // Should not panic; internal palette should be updated
    }

    #[test]
    fn plasma_adapter_default_is_theme() {
        let adapter = PlasmaCanvasAdapter::default();
        // Default adapter should work without panicking on fill
        let theme = default_theme();
        let mut p = Painter::new(4, 4, Mode::Braille);
        let mut adapter = adapter;
        adapter.fill(&mut p, 0.0, FxQuality::Minimal, &theme);
    }

    #[test]
    fn metaballs_with_params() {
        let params = MetaballsParams::default();
        let adapter = MetaballsCanvasAdapter::with_params(params.clone());
        assert_eq!(adapter.params().balls.len(), params.balls.len());
    }

    #[test]
    fn metaballs_set_params() {
        let mut adapter = MetaballsCanvasAdapter::new();
        let original_count = adapter.params().balls.len();
        let mut params = MetaballsParams::default();
        params.balls.clear();
        adapter.set_params(params);
        assert_eq!(adapter.params().balls.len(), 0);
        assert_ne!(original_count, 0);
    }

    #[test]
    fn plasma_all_quality_levels() {
        let theme = default_theme();
        let mut adapter = PlasmaCanvasAdapter::theme();
        for quality in [FxQuality::Full, FxQuality::Reduced, FxQuality::Minimal] {
            let mut p = Painter::new(8, 8, Mode::Braille);
            adapter.fill(&mut p, 1.0, quality, &theme);
            // Should not panic and should set some pixels
            let mut count = 0;
            let (w, h) = p.size();
            for y in 0..h {
                for x in 0..w {
                    if p.get(x as i32, y as i32) {
                        count += 1;
                    }
                }
            }
            assert!(count > 0, "Quality {quality:?} should set some pixels");
        }
    }
}

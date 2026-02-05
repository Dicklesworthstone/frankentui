#![forbid(unsafe_code)]

//! Metaballs backdrop effect (cell-space).
//!
//! Deterministic, no-allocation (steady state), and theme-aware.

use super::sampling::fill_normalized_coords;
#[cfg(feature = "fx-gpu")]
use crate::visual_fx::gpu;
use crate::visual_fx::{BackdropFx, FxContext, FxQuality, ThemeInputs};
use ftui_render::cell::PackedRgba;

/// Single metaball definition (normalized coordinates).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metaball {
    /// Base x position in [0, 1].
    pub x: f64,
    /// Base y position in [0, 1].
    pub y: f64,
    /// Velocity along x (units per simulated frame).
    pub vx: f64,
    /// Velocity along y (units per simulated frame).
    pub vy: f64,
    /// Base radius in normalized space.
    pub radius: f64,
    /// Base hue in [0, 1].
    pub hue: f64,
    /// Phase offset for pulsing.
    pub phase: f64,
}

/// Theme-aware palette presets for metaballs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetaballsPalette {
    /// Base gradient using theme primary + secondary accents.
    ThemeAccents,
    /// Aurora-like: use accent slots for a cooler gradient.
    Aurora,
    /// Lava-like: warmer gradient derived from theme accents.
    Lava,
    /// Ocean-like: cooler gradient derived from theme accents.
    Ocean,
}

impl MetaballsPalette {
    fn stops(self, theme: &ThemeInputs) -> [PackedRgba; 4] {
        match self {
            Self::ThemeAccents => [
                theme.bg_surface,
                theme.accent_primary,
                theme.accent_secondary,
                theme.fg_primary,
            ],
            Self::Aurora => [
                theme.accent_slots[0],
                theme.accent_primary,
                theme.accent_slots[1],
                theme.accent_secondary,
            ],
            Self::Lava => [
                theme.accent_slots[2],
                theme.accent_secondary,
                theme.accent_primary,
                theme.accent_slots[3],
            ],
            Self::Ocean => [
                theme.accent_primary,
                theme.accent_slots[3],
                theme.accent_slots[0],
                theme.fg_primary,
            ],
        }
    }

    #[allow(dead_code)]
    #[inline]
    fn color_at(self, hue: f64, intensity: f64, theme: &ThemeInputs) -> PackedRgba {
        let stops = self.stops(theme);
        let base = gradient_color(&stops, hue);
        let t = intensity.clamp(0.0, 1.0);
        lerp_color(theme.bg_base, base, t)
    }
}

/// Parameters controlling metaballs behavior.
#[derive(Debug, Clone)]
pub struct MetaballsParams {
    pub balls: Vec<Metaball>,
    pub palette: MetaballsPalette,
    /// Threshold for full intensity.
    pub threshold: f64,
    /// Threshold for glow ramp start.
    pub glow_threshold: f64,
    /// Pulse amplitude applied to radii.
    pub pulse_amount: f64,
    /// Pulse speed (radians per second).
    pub pulse_speed: f64,
    /// Hue drift speed (turns per second).
    pub hue_speed: f64,
    /// Time scaling to approximate a 60 FPS update step.
    pub time_scale: f64,
    /// Bounds for metaball motion (normalized).
    pub bounds_min: f64,
    pub bounds_max: f64,
    /// Radius clamp (normalized).
    pub radius_min: f64,
    pub radius_max: f64,
}

impl Default for MetaballsParams {
    fn default() -> Self {
        Self {
            balls: vec![
                Metaball {
                    x: 0.3,
                    y: 0.4,
                    vx: 0.012,
                    vy: 0.009,
                    radius: 0.20,
                    hue: 0.0,
                    phase: 0.0,
                },
                Metaball {
                    x: 0.7,
                    y: 0.6,
                    vx: -0.010,
                    vy: 0.013,
                    radius: 0.17,
                    hue: 0.2,
                    phase: 0.9,
                },
                Metaball {
                    x: 0.5,
                    y: 0.3,
                    vx: 0.009,
                    vy: -0.011,
                    radius: 0.22,
                    hue: 0.4,
                    phase: 1.8,
                },
                Metaball {
                    x: 0.2,
                    y: 0.7,
                    vx: -0.013,
                    vy: -0.008,
                    radius: 0.14,
                    hue: 0.6,
                    phase: 2.7,
                },
                Metaball {
                    x: 0.8,
                    y: 0.2,
                    vx: 0.007,
                    vy: 0.011,
                    radius: 0.18,
                    hue: 0.8,
                    phase: 3.6,
                },
                Metaball {
                    x: 0.4,
                    y: 0.8,
                    vx: -0.009,
                    vy: -0.010,
                    radius: 0.16,
                    hue: 0.1,
                    phase: 4.5,
                },
                Metaball {
                    x: 0.6,
                    y: 0.5,
                    vx: 0.011,
                    vy: -0.009,
                    radius: 0.19,
                    hue: 0.5,
                    phase: 5.4,
                },
            ],
            palette: MetaballsPalette::ThemeAccents,
            threshold: 1.0,
            glow_threshold: 0.6,
            pulse_amount: 0.22,
            pulse_speed: 2.8,
            hue_speed: 0.10,
            time_scale: 60.0,
            bounds_min: 0.05,
            bounds_max: 0.95,
            radius_min: 0.08,
            radius_max: 0.25,
        }
    }
}

impl MetaballsParams {
    #[inline]
    pub fn aurora() -> Self {
        Self {
            palette: MetaballsPalette::Aurora,
            ..Self::default()
        }
    }

    #[inline]
    pub fn lava() -> Self {
        Self {
            palette: MetaballsPalette::Lava,
            ..Self::default()
        }
    }

    #[inline]
    pub fn ocean() -> Self {
        Self {
            palette: MetaballsPalette::Ocean,
            ..Self::default()
        }
    }

    fn ball_count_for_quality(&self, quality: FxQuality) -> usize {
        let total = self.balls.len();
        if total == 0 {
            return 0;
        }
        match quality {
            FxQuality::Full => total,
            FxQuality::Reduced => total.saturating_sub(total / 4).max(4).min(total),
            FxQuality::Minimal => total.saturating_sub(total / 2).max(3).min(total),
            FxQuality::Off => 0, // No balls rendered when off
        }
    }

    fn thresholds(&self) -> (f64, f64) {
        let glow = self.glow_threshold.clamp(0.0, self.threshold.max(0.001));
        let mut threshold = self.threshold.max(glow + 0.0001);
        if threshold <= glow {
            threshold = glow + 0.0001;
        }
        (glow, threshold)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct BallSample {
    x: f64,
    y: f64,
    r2: f64,
    hue: f64,
}

/// Metaballs backdrop effect.
#[derive(Debug, Clone)]
pub struct MetaballsFx {
    params: MetaballsParams,
    x_coords: Vec<f64>,
    y_coords: Vec<f64>,
    ball_cache: Vec<BallSample>,
    #[cfg(feature = "fx-gpu")]
    gpu_ball_cache: Vec<gpu::GpuBall>,
}

impl MetaballsFx {
    /// Create a new metaballs effect with parameters.
    #[inline]
    pub fn new(params: MetaballsParams) -> Self {
        Self {
            params,
            x_coords: Vec::new(),
            y_coords: Vec::new(),
            ball_cache: Vec::new(),
            #[cfg(feature = "fx-gpu")]
            gpu_ball_cache: Vec::new(),
        }
    }

    /// Create a metaballs effect with default parameters.
    #[inline]
    pub fn default_theme() -> Self {
        Self::new(MetaballsParams::default())
    }

    /// Replace parameters (keeps caches).
    pub fn set_params(&mut self, params: MetaballsParams) {
        self.params = params;
    }

    fn ensure_coords(&mut self, width: u16, height: u16) {
        let w = width as usize;
        let h = height as usize;
        if w != self.x_coords.len() {
            self.x_coords.resize(w, 0.0);
            fill_normalized_coords(width, &mut self.x_coords);
        }
        if h != self.y_coords.len() {
            self.y_coords.resize(h, 0.0);
            fill_normalized_coords(height, &mut self.y_coords);
        }
    }

    fn ensure_ball_cache(&mut self, count: usize) {
        if self.ball_cache.len() != count {
            self.ball_cache.resize(count, BallSample::default());
        }
    }

    #[cfg(feature = "fx-gpu")]
    fn sync_gpu_ball_cache(&mut self) {
        if self.gpu_ball_cache.len() != self.ball_cache.len() {
            self.gpu_ball_cache
                .resize(self.ball_cache.len(), gpu::GpuBall::default());
        }
        for (dst, src) in self.gpu_ball_cache.iter_mut().zip(self.ball_cache.iter()) {
            *dst = gpu::GpuBall {
                x: src.x as f32,
                y: src.y as f32,
                r2: src.r2 as f32,
                hue: src.hue as f32,
            };
        }
    }

    fn populate_ball_cache(&mut self, time: f64, quality: FxQuality) {
        let count = self.params.ball_count_for_quality(quality);
        self.ensure_ball_cache(count);

        let t_scaled = time * self.params.time_scale;
        let (bounds_min, bounds_max) = ordered_pair(self.params.bounds_min, self.params.bounds_max);
        let (radius_min, radius_max) = ordered_pair(self.params.radius_min, self.params.radius_max);
        let pulse_amount = self.params.pulse_amount;
        let pulse_speed = self.params.pulse_speed;
        let hue_speed = self.params.hue_speed;

        for (slot, ball) in self
            .ball_cache
            .iter_mut()
            .zip(self.params.balls.iter().take(count))
        {
            let x = ping_pong(ball.x + ball.vx * t_scaled, bounds_min, bounds_max);
            let y = ping_pong(ball.y + ball.vy * t_scaled, bounds_min, bounds_max);
            let pulse = 1.0 + pulse_amount * (time * pulse_speed + ball.phase).sin();
            let radius = ball.radius.clamp(radius_min, radius_max).max(0.001) * pulse;
            let hue = (ball.hue + time * hue_speed).rem_euclid(1.0);

            *slot = BallSample {
                x,
                y,
                r2: radius * radius,
                hue,
            };
        }
    }
}

impl Default for MetaballsFx {
    fn default() -> Self {
        Self::default_theme()
    }
}

impl BackdropFx for MetaballsFx {
    fn name(&self) -> &'static str {
        "metaballs"
    }

    fn resize(&mut self, width: u16, height: u16) {
        if width == 0 || height == 0 {
            self.x_coords.clear();
            self.y_coords.clear();
            return;
        }
        self.ensure_coords(width, height);
    }

    fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
        // Early return if quality is Off (decorative effects are non-essential)
        if !ctx.quality.is_enabled() || ctx.is_empty() {
            return;
        }
        debug_assert_eq!(out.len(), ctx.len());

        self.ensure_coords(ctx.width, ctx.height);
        self.populate_ball_cache(ctx.time_seconds, ctx.quality);

        let (glow, threshold) = self.params.thresholds();
        let eps = 0.0001;

        // Hoist palette stops and bg_base outside the pixel loop to avoid
        // recomputing per-pixel. The stops array and bg_base are constant
        // for the entire frame.
        let stops = self.params.palette.stops(ctx.theme);
        let bg_base = ctx.theme.bg_base;

        #[cfg(feature = "fx-gpu")]
        if gpu::gpu_enabled() {
            self.sync_gpu_ball_cache();
            if gpu::render_metaballs(
                ctx,
                glow,
                threshold,
                bg_base,
                stops,
                &self.gpu_ball_cache,
                out,
            ) {
                return;
            }
        }

        let inv_threshold_range = 1.0 / (threshold - glow);

        for dy in 0..ctx.height {
            let ny = self.y_coords[dy as usize];
            for dx in 0..ctx.width {
                let idx = dy as usize * ctx.width as usize + dx as usize;
                let nx = self.x_coords[dx as usize];

                let mut sum = 0.0;
                let mut weighted_hue = 0.0;
                let mut total_weight = 0.0;

                for ball in &self.ball_cache {
                    let dx = nx - ball.x;
                    let dy = ny - ball.y;
                    let dist_sq = dx * dx + dy * dy;
                    if dist_sq > eps {
                        let contrib = ball.r2 / dist_sq;
                        sum += contrib;
                        weighted_hue += ball.hue * contrib;
                        total_weight += contrib;
                    } else {
                        sum += 100.0;
                        weighted_hue += ball.hue * 100.0;
                        total_weight += 100.0;
                    }
                }

                if sum > glow {
                    let avg_hue = if total_weight > 0.0 {
                        weighted_hue / total_weight
                    } else {
                        0.0
                    };

                    let intensity = if sum > threshold {
                        1.0
                    } else {
                        // Smooth-step easing for organic glow falloff
                        let t = (sum - glow) * inv_threshold_range;
                        t * t * (3.0 - 2.0 * t)
                    };

                    // Inline color_at: gradient_color + lerp with bg_base
                    let base = gradient_color(&stops, avg_hue);
                    out[idx] = lerp_color(bg_base, base, intensity);
                } else {
                    out[idx] = PackedRgba::TRANSPARENT;
                }
            }
        }
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

/// Fixed-point color lerp using u32 arithmetic (avoids f64 per channel).
///
/// t is clamped to [0.0, 1.0] and scaled to 0..256 for 8.8 fixed-point blending.
#[inline]
fn lerp_color(a: PackedRgba, b: PackedRgba, t: f64) -> PackedRgba {
    // Convert t to 0..256 fixed-point (8 fractional bits)
    let t256 = (t.clamp(0.0, 1.0) * 256.0) as u32;
    let inv = 256 - t256;
    let r = ((a.r() as u32 * inv + b.r() as u32 * t256) >> 8) as u8;
    let g = ((a.g() as u32 * inv + b.g() as u32 * t256) >> 8) as u8;
    let bl = ((a.b() as u32 * inv + b.b() as u32 * t256) >> 8) as u8;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "fx-gpu")]
    use std::sync::Mutex;

    #[cfg(feature = "fx-gpu")]
    static GPU_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn ctx(theme: &ThemeInputs) -> FxContext<'_> {
        FxContext {
            width: 24,
            height: 12,
            frame: 1,
            time_seconds: 1.25,
            quality: FxQuality::Full,
            theme,
        }
    }

    fn hash_pixels(pixels: &[PackedRgba]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for px in pixels {
            hash ^= px.0 as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn field_sum_at(fx: &MetaballsFx, x: f64, y: f64) -> f64 {
        let eps = 0.0001;
        let mut sum = 0.0;
        for ball in &fx.ball_cache {
            let dx = x - ball.x;
            let dy = y - ball.y;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq > eps {
                sum += ball.r2 / dist_sq;
            } else {
                sum += 100.0;
            }
        }
        sum
    }

    #[test]
    fn field_intensity_crosses_thresholds() {
        let params = MetaballsParams {
            balls: vec![Metaball {
                x: 0.5,
                y: 0.5,
                vx: 0.0,
                vy: 0.0,
                radius: 0.2,
                hue: 0.0,
                phase: 0.0,
            }],
            glow_threshold: 0.6,
            threshold: 1.0,
            pulse_amount: 0.0,
            ..Default::default()
        };
        let mut fx = MetaballsFx::new(params);
        fx.populate_ball_cache(0.0, FxQuality::Full);

        let center = field_sum_at(&fx, 0.5, 0.5);
        let far = field_sum_at(&fx, 0.9, 0.9);

        assert!(center > 1.0, "center intensity should exceed threshold");
        assert!(far < 0.6, "far intensity should be below glow threshold");
    }

    #[test]
    fn ball_cache_respects_bounds_and_radius_clamp() {
        let params = MetaballsParams {
            balls: vec![Metaball {
                x: 0.95,
                y: 0.05,
                vx: 0.5,
                vy: -0.4,
                radius: 1.0,
                hue: 0.0,
                phase: 0.0,
            }],
            bounds_min: 0.2,
            bounds_max: 0.8,
            radius_min: 0.1,
            radius_max: 0.2,
            pulse_amount: 0.0,
            ..Default::default()
        };
        let mut fx = MetaballsFx::new(params);
        fx.populate_ball_cache(1.0, FxQuality::Full);
        let ball = fx.ball_cache[0];

        assert!(
            ball.x >= 0.2 && ball.x <= 0.8,
            "x out of bounds: {}",
            ball.x
        );
        assert!(
            ball.y >= 0.2 && ball.y <= 0.8,
            "y out of bounds: {}",
            ball.y
        );

        let expected_r2 = 0.2 * 0.2;
        assert!(
            (ball.r2 - expected_r2).abs() < 1e-6,
            "radius clamp failed: r2={}, expected {}",
            ball.r2,
            expected_r2
        );
    }

    #[test]
    fn hue_wraps_into_unit_interval() {
        let params = MetaballsParams {
            balls: vec![Metaball {
                x: 0.5,
                y: 0.5,
                vx: 0.0,
                vy: 0.0,
                radius: 0.2,
                hue: 0.95,
                phase: 0.0,
            }],
            hue_speed: 0.2,
            pulse_amount: 0.0,
            ..Default::default()
        };
        let mut fx = MetaballsFx::new(params);
        fx.populate_ball_cache(1.0, FxQuality::Full);
        let hue = fx.ball_cache[0].hue;
        assert!(
            (0.0..=1.0).contains(&hue),
            "hue should wrap into [0,1], got {}",
            hue
        );
    }

    #[test]
    fn ball_cache_deterministic_for_fixed_time() {
        let mut fx = MetaballsFx::default();
        fx.populate_ball_cache(0.42, FxQuality::Full);
        let first = fx.ball_cache.clone();
        fx.populate_ball_cache(0.42, FxQuality::Full);
        let second = fx.ball_cache.clone();
        assert_eq!(first, second);
    }

    #[test]
    fn deterministic_for_fixed_inputs() {
        #[cfg(feature = "fx-gpu")]
        let _guard = GPU_TEST_LOCK.lock().expect("gpu test lock poisoned");

        let theme = ThemeInputs::default_dark();
        let mut fx = MetaballsFx::default();
        let ctx = ctx(&theme);
        let mut out1 = vec![PackedRgba::TRANSPARENT; ctx.len()];
        let mut out2 = vec![PackedRgba::TRANSPARENT; ctx.len()];
        #[cfg(feature = "fx-gpu")]
        gpu::force_disable_for_tests();
        fx.render(ctx, &mut out1);
        fx.render(ctx, &mut out2);
        #[cfg(feature = "fx-gpu")]
        gpu::reset_for_tests();
        let h1 = hash_pixels(&out1);
        let h2 = hash_pixels(&out2);
        assert_eq!(out1, out2, "hash1={h1:#x} hash2={h2:#x}");
    }

    #[test]
    fn field_sum_monotonic_with_distance() {
        let params = MetaballsParams {
            balls: vec![Metaball {
                x: 0.5,
                y: 0.5,
                vx: 0.0,
                vy: 0.0,
                radius: 0.2,
                hue: 0.1,
                phase: 0.0,
            }],
            ..Default::default()
        };
        let mut fx = MetaballsFx::new(params);
        fx.populate_ball_cache(0.0, FxQuality::Full);

        let center = field_sum_at(&fx, 0.5, 0.5);
        let near = field_sum_at(&fx, 0.55, 0.5);
        let far = field_sum_at(&fx, 0.8, 0.5);

        assert!(center > near, "Field should decrease with distance");
        assert!(near > far, "Field should decrease with distance");
    }

    #[test]
    fn high_threshold_yields_transparent_output() {
        let theme = ThemeInputs::default_dark();
        let params = MetaballsParams {
            balls: vec![Metaball {
                x: 0.5,
                y: 0.5,
                vx: 0.0,
                vy: 0.0,
                radius: 0.2,
                hue: 0.1,
                phase: 0.0,
            }],
            glow_threshold: 999.0,
            threshold: 1000.0,
            ..Default::default()
        };

        let mut fx = MetaballsFx::new(params);
        let ctx = FxContext {
            width: 8,
            height: 4,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out);

        assert!(
            out.iter().all(|&px| px == PackedRgba::TRANSPARENT),
            "High thresholds should yield transparent output"
        );
    }

    #[test]
    fn low_threshold_yields_visible_output() {
        let theme = ThemeInputs::default_dark();
        let params = MetaballsParams {
            balls: vec![Metaball {
                x: 0.5,
                y: 0.5,
                vx: 0.0,
                vy: 0.0,
                radius: 0.2,
                hue: 0.1,
                phase: 0.0,
            }],
            glow_threshold: 0.0,
            threshold: 0.1,
            ..Default::default()
        };

        let mut fx = MetaballsFx::new(params);
        let ctx = FxContext {
            width: 8,
            height: 4,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out);

        assert!(
            out.iter().any(|&px| px != PackedRgba::TRANSPARENT),
            "Low thresholds should yield visible output"
        );
    }

    #[test]
    fn tiny_area_safe() {
        let theme = ThemeInputs::default_dark();
        let mut fx = MetaballsFx::default();
        let ctx = FxContext {
            width: 0,
            height: 10,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Minimal,
            theme: &theme,
        };
        fx.render(ctx, &mut []);
    }

    #[test]
    fn tiny_area_safe_small_dims() {
        let theme = ThemeInputs::default_dark();
        let mut fx = MetaballsFx::default();
        for (width, height) in [(1, 1), (2, 1), (1, 2), (2, 2)] {
            let ctx = FxContext {
                width,
                height,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Minimal,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
            fx.render(ctx, &mut out);
        }
    }

    #[test]
    fn buffer_cache_stable_for_same_size() {
        let theme = ThemeInputs::default_dark();
        let mut fx = MetaballsFx::default();
        let ctx = ctx(&theme);
        fx.resize(ctx.width, ctx.height);
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out);
        let cap_x = fx.x_coords.capacity();
        let cap_y = fx.y_coords.capacity();
        let cap_balls = fx.ball_cache.capacity();
        fx.render(ctx, &mut out);

        assert_eq!(cap_x, fx.x_coords.capacity());
        assert_eq!(cap_y, fx.y_coords.capacity());
        assert_eq!(cap_balls, fx.ball_cache.capacity());
    }

    #[test]
    fn quality_reduces_ball_count() {
        let mut fx = MetaballsFx::default();
        fx.populate_ball_cache(0.0, FxQuality::Full);
        let full = fx.ball_cache.len();
        fx.populate_ball_cache(0.0, FxQuality::Reduced);
        let reduced = fx.ball_cache.len();
        fx.populate_ball_cache(0.0, FxQuality::Minimal);
        let minimal = fx.ball_cache.len();

        assert!(reduced <= full);
        assert!(minimal <= reduced);
    }

    #[test]
    fn thresholds_enforce_gap_and_order() {
        let params = MetaballsParams {
            threshold: 0.05,
            glow_threshold: 0.1,
            ..Default::default()
        };
        let (glow, threshold) = params.thresholds();
        assert!(glow <= threshold);
        assert!(threshold > glow, "threshold should exceed glow");
    }

    #[test]
    fn ordered_pair_sorts_values() {
        assert_eq!(ordered_pair(1.0, 2.0), (1.0, 2.0));
        assert_eq!(ordered_pair(2.0, 1.0), (1.0, 2.0));
    }

    #[test]
    fn ping_pong_stays_within_bounds() {
        let min = 0.1;
        let max = 0.9;
        for value in [-1.0, 0.1, 0.5, 0.9, 2.0] {
            let v = ping_pong(value, min, max);
            assert!(v >= min && v <= max, "value {v} out of bounds");
        }
    }

    #[test]
    fn palette_color_clamps_intensity() {
        let theme = ThemeInputs::default_dark();
        let palette = MetaballsPalette::ThemeAccents;
        let low = palette.color_at(0.3, -1.0, &theme);
        let high = palette.color_at(0.3, 2.0, &theme);
        assert_eq!(low, palette.color_at(0.3, 0.0, &theme));
        assert_eq!(high, palette.color_at(0.3, 1.0, &theme));
    }

    #[test]
    fn quality_off_leaves_buffer_unchanged() {
        let theme = ThemeInputs::default_dark();
        let mut fx = MetaballsFx::default();
        let ctx = FxContext {
            width: 8,
            height: 4,
            frame: 0,
            time_seconds: 0.5,
            quality: FxQuality::Off,
            theme: &theme,
        };
        // When quality is Off, backdrop effects should NOT modify the buffer.
        // This is the correct behavior - decorative effects are non-essential
        // and should simply skip rendering, leaving the existing content intact.
        let sentinel = PackedRgba::rgb(255, 0, 0);
        let mut out = vec![sentinel; ctx.len()];
        fx.render(ctx, &mut out);
        assert!(
            out.iter().all(|&px| px == sentinel),
            "Off quality should leave buffer unchanged"
        );
    }

    #[test]
    fn presets_are_within_bounds() {
        let presets = [
            MetaballsParams::default(),
            MetaballsParams::aurora(),
            MetaballsParams::lava(),
            MetaballsParams::ocean(),
        ];

        for params in presets {
            assert!(
                params.bounds_min <= params.bounds_max,
                "bounds_min > bounds_max: {} > {}",
                params.bounds_min,
                params.bounds_max
            );
            assert!(
                params.radius_min <= params.radius_max,
                "radius_min > radius_max: {} > {}",
                params.radius_min,
                params.radius_max
            );
            assert!(
                params.glow_threshold <= params.threshold,
                "glow_threshold > threshold: {} > {}",
                params.glow_threshold,
                params.threshold
            );

            for ball in &params.balls {
                assert!(
                    (0.0..=1.0).contains(&ball.x),
                    "ball.x out of range: {}",
                    ball.x
                );
                assert!(
                    (0.0..=1.0).contains(&ball.y),
                    "ball.y out of range: {}",
                    ball.y
                );
                assert!(ball.radius >= 0.0, "ball.radius negative: {}", ball.radius);
                assert!(
                    (0.0..=1.0).contains(&ball.hue),
                    "ball.hue out of range: {}",
                    ball.hue
                );
            }
        }
    }

    #[cfg(feature = "fx-gpu")]
    #[test]
    fn gpu_force_fail_falls_back_to_cpu() {
        let _guard = GPU_TEST_LOCK.lock().expect("gpu test lock poisoned");

        let theme = ThemeInputs::default_dark();
        let ctx = FxContext {
            width: 16,
            height: 8,
            frame: 2,
            time_seconds: 0.75,
            quality: FxQuality::Full,
            theme: &theme,
        };

        // Baseline CPU render (GPU disabled via test helper).
        gpu::force_disable_for_tests();

        let mut fx_cpu = MetaballsFx::default();
        let mut out_cpu = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx_cpu.render(ctx, &mut out_cpu);

        // Force GPU init failure; render should silently fall back to CPU.
        gpu::force_init_fail_for_tests();

        let mut fx_fallback = MetaballsFx::default();
        let mut out_fallback = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx_fallback.render(ctx, &mut out_fallback);

        assert_eq!(
            out_cpu, out_fallback,
            "forced GPU failure should fall back to CPU output"
        );
        assert!(
            gpu::is_disabled_for_tests(),
            "GPU should be marked unavailable after forced failure"
        );

        // Reset for other tests.
        gpu::reset_for_tests();
    }

    #[cfg(feature = "fx-gpu")]
    #[test]
    fn gpu_parity_sanity_small_buffer() {
        let _guard = GPU_TEST_LOCK.lock().expect("gpu test lock poisoned");

        // Reset to allow GPU initialization.
        gpu::reset_for_tests();

        let theme = ThemeInputs::default_dark();
        let ctx = FxContext {
            width: 12,
            height: 6,
            frame: 3,
            time_seconds: 0.9,
            quality: FxQuality::Full,
            theme: &theme,
        };

        let mut fx = MetaballsFx::default();
        fx.populate_ball_cache(ctx.time_seconds, ctx.quality);
        fx.sync_gpu_ball_cache();
        let stops = fx.params.palette.stops(ctx.theme);
        let (glow, threshold) = fx.params.thresholds();

        let mut gpu_out = vec![PackedRgba::TRANSPARENT; ctx.len()];
        let rendered = gpu::render_metaballs(
            ctx,
            glow,
            threshold,
            ctx.theme.bg_base,
            stops,
            &fx.gpu_ball_cache,
            &mut gpu_out,
        );
        if !rendered {
            return;
        }

        // Force CPU-only rendering for comparison.
        gpu::force_disable_for_tests();
        let mut fx_cpu = MetaballsFx::default();
        let mut cpu_out = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx_cpu.render(ctx, &mut cpu_out);

        // Reset for other tests.
        gpu::reset_for_tests();

        let max_diff = max_channel_diff(&cpu_out, &gpu_out);
        assert!(
            max_diff <= 8,
            "GPU output deviates from CPU beyond tolerance: {max_diff}"
        );
    }

    #[cfg(feature = "fx-gpu")]
    fn max_channel_diff(cpu: &[PackedRgba], gpu: &[PackedRgba]) -> u8 {
        let mut max_diff = 0u8;
        for (a, b) in cpu.iter().zip(gpu.iter()) {
            max_diff = max_diff.max(a.r().abs_diff(b.r()));
            max_diff = max_diff.max(a.g().abs_diff(b.g()));
            max_diff = max_diff.max(a.b().abs_diff(b.b()));
            max_diff = max_diff.max(a.a().abs_diff(b.a()));
        }
        max_diff
    }

    #[cfg(feature = "fx-gpu")]
    #[test]
    #[ignore = "requires GPU; run manually for perf comparison"]
    fn gpu_cpu_timing_baseline() {
        let _guard = GPU_TEST_LOCK.lock().expect("gpu test lock poisoned");

        // Reset to allow GPU initialization.
        gpu::reset_for_tests();

        let theme = ThemeInputs::default_dark();
        let sizes = [(120u16, 40u16), (240u16, 80u16)];

        for (width, height) in sizes {
            // Reset GPU state for each size to measure fresh init.
            gpu::reset_for_tests();

            let ctx = FxContext {
                width,
                height,
                frame: 5,
                time_seconds: 1.0,
                quality: FxQuality::Full,
                theme: &theme,
            };

            let mut fx = MetaballsFx::default();
            fx.populate_ball_cache(ctx.time_seconds, ctx.quality);
            fx.sync_gpu_ball_cache();
            let stops = fx.params.palette.stops(ctx.theme);
            let (glow, threshold) = fx.params.thresholds();
            let mut gpu_out = vec![PackedRgba::TRANSPARENT; ctx.len()];

            let gpu_start = std::time::Instant::now();
            let rendered = gpu::render_metaballs(
                ctx,
                glow,
                threshold,
                ctx.theme.bg_base,
                stops,
                &fx.gpu_ball_cache,
                &mut gpu_out,
            );
            let gpu_elapsed = gpu_start.elapsed();

            if !rendered {
                eprintln!("GPU unavailable for {width}x{height}, skipping timing");
                continue;
            }

            // Force CPU-only rendering for comparison.
            gpu::force_disable_for_tests();
            let mut fx_cpu = MetaballsFx::default();
            let mut cpu_out = vec![PackedRgba::TRANSPARENT; ctx.len()];
            let cpu_start = std::time::Instant::now();
            fx_cpu.render(ctx, &mut cpu_out);
            let cpu_elapsed = cpu_start.elapsed();

            eprintln!(
                "Metaballs {width}x{height}: GPU={:?} CPU={:?}",
                gpu_elapsed, cpu_elapsed
            );
        }

        // Reset for other tests.
        gpu::reset_for_tests();
    }
}

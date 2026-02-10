//! PSX Doom fire spread effect.
//!
//! Authentic port of the classic PlayStation Doom fire algorithm:
//! bottom row = max heat, each frame propagates upward with random
//! lateral spread and cooling. Uses a 37-color palette from black
//! through deep red, orange, yellow, to white.
//!
//! # Determinism
//!
//! Uses xorshift32 seeded from frame number for reproducible output.
//!
//! # No Per-Frame Allocations
//!
//! Heat buffer is grow-only; only resized on dimension change.

use crate::visual_fx::{BackdropFx, FxContext, FxQuality};
use ftui_render::cell::PackedRgba;

// ---------------------------------------------------------------------------
// PSX Doom Fire Palette (37 entries: 0 = black, 36 = white)
// ---------------------------------------------------------------------------

/// Classic PSX Doom fire palette: black -> deep red -> red -> orange -> yellow -> white.
const FIRE_PALETTE: [(u8, u8, u8); 37] = [
    (7, 7, 7),       // 0: near-black
    (31, 7, 7),      // 1
    (47, 15, 7),     // 2
    (71, 15, 7),     // 3
    (87, 23, 7),     // 4
    (103, 31, 7),    // 5
    (119, 31, 7),    // 6
    (143, 39, 7),    // 7
    (159, 47, 7),    // 8
    (175, 63, 7),    // 9
    (191, 71, 7),    // 10
    (199, 71, 7),    // 11
    (223, 79, 7),    // 12
    (223, 87, 7),    // 13
    (223, 95, 7),    // 14
    (215, 103, 15),  // 15
    (207, 111, 15),  // 16
    (207, 119, 15),  // 17
    (207, 127, 15),  // 18
    (207, 135, 23),  // 19
    (199, 135, 23),  // 20
    (199, 143, 23),  // 21
    (199, 151, 31),  // 22
    (191, 159, 31),  // 23
    (191, 159, 31),  // 24
    (191, 167, 39),  // 25
    (191, 167, 39),  // 26
    (191, 175, 47),  // 27
    (183, 175, 47),  // 28
    (183, 183, 47),  // 29
    (183, 183, 55),  // 30
    (207, 207, 111), // 31
    (223, 223, 159), // 32
    (231, 231, 191), // 33
    (239, 239, 223), // 34
    (247, 247, 239), // 35
    (255, 255, 255), // 36: white
];

// ---------------------------------------------------------------------------
// Xorshift32 RNG
// ---------------------------------------------------------------------------

/// Deterministic xorshift32 PRNG.
#[inline]
fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

// ---------------------------------------------------------------------------
// DoomFireFx
// ---------------------------------------------------------------------------

/// PSX Doom fire spread effect.
///
/// Implements the classic fire algorithm: a heat buffer where the bottom
/// row is max intensity and heat propagates upward with random wind and
/// cooling each frame.
///
/// # Quality Degradation
///
/// - `Full`: Every pixel updated every frame
/// - `Reduced`: Skip every other row during propagation
/// - `Minimal`: Only propagate every 4th row
/// - `Off`: No rendering
#[derive(Debug, Clone)]
pub struct DoomFireFx {
    /// Heat buffer, row-major. Values 0..=36.
    heat: Vec<u8>,
    /// Precomputed clamped source-x lookup for spread offsets (-2..=3).
    /// Layout: x * 6 + (offset + 2), where offset is in [-2, 3].
    src_x_lut: Vec<usize>,
    /// Width of the heat buffer.
    last_width: u16,
    /// Height of the heat buffer.
    last_height: u16,
    /// Width used to build `src_x_lut`.
    lut_width: u16,
    /// Wind value used to build `src_x_lut`.
    lut_wind: i32,
    /// Wind direction: -1, 0, or 1 (shifts flame left/right).
    wind: i32,
    /// Whether the fire is active (bottom row hot).
    active: bool,
}

impl DoomFireFx {
    /// Create a new fire effect.
    pub fn new() -> Self {
        Self {
            heat: Vec::new(),
            src_x_lut: Vec::new(),
            last_width: 0,
            last_height: 0,
            lut_width: 0,
            lut_wind: i32::MIN,
            wind: 0,
            active: true,
        }
    }

    /// Set wind direction (-1 = left, 0 = none, 1 = right).
    pub fn set_wind(&mut self, wind: i32) {
        self.wind = wind.clamp(-1, 1);
    }

    /// Set whether the fire is active (bottom row emits heat).
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    /// Ensure clamped source-x lookup is available for current width/wind.
    fn ensure_src_x_lut(&mut self, width: usize) {
        if self.lut_width as usize == width && self.lut_wind == self.wind {
            return;
        }

        self.src_x_lut.resize(width.saturating_mul(6), 0);
        let max_x = width.saturating_sub(1);

        for x in 0..width {
            let base = x * 6;
            for offset in -2..=3 {
                let clamped = (x as i32 + offset).clamp(0, max_x as i32) as usize;
                self.src_x_lut[base + (offset + 2) as usize] = clamped;
            }
        }

        self.lut_width = width as u16;
        self.lut_wind = self.wind;
    }

    /// Initialize or resize the heat buffer.
    fn ensure_buffer(&mut self, width: u16, height: u16) {
        if self.last_width == width && self.last_height == height {
            return;
        }

        let len = width as usize * height as usize;
        if len > self.heat.len() {
            self.heat.resize(len, 0);
        }

        // Dimensions changed, so we must reset the buffer. Even if `len`
        // stays the same (e.g., 12x8 -> 8x12), the row stride changes and
        // stale heat would be reinterpreted as a different 2D layout.
        for v in self.heat[..len].iter_mut() {
            *v = 0;
        }

        self.last_width = width;
        self.last_height = height;

        // Set bottom row to max heat
        if self.active && height > 0 {
            let w = width as usize;
            let bottom_start = (height as usize - 1) * w;
            for x in 0..w {
                self.heat[bottom_start + x] = 36;
            }
        }
    }

    /// Propagate fire upward one frame.
    fn spread_fire(&mut self, frame: u64, quality: FxQuality) {
        let w = self.last_width as usize;
        let h = self.last_height as usize;
        if w == 0 || h < 2 {
            return;
        }

        self.ensure_src_x_lut(w);

        let mut rng = (frame.wrapping_mul(2654435761) as u32) | 1;

        let row_step = match quality {
            FxQuality::Full => 1,
            FxQuality::Reduced => 2,
            FxQuality::Minimal => 4,
            FxQuality::Off => return,
        };

        // Process from second-to-bottom row upward
        // For each pixel, sample from the row below with random x-offset and cooling
        let mut y = 1;
        while y < h {
            for x in 0..w {
                let src_y = y; // source is below (we process upward)
                let rand_val = xorshift32(&mut rng);
                let x_offset = ((rand_val & 3) as i32) - 1 + self.wind; // -1, 0, 1, 2 + wind
                let decay = (rand_val >> 2) & 1; // 0 or 1

                let src_x = self.src_x_lut[x * 6 + (x_offset + 2) as usize];
                let src_idx = src_y * w + src_x;
                let dst_idx = (src_y - 1) * w + x;

                let heat_below = self.heat[src_idx];
                self.heat[dst_idx] = heat_below.saturating_sub(decay as u8);
            }
            y += row_step;
        }

        // Update bottom row based on active state
        if self.active {
            let bottom_start = (h - 1) * w;
            for x in 0..w {
                self.heat[bottom_start + x] = 36;
            }
        } else {
            // Cool down bottom row gradually
            let bottom_start = (h - 1) * w;
            for x in 0..w {
                let rand_val = xorshift32(&mut rng);
                let decay = ((rand_val & 7) + 1) as u8;
                self.heat[bottom_start + x] = self.heat[bottom_start + x].saturating_sub(decay);
            }
        }
    }
}

impl Default for DoomFireFx {
    fn default() -> Self {
        Self::new()
    }
}

impl BackdropFx for DoomFireFx {
    fn name(&self) -> &'static str {
        "Doom Fire"
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.ensure_buffer(width, height);
    }

    fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
        let w = ctx.width as usize;
        let h = ctx.height as usize;
        if w == 0 || h == 0 {
            return;
        }

        self.ensure_buffer(ctx.width, ctx.height);
        self.spread_fire(ctx.frame, ctx.quality);

        // Map heat buffer to colors
        let len = w * h;
        for i in 0..len.min(out.len()) {
            let heat_val = self.heat[i] as usize;
            let (r, g, b) = FIRE_PALETTE[heat_val.min(36)];
            out[i] = PackedRgba::rgb(r, g, b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_fx::ThemeInputs;

    fn default_theme() -> ThemeInputs {
        ThemeInputs::default_dark()
    }

    fn make_ctx(width: u16, height: u16, frame: u64) -> FxContext<'static> {
        make_ctx_with_quality(width, height, frame, FxQuality::Full)
    }

    fn make_ctx_with_quality(
        width: u16,
        height: u16,
        frame: u64,
        quality: FxQuality,
    ) -> FxContext<'static> {
        // We need a &ThemeInputs, so leak a boxed one for testing
        let theme = Box::leak(Box::new(default_theme()));
        FxContext {
            width,
            height,
            frame,
            time_seconds: frame as f64 / 60.0,
            quality,
            theme,
        }
    }

    #[test]
    fn fire_produces_output() {
        let mut fx = DoomFireFx::new();
        let ctx = make_ctx(10, 10, 5);
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
        fx.render(ctx, &mut buf);

        // Bottom row should be white/hot
        let bottom = &buf[90..100];
        for c in bottom {
            assert_eq!(*c, PackedRgba::rgb(255, 255, 255));
        }
    }

    #[test]
    fn fire_zero_dimensions() {
        let mut fx = DoomFireFx::new();
        let ctx = make_ctx(0, 0, 0);
        let mut buf = vec![];
        fx.render(ctx, &mut buf);
        // Should not panic
    }

    #[test]
    fn fire_deterministic() {
        let mut fx1 = DoomFireFx::new();
        let mut fx2 = DoomFireFx::new();
        let ctx = make_ctx(20, 15, 10);
        let mut buf1 = vec![PackedRgba::rgb(0, 0, 0); 300];
        let mut buf2 = vec![PackedRgba::rgb(0, 0, 0); 300];
        fx1.render(ctx, &mut buf1);
        fx2.render(ctx, &mut buf2);
        assert_eq!(buf1, buf2);
    }

    #[test]
    fn fire_wind_shifts() {
        let mut fx_no_wind = DoomFireFx::new();
        let mut fx_wind = DoomFireFx::new();
        fx_wind.set_wind(1);

        let mut buf1 = vec![PackedRgba::rgb(0, 0, 0); 300];
        let mut buf2 = vec![PackedRgba::rgb(0, 0, 0); 300];

        // Run multiple frames so fire has time to propagate with wind
        for frame in 0..30 {
            let ctx = make_ctx(20, 15, frame);
            fx_no_wind.render(ctx, &mut buf1);
            fx_wind.render(ctx, &mut buf2);
        }

        // After many frames, wind should create visible differences
        assert_ne!(buf1, buf2);
    }

    #[test]
    fn xorshift32_no_zero() {
        let mut state = 1u32;
        for _ in 0..1000 {
            let v = xorshift32(&mut state);
            assert_ne!(v, 0, "xorshift32 should never produce 0");
        }
    }

    #[test]
    fn fire_set_active_false_cools_bottom() {
        let mut fx = DoomFireFx::new();
        // Run a few frames to heat up
        for frame in 0..10 {
            let ctx = make_ctx(10, 10, frame);
            let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
            fx.render(ctx, &mut buf);
        }
        // Deactivate and run more frames
        fx.set_active(false);
        for frame in 10..80 {
            let ctx = make_ctx(10, 10, frame);
            let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
            fx.render(ctx, &mut buf);
        }
        // After many frames with no heat source, fire should be mostly cooled
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
        fx.render(make_ctx(10, 10, 80), &mut buf);
        let bottom = &buf[90..100];
        // Bottom row is no longer white since active=false
        assert!(
            bottom.iter().any(|c| *c != PackedRgba::rgb(255, 255, 255)),
            "bottom should cool when deactivated"
        );
    }

    #[test]
    fn fire_resize_resets_buffer() {
        let mut fx = DoomFireFx::new();
        // Render at one size
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
        fx.render(make_ctx(10, 10, 5), &mut buf);
        // Resize to different dimensions
        fx.resize(20, 15);
        let mut buf2 = vec![PackedRgba::rgb(0, 0, 0); 300];
        fx.render(make_ctx(20, 15, 6), &mut buf2);
        // Bottom row of new size should be white/hot
        let bottom = &buf2[280..300];
        for c in bottom {
            assert_eq!(*c, PackedRgba::rgb(255, 255, 255));
        }
    }

    #[test]
    fn fire_resize_same_len_resets_non_bottom_heat() {
        let mut fx = DoomFireFx::new();

        // Warm up so we have non-zero heat above the bottom row.
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 12 * 8];
        for frame in 0..40 {
            fx.render(make_ctx(12, 8, frame), &mut buf);
        }

        let w1 = 12usize;
        let h1 = 8usize;
        let bottom_start1 = (h1 - 1) * w1;
        assert!(
            fx.heat[..bottom_start1].iter().any(|&h| h > 0),
            "warmup should propagate heat above the bottom row"
        );

        // Resize to a different shape with the same total cell count.
        fx.resize(8, 12);

        let w2 = 8usize;
        let h2 = 12usize;
        let bottom_start2 = (h2 - 1) * w2;
        assert!(
            fx.heat[..bottom_start2].iter().all(|&h| h == 0),
            "non-bottom cells should be reset on resize even when len is unchanged"
        );
        assert!(
            fx.heat[bottom_start2..bottom_start2 + w2]
                .iter()
                .all(|&h| h == 36),
            "bottom row should be reseeded to max heat on resize when active"
        );
    }

    #[test]
    fn fire_wind_clamps_to_range() {
        let mut fx = DoomFireFx::new();
        fx.set_wind(5);
        // Wind should be clamped to 1, so fire still renders correctly
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
        fx.render(make_ctx(10, 10, 5), &mut buf);
        // Bottom row should still be hot
        let bottom = &buf[90..100];
        for c in bottom {
            assert_eq!(*c, PackedRgba::rgb(255, 255, 255));
        }
    }

    // --- Palette ---

    #[test]
    fn fire_palette_has_37_entries() {
        assert_eq!(FIRE_PALETTE.len(), 37);
    }

    #[test]
    fn fire_palette_starts_near_black() {
        let (r, g, b) = FIRE_PALETTE[0];
        assert!(r < 16 && g < 16 && b < 16);
    }

    #[test]
    fn fire_palette_ends_at_white() {
        assert_eq!(FIRE_PALETTE[36], (255, 255, 255));
    }

    #[test]
    fn fire_palette_monotonically_brighter() {
        // Overall brightness should increase from start to end
        let brightness = |i: usize| -> u32 {
            let (r, g, b) = FIRE_PALETTE[i];
            r as u32 + g as u32 + b as u32
        };
        assert!(brightness(0) < brightness(36));
        assert!(brightness(0) < brightness(18));
        assert!(brightness(18) < brightness(36));
    }

    // --- xorshift32 ---

    #[test]
    fn xorshift32_deterministic() {
        let mut s1 = 42u32;
        let mut s2 = 42u32;
        let v1: Vec<u32> = (0..100).map(|_| xorshift32(&mut s1)).collect();
        let v2: Vec<u32> = (0..100).map(|_| xorshift32(&mut s2)).collect();
        assert_eq!(v1, v2);
    }

    #[test]
    fn xorshift32_different_seeds() {
        let mut s1 = 1u32;
        let mut s2 = 2u32;
        let v1: Vec<u32> = (0..10).map(|_| xorshift32(&mut s1)).collect();
        let v2: Vec<u32> = (0..10).map(|_| xorshift32(&mut s2)).collect();
        assert_ne!(v1, v2);
    }

    // --- DoomFireFx construction ---

    #[test]
    fn new_fire_initial_state() {
        let fx = DoomFireFx::new();
        assert!(fx.heat.is_empty());
        assert!(fx.active);
        assert_eq!(fx.wind, 0);
        assert_eq!(fx.last_width, 0);
        assert_eq!(fx.last_height, 0);
    }

    #[test]
    fn default_matches_new() {
        let d = DoomFireFx::default();
        let n = DoomFireFx::new();
        assert_eq!(d.heat, n.heat);
        assert_eq!(d.active, n.active);
        assert_eq!(d.wind, n.wind);
    }

    // --- BackdropFx trait ---

    #[test]
    fn name_returns_doom_fire() {
        let fx = DoomFireFx::new();
        assert_eq!(fx.name(), "Doom Fire");
    }

    #[test]
    fn trait_resize_initializes_buffer() {
        let mut fx = DoomFireFx::new();
        fx.resize(10, 10);
        assert_eq!(fx.last_width, 10);
        assert_eq!(fx.last_height, 10);
    }

    // --- Quality levels ---

    #[test]
    fn quality_off_produces_first_frame_only() {
        let mut fx = DoomFireFx::new();
        let theme = Box::leak(Box::new(default_theme()));
        let ctx = FxContext {
            width: 10,
            height: 10,
            frame: 5,
            time_seconds: 5.0 / 60.0,
            quality: FxQuality::Off,
            theme,
        };
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
        fx.render(ctx, &mut buf);
        // Should still produce output (ensure_buffer runs), but no spread
        // Bottom row should be hot (buffer init), top should be cold (no spread)
        assert_eq!(buf[90], PackedRgba::rgb(255, 255, 255)); // bottom
    }

    #[test]
    fn quality_reduced_still_produces_fire() {
        let mut fx = DoomFireFx::new();
        let theme = Box::leak(Box::new(default_theme()));
        // Run enough frames for heat to propagate
        for frame in 0..20 {
            let ctx = FxContext {
                width: 10,
                height: 10,
                frame,
                time_seconds: frame as f64 / 60.0,
                quality: FxQuality::Reduced,
                theme,
            };
            let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
            fx.render(ctx, &mut buf);
        }
        // Heat should have propagated at least partially
        let top_row_brightness: u32 = (0..10)
            .map(|x| {
                let idx = FIRE_PALETTE[fx.heat[x] as usize];
                idx.0 as u32 + idx.1 as u32 + idx.2 as u32
            })
            .sum();
        // Top row should have some heat after 20 frames even at reduced quality
        assert!(top_row_brightness > 0);
    }

    #[test]
    fn quality_minimal_propagates_slower_than_full() {
        let mut full = DoomFireFx::new();
        let mut minimal = DoomFireFx::new();
        let mut full_buf = vec![PackedRgba::rgb(0, 0, 0); 200];
        let mut minimal_buf = vec![PackedRgba::rgb(0, 0, 0); 200];

        for frame in 0..24 {
            full.render(
                make_ctx_with_quality(10, 20, frame, FxQuality::Full),
                &mut full_buf,
            );
            minimal.render(
                make_ctx_with_quality(10, 20, frame, FxQuality::Minimal),
                &mut minimal_buf,
            );
        }

        let top_full: u32 = full.heat[..10].iter().map(|&h| h as u32).sum();
        let top_minimal: u32 = minimal.heat[..10].iter().map(|&h| h as u32).sum();
        assert!(
            top_minimal <= top_full,
            "minimal quality should propagate more slowly: full={top_full}, minimal={top_minimal}"
        );
    }

    // --- Edge cases ---

    #[test]
    fn single_row_no_spread() {
        let mut fx = DoomFireFx::new();
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 10];
        fx.render(make_ctx(10, 1, 5), &mut buf);
        // Single row = bottom row = all hot
        for c in &buf {
            assert_eq!(*c, PackedRgba::rgb(255, 255, 255));
        }
    }

    #[test]
    fn heat_decreases_upward() {
        let mut fx = DoomFireFx::new();
        // Run several frames so heat propagates
        for frame in 0..30 {
            let mut buf = vec![PackedRgba::rgb(0, 0, 0); 200];
            fx.render(make_ctx(10, 20, frame), &mut buf);
        }
        // Average heat in bottom half should be >= average heat in top half
        let bottom_avg: f32 = fx.heat[100..200].iter().map(|&h| h as f32).sum::<f32>() / 100.0;
        let top_avg: f32 = fx.heat[0..100].iter().map(|&h| h as f32).sum::<f32>() / 100.0;
        assert!(
            bottom_avg >= top_avg,
            "bottom ({bottom_avg}) should be >= top ({top_avg})"
        );
    }

    #[test]
    fn wind_negative_shifts_left() {
        let mut fx = DoomFireFx::new();
        fx.set_wind(-1);
        assert_eq!(fx.wind, -1);
        // Should render without panics
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
        for frame in 0..10 {
            fx.render(make_ctx(10, 10, frame), &mut buf);
        }
    }

    #[test]
    fn wind_clamps_negative() {
        let mut fx = DoomFireFx::new();
        fx.set_wind(-100);
        assert_eq!(fx.wind, -1);
    }

    #[test]
    fn wind_clamps_positive() {
        let mut fx = DoomFireFx::new();
        fx.set_wind(100);
        assert_eq!(fx.wind, 1);
    }

    // --- LUT caching ---

    #[test]
    fn lut_recomputed_on_width_change() {
        let mut fx = DoomFireFx::new();
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
        fx.render(make_ctx(10, 10, 1), &mut buf);
        let lut_len_1 = fx.src_x_lut.len();

        let mut buf2 = vec![PackedRgba::rgb(0, 0, 0); 200];
        fx.render(make_ctx(20, 10, 2), &mut buf2);
        let lut_len_2 = fx.src_x_lut.len();

        assert_ne!(lut_len_1, lut_len_2);
        assert_eq!(lut_len_2, 20 * 6);
    }

    #[test]
    fn lut_recomputed_on_wind_change() {
        let mut fx = DoomFireFx::new();
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
        fx.render(make_ctx(10, 10, 1), &mut buf);
        assert_eq!(fx.lut_wind, 0);

        fx.set_wind(1);
        fx.render(make_ctx(10, 10, 2), &mut buf);
        assert_eq!(fx.lut_wind, 1);
    }

    // --- Multiple frame rendering ---

    #[test]
    fn hundred_frames_no_panic() {
        let mut fx = DoomFireFx::new();
        for frame in 0..100 {
            let mut buf = vec![PackedRgba::rgb(0, 0, 0); 400];
            fx.render(make_ctx(20, 20, frame), &mut buf);
        }
    }

    #[test]
    fn reactivate_fire_restores_bottom_heat() {
        let mut fx = DoomFireFx::new();
        // Warm up
        for frame in 0..10 {
            let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
            fx.render(make_ctx(10, 10, frame), &mut buf);
        }
        // Deactivate and cool
        fx.set_active(false);
        for frame in 10..60 {
            let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
            fx.render(make_ctx(10, 10, frame), &mut buf);
        }
        // Reactivate
        fx.set_active(true);
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 100];
        fx.render(make_ctx(10, 10, 60), &mut buf);
        // Bottom row should be hot again
        for c in &buf[90..100] {
            assert_eq!(*c, PackedRgba::rgb(255, 255, 255));
        }
    }

    #[test]
    fn inactive_bottom_row_monotonically_cools() {
        let mut fx = DoomFireFx::new();
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 144];
        fx.render(make_ctx(12, 12, 0), &mut buf);
        fx.set_active(false);

        let width = 12usize;
        let height = 12usize;
        let bottom_start = (height - 1) * width;
        let mut prev_sum: u32 = fx.heat[bottom_start..bottom_start + width]
            .iter()
            .map(|&h| h as u32)
            .sum();

        for frame in 1..40 {
            fx.render(make_ctx(12, 12, frame), &mut buf);
            let next_sum: u32 = fx.heat[bottom_start..bottom_start + width]
                .iter()
                .map(|&h| h as u32)
                .sum();
            assert!(
                next_sum <= prev_sum,
                "inactive bottom row should not gain heat: prev={prev_sum}, next={next_sum}"
            );
            prev_sum = next_sum;
        }
    }

    #[test]
    fn heat_values_always_stay_within_palette_bounds() {
        let mut fx = DoomFireFx::new();
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 256];

        for frame in 0..200 {
            if frame == 80 {
                fx.set_active(false);
            } else if frame == 140 {
                fx.set_active(true);
            }
            fx.set_wind((frame as i32 % 3) - 1);
            fx.render(make_ctx(16, 16, frame), &mut buf);
            assert!(
                fx.heat.iter().all(|&h| h <= 36),
                "heat values must stay in palette range 0..=36"
            );
        }
    }
}

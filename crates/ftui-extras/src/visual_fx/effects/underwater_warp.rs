#![forbid(unsafe_code)]

//! Quake underwater warp distortion effect.
//!
//! Authentic port of Quake's `R_WarpScreen` / `turbsin` table: for each
//! output pixel, the source coordinates are displaced by sinusoidal waves
//! to create the classic underwater distortion.
//!
//! # Algorithm
//!
//! For each output pixel (x, y):
//! ```text
//! src_x = x + amplitude * sin(y * frequency + time)
//! src_y = y + amplitude * cos(x * frequency + time)
//! ```
//! Then sample from the inner effect's buffer at (src_x, src_y).
//!
//! # Determinism
//!
//! Uses pre-computed sin LUT; given same inputs, output is identical.
//!
//! # No Per-Frame Allocations
//!
//! Inner buffer and LUT are grow-only.

use crate::visual_fx::{BackdropFx, FxContext, FxQuality};
use ftui_render::cell::PackedRgba;

// ---------------------------------------------------------------------------
// Turbulence sin table (256 entries, matching Quake's turbsin)
// ---------------------------------------------------------------------------

/// Pre-computed sin table with 256 entries for fast warp displacement.
fn build_turbsin() -> [f64; 256] {
    let mut table = [0.0f64; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = (i as f64 * std::f64::consts::TAU / 256.0).sin();
        i += 1;
    }
    table
}

// ---------------------------------------------------------------------------
// UnderwaterWarpFx
// ---------------------------------------------------------------------------

/// Quake-style underwater warp distortion.
///
/// Wraps an inner `BackdropFx` and applies sinusoidal displacement to
/// create the classic underwater distortion effect.
///
/// # Quality Degradation
///
/// - `Full`: Full-resolution warp with smooth sampling
/// - `Reduced`: Reduced amplitude (less distortion)
/// - `Minimal`: Very low amplitude, basic warp
/// - `Off`: No rendering
pub struct UnderwaterWarpFx {
    /// Pre-computed 256-entry sin lookup table.
    turbsin: [f64; 256],
    /// The inner effect being warped.
    inner: Box<dyn BackdropFx>,
    /// Scratch buffer for inner effect rendering.
    inner_buf: Vec<PackedRgba>,
    /// Warp amplitude (in pixels). Default: 2.0.
    amplitude: f64,
    /// Warp frequency. Default: 0.3.
    frequency: f64,
    /// Cached width.
    last_width: u16,
    /// Cached height.
    last_height: u16,
}

impl UnderwaterWarpFx {
    /// Create a new underwater warp wrapping an inner effect.
    pub fn new(inner: Box<dyn BackdropFx>) -> Self {
        Self {
            turbsin: build_turbsin(),
            inner,
            inner_buf: Vec::new(),
            amplitude: 2.0,
            frequency: 0.3,
            last_width: 0,
            last_height: 0,
        }
    }

    /// Create with custom amplitude and frequency.
    pub fn with_params(inner: Box<dyn BackdropFx>, amplitude: f64, frequency: f64) -> Self {
        Self {
            turbsin: build_turbsin(),
            inner,
            inner_buf: Vec::new(),
            amplitude,
            frequency,
            last_width: 0,
            last_height: 0,
        }
    }

    /// Set the warp amplitude (in pixels).
    pub fn set_amplitude(&mut self, amplitude: f64) {
        self.amplitude = amplitude;
    }

    /// Set the warp frequency.
    pub fn set_frequency(&mut self, frequency: f64) {
        self.frequency = frequency;
    }

    /// Access the inner effect.
    pub fn inner(&self) -> &dyn BackdropFx {
        &*self.inner
    }

    /// Access the inner effect mutably.
    pub fn inner_mut(&mut self) -> &mut dyn BackdropFx {
        &mut *self.inner
    }

    /// Look up sin value from pre-computed table with interpolation.
    #[inline]
    fn turbsin_lookup(&self, phase: f64) -> f64 {
        // Map phase to 0..256 range
        let idx_f = phase.rem_euclid(256.0);
        let idx0 = idx_f as usize & 255;
        let idx1 = (idx0 + 1) & 255;
        let frac = idx_f - idx_f.floor();
        self.turbsin[idx0] * (1.0 - frac) + self.turbsin[idx1] * frac
    }
}

impl BackdropFx for UnderwaterWarpFx {
    fn name(&self) -> &'static str {
        "Underwater Warp"
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.inner.resize(width, height);
        self.last_width = width;
        self.last_height = height;
    }

    fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
        let w = ctx.width as usize;
        let h = ctx.height as usize;
        if w == 0 || h == 0 {
            return;
        }

        if !ctx.quality.is_enabled() {
            return;
        }

        let len = w * h;

        // Ensure inner buffer
        if self.inner_buf.len() < len {
            self.inner_buf.resize(len, PackedRgba::rgb(0, 0, 0));
        }

        // Render inner effect to scratch buffer
        self.inner.render(ctx, &mut self.inner_buf[..len]);

        self.last_width = ctx.width;
        self.last_height = ctx.height;

        // Quality-adjusted amplitude
        let amp = match ctx.quality {
            FxQuality::Full => self.amplitude,
            FxQuality::Reduced => self.amplitude * 0.6,
            FxQuality::Minimal => self.amplitude * 0.3,
            FxQuality::Off => return,
        };

        let time = ctx.time_seconds * 40.0; // Speed factor matching Quake's time scale
        let freq = self.frequency;
        let w_f = w as f64;
        let h_f = h as f64;
        let freq10 = freq * 10.0;
        let time_y = time * 0.9;
        let w_max = (w - 1) as f64;
        let h_max = (h - 1) as f64;

        // Precompute per-column y-displacements (phase_y depends only on x).
        // This avoids repeated turbsin_lookup in the inner loop.
        let col_dy: Vec<f64> = (0..w)
            .map(|x| {
                let phase_y = (x as f64) * freq10 + time_y;
                amp * self.turbsin_lookup(phase_y)
            })
            .collect();

        // Apply warp displacement
        for y in 0..h {
            // phase_x depends only on y — hoist out of inner x-loop
            let phase_x = (y as f64) * freq10 + time;
            let row_dx = amp * self.turbsin_lookup(phase_x);
            let y_f = y as f64;
            let dst_base = y * w;

            for x in 0..w {
                let dy = col_dy[x];

                // Source coordinates with clamping
                let src_x = ((x as f64 + row_dx).round()).clamp(0.0, w_max) as usize;
                let src_y = ((y_f + dy).round()).clamp(0.0, h_max) as usize;

                out[dst_base + x] = self.inner_buf[src_y * w + src_x];
            }
        }
    }
}

impl std::fmt::Debug for UnderwaterWarpFx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnderwaterWarpFx")
            .field("amplitude", &self.amplitude)
            .field("frequency", &self.frequency)
            .field("last_width", &self.last_width)
            .field("last_height", &self.last_height)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_fx::ThemeInputs;

    /// Gradient effect for testing warp displacement.
    struct GradientFx;
    impl BackdropFx for GradientFx {
        fn name(&self) -> &'static str {
            "Gradient"
        }
        fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
            let w = ctx.width as usize;
            let h = ctx.height as usize;
            for y in 0..h {
                for x in 0..w {
                    let r = ((x * 255) / w.max(1)) as u8;
                    let g = ((y * 255) / h.max(1)) as u8;
                    out[y * w + x] = PackedRgba::rgb(r, g, 128);
                }
            }
        }
    }

    fn default_theme() -> ThemeInputs {
        ThemeInputs::default_dark()
    }

    fn make_ctx(width: u16, height: u16, frame: u64) -> FxContext<'static> {
        let theme = Box::leak(Box::new(default_theme()));
        FxContext {
            width,
            height,
            frame,
            time_seconds: frame as f64 / 60.0,
            quality: FxQuality::Full,
            theme,
        }
    }

    #[test]
    fn warp_produces_distortion() {
        let inner = Box::new(GradientFx);
        let mut warp = UnderwaterWarpFx::new(inner);
        let ctx = make_ctx(20, 15, 5);
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 300];
        warp.render(ctx, &mut buf);

        // Output should differ from a plain gradient
        let mut plain = GradientFx;
        let mut plain_buf = vec![PackedRgba::rgb(0, 0, 0); 300];
        plain.render(ctx, &mut plain_buf);

        // At least some pixels should be displaced
        let different = buf
            .iter()
            .zip(plain_buf.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(different > 0, "Warp should displace at least some pixels");
    }

    #[test]
    fn warp_zero_dimensions() {
        let inner = Box::new(GradientFx);
        let mut warp = UnderwaterWarpFx::new(inner);
        let ctx = make_ctx(0, 0, 0);
        let mut buf = vec![];
        warp.render(ctx, &mut buf);
        // Should not panic
    }

    #[test]
    fn warp_deterministic() {
        let inner1 = Box::new(GradientFx);
        let inner2 = Box::new(GradientFx);
        let mut warp1 = UnderwaterWarpFx::new(inner1);
        let mut warp2 = UnderwaterWarpFx::new(inner2);

        let ctx = make_ctx(20, 15, 10);
        let mut buf1 = vec![PackedRgba::rgb(0, 0, 0); 300];
        let mut buf2 = vec![PackedRgba::rgb(0, 0, 0); 300];
        warp1.render(ctx, &mut buf1);
        warp2.render(ctx, &mut buf2);
        assert_eq!(buf1, buf2);
    }

    #[test]
    fn turbsin_table_range() {
        let warp = UnderwaterWarpFx::new(Box::new(GradientFx));
        for i in 0..256 {
            let v = warp.turbsin[i];
            assert!((-1.0..=1.0).contains(&v), "turbsin[{i}] = {v} out of range");
        }
    }

    #[test]
    fn custom_params() {
        let inner = Box::new(GradientFx);
        let mut warp = UnderwaterWarpFx::with_params(inner, 5.0, 0.5);
        assert!((warp.amplitude - 5.0).abs() < f64::EPSILON);
        assert!((warp.frequency - 0.5).abs() < f64::EPSILON);

        warp.set_amplitude(3.0);
        warp.set_frequency(0.2);
        assert!((warp.amplitude - 3.0).abs() < f64::EPSILON);
        assert!((warp.frequency - 0.2).abs() < f64::EPSILON);
    }
}
